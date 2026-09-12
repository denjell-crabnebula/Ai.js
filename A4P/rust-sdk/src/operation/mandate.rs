// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Operation authorization A4P primitives.

use std::sync::Arc;

use serde_json::Value;

use crate::errors::A4PError;
use crate::intent::mandate::user_authorization_policy;
use crate::mandate_security::{server_signature_payload, server_signed_mandate_without_user_signature};
use crate::operation::signing::{
    OPERATION_MANDATE_SERVER_KEY_ID, OPERATION_SERVER_SIGN_ALGORITHM, operation_server_signing_key,
};
use crate::security::{ed25519_sign_text, ed25519_verify_text};
use crate::types::{JsonDict, OperationMandate};
use crate::user_signature::{A4PUserSignatureMethod, UserSignatureContext, verify_user_signature};
use crate::util::{
    format_beijing_display_time, format_iso_z, get_object, is_truthy, now_epoch, parse_iso_z, py_str,
    python_json_dumps, str_trimmed, token_urlsafe,
};

/// Default operation mandate validity in seconds.
pub const DEFAULT_MANDATE_VALIDITY_SECONDS: i64 = 300;

/// Custom display text renderer. It receives a copy of the unsigned mandate.
pub type OperationDisplayTextRenderer = Arc<dyn Fn(JsonDict) -> String + Send + Sync>;

/// Normalize an exact operation into `{action, params}`.
///
/// `params` defaults to `{}` and must be an object when present.
pub fn normalize_operation(operation: Option<&Value>) -> Result<JsonDict, A4PError> {
    let Some(Value::Object(map)) = operation else {
        return Err(A4PError::value("Operation must be an object"));
    };
    normalize_operation_object(map)
}

fn normalize_operation_object(map: &JsonDict) -> Result<JsonDict, A4PError> {
    let action = str_trimmed(map, "action");
    if action.is_empty() {
        return Err(A4PError::value("Operation action missing"));
    }
    let params = match map.get("params") {
        None | Some(Value::Null) => JsonDict::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(_) => return Err(A4PError::value("Operation params must be an object")),
    };
    let mut normalized = JsonDict::new();
    normalized.insert("action".into(), Value::String(action));
    normalized.insert("params".into(), Value::Object(params));
    Ok(normalized)
}

fn operation_call_text(operation: &JsonDict) -> String {
    let action = operation
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let params = get_object(operation, "params").cloned().unwrap_or_default();
    if params.is_empty() {
        return format!("{action}(无参数)");
    }
    let mut keys: Vec<&String> = params.keys().collect();
    keys.sort();
    let params_text = keys
        .iter()
        .map(|key| format!("{key}={}", python_json_dumps(&params[*key])))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{action}({params_text})")
}

/// Build the default Chinese operation display text.
pub fn operation_display_text(operation: &JsonDict, until: &str) -> String {
    format!(
        "授权执行 {}（有效期至 {}）",
        operation_call_text(operation),
        format_beijing_display_time(until)
    )
}

/// Normalize a mandate: fixed type, normalized operation and signature containers.
pub fn normalize_operation_mandate(mandate: &JsonDict) -> Result<OperationMandate, A4PError> {
    let mut normalized = mandate.clone();
    let operation = get_object(mandate, "operation").cloned().unwrap_or_default();
    normalized.insert(
        "operation".into(),
        Value::Object(normalize_operation_object(&operation)?),
    );
    let signatures = get_object(mandate, "signatures").cloned().unwrap_or_default();
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
    normalized.insert(
        "type".into(),
        Value::String(crate::mandate_security::OPERATION_MANDATE_TYPE.into()),
    );
    Ok(normalized)
}

/// Return the complete signed core of an operation mandate.
pub fn operation_mandate_core_payload(mandate: &JsonDict) -> Result<JsonDict, A4PError> {
    let normalized = normalize_operation_mandate(mandate)?;
    let field = |key: &str, default: Value| normalized.get(key).cloned().unwrap_or(default);
    let mut core = JsonDict::new();
    core.insert("type".into(), field("type", Value::String(String::new())));
    core.insert(
        "operationId".into(),
        field("operationId", Value::String(String::new())),
    );
    core.insert("server".into(), field("server", Value::String(String::new())));
    core.insert("subject".into(), field("subject", Value::Object(JsonDict::new())));
    core.insert(
        "operation".into(),
        field("operation", Value::Object(JsonDict::new())),
    );
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

/// Build the algorithm neutral user signature context of an operation mandate.
pub fn operation_user_signature_context(
    mandate: &JsonDict,
    expected_user_id: Option<&str>,
) -> Result<UserSignatureContext, A4PError> {
    let normalized = normalize_operation_mandate(mandate)?;
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
        mandate_type: crate::mandate_security::OPERATION_MANDATE_TYPE.into(),
        server_signed_mandate: server_signed_mandate_without_user_signature(&normalized)?,
        signature_method,
        expected_user_id: expected_user_id.map(str::to_string),
    })
}

