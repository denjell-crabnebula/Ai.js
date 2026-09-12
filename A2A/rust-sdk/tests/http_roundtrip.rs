// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests: Rust client against the Rust server over HTTP.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use parking_lot::Mutex;
use std::collections::BTreeMap;
use std::sync::Arc;

use a2a_sdk::client::{
    ClientCallInterceptor, ClientConfig, ClientEvent, ClientFactory, HttpCardResolverBuilder, UpdateEvent,
    send_message_stream,
};
use a2a_sdk::server::{AgentExecutor, HttpConfig, HttpServerBuilder, Server};
use a2a_sdk::types::*;
use a2a_sdk::{A2AErrorCode, A2aClientError};
use common::*;
use futures::StreamExt;

async fn start_server(
    executor: Arc<dyn AgentExecutor>,
    streaming: bool,
) -> TestResult<(Arc<dyn Server>, String, AgentCard)> {
    let card = make_agent_card("http://127.0.0.1:1/jsonrpc", streaming);
    let config = HttpConfig::new("127.0.0.1", 0);
    let server = HttpServerBuilder::build(&config, &card, &AgentCard::default(), executor, None)?;
    server.start().await?;
    let addr = server.local_addr().required()?;
    let base = format!("http://{addr}");
    let mut client_card = card.clone();
    client_card.supported_interfaces[0].url = format!("{base}/jsonrpc");
    Ok((server, base, client_card))
}

fn client_for(card: &AgentCard, streaming: bool) -> TestResult<Arc<dyn a2a_sdk::client::Client>> {
    let cfg = ClientConfig {
        streaming,
        supported_transports: vec!["JSONRPC".into()],
        ..Default::default()
    };
    Ok(ClientFactory::create(card, &cfg, Vec::new(), Vec::new()).required()?)
}

