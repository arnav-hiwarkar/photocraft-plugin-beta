use std::cell::RefCell;
use std::path::PathBuf;

use craft::behavior::kurbo::Point;
use craft::behavior::{CanvasView, CancelReason, Cursor, CursorIcon};
use craft::host::{PickRequest as HostPick, Picked};
use craft::theme::variant::Tone;
use craft::ui::buttons::Button;
use craft::ui::contract::{Bind, Props as _, Show as _, ShowContainer as _, Sizing};
use craft::ui::display::{Progress, Shade, Text};
use craft::ui::layout_components::Row;
use craft::ui::popover::Popover;
use craft_ext_host::jobs::{JobInfo, JobState};
use craft::ui::canvas::{CanvasTool, ToolAction, ToolContext, ToolPath, ToolPointer};
use craft::ui::primitives::Draw;
use craft::ui::runtime::env::Label;
use craft::ui::runtime::memory::SaveKey;
use craft::ui::shell::ViewportWindow;
use craft_ext_core::ui::tree::{DrawOp, Gradient, Paint, PathCmd, UiEvent};
use craft_ext_ui::plugin_ui::{EngineChoice, Host, PluginUi, Surface, Xf, paint_ops};
use egui::{Pos2, Rect, pos2};
use photocraft_engine::plugin_ops::{PickMode, PickRequest};
use photocraft_engine::plugin_sessions::PREFIX;
use photocraft_engine::prefs::PluginUiEngine;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::tools::{FamilyTool, ToolScope};
use crate::panels::registry::{self, PluginPanel};
use crate::state::Tool;

const IMAGE_EXTENSIONS: [&str; 9] = ["png", "jpg", "jpeg", "tif", "tiff", "webp", "psd", "gif", "bmp"];
const SESSION_REPAINT: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) struct PhotocraftHost;
impl Host for PhotocraftHost {}

