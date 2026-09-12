// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Public A4P Server facade.

use std::sync::Arc;

use serde_json::Value;

use crate::errors::{A4PError, A4PProtocolError};
use crate::intent::mandate::IntentDisplayTextRenderer;
use crate::intent::service::IntentAuthorizationService;
use crate::intent::signing::intent_server_trusted_key;
use crate::intent::usage_store::A4PIntentTokenUsageStore;
use crate::operation::mandate::OperationDisplayTextRenderer;
use crate::operation::service::OperationAuthorizationService;
use crate::operation::signing::operation_server_trusted_key;
use crate::types::{
    IntentAuthorizationResponse, IntoPayload, JsonDict, OperationAuthorizationChallenge,
    OperationAuthorizationResult, TokenVerificationResponse,
};
use crate::user_signature::A4PUserSignatureMethod;
use crate::user_signature::ed25519::ED25519_SIGNATURE_METHOD;
use crate::user_signature::webauthn::WEBAUTHN_SIGNATURE_METHOD;
use crate::util::{is_truthy, py_str, str_or_empty, str_trimmed};

/// Default server id.
pub const DEFAULT_SERVER_ID: &str = "local://a4p";

/// Builder for [`A4PServer`], mirroring the Python constructor keyword arguments.
#[derive(Default)]
pub struct A4PServerBuilder {
    server_id: Option<String>,
    user_signature_method: Option<Arc<dyn A4PUserSignatureMethod>>,
    intent_display_text_renderer: Option<IntentDisplayTextRenderer>,
    operation_display_text_renderer: Option<OperationDisplayTextRenderer>,
    require_user_signature: Option<bool>,
    intent_token_usage_store: Option<Arc<dyn A4PIntentTokenUsageStore>>,
}

impl A4PServerBuilder {
    /// Set the server id written into mandates (default `local://a4p`).
    pub fn server_id(mut self, server_id: impl Into<String>) -> Self {
        self.server_id = Some(server_id.into());
        self
    }

    /// Set the single user signature method of this instance.
    pub fn user_signature_method(mut self, method: Arc<dyn A4PUserSignatureMethod>) -> Self {
        self.user_signature_method = Some(method);
        self
    }

    /// Set a custom intent display text renderer.
    pub fn intent_display_text_renderer(mut self, renderer: IntentDisplayTextRenderer) -> Self {
        self.intent_display_text_renderer = Some(renderer);
        self
    }

    /// Set a custom operation display text renderer.
    pub fn operation_display_text_renderer(mut self, renderer: OperationDisplayTextRenderer) -> Self {
        self.operation_display_text_renderer = Some(renderer);
        self
    }

    /// Require user signatures (default true). When false, `signatures.user` must be empty.
    pub fn require_user_signature(mut self, required: bool) -> Self {
        self.require_user_signature = Some(required);
        self
    }

    /// Set the intent token usage store (default SQLite at the configured path).
    pub fn intent_token_usage_store(mut self, store: Arc<dyn A4PIntentTokenUsageStore>) -> Self {
        self.intent_token_usage_store = Some(store);
        self
    }

    /// Build the server.
    pub fn build(self) -> Result<A4PServer, A4PError> {
        let require_user_signature = self.require_user_signature.unwrap_or(true);
        if require_user_signature && self.user_signature_method.is_none() {
            return Err(A4PError::value(
                "user_signature_method is required when user signatures are enabled",
            ));
        }
        if let Some(method) = &self.user_signature_method {
            let name = method.signature_method();
            if name.is_empty() || name != name.trim().to_lowercase() {
                return Err(A4PError::value(
                    "user_signature_method.signature_method must be a non-empty lowercase identifier",
                ));
            }
        }
        let server_id = self.server_id.unwrap_or_else(|| DEFAULT_SERVER_ID.to_string());
        let usage_store: Arc<dyn A4PIntentTokenUsageStore> = match self.intent_token_usage_store {
            Some(store) => store,
            #[cfg(feature = "sqlite")]
            None => Arc::new(crate::intent::usage_store::SQLiteIntentTokenUsageStore::new(
                None,
            )?),
            #[cfg(not(feature = "sqlite"))]
            None => Arc::new(crate::intent::usage_store::InMemoryIntentTokenUsageStore::new()),
        };
        let intent = IntentAuthorizationService::new(
            server_id.clone(),
            self.intent_display_text_renderer.clone(),
            require_user_signature,
            self.user_signature_method.clone(),
            usage_store.clone(),
        );
        let operation = OperationAuthorizationService::new(
            server_id.clone(),
            self.operation_display_text_renderer.clone(),
            require_user_signature,
            self.user_signature_method.clone(),
        );
        Ok(A4PServer {
            server_id,
            user_signature_method: self.user_signature_method,
            require_user_signature,
            intent_token_usage_store: usage_store,
            intent,
            operation,
        })
    }
}

/// Facade configured with exactly one user-signature method.
pub struct A4PServer {
    server_id: String,
    user_signature_method: Option<Arc<dyn A4PUserSignatureMethod>>,
    require_user_signature: bool,
    intent_token_usage_store: Arc<dyn A4PIntentTokenUsageStore>,
    intent: IntentAuthorizationService,
    operation: OperationAuthorizationService,
}

impl A4PServer {
    /// Start building a server.
    pub fn builder() -> A4PServerBuilder {
        A4PServerBuilder::default()
    }

