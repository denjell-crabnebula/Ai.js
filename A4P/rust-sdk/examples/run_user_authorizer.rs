// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Run the standalone A4P User Authorizer with a Browser/WebAuthn UI.
//!
//! The Agent POSTs `{"mandate", "signingOptions"}` to `/authorize` and waits.
//! The mandate is verified against the local trust store before it is queued
//! and shown; the browser page then approves with a passkey or rejects.
//! Registration ceremonies are relayed through `/register`.
//!
//! The HTML, CSS and JavaScript assets are the original browser assets served
//! under `/assets/`, so the original page works unchanged against this server.
//!
//! ```text
//! cargo run -p a4p --example run_user_authorizer -- --trusted-server-keys .a4p/trusted_server_keys.json
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use a4p::user_signature::A4PUserSigner;
use a4p::user_signature::webauthn::WebAuthnUserSigner;
use a4p::{
    JsonDict, StaticA4PServerTrustStore, UserAuthorizationRequest, mandate_identifier,
    sign_user_mandate_with_signer, verify_local_user_authorization_request,
};
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use clap::Parser;
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::oneshot;

const ASSETS: [(&str, &str, &str); 11] = [
    (
        "authorizer.css",
        "text/css; charset=utf-8",
        include_str!("user_authorizer_assets/authorizer.css"),
    ),
    (
        "authorizer.js",
        "text/javascript; charset=utf-8",
        include_str!("user_authorizer_assets/authorizer.js"),
    ),
    (
        "registration.js",
        "text/javascript; charset=utf-8",
        include_str!("user_authorizer_assets/registration.js"),
    ),
    (
        "webauthn.js",
        "text/javascript; charset=utf-8",
        include_str!("user_authorizer_assets/webauthn.js"),
    ),
    (
        "authorizer.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/authorizer.html"),
    ),
    (
        "authorization_card.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/authorization_card.html"),
    ),
    (
        "registration.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/registration.html"),
    ),
    (
        "empty.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/empty.html"),
    ),
    (
        "result.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/result.html"),
    ),
    (
        "error.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/error.html"),
    ),
    (
        "not_found.html",
        "text/html; charset=utf-8",
        include_str!("user_authorizer_assets/not_found.html"),
    ),
];

/// Assets served under `/assets/`; templates are never served directly.
const PUBLIC_ASSETS: [&str; 4] = [
    "authorizer.css",
    "authorizer.js",
    "registration.js",
    "webauthn.js",
];

fn asset_text(name: &str) -> &'static str {
    ASSETS
        .iter()
        .find(|(asset, _, _)| *asset == name)
        .map(|(_, _, text)| *text)
        .unwrap_or("")
}

fn asset_content_type(name: &str) -> &'static str {
    ASSETS
        .iter()
        .find(|(asset, _, _)| *asset == name)
        .map(|(_, content_type, _)| *content_type)
        .unwrap_or("application/octet-stream")
}

/// Python `string.Template.substitute` for `$name` placeholders.
fn render_template(name: &str, values: &[(&str, &str)]) -> String {
    let template = asset_text(name);
    let mut out = String::with_capacity(template.len());
    let mut chars = template.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let rest = &template[index + 1..];
        if rest.starts_with('$') {
            out.push('$');
            chars.next();
            continue;
        }
        let identifier: String = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            .collect();
        match values.iter().find(|(key, _)| *key == identifier) {
            Some((_, value)) if !identifier.is_empty() => {
                out.push_str(value);
                for _ in 0..identifier.len() {
                    chars.next();
                }
            }
            _ => out.push('$'),
        }
    }
    out
}

/// Python `html.escape(text)` with quotes escaped.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

struct PendingAuthorization {
    request: UserAuthorizationRequest,
    sender: oneshot::Sender<Value>,
}

struct PendingRegistration {
    user_id: String,
    creation_options: JsonDict,
    sender: oneshot::Sender<Value>,
}

/// Standalone Browser/WebAuthn User Authorizer service.
pub struct BrowserWebA4PUserAuthorizer {
    trust_store: StaticA4PServerTrustStore,
    user_signer: Box<dyn A4PUserSigner>,
    host: String,
    port: Mutex<u16>,
    open_browser: bool,
    pending: Mutex<IndexMap<String, PendingAuthorization>>,
    registrations: Mutex<HashMap<String, PendingRegistration>>,
}

