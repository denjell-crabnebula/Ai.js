// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Server core tests: task manager, request handler, JSON-RPC handler, updater,
//! server implementation and builder. Ports the intent of `tests/ut/server/*`.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use parking_lot::Mutex;
use std::net::SocketAddr;
use std::sync::Arc;

use a2a_sdk::server::*;
use a2a_sdk::types::*;
use a2a_sdk::{A2AErrorCode, A2aServerError};
use async_trait::async_trait;
use common::*;
use serde_json::{Value, json};

fn make_task(id: &str, ctx: &str, state: TaskState) -> Task {
    Task {
        id: id.into(),
        context_id: ctx.into(),
        status: TaskStatus::new(state),
        ..Default::default()
    }
}

fn status_event(id: &str, ctx: &str, state: TaskState) -> TaskStatusUpdateEvent {
    TaskStatusUpdateEvent {
        context_id: ctx.into(),
        metadata: None,
        status: TaskStatus::new(state),
        task_id: id.into(),
    }
}

fn artifact_event(
    id: &str,
    ctx: &str,
    artifact_id: &str,
    text: &str,
    append: Option<bool>,
) -> TaskArtifactUpdateEvent {
    TaskArtifactUpdateEvent {
        artifact: Artifact {
            artifact_id: artifact_id.into(),
            parts: vec![Part::text(text)],
            ..Default::default()
        },
        context_id: ctx.into(),
        task_id: id.into(),
        append,
        ..Default::default()
    }
}

type Captured = Arc<Mutex<Vec<StreamEvent>>>;

fn capture() -> (Captured, EventCb) {
    let events: Captured = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let cb: EventCb = Arc::new(move |ev: &StreamEvent| sink.lock().push(ev.clone()));
    (events, cb)
}

// ---------------------------------------------------------------------------
// TaskManager
// ---------------------------------------------------------------------------

#[test]
fn task_manager_register_and_get() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = TaskManager::new(store.clone());
    assert!(
        mgr.register_task("", Arc::new(TaskExecuteInfo::default()))
            .is_err()
    );
    mgr.register_task("task-1", Arc::new(TaskExecuteInfo::default()))?;
    assert!(mgr.is_registered("task-1"));
    assert!(mgr.get_task("task-1").is_none());
    store.save(&make_task("task-1", "ctx-1", TaskState::Working), None);
    assert_eq!(mgr.get_task("task-1").required()?.context_id, "ctx-1");
    assert_eq!(mgr.get_context_id("task-1"), "ctx-1");
    assert_eq!(mgr.get_context_id("nope"), "");
    assert!(mgr.get_task("unregistered").is_none());
    Ok(())
}

#[test]
fn task_manager_save_task_event_semantics() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = TaskManager::new(store.clone());
    mgr.register_task("task-1", Arc::new(TaskExecuteInfo::default()))?;

    // Task event saves directly.
    mgr.save_task_event(&StreamEvent::Task(make_task(
        "task-1",
        "ctx-1",
        TaskState::Working,
    )));
    assert_eq!(
        store.get("task-1", None).required()?.status.state,
        TaskState::Working
    );

    // Status event without a stored task creates one.
    mgr.register_task("task-2", Arc::new(TaskExecuteInfo::default()))?;
    mgr.save_task_event(&StreamEvent::StatusUpdate(status_event(
        "task-2",
        "ctx-2",
        TaskState::Working,
    )));
    let t2 = store.get("task-2", None).required()?;
    assert_eq!(t2.context_id, "ctx-2");
    assert_eq!(t2.status.state, TaskState::Working);

    // Metadata merge.
    let mut existing = make_task("task-1", "ctx-1", TaskState::Working);
    existing.metadata = Some(json!({"a": 1}));
    store.save(&existing, None);
    let mut ev = status_event("task-1", "ctx-1", TaskState::Completed);
    ev.metadata = Some(json!({"b": 2}));
    mgr.save_task_event(&StreamEvent::StatusUpdate(ev));
    let stored = store.get("task-1", None).required()?;
    assert_eq!(stored.metadata, Some(json!({"a": 1, "b": 2})));
    assert_eq!(stored.status.state, TaskState::Completed);

    // Status message moves into history.
    let mut existing = make_task("task-1", "ctx-1", TaskState::Working);
    existing.status.message = Some(user_message("old", "old"));
    store.save(&existing, None);
    let mut ev = status_event("task-1", "ctx-1", TaskState::Working);
    ev.status.message = Some(user_message("new", "new"));
    mgr.save_task_event(&StreamEvent::StatusUpdate(ev));
    let stored = store.get("task-1", None).required()?;
    assert_eq!(stored.history.as_ref().required()?.len(), 1);
    assert_eq!(stored.history.as_ref().required()?[0].message_id, "old");
    assert_eq!(stored.status.message.as_ref().required()?.message_id, "new");

    // Artifact event appends.
    mgr.save_task_event(&StreamEvent::ArtifactUpdate(artifact_event(
        "task-1", "ctx-1", "a", "1", None,
    )));
    mgr.save_task_event(&StreamEvent::ArtifactUpdate(artifact_event(
        "task-1",
        "ctx-1",
        "a",
        "2",
        Some(true),
    )));
    let stored = store.get("task-1", None).required()?;
    assert_eq!(stored.artifacts.as_ref().required()?[0].parts.len(), 2);

    // Message events are ignored, mismatching context only logs.
    mgr.save_task_event(&StreamEvent::Message(user_message("m", "x")));
    mgr.save_task_event(&StreamEvent::StatusUpdate(status_event(
        "task-1",
        "other-ctx",
        TaskState::Working,
    )));
    assert_eq!(
        store.get("task-1", None).required()?.status.state,
        TaskState::Working
    );
    Ok(())
}