/// Parameters of [`create_operation_mandate`].
#[derive(Clone)]
pub struct CreateOperationMandate<'a> {
    /// The exact operation, `{action, params}`.
    pub operation: &'a JsonDict,
    /// Server id written into the mandate.
    pub server_url: &'a str,
    /// Agent id, written as `subject.id = "agent:{agent_id}"`.
    pub agent_id: &'a str,
    /// Validity in seconds.
    pub validity_seconds: i64,
    /// Optional agent public key copied into `subject.agentKey`.
    pub agent_public_key: Option<&'a JsonDict>,
    /// Whether the mandate requires a user signature.
    pub require_user_signature: bool,
    /// The user signature method identifier.
    pub user_signature_method: Option<&'a str>,
    /// The method policy signed into the mandate.
    pub user_signature_method_policy: Option<&'a JsonDict>,
    /// Optional custom display text renderer.
    pub display_text_renderer: Option<&'a OperationDisplayTextRenderer>,
}

impl<'a> CreateOperationMandate<'a> {
    /// Defaults matching the Python keyword arguments.
    pub fn new(operation: &'a JsonDict, server_url: &'a str) -> Self {
        Self {
            operation,
            server_url,
            agent_id: "a4p-agent",
            validity_seconds: DEFAULT_MANDATE_VALIDITY_SECONDS,
            agent_public_key: None,
            require_user_signature: true,
            user_signature_method: None,
            user_signature_method_policy: None,
            display_text_renderer: None,
        }
    }
}

/// Create and Server-sign a new operation mandate.
pub fn create_operation_mandate(params: CreateOperationMandate<'_>) -> Result<OperationMandate, A4PError> {
    let signature_method = params
        .user_signature_method
        .unwrap_or_default()
        .trim()
        .to_string();
    if params.require_user_signature && signature_method.is_empty() {
        return Err(A4PError::value("userSignatureMethod missing"));
    }
    let operation_id = format!("op_{}", token_urlsafe(32));
    let until_iso = format_iso_z(now_epoch() + params.validity_seconds);
    let normalized_operation = normalize_operation_object(params.operation)?;

    let mut subject = JsonDict::new();
    subject.insert("type".into(), Value::String("agent".into()));
    subject.insert("id".into(), Value::String(format!("agent:{}", params.agent_id)));
    if let Some(agent_public_key) = params.agent_public_key {
        subject.insert("agentKey".into(), Value::Object(agent_public_key.clone()));
    }

    let mut valid_time = JsonDict::new();
    valid_time.insert("until".into(), Value::String(until_iso.clone()));
    valid_time.insert(
        "displayUntil".into(),
        Value::String(format_beijing_display_time(&until_iso)),
    );
    valid_time.insert("timezone".into(), Value::String("Asia/Shanghai".into()));

    let mut server_signature = JsonDict::new();
    server_signature.insert(
        "alg".into(),
        Value::String(OPERATION_SERVER_SIGN_ALGORITHM.into()),
    );
    server_signature.insert(
        "keyId".into(),
        Value::String(OPERATION_MANDATE_SERVER_KEY_ID.into()),
    );
    server_signature.insert("signature".into(), Value::String(String::new()));
    let mut signatures = JsonDict::new();
    signatures.insert("server".into(), Value::Object(server_signature));
    signatures.insert("user".into(), Value::Object(JsonDict::new()));

    let mut mandate = JsonDict::new();
    mandate.insert(
        "type".into(),
        Value::String(crate::mandate_security::OPERATION_MANDATE_TYPE.into()),
    );
    mandate.insert("operationId".into(), Value::String(operation_id));
    mandate.insert("server".into(), Value::String(params.server_url.into()));
    mandate.insert("subject".into(), Value::Object(subject));
    let display_text = operation_display_text(&normalized_operation, &until_iso);
    mandate.insert("operation".into(), Value::Object(normalized_operation));
    mandate.insert("validTime".into(), Value::Object(valid_time));
    mandate.insert(
        "userAuthorization".into(),
        Value::Object(user_authorization_policy(
            params.require_user_signature,
            &signature_method,
            params.user_signature_method_policy,
        )),
    );
    mandate.insert("displayText".into(), Value::String(display_text));
    mandate.insert("signatures".into(), Value::Object(signatures));
    if let Some(renderer) = params.display_text_renderer {
        let text = renderer(mandate.clone());
        mandate.insert("displayText".into(), Value::String(text));
    }
    sign_server_mandate(&mandate)
}