impl BrowserWebA4PUserAuthorizer {
    /// Create the authorizer with the WebAuthn user signer.
    pub fn new(trust_store: StaticA4PServerTrustStore, host: &str, port: u16, open_browser: bool) -> Self {
        Self {
            trust_store,
            user_signer: Box::new(WebAuthnUserSigner::new()),
            host: host.to_string(),
            port: Mutex::new(port),
            open_browser,
            pending: Mutex::new(IndexMap::new()),
            registrations: Mutex::new(HashMap::new()),
        }
    }

    fn port(&self) -> u16 {
        *self.port.lock()
    }

    fn request_url(&self, authorization_id: &str) -> String {
        format!(
            "http://{}:{}/authorize?authorizationId={}",
            self.host,
            self.port(),
            urlencoding::encode(authorization_id)
        )
    }

    fn registration_url(&self, request_id: &str) -> String {
        format!(
            "http://{}:{}/register?requestId={}",
            self.host,
            self.port(),
            urlencoding::encode(request_id)
        )
    }

    fn open_in_browser(&self, url: &str) {
        if !self.open_browser {
            return;
        }
        let command = if cfg!(target_os = "macos") {
            ("open", vec![url.to_string()])
        } else if cfg!(target_os = "windows") {
            (
                "cmd",
                vec!["/c".to_string(), "start".to_string(), url.to_string()],
            )
        } else {
            ("xdg-open", vec![url.to_string()])
        };
        let _ = std::process::Command::new(command.0).args(command.1).spawn();
    }

    /// True when an authorization with this id is queued.
    pub fn has_pending(&self, authorization_id: &str) -> bool {
        self.pending.lock().contains_key(authorization_id)
    }

    /// The hardened signing options of a queued authorization.
    pub fn pending_signing_options(&self, authorization_id: &str) -> Option<JsonDict> {
        self.pending
            .lock()
            .get(authorization_id)
            .map(|pending| pending.request.signing_options.clone())
    }

    /// Number of queued registrations.
    pub fn registration_count(&self) -> usize {
        self.registrations.lock().len()
    }

    /// Verify, queue and wait for the user to approve or reject a mandate.
    pub async fn authorize(&self, payload: JsonDict) -> Value {
        let Some(mandate) = payload.get("mandate").and_then(Value::as_object).cloned() else {
            return json!({"approved": false, "rejectReason": "mandate missing"});
        };
        let signing_options = payload
            .get("signingOptions")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let authorization_id = match mandate_identifier(&mandate) {
            Ok(id) => id,
            Err(error) => {
                return json!({"approved": false, "rejectReason": error.to_string(), "errorCode": "MANDATE_INVALID"});
            }
        };
        if self.has_pending(&authorization_id) {
            return json!({
                "approved": false,
                "rejectReason": format!("Authorization already pending: {authorization_id}"),
                "errorCode": "MANDATE_ID_CONFLICT",
            });
        }
        let request = UserAuthorizationRequest {
            mandate: mandate.clone(),
            signing_options,
        };
        let safe_signing_options = match verify_local_user_authorization_request(
            &request,
            &self.trust_store,
            Some(self.user_signer.signature_method()),
        ) {
            Ok(options) => options,
            Err(error) => {
                return json!({"approved": false, "rejectReason": error.message, "errorCode": error.code});
            }
        };
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().insert(
            authorization_id.clone(),
            PendingAuthorization {
                request: UserAuthorizationRequest {
                    mandate,
                    signing_options: safe_signing_options,
                },
                sender,
            },
        );
        let url = self.request_url(&authorization_id);
        println!("[A4P User Authorizer] pending authorization: {url}");
        self.open_in_browser(&url);
        receiver
            .await
            .unwrap_or_else(|_| json!({"approved": false, "rejectReason": "Authorization was dropped"}))
    }

