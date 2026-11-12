//! Session request/reply boundary. The document thread must drain requests and own
//! captured targets and the transaction; the plugin worker never accesses a document.
use craft_ext_host::api::{AppApi, AppContract, Code, Invocation, OpError};
use serde_json::Value;
use std::sync::{atomic::Ordering, mpsc};
use std::time::Duration;

pub enum Action {
    Begin,
    Call { op: String, args: Value },
    Finish { commit: bool },
}
pub struct Request {
    pub invocation: Invocation,
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
}
impl DocumentApi {
    pub fn channel(timeout: Duration) -> Result<(Self, mpsc::Receiver<Request>), craft_ext_core::id::IdError> {
        let (requests, receiver) = mpsc::sync_channel(32);
        Ok((Self { contract: super::contract()?, requests, timeout }, receiver))
    }
    fn request(&self, invocation: &Invocation, action: Action) -> Result<Value, OpError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.requests.try_send(Request { invocation: invocation.clone(), action, reply }).map_err(|_| OpError::new(Code::Stale, "The document is busy"))?;
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
    fn begin(&self, invocation: &Invocation) -> Result<(), OpError> {
        self.request(invocation, Action::Begin).map(|_| ())
    }
    fn call(&self, invocation: &Invocation, op: &str, args: Value) -> Result<Value, OpError> {
        self.request(invocation, Action::Call { op: op.into(), args })
    }
    fn finish(&self, invocation: &Invocation, commit: bool) {
        // The receiver must always process rollback even when invocation.cancelled().
        // TODO(PhotoCraft-plugin-transactions): wire this receiver to engine transactions.
        let _ = self.request(invocation, Action::Finish { commit });
    }
}
