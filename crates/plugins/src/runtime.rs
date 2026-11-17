use crate::document_thread::{Capture, DocumentApi, Request};
use craft_ext_core::{
    id::{AppId, ExtensionId},
    manifest::{Contribution, ContributionKind, Manifest, Runtime as Execution},
    permission::Permission,
    ui::tree::{DrawOp, Node, ui_level_for_session, validate_draw_list},
    version::Version,
};
use craft_ext_host::{
    api::{Code, Grants, Invocation, OpError},
    jobs::JobBoard,
    native::{Helper, HelperLaunch, Launch},
    policy::Strikes,
    wasm::{Plugin, PluginError, Session},
};
use craft_ext_store::Store;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, PoisonError, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

pub struct LiveGrants;
impl LiveGrants {
    fn view(&self) -> Option<craft_ext_store::view::GrantView> {
        craft_ext_store::view::GrantView::open(craft_ext_store::location::default_root().ok()?, crate::contract().ok()?.app).ok()
    }
}
impl Grants for LiveGrants {
    fn generation(&self, p: &ExtensionId) -> u64 {
        self.view().map_or(u64::MAX, |v| v.generation(p))
    }
    fn allowed(&self, p: &ExtensionId, r: &Permission) -> bool {
        self.view().is_some_and(|v| v.allowed(p, r))
    }
    fn stopped(&self, p: &ExtensionId, v: Version) -> bool {
        crate::safe_mode() || self.view().is_none_or(|g| g.stopped(p, v))
    }
    fn full_access(&self, p: &ExtensionId) -> bool {
        self.view().is_some_and(|v| v.full_access(p))
    }
}
enum Runner {
    Wasm(Session),
    Native(Helper),
}
impl Runner {
    fn dispatch(&mut self, inv: Invocation, event: &Value) -> Result<(), PluginError> {
        match self {
            Self::Wasm(s) => s.dispatch(inv, event),
            Self::Native(h) => h.dispatch(inv, event),
        }
    }
}
struct Job {
    capture: Capture,
    inv: Invocation,
    event: Value,
    done: mpsc::SyncSender<Result<(), String>>,
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
pub type Health = Arc<Mutex<BTreeMap<ExtensionId, Strikes>>>;

pub type Problems = Arc<Mutex<Vec<(ExtensionId, String)>>>;

fn worker_key(launch: &Launch) -> String {
    match launch {
        Launch::Wasm(module) => module.to_string_lossy().into_owned(),
        Launch::Native(_) => "#native".into(),
    }
}

enum Program {
    Wasm(Plugin),
    Native(HelperLaunch),
}

impl Program {
    fn load(plugin: &ExtensionId, launch: Launch) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(match launch {
            Launch::Wasm(module) => {
                use std::io::Read;
                let mut bytes = vec![];
                std::fs::File::open(module)?.take(craft_ext_host::wasm::MAX_MODULE_BYTES as u64 + 1).read_to_end(&mut bytes)?;
                Self::Wasm(Plugin::load(plugin.as_str(), &bytes)?)
            }
            Launch::Native(helper) => {
                std::fs::create_dir_all(&helper.work)?;
                Self::Native(helper)
            }
        })
    }

    fn start(&self, plugin: &ExtensionId, version: Version, api: Arc<DocumentApi>, jobs: Arc<JobBoard>) -> Result<Runner, PluginError> {
        let log: craft_ext_host::wasm::LogFn = Arc::new(|_, _, _| {});
        match self {
            Self::Wasm(p) => Session::new(p, api, Arc::new(LiveGrants), log).map(|mut s| {
                s.set_jobs(jobs);
                Runner::Wasm(s)
            }),
            Self::Native(launch) => Helper::start(plugin, version, launch, api, Arc::new(LiveGrants), log).map(|mut h| {
                h.set_jobs(jobs);
                Runner::Native(h)
            }),
        }
    }
}