    /// Queue a browser key registration and wait for the credential.
    pub async fn register(&self, payload: JsonDict) -> Value {
        let registration_request_id = payload
            .get("registrationRequestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let user_id = payload
            .get("userId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let creation_options = payload.get("creationOptions").and_then(Value::as_object).cloned();
        if registration_request_id.is_empty() {
            return json!({"ok": false, "message": "registrationRequestId missing"});
        }
        if user_id.is_empty() {
            return json!({"ok": false, "message": "userId missing"});
        }
        let Some(creation_options) = creation_options.filter(|options| {
            !options
                .get("challenge")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .is_empty()
        }) else {
            return json!({"ok": false, "message": "WebAuthn creationOptions missing"});
        };
        if self.registrations.lock().contains_key(&registration_request_id) {
            return json!({
                "ok": false,
                "message": format!("Browser key registration already pending: {registration_request_id}"),
            });
        }
        let (sender, receiver) = oneshot::channel();
        self.registrations.lock().insert(
            registration_request_id.clone(),
            PendingRegistration {
                user_id,
                creation_options,
                sender,
            },
        );
        let url = self.registration_url(&registration_request_id);
        println!("[A4P User Authorizer] pending browser key registration: {url}");
        self.open_in_browser(&url);
        receiver
            .await
            .unwrap_or_else(|_| json!({"ok": false, "message": "Registration was dropped"}))
    }

    /// `POST /approve` with `{"authorizationId", "assertion"}`.
    pub fn resolve_json_approval(&self, payload: &JsonDict) -> Value {
        let authorization_id = payload
            .get("authorizationId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(assertion) = payload.get("assertion").and_then(Value::as_object) else {
            return json!({"ok": false, "message": "WebAuthn assertion missing"});
        };
        self.resolve(authorization_id, true, "", Some(assertion))
    }

    /// `POST /register/complete` with `{"registrationRequestId", "credential"}`.
    pub fn resolve_registration(&self, payload: &JsonDict) -> Value {
        let registration_request_id = payload
            .get("registrationRequestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let credential = payload.get("credential").and_then(Value::as_object).cloned();
        let mut registrations = self.registrations.lock();
        if !registrations.contains_key(&registration_request_id) {
            return json!({"ok": false, "message": format!("No pending registration: {registration_request_id}")});
        }
        let Some(credential) = credential else {
            return json!({"ok": false, "message": "WebAuthn registration credential missing"});
        };
        let Some(pending) = registrations.remove(&registration_request_id) else {
            return json!({"ok": false, "message": format!("No pending registration: {registration_request_id}")});
        };
        let _ = pending.sender.send(json!({
            "registrationRequestId": registration_request_id,
            "userId": pending.user_id,
            "credential": credential,
        }));
        json!({"ok": true, "message": "Browser key registration response returned to Agent"})
    }

    /// Resolve a queued authorization with approval (and assertion) or rejection.
    pub fn resolve(
        &self,
        authorization_id: &str,
        approved: bool,
        reject_reason: &str,
        webauthn_assertion: Option<&JsonDict>,
    ) -> Value {
        let mut pending_map = self.pending.lock();
        if !pending_map.contains_key(authorization_id) {
            return json!({"ok": false, "message": format!("No pending authorization: {authorization_id}")});
        }
        if approved {
            let Some(assertion) = webauthn_assertion else {
                return json!({"ok": false, "message": "WebAuthn assertion missing"});
            };
            let mandate = pending_map[authorization_id].request.mandate.clone();
            let signing_input = json!({"assertion": assertion})
                .as_object()
                .cloned()
                .unwrap_or_default();
            let signed = match sign_user_mandate_with_signer(
                &mandate,
                self.user_signer.as_ref(),
                Some(&signing_input),
            ) {
                Ok(signed) => signed,
                Err(error) => return json!({"ok": false, "message": error.to_string()}),
            };
            let Some(pending) = pending_map.shift_remove(authorization_id) else {
                return json!({"ok": false, "message": format!("No pending authorization: {authorization_id}")});
            };
            let _ = pending
                .sender
                .send(json!({"approved": true, "signedMandate": signed}));
            return json!({"ok": true, "message": format!("Approved {authorization_id}")});
        }
        let Some(pending) = pending_map.shift_remove(authorization_id) else {
            return json!({"ok": false, "message": format!("No pending authorization: {authorization_id}")});
        };
        let reason = if reject_reason.is_empty() {
            "Rejected in browser User Authorizer"
        } else {
            reject_reason
        };
        let _ = pending
            .sender
            .send(json!({"approved": false, "rejectReason": reason}));
        json!({"ok": true, "message": format!("Rejected {authorization_id}")})
    }

    /// Render the index page, optionally focused on one authorization.
    pub fn render_index(&self, selected_authorization_id: Option<&str>) -> String {
        let pending = self.pending.lock();
        let body = match selected_authorization_id.filter(|id| !id.is_empty()) {
            Some(id) => match pending.get(id) {
                Some(item) => self.render_pending(&item.request),
                None => Self::render_empty(&format!("No pending authorization: {id}")),
            },
            None if pending.is_empty() => Self::render_empty("No pending A4P authorization requests."),
            None => pending
                .values()
                .map(|item| self.render_pending(&item.request))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        render_template("authorizer.html", &[("body", &body)])
    }

    fn render_pending(&self, request: &UserAuthorizationRequest) -> String {
        let mandate = &request.mandate;
        let display_text = html_escape(
            mandate
                .get("displayText")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .unwrap_or("A4P authorization request"),
        );
        let authorization_object = a4p::util::sorted_value(&json!({
            "signingOptions": request.signing_options,
            "mandate": mandate,
        }));
        let authorization_json =
            html_escape(&serde_json::to_string_pretty(&authorization_object).unwrap_or_default());
        let authorization_id = html_escape(&mandate_identifier(mandate).unwrap_or_default());
        let signing_options =
            html_escape(&serde_json::to_string(&request.signing_options).unwrap_or_default());
        render_template(
            "authorization_card.html",
            &[
                ("authorization_id", &authorization_id),
                ("signing_options", &signing_options),
                ("display_text", &display_text),
                ("authorization_json", &authorization_json),
            ],
        )
    }

    /// Render the registration page for a queued registration.
    pub fn render_registration(&self, registration_request_id: Option<&str>) -> String {
        let registrations = self.registrations.lock();
        let Some(pending) = registration_request_id.and_then(|id| registrations.get(id)) else {
            return Self::render_result("No pending browser key registration");
        };
        render_template(
            "registration.html",
            &[
                (
                    "request_id",
                    &html_escape(registration_request_id.unwrap_or_default()),
                ),
                ("user_id", &html_escape(&pending.user_id)),
                (
                    "options",
                    &html_escape(&serde_json::to_string(&pending.creation_options).unwrap_or_default()),
                ),
            ],
        )
    }

    fn render_empty(message: &str) -> String {
        render_template("empty.html", &[("message", &html_escape(message))])
    }

    fn render_result(message: &str) -> String {
        render_template("result.html", &[("message", &html_escape(message))])
    }

    fn render_not_found() -> String {
        render_template("not_found.html", &[])
    }

    fn render_error(message: &str) -> String {
        render_template("error.html", &[("message", &html_escape(message))])
    }

    /// Serve a public asset with its content type, or `None` when not public.
    pub fn asset_response(name: &str) -> Option<(&'static str, &'static str)> {
        if PUBLIC_ASSETS.contains(&name) {
            Some((asset_content_type(name), asset_text(name)))
        } else {
            None
        }
    }

    /// Build the axum router.
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/assets/{name}", get(serve_asset))
            .route("/", get(index_page))
            .route("/authorize", get(index_page).post(authorize_endpoint))
            .route("/register", get(registration_page).post(register_endpoint))
            .route("/register/complete", post(register_complete_endpoint))
            .route("/approve", post(approve_endpoint))
            .route("/reject", post(reject_endpoint))
            .fallback(not_found)
            .with_state(self)
    }

    /// Bind and serve until the process exits.
    pub async fn serve(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind((self.host.as_str(), self.port())).await?;
        *self.port.lock() = listener.local_addr()?.port();
        println!(
            "[A4P User Authorizer] HTTP server: http://{}:{}",
            self.host,
            self.port()
        );
        println!(
            "[A4P User Authorizer] A4P Server calls are relayed by the Agent; no remote client is configured here."
        );
        let router = self.router();
        axum::serve(listener, router).await?;
        Ok(())
    }
}

type Shared = Arc<BrowserWebA4PUserAuthorizer>;

fn html(status: StatusCode, body: String) -> Response {
    (status, Html(body)).into_response()
}

fn json_body(body: &Bytes) -> Result<JsonDict, String> {
    let text = std::str::from_utf8(body).map_err(|error| format!("Invalid JSON body: {error}"))?;
    let text = if text.trim().is_empty() { "{}" } else { text };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err("JSON body must be an object".into()),
        Err(error) => Err(format!("Invalid JSON body: {error}")),
    }
}

fn json_or_error(result: Result<Value, String>) -> Response {
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(message) => html(
            StatusCode::INTERNAL_SERVER_ERROR,
            BrowserWebA4PUserAuthorizer::render_error(&message),
        ),
    }
}

async fn serve_asset(Path(name): Path<String>) -> Response {
    match BrowserWebA4PUserAuthorizer::asset_response(&name) {
        Some((content_type, text)) => {
            (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], text).into_response()
        }
        None => html(
            StatusCode::NOT_FOUND,
            BrowserWebA4PUserAuthorizer::render_not_found(),
        ),
    }
}

