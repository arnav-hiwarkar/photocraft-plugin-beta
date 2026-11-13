//! Session request/reply boundary. The document thread must drain requests and own
//! captured targets and the transaction; the plugin worker never accesses a document.
use craft_ext_host::api::{AppApi, AppContract, Code, Invocation, OpError};
use serde_json::Value;
use std::sync::{atomic::Ordering, mpsc};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
    pub document: Option<u64>,
    pub epoch: u64,
    pub revision: u64,
    pub surface: Option<(String, u64)>,
    /// The tool gesture this event belongs to; events of an ended gesture are refused.
    pub gesture: Option<u64>,
}

pub enum Action {
    Begin,
    Call { op: String, args: Value },
    Finish { commit: bool },
}
pub struct Request {
    pub invocation: Invocation,
    pub capture: Capture,
    pub action: Action,
    reply: mpsc::SyncSender<Result<Value, OpError>>,
}
impl Request {
    /// Late requests must be discarded; this flag is also checked by the host.
    pub fn cancelled(&self) -> bool {
        self.invocation.cancelled()
    }
    pub fn reply(self, value: Result<Value, OpError>) {
        let _ = self.reply.send(value);
    }
}
pub struct DocumentApi {
    contract: AppContract,
    requests: mpsc::SyncSender<Request>,
    timeout: Duration,
    pub finish_error: std::sync::Arc<std::sync::Mutex<Option<OpError>>>,
    pub context: std::sync::Arc<std::sync::Mutex<Option<Capture>>>,
}
impl DocumentApi {
    pub fn channel(timeout: Duration) -> Result<(Self, mpsc::Receiver<Request>), craft_ext_core::id::IdError> {
        let (requests, receiver) = mpsc::sync_channel(32);
        Ok((Self { contract: super::contract()?, requests, timeout, finish_error: Default::default(), context: Default::default() }, receiver))
    }
    pub fn fork(&self) -> Result<Self, craft_ext_core::id::IdError> {
        Ok(Self {
            contract: super::contract()?,
            requests: self.requests.clone(),
            timeout: self.timeout,
            finish_error: Default::default(),
            context: Default::default(),
        })
    }
    fn request(&self, invocation: &Invocation, action: Action) -> Result<Value, OpError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        let capture =
            self.context.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone().ok_or_else(|| OpError::new(Code::Stale, "No dispatch context"))?;
        self.requests
            .try_send(Request { capture, invocation: invocation.clone(), action, reply })
            .map_err(|_| OpError::new(Code::Stale, "The document is busy"))?;
        match receiver.recv_timeout(self.timeout) {
            Ok(result) => result,
            Err(_) => {
                invocation.cancel.store(true, Ordering::Relaxed);
                Err(OpError::new(Code::Cancelled, "The document did not reply in time"))
            }
        }
    }
}
impl AppApi for DocumentApi {
    fn contract(&self) -> &AppContract {
        &self.contract
    }
    fn authorizes_on_owner_thread(&self) -> bool {
        true
    }
    fn begin(&self, invocation: &Invocation) -> Result<(), OpError> {
        self.request(invocation, Action::Begin).map(|_| ())
    }
    fn call(&self, invocation: &Invocation, op: &str, args: Value) -> Result<Value, OpError> {
        self.request(invocation, Action::Call { op: op.into(), args })
    }
    fn finish(&self, invocation: &Invocation, commit: bool) {
        // The receiver must always process rollback even when invocation.cancelled().
        *self.finish_error.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = self.request(invocation, Action::Finish { commit }).err();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, atomic::AtomicBool};
    fn invocation() -> Invocation {
        Invocation {
            app: crate::contract().unwrap().app,
            plugin: craft_ext_core::id::ExtensionId::new("io.test.bridge").unwrap(),
            version: craft_ext_core::version::Version::parse("1.0.0").unwrap(),
            contribution: "test".into(),
            grant_generation: 0,
            cancel: Arc::new(AtomicBool::new(false)),
            deadline: None,
            label: "Test · Bridge".into(),
            declared: Arc::from([]),
        }
    }
    fn capture(document: u64) -> Capture {
        Capture { document: Some(document), epoch: 1, revision: 2, surface: None, gesture: None }
    }
    #[test]
    fn recoverable_operation_error_allows_following_call_and_commit() {
        let (api, rx) = DocumentApi::channel(Duration::from_secs(1)).unwrap();
        assert!(api.authorizes_on_owner_thread());
        *api.context.lock().unwrap() = Some(capture(1));
        let worker = std::thread::spawn(move || {
            let inv = invocation();
            api.begin(&inv).unwrap();
            assert_eq!(api.call(&inv, "unknown", Value::Null).unwrap_err().code, Code::NotFound);
            assert!(!inv.cancelled());
            api.call(&inv, "document.info", Value::Null).unwrap();
            api.finish(&inv, true);
            assert!(api.finish_error.lock().unwrap().is_none());
        });
        rx.recv().unwrap().reply(Ok(Value::Null));
        rx.recv().unwrap().reply(Err(OpError::new(Code::NotFound, "Unknown operation")));
        rx.recv().unwrap().reply(Ok(Value::Null));
        let finish = rx.recv().unwrap();
        assert!(matches!(finish.action, Action::Finish { commit: true }));
        finish.reply(Ok(Value::Null));
        worker.join().unwrap();
    }
    #[test]
    fn tools_are_available_at_session_1_1() {
        let c = crate::contract().unwrap();
        assert!(c.provided.unsupported.is_empty());
        let tool: craft_ext_core::manifest::Contribution = serde_json::from_value(
            serde_json::json!({"id":"t","kind":"tool","contract":"photocraft.session@1.1","label":"T","module":"m.wasm"}),
        )
        .unwrap();
        assert!(tool.contract.check(&c.provided).is_none());
    }
    #[test]
    fn every_operation_has_a_total_permission_rule() {
        use craft_ext_core::permission::Permission;
        let c = crate::contract().unwrap();
        let free = ["ui.panel.set", "ui.notify", "canvas.overlay.set", "plugin.resources", "tool.state"];
        for op in &c.ops {
            let needs = (op.requires)(&serde_json::json!({}));
            assert_eq!(needs.is_empty(), free.contains(&op.id), "{}", op.id);
        }
        let pick = c.op("files.pick").unwrap();
        assert_eq!((pick.requires)(&serde_json::json!({"mode":"open"})), vec![Permission::FilesRead]);
        assert_eq!((pick.requires)(&serde_json::json!({"mode":"folder"})), vec![Permission::FilesWrite]);
        // An unknown mode never asks for less.
        assert_eq!((pick.requires)(&serde_json::json!({"mode":"x"})).len(), 2);
    }
    #[test]
    fn workers_keep_independent_dispatch_identity() {
        let (api, rx) = DocumentApi::channel(Duration::from_secs(1)).unwrap();
        let other = api.fork().unwrap();
        *api.context.lock().unwrap() = Some(capture(1));
        *other.context.lock().unwrap() = Some(capture(2));
        let worker = std::thread::spawn(move || {
            api.begin(&invocation()).unwrap();
            other.call(&invocation(), "document.info", Value::Null).unwrap();
        });
        let r = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(r.capture, capture(1));
        r.reply(Ok(Value::Null));
        let r = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(r.capture, capture(2));
        r.reply(Ok(Value::Null));
        worker.join().unwrap();
    }
    #[test]
    fn timeout_cancels_but_rollback_still_crosses_bridge() {
        let (api, rx) = DocumentApi::channel(Duration::from_millis(20)).unwrap();
        *api.context.lock().unwrap() = Some(capture(1));
        let inv = invocation();
        let worker_inv = inv.clone();
        let worker = std::thread::spawn(move || {
            assert!(api.begin(&worker_inv).is_err());
            api.finish(&worker_inv, false);
        });
        let expired = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let rollback = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(inv.cancelled());
        assert!(expired.cancelled());
        assert!(matches!(rollback.action, Action::Finish { commit: false }));
        rollback.reply(Ok(Value::Null));
        worker.join().unwrap();
    }
    #[test]
    fn finish_rejection_is_reported_to_worker() {
        let (api, rx) = DocumentApi::channel(Duration::from_secs(1)).unwrap();
        *api.context.lock().unwrap() = Some(capture(1));
        let status = api.finish_error.clone();
        let worker = std::thread::spawn(move || api.finish(&invocation(), true));
        rx.recv_timeout(Duration::from_secs(1)).unwrap().reply(Err(OpError::denied("revoked")));
        worker.join().unwrap();
        assert_eq!(status.lock().unwrap().as_ref().unwrap().code, Code::Denied);
    }
}
