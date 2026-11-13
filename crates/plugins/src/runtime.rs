//! Ordered session workers. Only the engine consumes document requests.
use crate::document_thread::{Capture, DocumentApi, Request};
use craft_ext_core::{
    manifest::{Contribution, ContributionKind},
    permission::Permission,
    ui::tree::{DrawOp, Node, ui_level_for_session, validate_draw_list},
};
use craft_ext_host::{
    api::{Code, Grants, Invocation, OpError},
    wasm::{Plugin, Session},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

/// Open a fresh view for each authorization; store errors fail closed.
pub struct LiveGrants;
impl LiveGrants {
    fn view(&self) -> Option<craft_ext_store::view::GrantView> {
        craft_ext_store::view::GrantView::open(craft_ext_store::location::default_root().ok()?, crate::contract().ok()?.app).ok()
    }
}
impl Grants for LiveGrants {
    fn generation(&self, p: &craft_ext_core::id::ExtensionId) -> u64 {
        self.view().map_or(u64::MAX, |v| v.generation(p))
    }
    fn allowed(&self, p: &craft_ext_core::id::ExtensionId, r: &Permission) -> bool {
        self.view().is_some_and(|v| v.allowed(p, r))
    }
    fn stopped(&self, p: &craft_ext_core::id::ExtensionId, v: craft_ext_core::version::Version) -> bool {
        crate::safe_mode() || self.view().is_none_or(|g| g.stopped(p, v))
    }
    fn full_access(&self, p: &craft_ext_core::id::ExtensionId) -> bool {
        self.view().is_some_and(|v| v.full_access(p))
    }
}
enum Runner {
    Wasm(Session),
    Native(craft_ext_host::native::Helper),
}
struct Job {
    capture: Capture,
    inv: Invocation,
    event: Value,
    done: mpsc::SyncSender<Result<(), String>>,
    /// Set by the host when it rolled the event back on purpose (Escape): not a failure.
    expected_cancel: Arc<AtomicBool>,
}
pub struct Entry {
    pub id: String,
    pub plugin_name: String,
    pub contribution: Contribution,
    pub invocation: Invocation,
    tx: mpsc::SyncSender<Job>,
}
#[derive(Clone)]
pub struct Surface {
    pub key: String,
    pub id: String,
    pub title: String,
    pub tree: Node,
    pub rev: u64,
    pub open: bool,
    pub owner: String,
}
/// Failed events in a row before a plugin is paused for the rest of the process.
pub const PAUSE_AFTER: u32 = 3;

/// Consecutive failures per plugin, shared with its workers: (count, last error).
pub type Health = Arc<std::sync::Mutex<BTreeMap<String, (u32, String)>>>;

/// A plugin paused after [`PAUSE_AFTER`] failures in a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paused {
    pub plugin: String,
    pub name: String,
    pub reason: String,
}

