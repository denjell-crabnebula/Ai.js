// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Client core tests with a mock transport. Ports the intent of `tests/ut/client/*`.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use parking_lot::Mutex;
use std::sync::Arc;

use a2a_sdk::client::*;
use a2a_sdk::types::*;
use a2a_sdk::{A2AErrorCode, A2aClientError};
use common::*;
use futures::StreamExt;
use serde_json::json;

// ---------------------------------------------------------------------------
// ClientTaskManager
// ---------------------------------------------------------------------------

fn task(id: &str, state: TaskState) -> Task {
    Task {
        id: id.into(),
        context_id: "ctx".into(),
        status: TaskStatus::new(state),
        ..Default::default()
    }
}

fn status(id: &str, state: TaskState, msg: Option<Message>) -> StreamEvent {
    StreamEvent::StatusUpdate(TaskStatusUpdateEvent {
        context_id: "ctx".into(),
        metadata: None,
        status: TaskStatus {
            message: msg,
            state,
            timestamp: None,
        },
        task_id: id.into(),
    })
}

fn artifact(id: &str, artifact_id: &str, text: &str, append: Option<bool>) -> StreamEvent {
    StreamEvent::ArtifactUpdate(TaskArtifactUpdateEvent {
        artifact: Artifact {
            artifact_id: artifact_id.into(),
            parts: vec![Part::text(text)],
            ..Default::default()
        },
        context_id: "ctx".into(),
        task_id: id.into(),
        append,
        ..Default::default()
    })
}

#[test]
fn client_task_manager_task_events() -> TestResult {
    let mut mgr = ClientTaskManager::new();
    assert!(mgr.get_task_or_raise().is_err());
    assert!(mgr.current_task().is_none());
    mgr.save_task_event(&StreamEvent::Task(task("t", TaskState::Submitted)))?;
    assert_eq!(mgr.get_task_or_raise()?.id, "t");
    let err = mgr
        .save_task_event(&StreamEvent::Task(task("t", TaskState::Working)))
        .err_or_fail()?;
    assert!(err.contains("already set"));
    mgr.save_task_event(&StreamEvent::Message(user_message("m", "x")))?;
    Ok(())
}

#[test]
fn client_task_manager_status_events() -> TestResult {
    let mut mgr = ClientTaskManager::new();
    mgr.save_task_event(&status("t", TaskState::Working, Some(user_message("m1", "a"))))?;
    let t = mgr.get_task_or_raise()?;
    assert_eq!(t.id, "t");
    assert_eq!(t.context_id, "ctx");
    assert_eq!(t.status.state, TaskState::Working);
    assert_eq!(t.history.as_ref().required()?.len(), 1);

    let mut ev = TaskStatusUpdateEvent {
        context_id: "ctx".into(),
        metadata: Some(json!({"a": 1})),
        status: TaskStatus::new(TaskState::Completed),
        task_id: "t".into(),
    };
    mgr.save_task_event(&StreamEvent::StatusUpdate(ev.clone()))?;
    let t = mgr.get_task_or_raise()?;
    assert_eq!(t.status.state, TaskState::Completed);
    assert_eq!(t.metadata, Some(json!({"a": 1})));
    ev.metadata = Some(json!({"b": 2}));
    mgr.save_task_event(&StreamEvent::StatusUpdate(ev))?;
    assert_eq!(mgr.get_task_or_raise()?.metadata, Some(json!({"a": 1})), "kept");
    Ok(())
}

