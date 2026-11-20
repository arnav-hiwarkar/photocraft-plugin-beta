//! Where each frame's time goes (#9328), gathered every frame.
//!
//! [`FrameStages`] is one frame's record. Each stage has one writer: the raw input hook starts
//! the frame, `App::run` adds commands, the GPU canvas path adds damage tracking and budgeting,
//! [`crate::gpu_canvas::Perf::record`] adds composites and uploads, and the canvas adds stroke
//! rendering. `App::ui` closes it on the frame's last egui pass ([`FrameProbe::finish`]). A frame
//! over [`SLOW_FRAME_MS`] gets one `info` log line per second at most. The stroke trace
//! (`stroke_trace`) reads the same record, so the app times a frame in one place.
//!
//! The web build does the same: `now_ms()` is `performance.now()` there, and the log line goes to
//! the browser console.

/// A frame slower than this (ms) is logged.
pub const SLOW_FRAME_MS: f64 = 50.0;
pub const RECORDED_FRAMES: usize = 1 << 17;
/// At most one slow-frame log line per this many ms.
const LOG_EVERY_MS: f64 = 1000.0;

/// One frame's stages. Times in ms.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrameStages {
    /// Frame start (the raw input hook), from [`crate::gpu_canvas::now_ms`].
    pub t: f64,
    /// The whole frame: from its start until its UI is built. Never less than the stages below
    /// (a command run between frames counts toward the next one).
    pub frame_ms: f64,
    /// From the raw input hook to the start of the UI build (input handling, minus commands).
    pub input_ms: f64,
    /// Commands run through the app since the previous frame ended.
    pub command_ms: f64,
    /// The last of those commands ("" for none).
    pub command: String,
    /// Damage tracking and GPU memory budgeting on the GPU canvas path.
    pub damage_ms: f64,
    /// Canvas composites, CPU side (on the GPU path: encoding and submitting them).
    pub composite_ms: f64,
    /// Canvas conversions and uploads.
    pub upload_ms: f64,
    /// The GPU's own time for a recent composite, when the canvas is on the GPU and it reports one.
    pub gpu_ms: Option<f64>,
    /// eframe's CPU time for the previous frame minus that frame's UI build: tessellation and
    /// GPU submission (the previous frame's, so it isn't part of `frame_ms`).
    pub present_ms: Option<f64>,
    /// The last canvas refresh's kind ("full", "rect", "gpu-full", …; "" for none).
    pub refresh: &'static str,
    /// Pixels the frame's canvas refreshes covered.
    pub refresh_px: u64,
    /// "gpu" or "cpu": who drew the canvas.
    pub path: &'static str,
    /// Building the UI (`App::ui`, every egui pass of the frame), the canvas refresh included.
    pub ui_ms: f64,
    pub latency_ms: Option<f64>,
    /// Rendering stroke points (the stroke trace's `stroke`).
    #[serde(skip)]
    pub stroke_ms: f64,
    /// A stroke was live in some egui pass of the frame, even one a later pass cancelled (a tool
    /// change) and whose dab rendered no stroke points.
    #[serde(skip)]
    pub stroking: bool,
    /// Pointer moves the frame delivered.
    #[serde(skip)]
    pub moves: usize,
    /// eframe's CPU time for the previous frame.
    #[serde(skip)]
    pub cpu_prev_ms: Option<f64>,
    #[serde(skip)]
    pub injected: bool,
    /// The raw input hook ran for this frame.
    #[serde(skip)]
    started: bool,
    /// The UI build started (a later egui pass of the frame doesn't start it again).
    #[serde(skip)]
    ui_started: bool,
}

impl FrameStages {
    /// The raw input hook: the frame starts at `now` with `moves` pointer moves.
    pub fn start(&mut self, now: f64, moves: usize) {
        self.t = now;
        self.moves = moves;
        self.started = true;
    }

    /// The UI build starts at `now`; `cpu_prev` is eframe's CPU time for the previous frame, in s.
    /// A later egui pass of the same frame keeps the first pass's input time.
    pub fn ui_start(&mut self, now: f64, cpu_prev: Option<f32>) {
        if self.ui_started {
            return;
        }
        self.ui_started = true;
        if !self.started {
            self.start(now, 0);
        }
        self.input_ms = (now - self.t - self.command_ms).max(0.0);
        self.cpu_prev_ms = cpu_prev.map(|s| f64::from(s) * 1000.0);
    }

