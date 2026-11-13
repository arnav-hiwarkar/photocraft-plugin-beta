//! Shared host form widgets. Values remain in their declared coordinate and color spaces.
use craft_ext_core::form::{Form, FormState, ParamKind, Value};

pub(crate) fn draw(ui: &mut egui::Ui, form: &Form, state: &mut FormState) -> Vec<(String, serde_json::Value)> {
    let mut edits = vec![];
    for p in &form.params {
        if state.hidden.contains(&p.key) || p.visible_when.as_ref().is_some_and(|c| !c.eval(&state.values)) {
            continue;
        }
        let enabled = !state.disabled.contains(&p.key) && p.enabled_when.as_ref().is_none_or(|c| c.eval(&state.values));
        let label = &p.label.english;
        let Some(mut value) = state.values.get(&p.key).cloned().or_else(|| form.defaults().get(&p.key).cloned()) else {
            ui.add_enabled_ui(enabled, |ui| {
                ui.separator();
                ui.strong(label);
            });
            continue;
        };
        let before = value.clone();
        ui.push_id((&form.key, &p.key), |ui| {
            ui.add_enabled_ui(enabled, |ui| {
                ui.horizontal(|ui| {
                    ui.label(label);
                    match (&p.kind, &mut value) {
                        (ParamKind::Range { min, max, step, .. }, Value::Number(v)) => {
                            let mut slider = egui::Slider::new(v, *min..=*max);
                            if let Some(step) = step {
                                slider = slider.step_by(*step);
                            }
                            ui.add(slider);
                        }
                        (ParamKind::Int { min, max, .. }, Value::Number(v)) => {
                            ui.add(egui::DragValue::new(v).range(*min as f64..=*max as f64).max_decimals(0));
                            *v = v.round();
                        }
                        (ParamKind::Number { min, max, .. }, Value::Number(v)) => {
                            ui.add(egui::DragValue::new(v).range(min.unwrap_or(f64::MIN)..=max.unwrap_or(f64::MAX)));
                        }
                        (ParamKind::Angle { .. }, Value::Number(v)) => {
                            ui.add(egui::DragValue::new(v).suffix("°"));
                        }
                        (ParamKind::Bool { .. }, Value::Bool(v)) => {
                            ui.checkbox(v, label);
                        }
                        (ParamKind::Choice { options, .. } | ParamKind::Radio { options, .. }, Value::String(v)) => {
                            if matches!(p.kind, ParamKind::Radio { .. }) {
                                for o in options {
                                    ui.radio_value(v, o.id.clone(), &o.label.english);
                                }
                            } else {
                                let shown = options.iter().find(|o| o.id == *v).map(|o| o.label.english.as_str()).unwrap_or("");
                                egui::ComboBox::from_id_salt("choice").selected_text(shown).show_ui(ui, |ui| {
                                    for o in options {
                                        ui.selectable_value(v, o.id.clone(), &o.label.english);
                                    }
                                });
                            }
                        }
                        (ParamKind::Color { alpha, .. }, Value::Color(v)) => {
                            // Numeric channels preserve HDR and do not clamp working-space RGB.
                            for c in v.iter_mut().take(if *alpha { 4 } else { 3 }) {
                                ui.add(egui::DragValue::new(c).speed(0.01));
                            }
                            if *alpha {
                                v[3] = v[3].clamp(0.0, 1.0);
                            }
                        }
                        (ParamKind::Point { space, .. }, Value::Point(v)) => {
                            for c in v {
                                ui.add(egui::DragValue::new(c));
                            }
                            ui.label(match space {
                                craft_ext_core::form::PointSpace::LayerPixels => "layer px",
                                craft_ext_core::form::PointSpace::ImagePercent => "image %",
                            });
                        }
                        (ParamKind::Text { multiline, .. }, Value::String(v)) => {
                            if *multiline {
                                ui.text_edit_multiline(v);
                            } else {
                                ui.text_edit_singleline(v);
                            }
                        }
                        _ => {}
                    }
                    if let Some(unit) = &p.unit {
                        ui.label(match unit {
                            craft_ext_core::form::Unit::Pixels => "px",
                            craft_ext_core::form::Unit::Percent => "%",
                            craft_ext_core::form::Unit::Degrees => "°",
                            craft_ext_core::form::Unit::Seconds => "s",
                            craft_ext_core::form::Unit::Frames => "frames",
                            craft_ext_core::form::Unit::Custom(s) => s,
                        });
                    }
                    if let Some(help) = &p.help {
                        ui.label("ⓘ").on_hover_text(&help.english);
                    }
                });
            });
        });
        if value != before {
            state.values.insert(p.key.clone(), value.clone());
            if let Ok(v) = serde_json::to_value(value) {
                edits.push((p.key.clone(), v));
            }
        }
    }
    edits
}