#[test]
fn client_task_manager_artifact_events() -> TestResult {
    let mut mgr = ClientTaskManager::new();
    mgr.save_task_event(&artifact("t", "a", "1", None))?;
    let t = mgr.get_task_or_raise()?;
    assert_eq!(t.status.state, TaskState::Unspecified);
    assert_eq!(t.artifacts.as_ref().required()?.len(), 1);
    mgr.save_task_event(&artifact("t", "a", "2", Some(false)))?;
    let t = mgr.get_task_or_raise()?;
    assert_eq!(
        t.artifacts.as_ref().required()?[0].parts[0].text.as_deref(),
        Some("2")
    );
    mgr.save_task_event(&artifact("t", "a", "3", Some(true)))?;
    assert_eq!(
        mgr.get_task_or_raise()?.artifacts.as_ref().required()?[0]
            .parts
            .len(),
        2
    );
    mgr.save_task_event(&artifact("t", "b", "x", Some(true)))?;
    assert_eq!(
        mgr.get_task_or_raise()?.artifacts.as_ref().required()?.len(),
        2,
        "new artifact appended"
    );
    Ok(())
}

#[test]
fn client_task_manager_update_with_message() -> TestResult {
    let mut mgr = ClientTaskManager::new();
    let mut t = task("t", TaskState::Working);
    t.status.message = Some(user_message("s", "status"));
    let updated = mgr.update_with_message(&user_message("u", "user"), t);
    let h = updated.history.as_ref().required()?;
    assert_eq!(h.len(), 2);
    assert_eq!(h[0].message_id, "s");
    assert_eq!(h[1].message_id, "u");
    assert!(updated.status.message.is_none());
    assert_eq!(mgr.get_task_or_raise()?.history.as_ref().required()?.len(), 2);
    let updated = mgr.update_with_message(&user_message("u2", "x"), updated);
    assert_eq!(updated.history.as_ref().required()?.len(), 3);
    Ok(())
}

// ---------------------------------------------------------------------------
// ClientFactory
// ---------------------------------------------------------------------------

