//! Plugin filter dialogs. The controls are the shared renderer's [`FormView`]
//! (`craft_ext_ui::plugin_ui`), drawn with the engine PhotoCraft chose; this module keeps the dialog's
//! fields in step with it and runs the plugin's `craft_update` on a worker. Values stay in their
//! declared coordinate and colour spaces.

/// One in-flight sandbox invocation per dialog. Results from superseded edits are discarded.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn dialog(ui: &mut egui::Ui, command: &str, fields: &mut serde_json::Map<String, serde_json::Value>) {
    use craft_ext_core::form::{FORM_SCHEMA, Form, Patch, PatchOp, Value, Values};
    use craft_ext_ui::plugin_ui::FormView;

    let Some(form) = fields.get("__form").cloned().and_then(|v| serde_json::from_value::<Form>(v).ok()) else { return };
    let given: Values = form.params.iter().filter_map(|p| Some((p.key.clone(), fields.get(&p.key).cloned().and_then(|v| serde_json::from_value::<Value>(v).ok())?))).collect();
    let view_id = egui::Id::unique(("photocraft-plugin-form-view", command));
    let mut view = ui
        .ctx()
        .data_mut(|d| d.get_temp::<FormView>(view_id))
        .filter(|v| v.form().key == form.key)
        .unwrap_or_else(|| FormView::new(command, form.clone(), &given));
    // Values the dialog changed outside the form (a preset, Reset) reach the view as a host patch,
    // which keeps the plugin's hidden and disabled rows.
    let external: Vec<PatchOp> =
        given.iter().filter(|(k, v)| view.values().get(*k) != Some(*v)).map(|(k, v)| PatchOp::Set { key: k.clone(), value: v.clone() }).collect();
    if !external.is_empty() {
        let rev = view.state().rev;
        let _ = view.apply_patch(&Patch { schema: FORM_SCHEMA, base_rev: rev, rev: rev.saturating_add(1), ops: external });
    }
    let mut host = crate::plugins_ui::PhotocraftHost;
    view.show(ui, &mut host);

    type Reply = Result<(Form, Option<Patch>, Vec<serde_json::Value>), String>;
    type Pending = std::sync::Arc<std::sync::Mutex<std::sync::mpsc::Receiver<(Values, Reply)>>>;
    let id = ui.scope_id().with(("form-worker", command));
    let pending = ui.ctx().data_mut(|d| d.get_temp::<Pending>(id));
    let mut busy = pending.is_some();
    if let Some(pending) = pending {
        if let Ok((sent, result)) = pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner).try_recv() {
            busy = false;
            ui.ctx().data_mut(|d| d.remove::<Pending>(id));
            if &sent == view.values() {
                match result {
                    Ok((next, patch, presets)) => {
                        fields.insert("__presets".into(), serde_json::json!(presets));
                        view.replace_form(next);
                        fields.remove("__form_error");
                        if let Some(patch) = patch {
                            if let Err(error) = view.apply_patch(&patch) {
                                fields.insert("__form_error".into(), serde_json::json!(error));
                            }
                        }
                        fields.insert("__form_sent".into(), serde_json::json!(view.values()));
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
    let signature = serde_json::to_value(view.values()).unwrap_or_default();
    if !busy && fields.get("__form_sent") != Some(&signature) {
        fields.insert("__form_sent".into(), signature);
        let (tx, rx) = std::sync::mpsc::channel();
        let values = view.values().clone();
        let rev = view.state().rev;
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
    for (key, value) in view.values() {
        if let Ok(value) = serde_json::to_value(value) {
            fields.insert(key.clone(), value);
        }
    }
    fields.insert("__form".into(), serde_json::json!(view.form()));
    fields.insert("__form_rev".into(), serde_json::json!(view.state().rev));
    fields.insert("__form_hidden".into(), serde_json::json!(view.state().hidden));
    fields.insert("__form_disabled".into(), serde_json::json!(view.state().disabled));
    if let Some(error) = fields.get("__form_error").and_then(|v| v.as_str()) {
        ui.label(error);
    }
    ui.ctx().data_mut(|d| d.insert_temp(view_id, view));
}

/// The web build runs no plugins, so it never opens a plugin filter dialog.
#[cfg(target_arch = "wasm32")]
pub(crate) fn dialog(ui: &mut egui::Ui, _command: &str, _fields: &mut serde_json::Map<String, serde_json::Value>) {
    ui.label("Plugins run in the desktop app.");
}
