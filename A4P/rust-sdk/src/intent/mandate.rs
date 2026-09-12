// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Intent authorization A4P primitives.

use std::sync::Arc;

use serde_json::Value;

use crate::errors::A4PError;
use crate::intent::scope::{normalize_action_specs, normalize_execution_policy, normalize_intent_scope};
use crate::intent::signing::{
    INTENT_MANDATE_SERVER_KEY_ID, INTENT_SERVER_SIGN_ALGORITHM, intent_server_signing_key,
};
use crate::mandate_security::{server_signature_payload, server_signed_mandate_without_user_signature};
use crate::security::{ed25519_sign_text, ed25519_verify_text};
use crate::types::{IntentMandate, JsonDict};
use crate::user_signature::{A4PUserSignatureMethod, UserSignatureContext, verify_user_signature};
use crate::util::{
    format_beijing_display_time, format_iso_z, get_object, is_truthy, now_epoch, parse_iso_z, py_str,
    python_json_dumps, str_trimmed, token_urlsafe,
};

/// Default intent mandate validity in seconds.
pub const DEFAULT_INTENT_MANDATE_VALIDITY_SECONDS: i64 = 3600;

/// Custom display text renderer. It receives a copy of the unsigned mandate.
pub type IntentDisplayTextRenderer = Arc<dyn Fn(JsonDict) -> String + Send + Sync>;

