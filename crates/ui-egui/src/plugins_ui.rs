//! Plugins in the desktop UI (native only): host-drawn panels and windows through the shared
//! renderer (`craft_ext_ui::plugin_ui`, the engine chosen in Preferences ▸ Plugins), the selected
//! plugin tool on the canvas (pointer gestures in image pixels; Escape cancels and rolls back),
//! canvas overlays, answers to plugins' file questions, and notifications. Plugin code never runs
//! here: widget changes and pointer events go to the plugin runtime, which serves document
//! operations on this (the document) thread.
use std::cell::RefCell;
use std::path::PathBuf;

use craft_ext_core::ui::tree::{DrawOp, Gradient, Paint, PathCmd};
use craft_ext_ui::plugin_ui::{EngineChoice, Host, PluginUi, Surface, Xf, paint_ops};
use egui::{Pos2, Rect, pos2};
use photocraft_engine::plugin_ops::{PickMode, PickRequest};
use photocraft_engine::prefs::PluginUiEngine;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::file_dialog::{FileDialogAnswer, FileDialogRequest};
use crate::state::Tool;

/// What the renderer gets from PhotoCraft. Defaults today: English host words, no framework
/// components (their fallbacks draw), no widget registry.
pub(crate) struct PhotocraftHost;
impl Host for PhotocraftHost {}

#[derive(Default)]
struct State {
    renderer: Option<(EngineChoice, PluginUi)>,
    /// The file question whose dialog is open.
    awaiting: Option<PickRequest>,
    /// The built-in tool when the plugin tool was picked: choosing another puts the plugin tool down.
    base_tool: Option<Tool>,
    /// The last pointer position sent during a gesture, so a still pointer sends nothing.
    last_at: Option<[f64; 2]>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// The engine the app chose (Preferences ▸ Plugins ▸ Plugin UI Engine). Plugins never choose.
fn engine_choice(app: &PhotocraftApp) -> EngineChoice {
    match app.session.prefs().plug_ins.plugin_ui_engine {
        PluginUiEngine::Egui => EngineChoice::Egui,
        PluginUiEngine::Framework => EngineChoice::Framework,
    }
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    // This session has a desktop to ask the person: plugins' file questions open dialogs.
    app.session.plugin_picks.interactive = true;
    app.session.pump_plugins();
    follow_tool_choice(app, ctx);
    answer_picks(app);
    let choice = engine_choice(app);
    let surfaces = app.session.plugin_sessions.as_ref().map(|r| r.surfaces.values().cloned().collect::<Vec<_>>()).unwrap_or_default();
    let mut events = vec![];
    STATE.with(|st| {
        let mut st = st.borrow_mut();
        if st.renderer.as_ref().is_none_or(|(c, _)| *c != choice) {
            st.renderer = Some((choice, PluginUi::new(choice)));
        }
        let Some((_, renderer)) = st.renderer.as_mut() else { return };
        for sf in surfaces {
            if !sf.open {
                renderer.forget(&sf.key);
                continue;
            }
            let mut open = true;
            egui::Window::new(&sf.title).id(egui::Id::unique(("photocraft-plugin-surface", &sf.key))).open(&mut open).resizable(true).default_width(280.0).show(ctx, |ui| {
                for e in renderer.draw(ui, Surface { key: &sf.key, rev: sf.rev, tree: &sf.tree }, &mut PhotocraftHost) {
                    if !e.is_stale(sf.rev) {
                        events.push(json!({"surface": sf.key, "rev": e.rev, "widget": e.widget, "value": serde_json::to_value(&e.value).unwrap_or(Value::Null)}));
                    }
                }
            });
            if !open {
                renderer.forget(&sf.key);
                if let Some(s) = app.session.plugin_sessions.as_mut().and_then(|r| r.surfaces.get_mut(&sf.key)) {
                    s.open = false;
                }
            }
        }
    });
    for event in events {
        if let Err(e) = app.session.execute("plugin.uiEvent", event) {
            app.ui.status = e.to_string();
            app.ui.status_error = true;
        }
    }
    let notifications = if let Some(rt) = app.session.plugin_sessions.as_mut() {
        if !rt.entries.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        std::mem::take(&mut rt.notifications)
    } else {
        vec![]
    };
    for message in notifications {
        crate::notices::post(app, "Plugin notification", vec![message], false, None);
    }
}

/// Keeps the plugin tool and the toolbar honest: picking any built-in tool puts the plugin tool
/// down (a gesture under way rolls back), and Escape cancels a gesture or puts the tool down.
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
    if selected && app.ui.dialogs.is_empty() && !ctx.egui_wants_keyboard_input() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.session.plugin_tool_escape();
        STATE.with(|st| st.borrow_mut().last_at = None);
    }
}

