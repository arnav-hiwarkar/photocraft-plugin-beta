use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use craft_testkit::pair::frames::{self, Block, Heading, Identity, Opts, Probe, Run};
use craft_testkit::pair::{self, Gate, Ident, Workload};
use serde::Deserialize;

const BENCH: &str = "frame_probe_bench";
const PACKAGE: &str = "photocraft-ui-egui";
const WORKLOADS: [&str; 2] = ["hover", "paint"];
const STAGES: [&str; 6] = ["input", "ui_build", "command", "damage", "composite", "upload"];
const GATES: [Gate; 5] = [Gate::OverheadCpu, Gate::OverheadInterval, Gate::UiEqMissed, Gate::UiEqLongStalls, Gate::UiEqLatency];
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
    fn sample(&self) -> Result<frames::Frame<6>, String> {
        let at = |what: &str, ms: f64| frames::millis(ms).ok_or_else(|| format!("frame at t={}: {what} {ms} ms is not a time", self.t));
        Ok(frames::Frame {
            start: at("t", self.t)?,
            stages: [
                at("inputMs", self.input_ms)?,
                at("buildMs", self.build_ms)?,
                at("commandMs", self.command_ms)?,
                at("damageMs", self.damage_ms)?,
                at("compositeMs", self.composite_ms)?,
                at("uploadMs", self.upload_ms)?,
            ],
            latency: self.latency_ms.map(|ms| at("latencyMs", ms)).transpose()?,
            playback: pair::Count::default(),
            seek: None,
        })
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

pub fn probe(frames: &[Frame], refresh_hz: NonZeroU32) -> Result<Probe<6>, String> {
    Ok(Probe::of(&frames.iter().map(Frame::sample).collect::<Result<Vec<_>, _>>()?, refresh_hz))
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

fn identity(root: &Path, o: &Opts) -> Result<Identity, String> {
    let now = super::now_iso();
    let commit = super::git(root, &["rev-parse", "HEAD"]);
    let dirty = super::git(root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let lock = std::fs::read_to_string(root.join("Cargo.lock")).map_err(|e| format!("read Cargo.lock: {e}"))?;
    let run = format!("{}-{}", now.replace(['-', ':'], ""), std::process::id());
    Identity::of(o, run, commit.as_deref(), dirty, framework_rev(&lock).as_deref(), &now).map_err(|e| e.to_string())
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
    let probe = probe(&report.frames, o.refresh_hz).map_err(|e| format!("{}: {e}", json.display()))?;
    Ok((report.context, probe))
}

pub fn run(root: &Path, rest: &[&str]) -> Result<(), String> {
    let o = Opts::parse(rest, &WORKLOADS).map_err(|e| format!("perf --pair: {e}"))?;
    let budgets = std::fs::read_to_string(root.join(super::BUDGETS)).map_err(|e| format!("read {}: {e}", super::BUDGETS))?;
    let max_load_per_core = super::parse_budgets(&budgets)?.settings.max_load_per_core;
    let cores = std::thread::available_parallelism().map(|n| n.get()).ok();
    let load = frames::load_label(super::contended(super::load_avg(), cores, max_load_per_core));
    let target = crate::target_dir()?;
    let out_dir = target.join("perf");
    let dir = out_dir.join("pair");
    for stale in [out_dir.join(RECORDS), out_dir.join(SUMMARY), dir.clone()] {
        frames::clear(&stale)?;
    }
    if !o.skip_build {
        let mut c = crate::cargo();
        c.args(["build", "--release", "-p", PACKAGE, "--example", BENCH]);
        crate::run(c, &format!("cargo build --release -p {PACKAGE} --example {BENCH}"))?;
    }
    let exe = target.join("release").join("examples").join(format!("{BENCH}{}", std::env::consts::EXE_SUFFIX));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let id = identity(root, &o)?;
    let mut run = Run::default();
    for block in frames::blocks(o.units) {
        for name in &o.workloads {
            let (context, probe) = measure(&exe, &dir, root, &o, name, block)?;
            let workload = workload(name, &context, load)?;
            run.add(&id, name, workload, block, &probe, &GATES).map_err(|e| e.to_string())?;
        }
    }
    let all = run.records();
    let records_path = out_dir.join(RECORDS);
    super::write(&records_path, &pair::print_records(&all))?;
    let shown = records_path.display().to_string();
    let heading = Heading { id: &id, load, units: o.units, frames: o.frames, records: &shown };
    let summary = run.summary(&heading, STAGES).map_err(|e| e.to_string())?;
    super::write(&out_dir.join(SUMMARY), &summary)?;
    println!("\n{summary}");
    println!("wrote {} ({} records)", records_path.display(), all.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use craft_testkit::pair::{Arm, RunRecord};

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
        let probe = probe(&report.frames, hz(60)).unwrap();
        let workload = workload(&report.context.workload, &report.context, "idle").unwrap();
        frames::records(&identity(), "paint", &workload, block, &probe, &GATES).unwrap()
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
        let label = |one_minute: f64| frames::load_label(super::super::contended(Some([one_minute, 0.0, 0.0]), Some(8), 1.0));
        assert_eq!((label(8.0), label(8.5)), ("idle", "contended"));
    }

    #[test]
    fn a_frame_without_a_valid_time_is_an_error() {
        let report = |t: &str| {
            format!(
                r#"{{"context": {{"workload": "paint", "backend": "cpu", "scale": "s"}}, "frames": [{{"t": {t}, "inputMs": 0.5, "buildMs": 0.25, "commandMs": 0.0, "damageMs": 0.0, "compositeMs": 1.0, "uploadMs": 0.25, "latencyMs": -1.0}}]}}"#
            )
        };
        let errors = ["-1.0", "1e14"].map(|t| probe(&serde_json::from_str::<Report>(&report(t)).unwrap().frames, hz(60)).unwrap_err());
        assert_eq!(errors, ["frame at t=-1: t -1 ms is not a time", "frame at t=100000000000000: t 100000000000000 ms is not a time"]);
        let good = probe(&serde_json::from_str::<Report>(&report("2.0")).unwrap().frames, hz(60)).unwrap_err();
        assert_eq!(good, "frame at t=2: latencyMs -1 ms is not a time");
    }
}
