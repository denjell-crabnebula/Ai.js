// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Wire format tests, the port of `tests/ut/shared/test_jsonrpc.cpp` and `test_types.cpp`.

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::collections::BTreeMap;

use a2a_sdk::A2AErrorCode;
use a2a_sdk::types::*;
use serde_json::{Value, json};

fn to_json<T: serde::Serialize>(v: &T) -> TestResult<Value> {
    Ok(serde_json::to_value(v)?)
}

fn from_json<T: serde::de::DeserializeOwned>(v: Value) -> TestResult<T> {
    Ok(serde_json::from_value(v)?)
}

fn try_from_json<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| e.to_string())
}

fn text_message(id: &str, role: Role) -> Message {
    Message {
        message_id: id.into(),
        role,
        parts: vec![Part::text("hello")],
        ..Default::default()
    }
}

#[test]
fn role_serialization() -> TestResult {
    assert_eq!(to_json(&Role::Agent)?, json!("ROLE_AGENT"));
    assert_eq!(to_json(&Role::User)?, json!("ROLE_USER"));
    assert_eq!(to_json(&Role::Unspecified)?, json!("ROLE_UNSPECIFIED"));
    assert_eq!(from_json::<Role>(json!("ROLE_USER"))?, Role::User);
    assert!(try_from_json::<Role>(json!("ROLE_INVALID")).is_err());
    Ok(())
}

#[test]
fn part_text_raw_url_data() -> TestResult {
    let p = Part::text("hi").with_media_type("text/plain");
    let j = to_json(&p)?;
    assert_eq!(j, json!({"text": "hi", "mediaType": "text/plain"}));
    assert_eq!(from_json::<Part>(j)?, p);

    let raw = Part {
        raw: Some("YWJj".into()),
        filename: Some("a.bin".into()),
        ..Default::default()
    };
    let j = to_json(&raw)?;
    assert_eq!(j["raw"], "YWJj");
    assert_eq!(j["filename"], "a.bin");
    assert_eq!(from_json::<Part>(j)?, raw);

    let url = Part {
        url: Some("http://x".into()),
        ..Default::default()
    };
    assert_eq!(to_json(&url)?, json!({"url": "http://x"}));

    let data = Part::data(json!({"key": "value"}));
    let j = to_json(&data)?;
    assert_eq!(j["data"], json!({"key": "value"}));
    assert_eq!(from_json::<Part>(j)?, data);
    for v in [json!("s"), json!(1), json!(1.5), json!(true)] {
        let p: Part = from_json(json!({"data": v}))?;
        assert_eq!(p.data, Some(v));
    }
    assert!(try_from_json::<Part>(json!({"data": [1]})).is_err());
    Ok(())
}

#[test]
fn part_mutual_fields() -> TestResult {
    let e = try_from_json::<Part>(json!({"mediaType": "text/plain"})).err_or_fail()?;
    assert!(e.contains("Part must contain one of: text, raw, url, data"));
    let p = Part {
        text: Some("text".into()),
        url: Some("url".into()),
        ..Default::default()
    };
    let j = to_json(&p)?;
    assert!(j.get("text").is_some());
    assert!(j.get("url").is_none());
    // First non-null mutual field in document order wins.
    let p: Part = serde_json::from_str("{\"url\":\"u\",\"text\":\"t\"}")?;
    assert_eq!(p.url.as_deref(), Some("u"));
    assert!(p.text.is_none());
    let p: Part = serde_json::from_str("{\"url\":null,\"text\":\"t\"}")?;
    assert_eq!(p.text.as_deref(), Some("t"));
    let p: Part = from_json(json!({"text": "t", "metadata": {"k": 1}, "filename": null}))?;
    assert_eq!(p.metadata, Some(json!({"k": 1})));
    assert!(p.filename.is_none());
    Ok(())
}