/// Puts the plugin tool down (rolling back a gesture under way).
pub(crate) fn deselect(app: &mut PhotocraftApp) {
    if app.session.plugin_gesture_active() {
        app.session.plugin_tool_escape();
    }
    app.session.plugin_tool = None;
    STATE.with(|st| {
        let mut st = st.borrow_mut();
        st.base_tool = None;
        st.last_at = None;
    });
}

/// Whether a plugin tool is selected (the toolbar shows no built-in tool as current then).
pub(crate) fn tool_selected(app: &PhotocraftApp) -> bool {
    app.session.plugin_tool.is_some()
}

/// Shows the file question a plugin asked once its event committed; a closed dialog that never
/// answered is a Cancel.
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
    let request = match req.mode {
        PickMode::Folder => FileDialogRequest::Folder { initial_dir: None },
        PickMode::Save => FileDialogRequest::Save { suggested: req.name.clone() },
        PickMode::Open => FileDialogRequest::Open {
            multiple: false,
            initial_dir: None,
            extensions: Some(["png", "jpg", "jpeg", "tif", "tiff", "webp", "psd", "gif", "bmp"].iter().map(|s| s.to_string()).collect()),
        },
    };
    let answered = req.clone();
    let asked = app.ask_file(request, move |app, answer| {
        STATE.with(|st| st.borrow_mut().awaiting = None);
        let path = match answer {
            FileDialogAnswer::Paths(p) => p.into_iter().next(),
            FileDialogAnswer::SaveTo(p) | FileDialogAnswer::FolderTo(p) => Some(p),
            _ => None,
        };
        app.session.answer_plugin_pick(&answered, path.map(PathBuf::from));
        Ok(Value::Null)
    });
    match asked {
        Ok(_) => STATE.with(|st| st.borrow_mut().awaiting = Some(req)),
        Err(_) => app.session.answer_plugin_pick(&req, None),
    }
}