#[test]
fn task_manager_ensure_task_for_event() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = TaskManager::new(store.clone());
    store.save(&make_task("t", "c", TaskState::Working), None);
    let t = mgr.ensure_task_for_event(&StreamEvent::StatusUpdate(status_event(
        "t",
        "c",
        TaskState::Completed,
    )));
    assert_eq!(t.status.state, TaskState::Working);
    let t = mgr.ensure_task_for_event(&StreamEvent::ArtifactUpdate(artifact_event(
        "new", "c2", "a", "x", None,
    )));
    assert_eq!(t.status.state, TaskState::Submitted);
    assert_eq!(t.context_id, "c2");
    assert!(mgr.is_registered("new"));
    assert!(store.get("new", None).is_some());
    Ok(())
}

#[test]
fn task_manager_process_and_callbacks() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = TaskManager::new(store.clone());
    let info = Arc::new(TaskExecuteInfo::default());
    mgr.register_task("task-1", info.clone())?;
    store.save(&make_task("task-1", "ctx-1", TaskState::Submitted), None);
    let (events, cb) = capture();
    mgr.add_event_callback("task-1", cb);
    assert_eq!(info.callback_count(), 1);

    mgr.process(
        "task-1",
        &StreamEvent::StatusUpdate(status_event("task-1", "ctx-1", TaskState::Working)),
    );
    assert_eq!(events.lock().len(), 1);
    assert_eq!(
        store.get("task-1", None).required()?.status.state,
        TaskState::Working
    );
    mgr.process(
        "task-1",
        &StreamEvent::ArtifactUpdate(artifact_event("task-1", "ctx-1", "a", "x", None)),
    );
    assert_eq!(events.lock().len(), 2);
    assert_eq!(
        store
            .get("task-1", None)
            .required()?
            .artifacts
            .as_ref()
            .required()?
            .len(),
        1
    );
    mgr.process("task-1", &StreamEvent::Message(user_message("m", "x")));
    assert_eq!(events.lock().len(), 3);
    assert!(!mgr.is_registered("task-1"), "message is final");
    mgr.process(
        "task-1",
        &StreamEvent::StatusUpdate(status_event("task-1", "ctx-1", TaskState::Working)),
    );
    assert_eq!(events.lock().len(), 3, "unregistered task ignored");

    let mgr2 = TaskManager::new(store.clone());
    mgr2.register_task("task-1", Arc::new(TaskExecuteInfo::default()))?;
    mgr2.process(
        "task-1",
        &StreamEvent::StatusUpdate(status_event("task-1", "ctx-1", TaskState::Completed)),
    );
    assert!(!mgr2.is_registered("task-1"));
    Ok(())
}

#[test]
fn task_manager_update_with_message_and_flags() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = TaskManager::new(store.clone());
    let mut task = make_task("t", "c", TaskState::Working);
    task.status.message = Some(user_message("s", "status"));
    let updated = mgr.update_with_message(&user_message("u", "user"), task);
    assert!(updated.status.message.is_none());
    let h = updated.history.as_ref().required()?;
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].message_id, "s");
    assert_eq!(h[1].message_id, "u");
    assert_eq!(
        store
            .get("t", None)
            .required()?
            .history
            .as_ref()
            .required()?
            .len(),
        2
    );
    let updated = mgr.update_with_message(&user_message("u2", "x"), make_task("t2", "c", TaskState::Working));
    assert_eq!(updated.history.as_ref().required()?.len(), 1);

    assert!(
        !mgr.exchange_message_sent("t", true),
        "unregistered returns false"
    );
    mgr.register_task("t", Arc::new(TaskExecuteInfo::default()))?;
    assert!(!mgr.exchange_message_sent("t", true));
    assert!(mgr.exchange_message_sent("t", false));
    assert!(!mgr.exchange_message_sent("t", true));
    Ok(())
}

#[test]
fn task_manager_cancel_task() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = TaskManager::new(store.clone());
    let mut done = make_task("done", "c", TaskState::Completed);
    mgr.cancel_task(&mut done);
    assert_eq!(done.status.state, TaskState::Completed);

    let mut idle = make_task("idle", "c", TaskState::InputRequired);
    store.save(&idle, None);
    mgr.cancel_task(&mut idle);
    assert_eq!(idle.status.state, TaskState::Canceled);
    assert_eq!(
        store.get("idle", None).required()?.status.state,
        TaskState::Canceled
    );

    let mut active = make_task("active", "c", TaskState::Working);
    store.save(&active, None);
    mgr.register_task("active", Arc::new(TaskExecuteInfo::default()))?;
    let (events, cb) = capture();
    mgr.add_event_callback("active", cb);
    mgr.cancel_task(&mut active);
    assert_eq!(active.status.state, TaskState::Canceled);
    let events = events.lock();
    assert_eq!(events.len(), 1);
    assert!(matches!(&events[0], StreamEvent::StatusUpdate(u) if u.status.state == TaskState::Canceled));
    assert!(!mgr.is_registered("active"));
    Ok(())
}

// ---------------------------------------------------------------------------
// TaskUpdaterImpl
// ---------------------------------------------------------------------------

fn updater_fixture() -> TestResult<(
    Arc<InMemoryTaskStore>,
    Arc<TaskManager>,
    TaskUpdaterImpl,
    Captured,
)> {
    let store = Arc::new(InMemoryTaskStore::new());
    let mgr = Arc::new(TaskManager::new(store.clone()));
    store.save(&make_task("task-1", "ctx-1", TaskState::Submitted), None);
    mgr.register_task("task-1", Arc::new(TaskExecuteInfo::default()))?;
    let (events, cb) = capture();
    mgr.add_event_callback("task-1", cb);
    let updater = TaskUpdaterImpl::new("task-1", "ctx-1", mgr.clone());
    Ok((store, mgr, updater, events))
}

