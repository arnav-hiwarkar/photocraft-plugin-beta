use std::path::PathBuf;
use std::time::{Duration, Instant};

use craft::ui::runtime::windows::WindowHost;
use photocraft_ui_egui::control::{Reply, handle};
use photocraft_ui_egui::frame_stages::RECORDED_FRAMES;
use photocraft_ui_egui::{ControlRequest, PhotocraftApp, Services, Tool};
use serde_json::{Value, json};

type H = egui_kittest::Harness<'static, Box<PhotocraftApp>>;

const WINDOW: [f32; 2] = [1920.0, 1080.0];
const DOCUMENT: [u32; 2] = [3000, 2000];
const FIRST_VIEW_FRAMES: u32 = 4;
const WARMUP_FRAMES: u32 = 30;
const STROKE_STEPS: u32 = 120;

#[derive(Clone, Copy)]
enum Workload {
    Hover,
    Paint,
}

impl Workload {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "hover" => Ok(Workload::Hover),
            "paint" => Ok(Workload::Paint),
            other => Err(format!("unknown workload `{other}`: hover or paint")),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Workload::Hover => "hover",
            Workload::Paint => "paint",
        }
    }

    fn request(self, step: u32) -> (&'static str, Value) {
        let turn = f64::from(step) / f64::from(STROKE_STEPS) * std::f64::consts::TAU;
        let at = |extra: Value| {
            let mut p = json!({"x": 1500.0 + 600.0 * turn.cos(), "y": 1000.0 + 400.0 * (2.0 * turn).sin(), "coordinates": "document"});
            if let (Some(p), Some(extra)) = (p.as_object_mut(), extra.as_object()) {
                p.extend(extra.clone());
            }
            p
        };
        match (self, step % STROKE_STEPS) {
            (Workload::Hover, _) => ("ui.move", at(json!({}))),
            (Workload::Paint, 0) => ("ui.click", at(json!({"hold": true}))),
            (Workload::Paint, last) if last + 1 == STROKE_STEPS => ("ui.release", at(json!({}))),
            (Workload::Paint, _) => ("ui.move", at(json!({}))),
        }
    }
}

struct Args {
    workload: Workload,
    frames: u32,
    refresh_hz: u32,
    json: Option<PathBuf>,
}

fn value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let number = |name: &str, default: u32| match value(args, name) {
        None => Ok(default),
        Some(v) => v.parse::<u32>().ok().filter(|n| *n > 0).ok_or(format!("{name} needs a whole number above 0")),
    };
    let frames = number("--frames", 3600)?;
    if usize::try_from(frames).unwrap_or(usize::MAX) > RECORDED_FRAMES {
        return Err(format!("--frames is at most {RECORDED_FRAMES}, what the frame probe keeps"));
    }
    Ok(Args {
        workload: Workload::parse(value(args, "--workload").unwrap_or("hover"))?,
        frames,
        refresh_hz: number("--refresh-hz", 60)?,
        json: value(args, "--json").map(PathBuf::from),
    })
}

struct Display {
    period: Duration,
    next: Instant,
}

impl Display {
    fn present(&mut self, h: &mut H) {
        let wait = self.next.saturating_duration_since(Instant::now());
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
        h.step();
        let done = Instant::now();
        while self.next <= done {
            self.next += self.period;
        }
    }
}

fn inject(h: &mut H, workload: Workload, step: &mut u32) -> Result<(), String> {
    let ctx = h.ctx.clone();
    if !WindowHost::of(&ctx).injected_idle() {
        return Ok(());
    }
    let (method, params) = workload.request(*step);
    let (req, _reply) = ControlRequest::new(method, params);
    match handle(h.state_mut(), &ctx, &req) {
        Reply::AfterInput => {
            *step += 1;
            Ok(())
        }
        Reply::Done(v) => Err(format!("{method}: {v}")),
        _ => Err(format!("{method}: no synthetic input queued")),
    }
}

fn run(args: &Args) -> Result<Value, String> {
    let mut session = photocraft_engine::Session::new();
    session.execute("file.new", json!({"width": DOCUMENT[0], "height": DOCUMENT[1], "background": "white"})).map_err(|e| format!("file.new: {e}"))?;
    let mut h: H = egui_kittest::Harness::builder().with_size(egui::vec2(WINDOW[0], WINDOW[1])).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        PhotocraftApp::with_context(cc.egui_ctx.clone(), session, Services::default())
    });
    h.state_mut().ui.tool = Tool::Brush;
    let mut display = Display { period: Duration::from_secs(1) / args.refresh_hz, next: Instant::now() };
    let mut step = 0;
    for _ in 0..FIRST_VIEW_FRAMES {
        display.present(&mut h);
    }
    for _ in 0..WARMUP_FRAMES {
        inject(&mut h, args.workload, &mut step)?;
        display.present(&mut h);
    }
    h.state_mut().frame_probe.set_recording(true);
    for _ in 0..args.frames {
        inject(&mut h, args.workload, &mut step)?;
        display.present(&mut h);
    }
    let app = h.state_mut();
    let frames = app.frame_probe.take_frames();
    let backend = if app.perf.gpu { "gpu" } else { "cpu" };
    let measured = frames.iter().filter(|f| f.latency_ms.is_some()).count();
    eprintln!("{}: {} frames, {measured} with input latency, {step} requests, {backend} canvas", args.workload.name(), frames.len());
    Ok(json!({
        "context": {
            "workload": args.workload.name(),
            "backend": backend,
            "scale": format!("{}x{}@{}x{}", DOCUMENT[0], DOCUMENT[1], WINDOW[0], WINDOW[1]),
            "refresh_hz": args.refresh_hz,
        },
        "frames": frames,
    }))
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    let args = parse_args(&args)?;
    let report = run(&args)?;
    if let Some(path) = &args.json {
        let text = serde_json::to_string(&report).map_err(|e| format!("encode: {e}"))?;
        std::fs::write(path, text).map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(())
}