#[tokio::test]
async fn agent_card_is_served_and_resolved() -> TestResult {
    let (server, base, _card) = start_server(Arc::new(HelloExecutor), false).await?;
    let resolver =
        HttpCardResolverBuilder::build(&base, "/.well-known/agent-card.json", &BTreeMap::new()).required()?;
    let card = resolver.get_agent_card(None).await?;
    assert_eq!(card.name, "TestAgent");
    assert_eq!(card.supported_interfaces[0].protocol_binding, "JSONRPC");
    let all = resolver.get_all_agent_cards().await?;
    assert_eq!(all.len(), 1);

    let raw: serde_json::Value = reqwest::get(format!("{base}/.well-known/agent-card.json"))
        .await?
        .json()
        .await?;
    assert_eq!(raw["capabilities"]["streaming"], false);
    assert!(raw.get("provider").is_none());

    let missing = reqwest::get(format!("{base}/nope")).await?;
    assert_eq!(missing.status(), 404);
    assert_eq!(missing.text().await?, "Endpoint not found");

    let bad = HttpCardResolverBuilder::build(&base, "/missing.json", &BTreeMap::new()).required()?;
    let err = bad.get_agent_card(None).await.err_or_fail()?;
    assert!(matches!(err, A2aClientError::Http { status_code: 404, .. }));
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn non_streaming_send_message_returns_agent_message() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(HelloExecutor), false).await?;
    let client = client_for(&card, false)?;
    let events: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    client.add_event_consumer(Arc::new(move |ev, _card| sink.lock().push(ev.clone())));
    let got: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    client
        .send_message(
            &user_message("m-1", "hello"),
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await?;
    let got = got.lock().clone();
    assert_eq!(got.len(), 1);
    match &got[0] {
        ClientEvent::Message(m) => {
            assert_eq!(m.role, Role::Agent);
            assert_eq!(m.parts[0].text.as_deref(), Some("Processed: hello"));
            let task_id = m.task_id.clone().required()?;
            let task = client.get_task(&TaskQueryParams::new(task_id), None, 0).await?;
            assert_eq!(task.status.state, TaskState::Completed);
            assert_eq!(task.history.as_ref().required()?.len(), 1);
        }
        other => {
            return Err(ap_support::testing::TestFailure::new(format!("unexpected event {other:?}")).into());
        }
    }
    assert_eq!(events.lock().len(), 1);
    client.close();
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn polling_send_message_returns_task_immediately() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(HelloExecutor), false).await?;
    let cfg = ClientConfig {
        streaming: false,
        polling: true,
        ..Default::default()
    };
    let client = ClientFactory::create(&card, &cfg, Vec::new(), Vec::new()).required()?;
    let got: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    client
        .send_message(
            &user_message("m-2", "poll"),
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await?;
    let got = got.lock().clone();
    match &got[0] {
        ClientEvent::TaskUpdate(t, UpdateEvent::None) => {
            assert!(t.id.starts_with("task-"));
        }
        other => {
            return Err(ap_support::testing::TestFailure::new(format!("unexpected event {other:?}")).into());
        }
    }
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn streaming_send_message_delivers_events_in_order() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(StreamingExecutor), true).await?;
    let client = client_for(&card, true)?;
    let got: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    let ctx = ClientCallContext {
        state: "{\"sessionId\":\"s\"}".into(),
        headers: String::new(),
    };
    client
        .send_message(
            &user_message("m-3", "stream"),
            Some(&ctx),
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            5,
        )
        .await?;
    let got = got.lock().clone();
    assert_eq!(got.len(), 5, "{got:?}");
    assert!(
        matches!(&got[0], ClientEvent::TaskUpdate(t, UpdateEvent::None) if t.status.state == TaskState::Submitted)
    );
    assert!(
        matches!(&got[1], ClientEvent::TaskUpdate(_, UpdateEvent::Status(u)) if u.status.state == TaskState::Working)
    );
    assert!(
        matches!(&got[2], ClientEvent::TaskUpdate(_, UpdateEvent::Artifact(a)) if a.append == Some(false))
    );
    match &got[3] {
        ClientEvent::TaskUpdate(t, UpdateEvent::Artifact(a)) => {
            assert_eq!(a.append, Some(true));
            assert_eq!(a.last_chunk, Some(true));
            let arts = t.artifacts.as_ref().required()?;
            assert_eq!(arts.len(), 1);
            assert_eq!(arts[0].parts.len(), 2);
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    }
    match &got[4] {
        ClientEvent::TaskUpdate(t, UpdateEvent::Status(u)) => {
            assert_eq!(u.status.state, TaskState::Completed);
            assert_eq!(t.status.state, TaskState::Completed);
            assert_eq!(t.history.as_ref().required()?.len(), 3);
            let stored = client
                .get_task(&TaskQueryParams::new(t.id.clone()), None, 0)
                .await?;
            assert_eq!(stored.status.state, TaskState::Completed);
            assert_eq!(stored.artifacts.as_ref().required()?[0].parts.len(), 2);
            let limited = client
                .get_task(
                    &TaskQueryParams {
                        id: t.id.clone(),
                        history_length: Some(1),
                        metadata: None,
                    },
                    None,
                    0,
                )
                .await?;
            assert_eq!(limited.history.as_ref().required()?.len(), 1);
        }
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    }
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn streaming_as_stream_and_resubscribe() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(StreamingExecutor), true).await?;
    let client = client_for(&card, true)?;
    let mut stream = send_message_stream(client.clone(), user_message("m-4", "stream"), None, 0);
    let mut events = Vec::new();
    while let Some(ev) = stream.next().await {
        events.push(ev);
    }
    assert_eq!(events.len(), 5);
    let task_id = match &events[0] {
        ClientEvent::TaskUpdate(t, _) => t.id.clone(),
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    };
    let got: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    client
        .resubscribe(
            &TaskIdParams::new(task_id.clone()),
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await?;
    let got = got.lock().clone();
    assert_eq!(got.len(), 1);
    assert!(
        matches!(&got[0], ClientEvent::TaskUpdate(t, UpdateEvent::None) if t.status.state == TaskState::Completed)
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn task_lifecycle_errors_and_cancel() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(InputRequiredExecutor), true).await?;
    let client = client_for(&card, true)?;
    let got: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    client
        .send_message(
            &user_message("m-5", "start"),
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await?;
    let task_id = match &got.lock()[0] {
        ClientEvent::TaskUpdate(t, _) => t.id.clone(),
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    };
    let task = client
        .get_task(&TaskQueryParams::new(task_id.clone()), None, 0)
        .await?;
    assert_eq!(task.status.state, TaskState::InputRequired);

    let err = client
        .get_task(&TaskQueryParams::new("missing"), None, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::TaskNotFound.code());
    assert_eq!(err.message(), "Task id not found");

    let canceled = client
        .cancel_task(&TaskIdParams::new(task_id.clone()), None, 0)
        .await?;
    assert_eq!(canceled.status.state, TaskState::Canceled);
    let err = client
        .cancel_task(&TaskIdParams::new(task_id.clone()), None, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::TaskNotCancelable.code());

    let mut follow_up = user_message("m-6", "again");
    follow_up.task_id = Some(task_id.clone());
    let errors: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = errors.clone();
    let err = client
        .send_message(
            &follow_up,
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::UnsupportedOperation.code());
    assert!(matches!(&errors.lock()[0], ClientEvent::Error(e) if e.code == -32004));

    let mut wrong_ctx = user_message("m-7", "again");
    wrong_ctx.task_id = Some("does-not-exist".into());
    let err = client
        .send_message(&wrong_ctx, None, Box::new(|_, _| {}), 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::TaskNotFound.code());
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn push_notification_config_round_trip() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(HelloExecutor), false).await?;
    let client = client_for(&card, false)?;
    let got: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    client
        .send_message(
            &user_message("m-8", "hi"),
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await?;
    let task_id = match &got.lock()[0] {
        ClientEvent::Message(m) => m.task_id.clone().required()?,
        other => return Err(ap_support::testing::TestFailure::new(format!("unexpected {other:?}")).into()),
    };
    let cfg = TaskPushNotificationConfig {
        task_id: task_id.clone(),
        push_notification_config: PushNotificationConfig {
            url: "http://hook.example/notify".into(),
            token: Some("tok".into()),
            ..Default::default()
        },
    };
    let echoed = client.set_task_push_notification_config(&cfg, None, 0).await?;
    assert_eq!(echoed, cfg);
    let fetched = client
        .get_task_push_notification_config(
            &GetTaskPushNotificationConfigParams {
                id: task_id.clone(),
                metadata: None,
                push_notification_config_id: None,
            },
            None,
            0,
        )
        .await?;
    assert_eq!(fetched.push_notification_config.url, "http://hook.example/notify");
    assert_eq!(
        fetched.push_notification_config.id.as_deref(),
        Some(task_id.as_str())
    );
    let listed = client
        .list_task_push_notification_configs(
            &ListTaskPushNotificationConfigParams {
                id: task_id.clone(),
                metadata: None,
            },
            None,
            0,
        )
        .await?;
    assert_eq!(listed.len(), 1);
    client
        .delete_task_push_notification_config(
            &DeleteTaskPushNotificationConfigParams {
                id: task_id.clone(),
                metadata: None,
                push_notification_config_id: task_id.clone(),
            },
            None,
            0,
        )
        .await?;
    let listed = client
        .list_task_push_notification_configs(
            &ListTaskPushNotificationConfigParams {
                id: task_id.clone(),
                metadata: None,
            },
            None,
            0,
        )
        .await?;
    assert!(listed.is_empty());
    let err = client
        .get_task_push_notification_config(
            &GetTaskPushNotificationConfigParams {
                id: task_id.clone(),
                metadata: None,
                push_notification_config_id: None,
            },
            None,
            0,
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::JsonrpcInternalError.code());
    assert_eq!(err.message(), "PushNotificationConfig.url cannot be empty");
    let err = client
        .set_task_push_notification_config(
            &TaskPushNotificationConfig {
                task_id: "unknown".into(),
                push_notification_config: PushNotificationConfig {
                    url: "http://x".into(),
                    ..Default::default()
                },
            },
            None,
            0,
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::TaskNotFound.code());
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn raw_jsonrpc_wire_format() -> TestResult {
    let (server, base, _card) = start_server(Arc::new(HelloExecutor), false).await?;
    let http = reqwest::Client::new();
    let endpoint = format!("{base}/jsonrpc");

    let resp: serde_json::Value = http.post(&endpoint).body("{}").send().await?.json().await?;
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["name"], "TestAgent");

    let resp: serde_json::Value = http.post(&endpoint).body("not json").send().await?.json().await?;
    assert_eq!(resp["error"]["code"], -32603);
    assert!(
        resp["error"]["message"]
            .as_str()
            .required()?
            .starts_with("Internal error: ")
    );
    assert!(resp["id"].is_null());

    let resp: serde_json::Value = http
        .post(&endpoint)
        .body("{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"Nope\",\"params\":{}}")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(resp["error"]["code"], -32601);
    assert_eq!(resp["error"]["message"], "Method not found: Nope");
    assert_eq!(resp["id"], 7);

    let resp: serde_json::Value = http
        .post(&endpoint)
        .body("{\"jsonrpc\":\"2.0\",\"id\":\"r1\",\"method\":\"GetTask\",\"params\":{\"id\":\"zzz\"}}")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(resp["error"]["code"], -32001);
    assert_eq!(resp["id"], "r1");

    let resp: serde_json::Value = http
        .post(&endpoint)
        .body("{\"jsonrpc\":\"2.0\",\"id\":\"r2\",\"method\":\"GetTask\"}")
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(resp["error"]["code"], -32603);

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": "r3", "method": "SendMessage",
        "params": {"message": {"messageId": "x", "role": "ROLE_USER", "parts": [{"text": "hi"}]}}
    });
    let resp: serde_json::Value = http.post(&endpoint).json(&body).send().await?.json().await?;
    assert_eq!(resp["id"], "r3");
    assert_eq!(resp["result"]["message"]["role"], "ROLE_AGENT");
    assert_eq!(resp["result"]["message"]["parts"][0]["text"], "Processed: hi");

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": "r4", "method": "SendStreamingMessage",
        "params": {"message": {"messageId": "y", "role": "ROLE_USER", "parts": [{"text": "hi"}]}}
    });
    let resp = http.post(&endpoint).json(&body).send().await?;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()
            .get("content-type")
            .required()?
            .to_str()?
            .contains("text/event-stream")
    );
    let text = resp.text().await?;
    let events = ap_jsonrpc::sse::parse_all(&text);
    assert_eq!(events.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&events[0].data)?;
    assert_eq!(v["error"]["code"], -32004);
    assert_eq!(v["error"]["message"], "Streaming is not supported by the agent");
    assert_eq!(v["id"], "r4");
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn raw_streaming_wire_format() -> TestResult {
    let (server, base, _card) = start_server(Arc::new(StreamingExecutor), true).await?;
    let http = reqwest::Client::new();
    let endpoint = format!("{base}/jsonrpc");
    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": "s1", "method": "SendStreamingMessage",
        "params": {"message": {"messageId": "y", "role": "ROLE_USER", "parts": [{"text": "hi"}]}}
    });
    let text = http.post(&endpoint).json(&body).send().await?.text().await?;
    let events = ap_jsonrpc::sse::parse_all(&text);
    assert_eq!(events.len(), 5);
    let first: serde_json::Value = serde_json::from_str(&events[0].data)?;
    assert_eq!(first["jsonrpc"], "2.0");
    assert_eq!(first["id"], "s1");
    assert_eq!(first["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    let second: serde_json::Value = serde_json::from_str(&events[1].data)?;
    assert_eq!(
        second["result"]["statusUpdate"]["status"]["state"],
        "TASK_STATE_WORKING"
    );
    let third: serde_json::Value = serde_json::from_str(&events[2].data)?;
    assert_eq!(
        third["result"]["artifactUpdate"]["artifact"]["artifactId"],
        "art-1"
    );
    assert_eq!(third["result"]["artifactUpdate"]["append"], false);
    let last: serde_json::Value = serde_json::from_str(&events[4].data)?;
    assert_eq!(
        last["result"]["statusUpdate"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );

    let body = serde_json::json!({
        "jsonrpc": "2.0", "id": "s2", "method": "SubscribeToTask", "params": {"id": "missing"}
    });
    let text = http.post(&endpoint).json(&body).send().await?.text().await?;
    let events = ap_jsonrpc::sse::parse_all(&text);
    assert_eq!(events.len(), 1);
    let v: serde_json::Value = serde_json::from_str(&events[0].data)?;
    assert_eq!(v["error"]["code"], -32001);
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn executor_error_maps_to_jsonrpc_error() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(FailingExecutor), false).await?;
    let client = client_for(&card, false)?;
    let errors: Arc<Mutex<Vec<ClientEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = errors.clone();
    let err = client
        .send_message(
            &user_message("m-9", "boom"),
            None,
            Box::new(move |ev, _| sink.lock().push(ev.clone())),
            0,
        )
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), -32005);
    assert_eq!(err.message(), "agent exploded");
    assert_eq!(errors.lock().len(), 1);
    server.stop().await;
    Ok(())
}