#[test]
fn updater_status_events() -> TestResult {
    let (store, _mgr, updater, events) = updater_fixture()?;
    updater.update_status(TaskState::Working, None, Some("ts".into()), Some(json!({"m": 1})));
    {
        let events = events.lock();
        match &events[0] {
            StreamEvent::StatusUpdate(u) => {
                assert_eq!(u.task_id, "task-1");
                assert_eq!(u.context_id, "ctx-1");
                assert_eq!(u.status.state, TaskState::Working);
                assert_eq!(u.status.timestamp.as_deref(), Some("ts"));
                assert_eq!(u.metadata, Some(json!({"m": 1})));
            }
            other => {
                return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into());
            }
        }
    }
    updater.start_work(None);
    let ts = match &events.lock()[1] {
        StreamEvent::StatusUpdate(u) => u.status.timestamp.clone().required()?,
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    };
    assert_eq!(ts.len(), 24);
    assert!(ts.ends_with('Z'));
    assert!(!updater.terminal_state_reached());
    updater.complete(Some(updater.new_agent_message(vec![Part::text("bye")], None)));
    assert!(updater.terminal_state_reached());
    assert_eq!(events.lock().len(), 3);
    updater.start_work(None);
    updater.add_artifact(&TaskArtifactParam::default());
    assert_eq!(events.lock().len(), 3, "ignored after terminal state");
    assert_eq!(
        store.get("task-1", None).required()?.status.state,
        TaskState::Completed
    );
    Ok(())
}

#[test]
fn updater_shortcuts_and_artifacts() -> TestResult {
    for (name, state) in [
        ("failed", TaskState::Failed),
        ("reject", TaskState::Rejected),
        ("submit", TaskState::Submitted),
        ("cancel", TaskState::Canceled),
        ("requires_input", TaskState::InputRequired),
        ("requires_auth", TaskState::AuthRequired),
    ] {
        let (_store, _mgr, updater, events) = updater_fixture()?;
        match name {
            "failed" => updater.failed(None),
            "reject" => updater.reject(None),
            "submit" => updater.submit(None),
            "cancel" => updater.cancel(None),
            "requires_input" => updater.requires_input(None),
            _ => updater.requires_auth(None),
        }
        let events = events.lock();
        assert!(
            matches!(&events[0], StreamEvent::StatusUpdate(u) if u.status.state == state),
            "{name}"
        );
    }

    let (_store, _mgr, updater, events) = updater_fixture()?;
    updater.add_artifact(&TaskArtifactParam {
        parts: vec![Part::text("x")],
        artifact_id: Some("art".into()),
        name: Some("n".into()),
        metadata: Some(json!({"k": 1})),
        append: true,
        last_chunk: true,
        extensions: vec!["e".into()],
    });
    updater.add_artifact(&TaskArtifactParam::default());
    let events = events.lock();
    match &events[0] {
        StreamEvent::ArtifactUpdate(a) => {
            assert_eq!(a.artifact.artifact_id, "art");
            assert_eq!(a.artifact.name.as_deref(), Some("n"));
            assert_eq!(a.artifact.extensions, Some(vec!["e".to_string()]));
            assert_eq!(a.append, Some(true));
            assert_eq!(a.last_chunk, Some(true));
            assert_eq!(a.metadata, Some(json!({"k": 1})));
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    }
    match &events[1] {
        StreamEvent::ArtifactUpdate(a) => assert!(!a.artifact.artifact_id.is_empty()),
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    }
    let m = updater.new_agent_message(vec![Part::text("hi")], Some(json!({"x": 1})));
    assert_eq!(m.role, Role::Agent);
    assert_eq!(m.task_id.as_deref(), Some("task-1"));
    assert_eq!(m.context_id.as_deref(), Some("ctx-1"));
    assert!(!m.message_id.is_empty());
    assert_eq!(m.metadata, Some(json!({"x": 1})));
    Ok(())
}

#[test]
fn updater_send_response_message() -> TestResult {
    let (_store, mgr, updater, events) = updater_fixture()?;
    updater.send_response_message(&user_message("m", "x"));
    assert!(updater.terminal_state_reached());
    assert!(matches!(&events.lock()[0], StreamEvent::Message(m) if m.message_id == "m"));
    assert!(!mgr.is_registered("task-1"));
    Ok(())
}

// ---------------------------------------------------------------------------
// DefaultRequestHandler
// ---------------------------------------------------------------------------

fn handler_with(executor: Option<Arc<dyn AgentExecutor>>) -> (DefaultRequestHandler, Arc<InMemoryTaskStore>) {
    let store = Arc::new(InMemoryTaskStore::new());
    let card = Arc::new(make_agent_card("http://h/jsonrpc", true));
    let handler = DefaultRequestHandler::new(executor, card, Some(store.clone()));
    (handler, store)
}

fn params_for(msg: Message) -> MessageSendParams {
    MessageSendParams {
        message: msg,
        ..Default::default()
    }
}

#[test]
fn determine_task_id_rules() -> TestResult {
    let (handler, store) = handler_with(None);
    let (id, existing) = handler.determine_task_id(&params_for(user_message("m", "x")), None)?;
    assert!(id.starts_with("task-"));
    assert!(existing.is_none());

    store.save(&make_task("t1", "c1", TaskState::Working), None);
    let mut msg = user_message("m", "x");
    msg.task_id = Some("t1".into());
    let (id, existing) = handler.determine_task_id(&params_for(msg.clone()), None)?;
    assert_eq!(id, "t1");
    assert_eq!(existing.required()?.context_id, "c1");

    msg.context_id = Some("other".into());
    let err = handler
        .determine_task_id(&params_for(msg.clone()), None)
        .err_or_fail()?;
    assert_eq!(err.status_code(), A2AErrorCode::JsonrpcInvalidRequest.code());

    msg.context_id = None;
    msg.task_id = Some("missing".into());
    let err = handler.determine_task_id(&params_for(msg), None).err_or_fail()?;
    assert_eq!(err.status_code(), A2AErrorCode::TaskNotFound.code());
    Ok(())
}

#[test]
fn related_tasks_from_reference_ids() -> TestResult {
    let (handler, store) = handler_with(None);
    let mut msg = user_message("m", "x");
    assert!(
        handler
            .get_related_tasks_from_reference_task_ids(&params_for(msg.clone()), None)
            .is_empty()
    );
    msg.reference_task_ids = Some(vec![]);
    assert!(
        handler
            .get_related_tasks_from_reference_task_ids(&params_for(msg.clone()), None)
            .is_empty()
    );
    store.save(&make_task("a", "c", TaskState::Completed), None);
    store.save(&make_task("b", "c2", TaskState::Failed), None);
    msg.reference_task_ids = Some(vec!["b".into(), "".into(), "missing".into(), "a".into()]);
    let related = handler.get_related_tasks_from_reference_task_ids(&params_for(msg), None);
    assert_eq!(related.len(), 2);
    assert_eq!(related[0].id, "b");
    assert_eq!(related[1].id, "a");
    Ok(())
}

#[tokio::test]
async fn get_task_and_history_trimming() -> TestResult {
    let (handler, store) = handler_with(None);
    let err = handler
        .on_get_task(TaskQueryParams::new("nope"), None)
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32001);
    assert_eq!(err.message(), "Task id not found");
    let mut t = make_task("t", "c", TaskState::Working);
    t.history = Some((0..5).map(|i| user_message(&format!("m{i}"), "x")).collect());
    store.save(&t, None);
    let got = handler.on_get_task(TaskQueryParams::new("t"), None).await?;
    assert_eq!(got.history.as_ref().required()?.len(), 5);
    let got = handler
        .on_get_task(
            TaskQueryParams {
                id: "t".into(),
                history_length: Some(2),
                metadata: None,
            },
            None,
        )
        .await?;
    let h = got.history.as_ref().required()?;
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].message_id, "m3");
    let got = handler
        .on_get_task(
            TaskQueryParams {
                id: "t".into(),
                history_length: Some(10),
                metadata: None,
            },
            None,
        )
        .await?;
    assert_eq!(got.history.as_ref().required()?.len(), 5);
    Ok(())
}