fn format_params(params: &Value) -> String {
    match params {
        Value::String(text) if text == "*" => "任意参数".to_string(),
        Value::Object(map) if !map.is_empty() => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            keys.iter()
                .map(|key| {
                    let value = &map[*key];
                    if value == &Value::String("*".into()) {
                        format!("{key}=任意")
                    } else {
                        format!("{key}={}", python_json_dumps(value))
                    }
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => "无参数".to_string(),
    }
}

fn format_action_call(action: &JsonDict) -> String {
    let name = action.get("name").and_then(Value::as_str).unwrap_or_default();
    let params = action.get("params").cloned().unwrap_or(Value::Null);
    format!("{name}({})", format_params(&params))
}

/// Build the default Chinese intent display text.
pub fn build_intent_display_text(
    agent_id: &str,
    actions: &[JsonDict],
    start_iso: &str,
    end_iso: &str,
    execution_policy: Option<&JsonDict>,
) -> String {
    let start_local = format_beijing_display_time(start_iso);
    let end_local = format_beijing_display_time(end_iso);
    let action_text = actions
        .iter()
        .map(format_action_call)
        .collect::<Vec<_>>()
        .join("；");
    let policy_text = execution_policy
        .map(|policy| {
            format!(
                "，最多 {} 次",
                policy.get("maxExecutions").map(py_str).unwrap_or_default()
            )
        })
        .unwrap_or_default();
    format!("授权 agent:{agent_id} 在 {start_local} 至 {end_local} 期间调用 {action_text}{policy_text}")
}

/// Normalize a mandate: fixed type, normalized intent scope and signature containers.
pub fn normalize_intent_mandate(mandate: &JsonDict) -> Result<IntentMandate, A4PError> {
    let mut normalized = mandate.clone();
    let signatures = get_object(mandate, "signatures").cloned().unwrap_or_default();
    normalized.insert(
        "type".into(),
        Value::String(crate::mandate_security::INTENT_MANDATE_TYPE.into()),
    );
    normalized.insert(
        "intent".into(),
        Value::Object(normalize_intent_scope(mandate.get("intent"))?),
    );
    let mut signature_container = JsonDict::new();
    signature_container.insert(
        "server".into(),
        Value::Object(get_object(&signatures, "server").cloned().unwrap_or_default()),
    );
    signature_container.insert(
        "user".into(),
        Value::Object(get_object(&signatures, "user").cloned().unwrap_or_default()),
    );
    normalized.insert("signatures".into(), Value::Object(signature_container));
    normalized.insert(
        "userAuthorization".into(),
        Value::Object(
            get_object(mandate, "userAuthorization")
                .cloned()
                .unwrap_or_default(),
        ),
    );
    Ok(normalized)
}

/// Return the complete signed core of an intent mandate.
pub fn intent_mandate_core_payload(mandate: &JsonDict) -> Result<JsonDict, A4PError> {
    let normalized = normalize_intent_mandate(mandate)?;
    let field = |key: &str, default: Value| normalized.get(key).cloned().unwrap_or(default);
    let mut core = JsonDict::new();
    core.insert("type".into(), field("type", Value::String(String::new())));
    core.insert(
        "mandateId".into(),
        field("mandateId", Value::String(String::new())),
    );
    core.insert("server".into(), field("server", Value::String(String::new())));
    core.insert("subject".into(), field("subject", Value::Object(JsonDict::new())));
    core.insert("intent".into(), field("intent", Value::Object(JsonDict::new())));
    core.insert(
        "validTime".into(),
        field("validTime", Value::Object(JsonDict::new())),
    );
    core.insert(
        "userAuthorization".into(),
        field("userAuthorization", Value::Object(JsonDict::new())),
    );
    core.insert(
        "displayText".into(),
        field("displayText", Value::String(String::new())),
    );
    Ok(core)
}

/// Build the algorithm neutral user signature context of an intent mandate.
pub fn intent_user_signature_context(
    mandate: &JsonDict,
    expected_user_id: Option<&str>,
) -> Result<UserSignatureContext, A4PError> {
    let normalized = normalize_intent_mandate(mandate)?;
    let user_authorization = get_object(&normalized, "userAuthorization")
        .cloned()
        .unwrap_or_default();
    let Some(Value::Bool(required)) = user_authorization.get("required") else {
        return Err(A4PError::value("userAuthorization.required missing"));
    };
    let signature_method = str_trimmed(&user_authorization, "signatureMethod");
    if *required && signature_method.is_empty() {
        return Err(A4PError::value("userAuthorization.signatureMethod missing"));
    }
    Ok(UserSignatureContext {
        mandate_type: crate::mandate_security::INTENT_MANDATE_TYPE.into(),
        server_signed_mandate: server_signed_mandate_without_user_signature(&normalized)?,
        signature_method,
        expected_user_id: expected_user_id.map(str::to_string),
    })
}

/// Parameters of [`create_intent_mandate`].
#[derive(Clone)]
pub struct CreateIntentMandate<'a> {
    /// Server id written into the mandate.
    pub server: &'a str,
    /// Agent id, written as `subject.id = "{subject_type}:{agent_id}"`.
    pub agent_id: &'a str,
    /// Raw action specs.
    pub actions: Option<&'a Value>,
    /// Raw execution policy.
    pub execution_policy: Option<&'a Value>,
    /// Validity in seconds.
    pub validity_seconds: i64,
    /// Subject type, default `agent`.
    pub subject_type: &'a str,
    /// Optional agent public key copied into `subject.agentKey`.
    pub agent_public_key: Option<&'a JsonDict>,
    /// Whether the mandate requires a user signature.
    pub require_user_signature: bool,
    /// The user signature method identifier.
    pub user_signature_method: Option<&'a str>,
    /// The method policy signed into the mandate.
    pub user_signature_method_policy: Option<&'a JsonDict>,
    /// Optional custom display text renderer.
    pub display_text_renderer: Option<&'a IntentDisplayTextRenderer>,
}

impl<'a> CreateIntentMandate<'a> {
    /// Defaults matching the Python keyword arguments.
    pub fn new(server: &'a str, agent_id: &'a str, actions: Option<&'a Value>) -> Self {
        Self {
            server,
            agent_id,
            actions,
            execution_policy: None,
            validity_seconds: DEFAULT_INTENT_MANDATE_VALIDITY_SECONDS,
            subject_type: "agent",
            agent_public_key: None,
            require_user_signature: true,
            user_signature_method: None,
            user_signature_method_policy: None,
            display_text_renderer: None,
        }
    }
}

/// Create and Server-sign a new intent mandate.
pub fn create_intent_mandate(params: CreateIntentMandate<'_>) -> Result<IntentMandate, A4PError> {
    let signature_method = params
        .user_signature_method
        .unwrap_or_default()
        .trim()
        .to_string();
    if params.require_user_signature && signature_method.is_empty() {
        return Err(A4PError::value("userSignatureMethod missing"));
    }
    let mandate_id = format!("mdt_{}", token_urlsafe(32));
    let now = now_epoch();
    let start_iso = format_iso_z(now);
    let end_iso = format_iso_z(now + params.validity_seconds);
    let normalized_actions = normalize_action_specs(params.actions)?;
    let normalized_policy = normalize_execution_policy(params.execution_policy)?;

    let mut subject = JsonDict::new();
    subject.insert("type".into(), Value::String(params.subject_type.into()));
    subject.insert(
        "id".into(),
        Value::String(format!("{}:{}", params.subject_type, params.agent_id)),
    );
    if let Some(agent_public_key) = params.agent_public_key {
        subject.insert("agentKey".into(), Value::Object(agent_public_key.clone()));
    }

    let mut intent_scope = JsonDict::new();
    intent_scope.insert(
        "actions".into(),
        Value::Array(normalized_actions.iter().cloned().map(Value::Object).collect()),
    );
    if let Some(policy) = &normalized_policy {
        intent_scope.insert("executionPolicy".into(), Value::Object(policy.clone()));
    }

    let mut valid_time = JsonDict::new();
    valid_time.insert("start".into(), Value::String(start_iso.clone()));
    valid_time.insert("end".into(), Value::String(end_iso.clone()));

    let user_authorization = user_authorization_policy(
        params.require_user_signature,
        &signature_method,
        params.user_signature_method_policy,
    );

    let mut server_signature = JsonDict::new();
    server_signature.insert("alg".into(), Value::String(INTENT_SERVER_SIGN_ALGORITHM.into()));
    server_signature.insert("keyId".into(), Value::String(INTENT_MANDATE_SERVER_KEY_ID.into()));
    server_signature.insert("signature".into(), Value::String(String::new()));
    let mut signatures = JsonDict::new();
    signatures.insert("server".into(), Value::Object(server_signature));
    signatures.insert("user".into(), Value::Object(JsonDict::new()));

    let mut mandate = JsonDict::new();
    mandate.insert(
        "type".into(),
        Value::String(crate::mandate_security::INTENT_MANDATE_TYPE.into()),
    );
    mandate.insert("mandateId".into(), Value::String(mandate_id));
    mandate.insert("server".into(), Value::String(params.server.into()));
    mandate.insert("subject".into(), Value::Object(subject));
    mandate.insert("intent".into(), Value::Object(intent_scope));
    mandate.insert("validTime".into(), Value::Object(valid_time));
    mandate.insert("userAuthorization".into(), Value::Object(user_authorization));
    mandate.insert(
        "displayText".into(),
        Value::String(build_intent_display_text(
            params.agent_id,
            &normalized_actions,
            &start_iso,
            &end_iso,
            normalized_policy.as_ref(),
        )),
    );
    mandate.insert("signatures".into(), Value::Object(signatures));
    if let Some(renderer) = params.display_text_renderer {
        let text = renderer(mandate.clone());
        mandate.insert("displayText".into(), Value::String(text));
    }
    sign_server_mandate(&mandate)
}

/// Build the `userAuthorization` policy object shared by both mandate types.
pub(crate) fn user_authorization_policy(
    require_user_signature: bool,
    signature_method: &str,
    method_policy: Option<&JsonDict>,
) -> JsonDict {
    let mut user_authorization = JsonDict::new();
    if require_user_signature {
        user_authorization.insert("required".into(), Value::Bool(true));
        user_authorization.insert("signatureMethod".into(), Value::String(signature_method.into()));
        user_authorization.insert(
            "methodPolicy".into(),
            Value::Object(method_policy.cloned().unwrap_or_default()),
        );
    } else {
        user_authorization.insert("required".into(), Value::Bool(false));
    }
    user_authorization
}

/// Attach a fresh Server signature to an intent mandate.
pub fn sign_server_mandate(mandate: &JsonDict) -> Result<IntentMandate, A4PError> {
    let mut signed = normalize_intent_mandate(mandate)?;
    let payload = server_signature_payload(&intent_mandate_core_payload(&signed)?)?;
    let key = intent_server_signing_key()?;
    let mut server_signature = JsonDict::new();
    server_signature.insert("alg".into(), Value::String(INTENT_SERVER_SIGN_ALGORITHM.into()));
    server_signature.insert("keyId".into(), Value::String(INTENT_MANDATE_SERVER_KEY_ID.into()));
    server_signature.insert(
        "signature".into(),
        Value::String(ed25519_sign_text(&payload, &key)),
    );
    if let Some(Value::Object(signatures)) = signed.get_mut("signatures") {
        signatures.insert("server".into(), Value::Object(server_signature));
    }
    Ok(signed)
}

/// Options of [`verify_intent_mandate`].
#[derive(Clone, Copy, Default)]
pub struct VerifyIntentMandate<'a> {
    /// Expected `server` value, when checked.
    pub expected_server: Option<&'a str>,
    /// Expected owner of the user credential, when checked.
    pub expected_user_id: Option<&'a str>,
    /// Whether `signatures.user` must be present and valid.
    pub require_user_signature: bool,
    /// The method used to verify the user signature.
    pub user_signature_method: Option<&'a dyn A4PUserSignatureMethod>,
}

