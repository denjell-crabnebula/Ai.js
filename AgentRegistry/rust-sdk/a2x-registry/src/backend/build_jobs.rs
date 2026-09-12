// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Build job bookkeeping keyed by dataset.
//!
//! Tracks `{status, message, started_at, finished_at, logs}` per dataset,
//! the cancellation token of a running build and the SSE subscribers that
//! receive log and status events.

use std::collections::HashMap;

use parking_lot::Mutex;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::util::now_wall;

/// Subscriber queue capacity; slow consumers drop events.
pub const SUBSCRIBER_CAPACITY: usize = 1000;

/// One dataset's build job.
#[derive(Clone, Debug)]
pub struct BuildJob {
    /// `running`, `done`, `cancelled` or `error`.
    pub status: String,
    pub message: String,
    pub started_at: f64,
    pub finished_at: Option<f64>,
    pub logs: Vec<String>,
    pub cancel: CancellationToken,
}

impl BuildJob {
    /// `{status, message, started_at, finished_at, logs}`.
    pub fn to_json(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("status".into(), Value::String(self.status.clone()));
        m.insert("message".into(), Value::String(self.message.clone()));
        m.insert("started_at".into(), json!(self.started_at));
        m.insert(
            "finished_at".into(),
            self.finished_at.map(|f| json!(f)).unwrap_or(Value::Null),
        );
        m.insert("logs".into(), json!(self.logs));
        m
    }
}

/// Registry of build jobs and their SSE subscribers.
#[derive(Default)]
pub struct BuildJobs {
    jobs: Mutex<HashMap<String, BuildJob>>,
    subs: Mutex<HashMap<String, Vec<mpsc::Sender<Value>>>>,
}