pub struct Runtime {
    pub entries: Vec<Entry>,
    pub requests: mpsc::Receiver<Request>,
    pub pending: std::collections::VecDeque<Request>,
    pub surfaces: BTreeMap<String, Surface>,
    pub notifications: Vec<String>,
    /// Canvas overlays by plugin id: draw lists in image pixels (`canvas.overlay.set`).
    pub overlays: BTreeMap<String, Vec<DrawOp>>,
    pub generation: u64,
    pub health: Health,
    next_rev: u64,
}
impl Runtime {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let (api, requests) = DocumentApi::channel(Duration::from_secs(10))?;
        let api = Arc::new(api);
        let mut rt = Self {
            entries: vec![],
            requests,
            pending: Default::default(),
            surfaces: BTreeMap::new(),
            notifications: vec![],
            overlays: BTreeMap::new(),
            generation: crate::store::generation()?,
            health: Health::default(),
            next_rev: 0,
        };
        let contract = crate::contract()?;
        for (plugin, version, root, manifest, enabled) in crate::store::enabled()? {
            // A module owns one persistent instance, shared by all its contributions.
            let mut workers: BTreeMap<String, mpsc::SyncSender<Job>> = BTreeMap::new();
            for c in manifest.contributions {
                if !enabled.contains(&c.id)
                    || c.contract.check(&contract.provided).is_some()
                    || !matches!(c.kind, ContributionKind::Command | ContributionKind::Panel | ContributionKind::Tool | ContributionKind::EventHook)
                {
                    continue;
                }
                let module = if manifest.native.is_some() { "#native" } else { c.module.as_deref().ok_or("Session contribution has no module")? };
                let tx = if let Some(tx) = workers.get(module) {
                    tx.clone()
                } else {
                    let program = if manifest.native.is_none() {
                        use std::io::Read;
                        let mut bytes = vec![];
                        std::fs::File::open(root.join(module))?.take(craft_ext_host::wasm::MAX_MODULE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                        Some(Plugin::load(plugin.as_str(), &bytes)?)
                    } else {
                        None
                    };
                    let native = if let Some(n) = &manifest.native {
                        let exe = root.join(n.executables.get(&craft_ext_host::native::platform()).ok_or("No helper for this platform")?);
                        let work = craft_ext_store::Store::at(craft_ext_store::location::default_root()?).private_dir(&contract.app, &plugin);
                        std::fs::create_dir_all(&work)?;
                        Some((plugin.clone(), version, exe, work))
                    } else {
                        None
                    };
                    let (tx, rx) = mpsc::sync_channel::<Job>(64);
                    let api = Arc::new(api.fork()?);
                    let context = api.context.clone();
                    let finish_error = api.finish_error.clone();
                    let health = rt.health.clone();
                    let key = plugin.to_string();
                    std::thread::Builder::new().name(format!("plugin-{plugin}")).spawn(move || {
                        let log: craft_ext_host::wasm::LogFn = Arc::new(|_, _, _| {});
                        let runner = if let Some(p) = program {
                            Session::new(&p, api, Arc::new(LiveGrants), log).map(Runner::Wasm)
                        } else if let Some((id, v, exe, work)) = native {
                            craft_ext_host::native::Helper::start(&id, v, &exe, &work, api, Arc::new(LiveGrants), log).map(Runner::Native)
                        } else {
                            return;
                        };
                        let Ok(mut session) = runner else { return };
                        let lock = |h: &Health| h.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&key).map_or(0, |(n, _)| *n);
                        loop {
                            // Consent, revocation and unreadable grant stores must stop
                            // helpers even when no plugin events arrive.
                            if let Runner::Native(helper) = &mut session {
                                if !helper.supervise() {
                                    break;
                                }
                            }
                            let job = match rx.recv_timeout(Duration::from_secs(1)) {
                                Ok(job) => job,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            };
                            if lock(&health) >= PAUSE_AFTER {
                                let _ = job.done.send(Err("Plugin paused after three failures; resume it in the Plugins window".into()));
                                continue;
                            }
                            *context.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(job.capture);
                            let result = match &mut session {
                                Runner::Wasm(s) => s.dispatch(job.inv, &job.event),
                                Runner::Native(h) => h.dispatch(job.inv, &job.event),
                            }
                            .map_err(|e| e.to_string());
                            let finish = finish_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                            let result = result.and_then(|_| finish.map_or(Ok(()), |e| Err(e.to_string())));
                            {
                                let mut h = health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                                match &result {
                                    // A cancelled event (Escape, a newer gesture) is not the plugin's failure.
                                    Err(e) if !job.expected_cancel.load(std::sync::atomic::Ordering::Relaxed) => {
                                        let entry = h.entry(key.clone()).or_insert((0, String::new()));
                                        *entry = (entry.0.saturating_add(1), e.clone());
                                    }
                                    Err(_) => {}
                                    Ok(()) => {
                                        h.remove(&key);
                                    }
                                }
                            }
                            let _ = job.done.send(result);
                        }
                    })?;
                    workers.insert(module.to_string(), tx.clone());
                    tx
                };
                let inv = Invocation {
                    app: contract.app.clone(),
                    plugin: plugin.clone(),
                    version,
                    contribution: c.id.clone(),
                    grant_generation: LiveGrants.generation(&plugin),
                    cancel: Arc::new(AtomicBool::new(false)),
                    deadline: None,
                    label: format!("{} · {}", c.label, manifest.name),
                    declared: c.permissions.iter().chain(&c.optional_permissions).cloned().collect(),
                };
                rt.entries.push(Entry {
                    id: format!("plugin.session.{}/{}", plugin, c.id),
                    plugin_name: manifest.name.clone(),
                    contribution: c,
                    invocation: inv,
                    tx,
                });
            }
        }
        Ok(rt)
    }
    pub fn valid(&self, inv: &Invocation) -> Result<(), OpError> {
        if inv.cancelled() {
            return Err(OpError::new(Code::Cancelled, "Event expired"));
        }
        let enabled = crate::store::enabled().map_err(|e| OpError::denied(e.to_string()))?;
        if !enabled.iter().any(|(p, v, _, _, cs)| p == &inv.plugin && v == &inv.version && cs.contains(&inv.contribution))
            || LiveGrants.stopped(&inv.plugin, inv.version)
            || LiveGrants.generation(&inv.plugin) != inv.grant_generation
        {
            return Err(OpError::denied("Contribution disabled or grants changed"));
        }
        Ok(())
    }
    pub fn dispatch(&self, id: &str, capture: Capture, event: Value) -> Result<mpsc::Receiver<Result<(), String>>, OpError> {
        self.dispatch_with(id, capture, event, Arc::new(AtomicBool::new(false)))
    }
    /// As [`Runtime::dispatch`]; setting `expected_cancel` before the event finishes keeps its
    /// rollback from counting as a failure.
    pub fn dispatch_with(&self, id: &str, capture: Capture, event: Value, expected_cancel: Arc<AtomicBool>) -> Result<mpsc::Receiver<Result<(), String>>, OpError> {
        let e = self.entries.iter().find(|e| e.id == id).ok_or_else(|| OpError::new(Code::NotFound, "Unknown contribution"))?;
        let mut inv = e.invocation.clone();
        inv.cancel = Arc::new(AtomicBool::new(false));
        inv.deadline = Some(Instant::now() + Duration::from_secs(8));
        inv.grant_generation = LiveGrants.generation(&inv.plugin);
        self.valid(&inv)?;
        let (done, rx) = mpsc::sync_channel(1);
        e.tx.try_send(Job { capture, inv, event, done, expected_cancel }).map_err(|_| OpError::new(Code::Stale, "Plugin worker is busy"))?;
        Ok(rx)
    }
    /// Plugins paused after [`PAUSE_AFTER`] failed events in a row.
    pub fn paused(&self) -> Vec<Paused> {
        let h = self.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        h.iter()
            .filter(|(_, (n, _))| *n >= PAUSE_AFTER)
            .map(|(plugin, (_, reason))| Paused {
                plugin: plugin.clone(),
                name: self.entries.iter().find(|e| e.invocation.plugin.as_str() == plugin).map_or_else(|| plugin.clone(), |e| e.plugin_name.clone()),
                reason: reason.clone(),
            })
            .collect()
    }
    /// Clears a pause; the caller sends `init` again so the plugin redraws its panels.
    pub fn resume(&self, plugin: &str) -> Result<(), String> {
        let mut h = self.health.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if h.get(plugin).is_none_or(|(n, _)| *n < PAUSE_AFTER) {
            return Err(format!("{plugin} is not paused"));
        }
        h.remove(plugin);
        Ok(())
    }
    /// The session level a contribution declared, which sets the UI level its trees may use.
    fn declared_level(&self, inv: &Invocation) -> craft_ext_core::contract::Level {
        self.entries
            .iter()
            .find(|e| e.invocation.plugin == inv.plugin && e.contribution.id == inv.contribution)
            .map_or(craft_ext_core::contract::Level::new(1, 0), |e| e.contribution.contract.level)
    }
    /// Validates an overlay draw list (`canvas.overlay.set`); the caller publishes it on commit.
    pub fn overlay(args: &Value) -> Result<Vec<DrawOp>, OpError> {
        let ops = args.get("ops").cloned().unwrap_or(Value::Array(vec![]));
        if serde_json::to_vec(&ops).map_err(|e| OpError::invalid(e.to_string()))?.len() > 1024 * 1024 {
            return Err(OpError::new(Code::Budget, "The overlay exceeds 1 MiB"));
        }
        let ops: Vec<DrawOp> = serde_json::from_value(ops).map_err(|e| OpError::invalid(e.to_string()))?;
        validate_draw_list(&ops).map_err(|e| OpError::invalid(e.0))?;
        Ok(ops)
    }
    pub fn app_event(&self, capture: Capture, name: &str, payload: Value) {
        for e in &self.entries {
            if e.contribution.kind == ContributionKind::EventHook
                && e.invocation.declared.iter().any(|p| p.to_string() == "app.events")
                && Permission::parse("app.events").is_ok_and(|p| LiveGrants.allowed(&e.invocation.plugin, &p))
            {
                let _ = self.dispatch(&e.id, capture.clone(), json!({"event":"app","name":name,"payload":payload}));
            }
        }
    }
    pub fn ui_change(&mut self, inv: &Invocation, op: &str, args: &Value) -> Result<Value, OpError> {
        if op == "ui.notify" {
            if self.notifications.len() >= 32 {
                return Err(OpError::new(Code::Budget, "Too many notifications"));
            }
            self.notifications.push(args["message"].as_str().unwrap_or("").chars().take(4000).collect());
            return Ok(Value::Null);
        }
        let id = args[if op == "ui.panel.set" { "panel" } else { "id" }]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 64)
            .ok_or_else(|| OpError::invalid("Invalid surface id"))?;
        if op == "ui.panel.set"
            && !self.entries.iter().any(|e| e.invocation.plugin == inv.plugin && e.contribution.id == id && e.contribution.kind == ContributionKind::Panel)
        {
            return Err(OpError::denied("Undeclared panel"));
        }
        let key = format!("{}/{}", inv.plugin, id);
        if !self.surfaces.contains_key(&key) && self.surfaces.len() >= 32 {
            return Err(OpError::new(Code::Budget, "Too many plugin surfaces"));
        }
        let bytes = self
            .surfaces
            .iter()
            .filter(|(k, _)| *k != &key)
            .try_fold(0usize, |total, (_, s)| serde_json::to_vec(&s.tree).map(|b| total.saturating_add(b.len())))
            .map_err(|e| OpError::invalid(e.to_string()))?;
        if bytes.saturating_add(serde_json::to_vec(args).map_err(|e| OpError::invalid(e.to_string()))?.len()) > 4 * 1024 * 1024 {
            return Err(OpError::new(Code::Budget, "Plugin UI exceeds host budget"));
        }
        let tree: Node = serde_json::from_value(args["tree"].clone()).map_err(|e| OpError::invalid(e.to_string()))?;
        tree.validate_at(ui_level_for_session(self.declared_level(inv))).map_err(|e| OpError::invalid(e.0))?;
        self.next_rev = self.next_rev.saturating_add(1);
        let owner = self
            .entries
            .iter()
            .find(|e| e.invocation.plugin == inv.plugin && e.contribution.id == inv.contribution)
            .ok_or_else(|| OpError::denied("Unknown owner"))?
            .id
            .clone();
        // Updating a tree never reopens what the user closed. New panels start closed and open from
        // their menu entry; a window opens because the plugin asked for it (`ui.window.open`).
        let open = op != "ui.panel.set" || self.surfaces.get(&key).is_some_and(|s| s.open);
        self.surfaces.insert(
            key.clone(),
            Surface {
                key,
                id: id.into(),
                title: format!("{} · {}", args["title"].as_str().unwrap_or(id), inv.plugin),
                tree,
                rev: self.next_rev,
                open,
                owner,
            },
        );
        Ok(json!({"rev":self.next_rev}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Runtime, Invocation) {
        let (_, requests) = mpsc::sync_channel(1);
        let (tx, _) = mpsc::sync_channel(1);
        let inv = Invocation {
            app: crate::contract().unwrap().app,
            plugin: craft_ext_core::id::ExtensionId::new("io.test.panels").unwrap(),
            version: craft_ext_core::version::Version::new(1, 0, 0),
            contribution: "panel".into(),
            grant_generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: None,
            label: "Panel · Test".into(),
            declared: Arc::from([]),
        };
        let contribution: Contribution =
            serde_json::from_value(json!({"id":"panel","kind":"panel","contract":"photocraft.session@1.0","label":"Panel","module":"main.wasm"})).unwrap();
        (
            Runtime {
                entries: vec![Entry { id: "panel-command".into(), plugin_name: "Test".into(), contribution, invocation: inv.clone(), tx }],
                requests,
                pending: Default::default(),
                surfaces: Default::default(),
                notifications: vec![],
                overlays: Default::default(),
                generation: 0,
                health: Default::default(),
                next_rev: 0,
            },
            inv,
        )
    }
    #[test]
    fn panel_replacements_get_new_revisions_and_keep_owner() {
        let (mut rt, inv) = fixture();
        let args = json!({"panel":"panel","tree":{"type":"button","id":"go","label":"Go"}});
        let a = rt.ui_change(&inv, "ui.panel.set", &args).unwrap();
        let b = rt.ui_change(&inv, "ui.panel.set", &args).unwrap();
        assert!(b["rev"].as_u64() > a["rev"].as_u64());
        assert_eq!(rt.surfaces.len(), 1);
        assert_eq!(rt.surfaces.values().next().unwrap().owner, "panel-command");
    }
    #[test]
    fn panel_updates_never_reopen_a_closed_panel() {
        let (mut rt, inv) = fixture();
        let args = json!({"panel":"panel","tree":{"type":"label","text":"x"}});
        rt.ui_change(&inv, "ui.panel.set", &args).unwrap();
        // New panels start closed (their menu entry opens them).
        assert!(!rt.surfaces.values().next().unwrap().open);
        rt.surfaces.values_mut().next().unwrap().open = true;
        rt.ui_change(&inv, "ui.panel.set", &args).unwrap();
        assert!(rt.surfaces.values().next().unwrap().open, "an open panel stays open");
        rt.surfaces.values_mut().next().unwrap().open = false;
        rt.ui_change(&inv, "ui.panel.set", &args).unwrap();
        assert!(!rt.surfaces.values().next().unwrap().open, "a panel the user closed stays closed");
    }
    #[test]
    fn ui_level_follows_the_declared_session_contract() {
        let (mut rt, inv) = fixture();
        let tabs = json!({"panel":"panel","tree":{"type":"tabs","id":"t","tabs":[{"id":"a","label":"A","children":[{"type":"label","text":"x"}]}]}});
        // The fixture declares photocraft.session@1.0: a level-1.1 node is refused with the reason.
        assert!(rt.ui_change(&inv, "ui.panel.set", &tabs).is_err());
        rt.entries[0].contribution.contract = craft_ext_core::contract::ContractRef::parse("photocraft.session@1.1").unwrap();
        assert!(rt.ui_change(&inv, "ui.panel.set", &tabs).is_ok());
        let tree = &rt.surfaces.values().next().unwrap().tree;
        assert_eq!(tree.kind(), "tabs");
    }
    #[test]
    fn overlays_are_validated_draw_lists_without_images() {
        assert_eq!(Runtime::overlay(&json!({"ops":[{"op":"line","from":[0,0],"to":[5,5],"width":1,"rgba":[1,0,0,1]}]})).unwrap().len(), 1);
        assert!(Runtime::overlay(&json!({"ops":[{"op":"image","image":"x","min":[0,0],"max":[1,1]}]})).is_err());
        assert!(Runtime::overlay(&json!({"ops":[{"op":"line","from":[0,0],"to":[5,5],"width":-1,"rgba":[1,0,0,1]}]})).is_err());
        assert!(Runtime::overlay(&json!({"ops":"nope"})).is_err());
        assert!(Runtime::overlay(&json!({})).unwrap().is_empty());
    }
    #[test]
    fn three_failures_pause_and_resume_clears_the_pause() {
        let (rt, _) = fixture();
        rt.health.lock().unwrap().insert("io.test.panels".into(), (PAUSE_AFTER - 1, "boom".into()));
        assert!(rt.paused().is_empty());
        assert!(rt.resume("io.test.panels").is_err(), "not paused yet");
        rt.health.lock().unwrap().insert("io.test.panels".into(), (PAUSE_AFTER, "boom".into()));
        assert_eq!(rt.paused(), vec![Paused { plugin: "io.test.panels".into(), name: "Test".into(), reason: "boom".into() }]);
        rt.resume("io.test.panels").unwrap();
        assert!(rt.paused().is_empty());
    }
    #[test]
    fn undeclared_panels_and_invalid_trees_never_publish() {
        let (mut rt, inv) = fixture();
        assert!(rt.ui_change(&inv, "ui.panel.set", &json!({"panel":"foreign","tree":{"type":"label","text":"No"}})).is_err());
        assert!(
            rt.ui_change(&inv, "ui.panel.set", &json!({"panel":"panel","tree":{"type":"slider","id":"x","label":"Size","value":0,"min":2,"max":1}})).is_err()
        );
        assert!(rt.surfaces.is_empty());
    }
}
