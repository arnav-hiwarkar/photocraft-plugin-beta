//! Document-thread half of the session bridge. No guest executes on this thread.
use crate::commands::CommandSpec;
use crate::{EngineError, LayerId, Session};
use craft_ext_host::api::{Code, Invocation, OpError, authorize};
use photocraft_plugins::{
    document_thread::{Action, Capture, Request},
    runtime::{LiveGrants, Runtime},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};
fn error(e: impl std::fmt::Display) -> OpError {
    OpError::new(Code::Invalid, e.to_string())
}
impl Session {
    pub fn plugin_capture(&self) -> Capture {
        Capture {
            document: self.active().map(|d| d.doc.id.0),
            epoch: self.active_generation(),
            revision: self.active().map_or(0, |d| d.revision),
            surface: None,
        }
    }
    fn ensure_plugin_sessions(&mut self) {
        let generation = photocraft_plugins::store::generation().ok();
        if self.plugin_sessions.as_ref().is_some_and(|r| Some(r.generation) == generation) {
            return;
        }
        self.plugin_sessions = Runtime::load().ok();
        if let Some(rt) = &self.plugin_sessions {
            for e in &rt.entries {
                let _ = rt.dispatch(&e.id, self.plugin_capture(), json!({"event":"init","contribution":e.contribution.id}));
            }
        }
    }
    pub fn pump_plugins(&mut self) {
        self.ensure_plugin_sessions();
        let Some(mut rt) = self.plugin_sessions.take() else { return };
        let now = self.active().map(|d| (d.doc.id.0, self.active_generation(), d.selected_layers.clone(), d.doc.selection.clone()));
        let opened = self.docs.iter().filter(|d| !self.plugin_opened.contains(&d.doc.id.0)).map(|d| (d.doc.id.0, d.doc.name.clone())).collect::<Vec<_>>();
        for (id, name) in opened {
            self.plugin_opened.insert(id);
            rt.app_event(self.plugin_capture(), "document.opened", json!({"document":id,"name":name}));
        }
        if now.as_ref().map(|s| (&s.2, &s.3)) != self.plugin_seen.as_ref().map(|s| (&s.2, &s.3)) {
            rt.app_event(self.plugin_capture(), "selection.changed",json!({"layers":self.active().map(|d|d.selected_layers.iter().map(|l|l.0).collect::<Vec<_>>()),"has_selection":self.active().is_some_and(|d|d.doc.selection.is_some())}));
        }
        self.plugin_seen = now;
        while let Some(request) = rt.pending.pop_front().or_else(|| rt.requests.try_recv().ok()) {
            if matches!(request.action, Action::Begin) {
                self.plugin_transaction(&mut rt, request);
            } else {
                request.reply(Err(OpError::new(Code::Stale, "Event is no longer active")));
            }
        }
        self.plugin_sessions = Some(rt);
    }
    pub fn execute_plugin_session(&mut self, id: &str, params: Value) -> crate::Result<Value> {
        self.ensure_plugin_sessions();
        if let Some(rt) = self.plugin_sessions.as_mut() {
            if let Some(e) = rt.entries.iter().find(|e| e.id == id && e.contribution.kind == craft_ext_core::manifest::ContributionKind::Panel) {
                let key = format!("{}/{}", e.id.trim_start_matches("plugin.session.").split('/').next().unwrap_or(""), e.contribution.id);
                if let Some(sf) = rt.surfaces.get_mut(&key) {
                    sf.open = true;
                    return Ok(Value::Null);
                }
            }
        }
        let rt = self.plugin_sessions.as_ref().ok_or_else(|| EngineError::Other("Plugins unavailable".into()))?;
        let mut capture = self.plugin_capture();
        let (owner, event) = if id == "plugin.uiEvent" {
            let key = params["surface"].as_str().unwrap_or("");
            let sf = rt
                .surfaces
                .get(key)
                .filter(|s| s.open && Some(s.rev) == params["rev"].as_u64())
                .ok_or_else(|| EngineError::Other("Stale plugin surface".into()))?;
            capture.surface = Some((key.into(), sf.rev));
            (sf.owner.as_str(), json!({"event":"ui","surface":sf.id,"rev":sf.rev,"widget":params["widget"],"value":params["value"]}))
        } else {
            (id, json!({"event":"command","id":rt.entries.iter().find(|e|e.id==id).map(|e|&e.contribution.id),"params":params}))
        };
        let done = rt.dispatch(owner, capture, event).map_err(|e| EngineError::Other(e.to_string()))?;
        // Synchronous command callers (CLI/automation) also serve the worker.
        let Some(mut rt) = self.plugin_sessions.take() else { return Err(EngineError::Other("Plugins unavailable".into())) };
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = loop {
            match done.try_recv() {
                Ok(result) => break result.map(|_| Value::Null).map_err(EngineError::Other),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break Err(EngineError::Other("Plugin worker stopped".into())),
                Err(_) => {}
            }
            if Instant::now() >= deadline {
                break Err(EngineError::Other("Plugin timed out".into()));
            }
            if let Some(r) = rt.pending.pop_front().or_else(|| rt.requests.recv_timeout(Duration::from_millis(10)).ok()) {
                if matches!(r.action, Action::Begin) {
                    self.plugin_transaction(&mut rt, r);
                } else {
                    r.reply(Err(OpError::new(Code::Stale, "Event ended")));
                }
            }
        };
        self.plugin_sessions = Some(rt);
        result
    }
    fn plugin_valid(&self, rt: &Runtime, inv: &Invocation, c: &Capture, revision: u64) -> Result<(), OpError> {
        rt.valid(inv)?;
        let now = self.plugin_capture();
        if now.document != c.document || now.epoch != c.epoch || now.revision != revision {
            return Err(OpError::new(Code::Stale, "Document changed since dispatch"));
        }
        if let Some((key, rev)) = &c.surface {
            if !rt.surfaces.get(key).is_some_and(|s| s.open && s.rev == *rev) {
                return Err(OpError::new(Code::Stale, "Surface changed since dispatch"));
            }
        }
        Ok(())
    }
    fn plugin_transaction(&mut self, rt: &mut Runtime, begin: Request) {
        let inv = begin.invocation.clone();
        let capture = begin.capture.clone();
        if let Err(e) = self.plugin_valid(rt, &inv, &capture, capture.revision) {
            begin.reply(Err(e));
            return;
        }
        let before = self.active().cloned();
        let journal = self.journal.len();
        let active_layer = before.as_ref().and_then(|d| d.active_layer);
        let selection = before.as_ref().map(|d| json!({"layers":d.selected_layers.iter().map(|l|l.0).collect::<Vec<_>>(),"active":d.active_layer.map(|l|l.0),"has_selection":d.doc.selection.is_some(),"bounds":d.doc.selection.as_ref().map(|s|s.content_bounds())})).unwrap_or(Value::Null);
        let mut revision = capture.revision;
        let mut ui: Option<(std::collections::BTreeMap<String, photocraft_plugins::runtime::Surface>, Vec<String>)> = None;
        let mut failed = false;
        let mut commit = false;
        begin.reply(Ok(Value::Null));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            let Ok(r) = rt.requests.recv_timeout(deadline.saturating_duration_since(Instant::now())) else { break };
            if !Arc::ptr_eq(&inv.cancel, &r.invocation.cancel) {
                // Other workers retry their queued events rather than interleave edits.
                rt.pending.push_back(r);
                continue;
            }
            let valid = self.plugin_valid(rt, &inv, &capture, revision);
            match &r.action {
                Action::Finish { commit: wanted } => {
                    commit = *wanted && !failed && valid.is_ok();
                    r.reply(if commit { Ok(Value::Null) } else { Err(valid.err().unwrap_or_else(|| OpError::new(Code::Cancelled, "Event rolled back"))) });
                    break;
                }
                Action::Call { op, args } => {
                    let out = valid.and_then(|_| authorize(&photocraft_plugins::contract().map_err(error)?, &LiveGrants, &inv, op, args)).and_then(|_| {
                        if op.starts_with("ui.") {
                            // Validate in a temporary surface map; publish only after commit.
                            let surfaces = rt.surfaces.clone();
                            let notifications = rt.notifications.clone();
                            if let Some((pending, notices)) = ui.as_ref() {
                                rt.surfaces = pending.clone();
                                rt.notifications = notices.clone();
                            }
                            let out = rt.ui_change(&inv, op, args);
                            let pending = rt.surfaces.clone();
                            let notices = rt.notifications.clone();
                            rt.surfaces = surfaces;
                            rt.notifications = notifications;
                            if out.is_ok() {
                                ui = Some((pending, notices));
                            }
                            out
                        } else {
                            self.plugin_op(op, args.clone(), active_layer, &selection)
                        }
                    });
                    failed |= out.is_err();
                    revision = self.active().map_or(0, |d| d.revision);
                    r.reply(out);
                }
                Action::Begin => {
                    failed = true;
                    r.reply(Err(OpError::new(Code::Stale, "Nested event")));
                }
            }
        }
        if !commit {
            inv.cancel.store(true, Ordering::Relaxed);
        }
        self.finish_plugin_edits(before, journal, commit, &inv.label);
        if commit {
            if let Some((surfaces, notices)) = ui.take() {
                rt.surfaces = surfaces;
                rt.notifications = notices;
            }
        }
    }
    fn finish_plugin_edits(&mut self, before: Option<crate::DocState>, journal: usize, commit: bool, label: &str) {
        if let Some(before) = before {
            if let Some(d) = self.active_mut() {
                if commit {
                    let changed = d.doc != before.doc;
                    d.history = before.history.clone();
                    if changed {
                        d.history.record(label, before.doc.clone(), before.layer_target());
                        d.coalesce = None;
                    }
                } else {
                    *d = before;
                }
            }
        }
        self.journal.truncate(journal);
    }
    fn plugin_op(&mut self, op: &str, args: Value, layer: Option<LayerId>, selection: &Value) -> Result<Value, OpError> {
        if op == "plugin.resources" {
            return photocraft_plugins::resources::query().map_err(error).and_then(|r| serde_json::to_value(r).map_err(error));
        }
        let d = self.active().ok_or_else(|| OpError::new(Code::NotFound, "No document"))?;
        match op {
            "document.info" => Ok(json!({"id":d.doc.id.0,"name":d.doc.name,"width":d.doc.size.width,"height":d.doc.size.height,"revision":d.revision})),
            "layer.list" => {
                fn layers(ls: &[photocraft_doc::Layer], out: &mut Vec<Value>, depth: usize) {
                    if depth > 64 {
                        return;
                    }
                    for l in ls {
                        out.push(json!({"id":l.id.0,"name":l.name}));
                        if let photocraft_doc::LayerContent::Group(g) = &l.content {
                            layers(&g.children, out, depth + 1);
                        }
                    }
                }
                let mut out = vec![];
                layers(&d.doc.layers, &mut out, 0);
                Ok(json!(out))
            }
            "selection.get" => Ok(selection.clone()),
            "layer.rename" => {
                let target = args["layer"].as_u64().map(LayerId).or(layer).ok_or_else(|| OpError::invalid("No layer"))?;
                let name = args["name"].as_str().filter(|s| s.len() <= 4096).ok_or_else(|| OpError::invalid("Invalid name"))?;
                self.execute("layer.renameLayer", json!({"layer":target.0,"name":name})).map_err(error)
            }
            "filter.apply" => {
                // Only the allowlisted pixel filters run, with validated params (see session_filters).
                let (filter, params) = photocraft_plugins::session_filters::check(&args).map_err(|refusal| match refusal {
                    photocraft_plugins::session_filters::Refusal::NotAllowed(msg) => OpError::denied(msg),
                    photocraft_plugins::session_filters::Refusal::Invalid(msg) => OpError::invalid(msg),
                })?;
                self.execute(filter, params).map_err(error)
            }
            _ => Err(OpError::new(Code::NotFound, "Unknown operation")),
        }
    }
}