#[tokio::test]
async fn cancel_task_rules() -> TestResult {
    let (handler, store) = handler_with(None);
    let err = handler
        .on_cancel_task(TaskIdParams::new("nope"), None)
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32001);
    store.save(&make_task("done", "c", TaskState::Completed), None);
    let err = handler
        .on_cancel_task(TaskIdParams::new("done"), None)
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32002);
    assert_eq!(err.message(), "Cancel task failed");
    store.save(&make_task("w", "c", TaskState::Working), None);
    let t = handler.on_cancel_task(TaskIdParams::new("w"), None).await?;
    assert_eq!(t.status.state, TaskState::Canceled);
    assert_eq!(store.get("w", None).required()?.status.state, TaskState::Canceled);

    let (handler, store) = handler_with(Some(Arc::new(HelloExecutor)));
    store.save(&make_task("w", "c", TaskState::Working), None);
    handler
        .task_manager()
        .register_task("w", Arc::new(TaskExecuteInfo::default()))?;
    let t = handler.on_cancel_task(TaskIdParams::new("w"), None).await?;
    assert_eq!(t.status.state, TaskState::Canceled);
    assert!(
        t.status.timestamp.is_some(),
        "state produced by the executor's updater"
    );
    assert!(!handler.task_manager().is_registered("w"));

    // An unregistered task is canceled by the manager without an updater event.
    store.save(&make_task("u", "c", TaskState::Working), None);
    let t = handler.on_cancel_task(TaskIdParams::new("u"), None).await?;
    assert_eq!(t.status.state, TaskState::Canceled);
    assert!(t.status.timestamp.is_none());
    Ok(())
}

#[tokio::test]
async fn push_notification_config_handlers() -> TestResult {
    let (handler, store) = handler_with(None);
    let cfg = TaskPushNotificationConfig {
        task_id: "t".into(),
        push_notification_config: PushNotificationConfig {
            url: "u".into(),
            ..Default::default()
        },
    };
    let err = handler
        .on_set_task_push_notification_config(cfg.clone(), None)
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32001);
    store.save(&make_task("t", "c", TaskState::Working), None);
    handler
        .on_set_task_push_notification_config(cfg.clone(), None)
        .await?;
    let got = handler
        .on_get_task_push_notification_config(
            GetTaskPushNotificationConfigParams {
                id: "t".into(),
                metadata: None,
                push_notification_config_id: None,
            },
            None,
        )
        .await?;
    assert_eq!(got.push_notification_config.url, "u");
    assert_eq!(got.push_notification_config.id.as_deref(), Some("t"));
    let list = handler
        .on_list_task_push_notification_configs(
            ListTaskPushNotificationConfigParams {
                id: "t".into(),
                metadata: None,
            },
            None,
        )
        .await?;
    assert_eq!(list.len(), 1);
    handler
        .on_delete_task_push_notification_config(
            DeleteTaskPushNotificationConfigParams {
                id: "t".into(),
                metadata: None,
                push_notification_config_id: "t".into(),
            },
            None,
        )
        .await?;
    let fallback = handler
        .on_get_task_push_notification_config(
            GetTaskPushNotificationConfigParams {
                id: "t".into(),
                metadata: None,
                push_notification_config_id: None,
            },
            None,
        )
        .await?;
    assert_eq!(fallback.task_id, "t");
    assert!(fallback.push_notification_config.url.is_empty());

    let no_store = DefaultRequestHandler::with_stores(
        None,
        Arc::new(make_agent_card("http://h/jsonrpc", false)),
        store.clone(),
        None,
        None,
    );
    for err in [
        no_store
            .on_set_task_push_notification_config(cfg, None)
            .await
            .err_or_fail()?,
        no_store
            .on_get_task_push_notification_config(
                GetTaskPushNotificationConfigParams {
                    id: "t".into(),
                    metadata: None,
                    push_notification_config_id: None,
                },
                None,
            )
            .await
            .err_or_fail()?,
        no_store
            .on_list_task_push_notification_configs(
                ListTaskPushNotificationConfigParams {
                    id: "t".into(),
                    metadata: None,
                },
                None,
            )
            .await
            .err_or_fail()?,
        no_store
            .on_delete_task_push_notification_config(
                DeleteTaskPushNotificationConfigParams {
                    id: "t".into(),
                    metadata: None,
                    push_notification_config_id: "t".into(),
                },
                None,
            )
            .await
            .err_or_fail()?,
    ] {
        assert_eq!(err.status_code(), -32003);
    }
    assert_eq!(handler.on_get_card(None)?.name, "TestAgent");
    Ok(())
}