pub fn record_outcome(health: &Health, store: &Store, app: &AppId, plugin: &ExtensionId, version: Version, outcome: Result<(), &PluginError>) {
    let mut h = health.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(reason) = h.entry(plugin.clone()).or_default().record(outcome)
        && let Err(why) = store.quarantine(app, plugin, version, &reason)
    {
        eprintln!("plugins: {plugin} could not be turned off after repeated failures: {why}");
    }
}

struct Worker {
    plugin: ExtensionId,
    version: Version,
    health: Health,
    problems: Problems,
    jobs: Arc<JobBoard>,
}

impl Worker {
    fn problem(&self, message: String) {
        self.problems.lock().unwrap_or_else(PoisonError::into_inner).push((self.plugin.clone(), message));
    }

    fn record(&self, outcome: Result<(), &PluginError>) {
        if let (Ok(store), Ok(app)) = (crate::store::open(), crate::store::app()) {
            record_outcome(&self.health, &store, &app, &self.plugin, self.version, outcome);
        }
    }

    fn spawn(self, program: Program, api: Arc<DocumentApi>) -> Result<mpsc::SyncSender<Job>, Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::sync_channel::<Job>(64);
        let context = api.context.clone();
        let finish_error = api.finish_error.clone();
        std::thread::Builder::new().name(format!("plugin-{}", self.plugin)).spawn(move || {
            let start = || program.start(&self.plugin, self.version, api.clone(), self.jobs.clone());
            let mut runner = match start() {
                Ok(runner) => Some(runner),
                Err(e) => return self.problem(format!("could not start: {e}")),
            };
            loop {
                if let Some(Runner::Native(helper)) = &mut runner
                    && !helper.supervise()
                {
                    runner = None;
                    if LiveGrants.stopped(&self.plugin, self.version) || !LiveGrants.full_access(&self.plugin) {
                        return;
                    }
                    self.record(Err(&PluginError::Trap("the helper stopped".into())));
                }
                let job = match rx.recv_timeout(Duration::from_millis(250)) {
                    Ok(job) => job,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                };
                let running = match runner.take().map_or_else(&start, Ok) {
                    Ok(r) => runner.insert(r),
                    Err(e) => {
                        let _ = job.done.send(Err(e.to_string()));
                        continue;
                    }
                };
                *context.lock().unwrap_or_else(PoisonError::into_inner) = Some(job.capture);
                let result = running.dispatch(job.inv, &job.event);
                if matches!(running, Runner::Native(_)) && matches!(result, Err(PluginError::Stopped(_) | PluginError::TooSlow | PluginError::Trap(_))) {
                    runner = None;
                }
                if result.is_ok() || !job.expected_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    self.record(result.as_ref().map(|_| ()));
                }
                let finish = finish_error.lock().unwrap_or_else(PoisonError::into_inner).take();
                let result = result.map_err(|e| e.to_string()).and_then(|_| finish.map_or(Ok(()), |e| Err(e.to_string())));
                let _ = job.done.send(result);
            }
        })?;
        Ok(tx)
    }
}

pub struct Runtime {
    pub entries: Vec<Entry>,
    pub requests: mpsc::Receiver<Request>,
    pub pending: std::collections::VecDeque<Request>,
    pub surfaces: BTreeMap<String, Surface>,
    pub notifications: Vec<String>,
    pub errors: Vec<String>,
    pub overlays: BTreeMap<String, Vec<DrawOp>>,
    pub generation: u64,
    pub health: Health,
    pub problems: Problems,
    pub jobs: Arc<JobBoard>,
    pub unclean: Vec<ExtensionId>,
    inflight: Vec<(String, mpsc::Receiver<Result<(), String>>)>,
    next_rev: u64,
}
impl Runtime {
    pub fn load(jobs: Arc<JobBoard>) -> Result<Self, Box<dyn std::error::Error>> {
        let (api, requests) = DocumentApi::channel(Duration::from_secs(10))?;
        let mut rt = Self {
            entries: vec![],
            requests,
            pending: Default::default(),
            surfaces: BTreeMap::new(),
            notifications: vec![],
            errors: vec![],
            overlays: BTreeMap::new(),
            generation: crate::store::generation()?,
            health: Health::default(),
            problems: Problems::default(),
            jobs,
            unclean: vec![],
            inflight: vec![],
            next_rev: 0,
        };
        let store = crate::store::open()?;
        let enabled = crate::store::enabled()?;
        rt.problems.lock().unwrap_or_else(PoisonError::into_inner).extend(enabled.problems);
        for (plugin, version, root, manifest, on) in enabled.releases {
            let entries = rt.load_plugin(&api, &store, &plugin, version, &root, &manifest, &on);
            rt.entries.extend(entries);
        }
        rt.cancel_unloaded_jobs();
        Ok(rt)
    }

