use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, PoisonError};

use craft::ui::patterns::form_dialogs::{DialogFields, FieldValue, Param, ParamKind};
use craft::ui::runtime::env::Label;
use craft_ext_core::form::{FORM_SCHEMA, Form, Patch, PatchOp, Value as FormValue, Values};
use craft_ext_ui::plugin_ui::FormView;
use serde_json::{Map, Value, json};

const FORM: &str = "pluginForm";
const VALUES: &str = "pluginValues";
const PRESET: &str = "preset";
const APPLIED: &str = "presetApplied";
const BUSY_POLL: std::time::Duration = std::time::Duration::from_millis(50);

static OPENED: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct Preset {
    id: String,
    label: String,
    values: Map<String, Value>,
}

type Reply = Result<(Form, Option<Patch>), String>;

#[derive(Clone, Default)]
struct Worker {
    pending: Option<Arc<Mutex<Receiver<(Values, Reply)>>>>,
    sent: Option<Values>,
    error: Option<String>,
}

pub(crate) fn rows(command: &str) -> Result<(Label, Vec<Param>, DialogFields), String> {
    let unavailable = || format!("{command} is not an enabled plugin filter");
    let spec = photocraft_engine::plugin_filters::find(command).ok_or_else(unavailable)?;
    let mut opening = photocraft_engine::plugin_filters::dialog_fields(command).ok_or_else(unavailable)?;
    let form = opening.remove("__form").ok_or_else(unavailable)?;
    serde_json::from_value::<Form>(form.clone()).map_err(|e| format!("{command} has an invalid form: {e}"))?;
    let presets: Vec<Preset> = opening
        .remove("__presets")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|p| {
            Some(Preset { id: p.get("id")?.as_str()?.to_owned(), label: p.get("label")?.as_str()?.to_owned(), values: p.get("fields")?.as_object()?.clone() })
        })
        .collect();
    let mut fields = DialogFields::new();
    fields.set(FORM, FieldValue::Json(form));
    fields.set(VALUES, FieldValue::Json(Value::Object(opening)));
    let mut params = Vec::new();
    if !presets.is_empty() {
        let mut options = vec![(String::new(), Label::from(tl!("Custom").to_string()))];
        options.extend(presets.iter().map(|p| (p.id.clone(), Label::from(p.label.clone()))));
        params.push(Param::new(PRESET, Label::from(tl!("Preset").to_string()), ParamKind::choice(options, Some(String::new()))).not_remembered());
    }
    let salt = (command.to_owned(), OPENED.fetch_add(1, Ordering::Relaxed));
    params.push(
        Param::custom(FORM, Label::Empty, move |ui, fields, _| ui.scope(|ui| draw(ui, fields, &salt, &presets)).response)
            .full_width()
            .companions([VALUES, APPLIED]),
    );
    let title = Label::from(spec.label.trim_end_matches('…').to_owned());
    Ok((title, params, fields))
}

pub(crate) fn command_params(command: &str, fields: &DialogFields) -> Option<Value> {
    photocraft_engine::plugin_filters::find(command)?;
    Some(match fields.get(VALUES) {
        Some(FieldValue::Json(values @ Value::Object(_))) => values.clone(),
        _ => json!({}),
    })
}

fn values_of(fields: &DialogFields) -> Map<String, Value> {
    match fields.get(VALUES) {
        Some(FieldValue::Json(Value::Object(values))) => values.clone(),
        _ => Map::new(),
    }
}

fn apply_preset(fields: &mut DialogFields, presets: &[Preset]) {
    let chosen = fields.text(PRESET).unwrap_or_default().to_owned();
    if fields.text(APPLIED).unwrap_or_default() == chosen {
        return;
    }
    if let Some(preset) = presets.iter().find(|p| p.id == chosen) {
        let mut values = values_of(fields);
        values.extend(preset.values.clone());
        fields.set(VALUES, FieldValue::Json(Value::Object(values)));
    }
    fields.set(APPLIED, FieldValue::Text(chosen));
}