/// Push sender recording the task states it was called with.
struct RecordingSender(Mutex<Vec<TaskState>>);

impl PushNotificationSender for RecordingSender {
    fn send_notification(&self, task: &Task) {
        self.0.lock().push(task.status.state);
    }
}

#[tokio::test]
async fn send_message_creates_task_and_stores_push_config() -> TestResult {
    let store = Arc::new(InMemoryTaskStore::new());
    let push_store = Arc::new(InMemoryPushNotificationConfigStore::new());
    let sender = Arc::new(RecordingSender(Mutex::new(Vec::new())));
    let handler = DefaultRequestHandler::with_stores(
        Some(Arc::new(HelloExecutor)),
        Arc::new(make_agent_card("http://h/jsonrpc", false)),
        store.clone(),
        Some(push_store.clone()),
        Some(sender.clone()),
    );
    let (events, emit) = capture();
    let params = MessageSendParams {
        configuration: Some(MessageSendConfiguration {
            push_notification_config: Some(PushNotificationConfig {
                url: "http://hook".into(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        message: user_message("m", "hello"),
        metadata: None,
    };
    handler.on_send_message(params, None, emit, "SendMessage").await?;
    let events = events.lock();
    assert_eq!(events.len(), 1);
    let task_id = match &events[0] {
        StreamEvent::Message(m) => {
            assert_eq!(m.parts[0].text.as_deref(), Some("Processed: hello"));
            m.task_id.clone().required()?
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    };
    assert_eq!(
        store.get(&task_id, None).required()?.status.state,
        TaskState::Completed
    );
    assert_eq!(push_store.get_info(&task_id).len(), 1);
    assert_eq!(*sender.0.lock(), vec![TaskState::Completed]);
    Ok(())
}

#[tokio::test]
async fn send_message_on_existing_task_appends_history() -> TestResult {
    let (handler, store) = handler_with(Some(Arc::new(InputRequiredExecutor)));
    let (events, emit) = capture();
    handler
        .on_send_message(
            params_for(user_message("m1", "first")),
            None,
            emit.clone(),
            "SendMessage",
        )
        .await?;
    let task_id = match &events.lock()[0] {
        StreamEvent::Task(t) => {
            assert_eq!(t.status.state, TaskState::InputRequired);
            t.id.clone()
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    };
    let mut second = user_message("m2", "second");
    second.task_id = Some(task_id.clone());
    handler
        .on_send_message(params_for(second), None, emit, "SendMessage")
        .await?;
    let stored = store.get(&task_id, None).required()?;
    let ids: Vec<&str> = stored
        .history
        .as_ref()
        .required()?
        .iter()
        .map(|m| m.message_id.as_str())
        .collect();
    assert!(ids.contains(&"m1") && ids.contains(&"m2"));
    assert_eq!(events.lock().len(), 2);

    let (handler, _store) = handler_with(None);
    let err = handler
        .on_send_message(
            params_for(user_message("m", "x")),
            None,
            Arc::new(|_| {}),
            "SendMessage",
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32603);
    Ok(())
}

#[tokio::test]
async fn streaming_handlers() -> TestResult {
    let (handler, store) = handler_with(Some(Arc::new(StreamingExecutor)));
    let (events, emit) = capture();
    handler
        .on_send_message_streaming(params_for(user_message("m", "x")), emit.clone(), None)
        .await?;
    let task_id = {
        let events = events.lock();
        assert_eq!(events.len(), 5);
        match &events[0] {
            StreamEvent::Task(t) => {
                assert_eq!(t.status.state, TaskState::Submitted);
                t.id.clone()
            }
            other => {
                return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into());
            }
        }
    };
    assert_eq!(
        store.get(&task_id, None).required()?.status.state,
        TaskState::Completed
    );

    let (resub_events, resub_emit) = capture();
    handler
        .on_resubscribe_to_task(TaskIdParams::new(task_id.clone()), resub_emit, None)
        .await?;
    assert_eq!(resub_events.lock().len(), 1);
    let err = handler
        .on_resubscribe_to_task(TaskIdParams::new("nope"), Arc::new(|_| {}), None)
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32001);

    let mut again = user_message("m2", "y");
    again.task_id = Some(task_id);
    let err = handler
        .on_send_message_streaming(params_for(again), Arc::new(|_| {}), None)
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32004);
    assert_eq!(err.message(), "Cannot execute task in final state");

    let (existing, id) = handler.initialize_streaming_task(&params_for(user_message("m3", "z")), None)?;
    assert_eq!(existing.required()?.status.state, TaskState::Submitted);
    assert!(handler.task_manager().is_registered(&id));
    Ok(())
}

#[tokio::test]
async fn resubscribe_to_running_task_receives_later_events() -> TestResult {
    let (handler, store) = handler_with(Some(Arc::new(InputRequiredExecutor)));
    store.save(&make_task("run", "c", TaskState::Working), None);
    handler
        .task_manager()
        .register_task("run", Arc::new(TaskExecuteInfo::default()))?;
    let (events, emit) = capture();
    handler
        .on_resubscribe_to_task(TaskIdParams::new("run"), emit, None)
        .await?;
    assert_eq!(events.lock().len(), 1);
    let updater = TaskUpdaterImpl::new("run", "c", handler.task_manager().clone());
    updater.complete(None);
    let events = events.lock();
    assert_eq!(events.len(), 2);
    assert!(matches!(&events[1], StreamEvent::StatusUpdate(u) if u.status.state == TaskState::Completed));
    Ok(())
}

// ---------------------------------------------------------------------------
// JsonRpcHandler
// ---------------------------------------------------------------------------

fn rpc(id: Value, method: &str, params: Option<Value>) -> Value {
    let mut v = json!({"jsonrpc": "2.0", "id": id, "method": method});
    if let Some(p) = params {
        v["params"] = p;
    }
    v
}

#[tokio::test]
async fn jsonrpc_handler_responses() -> TestResult {
    let (handler, store) = handler_with(Some(Arc::new(HelloExecutor)));
    let h = JsonRpcHandler::new(Arc::new(handler));

    let resp = h
        .on_get_task(&rpc(json!(1), "GetTask", Some(json!({"id": "nope"}))))
        .await
        .to_value();
    assert_eq!(resp["error"]["code"], -32001);
    assert_eq!(resp["id"], 1);
    let resp = h.on_get_task(&rpc(json!("x"), "GetTask", None)).await.to_value();
    assert_eq!(resp["error"]["code"], -32603);
    assert_eq!(resp["error"]["message"], "key 'params' not found");
    let resp = h
        .on_get_task(&rpc(json!("x"), "GetTask", Some(json!({"id": ""}))))
        .await
        .to_value();
    assert_eq!(resp["error"]["code"], -32603);

    let mut t = make_task("t", "c", TaskState::Working);
    t.history = Some(vec![user_message("a", "1"), user_message("b", "2")]);
    store.save(&t, None);
    let resp = h
        .on_get_task(&rpc(
            json!("x"),
            "GetTask",
            Some(json!({"id": "t", "historyLength": 1})),
        ))
        .await
        .to_value();
    assert_eq!(resp["result"]["history"].as_array().required()?.len(), 1);
    assert_eq!(resp["id"], "x");

    let resp = h
        .on_cancel_task(&rpc(json!(2), "CancelTask", Some(json!({"id": "t"}))))
        .await
        .to_value();
    assert_eq!(resp["result"]["status"]["state"], "TASK_STATE_CANCELED");

    let set_req = rpc(
        json!(3),
        "CreateTaskPushNotificationConfig",
        Some(json!({"taskId": "t", "pushNotificationConfig": {"url": "u"}})),
    );
    let resp = h.on_set_push_notification_config(&set_req).await.to_value();
    assert_eq!(resp["result"]["pushNotificationConfig"]["url"], "u");
    let get_req = rpc(
        json!(4),
        "GetTaskPushNotificationConfig",
        Some(json!({"id": "t"})),
    );
    let resp = h.on_get_push_notification_config(&get_req).await.to_value();
    assert_eq!(resp["result"]["pushNotificationConfig"]["id"], "t");
    let list_req = rpc(
        json!(5),
        "ListTaskPushNotificationConfigs",
        Some(json!({"id": "t"})),
    );
    let resp = h.on_list_push_notification_config(&list_req).await.to_value();
    assert_eq!(resp["result"].as_array().required()?.len(), 1);
    let del_req = rpc(
        json!(6),
        "DeleteTaskPushNotificationConfig",
        Some(json!({"id": "t", "pushNotificationConfigId": "t"})),
    );
    let resp = h.on_delete_push_notification_config(&del_req).await.to_value();
    assert!(resp["result"].is_null());
    assert!(resp.get("error").is_none());
    let del_missing = rpc(
        json!(6),
        "DeleteTaskPushNotificationConfig",
        Some(json!({"id": "zzz", "pushNotificationConfigId": "t"})),
    );
    let resp = h
        .on_delete_push_notification_config(&del_missing)
        .await
        .to_value();
    assert_eq!(resp["error"]["code"], -32001);
    let resp = h
        .on_get_agent_card(&rpc(json!(7), "GetAgentCard", None))
        .to_value();
    assert_eq!(resp["result"]["name"], "TestAgent");

    let (events, emit) = capture();
    let params = json!({"message": {"messageId": "m", "parts": [{"text": "hi"}]}});
    let resp = h
        .on_message_send(&rpc(json!(8), "SendMessage", Some(params)), emit, "SendMessage")
        .await;
    assert!(resp.is_none());
    assert_eq!(events.lock().len(), 1);
    let resp = h
        .on_message_send(
            &rpc(json!(9), "SendMessage", Some(json!({}))),
            Arc::new(|_| {}),
            "SendMessage",
        )
        .await
        .required()?
        .to_value();
    assert_eq!(resp["error"]["code"], -32603);
    let bad_task = json!({"message": {"messageId": "m", "taskId": "missing", "parts": [{"text": "hi"}]}});
    let resp = h
        .on_message_send(
            &rpc(json!(10), "SendMessage", Some(bad_task)),
            Arc::new(|_| {}),
            "SendMessage",
        )
        .await
        .required()?
        .to_value();
    assert_eq!(resp["error"]["code"], -32001);

    let err = h
        .on_message_send_streaming(&rpc(json!(11), "SendStreamingMessage", None), Arc::new(|_| {}))
        .await
        .err_or_fail()?;
    assert!(err.message().starts_with("Streaming error: "));
    let err = h
        .on_resubscribe_to_task(
            &rpc(json!(12), "SubscribeToTask", Some(json!({}))),
            Arc::new(|_| {}),
        )
        .await
        .err_or_fail()?;
    assert!(err.message().starts_with("Streaming error: "));
    let err = h
        .on_resubscribe_to_task(
            &rpc(json!(12), "SubscribeToTask", Some(json!({"id": "nope"}))),
            Arc::new(|_| {}),
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.status_code(), -32001);
    Ok(())
}

// ---------------------------------------------------------------------------
// ServerImpl with a mock transport
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockTransport {
    started: Mutex<u32>,
    stopped: Mutex<u32>,
    rpc: Mutex<Option<ServerTransportRpcHandler>>,
    card: Mutex<Option<ServerTransportCardHandler>>,
    extended: Mutex<Option<ServerTransportCardHandler>>,
}

#[async_trait]
impl ServerTransport for MockTransport {
    async fn start(&self) -> Result<(), A2aServerError> {
        *self.started.lock() += 1;
        Ok(())
    }
    async fn stop(&self) {
        *self.stopped.lock() += 1;
    }
    fn set_rpc_handler(&self, handler: ServerTransportRpcHandler) {
        *self.rpc.lock() = Some(handler);
    }
    fn set_card_handler(&self, handler: ServerTransportCardHandler) {
        *self.card.lock() = Some(handler);
    }
    fn set_extended_card_handler(&self, handler: ServerTransportCardHandler) {
        *self.extended.lock() = Some(handler);
    }
    fn local_addr(&self) -> Option<SocketAddr> {
        None
    }
}

/// Emitter collecting everything written.
#[derive(Default)]
struct RecordingEmitter {
    streaming: Mutex<Vec<String>>,
    non_streaming: Mutex<Vec<String>>,
    done: Mutex<u32>,
}

impl TransportEmitter for RecordingEmitter {
    fn write_streaming_data(&self, data: &str) {
        self.streaming.lock().push(data.to_string());
    }
    fn write_non_streaming_data(&self, data: &str) {
        self.non_streaming.lock().push(data.to_string());
    }
    fn write_done(&self) {
        *self.done.lock() += 1;
    }
}

fn server_with_mock(
    streaming: bool,
    executor: Option<Arc<dyn AgentExecutor>>,
) -> (ServerImpl, Arc<MockTransport>) {
    let transport = Arc::new(MockTransport::default());
    let card = make_agent_card("http://h:1/jsonrpc", streaming);
    let mut extended = card.clone();
    extended.name = "Extended".into();
    let server = ServerImpl::with_transport(
        Arc::new(card),
        Arc::new(extended),
        executor,
        transport.clone(),
        None,
    );
    (server, transport)
}

#[tokio::test]
async fn server_impl_lifecycle_and_cards() -> TestResult {
    let (server, transport) = server_with_mock(true, Some(Arc::new(HelloExecutor)));
    assert!(!server.is_started());
    let emitter = Arc::new(RecordingEmitter::default());
    assert!(
        server
            .handle_request("{}".into(), emitter.clone())
            .await
            .is_none(),
        "not started"
    );
    server.start().await?;
    server.start().await?;
    assert_eq!(*transport.started.lock(), 1);
    assert!(server.is_started());
    let card_json = (transport.card.lock().clone().required()?)();
    assert!(card_json.contains("\"name\":\"TestAgent\""));
    let ext_json = (transport.extended.lock().clone().required()?)();
    assert!(ext_json.contains("\"name\":\"Extended\""));
    assert_eq!(server.on_get_card().name, "TestAgent");
    assert_eq!(server.on_get_authenticated_extended_card().name, "Extended");
    assert!(server.local_addr().is_none());
    server.stop().await;
    assert_eq!(*transport.stopped.lock(), 1);
    assert!(!server.is_started());

    let no_transport = ServerImpl::new(
        Arc::new(AgentCard::default()),
        Arc::new(AgentCard::default()),
        None,
        HttpConfig::default(),
        None,
    );
    assert!(no_transport.start().await.is_err());
    Ok(())
}

#[tokio::test]
async fn server_impl_request_routing() -> TestResult {
    let (server, transport) = server_with_mock(true, Some(Arc::new(StreamingExecutor)));
    server.start().await?;
    let rpc_handler = transport.rpc.lock().clone().required()?;

    let emitter = Arc::new(RecordingEmitter::default());
    let resp: Value = serde_json::from_str(&rpc_handler("{}".into(), emitter.clone()).await.required()?)?;
    assert_eq!(resp["result"]["name"], "TestAgent");
    assert_eq!(resp["id"], 1);

    let resp: Value = serde_json::from_str(
        &server
            .handle_request("garbage".into(), emitter.clone())
            .await
            .required()?,
    )?;
    assert_eq!(resp["error"]["code"], -32603);
    assert!(resp["id"].is_null());

    let resp: Value = serde_json::from_str(
        &server
            .handle_request(rpc(json!(1), "Unknown", None).to_string(), emitter.clone())
            .await
            .required()?,
    )?;
    assert_eq!(resp["error"]["code"], -32601);
    assert_eq!(resp["error"]["message"], "Method not found: Unknown");

    let resp: Value = serde_json::from_str(
        &server
            .handle_request(rpc(json!(2), "GetAgentCard", None).to_string(), emitter.clone())
            .await
            .required()?,
    )?;
    assert_eq!(resp["result"]["name"], "TestAgent");

    let resp: Value = serde_json::from_str(
        &server
            .handle_request(
                rpc(json!(3), "GetTask", Some(json!({"id": "nope"}))).to_string(),
                emitter.clone(),
            )
            .await
            .required()?,
    )?;
    assert_eq!(resp["error"]["code"], -32001);
    let resp: Value = serde_json::from_str(
        &server
            .handle_request(
                rpc(json!(3), "CancelTask", Some(json!({"id": "nope"}))).to_string(),
                emitter.clone(),
            )
            .await
            .required()?,
    )?;
    assert_eq!(resp["error"]["code"], -32001);

    // Streaming: events go through the emitter, nothing is returned.
    let params = json!({"message": {"messageId": "m", "parts": [{"text": "hi"}]}});
    let out = server
        .handle_request(
            rpc(json!("s"), "SendStreamingMessage", Some(params)).to_string(),
            emitter.clone(),
        )
        .await;
    assert!(out.is_none());
    let streamed = emitter.streaming.lock().clone();
    assert_eq!(streamed.len(), 5);
    let first: Value = serde_json::from_str(&streamed[0])?;
    assert_eq!(first["id"], "s");
    assert!(first["result"]["task"].is_object());
    let last: Value = serde_json::from_str(&streamed[4])?;
    assert_eq!(
        last["result"]["statusUpdate"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    assert_eq!(*emitter.done.lock(), 1);

    let emitter = Arc::new(RecordingEmitter::default());
    server
        .handle_request(
            rpc(json!("r"), "SubscribeToTask", Some(json!({"id": "nope"}))).to_string(),
            emitter.clone(),
        )
        .await;
    let streamed = emitter.streaming.lock().clone();
    let err: Value = serde_json::from_str(&streamed[0])?;
    assert_eq!(err["error"]["code"], -32001);
    assert_eq!(*emitter.done.lock(), 1);

    let emitter = Arc::new(RecordingEmitter::default());
    server
        .handle_request(
            rpc(json!("r"), "SendStreamingMessage", None).to_string(),
            emitter.clone(),
        )
        .await;
    let err: Value = serde_json::from_str(&emitter.streaming.lock()[0])?;
    assert_eq!(err["error"]["code"], -32603);
    assert!(
        err["error"]["message"]
            .as_str()
            .required()?
            .starts_with("Streaming error: ")
    );

    // Non-streaming send goes through the emitter too.
    let emitter = Arc::new(RecordingEmitter::default());
    let (server, _t) = server_with_mock(false, Some(Arc::new(HelloExecutor)));
    server.start().await?;
    let params = json!({"message": {"messageId": "m", "parts": [{"text": "hi"}]}});
    let out = server
        .handle_request(
            rpc(json!("n"), "SendMessage", Some(params)).to_string(),
            emitter.clone(),
        )
        .await;
    assert!(out.is_none());
    let body: Value = serde_json::from_str(&emitter.non_streaming.lock()[0])?;
    assert_eq!(body["id"], "n");
    assert_eq!(body["result"]["message"]["parts"][0]["text"], "Processed: hi");
    let emitter = Arc::new(RecordingEmitter::default());
    let params = json!({"message": {"messageId": "m", "parts": [{"text": "hi"}]}});
    server
        .handle_request(
            rpc(json!("x"), "SendStreamingMessage", Some(params)).to_string(),
            emitter.clone(),
        )
        .await;
    let err: Value = serde_json::from_str(&emitter.streaming.lock()[0])?;
    assert_eq!(err["error"]["code"], -32004);
    Ok(())
}

// ---------------------------------------------------------------------------
// HttpServerBuilder
// ---------------------------------------------------------------------------

#[test]
fn builder_validation() -> TestResult {
    let cfg = HttpConfig::new("127.0.0.1", 0);
    let exec: Arc<dyn AgentExecutor> = Arc::new(HelloExecutor);
    let err = HttpServerBuilder::build(
        &cfg,
        &AgentCard::default(),
        &AgentCard::default(),
        exec.clone(),
        None,
    )
    .err()
    .required()?;
    assert_eq!(err.message(), "agentCard.supportedInterfaces is empty");
    let card = make_agent_card("", false);
    let err = HttpServerBuilder::build(&cfg, &card, &AgentCard::default(), exec.clone(), None)
        .err()
        .required()?;
    assert_eq!(err.message(), "agentCard.supportedInterfaces[0].url is empty");
    for bad in ["http://h:0/x", "http://h:65536/x"] {
        let card = make_agent_card(bad, false);
        let err = HttpServerBuilder::build(&cfg, &card, &AgentCard::default(), exec.clone(), None)
            .err()
            .required()?;
        assert_eq!(err.message(), "Invalid port in agentCard.url");
    }
    for ok in [
        "http://h:8080/rpc",
        "http://h:8080",
        "h:8080/rpc",
        "http://h/custom",
        "nothing",
    ] {
        let card = make_agent_card(ok, false);
        assert!(
            HttpServerBuilder::build(&cfg, &card, &AgentCard::default(), exec.clone(), None).is_ok(),
            "{ok}"
        );
    }
    let mut card = make_agent_card("http://h:8080/rpc", false);
    card.supported_interfaces[0].protocol_binding = "GRPC".into();
    let server = HttpServerBuilder::build_impl(&cfg, &card, &AgentCard::default(), None, None)?;
    assert_eq!(
        server.on_get_card().supported_interfaces[0].protocol_binding,
        "JSONRPC"
    );
    assert_eq!(
        card.supported_interfaces[0].protocol_binding, "GRPC",
        "original untouched"
    );
    let custom_store: Arc<dyn TaskStore> = Arc::new(InMemoryTaskStore::new());
    assert!(HttpServerBuilder::build(&cfg, &card, &AgentCard::default(), exec, Some(custom_store)).is_ok());
    assert_eq!(HttpConfig::default().endpoint, "/jsonrpc");
    assert_eq!(HttpConfig::default().io_thread_num, 1);
    Ok(())
}

#[tokio::test]
async fn builder_endpoint_from_card_url_is_served() -> TestResult {
    let cfg = HttpConfig::new("127.0.0.1", 0);
    let card = make_agent_card("http://127.0.0.1:1/custom/rpc", false);
    let server = HttpServerBuilder::build(&cfg, &card, &AgentCard::default(), Arc::new(HelloExecutor), None)?;
    server.start().await?;
    let addr = server.local_addr().required()?;
    let resp: Value = reqwest::Client::new()
        .post(format!("http://{addr}/custom/rpc"))
        .body("{}")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(resp["result"]["name"], "TestAgent");
    let extended = reqwest::get(format!("http://{addr}/agent/authenticatedExtendedCard")).await?;
    assert_eq!(extended.status(), 200);
    let missing = reqwest::Client::new()
        .post(format!("http://{addr}/jsonrpc"))
        .body("{}")
        .send()
        .await?;
    assert_eq!(missing.status(), 404);
    server.stop().await;
    Ok(())
}
