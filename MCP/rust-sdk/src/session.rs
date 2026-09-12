// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared session machinery, port of `src/shared/base_session.*`.
//!
//! [`PendingRequests`] tracks outstanding requests, their completion
//! channels and progress callbacks. Both the client and the server session
//! build on it.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

use ap_jsonrpc::{RequestId, RpcError};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::error::McpError;
use crate::types::ProgressToken;

/// Progress callback for long running operations: `(progress, total, message)`.
pub type ProgressCallback = Arc<dyn Fn(f64, Option<f64>, Option<String>) + Send + Sync>;

/// Completion channel payload of a pending request.
pub type PendingResult = Result<Value, RpcError>;

/// How long a progress callback stays reachable after its request completed.
///
/// Progress notifications travel on a different HTTP connection than the
/// response and may arrive slightly later. The C++ SDK drops them; this port
/// keeps the callback for a short grace period.
pub const PROGRESS_GRACE: Duration = Duration::from_secs(5);

struct Pending {
    tx: oneshot::Sender<PendingResult>,
    progress: Option<ProgressCallback>,
}

/// Outstanding requests of one session.
pub struct PendingRequests {
    next_id: AtomicI64,
    map: Mutex<HashMap<RequestId, Pending>>,
    progress: Mutex<HashMap<RequestId, ProgressCallback>>,
    recent_progress: Mutex<Vec<(Instant, RequestId, ProgressCallback)>>,
}

impl Default for PendingRequests {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingRequests {
    /// Create an empty table. Request ids start at 1 like the C++ SDK.
    pub fn new() -> Self {
        Self {
            next_id: AtomicI64::new(1),
            map: Mutex::new(HashMap::new()),
            progress: Mutex::new(HashMap::new()),
            recent_progress: Mutex::new(Vec::new()),
        }
    }

    fn retire_progress(&self, id: &RequestId) {
        let removed = self.progress.lock().remove(id);
        let mut recent = self.recent_progress.lock();
        let now = Instant::now();
        recent.retain(|(at, _, _)| now.duration_since(*at) < PROGRESS_GRACE);
        if let Some(cb) = removed {
            recent.push((now, id.clone(), cb));
        }
    }

    /// Allocate the next request id.
    pub fn next_id(&self) -> RequestId {
        RequestId::Number(self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    /// Register a request and return its id and completion receiver.
    pub fn register(
        &self,
        progress: Option<ProgressCallback>,
    ) -> (RequestId, oneshot::Receiver<PendingResult>) {
        let id = self.next_id();
        let (tx, rx) = oneshot::channel();
        if let Some(cb) = &progress {
            self.progress.lock().insert(id.clone(), cb.clone());
        }
        self.map.lock().insert(id.clone(), Pending { tx, progress });
        (id, rx)
    }

    /// Complete a request. Returns false when the id is unknown.
    pub fn complete(&self, id: &RequestId, result: PendingResult) -> bool {
        self.retire_progress(id);
        let pending = self.map.lock().remove(id);
        match pending {
            Some(p) => {
                drop(p.progress);
                p.tx.send(result).is_ok()
            }
            None => false,
        }
    }

    /// Forget a request, for example after a timeout.
    pub fn remove(&self, id: &RequestId) {
        self.retire_progress(id);
        self.map.lock().remove(id);
    }

    /// Progress callback registered for the request a progress token refers to.
    /// Callbacks of recently completed requests stay reachable for [`PROGRESS_GRACE`].
    pub fn progress_callback(&self, token: &ProgressToken) -> Option<ProgressCallback> {
        let id = token.to_request_id();
        if let Some(cb) = self.progress.lock().get(&id) {
            return Some(cb.clone());
        }
        let now = Instant::now();
        self.recent_progress
            .lock()
            .iter()
            .rev()
            .find(|(at, rid, _)| *rid == id && now.duration_since(*at) < PROGRESS_GRACE)
            .map(|(_, _, cb)| cb.clone())
    }

    /// Number of outstanding requests.
    pub fn len(&self) -> usize {
        self.map.lock().len()
    }

    /// True when no request is outstanding.
    pub fn is_empty(&self) -> bool {
        self.map.lock().is_empty()
    }

    /// Number of registered progress callbacks.
    pub fn progress_len(&self) -> usize {
        self.progress.lock().len()
    }

    /// Fail every outstanding request with the given error.
    pub fn fail_all(&self, error: RpcError) {
        let entries: Vec<(RequestId, Pending)> = self.map.lock().drain().collect();
        self.progress.lock().clear();
        self.recent_progress.lock().clear();
        for (_, pending) in entries {
            let _ = pending.tx.send(Err(error.clone()));
        }
    }
}

/// Wait for a completion channel with a timeout.
pub async fn await_response(
    pending: &PendingRequests,
    id: &RequestId,
    rx: oneshot::Receiver<PendingResult>,
    timeout: Duration,
) -> Result<Value, McpError> {
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(McpError::Rpc(error)),
        Ok(Err(_)) => {
            pending.remove(id);
            Err(McpError::Closed)
        }
        Err(_) => {
            pending.remove(id);
            Err(McpError::Timeout(timeout.as_millis() as u64))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, ResultExt, TestResult};
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn register_complete_and_progress_lookup() -> TestResult {
        let pending = PendingRequests::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let cb: ProgressCallback = Arc::new(move |_, _, _| {
            c.fetch_add(1, Ordering::SeqCst);
        });
        let (id, rx) = pending.register(Some(cb));
        assert_eq!(id, RequestId::Number(1));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending.progress_len(), 1);
        pending.progress_callback(&ProgressToken::Number(1)).required()?(0.5, None, None);
        pending.progress_callback(&ProgressToken::from("1")).required()?(1.0, None, None);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(pending.complete(&id, Ok(json!({"ok": true}))));
        assert_eq!(pending.progress_len(), 0);
        // Still reachable during the grace period after completion.
        pending.progress_callback(&ProgressToken::Number(1)).required()?(1.0, None, None);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert!(pending.progress_callback(&ProgressToken::Number(99)).is_none());
        assert_eq!(rx.blocking_recv()??, json!({"ok": true}));
        assert!(!pending.complete(&id, Ok(Value::Null)));
        let (id2, _rx2) = pending.register(None);
        assert_eq!(id2, RequestId::Number(2));
        pending.remove(&id2);
        assert!(pending.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn await_response_paths() -> TestResult {
        let pending = PendingRequests::new();
        let (id, rx) = pending.register(None);
        pending.complete(&id, Err(RpcError::internal_error("err")));
        let err = await_response(&pending, &id, rx, Duration::from_millis(50))
            .await
            .err_or_fail()?;
        assert_eq!(err.code(), Some(-32603));

        let (id, rx) = pending.register(None);
        let err = await_response(&pending, &id, rx, Duration::from_millis(5))
            .await
            .err_or_fail()?;
        assert!(matches!(err, McpError::Timeout(5)));
        assert!(pending.is_empty());

        let (id, rx) = pending.register(None);
        pending.fail_all(RpcError::internal_error("closed"));
        let err = await_response(&pending, &id, rx, Duration::from_millis(50))
            .await
            .err_or_fail()?;
        assert_eq!(err.message(), "closed");
        Ok(())
    }
}
