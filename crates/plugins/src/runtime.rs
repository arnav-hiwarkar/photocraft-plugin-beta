//! Ordered session workers. Only the engine consumes document requests.
use crate::document_thread::{Capture, DocumentApi, Request};
use craft_ext_core::{
    manifest::{Contribution, ContributionKind},
    permission::Permission,
    ui::Node,
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
}
pub struct Entry {
    pub id: String,
    pub plugin_name: String,
    pub contribution: Contribution,
    invocation: Invocation,
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
pub struct Runtime {
    pub entries: Vec<Entry>,
    pub requests: mpsc::Receiver<Request>,
    pub pending: std::collections::VecDeque<Request>,
    pub surfaces: BTreeMap<String, Surface>,
    pub notifications: Vec<String>,
    pub generation: u64,
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
            generation: crate::store::generation()?,
            next_rev: 0,
        };
        let contract = crate::contract()?;
        for (plugin, version, root, manifest, enabled) in crate::store::enabled()? {
            // A module owns one persistent instance, shared by all its contributions.
            let mut workers: BTreeMap<String, mpsc::SyncSender<Job>> = BTreeMap::new();
            for c in manifest.contributions {
                if !enabled.contains(&c.id)
                    || c.contract.check(&contract.provided).is_some()
                    || !matches!(c.kind, ContributionKind::Command | ContributionKind::Panel | ContributionKind::EventHook)
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
                        let mut failures = 0u32;
                        for job in rx {
                            if failures >= 3 {
                                let _ = job.done.send(Err("Plugin paused after three failures".into()));
                                continue;
                            }
                            *context.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(job.capture);
                            let result = match &mut session {
                                Runner::Wasm(s) => s.dispatch(job.inv, &job.event),
                                Runner::Native(h) => h.dispatch(job.inv, &job.event),
                            }
                            .map_err(|e| e.to_string());
                            if result.is_err() {
                                failures = failures.saturating_add(1);
                            } else {
                                failures = 0;
                            }
                            let finish = finish_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
                            let _ = job.done.send(result.and_then(|_| finish.map_or(Ok(()), |e| Err(e.to_string()))));
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
            || LiveGrants.generation(&inv.plugin) != inv.grant_generation
        {
            return Err(OpError::denied("Contribution disabled or grants changed"));
        }
        Ok(())
    }
    pub fn dispatch(&self, id: &str, capture: Capture, event: Value) -> Result<mpsc::Receiver<Result<(), String>>, OpError> {
        let e = self.entries.iter().find(|e| e.id == id).ok_or_else(|| OpError::new(Code::NotFound, "Unknown contribution"))?;
        let mut inv = e.invocation.clone();
        inv.cancel = Arc::new(AtomicBool::new(false));
        inv.deadline = Some(Instant::now() + Duration::from_secs(8));
        inv.grant_generation = LiveGrants.generation(&inv.plugin);
        self.valid(&inv)?;
        let (done, rx) = mpsc::sync_channel(1);
        e.tx.try_send(Job { capture, inv, event, done }).map_err(|_| OpError::new(Code::Stale, "Plugin worker is busy"))?;
        Ok(rx)
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
        tree.validate().map_err(|e| OpError::invalid(e.0))?;
        self.next_rev = self.next_rev.saturating_add(1);
        let owner = self
            .entries
            .iter()
            .find(|e| e.invocation.plugin == inv.plugin && e.contribution.id == inv.contribution)
            .ok_or_else(|| OpError::denied("Unknown owner"))?
            .id
            .clone();
        self.surfaces.insert(
            key.clone(),
            Surface {
                key,
                id: id.into(),
                title: format!("{} · {}", args["title"].as_str().unwrap_or(id), inv.plugin),
                tree,
                rev: self.next_rev,
                open: true,
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
                generation: 0,
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
    fn undeclared_panels_and_invalid_trees_never_publish() {
        let (mut rt, inv) = fixture();
        assert!(rt.ui_change(&inv, "ui.panel.set", &json!({"panel":"foreign","tree":{"type":"label","text":"No"}})).is_err());
        assert!(
            rt.ui_change(&inv, "ui.panel.set", &json!({"panel":"panel","tree":{"type":"slider","id":"x","label":"Size","value":0,"min":2,"max":1}})).is_err()
        );
        assert!(rt.surfaces.is_empty());
    }
}