impl<'a> VerifyIntentMandate<'a> {
    /// Defaults matching the Python keyword arguments (`require_user_signature=True`).
    pub fn new() -> Self {
        Self {
            require_user_signature: true,
            ..Default::default()
        }
    }
}

/// Verify Server signature, user signature, validity and server binding.
pub fn verify_intent_mandate(mandate: &JsonDict, options: VerifyIntentMandate<'_>) -> Result<(), String> {
    if mandate.get("type").and_then(Value::as_str) != Some(crate::mandate_security::INTENT_MANDATE_TYPE) {
        return Err("Invalid mandate type".into());
    }
    let normalized = normalize_intent_mandate(mandate).map_err(|error| error.to_string())?;
    let signatures = get_object(&normalized, "signatures").cloned().unwrap_or_default();
    let server_sig = get_object(&signatures, "server").cloned().unwrap_or_default();
    let server_alg = str_trimmed(&server_sig, "alg");
    if server_alg != INTENT_SERVER_SIGN_ALGORITHM {
        return Err(format!(
            "Server signature alg mismatch: expected '{INTENT_SERVER_SIGN_ALGORITHM}', got '{server_alg}'"
        ));
    }
    let server_signature = str_trimmed(&server_sig, "signature");
    if server_signature.is_empty() {
        return Err("Server signature missing".into());
    }
    let server_payload =
        server_signature_payload(&intent_mandate_core_payload(&normalized).map_err(|e| e.to_string())?)
            .map_err(|error| error.to_string())?;
    let key =
        intent_server_signing_key().map_err(|error| format!("Server signing key unavailable: {error}"))?;
    if !ed25519_verify_text(&server_payload, &server_signature, &key.verifying_key()) {
        return Err("Server signature invalid".into());
    }

    let user_sig = get_object(&signatures, "user").cloned().unwrap_or_default();
    let context = intent_user_signature_context(&normalized, options.expected_user_id)
        .map_err(|error| error.to_string())?;
    verify_user_signature(
        &context,
        &user_sig,
        options.user_signature_method,
        options.require_user_signature,
    )?;

    let valid_time = get_object(&normalized, "validTime").cloned().unwrap_or_default();
    let start_str = valid_time.get("start").filter(|v| is_truthy(v)).map(py_str);
    let end_str = valid_time.get("end").filter(|v| is_truthy(v)).map(py_str);
    let (Some(start_str), Some(end_str)) = (start_str, end_str) else {
        return Err("Mandate has no validTime".into());
    };
    let (Some(start_ts), Some(end_ts)) = (parse_iso_z(&start_str), parse_iso_z(&end_str)) else {
        return Err("Mandate validTime format invalid".into());
    };
    let now = now_epoch();
    if now < start_ts {
        return Err("Mandate not yet valid".into());
    }
    if now > end_ts {
        return Err("Mandate has expired".into());
    }
    if let Some(expected_server) = options.expected_server {
        let mandate_server = str_trimmed(&normalized, "server");
        let normalized_expected = expected_server.trim();
        if mandate_server != normalized_expected {
            return Err(format!(
                "Mandate server mismatch: expected '{normalized_expected}', got '{mandate_server}'"
            ));
        }
    }
    Ok(())
}
