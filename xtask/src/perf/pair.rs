use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use craft_testkit::pair::frames::{self, Block, Heading, Identity, Probe, Section};
use craft_testkit::pair::{self, Gate, Ident, RunRecord, Workload};
use serde::Deserialize;

const BENCH: &str = "frame_probe_bench";
const PACKAGE: &str = "photocraft-ui-egui";
const WORKLOADS: [&str; 2] = ["hover", "paint"];
const STAGES: [&str; 6] = ["input", "ui_build", "command", "damage", "composite", "upload"];
const GATES: [Gate; 5] = [Gate::OverheadCpu, Gate::OverheadInterval, Gate::UiEqMissed, Gate::UiEqLongStalls, Gate::UiEqLatency];
const DEFAULT_REFRESH_HZ: NonZeroU32 = match NonZeroU32::new(60) {
    Some(hz) => hz,
    None => NonZeroU32::MIN,
};
const RECORDS: &str = "pair.records";
const SUMMARY: &str = "pair-summary.md";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub t: f64,
    pub input_ms: f64,
    pub build_ms: f64,
    pub command_ms: f64,
    pub damage_ms: f64,
    pub composite_ms: f64,
    pub upload_ms: f64,
    pub latency_ms: Option<f64>,
}

impl Frame {
    fn sample(&self) -> frames::Frame<6> {
        let took = |ms: f64| frames::millis(ms).unwrap_or_default();
        frames::Frame {
            start: took(self.t),
            stages: [self.input_ms, self.build_ms, self.command_ms, self.damage_ms, self.composite_ms, self.upload_ms].map(took),
            latency: self.latency_ms.and_then(frames::millis),
            playback: pair::Count::default(),
            seek: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Report {
    pub context: Context,
    pub frames: Vec<Frame>,
}

#[derive(Debug, Deserialize)]
pub struct Context {
    pub workload: String,
    pub backend: String,
    pub scale: String,
}

pub fn ident(text: &str) -> Result<Ident, String> {
    frames::ident(text).map_err(|e| e.to_string())
}

pub fn probe(frames: &[Frame], refresh_hz: NonZeroU32) -> Probe<6> {
    Probe::of(&frames.iter().map(Frame::sample).collect::<Vec<_>>(), refresh_hz)
}

pub fn workload(name: &str, context: &Context, load: &str) -> Result<Workload, String> {
    let fixture = crate::sha256::hex(format!("{BENCH}:{name}").as_bytes());
    Ok(Workload {
        fixture_hash: ident(fixture.get(..16).unwrap_or(&fixture))?,
        mode: ident("native")?,
        backend: ident(&context.backend)?,
        load: ident(load)?,
        scale: ident(&context.scale)?,
    })
}

pub fn records(id: &Identity, name: &str, workload: &Workload, block: Block, probe: &Probe<6>) -> Result<Vec<RunRecord>, String> {
    frames::records(id, name, workload, block, probe, &GATES).map_err(|e| e.to_string())
}

pub fn framework_rev(lock: &str) -> Option<String> {
    let lock: toml::Table = toml::from_str(lock).ok()?;
    lock.get("package")?
        .as_array()?
        .iter()
        .find(|p| p.get("name").and_then(toml::Value::as_str) == Some("craft"))?
        .get("source")?
        .as_str()?
        .rsplit_once('#')
        .map(|(_, rev)| rev.to_owned())
}

#[derive(Debug, PartialEq)]
struct Opts {
    workloads: Vec<String>,
    units: u32,
    frames: u32,
    refresh_hz: NonZeroU32,
    night: Option<String>,
    runner_image: Option<String>,
    skip_build: bool,
}

fn positive<T: std::str::FromStr + PartialOrd + Default>(value: Option<&&str>, flag: &str) -> Result<T, String> {
    value.and_then(|v| v.parse::<T>().ok()).filter(|v| *v > T::default()).ok_or_else(|| format!("{flag} needs a whole number above 0"))
}

fn parse_opts(rest: &[&str]) -> Result<Opts, String> {
    let mut o = Opts { workloads: Vec::new(), units: 3, frames: 3600, refresh_hz: DEFAULT_REFRESH_HZ, night: None, runner_image: None, skip_build: false };
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match *a {
            "--workload" => {
                let name = it.next().filter(|n| WORKLOADS.contains(n)).ok_or_else(|| format!("--workload needs one of {}", WORKLOADS.join(", ")))?;
                o.workloads.push((*name).to_owned());
            }
            "--units" => o.units = positive(it.next(), "--units")?,
            "--frames" => o.frames = positive(it.next(), "--frames")?,
            "--refresh-hz" => o.refresh_hz = NonZeroU32::new(positive(it.next(), "--refresh-hz")?).ok_or("--refresh-hz needs a whole number above 0")?,
            "--night" => o.night = Some(it.next().ok_or("--night needs an id")?.to_string()),
            "--runner-image" => o.runner_image = Some(it.next().ok_or("--runner-image needs an id")?.to_string()),
            "--skip-build" => o.skip_build = true,
            other => return Err(format!("perf --pair: unknown option `{other}`")),
        }
    }
    if o.frames < 2 {
        return Err("--frames needs at least 2: an interval is between two frames".into());
    }
    if o.workloads.is_empty() {
        o.workloads = WORKLOADS.iter().map(|w| (*w).to_owned()).collect();
    }
    Ok(o)
}

fn identity(root: &Path, o: &Opts) -> Result<Identity, String> {
    let now = super::now_iso();
    let commit = super::git(root, &["rev-parse", "HEAD"]).filter(|c| !c.is_empty()).unwrap_or_else(|| "unknown".into());
    let dirty = super::git(root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).map_err(|e| format!("read Cargo.lock: {e}"))?;
    let image = match (std::env::var("ImageOS"), std::env::var("ImageVersion")) {
        (Ok(os), Ok(version)) => format!("{os}-{version}"),
        _ => "local".into(),
    };
    Ok(Identity {
        run: format!("{}-{}", now.replace(['-', ':'], ""), std::process::id()),
        app_pin: ident(&if dirty { format!("{commit}+dirty") } else { commit })?,
        fw_rev: ident(&framework_rev(&lock).unwrap_or_else(|| "unknown".into()))?,
        os: ident(&format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))?,
        runner_image: ident(o.runner_image.as_deref().unwrap_or(&image))?,
        night: ident(o.night.as_deref().unwrap_or(now.get(..10).unwrap_or(&now)))?,
        refresh_hz: o.refresh_hz,
    })
}

struct Measured {
    workload: Workload,
    probe: Probe<6>,
    records: Vec<RunRecord>,
}

fn measure(exe: &Path, dir: &Path, root: &Path, o: &Opts, name: &str, block: Block) -> Result<(Context, Probe<6>), String> {
    let json = dir.join(format!("{name}-{}-{}.json", block.unit, block.position));
    let args = ["--workload", name, "--frames", &o.frames.to_string(), "--refresh-hz", &o.refresh_hz.to_string()].map(str::to_owned);
    let mut c = Command::new(exe);
    c.current_dir(root).args(&args).arg("--json").arg(&json);
    crate::run(c, &format!("{BENCH} {} --json {} ({:?}, unit {})", args.join(" "), json.display(), block.arm, block.unit))?;
    let text = std::fs::read_to_string(&json).map_err(|e| format!("{}: {e}", json.display()))?;
    let report: Report = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", json.display()))?;
    if report.context.workload != name {
        return Err(format!("{}: ran workload `{}`, not `{name}`", json.display(), report.context.workload));
    }
    let probe = probe(&report.frames, o.refresh_hz);
    Ok((report.context, probe))
}

pub fn load_label(contended: bool) -> &'static str {
    if contended { "contended" } else { "idle" }
}