#[derive(Default)]
struct State {
    renderer: Option<PluginUi>,
    awaiting: Option<PickRequest>,
    base_tool: Option<Tool>,
    jobs_open: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

fn engine_choice(app: &PhotocraftApp) -> EngineChoice {
    match app.session.prefs().plug_ins.plugin_ui_engine {
        PluginUiEngine::Egui => EngineChoice::Egui,
        PluginUiEngine::Framework => EngineChoice::Framework,
    }
}

fn with_renderer<R>(choice: EngineChoice, f: impl FnOnce(&mut PluginUi) -> R) -> R {
    STATE.with(|st| {
        let mut st = st.borrow_mut();
        let renderer = match st.renderer.take() {
            Some(renderer) if renderer.choice() == choice => renderer,
            _ => PluginUi::new(choice),
        };
        f(st.renderer.insert(renderer))
    })
}

pub(crate) fn register_panels() -> bool {
    registry::set_plugin_panels(photocraft_engine::plugin_sessions::panels())
}

pub(crate) fn logic(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.session.plugin_picks.interactive = true;
    app.session.pump_plugins();
    if register_panels() {
        crate::panel_docking::reconcile_plugin_panels(app);
    }
    sync_panel_surfaces(app);
    follow_tool_choice(app, ctx);
    answer_picks(app);
    let (notifications, errors) = match app.session.plugin_sessions.as_mut() {
        Some(rt) => {
            if !rt.entries.is_empty() || rt.busy() || rt.jobs.running() > 0 {
                ctx.request_repaint_after(SESSION_REPAINT);
            }
            (std::mem::take(&mut rt.notifications), std::mem::take(&mut rt.errors))
        }
        None => (Vec::new(), Vec::new()),
    };
    for message in notifications {
        crate::notices::post(app, "Plugin notification", vec![message], Tone::Neutral, None);
    }
    for message in errors {
        crate::notices::error(app, message);
    }
}

fn job_status(job: &JobInfo) -> String {
    match &job.state {
        JobState::Running if job.note.is_empty() => tl!("Working…").to_owned(),
        JobState::Running => job.note.clone(),
        JobState::Done(_) => tl!("Done").to_owned(),
        JobState::Failed(why) => crate::i18n::fmt(tl!("Failed: {why}"), &[("why", why)]),
        JobState::Cancelled => tl!("Cancelled").to_owned(),
    }
}

pub(crate) fn status_jobs(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let jobs = app.session.plugin_jobs();
    if jobs.is_empty() {
        STATE.with(|st| st.borrow_mut().jobs_open = false);
        return;
    }
    let running = jobs.iter().filter(|(j, _)| !j.state.finished()).count();
    let title = if running > 0 { crate::i18n::fmt(tl!("Plugin jobs: {n} running"), &[("n", &running.to_string())]) } else { tl!("Plugin jobs").to_owned() };
    let mut open = STATE.with(|st| st.borrow().jobs_open);
    let button = Button::new(title.clone()).show(ui);
    if button.clicked() {
        open = !open;
    }
    let mut cancel = Vec::new();
    let mut clear = false;
    Popover::new(tl!("Plugin jobs").to_owned(), Bind::bound(&mut open)).opened_by(&button.response).show(ui, |ui, _| {
        for (job, plugin) in &jobs {
            Row::new().fill_width().show(ui, |ui, _| {
                Text::new(job.label.clone()).show(ui);
                Text::new(plugin.clone()).shade(Shade::Faint).show(ui);
            });
            if !job.state.finished() {
                let bar = match job.progress {
                    Some(fraction) => Progress::new(fraction),
                    None => Progress::indeterminate(),
                };
                bar.label(job.label.clone()).sizing(Sizing::FillWidth).show(ui);
            }
            Row::new().fill_width().show(ui, |ui, _| {
                Text::new(job_status(job)).shade(Shade::Muted).show(ui);
                if !job.state.finished() && job.cancellable && Button::new(tl!("Cancel")).show(ui).clicked() {
                    cancel.push(job.id);
                }
            });
        }
        if jobs.iter().any(|(j, _)| j.state.finished()) && Button::new(tl!("Clear finished")).show(ui).clicked() {
            clear = true;
        }
    });
    STATE.with(|st| st.borrow_mut().jobs_open = open);
    for id in cancel {
        app.session.plugin_jobs.cancel(id, false);
    }
    if clear {
        app.session.plugin_jobs.clear_finished();
    }
}

fn unclean_exit_notice(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let names = app.session.plugins_after_unclean_exit();
    if names.is_empty() {
        return;
    }
    let mut answer = None;
    let window = ViewportWindow::new(Label::from(tl!("Plugins after an unexpected close").to_owned())).remember(SaveKey::named("photocraft.plugins.unclean"));
    let out = window.show(ui, |ui, _| {
        Text::new(crate::i18n::fmt(tl!("PhotoCraft closed unexpectedly while these plugins were running: {names}"), &[("names", &names.join(", "))])).show(ui);
        Row::new().show(ui, |ui, _| {
            if Button::new(tl!("Start without these plugins")).show(ui).clicked() {
                answer = Some(true);
            }
            if Button::new(tl!("Keep them on")).show(ui).clicked() {
                answer = Some(false);
            }
        });
    });
    if out.close_requested.is_some() {
        answer = answer.or(Some(false));
    }
    if let Some(start_without) = answer
        && let Err(e) = app.session.answer_unclean_exit(start_without)
    {
        crate::notices::error(app, e.to_string());
    }
}

fn sync_panel_surfaces(app: &mut PhotocraftApp) {
    let shown: Vec<(String, bool)> = registry::plugin_panels().into_iter().map(|p| (p.surface, crate::panel_docking::is_shown(app, p.id))).collect();
    let Some(rt) = app.session.plugin_sessions.as_mut() else { return };
    for (key, open) in shown {
        if let Some(surface) = rt.surfaces.get_mut(&key) {
            surface.open = open;
        }
    }
}

pub(crate) fn run_session_command(app: &mut PhotocraftApp, id: &str, params: &Value) -> Result<Value, String> {
    if !id.starts_with(PREFIX) {
        return Err(format!("{id} is not a plugin session command"));
    }
    let Some(panel) = registry::plugin_panels().into_iter().find(|p| p.command == id) else {
        return app.session.execute(id, params.clone()).map_err(|e| e.to_string());
    };
    let drawn = app.session.plugin_sessions.as_ref().is_some_and(|rt| rt.surfaces.contains_key(&panel.surface));
    if !drawn {
        app.session.execute(id, params.clone()).map_err(|e| e.to_string())?;
    }
    let shown = crate::panel_docking::show_panel(app, panel.id, Some(true))?;
    sync_panel_surfaces(app);
    Ok(json!({"panel": panel.id, "shown": shown}))
}

fn event_json(key: &str, event: &UiEvent) -> Value {
    json!({"surface": key, "rev": event.rev, "widget": event.widget, "value": serde_json::to_value(&event.value).unwrap_or(Value::Null)})
}

fn send_events(app: &mut PhotocraftApp, events: Vec<Value>) {
    for event in events {
        if let Err(e) = app.session.post_plugin_ui_event(&event) {
            crate::dialogs::alerts::report_error(app, e.to_string());
        }
    }
}

fn current_events(key: &str, rev: u64, drawn: Vec<UiEvent>) -> Vec<Value> {
    drawn.iter().filter(|e| !e.is_stale(rev)).map(|e| event_json(key, e)).collect()
}

pub(crate) fn panel_body(app: &mut PhotocraftApp, ui: &mut egui::Ui, panel: &PluginPanel) {
    let surface = app.session.plugin_sessions.as_ref().and_then(|rt| rt.surfaces.get(&panel.surface)).cloned();
    let choice = engine_choice(app);
    let events = with_renderer(choice, |renderer| {
        egui::ScrollArea::vertical()
            .id_salt(("plugin-panel", panel.id))
            .auto_shrink(false)
            .show(ui, |ui| match &surface {
                Some(sf) => current_events(&sf.key, sf.rev, renderer.draw(ui, Surface { key: &sf.key, rev: sf.rev, tree: &sf.tree }, &mut PhotocraftHost)),
                None => {
                    renderer.draw_json(ui, &panel.surface, 0, None, &mut PhotocraftHost);
                    Vec::new()
                }
            })
            .inner
    });
    send_events(app, events);
}

pub(crate) fn windows(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    unclean_exit_notice(app, ui);
    let panels: Vec<String> = registry::plugin_panels().into_iter().map(|p| p.surface).collect();
    let surfaces: Vec<_> =
        app.session.plugin_sessions.as_ref().map(|rt| rt.surfaces.values().filter(|s| !panels.contains(&s.key)).cloned().collect()).unwrap_or_default();
    let choice = engine_choice(app);
    let mut events = Vec::new();
    let mut closed = Vec::new();
    with_renderer(choice, |renderer| {
        for sf in surfaces {
            if !sf.open {
                renderer.forget(&sf.key);
                continue;
            }
            let window = ViewportWindow::new(Label::from(sf.title.clone())).remember(SaveKey::named(format!("photocraft.plugin.{}", sf.key)));
            let out = ui
                .push_id(("plugin-window", &sf.key), |ui| {
                    window.show(ui, |ui, _| {
                        let drawn = renderer.draw(ui, Surface { key: &sf.key, rev: sf.rev, tree: &sf.tree }, &mut PhotocraftHost);
                        events.extend(current_events(&sf.key, sf.rev, drawn));
                    })
                })
                .inner;
            if out.close_requested.is_some() {
                renderer.forget(&sf.key);
                closed.push(sf.key);
            }
        }
    });
    if let Some(rt) = app.session.plugin_sessions.as_mut() {
        for key in closed {
            if let Some(surface) = rt.surfaces.get_mut(&key) {
                surface.open = false;
            }
        }
    }
    send_events(app, events);
}

fn follow_tool_choice(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let selected = app.session.plugin_tool.is_some();
    let changed = STATE.with(|st| {
        let mut st = st.borrow_mut();
        match (selected, st.base_tool) {
            (true, None) => {
                st.base_tool = Some(app.ui.tool);
                false
            }
            (true, Some(base)) => base != app.ui.tool,
            (false, _) => {
                st.base_tool = None;
                false
            }
        }
    });
    if changed {
        deselect(app);
        return;
    }
    let escape = selected
        && !app.session.plugin_gesture_active()
        && !app.dialog_open()
        && !ctx.egui_wants_keyboard_input()
        && ctx.input(|i| i.key_pressed(egui::Key::Escape));
    if escape {
        app.session.plugin_tool_escape();
    }
}

pub(crate) fn deselect(app: &mut PhotocraftApp) {
    if app.session.plugin_gesture_active() {
        app.session.plugin_tool_escape();
    }
    app.session.plugin_tool = None;
    STATE.with(|st| st.borrow_mut().base_tool = None);
}

pub(crate) fn tool_selected(app: &PhotocraftApp) -> bool {
    app.session.plugin_tool.is_some()
}

fn host_pick(req: &PickRequest) -> HostPick {
    let pick = match req.mode {
        PickMode::Folder => HostPick::folder("plugin"),
        PickMode::Save => {
            let mut pick = HostPick::save("plugin");
            pick.file_name = Some(req.name.clone());
            pick
        }
        PickMode::Open => HostPick::open("plugin").filter("Images", &IMAGE_EXTENSIONS),
    };
    if req.title.is_empty() { pick } else { pick.title(req.title.clone()) }
}

fn answer_picks(app: &mut PhotocraftApp) {
    let awaiting = STATE.with(|st| st.borrow().awaiting.clone());
    if let Some(req) = awaiting {
        if !app.file_dialog_open() {
            STATE.with(|st| st.borrow_mut().awaiting = None);
            app.session.answer_plugin_pick(&req, None);
        }
        return;
    }
    if app.file_dialog_open() {
        return;
    }
    let Some(req) = app.session.take_plugin_pick() else { return };
    let answered = req.clone();
    let asked = app.ask_file(host_pick(&req), move |app, picked| {
        STATE.with(|st| st.borrow_mut().awaiting = None);
        let path: Option<PathBuf> = match picked {
            Picked::Paths(paths) => paths.into_iter().next(),
            _ => None,
        };
        app.session.answer_plugin_pick(&answered, path);
        Ok(Value::Null)
    });
    match asked {
        Ok(_) => STATE.with(|st| st.borrow_mut().awaiting = Some(req)),
        Err(_) => app.session.answer_plugin_pick(&req, None),
    }
}

struct PluginTool<'a> {
    scope: &'a ToolScope<'a>,
}

