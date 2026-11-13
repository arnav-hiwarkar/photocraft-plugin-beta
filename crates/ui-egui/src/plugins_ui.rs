//! Host-drawn plugin surfaces; guest code stays on workers.
use crate::PhotocraftApp;
use craft_ext_core::ui::{DrawOp, Node};
use serde_json::{Value, json};
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    app.session.pump_plugins();
    let surfaces = app.session.plugin_sessions.as_ref().map(|r| r.surfaces.values().filter(|s| s.open).cloned().collect::<Vec<_>>()).unwrap_or_default();
    let mut events = vec![];
    for sf in surfaces {
        let mut open = true;
        egui::Window::new(&sf.title).id(egui::Id::unique(("plugin-surface", &sf.key))).open(&mut open).show(ctx, |ui| {
            let mut out = vec![];
            node_ui(ui, &sf.key, sf.rev, &sf.tree, &mut out);
            for (widget, value) in out {
                events.push(json!({"surface":sf.key,"rev":sf.rev,"widget":widget,"value":value}));
            }
        });
        if !open {
            if let Some(sf) = app.session.plugin_sessions.as_mut().and_then(|r| r.surfaces.get_mut(&sf.key)) {
                sf.open = false;
            }
        }
    }
    for event in events {
        let _ = app.session.execute("plugin.uiEvent", event);
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
fn rgba(c: [f32; 4]) -> egui::Color32 {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(u(c[0]), u(c[1]), u(c[2]), u(c[3]))
}

/// Draws one validated tree; widget changes are pushed to `out` as (widget id, value).
fn node_ui(ui: &mut egui::Ui, key: &str, rev: u64, n: &Node, out: &mut Vec<(String, Value)>) {
    let id = match n {
        Node::Button { id, .. }
        | Node::Checkbox { id, .. }
        | Node::Slider { id, .. }
        | Node::TextInput { id, .. }
        | Node::Select { id, .. }
        | Node::Color { id, .. }
        | Node::Canvas { id, .. } => Some(id),
        _ => None,
    };
    if let Some(id) = id {
        ui.push_id((key, id), |ui| node_ui_inner(ui, key, rev, n, out));
    } else {
        node_ui_inner(ui, key, rev, n, out);
    }
}
fn node_ui_inner(ui: &mut egui::Ui, key: &str, rev: u64, n: &Node, out: &mut Vec<(String, Value)>) {
    match n {
        Node::Component { fallback, .. } => node_ui(ui, key, rev, fallback, out),
        Node::Form { form, values } => {
            let edit_id = egui::Id::unique(("plugin-form", key, &form.key, rev));
            let mut state = ui.ctx().data_mut(|d| d.get_temp::<craft_ext_core::form::FormState>(edit_id))
                .unwrap_or_else(|| { let mut all = form.defaults(); all.extend(values.clone()); craft_ext_core::form::FormState { values: all, ..Default::default() } });
            out.extend(crate::plugin_form::draw(ui, form, &mut state));
            ui.ctx().data_mut(|d| d.insert_temp(edit_id, state));
        }
        Node::Column { children } => {
            ui.vertical(|ui| children.iter().for_each(|c| node_ui(ui, key, rev, c, out)));
        }
        Node::Row { children } => {
            ui.horizontal_wrapped(|ui| children.iter().for_each(|c| node_ui(ui, key, rev, c, out)));
        }
        Node::Group { label, children } => {
            ui.group(|ui| {
                ui.label(egui::RichText::new(label).strong());
                children.iter().for_each(|c| node_ui(ui, key, rev, c, out));
            });
        }
        Node::Label { text } => {
            ui.label(text);
        }
        Node::Separator => {
            ui.separator();
        }
        Node::Button { id, label } => {
            let r = ui.button(label);
            if r.clicked() {
                out.push((id.clone(), Value::Null));
            }
        }
        Node::Checkbox { id, label, value } => {
            let mut v = *value;
            let r = ui.checkbox(&mut v, label);
            if r.changed() {
                out.push((id.clone(), json!(v)));
            }
        }
        Node::Slider { id, label, value, min, max } => {
            let edit_id = egui::Id::unique(("plugin-slider-edit", key, id));
            let (mut v, dragging, old_rev) = ui.ctx().data_mut(|d| d.get_temp::<(f64, bool, u64)>(edit_id)).unwrap_or((*value, false, rev));
            if !dragging && old_rev != rev {
                v = *value;
            }
            let r = ui.push_id((key, id), |ui| ui.add(egui::Slider::new(&mut v, *min..=*max).text(label))).inner;
            let active = r.dragged() || r.is_pointer_button_down_on();
            ui.ctx().data_mut(|d| d.insert_temp(edit_id, (v, active, rev)));
            if r.drag_stopped() || (dragging && !active) || (r.changed() && !active) {
                out.push((id.clone(), json!(v)));
            }
        }
        Node::TextInput { id, label, value } => {
            let edit_id = egui::Id::unique(("plugin-text-edit", key, rev, id));
            let (mut v, mut sent) = ui.ctx().data_mut(|d| d.get_temp::<(String, String)>(edit_id)).unwrap_or((value.clone(), value.clone()));
            ui.horizontal(|ui| {
                ui.label(label);
                let r = ui.push_id((key, rev, id), |ui| ui.text_edit_singleline(&mut v)).inner;
                if (r.lost_focus() || (r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && v != sent {
                    out.push((id.clone(), json!(v)));
                    sent = v.clone();
                }
            });
            ui.ctx().data_mut(|d| d.insert_temp(edit_id, (v, sent)));
        }
        Node::Select { id, label, options, selected } => {
            ui.label(label);
            let mut sel = *selected;
            let shown = options.get(sel as usize).cloned().unwrap_or_default();
            egui::ComboBox::from_id_salt(("plugin-select", key, id)).selected_text(shown).show_ui(ui, |ui| {
                for (i, o) in options.iter().enumerate() {
                    ui.selectable_value(&mut sel, i as u32, o);
                }
            });
            if sel != *selected {
                out.push((id.clone(), json!(sel)));
            }
        }
        Node::Color { id, label, rgba: c } => {
            ui.horizontal(|ui| {
                ui.label(label);
                let mut v = *c;
                let r = ui.color_edit_button_rgba_unmultiplied(&mut v);
                if r.changed() {
                    out.push((id.clone(), json!(v)));
                }
            });
        }
        Node::Image { width, height, rgba: px } => {
            let img = egui::ColorImage::from_rgba_unmultiplied([*width as usize, *height as usize], px);
            let tex = ui.ctx().load_texture(format!("plugin-image-{key}-{width}x{height}"), img, egui::TextureOptions::LINEAR);
            ui.image((tex.id(), egui::vec2(*width as f32, *height as f32)));
        }
        Node::Canvas { id, label, width, height, ops } => {
            ui.label(label);
            let (r, p) = ui.allocate_painter(egui::vec2(*width, *height), egui::Sense::click_and_drag());
            let o = r.rect.min;
            let at = |q: [f32; 2]| egui::pos2(o.x + q[0], o.y + q[1]);
            for op in ops {
                match op {
                    DrawOp::Line { from, to, width, rgba: c } => {
                        p.line_segment([at(*from), at(*to)], egui::Stroke::new(*width, rgba(*c)));
                    }
                    DrawOp::Rect { min, max, fill, stroke } => {
                        let rect = egui::Rect::from_min_max(at(*min), at(*max));
                        p.rect_filled(rect, 0.0, rgba(*fill));
                        if let Some(s) = stroke {
                            p.rect_stroke(rect, 0.0, egui::Stroke::new(1.0, rgba(*s)), egui::StrokeKind::Inside);
                        }
                    }
                    DrawOp::Circle { center, radius, fill } => {
                        p.circle_filled(at(*center), *radius, rgba(*fill));
                    }
                    DrawOp::Text { at: a, text, size, rgba: c } => {
                        p.text(at(*a), egui::Align2::LEFT_TOP, text, egui::FontId::proportional(size.clamp(4.0, 96.0)), rgba(*c));
                    }
                }
            }
            let kind = if r.drag_started() {
                Some("down")
            } else if r.dragged() {
                Some("drag")
            } else if r.drag_stopped() || r.clicked() {
                Some("up")
            } else {
                None
            };
            if let (Some(kind), Some(pos)) = (kind, r.interact_pointer_pos()) {
                out.push((id.clone(), json!({"kind": kind, "at": [pos.x - o.x, pos.y - o.y]})));
            }
        }
    }
}