    /// A command `id` ran for `ms`.
    pub fn command(&mut self, id: &str, ms: f64) {
        self.command_ms += ms;
        id.clone_into(&mut self.command);
    }

    /// A canvas refresh of kind `kind` and `px` pixels.
    pub fn refresh(&mut self, kind: &'static str, px: u64, composite_ms: f64, upload_ms: f64) {
        self.composite_ms += composite_ms;
        self.upload_ms += upload_ms;
        self.refresh = kind;
        self.refresh_px += px;
    }

    /// The stages that make up `frame_ms` (`present_ms` belongs to the previous frame).
    pub fn stage_ms(&self) -> f64 {
        self.input_ms + self.command_ms + self.damage_ms + self.composite_ms + self.upload_ms
    }

    /// The frame ends at `now`.
    fn end(&mut self, now: f64) {
        if !self.started {
            self.t = now;
        }
        self.frame_ms = (now - self.t).max(self.stage_ms());
    }

    /// One line for the log.
    pub fn line(&self) -> String {
        let opt = |v: Option<f64>| v.map_or("-".to_string(), |v| format!("{v:.1}"));
        let command = if self.command.is_empty() { String::new() } else { format!(" ({})", self.command) };
        format!(
            "slow frame: {:.1} ms on the {} path: input {:.1}, command {:.1}{command}, damage {:.1}, composite {:.1}, upload {:.1}, gpu {}, present {}; refresh {} {} px",
            self.frame_ms,
            self.path,
            self.input_ms,
            self.command_ms,
            self.damage_ms,
            self.composite_ms,
            self.upload_ms,
            opt(self.gpu_ms),
            opt(self.present_ms),
            if self.refresh.is_empty() { "none" } else { self.refresh },
            self.refresh_px,
        )
    }
}

#[derive(Debug, Default)]
pub struct FrameProbe {
    recording: Option<Recording>,
    prev_ui_ms: Option<f64>,
    logged_at: Option<f64>,
    unlogged: usize,
}

#[derive(Debug, Default)]
struct Recording {
    frames: Vec<FrameStages>,
    injected_at: Option<f64>,
}

impl Recording {
    fn keep(&mut self, s: &mut FrameStages, now: f64) {
        if s.injected {
            s.latency_ms = self.injected_at.take().map(|at| (now - at).max(0.0));
        }
        if self.frames.len() < RECORDED_FRAMES {
            self.frames.push(s.clone());
        }
    }
}

impl FrameProbe {
    pub fn set_recording(&mut self, on: bool) {
        self.recording = on.then(|| self.recording.take().unwrap_or_default());
    }

    pub fn frames(&self) -> &[FrameStages] {
        self.recording.as_ref().map(|r| r.frames.as_slice()).unwrap_or_default()
    }

    pub fn take_frames(&mut self) -> Vec<FrameStages> {
        self.recording.as_mut().map(|r| std::mem::take(&mut r.frames)).unwrap_or_default()
    }

    pub fn input_injected(&mut self, now: impl FnOnce() -> f64) {
        if let Some(r) = &mut self.recording {
            r.injected_at.get_or_insert_with(now);
        }
    }

    /// Close frame `s` at `now`: `gpu` says the canvas is on the GPU, `gpu_ms` is the GPU's
    /// latest composite time. A slow frame is logged at most once a second.
    pub fn finish(&mut self, mut s: FrameStages, now: f64, gpu: bool, gpu_ms: Option<f64>) -> FrameStages {
        s.end(now);
        let on_gpu = if s.refresh.is_empty() { gpu } else { s.refresh.starts_with("gpu") };
        s.path = if on_gpu { "gpu" } else { "cpu" };
        s.gpu_ms = gpu_ms.filter(|_| on_gpu);
        s.present_ms = s.cpu_prev_ms.zip(self.prev_ui_ms).map(|(cpu, ui)| (cpu - ui).max(0.0));
        self.prev_ui_ms = Some(s.ui_ms);
        if let Some(r) = &mut self.recording {
            r.keep(&mut s, now);
        }
        if let Some(line) = self.slow_line(&s) {
            log::info!("{line}");
        }
        s
    }