impl BuildJobs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of a job.
    pub fn get(&self, dataset: &str) -> Option<BuildJob> {
        self.jobs.lock().get(dataset).cloned()
    }

    pub fn is_running(&self, dataset: &str) -> bool {
        self.jobs
            .lock()
            .get(dataset)
            .map(|j| j.status == "running")
            .unwrap_or(false)
    }

    /// Status body: `{"dataset": ds, ...job}` or `{"dataset": ds, "status": "idle"}`.
    pub fn status_json(&self, dataset: &str) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("dataset".into(), Value::String(dataset.into()));
        match self.get(dataset) {
            Some(job) => m.extend(job.to_json()),
            None => {
                m.insert("status".into(), Value::String("idle".into()));
            }
        }
        m
    }

    /// Start a new running job. Returns `None` when one is already running.
    pub fn start(&self, dataset: &str, message: &str) -> Option<CancellationToken> {
        let mut jobs = self.jobs.lock();
        if jobs.get(dataset).map(|j| j.status == "running").unwrap_or(false) {
            return None;
        }
        let cancel = CancellationToken::new();
        jobs.insert(
            dataset.to_string(),
            BuildJob {
                status: "running".into(),
                message: message.into(),
                started_at: now_wall(),
                finished_at: None,
                logs: Vec::new(),
                cancel: cancel.clone(),
            },
        );
        Some(cancel)
    }

    /// Append a log line and push a `log` event to subscribers. The log
    /// append and the push happen under the jobs lock so a subscriber's
    /// snapshot never misses or duplicates a line.
    pub fn log(&self, dataset: &str, line: &str) {
        let mut jobs = self.jobs.lock();
        if let Some(job) = jobs.get_mut(dataset) {
            job.logs.push(line.to_string());
        }
        self.push_to_subs(dataset, json!({"type": "log", "message": line}));
        drop(jobs);
    }

    /// Transition a job to a terminal status and push the status event.
    /// When `only_if_running` is set, a job already finished (for example
    /// cancelled) is left untouched.
    pub fn finish(&self, dataset: &str, status: &str, message: &str, only_if_running: bool) -> bool {
        let mut jobs = self.jobs.lock();
        let Some(job) = jobs.get_mut(dataset) else {
            return false;
        };
        if only_if_running && job.status != "running" {
            return false;
        }
        job.status = status.to_string();
        job.message = message.to_string();
        job.finished_at = Some(now_wall());
        self.push_to_subs(
            dataset,
            json!({"type": "status", "status": status, "message": message}),
        );
        drop(jobs);
        true
    }

    /// Cancel a running job. Returns false when nothing is running.
    pub fn cancel(&self, dataset: &str, message: &str) -> bool {
        let token = {
            let jobs = self.jobs.lock();
            match jobs.get(dataset) {
                Some(j) if j.status == "running" => j.cancel.clone(),
                _ => return false,
            }
        };
        token.cancel();
        self.finish(dataset, "cancelled", message, false)
    }

    /// Register an SSE subscriber. Returns the receiver and a guard that
    /// unsubscribes on drop.
    pub fn subscribe(&self, dataset: &str) -> (mpsc::Receiver<Value>, Subscription<'_>) {
        let (_, rx, sub) = self.subscribe_with_snapshot(dataset);
        (rx, sub)
    }

    /// Atomically snapshot the job and register a subscriber, so no event
    /// is lost or duplicated between the replay and the live stream.
    pub fn subscribe_with_snapshot(
        &self,
        dataset: &str,
    ) -> (Option<BuildJob>, mpsc::Receiver<Value>, Subscription<'_>) {
        let jobs = self.jobs.lock();
        let snapshot = jobs.get(dataset).cloned();
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CAPACITY);
        self.subs
            .lock()
            .entry(dataset.to_string())
            .or_default()
            .push(tx.clone());
        drop(jobs);
        (
            snapshot,
            rx,
            Subscription {
                jobs: self,
                dataset: dataset.to_string(),
                tx,
            },
        )
    }

    fn push_to_subs(&self, dataset: &str, item: Value) {
        let subs: Vec<mpsc::Sender<Value>> = self.subs.lock().get(dataset).cloned().unwrap_or_default();
        for s in subs {
            let _ = s.try_send(item.clone());
        }
    }

    fn unsubscribe(&self, dataset: &str, tx: &mpsc::Sender<Value>) {
        if let Some(list) = self.subs.lock().get_mut(dataset) {
            list.retain(|s| !s.same_channel(tx));
        }
    }

    /// Number of live subscribers for a dataset (tests).
    pub fn subscriber_count(&self, dataset: &str) -> usize {
        self.subs.lock().get(dataset).map(|l| l.len()).unwrap_or(0)
    }
}

/// Removes its subscriber when dropped.
pub struct Subscription<'a> {
    jobs: &'a BuildJobs,
    dataset: String,
    tx: mpsc::Sender<Value>,
}

impl Drop for Subscription<'_> {
    fn drop(&mut self) {
        self.jobs.unsubscribe(&self.dataset, &self.tx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};

    #[tokio::test]
    async fn job_lifecycle_and_subscribers() -> TestResult {
        let jobs = BuildJobs::new();
        assert_eq!(jobs.status_json("ds")["status"], "idle");
        let token = jobs.start("ds", "building").required()?;
        assert!(jobs.start("ds", "again").is_none());
        let (mut rx, sub) = jobs.subscribe("ds");
        assert_eq!(jobs.subscriber_count("ds"), 1);
        jobs.log("ds", "line 1");
        assert_eq!(rx.recv().await.required()?["message"], "line 1");
        assert!(jobs.cancel("ds", "cancelled"));
        assert!(token.is_cancelled());
        assert_eq!(rx.recv().await.required()?["status"], "cancelled");
        assert!(!jobs.finish("ds", "done", "x", true));
        assert_eq!(jobs.get("ds").required()?.status, "cancelled");
        assert!(!jobs.cancel("ds", "again"));
        drop(sub);
        assert_eq!(jobs.subscriber_count("ds"), 0);
        let s = jobs.status_json("ds");
        assert_eq!(s["logs"], json!(["line 1"]));
        assert!(s["finished_at"].is_number());
        Ok(())
    }
}