struct HeaderCapture(Arc<Mutex<Vec<(String, String)>>>);

impl ClientCallInterceptor for HeaderCapture {
    fn intercept(
        &self,
        method_name: &str,
        payload: &mut String,
        headers: &mut BTreeMap<String, String>,
        _agent_card: Option<&AgentCard>,
        _context: Option<&ClientCallContext>,
    ) {
        headers.insert("X-Custom".into(), "yes".into());
        self.0.lock().push((method_name.to_string(), payload.clone()));
    }
}

#[tokio::test]
async fn interceptors_run_and_client_get_card_works() -> TestResult {
    let (server, _base, card) = start_server(Arc::new(HelloExecutor), false).await?;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let cfg = ClientConfig {
        streaming: false,
        ..Default::default()
    };
    let client = ClientFactory::create(
        &card,
        &cfg,
        Vec::new(),
        vec![Arc::new(HeaderCapture(seen.clone())) as Arc<dyn ClientCallInterceptor>],
    )
    .required()?;
    let err = client
        .get_task(&TaskQueryParams::new("nothing"), None, 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), -32001);
    let seen = seen.lock().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "GetTask");
    let payload: serde_json::Value = serde_json::from_str(&seen[0].1)?;
    assert_eq!(payload["method"], "GetTask");
    assert_eq!(payload["params"]["id"], "nothing");

    // GetCard through the JSON-RPC transport uses HTTP GET on the endpoint URL.
    let fetched = client.get_card(None, 0).await?;
    assert_eq!(fetched.name, "TestAgent");
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn connection_failure_and_closed_transport() -> TestResult {
    let card = make_agent_card("http://127.0.0.1:9/jsonrpc", false);
    let client = client_for(&card, false)?;
    let err = client
        .get_task(&TaskQueryParams::new("x"), None, 1)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aTransportException.code());
    client.close();
    let err = client
        .get_task(&TaskQueryParams::new("x"), None, 1)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aTransportException.code());
    let err = client
        .send_message(&user_message("", "x"), None, Box::new(|_, _| {}), 0)
        .await
        .err_or_fail()?;
    assert_eq!(err.code(), A2AErrorCode::A2aInvalidInput.code());
    Ok(())
}

#[tokio::test]
async fn server_start_twice_and_stop_are_idempotent() -> TestResult {
    let (server, _base, _card) = start_server(Arc::new(HelloExecutor), false).await?;
    server.start().await?;
    server.stop().await;
    server.stop().await;
    let card = make_agent_card("http://127.0.0.1:1/jsonrpc", false);
    assert!(
        HttpServerBuilder::build(
            &HttpConfig::new("127.0.0.1", 0),
            &AgentCard::default(),
            &card,
            Arc::new(HelloExecutor),
            None
        )
        .is_err()
    );
    Ok(())
}