    fn slow_line(&mut self, s: &FrameStages) -> Option<String> {
        if s.frame_ms.is_nan() || s.frame_ms <= SLOW_FRAME_MS {
            return None;
        }
        if self.logged_at.is_some_and(|at| s.t - at < LOG_EVERY_MS) {
            self.unlogged += 1;
            return None;
        }
        let mut line = s.line();
        if self.unlogged > 0 {
            line.push_str(&format!(" ({} more slow frames)", self.unlogged));
        }
        self.logged_at = Some(s.t);
        self.unlogged = 0;
        Some(line)
    }
}

impl serde::Serialize for FrameProbe {
    fn serialize<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match &self.recording {
            Some(r) => ser.collect_seq(r.frames.iter()),
            None => ser.serialize_none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn frame(t: f64, ms: f64) -> FrameStages {
        let mut s = FrameStages::default();
        s.start(t, 0);
        s.ui_start(t + 1.0, None);
        s.refresh("full", 100, ms / 2.0, 1.0);
        s
    }

    #[test]
    fn a_recording_probe_keeps_every_frame_with_its_input_latency() {
        let mut p = FrameProbe::default();
        p.set_recording(true);
        p.finish(frame(0.0, 2.0), 16.0, false, None);
        p.input_injected(|| 20.0);
        p.input_injected(|| 25.0);
        let mut consumed = frame(33.0, 2.0);
        consumed.injected = true;
        p.finish(consumed, 41.5, false, None);
        let mut later = frame(50.0, 2.0);
        later.injected = true;
        p.finish(later, 60.0, false, None);
        let json = serde_json::to_value(&p).unwrap();
        let read = |key: &str| json.as_array().unwrap().iter().map(|f| f[key].clone()).collect::<Vec<_>>();
        assert_eq!(read("t"), [json!(0.0), json!(33.0), json!(50.0)]);
        assert_eq!(read("frameMs"), [json!(16.0), json!(8.5), json!(10.0)]);
        assert_eq!(read("latencyMs"), [Value::Null, json!(21.5), Value::Null]);
        assert_eq!(p.take_frames().len(), 3);
        assert_eq!(serde_json::to_value(&p).unwrap(), json!([]));
    }

    #[test]
    fn the_disabled_probe_keeps_nothing_and_reads_no_clock() {
        let reads = std::cell::Cell::new(0);
        let run = |p: &mut FrameProbe| -> Vec<FrameStages> {
            (0..10_000)
                .map(|k| {
                    let t = f64::from(k) * 16.0;
                    p.input_injected(|| {
                        reads.set(reads.get() + 1);
                        t
                    });
                    let mut s = frame(t, 2.0);
                    s.injected = true;
                    p.finish(s, t + 8.0, false, None)
                })
                .collect()
        };
        let mut off = FrameProbe::default();
        let closed_off = run(&mut off);
        assert_eq!((reads.get(), off.frames().len(), off.take_frames().capacity()), (0, 0, 0));
        assert_eq!(serde_json::to_value(&off).unwrap(), Value::Null);
        assert_eq!(closed_off.iter().map(|s| (s.frame_ms, s.latency_ms)).next_back(), Some((8.0, None)));
        let mut on = FrameProbe::default();
        on.set_recording(true);
        let closed_on = run(&mut on);
        assert_eq!((reads.get(), on.frames().len()), (10_000, 10_000));
        assert_eq!(closed_on.iter().filter_map(|s| s.latency_ms).sum::<f64>(), 80_000.0);
    }

    #[test]
    fn recording_stops_keeping_frames_at_its_cap() {
        let mut p = FrameProbe::default();
        p.set_recording(true);
        for k in 0..RECORDED_FRAMES + 5 {
            p.finish(FrameStages::default(), k as f64, false, None);
        }
        assert_eq!(p.frames().len(), 131_072);
        assert_eq!(p.frames().last().map(|f| f.t), Some(131_071.0));
    }

    #[test]
    fn a_fast_frame_logs_nothing() {
        let mut p = FrameProbe::default();
        let s = p.finish(frame(0.0, 0.0), 49.0, true, Some(3.0));
        assert_eq!(s.frame_ms, 49.0);
        assert_eq!(s.path, "cpu", "the refresh says who drew it");
        assert_eq!(s.gpu_ms, None, "no GPU time for a CPU refresh");
        let mut exactly = frame(100.0, 0.0);
        exactly.end(150.0);
        assert_eq!(p.slow_line(&exactly), None, "exactly 50 ms isn't over");
    }

    #[test]
    fn stages_never_exceed_the_frame() {
        let mut r = FrameProbe::default();
        // A command run between frames (before the hook) makes the stages longer than the clock.
        let mut s = FrameStages::default();
        s.command("layer.setProps", 80.0);
        s.start(1000.0, 2);
        s.ui_start(1005.0, Some(0.004));
        s.damage_ms = 2.0;
        s.refresh("gpu-full", 12, 30.0, 10.0);
        s.ui_ms = 40.0;
        let s = r.finish(s, 1050.0, true, Some(7.0));
        assert!(s.stage_ms() <= s.frame_ms, "{s:?}");
        assert_eq!((s.input_ms, s.frame_ms, s.path, s.gpu_ms), (0.0, 122.0, "gpu", Some(7.0)));
        assert_eq!(s.present_ms, None, "no previous frame");
        let mut next = frame(1100.0, 4.0);
        next.cpu_prev_ms = Some(45.0);
        let next = r.finish(next, 1110.0, true, None);
        assert_eq!(next.present_ms, Some(5.0), "the previous frame's CPU time minus its UI build");
        assert!(next.stage_ms() <= next.frame_ms);
    }

    #[test]
    fn slow_frame_log_is_rate_limited() {
        let mut r = FrameProbe::default();
        let lines: Vec<String> = (0..10)
            .filter_map(|k| {
                let mut s = frame(f64::from(k) * 100.0, 0.0);
                s.end(s.t + 60.0);
                r.slow_line(&s)
            })
            .collect();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("slow frame: 60.0 ms") && !lines[0].contains("more"), "{}", lines[0]);
        let mut s = frame(1000.0, 0.0);
        s.end(1070.0);
        let line = r.slow_line(&s).expect("a second later, the next line");
        assert!(line.ends_with("(9 more slow frames)"), "{line}");
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod app_tests {
    use serde_json::json;

    use crate::PhotocraftApp;

    /// One frame of `passes` egui passes of `pass_ms` each, through the app's hooks: the raw input
    /// hook starts the record once, and each pass's UI build ends in `finish_frame`. Every pass but
    /// the last asks egui for another, as a sizing pass does. `stroke`: each pass renders stroke
    /// points for 1 ms. Returns the first pass's input time, before any UI work.
    fn frame(app: &mut PhotocraftApp, ctx: &egui::Context, passes: usize, pass_ms: u64, stroke: bool) -> Option<f64> {
        let mut first_input_ms = None;
        app.perf.stage.start(crate::gpu_canvas::now_ms(), 1);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            let t0 = crate::gpu_canvas::now_ms();
            app.perf.stage.ui_start(t0, None);
            first_input_ms.get_or_insert(app.perf.stage.input_ms);
            std::thread::sleep(std::time::Duration::from_millis(pass_ms));
            if stroke {
                app.perf.stage.stroke_ms += 1.0;
            }
            if ctx.current_pass_index() + 1 < passes {
                ctx.request_discard("a sizing pass");
            }
            app.finish_frame(&ctx, crate::gpu_canvas::now_ms() - t0);
        })
        .drop_without_applying_deltas();
        first_input_ms
    }

