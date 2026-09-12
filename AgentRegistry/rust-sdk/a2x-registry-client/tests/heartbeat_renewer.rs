// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `test_heartbeat_renewer.py`: task lifecycle and backoff.
//!
//! Tokio's paused clock replaces the real sleeps of the Python tests.

use ap_support::testing::{ResultExt, TestResult};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use a2x_registry_client::{ClientError, HeartbeatFn, HeartbeatRegistry, HeartbeatRenewer};

fn counting_fn(calls: Arc<Mutex<Vec<(String, String)>>>) -> HeartbeatFn {
    Arc::new(move |ds, sid| {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.lock().push((ds, sid));
            Ok(())
        })
    })
}

fn noop_fn() -> HeartbeatFn {
    Arc::new(|_, _| Box::pin(async { Ok(()) }))
}

#[tokio::test(start_paused = true)]
async fn renewer_calls_fn_at_period() -> TestResult {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let r = HeartbeatRenewer::new(
        "ds",
        "sid",
        3,
        counting_fn(Arc::clone(&calls)),
        Some(Duration::from_millis(50)),
    )?;
    r.start();
    assert!(r.is_running());
    // The first heartbeat happens after the first period, not at start.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(calls.lock().is_empty());
    tokio::time::sleep(Duration::from_millis(250)).await;
    r.stop().await;
    let n = calls.lock().len();
    assert!(
        n >= 3,
        "expected at least 3 calls in 0.25s with period=0.05, got {n}"
    );
    assert_eq!(calls.lock()[0], ("ds".to_string(), "sid".to_string()));
    assert!(!r.is_running());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn renewer_idempotent_start_stop() -> TestResult {
    let r = HeartbeatRenewer::new("ds", "sid", 3, noop_fn(), Some(Duration::from_millis(50)))?;
    r.stop().await; // before start: must not panic
    r.start();
    r.start(); // second start is a no-op
    assert!(r.is_running());
    r.stop().await;
    r.stop().await; // double stop
    assert!(r.is_stopped());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn renewer_task_is_aborted_when_last_handle_drops() -> TestResult {
    // Python uses daemon threads so process exit never hangs; here the task
    // is aborted as soon as the last handle is dropped.
    let calls = Arc::new(Mutex::new(Vec::new()));
    let r = HeartbeatRenewer::new(
        "ds",
        "sid",
        3,
        counting_fn(Arc::clone(&calls)),
        Some(Duration::from_millis(10)),
    )?;
    r.start();
    tokio::time::sleep(Duration::from_millis(35)).await;
    let before = calls.lock().len();
    assert!(before >= 2);
    drop(r);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(calls.lock().len(), before, "task kept running after drop");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn renewer_backoff_on_failure() -> TestResult {
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let failing: HeartbeatFn = Arc::new(move |_, _| {
        let a = Arc::clone(&a);
        Box::pin(async move {
            a.fetch_add(1, Ordering::SeqCst);
            Err(ClientError::Connection {
                message: "simulated network failure".into(),
            })
        })
    });
    // period 0.05s, ttl 4s: attempts at 0.05, +0.1, +0.2, +0.4 ... capped at 4s.
    let r = HeartbeatRenewer::new("ds", "sid", 4, failing, Some(Duration::from_millis(50)))?;
    r.start();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let n = attempts.load(Ordering::SeqCst);
    assert!(
        (3..=4).contains(&n),
        "expected exponential backoff (3 or 4 attempts in 0.5s), got {n}"
    );
    // After the cap the renewer keeps trying once per ttl instead of giving up.
    // Schedule: 0.75, 1.55, 3.15, 6.35 (cap reached), 10.35, 14.35, 18.35.
    tokio::time::sleep(Duration::from_secs(20)).await;
    let later = attempts.load(Ordering::SeqCst);
    assert!(
        (6..=8).contains(&(later - n)),
        "attempts after the cap should be about one per ttl, got {}",
        later - n
    );
    r.stop().await;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn renewer_resets_period_after_success() -> TestResult {
    let attempts = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&attempts);
    let flaky: HeartbeatFn = Arc::new(move |_, _| {
        let a = Arc::clone(&a);
        Box::pin(async move {
            let n = a.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(ClientError::Connection {
                    message: "once".into(),
                })
            } else {
                Ok(())
            }
        })
    });
    let r = HeartbeatRenewer::new("ds", "sid", 3, flaky, Some(Duration::from_millis(10)))?;
    r.start();
    // t=10 fail, t=30 ok (period back to 10), t=40, t=50 ... ok
    tokio::time::sleep(Duration::from_millis(95)).await;
    r.stop().await;
    let n = attempts.load(Ordering::SeqCst);
    assert!(
        n >= 7,
        "period should reset to nominal after success, got {n} attempts"
    );
    Ok(())
}

#[tokio::test]
async fn renewer_rejects_bad_ttl() -> TestResult {
    let err = HeartbeatRenewer::new("ds", "sid", 0, noop_fn(), None).err_or_fail()?;
    assert!(matches!(err, ClientError::InvalidArgument { .. }));
    let r = HeartbeatRenewer::new("ds", "sid", 60, noop_fn(), None)?;
    assert_eq!(r.period(), Duration::from_secs(20));
    let r = HeartbeatRenewer::new("ds", "sid", 2, noop_fn(), None)?;
    assert_eq!(r.period(), Duration::from_secs(1), "period never drops below 1s");
    assert_eq!(r.ttl(), 2);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn registry_replaces_existing_renewer() -> TestResult {
    let reg = HeartbeatRegistry::new();
    let r1 = HeartbeatRenewer::new("ds", "sid", 5, noop_fn(), Some(Duration::from_millis(100)))?;
    let r2 = HeartbeatRenewer::new("ds", "sid", 5, noop_fn(), Some(Duration::from_millis(100)))?;
    reg.add(r1.clone());
    reg.add(r2.clone());
    assert!(r1.is_stopped());
    assert!(!r2.is_stopped());
    assert!(r2.is_running());
    assert_eq!(reg.len(), 1);
    assert!(reg.contains("ds", "sid"));
    reg.shutdown_all(Duration::from_secs(1)).await;
    assert!(r2.is_stopped());
    assert!(reg.is_empty());
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn registry_shutdown_all_stops_everyone() -> TestResult {
    let reg = HeartbeatRegistry::new();
    let renewers: Vec<_> = (0..5)
        .map(|i| -> TestResult<_> {
            Ok({
                HeartbeatRenewer::new(
                    "ds",
                    &format!("sid{i}"),
                    5,
                    noop_fn(),
                    Some(Duration::from_millis(100)),
                )?
            })
        })
        .collect::<TestResult<Vec<_>>>()?;
    for r in &renewers {
        reg.add(r.clone());
    }
    assert_eq!(reg.len(), 5);
    reg.shutdown_all(Duration::from_secs(1)).await;
    for r in &renewers {
        assert!(r.is_stopped());
        assert!(!r.is_running());
    }
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn registry_remove_is_idempotent() -> TestResult {
    let reg = HeartbeatRegistry::new();
    let r = HeartbeatRenewer::new("ds", "sid", 5, noop_fn(), Some(Duration::from_millis(100)))?;
    reg.add(r.clone());
    reg.remove("ds", "sid").await;
    assert!(r.is_stopped());
    reg.remove("ds", "sid").await;
    reg.remove("other", "sid").await;
    assert!(reg.is_empty());
    Ok(())
}