async fn index_page(
    State(authorizer): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let selected = query.get("authorizationId").map(String::as_str);
    html(StatusCode::OK, authorizer.render_index(selected))
}

async fn authorize_endpoint(State(authorizer): State<Shared>, body: Bytes) -> Response {
    match json_body(&body) {
        Ok(payload) => axum::Json(authorizer.authorize(payload).await).into_response(),
        Err(message) => json_or_error(Err(message)),
    }
}

async fn registration_page(
    State(authorizer): State<Shared>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let selected = query.get("requestId").map(String::as_str);
    html(StatusCode::OK, authorizer.render_registration(selected))
}

async fn register_endpoint(State(authorizer): State<Shared>, body: Bytes) -> Response {
    match json_body(&body) {
        Ok(payload) => axum::Json(authorizer.register(payload).await).into_response(),
        Err(message) => json_or_error(Err(message)),
    }
}

async fn register_complete_endpoint(State(authorizer): State<Shared>, body: Bytes) -> Response {
    json_or_error(json_body(&body).map(|payload| authorizer.resolve_registration(&payload)))
}

async fn approve_endpoint(State(authorizer): State<Shared>, body: Bytes) -> Response {
    json_or_error(json_body(&body).map(|payload| authorizer.resolve_json_approval(&payload)))
}