    #[test]
    fn two_30_ms_passes_of_one_frame_are_one_60_ms_frame() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.frame_probe.set_recording(true);
        let ctx = app.ctx().clone();
        let first_input_ms = frame(&mut app, &ctx, 2, 30, false);
        let frames = app.frame_probe.take_frames();
        assert_eq!(frames.len(), 1, "{frames:?}");
        let f = &frames[0];
        assert!(f.frame_ms >= 60.0 && f.ui_ms >= 60.0, "both passes: {f:?}");
        // Context initialization or scheduler delays before the first pass are real input time.
        // The invariant is that a later pass does not replace it with the first pass's UI time.
        assert_eq!(Some(f.input_ms), first_input_ms, "the first pass isn't the second's input: {f:?}");
        assert_eq!(app.perf.stage, super::FrameStages::default(), "the last pass closed the record");
    }

    #[test]
    fn the_stroke_trace_gets_one_row_per_frame_not_per_pass() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.stroke_trace.output = crate::stroke_trace::Output::Stderr;
        let ctx = app.ctx().clone();
        for _ in 0..3 {
            assert!(frame(&mut app, &ctx, 2, 1, true).is_some(), "the frame ran at least one UI pass");
        }
        assert!(frame(&mut app, &ctx, 2, 1, false).is_some(), "the frame ran at least one UI pass");
        let s = app.stroke_trace.last.clone().expect("the stroke ended");
        assert_eq!(s.frames, 3, "{s:?}");
        assert_eq!(s.moves, [1.0, 1.0], "each row has its frame's moves: {s:?}");
        assert_eq!(s.stroke, [2.0, 2.0], "both passes' stroke rendering: {s:?}");
    }

    #[test]
    fn a_stroke_live_only_in_a_discarded_pass_still_gets_its_frame_traced() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.stroke_trace.output = crate::stroke_trace::Output::Stderr;
        let ctx = app.ctx().clone();
        app.perf.stage.start(crate::gpu_canvas::now_ms(), 1);
        ctx.run_ui(egui::RawInput::default(), |ui| {
            let ctx = ui.ctx().clone();
            app.perf.stage.ui_start(crate::gpu_canvas::now_ms(), None);
            if ctx.current_pass_index() == 0 {
                // The first pass's live stroke (its `finish_frame` saw `live_stroke`), which the
                // last pass cancels before rendering any stroke points.
                app.perf.stage.stroking = true;
                ctx.request_discard("a sizing pass");
            }
            app.finish_frame(&ctx, 1.0);
        })
        .drop_without_applying_deltas();
        assert!(app.live_stroke.is_none() && app.stroke_trace.last.is_none());
        assert!(frame(&mut app, &ctx, 2, 1, false).is_some(), "the frame ran at least one UI pass");
        let s = app.stroke_trace.last.clone().expect("the stroke ended");
        assert_eq!(s.frames, 1, "the frame whose stroke the last pass cancelled: {s:?}");
    }

    #[test]
    fn an_app_frame_with_a_full_refresh_records_its_stages() {
        let (w, h) = (4000u64, 3000u64);
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": w, "height": h, "background": "white"})).unwrap();
        let layer = s.active().and_then(|d| d.active_layer).expect("the background").0;
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(1440.0, 900.0)).with_max_steps(4).build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            PhotocraftApp::with_context(cc.egui_ctx.clone(), s, crate::Services::default())
        });
        harness.run_steps(6);
        harness.state_mut().frame_probe.set_recording(true);
        harness.state_mut().run("layer.setProps", json!({"layer": layer, "visible": false})).unwrap();
        harness.step();
        let app = harness.state();
        let f = app.frame_probe.frames().iter().rev().find(|f| f.command == "layer.setProps").expect("the toggle's frame is kept").clone();
        // The CPU canvas (kittest has no wgpu) redraws the whole document. A damage refresh counts
        // reduced texels times the factor squared, which can fall a few rows short (see P80).
        assert_eq!(f.path, "cpu", "{f:?}");
        assert!(matches!(f.refresh, "full" | "rect"), "{f:?}");
        assert!(f.refresh_px * 100 >= w * h * 95 && f.refresh_px <= w * h, "{f:?}");
        assert!(f.composite_ms > 0.0 && f.command_ms > 0.0, "{f:?}");
        assert!(f.stage_ms() <= f.frame_ms, "{f:?}");
    }

    #[test]
    fn the_frame_that_consumes_a_synthetic_move_records_its_latency() {
        let mut harness = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).with_max_steps(4).build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            PhotocraftApp::with_context(cc.egui_ctx.clone(), photocraft_engine::Session::new(), crate::Services::default())
        });
        harness.run_steps(2);
        harness.state_mut().frame_probe.set_recording(true);
        let ctx = harness.ctx.clone();
        let (req, _reply) = crate::ControlRequest::new("ui.move", json!({"x": 200, "y": 150}));
        assert!(matches!(crate::control::handle(harness.state_mut(), &ctx, &req), crate::control::Reply::AfterInput));
        harness.run_steps(4);
        let frames = harness.state_mut().frame_probe.take_frames();
        let measured: Vec<f64> = frames.iter().filter_map(|f| f.latency_ms).collect();
        assert_eq!(measured.len(), 1, "{frames:?}");
        assert!(measured.iter().all(|ms| ms.is_finite() && *ms > 0.0), "{measured:?}");
    }
}