fn clear(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path).or_else(|_| std::fs::remove_dir_all(path)) {
        Err(e) if path.exists() => Err(format!("remove {}: {e}", path.display())),
        _ => Ok(()),
    }
}

pub fn run(root: &Path, rest: &[&str]) -> Result<(), String> {
    let o = parse_opts(rest)?;
    let budgets = std::fs::read_to_string(root.join(super::BUDGETS)).map_err(|e| format!("read {}: {e}", super::BUDGETS))?;
    let max_load_per_core = super::parse_budgets(&budgets)?.settings.max_load_per_core;
    let cores = std::thread::available_parallelism().map(|n| n.get()).ok();
    let load = load_label(super::contended(super::load_avg(), cores, max_load_per_core));
    let target = crate::target_dir()?;
    let out_dir = target.join("perf");
    let dir = out_dir.join("pair");
    for stale in [out_dir.join(RECORDS), out_dir.join(SUMMARY), dir.clone()] {
        clear(&stale)?;
    }
    if !o.skip_build {
        let mut c = crate::cargo();
        c.args(["build", "--release", "-p", PACKAGE, "--example", BENCH]);
        crate::run(c, &format!("cargo build --release -p {PACKAGE} --example {BENCH}"))?;
    }
    let exe = target.join("release").join("examples").join(format!("{BENCH}{}", std::env::consts::EXE_SUFFIX));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let id = identity(root, &o)?;
    let mut measured: BTreeMap<&str, Measured> = BTreeMap::new();
    for block in frames::blocks(o.units) {
        for name in &o.workloads {
            let (context, probe) = measure(&exe, &dir, root, &o, name, block)?;
            let workload = workload(name, &context, load)?;
            let block_records = records(&id, name, &workload, block, &probe)?;
            let m = measured.entry(name).or_insert_with(|| Measured { workload: workload.clone(), probe: Probe::default(), records: Vec::new() });
            if m.workload != workload {
                return Err(format!("{name}: the workload changed between blocks ({:?} then {workload:?})", m.workload));
            }
            m.probe.merge(&probe);
            m.records.extend(block_records);
        }
    }
    let all: Vec<RunRecord> = measured.values().flat_map(|m| m.records.iter().cloned()).collect();
    let records_path = out_dir.join(RECORDS);
    super::write(&records_path, &pair::print_records(&all))?;
    let shown = records_path.display().to_string();
    let mut summary = Heading { id: &id, load, units: o.units, frames: o.frames, records: &shown }.to_string();
    for (name, m) in &measured {
        let verdicts = frames::verdicts(&m.records).map_err(|e| e.to_string())?;
        let section = Section { name, workload: &m.workload, stages: STAGES, probe: &m.probe, verdicts: &verdicts };
        summary.push_str(&format!("{section}\n"));
    }
    super::write(&out_dir.join(SUMMARY), &summary)?;
    println!("\n{summary}");
    println!("wrote {} ({} records)", records_path.display(), all.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use craft_testkit::pair::Arm;

    use super::*;

    const FIXTURE: &str = r#"{
        "context": {"workload": "paint", "backend": "cpu", "scale": "3000x2000@1920x1080"},
        "frames": [
            {"t": 0.0, "frameMs": 2.0, "inputMs": 0.5, "buildMs": 0.25, "commandMs": 0.0, "damageMs": 0.0, "compositeMs": 1.0, "uploadMs": 0.25, "latencyMs": null, "path": "cpu"},
            {"t": 16.667, "frameMs": 3.0, "inputMs": 0.5, "buildMs": 0.25, "commandMs": 0.0, "damageMs": 0.0, "compositeMs": 2.0, "uploadMs": 0.25, "latencyMs": 12.0, "path": "cpu"},
            {"t": 33.333, "frameMs": 3.0, "inputMs": 0.5, "buildMs": 0.25, "commandMs": 0.0, "damageMs": 0.0, "compositeMs": 2.0, "uploadMs": 0.25, "latencyMs": null, "path": "cpu"},
            {"t": 75.0, "frameMs": 40.0, "inputMs": 0.5, "buildMs": 2.0, "commandMs": 30.0, "damageMs": 1.0, "compositeMs": 6.0, "uploadMs": 0.5, "latencyMs": null, "path": "cpu"}
        ]
    }"#;

    fn hz(n: u32) -> NonZeroU32 {
        NonZeroU32::new(n).unwrap()
    }

    fn identity() -> Identity {
        Identity {
            run: "r1".into(),
            app_pin: ident("359051359").unwrap(),
            fw_rev: ident("bafb4522").unwrap(),
            os: ident("macos-aarch64").unwrap(),
            runner_image: ident("macos15-20251110.1").unwrap(),
            night: ident("2026-11-21").unwrap(),
            refresh_hz: hz(60),
        }
    }

    fn fixture_records(block: Block) -> Vec<RunRecord> {
        let report: Report = serde_json::from_str(FIXTURE).unwrap();
        let probe = probe(&report.frames, hz(60));
        let workload = workload(&report.context.workload, &report.context, "idle").unwrap();
        records(&identity(), "paint", &workload, block, &probe).unwrap()
    }

    #[test]
    fn a_fixture_run_emits_exact_record_lines() {
        let text = pair::print_records(&fixture_records(Block { unit: 2, position: 1, arm: Arm::AA }));
        let head = "craft-pair/1 test_id=CONF-calibrate run_id=r1.paint.2.1.";
        let identity = "app_pin=359051359 fw_rev=bafb4522 os=macos-aarch64 runner_image=macos15-20251110.1 night=2026-11-21 \
                        fixture_hash=811531d151db2281 mode=native backend=cpu load=idle scale=3000x2000@1920x1080 block=2 arm=AA";
        let line = |gate: &str, samples: &str| format!("{head}{gate} {identity} gate={gate} refresh_hz=60 samples={samples}\n");
        let expected = [
            line("OverheadCpu", "hist:763:1,796:2,1037:1"),
            line("OverheadInterval", "hist:960:2,1040:1"),
            line("UiEqMissed", "count:1/3"),
            line("UiEqLongStalls", "count:0/3"),
            line("UiEqLatency", "hist:924:1"),
        ]
        .concat();
        assert_eq!(text, expected);
    }

    #[test]
    fn the_framework_rev_is_the_craft_package_source_commit() {
        let lock = r#"
version = 4

[[package]]
name = "craft-cms"
version = "0.1.0"
source = "git+https://github.com/storytold/craft-libs?rev=aaaa#aaaa"

[[package]]
name = "craft"
version = "0.1.0"
source = "git+https://github.com/storytold/craft-libs?rev=bafb4522#bafb4522"
"#;
        assert_eq!(framework_rev(lock).as_deref(), Some("bafb4522"));
        assert_eq!(framework_rev("version = 4\n"), None);
    }

    #[test]
    fn the_load_label_follows_the_contention_check() {
        let label = |one_minute: f64| load_label(super::super::contended(Some([one_minute, 0.0, 0.0]), Some(8), 1.0));
        assert_eq!((label(8.0), label(8.5)), ("idle", "contended"));
    }

    #[test]
    fn clearing_removes_stale_records_and_block_reports() {
        let dir = tempfile::tempdir().unwrap();
        let records = dir.path().join(RECORDS);
        let blocks = dir.path().join("pair");
        std::fs::write(&records, "craft-pair/1 stale\n").unwrap();
        std::fs::create_dir_all(&blocks).unwrap();
        std::fs::write(blocks.join("paint-1-0.json"), "{}").unwrap();
        for stale in [&records, &blocks, &dir.path().join(SUMMARY)] {
            clear(stale).unwrap();
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn options() {
        let o = parse_opts(&[]).unwrap();
        assert_eq!((o.workloads, o.units, o.frames, o.refresh_hz.get(), o.skip_build), (vec!["hover".to_owned(), "paint".to_owned()], 3, 3600, 60, false));
        let o = parse_opts(&[
            "--workload",
            "paint",
            "--units",
            "5",
            "--frames",
            "600",
            "--refresh-hz",
            "120",
            "--night",
            "n1",
            "--runner-image",
            "img",
            "--skip-build",
        ])
        .unwrap();
        assert_eq!((o.workloads, o.units, o.frames, o.refresh_hz.get()), (vec!["paint".to_owned()], 5, 600, 120));
        assert_eq!((o.night.as_deref(), o.runner_image.as_deref(), o.skip_build), (Some("n1"), Some("img"), true));
        for bad in [&["--units", "0"][..], &["--frames", "x"], &["--frames", "1"], &["--refresh-hz", "0"], &["--workload", "scroll"], &["--quick"]] {
            assert!(parse_opts(bad).is_err(), "{bad:?}");
        }
    }
}