/// One in-flight sandbox invocation per dialog. Results from superseded edits are discarded.
pub(crate) fn dialog(ui: &mut egui::Ui, command: &str, fields: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(mut form) = fields.get("__form").cloned().and_then(|v| serde_json::from_value::<Form>(v).ok()) else { return };
    let mut state = FormState {
        rev: fields.get("__form_rev").and_then(|v| v.as_u64()).unwrap_or(0),
        values: form.defaults(),
        hidden: fields.get("__form_hidden").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
        disabled: fields.get("__form_disabled").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
    };
    for p in &form.params {
        if let Some(v) = fields.get(&p.key).cloned().and_then(|v| serde_json::from_value(v).ok()) {
            state.values.insert(p.key.clone(), v);
        }
    }
    draw(ui, &form, &mut state);
    #[cfg(not(target_arch = "wasm32"))]
    {
        type Reply = Result<(Form, Option<craft_ext_core::form::Patch>, Vec<serde_json::Value>), String>;
        type Pending = std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<(craft_ext_core::form::Values, Reply)>>>;
        let id = ui.scope_id().with(("form-worker", command));
        let pending = ui.ctx().data_mut(|d| d.get_temp::<Pending>(id));
        let mut busy = pending.is_some();
        if let Some(pending) = pending {
            if let Ok((sent, result)) = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).try_recv() {
                busy = false;
                ui.ctx().data_mut(|d| d.remove::<Pending>(id));
                if sent == state.values {
                    match result {
                        Ok((next, patch, presets)) => {
                            fields.insert("__presets".into(), serde_json::json!(presets));
                            let mut values = next.defaults();
                            values.extend(state.values.clone());
                            state.values = values;
                            form = next;
                            fields.remove("__form_error");
                            if let Some(patch) = patch {
                                if let Err(error) = form.apply_patch(&mut state, &patch) {
                                    fields.insert("__form_error".into(), serde_json::json!(error));
                                }
                            }
                            fields.insert("__form_sent".into(), serde_json::json!(state.values));
                        }
                        Err(error) => {
                            fields.insert("__form_error".into(), serde_json::json!(error));
                        }
                    }
                } else {
                    fields.remove("__form_sent");
                }
            }
        }
        let signature = serde_json::to_value(&state.values).unwrap_or_default();
        if !busy && fields.get("__form_sent") != Some(&signature) {
            fields.insert("__form_sent".into(), signature);
            let (tx, rx) = std::sync::mpsc::channel();
            let values = state.values.clone();
            let rev = state.rev;
            let command = command.to_string();
            let ctx = ui.ctx().clone();
            match std::thread::Builder::new().name("plugin-form".into()).spawn(move || {
                let result = photocraft_engine::plugin_filters::update_form(&command, values.clone(), rev);
                let _ = tx.send((values, result));
                ctx.request_repaint();
            }) {
                Ok(_) => {
                    ui.ctx().data_mut(|d| d.insert_temp(id, std::sync::Arc::new(std::sync::Mutex::new(rx))));
                }
                Err(error) => {
                    fields.insert("__form_error".into(), serde_json::json!(error.to_string()));
                }
            }
        }
        if busy {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
    for (key, value) in &state.values {
        if let Ok(value) = serde_json::to_value(value) {
            fields.insert(key.clone(), value);
        }
    }
    fields.insert("__form".into(), serde_json::json!(form));
    fields.insert("__form_rev".into(), serde_json::json!(state.rev));
    fields.insert("__form_hidden".into(), serde_json::json!(state.hidden));
    fields.insert("__form_disabled".into(), serde_json::json!(state.disabled));
    if let Some(error) = fields.get("__form_error").and_then(|v| v.as_str()) {
        ui.label(error);
    }
}