/// The id prefix of session commands and panels: `plugin.session.<plugin>/<contribution>`.
pub const PREFIX: &str = "plugin.session.";

/// Command specs of the session contributions the store enables (commands and panels), listed as
/// Plugins › <plugin name> › <label>. Like plugin filters, the list follows the store's generation,
/// checked at most every two seconds, so disabled and kill-listed contributions drop out.
mod registry {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    use craft_ext_core::manifest::ContributionKind;

    use super::*;

    #[derive(Default)]
    struct Registry {
        checked: Option<Instant>,
        generation: Option<u64>,
        specs: Vec<&'static CommandSpec>,
        /// Specs are `'static`: each distinct (id, label, plugin name) is leaked once and reused.
        interned: HashMap<String, &'static CommandSpec>,
    }

    static REVISION: AtomicU64 = AtomicU64::new(0);

    fn registry() -> &'static Mutex<Registry> {
        static R: OnceLock<Mutex<Registry>> = OnceLock::new();
        R.get_or_init(Mutex::default)
    }

    fn leak(s: String) -> &'static str {
        Box::leak(s.into_boxed_str())
    }

    fn spec_for(r: &mut Registry, id: String, label: &str, plugin_name: &str) -> &'static CommandSpec {
        let key = format!("{id}\u{0}{label}\u{0}{plugin_name}");
        if let Some(spec) = r.interned.get(&key) {
            return *spec;
        }
        let menu: &'static [&'static str] = Box::leak(vec!["Plugins", leak(plugin_name.to_string())].into_boxed_slice());
        let spec: &'static CommandSpec = Box::leak(Box::new(CommandSpec {
            id: leak(id),
            label: leak(label.to_string()),
            menu,
            shortcut: None,
            params: r##"{…plugin command parameters}"##,
            enabled: session_enabled,
            run: session_run,
            journal: false,
        }));
        r.interned.insert(key, spec);
        spec
    }

    /// Rebuild from the store when its generation changed. Store errors keep the last list.
    fn refresh(r: &mut Registry) {
        if r.checked.is_some_and(|t| t.elapsed() < Duration::from_secs(2)) {
            return;
        }
        r.checked = Some(Instant::now());
        let generation = if photocraft_plugins::safe_mode() { Some(0) } else { photocraft_plugins::store::generation().ok() };
        if generation.is_none() || generation == r.generation {
            return;
        }
        let (Ok(releases), Ok(contract)) = (photocraft_plugins::store::enabled(), photocraft_plugins::contract()) else { return };
        let mut specs = Vec::new();
        for (plugin, _version, _root, manifest, enabled) in releases {
            for c in manifest.contributions {
                // The same selection `Runtime::load` makes: only these kinds get a worker.
                let listed = enabled.contains(&c.id)
                    && matches!(c.kind, ContributionKind::Command | ContributionKind::Panel)
                    && c.contract.check(&contract.provided).is_none();
                if listed {
                    specs.push(spec_for(r, format!("{PREFIX}{plugin}/{}", c.id), &c.label, &manifest.name));
                }
            }
        }
        r.generation = generation;
        r.specs = specs;
        REVISION.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn with<T>(f: impl FnOnce(&[&'static CommandSpec]) -> T) -> T {
        let mut r = registry().lock().unwrap_or_else(PoisonError::into_inner);
        refresh(&mut r);
        f(&r.specs)
    }

    pub(super) fn revision() -> u64 {
        with(|_| ());
        REVISION.load(Ordering::Relaxed)
    }
}

/// The command specs of the enabled session contributions, for discovery and menus.
pub fn specs() -> Vec<&'static CommandSpec> {
    registry::with(|specs| specs.to_vec())
}

/// The spec of session command `id`, while its contribution is enabled.
pub fn find(id: &str) -> Option<&'static CommandSpec> {
    if !id.starts_with(PREFIX) {
        return None;
    }
    registry::with(|specs| specs.iter().copied().find(|spec| spec.id == id))
}