#[test]
fn artifact_serialization() -> TestResult {
    let a = Artifact {
        artifact_id: "art-001".into(),
        name: Some("n".into()),
        description: Some("d".into()),
        extensions: Some(vec!["e".into()]),
        metadata: Some(json!({"m": true})),
        parts: vec![Part::text("x")],
    };
    let j = to_json(&a)?;
    assert_eq!(j["artifactId"], "art-001");
    assert_eq!(j["parts"][0]["text"], "x");
    assert_eq!(j["extensions"], json!(["e"]));
    assert_eq!(from_json::<Artifact>(j)?, a);

    let empty = Artifact {
        artifact_id: "art-001".into(),
        ..Default::default()
    };
    let j = to_json(&empty)?;
    assert_eq!(j, json!({"artifactId": "art-001", "parts": []}));
    assert_eq!(from_json::<Artifact>(j)?.parts.len(), 0);

    assert!(serde_json::to_value(Artifact::default()).is_err());
    assert!(try_from_json::<Artifact>(json!({"artifactId": "x"})).is_err());
    assert!(try_from_json::<Artifact>(json!({"artifactId": "", "parts": []})).is_err());
    Ok(())
}

#[test]
fn message_serialization() -> TestResult {
    let m = Message {
        context_id: Some("ctx".into()),
        extensions: Some(vec!["ext".into()]),
        message_id: "m1".into(),
        metadata: Some(json!({"a": 1})),
        parts: vec![Part::text("hi")],
        reference_task_ids: Some(vec!["t1".into()]),
        role: Role::Agent,
        task_id: Some("t1".into()),
    };
    let j = to_json(&m)?;
    assert_eq!(j["messageId"], "m1");
    assert_eq!(j["role"], "ROLE_AGENT");
    assert_eq!(j["contextId"], "ctx");
    assert_eq!(j["referenceTaskIds"], json!(["t1"]));
    assert_eq!(j["taskId"], "t1");
    assert_eq!(from_json::<Message>(j)?, m);

    assert!(try_from_json::<Message>(json!({"messageId": "x"})).is_err());
    assert!(try_from_json::<Message>(json!({"messageId": "", "parts": []})).is_err());
    let m: Message = from_json(json!({"messageId": "x", "parts": []}))?;
    assert_eq!(m.role, Role::User);
    assert!(serde_json::to_value(Message::default()).is_err());
    let m: Message = from_json(json!({"messageId": "x", "parts": [], "metadata": "text"}))?;
    assert_eq!(m.metadata, Some(json!("text")));
    Ok(())
}

#[test]
fn task_status_states() -> TestResult {
    let pairs = [
        (TaskState::Submitted, "TASK_STATE_SUBMITTED"),
        (TaskState::Working, "TASK_STATE_WORKING"),
        (TaskState::InputRequired, "TASK_STATE_INPUT_REQUIRED"),
        (TaskState::Completed, "TASK_STATE_COMPLETED"),
        (TaskState::Canceled, "TASK_STATE_CANCELED"),
        (TaskState::Failed, "TASK_STATE_FAILED"),
        (TaskState::Rejected, "TASK_STATE_REJECTED"),
        (TaskState::AuthRequired, "TASK_STATE_AUTH_REQUIRED"),
        (TaskState::Unspecified, "TASK_STATE_UNSPECIFIED"),
    ];
    for (state, wire) in pairs {
        let s = TaskStatus::new(state);
        let j = to_json(&s)?;
        assert_eq!(j["state"], wire);
        assert_eq!(from_json::<TaskStatus>(j)?.state, state);
    }
    let s: TaskStatus = from_json(json!({"state": "SOMETHING_ELSE"}))?;
    assert_eq!(s.state, TaskState::Unspecified);
    let s: TaskStatus = from_json(json!({}))?;
    assert_eq!(s.state, TaskState::Unspecified);
    assert_eq!(TaskStatus::default().state, TaskState::Unspecified);

    let full = TaskStatus {
        message: Some(text_message("m", Role::Agent)),
        state: TaskState::Working,
        timestamp: Some("2026-01-01T00:00:00.000Z".into()),
    };
    let j = to_json(&full)?;
    assert_eq!(j["message"]["messageId"], "m");
    assert_eq!(j["timestamp"], "2026-01-01T00:00:00.000Z");
    assert_eq!(from_json::<TaskStatus>(j)?, full);
    Ok(())
}