    /// The configured server id.
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// Whether user signatures are required.
    pub fn require_user_signature(&self) -> bool {
        self.require_user_signature
    }

    /// The configured signature method identifier, if any.
    pub fn signature_method(&self) -> Option<&str> {
        self.user_signature_method
            .as_ref()
            .map(|method| method.signature_method())
    }

    /// The configured user signature method, if any.
    pub fn user_signature_method(&self) -> Option<&Arc<dyn A4PUserSignatureMethod>> {
        self.user_signature_method.as_ref()
    }

    /// The intent token usage store.
    pub fn intent_token_usage_store(&self) -> &Arc<dyn A4PIntentTokenUsageStore> {
        &self.intent_token_usage_store
    }

    /// The intent authorization service.
    pub fn intent_service(&self) -> &IntentAuthorizationService {
        &self.intent
    }

    /// The operation authorization service.
    pub fn operation_service(&self) -> &OperationAuthorizationService {
        &self.operation
    }

    fn require_signature_method(&self, requested: &str) -> Result<(), A4PError> {
        if self.signature_method() != Some(requested) {
            return Err(
                A4PProtocolError::signature_method_not_enabled(self.signature_method(), requested).into(),
            );
        }
        Ok(())
    }

    /// Return `{serverId: {keyId: {alg, publicKey}}}` for local trust stores.
    pub fn server_trust_config(&self) -> Result<JsonDict, A4PError> {
        let intent_key = intent_server_trusted_key()?;
        let operation_key = operation_server_trusted_key()?;
        let mut keys = JsonDict::new();
        for key in [intent_key, operation_key] {
            let mut entry = JsonDict::new();
            entry.insert("alg".into(), key.get("alg").cloned().unwrap_or(Value::Null));
            entry.insert(
                "publicKey".into(),
                key.get("publicKey").cloned().unwrap_or(Value::Null),
            );
            keys.insert(str_or_empty(&key, "keyId"), Value::Object(entry));
        }
        let mut config = JsonDict::new();
        config.insert(self.server_id.clone(), Value::Object(keys));
        Ok(config)
    }

    /// Register an Ed25519 OKP JWK for a user (`/user-credentials/ed25519/register`).
    pub fn register_ed25519_credential(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        self.require_signature_method(ED25519_SIGNATURE_METHOD)?;
        let registrar = self
            .user_signature_method
            .as_ref()
            .and_then(|method| method.ed25519_registrar())
            .ok_or_else(|| A4PError::value("Configured ed25519 method does not support registration"))?;
        registrar.register(request)
    }

    /// Create WebAuthn creation options (`/user-credentials/webauthn/register/options`).
    pub fn webauthn_registration_options(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        self.require_signature_method(WEBAUTHN_SIGNATURE_METHOD)?;
        let registrar = self
            .user_signature_method
            .as_ref()
            .and_then(|method| method.webauthn_registrar())
            .ok_or_else(|| A4PError::value("Configured webauthn method does not support registration"))?;
        let user_id = str_trimmed(request, "userId");
        if user_id.is_empty() {
            return Err(A4PError::value("userId missing"));
        }
        let user_name = match request.get("userName").filter(|value| is_truthy(value)) {
            Some(value) => py_str(value),
            None => user_id.clone(),
        };
        let user_display_name = match request.get("userDisplayName").filter(|value| is_truthy(value)) {
            Some(value) => py_str(value),
            None => user_name.clone(),
        };
        registrar.registration_options(&user_id, Some(&user_name), Some(&user_display_name))
    }

    /// Verify and store a WebAuthn registration (`/user-credentials/webauthn/register/verify`).
    pub fn verify_webauthn_registration(&self, request: &JsonDict) -> Result<JsonDict, A4PError> {
        self.require_signature_method(WEBAUTHN_SIGNATURE_METHOD)?;
        let registrar = self
            .user_signature_method
            .as_ref()
            .and_then(|method| method.webauthn_registrar())
            .ok_or_else(|| A4PError::value("Configured webauthn method does not support registration"))?;
        let record = registrar.verify_registration(request)?;
        let mut response = JsonDict::new();
        response.insert("registered".into(), Value::Bool(true));
        response.insert("created".into(), Value::Bool(true));
        response.insert("credential".into(), Value::Object(record.to_json()));
        Ok(response)
    }

    /// Prepare an intent authorization.
    pub async fn prepare_intent_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<IntentAuthorizationResponse, A4PError> {
        self.intent.prepare(request)
    }

    /// Complete an intent authorization and issue a token.
    pub async fn complete_intent_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<IntentAuthorizationResponse, A4PError> {
        self.intent.complete(request)
    }

    /// Verify an intent token and consume execution usage when a policy exists.
    pub async fn verify_intent_token(
        &self,
        request: impl IntoPayload,
    ) -> Result<TokenVerificationResponse, A4PError> {
        self.intent.verify_token(request)
    }

    /// Prepare a one-time operation authorization.
    pub async fn prepare_operation_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<OperationAuthorizationChallenge, A4PError> {
        self.operation.prepare(request)
    }

    /// Complete and consume a one-time operation authorization.
    pub async fn complete_operation_authorization(
        &self,
        request: impl IntoPayload,
    ) -> Result<OperationAuthorizationResult, A4PError> {
        self.operation.complete(request)
    }
}