/// The selected plugin tool takes the canvas's primary-button gestures (the caller has already
/// left Space, the middle button, held zoom keys and Free Transform to the canvas). Sends begin on
/// the press, at most one update per frame while the pointer moves, and commit on release.
pub(crate) fn canvas_pointer(app: &mut PhotocraftApp, ctx: &egui::Context, response: &egui::Response, xf: &ViewXform) {
    if response.hovered() || app.session.plugin_gesture_active() {
        ctx.set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let (pressed, down, released, mods, pos, origin) = ctx.input(|i| {
        (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.primary_released(), i.modifiers, i.pointer.interact_pos(), i.pointer.press_origin())
    });
    let mods = json!({"shift": mods.shift, "alt": mods.alt, "ctrl": mods.ctrl, "cmd": mods.command});
    let extra = json!({"modifiers": mods, "button": "primary", "pressure": app.stylus.pressure(), "zoom": xf.zoom});
    let report = |app: &mut PhotocraftApp, r: photocraft_engine::Result<()>| {
        if let Err(e) = r {
            app.ui.status = e.to_string();
            app.ui.status_error = true;
        }
    };
    if pressed && !app.session.plugin_gesture_active() {
        if let Some(p) = origin.filter(|p| response.rect.contains(*p) && response.contains_pointer()) {
            let at = xf.to_doc(p);
            STATE.with(|st| st.borrow_mut().last_at = Some(at));
            let r = app.session.plugin_tool_event("begin", at, extra.clone());
            report(app, r);
        }
        return;
    }
    if !app.session.plugin_gesture_active() {
        return;
    }
    let at = pos.map(|p| xf.to_doc(p));
    if released || !down {
        let at = at.or_else(|| STATE.with(|st| st.borrow().last_at)).unwrap_or([0.0, 0.0]);
        STATE.with(|st| st.borrow_mut().last_at = None);
        let r = app.session.plugin_tool_event("commit", at, extra);
        report(app, r);
        return;
    }
    if let Some(at) = at {
        let moved = STATE.with(|st| {
            let mut st = st.borrow_mut();
            let moved = st.last_at != Some(at);
            st.last_at = Some(at);
            moved
        });
        if moved {
            let r = app.session.plugin_tool_event("update", at, extra);
            report(app, r);
        }
    }
    ctx.request_repaint();
}

/// Paints the plugins' canvas overlays (image pixels) and the selected tool's name.
pub(crate) fn paint_overlay(app: &PhotocraftApp, painter: &egui::Painter, xf: &ViewXform, clip: Rect) {
    let ops = app.session.plugin_overlays();
    if !ops.is_empty() {
        let screen: Vec<DrawOp> = ops.iter().filter_map(|op| to_screen(op, xf)).collect();
        paint_ops(painter, Xf::new(Pos2::ZERO, 1.0), clip, &screen, &|_| None);
    }
    if let Some(label) = app.session.plugin_tool_label() {
        let t = crate::theme::Tokens::get(painter.ctx());
        let text = format!("{label} — Esc puts it down");
        let galley = painter.layout_no_wrap(text, egui::FontId::proportional(12.0), t.text);
        let r = Rect::from_min_size(clip.min + egui::vec2(8.0, 8.0), galley.size() + egui::vec2(12.0, 6.0));
        painter.rect_filled(r, 4.0, t.chrome);
        painter.galley(r.min + egui::vec2(6.0, 3.0), galley, t.text);
    }
}

/// An overlay op in image pixels mapped to screen points through the view (zoom, pan, rotation,
/// flip, pixel aspect). Lengths (radii) scale with the zoom; stroke widths and text sizes stay in
/// screen points. Rotated rectangles become polygons.
fn to_screen(op: &DrawOp, xf: &ViewXform) -> Option<DrawOp> {
    let p = |q: [f32; 2]| {
        let s = xf.to_screen(q[0], q[1]);
        [s.x, s.y]
    };
    let len = |v: f32| v * xf.zoom_x().abs();
    let paint = |f: &Paint| match f {
        Paint::Solid(c) => Paint::Solid(*c),
        Paint::Gradient(Gradient::Linear { from, to, stops }) => Paint::Gradient(Gradient::Linear { from: p(*from), to: p(*to), stops: stops.clone() }),
        Paint::Gradient(Gradient::Radial { center, radius, stops }) => Paint::Gradient(Gradient::Radial { center: p(*center), radius: len(*radius), stops: stops.clone() }),
    };
    let axis_aligned = xf.rotation.rem_euclid(360.0) == 0.0 && !xf.flip;
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
        // Overlays carry no images (`validate_draw_list` refuses them).
        DrawOp::Image(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xf(zoom: f32, rotation: f32) -> ViewXform {
        ViewXform { rect: Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0)), zoom, center: [50.0, 25.0], flip: false, rotation, aspect: 1.0 }
    }

    #[test]
    fn overlay_ops_follow_the_view_and_scale_radii_not_strokes() {
        let line: DrawOp = serde_json::from_value(json!({"op":"line","from":[50,25],"to":[60,25],"width":2,"rgba":[1,0,0,1]})).unwrap();
        let DrawOp::Line(l) = to_screen(&line, &xf(2.0, 0.0)).unwrap() else { panic!() };
        // The view's centre is the document's [50, 25]; ten pixels at zoom 2 are 20 points.
        assert!((l.to[0] - l.from[0] - 20.0).abs() < 1e-3, "{l:?}");
        assert_eq!(l.width, 2.0);
        let circle: DrawOp = serde_json::from_value(json!({"op":"circle","center":[50,25],"radius":5,"fill":[1,1,1,1]})).unwrap();
        let DrawOp::Circle(c) = to_screen(&circle, &xf(2.0, 0.0)).unwrap() else { panic!() };
        assert!((c.radius - 10.0).abs() < 1e-3);
        let rect: DrawOp = serde_json::from_value(json!({"op":"rect","min":[0,0],"max":[10,10],"fill":[1,1,1,1]})).unwrap();
        assert!(matches!(to_screen(&rect, &xf(1.0, 0.0)), Some(DrawOp::Rect(_))));
        assert!(matches!(to_screen(&rect, &xf(1.0, 30.0)), Some(DrawOp::Polygon(_))), "a rotated view turns rectangles");
    }
}