#[test]
fn task_serialization() -> TestResult {
    let t = Task {
        artifacts: Some(vec![Artifact {
            artifact_id: "a".into(),
            parts: vec![Part::text("x")],
            ..Default::default()
        }]),
        context_id: "ctx".into(),
        history: Some(vec![text_message("m", Role::User)]),
        id: "t1".into(),
        metadata: Some(json!({"k": "v"})),
        status: TaskStatus::new(TaskState::Completed),
        creat_at: None,
        last_modified: None,
    };
    let j = to_json(&t)?;
    assert_eq!(j["id"], "t1");
    assert_eq!(j["contextId"], "ctx");
    assert_eq!(j["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(j["artifacts"][0]["artifactId"], "a");
    assert_eq!(j["history"][0]["messageId"], "m");
    assert_eq!(j["metadata"]["k"], "v");
    assert_eq!(from_json::<Task>(j)?, t);

    assert!(serde_json::to_value(Task::default()).is_err());
    assert!(try_from_json::<Task>(json!({"id": "t", "contextId": "c"})).is_err());
    assert!(try_from_json::<Task>(json!({"id": "", "contextId": "c", "status": {}})).is_err());
    let minimal: Task =
        from_json(json!({"id": "t", "contextId": "c", "status": {"state": "TASK_STATE_WORKING"}}))?;
    assert!(minimal.history.is_none());
    assert!(minimal.artifacts.is_none());
    Ok(())
}

#[test]
fn push_notification_types() -> TestResult {
    let auth = PushNotificationAuthenticationInfo {
        credentials: Some("secret".into()),
        schemes: vec!["Bearer".into()],
    };
    let j = to_json(&auth)?;
    assert_eq!(j, json!({"schemes": ["Bearer"], "credentials": "secret"}));
    assert_eq!(from_json::<PushNotificationAuthenticationInfo>(j)?, auth);
    assert!(try_from_json::<PushNotificationAuthenticationInfo>(json!({})).is_err());
    assert!(try_from_json::<PushNotificationAuthenticationInfo>(json!({"schemes": [1]})).is_err());

    let cfg = PushNotificationConfig {
        authentication: Some(auth),
        config_id: None,
        id: Some("cfg-1".into()),
        token: Some("tok".into()),
        url: "https://hook".into(),
        created_at: None,
    };
    let j = to_json(&cfg)?;
    assert_eq!(j["url"], "https://hook");
    assert_eq!(j["id"], "cfg-1");
    assert_eq!(j["token"], "tok");
    assert_eq!(from_json::<PushNotificationConfig>(j)?, cfg);
    assert!(try_from_json::<PushNotificationConfig>(json!({})).is_err());
    assert!(try_from_json::<PushNotificationConfig>(json!({"url": ""})).is_err());
    assert!(serde_json::to_value(PushNotificationConfig::default()).is_err());
    let with_config_id: PushNotificationConfig = from_json(json!({"url": "u", "id": "a", "configId": "b"}))?;
    assert_eq!(with_config_id.id.as_deref(), Some("b"));

    let tc = TaskPushNotificationConfig {
        task_id: "t".into(),
        push_notification_config: PushNotificationConfig {
            url: "u".into(),
            ..Default::default()
        },
    };
    let j = to_json(&tc)?;
    assert_eq!(j, json!({"pushNotificationConfig": {"url": "u"}, "taskId": "t"}));
    assert_eq!(from_json::<TaskPushNotificationConfig>(j)?, tc);
    assert!(try_from_json::<TaskPushNotificationConfig>(json!({"taskId": "t"})).is_err());
    assert!(serde_json::to_value(TaskPushNotificationConfig::default()).is_err());
    Ok(())
}

#[test]
fn message_send_configuration_and_params() -> TestResult {
    let cfg = MessageSendConfiguration {
        accepted_output_modes: Some(vec!["text".into()]),
        history_length: Some(5),
        push_notification_config: Some(PushNotificationConfig {
            url: "u".into(),
            ..Default::default()
        }),
        return_immediately: Some(true),
    };
    let j = to_json(&cfg)?;
    assert_eq!(j["acceptedOutputModes"], json!(["text"]));
    assert_eq!(j["historyLength"], 5);
    assert_eq!(j["returnImmediately"], true);
    assert_eq!(from_json::<MessageSendConfiguration>(j)?, cfg);
    assert_eq!(to_json(&MessageSendConfiguration::default())?, json!({}));
    assert!(
        try_from_json::<MessageSendConfiguration>(json!({"acceptedOutputModes": "not an array"})).is_err()
    );
    let c: MessageSendConfiguration = from_json(json!({"acceptedOutputModes": null}))?;
    assert!(c.accepted_output_modes.is_none());

    let p = MessageSendParams {
        configuration: Some(cfg),
        message: text_message("m", Role::User),
        metadata: Some(json!({"x": 1})),
    };
    let j = to_json(&p)?;
    assert_eq!(j["message"]["messageId"], "m");
    assert_eq!(j["configuration"]["historyLength"], 5);
    assert_eq!(j["metadata"]["x"], 1);
    assert_eq!(from_json::<MessageSendParams>(j)?, p);
    let minimal = MessageSendParams {
        message: text_message("m", Role::User),
        ..Default::default()
    };
    let j = to_json(&minimal)?;
    assert!(j.get("configuration").is_none());
    assert!(j.get("metadata").is_none());
    assert!(try_from_json::<MessageSendParams>(json!({})).is_err());
    Ok(())
}

#[test]
fn update_events() -> TestResult {
    let a = TaskArtifactUpdateEvent {
        append: Some(true),
        artifact: Artifact {
            artifact_id: "a".into(),
            parts: vec![Part::text("p")],
            ..Default::default()
        },
        context_id: "c".into(),
        last_chunk: Some(false),
        metadata: Some(json!({"m": 1})),
        task_id: "t".into(),
        index: None,
    };
    let j = to_json(&a)?;
    assert_eq!(j["artifact"]["artifactId"], "a");
    assert_eq!(j["append"], true);
    assert_eq!(j["lastChunk"], false);
    assert_eq!(j["taskId"], "t");
    assert_eq!(from_json::<TaskArtifactUpdateEvent>(j)?, a);
    let minimal: TaskArtifactUpdateEvent = from_json(json!({
        "artifact": {"artifactId": "a", "parts": []}, "contextId": "c", "taskId": "t"
    }))?;
    assert!(minimal.append.is_none());
    assert!(try_from_json::<TaskArtifactUpdateEvent>(json!({"contextId": "c", "taskId": "t"})).is_err());
    assert!(
        try_from_json::<TaskArtifactUpdateEvent>(json!({
            "artifact": {"artifactId": "a", "parts": []}, "contextId": "c", "taskId": ""
        }))
        .is_err()
    );

    let s = TaskStatusUpdateEvent {
        context_id: "c".into(),
        metadata: None,
        status: TaskStatus::new(TaskState::Working),
        task_id: "t".into(),
    };
    let j = to_json(&s)?;
    assert_eq!(
        j,
        json!({"contextId": "c", "status": {"state": "TASK_STATE_WORKING"}, "taskId": "t"})
    );
    assert_eq!(from_json::<TaskStatusUpdateEvent>(j)?, s);
    assert!(try_from_json::<TaskStatusUpdateEvent>(json!({"contextId": "c", "taskId": "t"})).is_err());
    Ok(())
}

#[test]
fn id_param_types() -> TestResult {
    let p = TaskIdParams {
        id: "t".into(),
        metadata: Some(json!({"a": 1})),
    };
    let j = to_json(&p)?;
    assert_eq!(j, json!({"id": "t", "metadata": {"a": 1}}));
    assert_eq!(from_json::<TaskIdParams>(j)?, p);
    assert!(serde_json::to_value(TaskIdParams::default()).is_err());
    assert!(try_from_json::<TaskIdParams>(json!({"id": ""})).is_err());
    assert!(try_from_json::<TaskIdParams>(json!({})).is_err());

    let q = TaskQueryParams {
        history_length: Some(3),
        id: "t".into(),
        metadata: None,
    };
    let j = to_json(&q)?;
    assert_eq!(j, json!({"id": "t", "historyLength": 3}));
    assert_eq!(from_json::<TaskQueryParams>(j)?, q);

    let g = GetTaskPushNotificationConfigParams {
        id: "t".into(),
        metadata: None,
        push_notification_config_id: Some("c".into()),
    };
    let j = to_json(&g)?;
    assert_eq!(j, json!({"id": "t", "pushNotificationConfigId": "c"}));
    assert_eq!(from_json::<GetTaskPushNotificationConfigParams>(j)?, g);

    let l = ListTaskPushNotificationConfigParams {
        id: "t".into(),
        metadata: None,
    };
    assert_eq!(to_json(&l)?, json!({"id": "t"}));
    assert_eq!(
        from_json::<ListTaskPushNotificationConfigParams>(json!({"id": "t"}))?,
        l
    );

    let d = DeleteTaskPushNotificationConfigParams {
        id: "t".into(),
        metadata: None,
        push_notification_config_id: "c".into(),
    };
    let j = to_json(&d)?;
    assert_eq!(j, json!({"id": "t", "pushNotificationConfigId": "c"}));
    assert_eq!(from_json::<DeleteTaskPushNotificationConfigParams>(j)?, d);
    assert!(try_from_json::<DeleteTaskPushNotificationConfigParams>(json!({"id": "t"})).is_err());
    assert!(
        try_from_json::<DeleteTaskPushNotificationConfigParams>(
            json!({"id": "t", "pushNotificationConfigId": ""})
        )
        .is_err()
    );
    assert!(
        serde_json::to_value(DeleteTaskPushNotificationConfigParams {
            id: "t".into(),
            ..Default::default()
        })
        .is_err()
    );
    Ok(())
}

#[test]
fn a2a_error_serialization_and_predefined_errors() -> TestResult {
    let e = A2AError {
        code: -32001,
        data: Some(json!("Additional info")),
        message: Some("Task not found".into()),
    };
    let j = to_json(&e)?;
    assert_eq!(
        j,
        json!({"code": -32001, "message": "Task not found", "data": "Additional info"})
    );
    assert_eq!(from_json::<A2AError>(j)?, e);
    let minimal = A2AError {
        code: -32600,
        ..Default::default()
    };
    let j = to_json(&minimal)?;
    assert_eq!(j, json!({"code": -32600, "message": ""}));
    let back: A2AError = from_json(j)?;
    assert_eq!(back.message.as_deref(), Some(""));
    assert!(back.data.is_none());
    assert_eq!(A2AError::default().code, -1);

    let cases: Vec<(A2AError, i64, &str)> = vec![
        (
            A2AError::invalid_request(),
            -32600,
            "Request payload validation error",
        ),
        (A2AError::server_error(), -32603, "Server error"),
        (A2AError::invalid_params(), -32602, "Invalid parameters"),
        (A2AError::json_parse_error(), -32700, "Invalid JSON payload"),
        (A2AError::internal_error(), -32603, "Internal error"),
        (A2AError::method_not_found(), -32601, "Method not found"),
        (A2AError::task_not_found(), -32001, "Task not found"),
        (A2AError::task_not_cancelable(), -32002, "Task cannot be canceled"),
        (
            A2AError::push_notification_not_supported(),
            -32003,
            "Push Notification is not supported",
        ),
        (
            A2AError::unsupported_operation(),
            -32004,
            "This operation is not supported",
        ),
        (
            A2AError::content_type_not_supported(),
            -32005,
            "Incompatible content types",
        ),
        (
            A2AError::invalid_agent_response(),
            -32006,
            "Invalid agent response",
        ),
        (
            A2AError::authenticated_extended_card_not_configured(),
            -32007,
            "Authenticated Extended Card is not configured",
        ),
        (
            A2AError::extension_support_required(),
            -32008,
            "Extension Support Required",
        ),
        (A2AError::version_not_supported(), -32009, "Version Not Supported"),
    ];
    for (e, code, msg) in cases {
        assert_eq!(e.code, code);
        assert_eq!(e.message.as_deref(), Some(msg));
        let j = to_json(&e)?;
        assert_eq!(j["code"], code);
        assert_eq!(from_json::<A2AError>(j)?, e);
    }
    assert_eq!(A2AErrorCode::JsonrpcParseError.code(), -32700);
    assert_eq!(A2AErrorCode::A2aConcurrentLimit.code(), -32111);
    Ok(())
}

#[test]
fn security_schemes() -> TestResult {
    let api = SecurityScheme::ApiKey(APIKeySecurityScheme {
        description: Some("d".into()),
        in_: "header".into(),
        name: "X-Key".into(),
    });
    let j = to_json(&api)?;
    assert_eq!(
        j,
        json!({"type": "apiKey", "description": "d", "in": "header", "name": "X-Key"})
    );
    assert_eq!(from_json::<SecurityScheme>(j)?, api);

    let http = SecurityScheme::HttpAuth(HTTPAuthSecurityScheme {
        description: None,
        scheme: "bearer".into(),
    });
    assert_eq!(to_json(&http)?, json!({"type": "http", "scheme": "bearer"}));

    let mut scopes = BTreeMap::new();
    scopes.insert("read".to_string(), "Read".to_string());
    let oauth = SecurityScheme::OAuth2(Box::new(OAuth2SecurityScheme {
        description: None,
        flows: OAuthFlows {
            authorization_code: Some(AuthorizationCodeOAuthFlow {
                authorization_url: "https://a".into(),
                refresh_url: Some("https://r".into()),
                scopes: scopes.clone(),
                token_url: "https://t".into(),
            }),
            client_credentials: Some(ClientCredentialsOAuthFlow {
                refresh_url: None,
                scopes: scopes.clone(),
                token_url: "https://t".into(),
            }),
            implicit: Some(ImplicitOAuthFlow {
                authorization_url: "https://a".into(),
                refresh_url: None,
                scopes: scopes.clone(),
            }),
            password: Some(PasswordOAuthFlow {
                refresh_url: None,
                scopes,
                token_url: "https://t".into(),
            }),
        },
        oauth2_metadata_url: Some("https://m".into()),
    }));
    let j = to_json(&oauth)?;
    assert_eq!(j["type"], "oauth2");
    assert_eq!(j["flows"]["authorizationCode"]["authorizationUrl"], "https://a");
    assert_eq!(j["flows"]["authorizationCode"]["refreshUrl"], "https://r");
    assert_eq!(j["flows"]["clientCredentials"]["tokenUrl"], "https://t");
    assert_eq!(j["flows"]["implicit"]["scopes"]["read"], "Read");
    assert_eq!(j["flows"]["password"]["tokenUrl"], "https://t");
    assert_eq!(j["oauth2MetadataUrl"], "https://m");
    assert_eq!(from_json::<SecurityScheme>(j)?, oauth);

    let oidc = SecurityScheme::OpenIdConnect(OpenIdConnectSecurityScheme {
        description: None,
        open_id_connect_url: "https://o".into(),
    });
    assert_eq!(
        to_json(&oidc)?,
        json!({"type": "openIdConnect", "openIdConnectUrl": "https://o"})
    );
    let mtls = SecurityScheme::MutualTls(MutualTLSSecurityScheme { description: None });
    assert_eq!(to_json(&mtls)?, json!({"type": "mutualTLS"}));
    assert_eq!(mtls.type_name(), "mutualTLS");
    assert!(try_from_json::<SecurityScheme>(json!({"type": "unknown"})).is_err());
    assert!(try_from_json::<SecurityScheme>(json!({"scheme": "x"})).is_err());
    Ok(())
}

#[test]
fn agent_card_components() -> TestResult {
    let skill = AgentSkill {
        id: "s".into(),
        name: "n".into(),
        description: "d".into(),
        tags: vec!["t".into()],
        examples: Some(vec!["e".into()]),
        input_modes: Some(vec!["text".into()]),
        output_modes: Some(vec!["text".into()]),
        extension: Some("x".into()),
    };
    let j = to_json(&skill)?;
    assert_eq!(j["inputModes"], json!(["text"]));
    assert_eq!(from_json::<AgentSkill>(j)?, skill);
    assert!(try_from_json::<AgentSkill>(json!({"id": "s", "name": "n"})).is_err());
    assert!(
        try_from_json::<AgentSkill>(json!({"id": "", "name": "n", "description": "", "tags": []})).is_err()
    );
    assert!(serde_json::to_value(AgentSkill::default()).is_err());

    let ext = AgentExtension {
        uri: "urn:x".into(),
        required: Some(true),
        description: Some("d".into()),
        params: Some(json!({"p": 1})),
    };
    let j = to_json(&ext)?;
    assert_eq!(j["uri"], "urn:x");
    assert_eq!(j["params"]["p"], 1);
    assert_eq!(from_json::<AgentExtension>(j)?, ext);
    assert!(try_from_json::<AgentExtension>(json!({})).is_err());
    assert!(try_from_json::<AgentExtension>(json!({"uri": ""})).is_err());

    let caps = AgentCapabilities {
        streaming: Some(true),
        push_notifications: Some(false),
        extended_agent_card: Some(true),
        extension: Some("e".into()),
        extensions: Some(vec![ext]),
    };
    let j = to_json(&caps)?;
    assert_eq!(j["streaming"], true);
    assert_eq!(j["pushNotifications"], false);
    assert_eq!(j["extendedAgentCard"], true);
    assert_eq!(j["extensions"][0]["uri"], "urn:x");
    assert_eq!(from_json::<AgentCapabilities>(j)?, caps);
    assert_eq!(to_json(&AgentCapabilities::default())?, json!({}));

    let provider = AgentProvider {
        organization: "org".into(),
        url: "https://org".into(),
    };
    assert_eq!(
        to_json(&provider)?,
        json!({"organization": "org", "url": "https://org"})
    );
    assert!(try_from_json::<AgentProvider>(json!({"organization": "org"})).is_err());

    let iface = AgentInterface {
        url: "http://h/jsonrpc".into(),
        protocol_binding: "JSONRPC".into(),
        protocol_version: "1.0".into(),
        tenant: Some("t".into()),
    };
    let j = to_json(&iface)?;
    assert_eq!(j["protocolBinding"], "JSONRPC");
    assert_eq!(j["protocolVersion"], "1.0");
    assert_eq!(j["tenant"], "t");
    assert_eq!(from_json::<AgentInterface>(j)?, iface);
    let minimal = AgentInterface::jsonrpc("http://h");
    assert!(to_json(&minimal)?.get("tenant").is_none());
    assert!(try_from_json::<AgentInterface>(json!({"url": "u", "protocolBinding": "b"})).is_err());

    let mut schemes = BTreeMap::new();
    schemes.insert("oauth".to_string(), vec!["read".to_string()]);
    let req = SecurityRequirement { schemes };
    let j = to_json(&req)?;
    assert_eq!(j, json!({"oauth": ["read"]}));
    assert_eq!(from_json::<SecurityRequirement>(j)?, req);
    assert!(try_from_json::<SecurityRequirement>(json!({"oauth": "read"})).is_err());
    assert!(try_from_json::<SecurityRequirement>(json!({"oauth": [1]})).is_err());

    let sig = AgentCardSignature {
        protected_: "p".into(),
        signature: "s".into(),
        header: Some(json!({"kid": "k"})),
    };
    let j = to_json(&sig)?;
    assert_eq!(
        j,
        json!({"protected": "p", "signature": "s", "header": {"kid": "k"}})
    );
    assert_eq!(from_json::<AgentCardSignature>(j)?, sig);
    assert!(try_from_json::<AgentCardSignature>(json!({"signature": "s"})).is_err());
    let no_header: AgentCardSignature =
        from_json(json!({"protected": "p", "signature": "s", "header": null}))?;
    assert!(no_header.header.is_none());
    Ok(())
}

#[test]
fn agent_card_full_and_minimal() -> TestResult {
    let mut schemes = BTreeMap::new();
    schemes.insert(
        "api".to_string(),
        SecurityScheme::ApiKey(APIKeySecurityScheme {
            description: None,
            in_: "header".into(),
            name: "X".into(),
        }),
    );
    let card = AgentCard {
        name: "Agent".into(),
        description: "desc".into(),
        provider: Some(AgentProvider {
            organization: "o".into(),
            url: "u".into(),
        }),
        icon_url: Some("i".into()),
        version: "1.0".into(),
        documentation_url: Some("d".into()),
        capabilities: AgentCapabilities {
            streaming: Some(true),
            ..Default::default()
        },
        security_schemes: Some(schemes),
        security: Some(vec!["api".into()]),
        default_input_modes: vec!["text".into()],
        default_output_modes: vec!["text".into()],
        skills: vec![AgentSkill {
            id: "s".into(),
            name: "n".into(),
            ..Default::default()
        }],
        supported_interfaces: vec![AgentInterface::jsonrpc("http://h/jsonrpc")],
        security_requirements: Some(vec![SecurityRequirement::default()]),
        signatures: Some(vec![AgentCardSignature {
            protected_: "p".into(),
            signature: "s".into(),
            header: None,
        }]),
        category: Some("c".into()),
        extension: Some("e".into()),
    };
    let j = to_json(&card)?;
    assert_eq!(j["name"], "Agent");
    assert_eq!(j["capabilities"]["streaming"], true);
    assert_eq!(j["securitySchemes"]["api"]["type"], "apiKey");
    assert_eq!(j["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
    assert_eq!(j["signatures"][0]["protected"], "p");
    assert_eq!(j["category"], "c");
    assert_eq!(from_json::<AgentCard>(j)?, card);

    let minimal = json!({
        "name": "A", "description": "", "version": "1", "capabilities": {},
        "defaultInputModes": [], "defaultOutputModes": [], "skills": [], "supportedInterfaces": []
    });
    let c: AgentCard = from_json(minimal.clone())?;
    assert!(c.provider.is_none());
    let back = to_json(&c)?;
    assert_eq!(back, minimal);
    let mut missing = minimal.clone();
    missing.as_object_mut().required()?.remove("skills");
    let e = try_from_json::<AgentCard>(missing).err_or_fail()?;
    assert!(e.contains("AgentCard missing required field: skills"));

    let text = serde_json::to_string(&AgentCard::default())?;
    let round: AgentCard = serde_json::from_str(&text)?;
    assert_eq!(round, AgentCard::default());
    Ok(())
}

#[test]
fn response_result_payloads() -> TestResult {
    let task = Task {
        id: "t".into(),
        context_id: "c".into(),
        status: TaskStatus::new(TaskState::Working),
        ..Default::default()
    };
    let r = SendMessageResult::Task(Box::new(task.clone()));
    let j = to_json(&r)?;
    assert_eq!(j["task"]["id"], "t");
    assert_eq!(from_json::<SendMessageResult>(j)?, r);
    let r = SendMessageResult::Message(Box::new(text_message("m", Role::Agent)));
    let j = to_json(&r)?;
    assert_eq!(j["message"]["messageId"], "m");
    assert_eq!(from_json::<SendMessageResult>(j)?, r);
    assert!(try_from_json::<SendMessageResult>(json!({"other": 1})).is_err());
    assert!(
        try_from_json::<SendMessageResult>(json!({
            "task": to_json(&task)?, "message": to_json(&text_message("m", Role::Agent))?
        }))
        .is_err()
    );

    let s = StreamEvent::StatusUpdate(TaskStatusUpdateEvent {
        context_id: "c".into(),
        metadata: None,
        status: TaskStatus::new(TaskState::Completed),
        task_id: "t".into(),
    });
    let j = to_json(&s)?;
    assert_eq!(j["statusUpdate"]["taskId"], "t");
    assert_eq!(from_json::<StreamEvent>(j)?, s);
    let a = StreamEvent::ArtifactUpdate(TaskArtifactUpdateEvent {
        artifact: Artifact {
            artifact_id: "a".into(),
            ..Default::default()
        },
        context_id: "c".into(),
        task_id: "t".into(),
        ..Default::default()
    });
    let j = to_json(&a)?;
    assert_eq!(j["artifactUpdate"]["artifact"]["artifactId"], "a");
    assert_eq!(from_json::<StreamEvent>(j)?, a);
    assert_eq!(to_json(&StreamEvent::Task(task.clone()))?["task"]["id"], "t");
    assert_eq!(
        to_json(&StreamEvent::Message(text_message("m", Role::Agent)))?["message"]["messageId"],
        "m"
    );
    assert!(try_from_json::<StreamEvent>(json!({"nope": {}})).is_err());
    assert_eq!(StreamEvent::Task(task.clone()).task_id(), Some("t"));

    let resp = success_response(Some(ap_jsonrpc::RequestId::from("1")), &task)?;
    let v = resp.to_value();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], "1");
    assert_eq!(v["result"]["id"], "t");
    assert!(success_response(None, &Task::default()).is_err());
    let list = vec![TaskPushNotificationConfig {
        task_id: "t".into(),
        push_notification_config: PushNotificationConfig {
            url: "u".into(),
            ..Default::default()
        },
    }];
    let resp = success_response(Some(ap_jsonrpc::RequestId::from(2)), &list)?;
    assert_eq!(resp.to_value()["result"][0]["taskId"], "t");
    let parsed: Vec<TaskPushNotificationConfig> = resp.result_as()?;
    assert_eq!(parsed, list);
    Ok(())
}