/// Attach a fresh Server signature to an operation mandate.
pub fn sign_server_mandate(mandate: &JsonDict) -> Result<OperationMandate, A4PError> {
    let mut signed = normalize_operation_mandate(mandate)?;
    let payload = server_signature_payload(&operation_mandate_core_payload(&signed)?)?;
    let key = operation_server_signing_key()?;
    let mut server_signature = JsonDict::new();
    server_signature.insert(
        "alg".into(),
        Value::String(OPERATION_SERVER_SIGN_ALGORITHM.into()),
    );
    server_signature.insert(
        "keyId".into(),
        Value::String(OPERATION_MANDATE_SERVER_KEY_ID.into()),
    );
    server_signature.insert(
        "signature".into(),
        Value::String(ed25519_sign_text(&payload, &key)),
    );
    if let Some(Value::Object(signatures)) = signed.get_mut("signatures") {
        signatures.insert("server".into(), Value::Object(server_signature));
    }
    Ok(signed)
}

/// Options of [`verify_operation_mandate_for_completion`].
#[derive(Clone, Copy, Default)]
pub struct VerifyOperationMandate<'a> {
    /// The current operation rebuilt by the Tool Server.
    pub expected: Option<&'a JsonDict>,
    /// Expected owner of the user credential, when checked.
    pub expected_user_id: Option<&'a str>,
    /// Whether `signatures.user` must be present and valid.
    pub require_user_signature: bool,
    /// The method used to verify the user signature.
    pub user_signature_method: Option<&'a dyn A4PUserSignatureMethod>,
}

impl<'a> VerifyOperationMandate<'a> {
    /// Defaults matching the Python keyword arguments (`require_user_signature=True`).
    pub fn new() -> Self {
        Self {
            require_user_signature: true,
            ..Default::default()
        }
    }
}

/// Verify operation equality, Server signature, user signature and validity.
pub fn verify_operation_mandate_for_completion(
    mandate: &JsonDict,
    options: VerifyOperationMandate<'_>,
) -> Result<(), String> {
    if mandate.get("type").and_then(Value::as_str) != Some(crate::mandate_security::OPERATION_MANDATE_TYPE) {
        return Err("Invalid mandate type".into());
    }
    let normalized = normalize_operation_mandate(mandate).map_err(|error| error.to_string())?;
    let empty = JsonDict::new();
    let expected_operation =
        normalize_operation_object(options.expected.unwrap_or(&empty)).map_err(|error| error.to_string())?;
    let operation = get_object(&normalized, "operation").cloned().unwrap_or_default();
    if operation.get("action") != expected_operation.get("action") {
        return Err("Operation action mismatch".into());
    }
    if operation.get("params") != expected_operation.get("params") {
        return Err("Operation params mismatch".into());
    }

    let signatures = get_object(&normalized, "signatures").cloned().unwrap_or_default();
    let server_sig = get_object(&signatures, "server").cloned().unwrap_or_default();
    let server_alg = str_trimmed(&server_sig, "alg");
    if server_alg != OPERATION_SERVER_SIGN_ALGORITHM {
        return Err(format!(
            "Server signature alg mismatch: expected '{OPERATION_SERVER_SIGN_ALGORITHM}', got '{server_alg}'"
        ));
    }
    let server_signature = str_trimmed(&server_sig, "signature");
    if server_signature.is_empty() {
        return Err("Server signature missing".into());
    }
    let server_payload =
        server_signature_payload(&operation_mandate_core_payload(&normalized).map_err(|e| e.to_string())?)
            .map_err(|error| error.to_string())?;
    let key =
        operation_server_signing_key().map_err(|error| format!("Server signing key unavailable: {error}"))?;
    if !ed25519_verify_text(&server_payload, &server_signature, &key.verifying_key()) {
        return Err("Server signature invalid".into());
    }

    let user_sig = get_object(&signatures, "user").cloned().unwrap_or_default();
    let context = operation_user_signature_context(&normalized, options.expected_user_id)
        .map_err(|error| error.to_string())?;
    verify_user_signature(
        &context,
        &user_sig,
        options.user_signature_method,
        options.require_user_signature,
    )?;

    let valid_time = get_object(&normalized, "validTime").cloned().unwrap_or_default();
    let Some(until_str) = valid_time.get("until").filter(|v| is_truthy(v)).map(py_str) else {
        return Err("Mandate has no validTime".into());
    };
    let Some(until_ts) = parse_iso_z(&until_str) else {
        return Err("Mandate validTime format invalid".into());
    };
    if now_epoch() > until_ts {
        return Err(format!("Mandate has expired (expired at {until_str})"));
    }
    Ok(())
}
