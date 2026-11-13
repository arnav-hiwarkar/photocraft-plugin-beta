//! Document-thread half of the session bridge. No guest executes on this thread.
use crate::commands::CommandSpec;
use crate::{EngineError, Session};
use craft_ext_core::manifest::ContributionKind;
use craft_ext_host::api::Grants as _;
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
/// A plugin tool gesture under way: begin … commit is one history step; anything else rolls the
/// document back to `before`.
pub struct Gesture {
    pub(crate) id: u64,
    /// The tool's session entry (`plugin.session.<plugin>/<tool>`).
    pub(crate) entry: String,
    pub(crate) plugin: String,
    pub(crate) contribution: String,
    pub(crate) label: String,
    pub(crate) before: Option<crate::DocState>,
    pub(crate) doc: Option<u64>,
    pub(crate) epoch: u64,
    pub(crate) journal: usize,
    pub(crate) grant_generation: u64,
    pub(crate) start: [f64; 2],
    /// The latest `tool` event, for `tool.state`.
    pub(crate) payload: Value,
}

static NEXT_GESTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Events a pointer can send at most per gesture through `{"points": …}`.
pub const MAX_GESTURE_POINTS: usize = 10_000;

impl Session {
    pub fn plugin_capture(&self) -> Capture {
        Capture {
            document: self.active().map(|d| d.doc.id.0),
            epoch: self.active_generation(),
            revision: self.active().map_or(0, |d| d.revision),
            surface: None,
            gesture: None,
        }
    }
    pub(crate) fn ensure_plugin_sessions(&mut self) {
        let generation = photocraft_plugins::store::generation().ok();
        if self.plugin_sessions.as_ref().is_some_and(|r| Some(r.generation) == generation) {
            return;
        }
        // Reloading (an install, a grant or a revocation changed the store) ends any gesture.
        self.plugin_gesture_end(false);
        self.plugin_picks.invalidate_pending();
        self.plugin_sessions = Runtime::load().ok();
        if let Some(rt) = &self.plugin_sessions {
            if self.plugin_tool.as_ref().is_some_and(|t| !rt.entries.iter().any(|e| &e.id == t)) {
                self.plugin_tool = None;
            }
            for e in &rt.entries {
                let _ = rt.dispatch(&e.id, self.plugin_capture(), json!({"event":"init","contribution":e.contribution.id}));
            }
        } else {
            self.plugin_tool = None;
        }
    }
    pub fn pump_plugins(&mut self) {
        self.ensure_plugin_sessions();
        self.plugin_check_gesture();
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
        // Answers to file questions, sent now that no event of theirs is running.
        while let Some((owner, event)) = self.plugin_followups.pop_front() {
            if let Err(e) = rt.dispatch(&owner, self.plugin_capture(), event) {
                rt.notifications.push(format!("A plugin could not receive its file answer: {e}"));
            }
        }
        while let Some(request) = rt.pending.pop_front().or_else(|| rt.requests.try_recv().ok()) {
            if matches!(request.action, Action::Begin) {
                self.plugin_transaction(&mut rt, request);
            } else {
                request.reply(Err(OpError::new(Code::Stale, "Event is no longer active")));
            }
        }
        self.plugin_sessions = Some(rt);
    }
    /// Serves the worker until `done` answers (CLI, automation and widget events wait here).
    fn plugin_wait(&mut self, done: std::sync::mpsc::Receiver<Result<(), String>>) -> crate::Result<()> {
        let Some(mut rt) = self.plugin_sessions.take() else { return Err(EngineError::Other("Plugins unavailable".into())) };
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = loop {
            match done.try_recv() {
                Ok(result) => break result.map_err(EngineError::Other),
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
    /// Sends queued `files.picked` answers and waits for each (bounded).
    fn plugin_run_followups(&mut self) -> Vec<String> {
        let mut errors = vec![];
        for _ in 0..16 {
            let Some((owner, event)) = self.plugin_followups.pop_front() else { break };
            let done = match self.plugin_sessions.as_ref().map(|rt| rt.dispatch(&owner, self.plugin_capture(), event)) {
                Some(Ok(done)) => done,
                Some(Err(e)) => {
                    errors.push(e.to_string());
                    continue;
                }
                None => break,
            };
            if let Err(e) = self.plugin_wait(done) {
                errors.push(e.to_string());
            }
        }
        errors
    }
    pub fn execute_plugin_session(&mut self, id: &str, params: Value) -> crate::Result<Value> {
        self.ensure_plugin_sessions();
        let kind = self.plugin_sessions.as_ref().and_then(|rt| rt.entries.iter().find(|e| e.id == id)).map(|e| e.contribution.kind);
        if kind == Some(ContributionKind::Tool) {
            return self.plugin_tool_command(id, &params);
        }
        if let Some(rt) = self.plugin_sessions.as_mut() {
            if let Some(e) = rt.entries.iter().find(|e| e.id == id && e.contribution.kind == ContributionKind::Panel) {
                let key = format!("{}/{}", e.invocation.plugin, e.contribution.id);
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
        let seen = rt.notifications.len();
        let done = rt.dispatch(owner, capture, event).map_err(|e| EngineError::Other(e.to_string()))?;
        // Synchronous command callers (CLI/automation) also serve the worker.
        self.plugin_wait(done)?;
        let errors = self.plugin_run_followups();
        if !errors.is_empty() {
            return Err(EngineError::Other(errors.join("; ")));
        }
        // What the plugin told the person during this call (the desktop also shows them).
        let notes = self.plugin_sessions.as_ref().map(|rt| rt.notifications.iter().skip(seen).cloned().collect::<Vec<_>>()).unwrap_or_default();
        Ok(if notes.is_empty() { Value::Null } else { json!({"notifications": notes}) })
    }

    /// A tool command: `{"points": [[x, y], …]}` runs a whole gesture (CLI, automation), anything
    /// else selects the tool for the canvas.
    fn plugin_tool_command(&mut self, id: &str, params: &Value) -> crate::Result<Value> {
        let bad = |msg: &str| EngineError::BadParams { cmd: id.to_string(), msg: msg.to_string() };
        let Some(points) = params.get("points") else {
            self.plugin_gesture_end(false);
            self.plugin_tool = Some(id.to_string());
            return Ok(json!({"tool": id, "selected": true}));
        };
        let points =
            points.as_array().filter(|p| !p.is_empty() && p.len() <= MAX_GESTURE_POINTS).ok_or_else(|| bad("`points` must hold 1 to 10000 [x, y] points"))?;
        let points = points
            .iter()
            .map(|p| match p.as_array().map(Vec::as_slice) {
                Some([x, y]) => match (x.as_f64(), y.as_f64()) {
                    (Some(x), Some(y)) if x.is_finite() && y.is_finite() && x.abs() <= 1e7 && y.abs() <= 1e7 => Ok([x, y]),
                    _ => Err(bad("points are [x, y] numbers")),
                },
                _ => Err(bad("points are [x, y] numbers")),
            })
            .collect::<crate::Result<Vec<[f64; 2]>>>()?;
        let mut extra = json!({});
        if let Some(m) = params.get("modifiers") {
            extra["modifiers"] = modifiers(m).ok_or_else(|| bad("`modifiers` is {shift, alt, ctrl, cmd} booleans"))?;
        }
        if let Some(z) = params.get("zoom") {
            extra["zoom"] = json!(z.as_f64().filter(|z| z.is_finite() && *z > 0.0 && *z <= 1e4).ok_or_else(|| bad("`zoom` must be a positive number"))?);
        }
        let cancel = params.get("cancel").and_then(Value::as_bool).unwrap_or(false);
        let previous = self.plugin_tool.replace(id.to_string());
        let run = (|| {
            let (first, rest) = points.split_first().ok_or_else(|| bad("no points"))?;
            self.plugin_tool_event("begin", *first, extra.clone())?;
            if let Some((last, middle)) = rest.split_last() {
                for p in middle {
                    self.plugin_tool_event("update", *p, extra.clone())?;
                }
                if cancel {
                    self.plugin_tool_event("update", *last, extra.clone())?;
                }
            }
            let last = *points.last().unwrap_or(first);
            self.plugin_tool_event(if cancel { "cancel" } else { "commit" }, last, extra.clone())
        })();
        self.plugin_tool = previous;
        run?;
        Ok(json!({"tool": id, "points": points.len(), "cancelled": cancel}))
    }

    /// One pointer event of the selected plugin tool, in image pixels. `extra` adds `modifiers`,
    /// `button`, `pressure` and `zoom`. Runs the plugin's event synchronously; a failure rolls the
    /// whole gesture back and returns the reason.
    pub fn plugin_tool_event(&mut self, phase: &str, at: [f64; 2], extra: Value) -> crate::Result<()> {
        self.ensure_plugin_sessions();
        let Some(tool) = self.plugin_tool.clone() else { return Ok(()) };
        if !at.iter().all(|v| v.is_finite()) {
            return Err(EngineError::Other("Invalid pointer position".into()));
        }
        match phase {
            "begin" => {
                self.plugin_gesture_end(false);
                let rt = self.plugin_sessions.as_ref().ok_or_else(|| EngineError::Other("Plugins unavailable".into()))?;
                let Some(entry) = rt.entries.iter().find(|e| e.id == tool) else {
                    self.plugin_tool = None;
                    return Err(EngineError::Other("The plugin tool is no longer available".into()));
                };
                let d = self.active().ok_or(EngineError::NoDocument)?;
                self.plugin_gesture = Some(Gesture {
                    id: NEXT_GESTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
                    entry: entry.id.clone(),
                    plugin: entry.invocation.plugin.to_string(),
                    contribution: entry.contribution.id.clone(),
                    label: entry.invocation.label.clone(),
                    before: Some(d.clone()),
                    doc: Some(d.doc.id.0),
                    epoch: self.active_generation(),
                    journal: self.journal.len(),
                    grant_generation: LiveGrants.generation(&entry.invocation.plugin),
                    start: at,
                    payload: Value::Null,
                });
            }
            "update" | "commit" | "cancel" => {
                if self.plugin_gesture.is_none() {
                    return Ok(());
                }
            }
            other => return Err(EngineError::Other(format!("Unknown tool phase {other}"))),
        }
        let Some(g) = self.plugin_gesture.as_mut() else { return Ok(()) };
        let mut payload = json!({"event":"tool","tool":g.contribution,"phase":phase,"at":at,"start":g.start,"modifiers":{},"button":"primary","pressure":1.0});
        if let (Value::Object(p), Value::Object(e)) = (&mut payload, extra) {
            for (k, v) in e {
                if matches!(k.as_str(), "modifiers" | "button" | "pressure" | "zoom") {
                    p.insert(k, v);
                }
            }
        }
        g.payload = payload.clone();
        let (entry, gid) = (g.entry.clone(), g.id);
        let mut capture = self.plugin_capture();
        capture.gesture = Some(gid);
        let expected = Arc::new(std::sync::atomic::AtomicBool::new(phase == "cancel"));
        let dispatched = self.plugin_sessions.as_ref().map(|rt| rt.dispatch_with(&entry, capture, payload, expected));
        let result = match dispatched {
            Some(Ok(done)) => self.plugin_wait(done),
            Some(Err(e)) => Err(EngineError::Other(e.to_string())),
            None => Err(EngineError::Other("Plugins unavailable".into())),
        };
        match (phase, &result) {
            ("cancel", _) | (_, Err(_)) => self.plugin_gesture_end(false),
            ("commit", Ok(())) => self.plugin_gesture_end(true),
            _ => {}
        }
        if phase == "cancel" { Ok(()) } else { result }
    }

    /// Escape on the canvas: cancels the gesture (the plugin hears `cancel`), else deselects the tool.
    pub fn plugin_tool_escape(&mut self) {
        if self.plugin_gesture.is_some() {
            let at = self
                .plugin_gesture
                .as_ref()
                .and_then(|g| g.payload["at"].as_array().map(|a| [a[0].as_f64().unwrap_or(0.0), a.get(1).and_then(Value::as_f64).unwrap_or(0.0)]))
                .unwrap_or([0.0, 0.0]);
            let _ = self.plugin_tool_event("cancel", at, json!({}));
        } else {
            self.plugin_tool = None;
        }
    }

    /// Whether a plugin tool gesture is under way.
    pub fn plugin_gesture_active(&self) -> bool {
        self.plugin_gesture.is_some()
    }

    /// Ends the gesture: commit records one history step from where it began; otherwise the
    /// document returns to that state. Either way the plugin's overlay is cleared.
    fn plugin_gesture_end(&mut self, commit: bool) {
        let Some(mut g) = self.plugin_gesture.take() else { return };
        if let Some(rt) = self.plugin_sessions.as_mut() {
            rt.overlays.remove(&g.plugin);
        }
        let Some(before) = g.before.take() else { return };
        let same = self.active().map(|d| d.doc.id.0) == g.doc && self.active_generation() == g.epoch;
        if same {
            self.finish_plugin_edits(Some(before), g.journal, commit, &g.label, false);
        } else if let Some(d) = self.docs.iter_mut().find(|d| Some(d.doc.id.0) == g.doc) {
            // The document lost focus mid-gesture: it is rolled back where it is.
            *d = before;
        }
    }

    /// Rolls a gesture back when its document changed, or its plugin was turned off or revoked.
    fn plugin_check_gesture(&mut self) {
        let Some(g) = &self.plugin_gesture else { return };
        let doc_ok = self.active().map(|d| d.doc.id.0) == g.doc && self.active_generation() == g.epoch;
        let grants_ok = self.plugin_sessions.as_ref().and_then(|rt| rt.entries.iter().find(|e| e.id == g.entry).map(|e| (rt, e))).is_some_and(|(rt, e)| {
            let mut inv = e.invocation.clone();
            inv.grant_generation = g.grant_generation;
            rt.valid(&inv).is_ok()
        });
        if !(doc_ok && grants_ok) {
            let (entry, payload) = (g.entry.clone(), g.payload.clone());
            self.plugin_gesture_end(false);
            if let Some(rt) = &self.plugin_sessions {
                let mut ev = payload;
                ev["phase"] = json!("cancel");
                let _ = rt.dispatch_with(&entry, self.plugin_capture(), ev, Arc::new(std::sync::atomic::AtomicBool::new(true)));
                if !grants_ok {
                    self.plugin_tool = None;
                }
            }
        }
    }

    /// Plugins paused after three failed events in a row.
    pub fn plugins_paused(&self) -> Vec<photocraft_plugins::runtime::Paused> {
        self.plugin_sessions.as_ref().map(Runtime::paused).unwrap_or_default()
    }

    /// Clears a plugin's pause and sends its contributions `init` again.
    pub fn resume_plugin(&mut self, plugin: &str) -> crate::Result<()> {
        self.ensure_plugin_sessions();
        let rt = self.plugin_sessions.as_ref().ok_or_else(|| EngineError::Other("Plugins unavailable".into()))?;
        rt.resume(plugin).map_err(EngineError::Other)?;
        for e in rt.entries.iter().filter(|e| e.invocation.plugin.as_str() == plugin) {
            let _ = rt.dispatch(&e.id, self.plugin_capture(), json!({"event":"init","contribution":e.contribution.id}));
        }
        Ok(())
    }

    /// The canvas overlays of every plugin (image pixels).
    pub fn plugin_overlays(&self) -> Vec<craft_ext_core::ui::tree::DrawOp> {
        self.plugin_sessions.as_ref().map(|rt| rt.overlays.values().flatten().cloned().collect()).unwrap_or_default()
    }

    /// The selected plugin tool's label, for the canvas and status bar.
    pub fn plugin_tool_label(&self) -> Option<String> {
        let tool = self.plugin_tool.as_ref()?;
        self.plugin_sessions.as_ref()?.entries.iter().find(|e| &e.id == tool).map(|e| e.invocation.label.clone())
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
        if c.gesture.is_some() && self.plugin_gesture.as_ref().map(|g| g.id) != c.gesture {
            return Err(OpError::new(Code::Stale, "The tool gesture ended"));
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
        let in_gesture = capture.gesture.is_some();
        let before = self.active().cloned();
        let journal = self.journal.len();
        let selection = before.as_ref().map(|d| json!({"layers":d.selected_layers.iter().map(|l|l.0).collect::<Vec<_>>(),"active":d.active_layer.map(|l|l.0),"has_selection":d.doc.selection.is_some(),"bounds":d.doc.selection.as_ref().map(|s|{let r=s.content_bounds();[r.x0,r.y0,r.width() as i32,r.height() as i32]})})).unwrap_or(Value::Null);
        let owner =
            rt.entries.iter().find(|e| e.invocation.plugin == inv.plugin && e.contribution.id == inv.contribution).map(|e| e.id.clone()).unwrap_or_default();
        let plugin = inv.plugin.to_string();
        let mut revision = capture.revision;
        let mut ui: Option<(std::collections::BTreeMap<String, photocraft_plugins::runtime::Surface>, Vec<String>)> = None;
        let mut overlay: Option<Vec<craft_ext_core::ui::tree::DrawOp>> = None;
        let mut staged = crate::plugin_ops::Staged::default();
        let mut failed = false;
        let mut commit = false;
        let mut finish = None;
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
            // Only invocation/scope invalidation poisons the transaction. Guests may
            // handle individual authorization and validation errors and continue.
            failed |= valid.is_err();
            match &r.action {
                Action::Finish { commit: wanted } => {
                    let result = valid.and_then(|_| {
                        if !*wanted || failed {
                            return Err(OpError::new(Code::Cancelled, "Event rolled back"));
                        }
                        staged.publish_files(|| {
                            self.plugin_valid(rt, &inv, &capture, revision)?;
                            if Instant::now() >= deadline {
                                return Err(OpError::new(Code::Cancelled, "Event timed out during publication"));
                            }
                            Ok(())
                        })
                    });
                    commit = result.is_ok();
                    // Finish errors reach the worker and synchronous callers. File publication is
                    // ordered and may be partial; document/UI changes roll back on any failure.
                    finish = Some((r, result.map(|_| Value::Null)));
                    break;
                }
                Action::Call { op, args } => {
                    let out = valid.and_then(|_| authorize(&photocraft_plugins::contract().map_err(error)?, &LiveGrants, &inv, op, args)).and_then(|_| {
                        if op == "canvas.overlay.set" {
                            overlay = Some(Runtime::overlay(args)?);
                            Ok(Value::Null)
                        } else if op.starts_with("ui.") {
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
                            let mut cx = crate::plugin_ops::OpCtx { plugin: &plugin, owner: &owner, selection: &selection, staged: &mut staged };
                            self.plugin_op(op, args.clone(), &mut cx)
                        }
                    });
                    failed |= rt.valid(&inv).is_err();
                    revision = self.active().map_or(0, |d| d.revision);
                    r.reply(out);
                }
                Action::Begin => {
                    r.reply(Err(OpError::new(Code::Stale, "Nested event")));
                }
            }
        }
        if !commit {
            inv.cancel.store(true, Ordering::Relaxed);
        }
        self.finish_plugin_edits(before, journal, commit, &inv.label, in_gesture);
        if commit {
            if let Some((surfaces, notices)) = ui.take() {
                rt.surfaces = surfaces;
                rt.notifications = notices;
            }
            if let Some(ops) = overlay {
                rt.overlays.insert(plugin, ops);
            }
            for e in self.plugin_commit_picks(staged.picks) {
                if rt.notifications.len() < 32 {
                    rt.notifications.push(e);
                }
            }
        }
        if let Some((request, result)) = finish {
            request.reply(result);
        }
    }
    /// Folds an event's edits into one labelled history step (commit) or restores the document
    /// (rollback). Inside a gesture a committed event records nothing: the gesture's end does.
    fn finish_plugin_edits(&mut self, before: Option<crate::DocState>, journal: usize, commit: bool, label: &str, in_gesture: bool) {
        if let Some(before) = before {
            if let Some(d) = self.active_mut() {
                if commit {
                    let changed = d.doc != before.doc;
                    d.history = before.history.clone();
                    if changed && !in_gesture {
                        d.history.record(label, before.doc.clone(), before.layer_target());
                        d.coalesce = None;
                    }
                } else {
                    *d = before;
                }
            }
        }
        if !in_gesture {
            self.journal.truncate(journal);
        }
    }
}

/// `{shift, alt, ctrl, cmd}` booleans, unknown keys refused.
fn modifiers(v: &Value) -> Option<Value> {
    let m = v.as_object()?;
    if m.len() > 4 || m.iter().any(|(k, v)| !matches!(k.as_str(), "shift" | "alt" | "ctrl" | "cmd") || !v.is_boolean()) {
        return None;
    }
    Some(v.clone())
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
                    && matches!(c.kind, ContributionKind::Command | ContributionKind::Panel | ContributionKind::Tool)
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
    let mut all = manage_specs();
    all.extend(registry::with(|specs| specs.to_vec()));
    all
}

/// Commands that manage running plugins (not in menus: the Plugins window has the buttons).
static MANAGE: [CommandSpec; 2] = [
    CommandSpec {
        id: "plugin.resume",
        label: "Resume Plugin",
        menu: &[],
        shortcut: None,
        params: r##"{"plugin":id} (a plugin paused after three failed events in a row; pauses last for one process)"##,
        enabled: session_enabled,
        run: resume_run,
        journal: false,
    },
    CommandSpec {
        id: "plugin.paused",
        label: "Paused Plugins",
        menu: &[],
        shortcut: None,
        params: "{} → [{plugin, name, reason}]",
        enabled: session_enabled,
        run: paused_run,
        journal: false,
    },
];

fn resume_run(s: &mut Session, p: &Value) -> crate::Result<Value> {
    let plugin = p
        .get("plugin")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 256)
        .ok_or_else(|| EngineError::BadParams { cmd: "plugin.resume".into(), msg: "`plugin` is the plugin id".into() })?;
    s.resume_plugin(plugin)?;
    Ok(json!({"resumed": plugin}))
}

fn paused_run(s: &mut Session, _: &Value) -> crate::Result<Value> {
    s.ensure_plugin_sessions();
    Ok(json!(s.plugins_paused().into_iter().map(|p| json!({"plugin": p.plugin, "name": p.name, "reason": p.reason})).collect::<Vec<_>>()))
}

/// The plugin management commands.
pub fn manage_specs() -> Vec<&'static CommandSpec> {
    MANAGE.iter().collect()
}

/// The spec of session command `id`, while its contribution is enabled.
pub fn find(id: &str) -> Option<&'static CommandSpec> {
    if let Some(spec) = MANAGE.iter().find(|s| s.id == id) {
        return Some(spec);
    }
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
    use crate::plugin_ops::{OpCtx, Staged};

    /// Runs one operation as plugin `io.test.ops` would, outside a worker.
    fn op(s: &mut Session, op: &str, args: Value) -> Result<Value, OpError> {
        let mut staged = Staged::default();
        s.plugin_op(op, args, &mut OpCtx { plugin: "io.test.ops", owner: "plugin.session.io.test.ops/cmd", selection: &Value::Null, staged: &mut staged })
    }
    #[test]
    fn multiple_session_edits_fold_to_one_labelled_history_step() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let before = s.active().unwrap().clone();
        let journal = s.journal.len();
        let n = before.history.past_len();
        op(&mut s, "layer.rename", json!({"name":"First"})).unwrap();
        op(&mut s, "layer.rename", json!({"name":"Second"})).unwrap();
        s.finish_plugin_edits(Some(before.clone()), journal, true, "Rename · Test", false);
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
        op(&mut s, "layer.rename", json!({"name":"Transient"})).unwrap();
        assert!(op(&mut s, "layer.rename", json!({"name":false})).is_err());
        s.finish_plugin_edits(Some(before.clone()), journal, false, "Rename · Test", false);
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
        op(&mut s, "filter.apply", json!({"filter":"filter.blur.blur"})).unwrap();
        // A filter that reads a file or another document, a built-in that runs other commands,
        // and any plugin or session command are refused as not available to plugins.
        for filter in ["filter.distort.displace", "filter.lastFilter", "plugin.someone/effect", "plugin.session.someone/panel"] {
            let err = op(&mut s, "filter.apply", json!({"filter": filter})).unwrap_err();
            assert!(matches!(err.code, Code::Denied), "{filter}: {}", err.message);
        }
        // Params may not name a file, another document, another layer or a target.
        for params in [json!({"mapPath": "/etc/hosts"}), json!({"mapDocument": 0}), json!({"target": "mask"}), json!({"layer": 3})] {
            let err = op(&mut s, "filter.apply", json!({"filter": "filter.blur.blur", "params": params})).unwrap_err();
            assert!(matches!(err.code, Code::Invalid), "{params}: {}", err.message);
        }
    }
    #[test]
    fn layers_pixels_histogram_selection_and_guides_round_trip() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":8,"height":6,"background":"#806040"})).unwrap();
        let id = op(&mut s, "layer.create", json!({"name":"Paint","kind":"pixel"})).unwrap()["id"].as_u64().unwrap();
        let list = op(&mut s, "layer.list", json!({})).unwrap();
        assert_eq!(list[0]["id"], id, "top to bottom");
        assert_eq!(list[0]["kind"], "pixel");
        // A red 2×2 square at (1, 1), read back exactly.
        let red = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [255u8, 0, 0, 255].repeat(4));
        op(&mut s, "pixels.set", json!({"layer":id,"x":1,"y":1,"width":2,"height":2,"data":red})).unwrap();
        let got = op(&mut s, "pixels.get", json!({"layer":id,"rect":[1,1,2,2]})).unwrap();
        assert_eq!(got["data"], red);
        let comp = op(&mut s, "histogram.get", json!({"composite":true})).unwrap();
        assert_eq!(comp["count"], 48);
        assert_eq!(comp["red"][255], 4);
        // Outside the image, the wrong size, a locked layer: refused, nothing written.
        assert!(op(&mut s, "pixels.set", json!({"layer":id,"x":7,"y":5,"width":2,"height":2,"data":red})).is_err());
        assert!(op(&mut s, "pixels.set", json!({"layer":id,"x":0,"y":0,"width":3,"height":2,"data":red})).is_err());
        assert!(op(&mut s, "pixels.get", json!({"rect":[0,0,4096,4096],"format":"rgba32f"})).is_ok(), "clipped to the image");
        op(&mut s, "selection.set", json!({"rect":[0,0,4,3]})).unwrap();
        let sel = op(&mut s, "histogram.get", json!({"composite":true})).unwrap();
        assert_eq!(sel["count"], 12, "the selection limits the histogram");
        let mask = op(&mut s, "selection.mask.get", json!({"rect":[3,2,2,2]})).unwrap();
        assert_eq!(base64::Engine::decode(&base64::engine::general_purpose::STANDARD, mask["data"].as_str().unwrap()).unwrap(), vec![255, 0, 0, 0]);
        op(&mut s, "selection.set", json!({"none":true})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_none());
        let m = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, [255u8, 0, 0, 255]);
        op(&mut s, "selection.set", json!({"mask":{"x":0,"y":0,"width":2,"height":2,"data":m}})).unwrap();
        assert!(s.active().unwrap().doc.selection.is_some());
        assert!(op(&mut s, "selection.set", json!({"rect":[0,0,1,1],"all":true})).is_err(), "one shape at a time");
        op(&mut s, "guide.add", json!({"orientation":"vertical","position":4})).unwrap();
        assert_eq!(op(&mut s, "guides.list", json!({})).unwrap()["vertical"], json!([4.0]));
        assert!(op(&mut s, "guide.add", json!({"orientation":"diagonal","position":4})).is_err());
        assert!(op(&mut s, "guide.add", json!({"orientation":"vertical","position":1e12})).is_err());
        op(&mut s, "layer.set", json!({"layer":id,"opacity":50,"blend":"multiply","visible":false})).unwrap();
        let l = op(&mut s, "layer.get", json!({"layer":id})).unwrap();
        assert_eq!((l["opacity"].as_f64(), l["blend"].as_str(), l["visible"].as_bool()), (Some(50.0), Some("multiply"), Some(false)));
        assert!(op(&mut s, "layer.set", json!({"layer":id,"blend":"sparkle"})).is_err());
        op(&mut s, "adjustment.apply", json!({"adjustment":"invert","as_layer":true})).unwrap();
        assert!(op(&mut s, "adjustment.apply", json!({"adjustment":"colorLookup"})).is_err());
        let fill = op(&mut s, "layer.create", json!({"kind":"fill","color":[0,0,1],"name":"Blue"})).unwrap()["id"].as_u64().unwrap();
        assert_eq!(op(&mut s, "layer.get", json!({"layer":fill})).unwrap()["kind"], "fill");
        op(&mut s, "layer.delete", json!({"layer":fill})).unwrap();
        assert!(op(&mut s, "layer.get", json!({"layer":fill})).is_err());
        assert_eq!(op(&mut s, "channels.list", json!({})).unwrap()[0]["name"], "Red");
        assert!(op(&mut s, "history.list", json!({})).unwrap()["entries"].as_array().unwrap().len() > 3);
        assert!(op(&mut s, "no.such.op", json!({})).is_err());
    }

    #[test]
    fn non_rgb_documents_refuse_pixel_operations() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4,"mode":"cmyk"})).unwrap();
        assert!(op(&mut s, "pixels.get", json!({})).is_err());
        assert!(op(&mut s, "histogram.get", json!({})).is_err());
        assert!(op(&mut s, "document.info", json!({})).is_ok());
    }

    #[test]
    #[ignore = "requires a disposable CRAFT_EXT_DIR"]
    fn picked_files_are_plugin_scoped_staged_and_never_paths() {
        let dir = std::env::temp_dir().join(format!("pc-plugin-picks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let mut staged = Staged::default();
        let mut cx = OpCtx { plugin: "io.test.ops", owner: "plugin.session.io.test.ops/cmd", selection: &Value::Null, staged: &mut staged };
        let r = s.plugin_op("files.pick", json!({"mode":"folder","title":"Where"}), &mut cx).unwrap();
        assert!(r["request"].as_u64().is_some());
        assert!(s.plugin_op("files.pick", json!({"mode":"desktop"}), &mut cx).is_err());
        crate::plugin_ops::bind_test_picks(&mut staged.picks);
        s.plugin_picks.answers.push_back(dir.clone());
        let errors = s.plugin_commit_picks(staged.picks);
        assert!(errors.is_empty(), "{errors:?}");
        let (owner, event) = s.plugin_followups.pop_front().unwrap();
        assert_eq!(owner, "plugin.session.io.test.ops/export");
        let handle = event["handle"].as_str().unwrap().to_string();
        assert!(!event.to_string().contains(&*dir.to_string_lossy()), "the plugin never sees the path");
        let mut staged = Staged::default();
        let mut cx = OpCtx { plugin: "io.test.ops", owner: "x", selection: &Value::Null, staged: &mut staged };
        s.plugin_op("document.export", json!({"handle":handle,"name":"image.png"}), &mut cx).unwrap();
        s.plugin_op("files.write", json!({"handle":handle,"name":"notes.txt","text":"hello"}), &mut cx).unwrap();
        assert!(s.plugin_op("files.write", json!({"handle":handle,"name":"../escape.txt","text":"x"}), &mut cx).is_err());
        assert!(s.plugin_op("files.write", json!({"handle":handle,"name":"run.sh","text":"x"}), &mut cx).is_err());
        assert!(s.plugin_op("document.export", json!({"handle":handle,"name":"image.exe"}), &mut cx).is_err());
        assert!(!dir.join("image.png").exists(), "nothing is written before the event commits");
        // Another plugin can't use the handle.
        let mut other = Staged::default();
        let mut cx2 = OpCtx { plugin: "io.test.other", owner: "y", selection: &Value::Null, staged: &mut other };
        assert_eq!(s.plugin_op("files.write", json!({"handle":handle,"name":"a.txt","text":"x"}), &mut cx2).unwrap_err().code, Code::NotFound);
        staged.publish_files(|| Ok(())).unwrap();
        assert!(s.plugin_commit_picks(staged.picks).is_empty());
        assert!(std::fs::read(dir.join("image.png")).unwrap().starts_with(b"\x89PNG"));
        assert_eq!(std::fs::read_to_string(dir.join("notes.txt")).unwrap(), "hello");
        // Headless without an answer: the plugin hears "cancelled".
        let mut staged = Staged::default();
        let mut cx = OpCtx { plugin: "io.test.ops", owner: "plugin.session.io.test.ops/export", selection: &Value::Null, staged: &mut staged };
        s.plugin_op("files.pick", json!({"mode":"save","name":"out.png"}), &mut cx).unwrap();
        assert_eq!(s.plugin_commit_picks(staged.picks).len(), 1);
        assert_eq!(s.plugin_followups.pop_front().unwrap().1["cancelled"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn histogram_tiles_match_the_full_area_and_reject_excess_work() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":260,"height":258,"background":"#ff0000"})).unwrap();
        let h = op(&mut s, "histogram.get", json!({"composite":true})).unwrap();
        assert_eq!(h["count"], 260 * 258);
        assert_eq!(h["red"][255], 260 * 258);
        // Resize only metadata: a sparse giant document must be rejected before reading pixels.
        std::sync::Arc::make_mut(&mut s.active_mut().unwrap().doc).size = photocraft_geom::Size { width: 100_000, height: 100_000 };
        assert_eq!(op(&mut s, "histogram.get", json!({})).unwrap_err().code, Code::Budget);
        let px = op(&mut s, "pixels.get", json!({"composite":true,"rect":[0,0,1,1]})).unwrap();
        assert_eq!(px["width"], 1, "small reads stay small on a giant canvas");
    }

    #[test]
    fn failed_file_publication_rolls_back_document_history_and_picks() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let before = s.active().unwrap().clone();
        let journal = s.journal.len();
        op(&mut s, "layer.rename", json!({"name":"Transient"})).unwrap();
        let mut staged = Staged::default();
        // Publication creates missing folders, so make the parent a regular file to force a failure.
        std::fs::write(dir.path().join("blocker"), b"").unwrap();
        let handle = s.plugin_picks.grant("io.test.ops", dir.path().join("blocker/out.txt"), crate::plugin_ops::PickMode::Save);
        let mut cx = OpCtx { plugin: "io.test.ops", owner: "x", selection: &Value::Null, staged: &mut staged };
        s.plugin_op("files.write", json!({"handle":handle,"text":"data"}), &mut cx).unwrap();
        s.plugin_op("files.pick", json!({"mode":"folder"}), &mut cx).unwrap();
        let result = staged.publish_files(|| Ok(()));
        assert!(result.is_err());
        s.finish_plugin_edits(Some(before.clone()), journal, result.is_ok(), "Files", false);
        assert_eq!(s.active().unwrap().doc, before.doc);
        assert_eq!(s.active().unwrap().revision, before.revision);
        assert_eq!(s.active().unwrap().history.entries(), before.history.entries());
        assert_eq!(s.journal.len(), journal);
        assert!(s.plugin_followups.is_empty());
        assert!(s.plugin_picks.requests.is_empty());
    }

    #[test]
    fn tool_gestures_refuse_file_operations_before_staging() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let before = s.active().unwrap().clone();
        s.plugin_gesture = Some(Gesture {
            id: 1,
            entry: "test".into(),
            plugin: "io.test.ops".into(),
            contribution: "tool".into(),
            label: "Tool".into(),
            before: Some(before),
            doc: s.active().map(|d| d.doc.id.0),
            epoch: s.active_generation(),
            journal: s.journal.len(),
            grant_generation: 0,
            start: [0.0, 0.0],
            payload: Value::Null,
        });
        for name in ["files.pick", "files.read", "files.write", "document.export", "layer.import"] {
            let mut staged = Staged::default();
            let mut cx = OpCtx { plugin: "io.test.ops", owner: "x", selection: &Value::Null, staged: &mut staged };
            assert_eq!(s.plugin_op(name, json!({}), &mut cx).unwrap_err().code, Code::Denied, "{name}");
            assert!(staged.writes.is_empty());
            assert!(staged.picks.is_empty());
        }
        s.plugin_gesture_end(false);
    }

    #[test]
    fn gesture_events_record_nothing_until_the_gesture_commits() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width":4,"height":4})).unwrap();
        let start = s.active().unwrap().clone();
        let n = start.history.past_len();
        // Two committed events inside a gesture: no history step yet.
        for name in ["First", "Second"] {
            let before = s.active().unwrap().clone();
            let journal = s.journal.len();
            op(&mut s, "layer.rename", json!({"name": name})).unwrap();
            s.finish_plugin_edits(Some(before), journal, true, "Draw · Test", true);
            assert_eq!(s.active().unwrap().history.past_len(), n);
        }
        // The gesture's end folds both into one step from where it began.
        s.finish_plugin_edits(Some(start.clone()), 0, true, "Draw · Test", false);
        assert_eq!(s.active().unwrap().history.past_len(), n + 1);
        assert_eq!(s.active().unwrap().history.undo_label(), Some("Draw · Test"));
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.active().unwrap().doc, start.doc);
    }
}