fn draw(ui: &mut egui::Ui, fields: &mut DialogFields, salt: &(String, u64), presets: &[Preset]) {
    let command = salt.0.as_str();
    let Some(form) = fields.get(FORM).and_then(|f| match f {
        FieldValue::Json(form) => serde_json::from_value::<Form>(form.clone()).ok(),
        _ => None,
    }) else {
        ui.label(tl!("This plugin filter has no form."));
        return;
    };
    apply_preset(fields, presets);
    let stored = values_of(fields);
    let given: Values = form
        .params
        .iter()
        .filter_map(|p| Some((p.key.clone(), serde_json::from_value::<FormValue>(stored.get(&p.key)?.clone()).ok()?)))
        .collect();
    let view_id = egui::Id::unique(("photocraft-plugin-form", salt));
    let worker_id = view_id.with("worker");
    let mut view =
        ui.ctx().data_mut(|d| d.get_temp::<FormView>(view_id)).filter(|v| v.form().key == form.key).unwrap_or_else(|| FormView::new(command, form.clone(), &given));
    let external: Vec<PatchOp> =
        given.iter().filter(|(k, v)| view.values().get(*k) != Some(*v)).map(|(k, v)| PatchOp::Set { key: k.clone(), value: v.clone() }).collect();
    if !external.is_empty() {
        let rev = view.state().rev;
        if let Err(error) = view.apply_patch(&Patch { schema: FORM_SCHEMA, base_rev: rev, rev: rev.saturating_add(1), ops: external }) {
            ui.ctx().data_mut(|d| d.get_temp_mut_or_default::<Worker>(worker_id).error = Some(error));
        }
    }
    view.show(ui, &mut crate::plugins_ui::PhotocraftHost);
    let mut worker = ui.ctx().data_mut(|d| d.get_temp::<Worker>(worker_id)).unwrap_or_default();
    receive(&mut worker, &mut view);
    if worker.pending.is_none() && worker.sent.as_ref() != Some(view.values()) {
        send(ui.ctx(), &mut worker, command, &view);
    }
    if worker.pending.is_some() {
        ui.ctx().request_repaint_after(BUSY_POLL);
    }
    if let Some(error) = &worker.error {
        ui.colored_label(ui.visuals().warn_fg_color, error);
    }
    fields.set(FORM, FieldValue::Json(json!(view.form())));
    fields.set(VALUES, FieldValue::Json(json!(view.values())));
    ui.ctx().data_mut(|d| {
        d.insert_temp(worker_id, worker);
        d.insert_temp(view_id, view);
    });
}

fn receive(worker: &mut Worker, view: &mut FormView) {
    let Some(pending) = worker.pending.clone() else { return };
    let Ok((sent, reply)) = pending.lock().unwrap_or_else(PoisonError::into_inner).try_recv() else { return };
    worker.pending = None;
    if &sent != view.values() {
        worker.sent = None;
        return;
    }
    match reply {
        Ok((next, patch)) => {
            view.replace_form(next);
            worker.error = patch.and_then(|patch| view.apply_patch(&patch).err());
            worker.sent = Some(view.values().clone());
        }
        Err(error) => worker.error = Some(error),
    }
}

fn send(ctx: &egui::Context, worker: &mut Worker, command: &str, view: &FormView) {
    let (tx, rx) = std::sync::mpsc::channel();
    let values = view.values().clone();
    let rev = view.state().rev;
    let id = command.to_owned();
    let repaint = ctx.clone();
    let spawned = std::thread::Builder::new().name("plugin-form".into()).spawn({
        let values = values.clone();
        move || {
            let reply = photocraft_engine::plugin_filters::update_form(&id, values.clone(), rev).map(|(form, patch, _)| (form, patch));
            let _ = tx.send((values, reply));
            repaint.request_repaint();
        }
    });
    worker.sent = Some(values);
    match spawned {
        Ok(_) => worker.pending = Some(Arc::new(Mutex::new(rx))),
        Err(error) => worker.error = Some(error.to_string()),
    }
}