pub(crate) fn tool<'a>(scope: &'a ToolScope<'a>) -> FamilyTool<'a> {
    Box::new(PluginTool { scope })
}

impl PluginTool<'_> {
    fn send(&self, phase: &str, pointer: &ToolPointer, cx: &ToolContext<'_>) -> bool {
        let Some(view) = cx.space().map(|space| space.view) else { return false };
        let at = view.to_doc(pointer.pos);
        if !(at.x.is_finite() && at.y.is_finite()) {
            return false;
        }
        let mods = json!({"shift": pointer.mods.shift, "alt": pointer.mods.alt, "ctrl": pointer.mods.ctrl, "cmd": pointer.mods.cmd});
        let extra = json!({"modifiers": mods, "button": "primary", "pressure": pointer.sample.pressure, "zoom": view.zoom});
        self.scope.with_app(|app| match app.session.plugin_tool_event(phase, [at.x, at.y], extra) {
            Ok(()) => app.session.plugin_gesture_active(),
            Err(e) => {
                crate::dialogs::alerts::report_error(app, e.to_string());
                false
            }
        })
    }
}

impl CanvasTool for PluginTool<'_> {
    fn press(&mut self, pointer: &ToolPointer, cx: &mut ToolContext<'_>) -> ToolAction {
        if self.send("begin", pointer, cx) { ToolAction::Claimed } else { ToolAction::Done }
    }

    fn drag(&mut self, pointer: &ToolPointer, cx: &mut ToolContext<'_>) {
        if self.scope.with_app(|app| app.session.plugin_gesture_active()) {
            self.send("update", pointer, cx);
        }
    }

    fn release(&mut self, pointer: &ToolPointer, cx: &mut ToolContext<'_>) {
        if self.scope.with_app(|app| app.session.plugin_gesture_active()) {
            self.send("commit", pointer, cx);
        }
    }

    fn cancel(&mut self, _: CancelReason, _: &mut ToolContext<'_>) {
        self.scope.with_app(|app| {
            if app.session.plugin_gesture_active() {
                app.session.plugin_tool_escape();
            }
        });
    }

    fn paint(&self, _: &Draw<'_>, _: &ToolContext<'_>) {}

    fn cursor(&self, _: &ToolContext<'_>) -> Cursor {
        Cursor::System(CursorIcon::Crosshair)
    }

    fn autoscrolls(&self) -> bool {
        false
    }

    fn command_form(&self) -> ToolPath {
        ToolPath::Command("the plugin tool's session command with points")
    }
}