#[test]
fn factory_transport_selection() -> TestResult {
    let card = make_agent_card("http://h/jsonrpc", true);
    let cfg = ClientConfig::default();
    assert_eq!(
        ClientFactory::select_transport(&card, &cfg),
        Some(("JSONRPC".to_string(), "http://h/jsonrpc".to_string()))
    );
    assert!(ClientFactory::create(&card, &cfg, Vec::new(), Vec::new()).is_some());

    let mut empty = card.clone();
    empty.supported_interfaces.clear();
    assert!(ClientFactory::create(&empty, &cfg, Vec::new(), Vec::new()).is_none());

    for field in ["url", "binding", "version"] {
        let mut bad = card.clone();
        match field {
            "url" => bad.supported_interfaces[0].url.clear(),
            "binding" => bad.supported_interfaces[0].protocol_binding.clear(),
            _ => bad.supported_interfaces[0].protocol_version.clear(),
        }
        assert!(
            ClientFactory::create(&bad, &cfg, Vec::new(), Vec::new()).is_none(),
            "{field}"
        );
    }

    let bad_cfg = ClientConfig {
        supported_transports: vec!["".into()],
        ..Default::default()
    };
    assert!(ClientFactory::create(&card, &bad_cfg, Vec::new(), Vec::new()).is_none());

    let grpc_cfg = ClientConfig {
        supported_transports: vec!["GRPC".into()],
        ..Default::default()
    };
    assert!(ClientFactory::create(&card, &grpc_cfg, Vec::new(), Vec::new()).is_none());

    let mut multi = card.clone();
    multi.supported_interfaces.insert(
        0,
        AgentInterface {
            url: "grpc://h".into(),
            protocol_binding: "GRPC".into(),
            protocol_version: "1.0".into(),
            tenant: None,
        },
    );
    let client_pref = ClientConfig {
        supported_transports: vec!["JSONRPC".into(), "GRPC".into()],
        use_client_preference: true,
        ..Default::default()
    };
    assert_eq!(
        ClientFactory::select_transport(&multi, &client_pref)
            .required()?
            .0,
        "JSONRPC"
    );
    let server_pref = ClientConfig {
        supported_transports: vec!["JSONRPC".into(), "GRPC".into()],
        use_client_preference: false,
        ..Default::default()
    };
    assert_eq!(
        ClientFactory::select_transport(&multi, &server_pref)
            .required()?
            .0,
        "GRPC",
        "server set is ordered by binding name"
    );
    assert!(
        ClientFactory::create(&multi, &server_pref, Vec::new(), Vec::new()).is_none(),
        "GRPC unsupported"
    );

    let mut dup = card.clone();
    dup.supported_interfaces
        .push(AgentInterface::jsonrpc("http://other"));
    assert_eq!(
        ClientFactory::select_transport(&dup, &cfg).required()?.1,
        "http://h/jsonrpc"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// DefaultClient with a mock transport
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockState {
    calls: Vec<(String, String)>,
    callback: Option<TransportEventCallback>,
    middleware: usize,
    closed: bool,
    fail_next: bool,
}

#[derive(Default)]
struct MockTransport(Mutex<MockState>);

impl MockTransport {
    fn record(&self, method: &str, request_id: &str) -> Result<(), A2aClientError> {
        let mut s = self.0.lock();
        if s.fail_next {
            s.fail_next = false;
            return Err(A2aClientError::from_code(
                A2AErrorCode::A2aTransportException,
                "transport exception",
            ));
        }
        s.calls.push((method.to_string(), request_id.to_string()));
        Ok(())
    }

    fn last_request_id(&self) -> TestResult<String> {
        Ok(self.0.lock().calls.last().required()?.1.clone())
    }

    fn emit(&self, request_id: &str, ev: TransportEvent) -> TestResult {
        let cb = self.0.lock().callback.clone().required()?;
        cb(request_id, ev);
        Ok(())
    }
}

impl ClientTransport for MockTransport {
    fn send_message(
        &self,
        id: &str,
        _r: &MessageSendParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("SendMessage", id)
    }
    fn send_message_streaming(
        &self,
        id: &str,
        _r: &MessageSendParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("SendStreamingMessage", id)
    }
    fn get_task(
        &self,
        id: &str,
        _p: &TaskQueryParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("GetTask", id)
    }
    fn cancel_task(
        &self,
        id: &str,
        _p: &TaskIdParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("CancelTask", id)
    }
    fn set_task_push_notification_config(
        &self,
        id: &str,
        _p: &TaskPushNotificationConfig,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("CreateTaskPushNotificationConfig", id)
    }
    fn get_task_push_notification_config(
        &self,
        id: &str,
        _p: &GetTaskPushNotificationConfigParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("GetTaskPushNotificationConfig", id)
    }
    fn list_task_push_notification_configs(
        &self,
        id: &str,
        _p: &ListTaskPushNotificationConfigParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("ListTaskPushNotificationConfigs", id)
    }
    fn delete_task_push_notification_config(
        &self,
        id: &str,
        _p: &DeleteTaskPushNotificationConfigParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("DeleteTaskPushNotificationConfig", id)
    }
    fn resubscribe(
        &self,
        id: &str,
        _p: &TaskIdParams,
        _c: Option<&ClientCallContext>,
        _t: u64,
    ) -> Result<(), A2aClientError> {
        self.record("SubscribeToTask", id)
    }
    fn get_card(&self, id: &str, _c: Option<&ClientCallContext>, _t: u64) -> Result<(), A2aClientError> {
        self.record("GetAgentCard", id)
    }
    fn set_transport_callback(&self, callback: TransportEventCallback) {
        self.0.lock().callback = Some(callback);
    }
    fn close(&self) {
        self.0.lock().closed = true;
    }
    fn add_request_middleware(&self, _m: Arc<dyn ClientCallInterceptor>) {
        self.0.lock().middleware += 1;
    }
}

fn mock_client(streaming: bool) -> (DefaultClient, Arc<MockTransport>) {
    let transport = Arc::new(MockTransport::default());
    let card = make_agent_card("http://h/jsonrpc", streaming);
    let cfg = ClientConfig {
        streaming,
        ..Default::default()
    };
    let client = DefaultClient::new(card, cfg, transport.clone(), Vec::new());
    assert!(transport.0.lock().callback.is_some());
    (client, transport)
}

type Events = Arc<Mutex<Vec<ClientEvent>>>;

fn collector() -> (Events, ResponseHandler) {
    let events: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    (events, Box::new(move |ev, _| sink.lock().push(ev.clone())))
}

#[tokio::test]
async fn send_message_validation_and_dispatch() -> TestResult {
    let (client, transport) = mock_client(false);
    let (_, h) = collector();
    let err = client
        .send_message(&user_message("", "x"), None, h, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aInvalidInput.code());
    let (_, h) = collector();
    let mut no_parts = user_message("m", "x");
    no_parts.parts.clear();
    let err = client.send_message(&no_parts, None, h, 0).await.err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aInvalidInput.code());

    transport.0.lock().fail_next = true;
    let (_, h) = collector();
    let err = client
        .send_message(&user_message("m", "x"), None, h, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aTransportException.code());
    assert_eq!(client.pending_requests(), 0);

    let (events, h) = collector();
    let consumed: Events = Arc::new(Mutex::new(Vec::new()));
    let sink = consumed.clone();
    client.add_event_consumer(Arc::new(move |ev, card| {
        assert_eq!(card.name, "TestAgent");
        sink.lock().push(ev.clone());
    }));
    let t = transport.clone();
    let c = client.clone();
    let join = tokio::spawn(async move { c.send_message(&user_message("m", "x"), None, h, 0).await });
    tokio::task::yield_now().await;
    while t.0.lock().calls.is_empty() {
        tokio::task::yield_now().await;
    }
    assert_eq!(t.0.lock().calls[0].0, "SendMessage");
    let id = t.last_request_id()?;
    t.emit(&id, TransportEvent::Message(user_message("reply", "y")))?;
    join.await??;
    assert!(matches!(&events.lock()[0], ClientEvent::Message(m) if m.message_id == "reply"));
    assert_eq!(consumed.lock().len(), 1);
    assert_eq!(client.pending_requests(), 0);
    t.emit(&id, TransportEvent::Message(user_message("late", "y")))?;
    assert_eq!(events.lock().len(), 1, "unknown request id ignored");
    Ok(())
}

async fn wait_for_call(transport: &MockTransport, n: usize) -> TestResult<String> {
    loop {
        if transport.0.lock().calls.len() >= n {
            return transport.last_request_id();
        }
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn streaming_send_message_accumulates_task() -> TestResult {
    let (client, transport) = mock_client(true);
    assert!(client.can_stream());
    let (events, h) = collector();
    let c = client.clone();
    let join = tokio::spawn(async move { c.send_message(&user_message("m", "x"), None, h, 0).await });
    let id = wait_for_call(&transport, 1).await?;
    assert_eq!(transport.0.lock().calls[0].0, "SendStreamingMessage");
    transport.emit(&id, TransportEvent::Task(task("t", TaskState::Submitted)))?;
    transport.emit(
        &id,
        TransportEvent::StatusUpdate(TaskStatusUpdateEvent {
            context_id: "ctx".into(),
            metadata: None,
            status: TaskStatus::new(TaskState::Working),
            task_id: "t".into(),
        }),
    )?;
    let StreamEvent::ArtifactUpdate(a) = artifact("t", "a", "1", None) else {
        unreachable!()
    };
    transport.emit(&id, TransportEvent::ArtifactUpdate(a))?;
    assert_eq!(client.pending_requests(), 1);
    transport.emit(
        &id,
        TransportEvent::StatusUpdate(TaskStatusUpdateEvent {
            context_id: "ctx".into(),
            metadata: None,
            status: TaskStatus::new(TaskState::Completed),
            task_id: "t".into(),
        }),
    )?;
    join.await??;
    assert_eq!(client.pending_requests(), 0);
    let events = events.lock();
    assert_eq!(events.len(), 4);
    assert!(matches!(&events[0], ClientEvent::TaskUpdate(t, UpdateEvent::None) if t.id == "t"));
    assert!(
        matches!(&events[1], ClientEvent::TaskUpdate(t, UpdateEvent::Status(_)) if t.status.state == TaskState::Working)
    );
    assert!(
        matches!(&events[2], ClientEvent::TaskUpdate(t, UpdateEvent::Artifact(_)) if t.artifacts.as_ref().required()?.len() == 1)
    );
    assert!(
        matches!(&events[3], ClientEvent::TaskUpdate(t, UpdateEvent::Status(u)) if t.status.state == TaskState::Completed && u.status.state == TaskState::Completed)
    );
    Ok(())
}

#[tokio::test]
async fn streaming_errors_and_stream_end() -> TestResult {
    let (client, transport) = mock_client(true);
    let (events, h) = collector();
    let c = client.clone();
    let join = tokio::spawn(async move { c.send_message(&user_message("m", "x"), None, h, 0).await });
    let id = wait_for_call(&transport, 1).await?;
    transport.emit(
        &id,
        TransportEvent::Error(TransportError::new(-32001, "Task not found")),
    )?;
    let err = join.await?.err_or_fail()?;
    assert_eq!(err.code(), -32001);
    assert!(matches!(&events.lock()[0], ClientEvent::Error(e) if e.code == -32001));
    assert_eq!(client.pending_requests(), 0);

    let (events, h) = collector();
    let c = client.clone();
    let join = tokio::spawn(async move { c.send_message(&user_message("m", "x"), None, h, 0).await });
    let id = wait_for_call(&transport, 2).await?;
    transport.emit(&id, TransportEvent::Task(task("t", TaskState::Working)))?;
    transport.emit(&id, TransportEvent::Error(TransportError::stream_end()))?;
    join.await??;
    assert_eq!(
        events.lock().len(),
        1,
        "clean end of stream is not an error event"
    );

    // Artifact before any task creates a shell; status update without task on a fresh stream also works.
    let (events, h) = collector();
    let c = client.clone();
    let join = tokio::spawn(async move { c.send_message(&user_message("m", "x"), None, h, 0).await });
    let id = wait_for_call(&transport, 3).await?;
    transport.emit(
        &id,
        TransportEvent::StatusUpdate(TaskStatusUpdateEvent {
            context_id: "ctx".into(),
            metadata: None,
            status: TaskStatus::new(TaskState::InputRequired),
            task_id: "t".into(),
        }),
    )?;
    join.await??;
    assert!(
        matches!(&events.lock()[0], ClientEvent::TaskUpdate(t, _) if t.status.state == TaskState::InputRequired)
    );
    Ok(())
}

#[tokio::test]
async fn simple_requests_complete_from_transport_events() -> TestResult {
    let (client, transport) = mock_client(false);

    let err = client
        .get_task(&TaskQueryParams::new(""), None, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aInvalidInput.code());
    let c = client.clone();
    let join = tokio::spawn(async move { c.get_task(&TaskQueryParams::new("t"), None, 0).await });
    let id = wait_for_call(&transport, 1).await?;
    transport.emit(&id, TransportEvent::Task(task("t", TaskState::Working)))?;
    assert_eq!(join.await??.id, "t");

    let err = client
        .cancel_task(&TaskIdParams::new(""), None, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aInvalidInput.code());
    let c = client.clone();
    let join = tokio::spawn(async move { c.cancel_task(&TaskIdParams::new("t"), None, 0).await });
    let id = wait_for_call(&transport, 2).await?;
    transport.emit(&id, TransportEvent::Error(TransportError::new(-32002, "no")))?;
    assert_eq!(join.await?.err_or_fail()?.code(), -32002);

    let cfg = TaskPushNotificationConfig {
        task_id: "t".into(),
        push_notification_config: PushNotificationConfig {
            url: "u".into(),
            ..Default::default()
        },
    };
    let mut empty = cfg.clone();
    empty.task_id.clear();
    assert!(
        client
            .set_task_push_notification_config(&empty, None, 0)
            .await
            .is_err()
    );
    let c = client.clone();
    let cfg2 = cfg.clone();
    let join = tokio::spawn(async move { c.set_task_push_notification_config(&cfg2, None, 0).await });
    let id = wait_for_call(&transport, 3).await?;
    transport.emit(&id, TransportEvent::PushNotificationConfig(cfg.clone()))?;
    assert_eq!(join.await??, cfg);

    let get_params = GetTaskPushNotificationConfigParams {
        id: "t".into(),
        metadata: None,
        push_notification_config_id: None,
    };
    let c = client.clone();
    let join = tokio::spawn(async move { c.get_task_push_notification_config(&get_params, None, 0).await });
    let id = wait_for_call(&transport, 4).await?;
    transport.emit(&id, TransportEvent::PushNotificationConfig(cfg.clone()))?;
    assert_eq!(join.await??, cfg);

    let list_params = ListTaskPushNotificationConfigParams {
        id: "t".into(),
        metadata: None,
    };
    let c = client.clone();
    let join =
        tokio::spawn(async move { c.list_task_push_notification_configs(&list_params, None, 0).await });
    let id = wait_for_call(&transport, 5).await?;
    transport.emit(
        &id,
        TransportEvent::PushNotificationConfigs(vec![cfg.clone(), cfg.clone()]),
    )?;
    assert_eq!(join.await??.len(), 2);

    let del_params = DeleteTaskPushNotificationConfigParams {
        id: "t".into(),
        metadata: None,
        push_notification_config_id: "c".into(),
    };
    let c = client.clone();
    let join =
        tokio::spawn(async move { c.delete_task_push_notification_config(&del_params, None, 0).await });
    let id = wait_for_call(&transport, 6).await?;
    transport.emit(&id, TransportEvent::None)?;
    join.await??;

    let c = client.clone();
    let join = tokio::spawn(async move { c.get_card(None, 0).await });
    let id = wait_for_call(&transport, 7).await?;
    transport.emit(&id, TransportEvent::AgentCard(make_agent_card("http://x", true)))?;
    assert_eq!(join.await??.name, "TestAgent");

    // Wrong result type abandons the request.
    let c = client.clone();
    let join = tokio::spawn(async move { c.get_task(&TaskQueryParams::new("t"), None, 0).await });
    let id = wait_for_call(&transport, 8).await?;
    transport.emit(&id, TransportEvent::None)?;
    assert_eq!(
        join.await?.err_or_fail()?.code(),
        A2AErrorCode::A2aStatusError.code()
    );
    assert_eq!(client.pending_requests(), 0);

    client.add_request_middleware(Arc::new(ProtocolVersionInterceptor::new()));
    assert_eq!(transport.0.lock().middleware, 1);
    client.close();
    assert!(transport.0.lock().closed);
    Ok(())
}

#[tokio::test]
async fn resubscribe_requires_streaming() -> TestResult {
    let (client, _transport) = mock_client(false);
    let (_, h) = collector();
    let err = client
        .resubscribe(&TaskIdParams::new("t"), None, h, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::UnsupportedOperation.code());

    let (client, transport) = mock_client(true);
    let (events, h) = collector();
    let c = client.clone();
    let join = tokio::spawn(async move { c.resubscribe(&TaskIdParams::new("t"), None, h, 0).await });
    let id = wait_for_call(&transport, 1).await?;
    assert_eq!(transport.0.lock().calls[0].0, "SubscribeToTask");
    transport.emit(&id, TransportEvent::Task(task("t", TaskState::Completed)))?;
    transport.emit(&id, TransportEvent::Error(TransportError::stream_end()))?;
    join.await??;
    assert_eq!(events.lock().len(), 1);
    Ok(())
}

#[tokio::test]
async fn event_stream_convenience() -> TestResult {
    let (client, transport) = mock_client(false);
    let client: Arc<dyn Client> = Arc::new(client);
    let mut stream = send_message_stream(client.clone(), user_message("m", "x"), None, 0);
    let id = wait_for_call(&transport, 1).await?;
    transport.emit(&id, TransportEvent::Message(user_message("reply", "y")))?;
    let first = stream.next().await.required()?;
    assert!(matches!(first, ClientEvent::Message(m) if m.message_id == "reply"));
    assert!(stream.next().await.is_none());

    let mut stream = send_message_stream(client, user_message("", "x"), None, 0);
    let ev = stream.next().await.required()?;
    assert!(matches!(ev, ClientEvent::Error(e) if e.code == A2AErrorCode::A2aInvalidInput.code()));
    assert!(stream.next().await.is_none());
    Ok(())
}

#[tokio::test]
async fn send_message_uses_config_values() -> TestResult {
    #[derive(Default)]
    struct Capture(Mutex<Vec<MessageSendParams>>);
    impl ClientTransport for Capture {
        fn send_message(
            &self,
            _id: &str,
            r: &MessageSendParams,
            _c: Option<&ClientCallContext>,
            _t: u64,
        ) -> Result<(), A2aClientError> {
            self.0.lock().push(r.clone());
            Ok(())
        }
        fn send_message_streaming(
            &self,
            _id: &str,
            r: &MessageSendParams,
            _c: Option<&ClientCallContext>,
            _t: u64,
        ) -> Result<(), A2aClientError> {
            self.0.lock().push(r.clone());
            Ok(())
        }
        fn get_task(
            &self,
            _: &str,
            _: &TaskQueryParams,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn cancel_task(
            &self,
            _: &str,
            _: &TaskIdParams,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn set_task_push_notification_config(
            &self,
            _: &str,
            _: &TaskPushNotificationConfig,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn get_task_push_notification_config(
            &self,
            _: &str,
            _: &GetTaskPushNotificationConfigParams,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn list_task_push_notification_configs(
            &self,
            _: &str,
            _: &ListTaskPushNotificationConfigParams,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn delete_task_push_notification_config(
            &self,
            _: &str,
            _: &DeleteTaskPushNotificationConfigParams,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn resubscribe(
            &self,
            _: &str,
            _: &TaskIdParams,
            _: Option<&ClientCallContext>,
            _: u64,
        ) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn get_card(&self, _: &str, _: Option<&ClientCallContext>, _: u64) -> Result<(), A2aClientError> {
            Ok(())
        }
        fn set_transport_callback(&self, _: TransportEventCallback) {}
        fn close(&self) {}
        fn add_request_middleware(&self, _: Arc<dyn ClientCallInterceptor>) {}
    }
    let transport = Arc::new(Capture::default());
    let cfg = ClientConfig {
        streaming: false,
        polling: true,
        accepted_output_modes: Some(vec!["text".into()]),
        push_notification_configs: vec![
            PushNotificationConfig {
                url: "first".into(),
                ..Default::default()
            },
            PushNotificationConfig {
                url: "second".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    let client = ClientFactory::create_with_transport(
        &make_agent_card("http://h", false),
        &cfg,
        transport.clone(),
        Vec::new(),
    )
    .required()?;
    let c = client.clone();
    let join = tokio::spawn(async move {
        tokio::time::timeout(
            std::time::Duration::from_millis(50),
            c.send_message(&user_message("m", "x"), None, Box::new(|_, _| {}), 0),
        )
        .await
    });
    let _ = join.await?;
    let sent = transport.0.lock();
    let cfg = sent[0].configuration.as_ref().required()?;
    assert_eq!(cfg.accepted_output_modes, Some(vec!["text".to_string()]));
    assert_eq!(cfg.return_immediately, Some(true));
    assert_eq!(cfg.push_notification_config.as_ref().required()?.url, "first");
    Ok(())
}