    pub fn load_problems(&self) -> Vec<(ExtensionId, String)> {
        self.problems.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn begin_run(&self) -> Vec<ExtensionId> {
        let running = self.running();
        let previous = match (crate::store::open(), crate::store::app()) {
            (Ok(store), Ok(app)) => store.begin_run(&app, &running).unwrap_or_default(),
            _ => vec![],
        };
        previous.into_iter().filter(|p| running.contains(p)).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn load_plugin(
        &self,
        api: &DocumentApi,
        store: &Store,
        plugin: &ExtensionId,
        version: Version,
        root: &Path,
        manifest: &Manifest,
        enabled: &std::collections::BTreeSet<String>,
    ) -> Vec<Entry> {
        let contract = match crate::contract() {
            Ok(c) => c,
            Err(e) => {
                self.problems.lock().unwrap_or_else(PoisonError::into_inner).push((plugin.clone(), e.to_string()));
                return vec![];
            }
        };
        let work = store.private_dir(&contract.app, plugin);
        let mut workers: BTreeMap<String, mpsc::SyncSender<Job>> = BTreeMap::new();
        let mut entries = vec![];
        for c in &manifest.contributions {
            if !enabled.contains(&c.id)
                || c.contract.check(&contract.provided).is_some()
                || !matches!(c.kind, ContributionKind::Command | ContributionKind::Panel | ContributionKind::Tool | ContributionKind::EventHook)
                || matches!(manifest.runtime(c), Execution::Data)
            {
                continue;
            }
            let worker = |launch: Launch| -> Result<mpsc::SyncSender<Job>, Box<dyn std::error::Error>> {
                let worker = Worker { plugin: plugin.clone(), version, health: self.health.clone(), problems: self.problems.clone(), jobs: self.jobs.clone() };
                worker.spawn(Program::load(plugin, launch)?, Arc::new(api.fork()?))
            };
            let tx = match Launch::plan(manifest, c, root, &work) {
                Ok(launch) => {
                    let key = worker_key(&launch);
                    match workers.get(&key).cloned().map_or_else(|| worker(launch), Ok) {
                        Ok(tx) => {
                            workers.insert(key, tx.clone());
                            tx
                        }
                        Err(e) => {
                            self.problems.lock().unwrap_or_else(PoisonError::into_inner).push((plugin.clone(), format!("{}: {e}", c.label)));
                            continue;
                        }
                    }
                }
                Err(e) => {
                    self.problems.lock().unwrap_or_else(PoisonError::into_inner).push((plugin.clone(), format!("{}: {e}", c.label)));
                    continue;
                }
            };
            let inv = Invocation {
                app: contract.app.clone(),
                plugin: plugin.clone(),
                version,
                contribution: c.id.clone(),
                grant_generation: LiveGrants.generation(plugin),
                cancel: Arc::new(AtomicBool::new(false)),
                deadline: None,
                label: format!("{} · {}", c.label, manifest.name),
                document: None,
                declared: c.permissions.iter().chain(&c.optional_permissions).cloned().collect(),
            };
            entries.push(Entry { id: format!("plugin.session.{}/{}", plugin, c.id), plugin_name: manifest.name.clone(), contribution: c.clone(), invocation: inv, tx });
        }
        entries
    }

    pub fn running(&self) -> Vec<ExtensionId> {
        let mut ids: Vec<ExtensionId> = self.entries.iter().map(|e| e.invocation.plugin.clone()).collect();
        ids.dedup();
        ids
    }

    pub fn cancel_unloaded_jobs(&self) {
        let running = self.running();
        for job in self.jobs.list() {
            if !job.state.finished() && !running.contains(&job.plugin) {
                self.jobs.cancel_plugin(&job.plugin);
            }
        }
    }

    pub fn post(&mut self, id: &str, capture: Capture, event: Value) -> Result<(), OpError> {
        let done = self.dispatch(id, capture, event)?;
        self.inflight.push((id.to_owned(), done));
        Ok(())
    }

    pub fn deliver_jobs(&mut self, capture: &Capture, busy: bool) {
        let Some(document) = capture.document.map(|d| d.to_string()).filter(|_| !busy) else { return };
        let owners: Vec<(ExtensionId, String, String)> = self.entries.iter().map(|e| (e.invocation.plugin.clone(), e.contribution.id.clone(), e.id.clone())).collect();
        let owner = |job: &craft_ext_host::jobs::JobInfo| owners.iter().find(|(p, c, _)| p == &job.plugin && c == &job.contribution).map(|(_, _, id)| id.clone());
        let jobs = self.jobs.clone();
        jobs.deliver(
            |job| job.document.as_deref() == Some(document.as_str()) && owner(job).is_some(),
            |job| owner(job).is_some_and(|id| self.post(&id, capture.clone(), job.event()).is_ok()),
        );
    }

    pub fn settle(&mut self) {
        let mut errors = vec![];
        self.inflight.retain(|(_, done)| match done.try_recv() {
            Ok(Ok(())) => false,
            Ok(Err(e)) => {
                errors.push(e);
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => false,
        });
        self.errors.extend(errors);
    }

    pub fn busy(&self) -> bool {
        !self.inflight.is_empty()
    }

    pub fn valid(&self, inv: &Invocation) -> Result<(), OpError> {
        if inv.cancelled() {
            return Err(OpError::new(Code::Cancelled, "Event expired"));
        }
        let enabled = crate::store::enabled().map_err(|e| OpError::denied(e.to_string()))?.releases;
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
    pub fn dispatch_with(&self, id: &str, capture: Capture, event: Value, expected_cancel: Arc<AtomicBool>) -> Result<mpsc::Receiver<Result<(), String>>, OpError> {
        let e = self.entries.iter().find(|e| e.id == id).ok_or_else(|| OpError::new(Code::NotFound, "Unknown contribution"))?;
        let mut inv = e.invocation.clone();
        inv.document = capture.document.map(|d| d.to_string());
        inv.cancel = Arc::new(AtomicBool::new(false));
        inv.deadline = Some(Instant::now() + Duration::from_secs(8));
        inv.grant_generation = LiveGrants.generation(&inv.plugin);
        self.valid(&inv)?;
        let (done, rx) = mpsc::sync_channel(1);
        e.tx.try_send(Job { capture, inv, event, done, expected_cancel }).map_err(|_| OpError::new(Code::Stale, "Plugin worker is busy"))?;
        Ok(rx)
    }
    fn declared_level(&self, inv: &Invocation) -> craft_ext_core::contract::Level {
        self.entries
            .iter()
            .find(|e| e.invocation.plugin == inv.plugin && e.contribution.id == inv.contribution)
            .map_or(craft_ext_core::contract::Level::new(1, 0), |e| e.contribution.contract.level)
    }
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
            document: None,
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
                errors: vec![],
                overlays: Default::default(),
                generation: 0,
                health: Default::default(),
                problems: Default::default(),
                jobs: JobBoard::new(),
                unclean: vec![],
                inflight: vec![],
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
    fn a_finished_job_waits_for_its_document_and_for_no_gesture() {
        let (mut rt, inv) = fixture();
        let capture = |document| Capture { document, epoch: 0, revision: 0, surface: None, gesture: None };
        let ticket = rt.jobs.admit(&inv.plugin, "panel", "Analyse", true, Some("7")).unwrap();
        ticket.finish(craft_ext_host::jobs::JobState::Done(json!({"frames": 3})));
        let pending = |rt: &Runtime| rt.jobs.list().iter().filter(|j| !j.delivered).count();
        rt.deliver_jobs(&capture(Some(8)), false);
        rt.deliver_jobs(&capture(Some(7)), true);
        rt.deliver_jobs(&capture(None), false);
        assert_eq!(pending(&rt), 1, "another document, an open gesture or no document keeps the result");
        assert!(rt.inflight.is_empty());
    }
    #[test]
    fn jobs_of_plugins_that_are_no_longer_loaded_are_cancelled() {
        let (rt, inv) = fixture();
        let mine = rt.jobs.admit(&inv.plugin, "panel", "Mine", true, None).unwrap();
        let gone = rt.jobs.admit(&ExtensionId::new("io.test.gone").unwrap(), "panel", "Gone", true, None).unwrap();
        rt.cancel_unloaded_jobs();
        assert!(!mine.cancel.load(std::sync::atomic::Ordering::Relaxed));
        assert!(gone.cancel.load(std::sync::atomic::Ordering::Relaxed));
    }
    #[test]
    fn the_third_fault_in_a_row_quarantines_the_release() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        let app = AppId::new("photocraft").unwrap();
        let installed = install_test_package(&store);
        store.set_active(&app, &installed.id, installed.version).unwrap();
        store.set_enabled(&app, &installed.id, "run", true).unwrap();
        let health = Health::default();
        let trap = PluginError::Trap("boom".into());
        let record = |outcome: Result<(), &PluginError>| record_outcome(&health, &store, &app, &installed.id, installed.version, outcome);
        record(Err(&trap));
        record(Ok(()));
        record(Err(&trap));
        record(Err(&PluginError::Failed(-1, "bad input".into())));
        record(Err(&PluginError::Stopped("document changed".into())));
        record(Err(&trap));
        assert!(store.state().unwrap().apps[&app][&installed.id].quarantine.is_none(), "a success resets the count and the plugin's own errors never count");
        record(Err(&trap));
        let q = store.state().unwrap().apps[&app][&installed.id].quarantine.clone().unwrap();
        assert_eq!((q.version, q.reason.as_str()), (installed.version, "stopped after 3 failures in a row: the plugin crashed: boom"));
        let view = craft_ext_store::view::GrantView::open(dir.path(), app.clone()).unwrap();
        assert!(view.stopped(&installed.id, installed.version));
        assert!(view.enabled_contributions(&app).releases.is_empty(), "a quarantined release does not load at the next start");
    }
    fn install_test_package(store: &Store) -> craft_ext_store::store::Installed {
        use std::io::Write;
        let manifest = br#"schema = 2
id = "io.test.crashy"
name = "Crashy"
version = "1.0.0"
license = "MIT"
source = "https://github.com/example/crashy"
summary = "Crashes"
apps = ["photocraft"]
[[contribution]]
id = "run"
kind = "command"
contract = "photocraft.session@1.0"
label = "Run"
module = "run.wasm"
"#;
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, bytes) in [(craft_ext_core::manifest::MANIFEST_NAME, &manifest[..]), ("run.wasm", b"\0asm\x01\0\0\0")] {
            zip.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            zip.write_all(bytes).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        store
            .install_bytes(&bytes, craft_ext_core::tier::Tier::Community, craft_ext_store::state::Origin::Catalogue { vendor: "example".into(), repo: "crashy".into() })
            .unwrap()
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