async fn reject_endpoint(State(authorizer): State<Shared>, body: Bytes) -> Response {
    let fields: HashMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    let authorization_id = fields
        .get("authorizationId")
        .map(String::as_str)
        .unwrap_or_default();
    let reason = fields
        .get("reason")
        .map(String::as_str)
        .unwrap_or("Rejected in browser User Authorizer");
    let payload = authorizer.resolve(authorization_id, false, reason, None);
    let message = payload.get("message").and_then(Value::as_str).unwrap_or("Done");
    html(
        StatusCode::OK,
        BrowserWebA4PUserAuthorizer::render_result(message),
    )
}

async fn not_found() -> Response {
    html(
        StatusCode::NOT_FOUND,
        BrowserWebA4PUserAuthorizer::render_not_found(),
    )
}

#[derive(Parser, Debug)]
#[command(about = "Standalone Browser/WebAuthn A4P User Authorizer")]
struct Args {
    /// Print authorization URLs without opening a browser.
    #[arg(long)]
    no_open_browser: bool,
    /// JSON file containing locally trusted A4P Server Ed25519 public keys.
    #[arg(long, default_value = ".a4p/trusted_server_keys.json")]
    trusted_server_keys: String,
    /// Bind host. The WebAuthn RP ID and origin of the demo expect `localhost`.
    #[arg(long, default_value = "localhost")]
    host: String,
    /// Bind port.
    #[arg(long, default_value_t = 8970)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let trust_store = StaticA4PServerTrustStore::from_json_file(&args.trusted_server_keys)?;
    let authorizer = Arc::new(BrowserWebA4PUserAuthorizer::new(
        trust_store,
        &args.host,
        args.port,
        !args.no_open_browser,
    ));
    authorizer.serve().await
}

/// Port of `tests/examples/test_browser_user_authorizer.py`.
#[cfg(test)]
mod tests {
    use ap_support::testing::{OptionExt, TestResult};

    use super::*;
    use a4p::A4PServer;
    use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
    use a4p::operation::mandate::{CreateOperationMandate, create_operation_mandate};

    fn trust_store(server_id: &str) -> TestResult<StaticA4PServerTrustStore> {
        let server = A4PServer::builder()
            .server_id(server_id)
            .require_user_signature(false)
            .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
            .build()?;
        Ok(StaticA4PServerTrustStore::new(&server.server_trust_config()?)?)
    }