/// Changes whenever the session command list does (for menu caches).
pub fn revision() -> u64 {
    registry::revision()
}

/// Starts the plugin catalogue refresh (once per process). The desktop app calls it at startup;
/// it refreshes now, then every six hours, and each refresh that changes the store's generation
/// reloads the plugins on their next use, so revoked contributions stop.
pub fn start_catalogue_refresh() {
    photocraft_plugins::store::start_catalogue_refresh();
}

/// The registry only lists enabled contributions, so a listed command is always enabled; a session
/// event that cannot run reports its own error when it is run.
fn session_enabled(_: &Session) -> std::result::Result<(), String> {
    Ok(())
}

/// Run session command `__plugin` with the other params (the id is injected by `inject_kind`).
fn session_run(s: &mut Session, p: &Value) -> crate::Result<Value> {
    let id = p.get(crate::plugin_filters::COMMAND_PARAM).and_then(Value::as_str).unwrap_or_default().to_string();
    let mut params = p.clone();
    if let Value::Object(m) = &mut params {
        m.remove(crate::plugin_filters::COMMAND_PARAM);
    }
    s.execute_plugin_session(&id, params)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiple_session_edits_fold_to_one_labelled_history_step() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let before = s.active().unwrap().clone();
        let journal = s.journal.len();
        let n = before.history.past_len();
        let layer = before.active_layer;
        s.plugin_op("layer.rename", json!({"name":"First"}), layer, &Value::Null).unwrap();
        s.plugin_op("layer.rename", json!({"name":"Second"}), layer, &Value::Null).unwrap();
        s.finish_plugin_edits(Some(before.clone()), journal, true, "Rename · Test");
        assert_eq!(s.active().unwrap().history.past_len(), n + 1);
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Rename · Test"));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, before.doc);
    }
    #[test]
    fn failed_event_restores_document_history_revision_and_journal() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let before = s.active().unwrap().clone();
        let journal = s.journal.len();
        s.plugin_op("layer.rename", json!({"name":"Transient"}), before.active_layer, &Value::Null).unwrap();
        assert!(s.plugin_op("layer.rename", json!({"name":false}), before.active_layer, &Value::Null).is_err());
        s.finish_plugin_edits(Some(before.clone()), journal, false, "Rename · Test");
        let d = s.active().unwrap();
        assert_eq!(d.doc, before.doc);
        assert_eq!(d.revision, before.revision);
        assert_eq!(d.history.entries(), before.history.entries());
        assert_eq!(s.journal.len(), journal);
    }
    #[test]
    fn filter_apply_runs_only_allowlisted_filters_with_clean_params() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let layer = s.active().unwrap().active_layer;
        s.plugin_op("filter.apply", json!({"filter":"filter.blur.blur"}), layer, &Value::Null).unwrap();
        // A filter that reads a file or another document, a built-in that runs other commands,
        // and any plugin or session command are refused as not available to plugins.
        for filter in ["filter.distort.displace", "filter.lastFilter", "plugin.someone/effect", "plugin.session.someone/panel"] {
            let err = s.plugin_op("filter.apply", json!({"filter": filter}), layer, &Value::Null).unwrap_err();
            assert!(matches!(err.code, Code::Denied), "{filter}: {}", err.message);
        }
        // Params may not name a file, another document, another layer or a target.
        for params in [json!({"mapPath": "/etc/hosts"}), json!({"mapDocument": 0}), json!({"target": "mask"}), json!({"layer": 3})] {
            let err = s.plugin_op("filter.apply", json!({"filter": "filter.blur.blur", "params": params}), layer, &Value::Null).unwrap_err();
            assert!(matches!(err.code, Code::Invalid), "{params}: {}", err.message);
        }
    }
}