pub(crate) fn paint_overlay(app: &PhotocraftApp, ui: &egui::Ui, clip: Rect, view: &CanvasView) {
    let painter = ui.painter().with_clip_rect(clip);
    let ops = app.session.plugin_overlays();
    if !ops.is_empty() {
        let screen: Vec<DrawOp> = ops.iter().filter_map(|op| to_screen(op, view)).collect();
        paint_ops(&painter, Xf::new(Pos2::ZERO, 1.0), clip, &screen, &|_| None);
    }
    if let Some(label) = app.session.plugin_tool_label() {
        let t = crate::theme::Tokens::get(painter.ctx());
        let galley = painter.layout_no_wrap(format!("{label} — Esc puts it down"), egui::TextStyle::Small.resolve(ui.style()), t.text);
        let pad = ui.spacing().button_padding;
        let at = clip.min + ui.spacing().item_spacing;
        let chip = Rect::from_min_size(at, galley.size() + 2.0 * pad);
        painter.rect_filled(chip, ui.visuals().widgets.noninteractive.corner_radius, t.chrome);
        painter.galley(chip.min + pad, galley, t.text);
    }
}

fn to_screen(op: &DrawOp, view: &CanvasView) -> Option<DrawOp> {
    let p = |q: [f32; 2]| {
        let s = view.to_screen(Point::new(f64::from(q[0]), f64::from(q[1])));
        [s.x, s.y]
    };
    let len = |v: f32| v * view.zoom_x().abs() as f32;
    let paint = |f: &Paint| match f {
        Paint::Solid(c) => Paint::Solid(*c),
        Paint::Gradient(Gradient::Linear { from, to, stops }) => Paint::Gradient(Gradient::Linear { from: p(*from), to: p(*to), stops: stops.clone() }),
        Paint::Gradient(Gradient::Radial { center, radius, stops }) => Paint::Gradient(Gradient::Radial { center: p(*center), radius: len(*radius), stops: stops.clone() }),
    };
    let axis_aligned = view.rotation.rem_euclid(360.0) == 0.0 && !view.flip_x;
    Some(match op {
        DrawOp::Line(l) => {
            let mut l = l.clone();
            (l.from, l.to) = (p(l.from), p(l.to));
            DrawOp::Line(l)
        }
        DrawOp::Rect(r) if axis_aligned => {
            let mut r = r.clone();
            (r.min, r.max) = (p(r.min), p(r.max));
            r.radius = r.radius.map(len);
            r.fill = paint(&r.fill);
            DrawOp::Rect(r)
        }
        DrawOp::Rect(r) => DrawOp::Polygon(craft_ext_core::ui::tree::PolygonOp {
            points: vec![p(r.min), p([r.max[0], r.min[1]]), p(r.max), p([r.min[0], r.max[1]])],
            fill: Some(paint(&r.fill)),
            stroke: r.stroke,
            stroke_width: r.stroke_width,
        }),
        DrawOp::Circle(c) => {
            let mut c = c.clone();
            c.center = p(c.center);
            c.radius = len(c.radius);
            c.fill = paint(&c.fill);
            DrawOp::Circle(c)
        }
        DrawOp::Text(t) => {
            let mut t = t.clone();
            t.at = p(t.at);
            DrawOp::Text(t)
        }
        DrawOp::Path(path) => {
            let mut path = path.clone();
            for c in &mut path.path {
                match c {
                    PathCmd::Move(a) | PathCmd::Line(a) => *a = p(*a),
                    PathCmd::Quad(q) => q.iter_mut().for_each(|a| *a = p(*a)),
                    PathCmd::Cubic(q) => q.iter_mut().for_each(|a| *a = p(*a)),
                    PathCmd::Close => {}
                }
            }
            path.fill = path.fill.as_ref().map(paint);
            DrawOp::Path(path)
        }
        DrawOp::Polygon(poly) => {
            let mut poly = poly.clone();
            poly.points.iter_mut().for_each(|a| *a = p(*a));
            poly.fill = poly.fill.as_ref().map(paint);
            DrawOp::Polygon(poly)
        }
        DrawOp::Clip(c) => {
            let (a, b) = (p(c.min), p(c.max));
            let r = Rect::from_two_pos(pos2(a[0], a[1]), pos2(b[0], b[1]));
            DrawOp::Clip(craft_ext_core::ui::tree::ClipOp { min: [r.min.x, r.min.y], max: [r.max.x, r.max.y] })
        }
        DrawOp::ResetClip => DrawOp::ResetClip,
        DrawOp::Image(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(zoom: f64, rotation: f64) -> CanvasView {
        let mut view = CanvasView::new(Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0)), Point::new(50.0, 25.0), zoom);
        view.rotation = rotation;
        view
    }

    #[test]
    fn overlay_ops_follow_the_view_and_scale_radii_not_strokes() {
        let line: DrawOp = serde_json::from_value(json!({"op":"line","from":[50,25],"to":[60,25],"width":2,"rgba":[1,0,0,1]})).unwrap();
        let Some(DrawOp::Line(l)) = to_screen(&line, &view(2.0, 0.0)) else { panic!("a line stays a line") };
        assert_eq!(l.from, [100.0, 50.0]);
        assert_eq!(l.to, [120.0, 50.0]);
        assert_eq!(l.width, 2.0);
        let circle: DrawOp = serde_json::from_value(json!({"op":"circle","center":[50,25],"radius":5,"fill":[1,1,1,1]})).unwrap();
        let Some(DrawOp::Circle(c)) = to_screen(&circle, &view(2.0, 0.0)) else { panic!("a circle stays a circle") };
        assert_eq!(c.radius, 10.0);
        let rect: DrawOp = serde_json::from_value(json!({"op":"rect","min":[0,0],"max":[10,10],"fill":[1,1,1,1]})).unwrap();
        assert!(matches!(to_screen(&rect, &view(1.0, 0.0)), Some(DrawOp::Rect(_))));
        assert!(matches!(to_screen(&rect, &view(1.0, 30.0)), Some(DrawOp::Polygon(_))));
    }
}