    fn webauthn_mandate() -> TestResult<JsonDict> {
        let operation = json!({"action": "delete_note", "params": {"note_id": "note-1"}})
            .as_object()
            .cloned()
            .required()?;
        let policy = json!({"userVerification": "required"})
            .as_object()
            .cloned()
            .required()?;
        Ok(create_operation_mandate(CreateOperationMandate {
            agent_id: "agent-1",
            validity_seconds: 60,
            user_signature_method: Some("webauthn"),
            user_signature_method_policy: Some(&policy),
            ..CreateOperationMandate::new(&operation, "local://test")
        })?)
    }

    fn authorize_payload(mandate: &JsonDict) -> TestResult<JsonDict> {
        Ok(json!({
            "mandate": mandate,
            "signingOptions": {
                "signatureMethod": "webauthn",
                "methodOptions": {"challenge": "agent-substituted-challenge"},
            },
        })
        .as_object()
        .cloned()
        .required()?)
    }

    async fn wait_until(condition: impl Fn() -> bool) -> TestResult {
        for _ in 0..1000 {
            if condition() {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        Err(ap_support::testing::TestFailure::new("condition not met".to_string()).into())
    }

    #[tokio::test]
    async fn standalone_user_authorizer_returns_signed_mandate() -> TestResult {
        let authorizer = Arc::new(BrowserWebA4PUserAuthorizer::new(
            trust_store("local://test")?,
            "localhost",
            8970,
            false,
        ));
        let mandate = webauthn_mandate()?;
        let operation_id = mandate["operationId"].as_str().required()?.to_string();
        let task = {
            let authorizer = authorizer.clone();
            let payload = authorize_payload(&mandate)?;
            tokio::spawn(async move { authorizer.authorize(payload).await })
        };
        wait_until(|| authorizer.has_pending(&operation_id)).await?;
        let assertion = json!({"id": "cred-1", "type": "public-key", "response": {}})
            .as_object()
            .cloned()
            .required()?;
        let resolved = authorizer.resolve(&operation_id, true, "", Some(&assertion));
        let response = task.await?;
        assert_eq!(resolved["ok"], true);
        assert_eq!(response["approved"], true);
        assert_eq!(
            response["signedMandate"]["signatures"]["user"]["signatureMethod"],
            "webauthn"
        );
        assert_eq!(
            response["signedMandate"]["signatures"]["user"]["credentialId"],
            "cred-1"
        );
        assert!(!authorizer.has_pending(&operation_id));

        let duplicate = authorizer.resolve(&operation_id, true, "", Some(&assertion));
        assert_eq!(duplicate["ok"], false);
        let untrusted = authorizer
            .authorize(
                json!({"mandate": {"type": "x"}})
                    .as_object()
                    .cloned()
                    .required()?,
            )
            .await;
        assert_eq!(untrusted["errorCode"], "MANDATE_INVALID");
        let mut tampered = mandate.clone();
        tampered.insert("displayText".into(), json!("tampered"));
        let rejected = authorizer.authorize(authorize_payload(&tampered)?).await;
        assert_eq!(rejected["approved"], false);
        assert_eq!(rejected["errorCode"], "SERVER_SIGNATURE_INVALID");
        Ok(())
    }

    #[tokio::test]
    async fn standalone_user_authorizer_overwrites_options_before_queuing() -> TestResult {
        let authorizer = Arc::new(BrowserWebA4PUserAuthorizer::new(
            trust_store("local://test")?,
            "localhost",
            8970,
            false,
        ));
        let mandate = webauthn_mandate()?;
        let operation_id = mandate["operationId"].as_str().required()?.to_string();
        let task = {
            let authorizer = authorizer.clone();
            let payload = authorize_payload(&mandate)?;
            tokio::spawn(async move { authorizer.authorize(payload).await })
        };
        wait_until(|| authorizer.has_pending(&operation_id)).await?;

        let signing_options = authorizer.pending_signing_options(&operation_id).required()?;
        assert_ne!(
            signing_options["methodOptions"]["challenge"],
            "agent-substituted-challenge"
        );
        assert_eq!(
            signing_options["methodOptions"]["challenge"],
            Value::String(a4p::user_authorization_challenge_base64url(&mandate)?)
        );
        assert_eq!(signing_options["methodOptions"]["userVerification"], "required");

        let page = authorizer.render_index(Some(&operation_id));
        assert!(page.contains("<script src=\"/assets/webauthn.js\"></script>"));
        assert!(page.contains("<script src=\"/assets/authorizer.js\"></script>"));
        assert!(!page.contains("function b64urlToBuffer"));
        assert!(page.contains(&html_escape(&operation_id)));
        let listing = authorizer.render_index(None);
        assert!(listing.contains("data-authorization-id"));
        let missing = authorizer.render_index(Some("op_missing"));
        assert!(missing.contains("No pending authorization: op_missing"));

        let (content_type, text) = BrowserWebA4PUserAuthorizer::asset_response("authorizer.js").required()?;
        assert!(content_type.starts_with("text/javascript"));
        assert!(text.contains("approveWithPasskey"));
        assert!(BrowserWebA4PUserAuthorizer::asset_response("authorizer.html").is_none());

        authorizer.resolve(&operation_id, false, "test", None);
        let response = task.await?;
        assert_eq!(response["approved"], false);
        assert_eq!(response["rejectReason"], "test");
        Ok(())
    }

    #[tokio::test]
    async fn standalone_user_authorizer_returns_registration_credential_to_agent() -> TestResult {
        let authorizer = Arc::new(BrowserWebA4PUserAuthorizer::new(
            trust_store("local://a4p")?,
            "localhost",
            8970,
            false,
        ));
        let task = {
            let authorizer = authorizer.clone();
            let payload = json!({
                "registrationRequestId": "registration-1",
                "userId": "user-1",
                "creationOptions": {"challenge": "registration-challenge"},
            })
            .as_object()
            .cloned()
            .required()?;
            tokio::spawn(async move { authorizer.register(payload).await })
        };
        wait_until(|| authorizer.registration_count() == 1).await?;

        let page = authorizer.render_registration(Some("registration-1"));
        assert!(page.contains("<script src=\"/assets/registration.js\"></script>"));
        assert!(!page.contains("navigator.credentials.create"));
        assert!(page.contains("registration-challenge"));

        let resolved = authorizer.resolve_registration(
            &json!({
                "registrationRequestId": "registration-1",
                "credential": {"id": "cred-1", "type": "public-key", "response": {}},
            })
            .as_object()
            .cloned()
            .required()?,
        );
        let response = task.await?;
        assert_eq!(resolved["ok"], true);
        assert_eq!(response["userId"], "user-1");
        assert_eq!(response["credential"]["id"], "cred-1");
        assert_eq!(authorizer.registration_count(), 0);

        let missing = authorizer
            .register(json!({"userId": "user-1"}).as_object().cloned().required()?)
            .await;
        assert_eq!(missing["message"], "registrationRequestId missing");
        Ok(())
    }

    #[tokio::test]
    async fn http_routes_serve_assets_and_pages() -> TestResult {
        let authorizer = Arc::new(BrowserWebA4PUserAuthorizer::new(
            trust_store("local://test")?,
            "127.0.0.1",
            0,
            false,
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let router = authorizer.clone().router();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let client = reqwest::Client::new();
        let base = format!("http://127.0.0.1:{port}");

        let asset = client.get(format!("{base}/assets/webauthn.js")).send().await?;
        assert_eq!(asset.status(), 200);
        assert!(asset.text().await?.contains("function b64urlToBuffer"));
        let hidden = client
            .get(format!("{base}/assets/authorizer.html"))
            .send()
            .await?;
        assert_eq!(hidden.status(), 404);
        let index = client.get(format!("{base}/")).send().await?;
        assert_eq!(index.status(), 200);
        assert!(
            index
                .text()
                .await?
                .contains("No pending A4P authorization requests.")
        );
        let approve = client
            .post(format!("{base}/approve"))
            .json(&json!({"authorizationId": "op_x", "assertion": {"id": "c"}}))
            .send()
            .await?;
        let body: Value = approve.json().await?;
        assert_eq!(body["ok"], false);
        let reject = client
            .post(format!("{base}/reject"))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body("authorizationId=op_x&reason=nope")
            .send()
            .await?;
        assert!(reject.text().await?.contains("No pending authorization: op_x"));
        let unknown = client.get(format!("{base}/nowhere")).send().await?;
        assert_eq!(unknown.status(), 404);
        server.abort();
        Ok(())
    }

    #[test]
    fn template_substitution_matches_python_template() -> TestResult {
        assert_eq!(
            html_escape("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#x27;&amp;&#x27;&lt;/a&gt;"
        );
        let rendered = render_template("empty.html", &[("message", "hi")]);
        assert_eq!(rendered.trim(), "<section class=\"empty\">hi</section>");
        Ok(())
    }
}
