// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Authentication and authorization hooks, port of `mcp_auth.h`.
//!
//! Header maps use lowercase header names, matching the C++ HTTP layer.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use parking_lot::RwLock;

use crate::error::McpError;
use crate::protocol::headers::AUTHORIZATION;

/// Attaches authentication information to outgoing HTTP requests.
pub trait AuthProvider: Send + Sync {
    /// Add authentication headers before a request is sent.
    fn apply(&self, headers: &mut HashMap<String, String>);
}

/// A bearer token provider (`Mcp::BearerTokenProvider`).
#[derive(Debug, Default)]
pub struct BearerTokenProvider {
    token: RwLock<String>,
}

impl BearerTokenProvider {
    /// Create a provider with the given token. An empty token adds no header.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: RwLock::new(token.into()),
        }
    }

    /// Replace the token.
    pub fn set_token(&self, token: impl Into<String>) {
        *self.token.write() = token.into();
    }

    /// The current token.
    pub fn token(&self) -> String {
        self.token.read().clone()
    }
}

impl AuthProvider for BearerTokenProvider {
    fn apply(&self, headers: &mut HashMap<String, String>) {
        let token = self.token.read();
        if token.is_empty() {
            return;
        }
        // Defend against header injection through CR or LF in the token.
        if token.contains('\r') || token.contains('\n') {
            return;
        }
        headers.insert(AUTHORIZATION.to_string(), format!("Bearer {}", *token));
    }
}

/// Context produced by a successful authentication.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthContext {
    /// Client identifier.
    pub client_id: Option<String>,
    /// Space separated authorization scopes.
    pub scopes: Option<String>,
}

/// Result of an authentication attempt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AuthenticationResult {
    /// True when the request is authenticated.
    pub authenticated: bool,
    /// Present when authenticated.
    pub context: Option<AuthContext>,
    /// Present when authentication failed.
    pub error_description: Option<String>,
}

impl AuthenticationResult {
    fn failure(description: &str) -> Self {
        Self {
            authenticated: false,
            context: None,
            error_description: Some(description.to_string()),
        }
    }
}

/// Verifies bearer tokens, for example by introspection or JWT validation.
pub trait TokenVerifier: Send + Sync {
    /// Verify a token and describe the caller.
    fn verify_token(&self, token: &str) -> AuthenticationResult;
}

/// Authenticates an HTTP request from its headers.
pub trait Authenticator: Send + Sync {
    /// Authenticate the request. Header names are lowercase.
    fn authenticate(&self, headers: &HashMap<String, String>) -> AuthenticationResult;
}

/// An authenticator that accepts every request.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoAuthAuthenticator;

impl Authenticator for NoAuthAuthenticator {
    fn authenticate(&self, _headers: &HashMap<String, String>) -> AuthenticationResult {
        AuthenticationResult {
            authenticated: true,
            context: Some(AuthContext::default()),
            error_description: None,
        }
    }
}

/// Bearer token authenticator delegating to a [`TokenVerifier`].
pub struct BearerTokenAuthenticator {
    verifier: Option<Arc<dyn TokenVerifier>>,
}

impl BearerTokenAuthenticator {
    /// Create an authenticator. A missing verifier fails every request.
    pub fn new(verifier: Option<Arc<dyn TokenVerifier>>) -> Self {
        Self { verifier }
    }
}

impl Authenticator for BearerTokenAuthenticator {
    fn authenticate(&self, headers: &HashMap<String, String>) -> AuthenticationResult {
        let Some(verifier) = &self.verifier else {
            return AuthenticationResult::failure("Token verifier not configured");
        };
        let Some(value) = headers.get(AUTHORIZATION) else {
            return AuthenticationResult::failure("Missing Authorization header");
        };
        const PREFIX: &str = "bearer ";
        if value.len() < PREFIX.len() || !value[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
            return AuthenticationResult::failure("Authorization header must use Bearer scheme");
        }
        let token = value[PREFIX.len()..].trim_start_matches([' ', '\t']);
        if token.is_empty() {
            return AuthenticationResult::failure("Bearer token is empty");
        }
        verifier.verify_token(token)
    }
}

/// Decides whether an authenticated caller may proceed.
pub trait Authorizer: Send + Sync {
    /// Return true when the caller is authorized.
    fn authorize(&self, auth_result: &AuthenticationResult) -> bool;
}

fn split_scopes(scopes: &str) -> HashSet<String> {
    scopes.split_whitespace().map(|s| s.to_string()).collect()
}

/// Authorizer that requires every configured scope to be present.
#[derive(Debug)]
pub struct ScopeBasedAuthorizer {
    required: RwLock<HashSet<String>>,
}

impl ScopeBasedAuthorizer {
    /// Create an authorizer from a space separated scope list.
    ///
    /// Returns [`McpError::InvalidArgument`] when the list is empty.
    pub fn new(required_scopes: &str) -> Result<Self, McpError> {
        let scopes = split_scopes(required_scopes);
        if scopes.is_empty() {
            return Err(McpError::argument("requiredScopes must not be empty"));
        }
        Ok(Self {
            required: RwLock::new(scopes),
        })
    }

    /// Replace the required scopes. Fails when the list is empty.
    pub fn set_required_scopes(&self, scopes: &str) -> Result<(), McpError> {
        let parsed = split_scopes(scopes);
        if parsed.is_empty() {
            return Err(McpError::argument("requiredScopes must not be empty"));
        }
        *self.required.write() = parsed;
        Ok(())
    }

    /// The required scopes as a space separated string.
    pub fn required_scopes(&self) -> String {
        let mut list: Vec<String> = self.required.read().iter().cloned().collect();
        list.sort();
        list.join(" ")
    }
}

impl Authorizer for ScopeBasedAuthorizer {
    fn authorize(&self, auth_result: &AuthenticationResult) -> bool {
        if !auth_result.authenticated {
            return false;
        }
        let Some(ctx) = &auth_result.context else {
            return false;
        };
        let Some(scopes) = &ctx.scopes else {
            return false;
        };
        let provided = split_scopes(scopes);
        if provided.is_empty() {
            return false;
        }
        self.required.read().iter().all(|s| provided.contains(s))
    }
}

/// Token verifier backed by a static token to scopes map. Mirrors the
/// `SimpleTokenVerifier` shipped with the C++ server example.
#[derive(Debug, Default)]
pub struct SimpleTokenVerifier {
    tokens: RwLock<HashMap<String, String>>,
}

impl SimpleTokenVerifier {
    /// Create a verifier from a token to scopes map.
    pub fn new(token_scopes: HashMap<String, String>) -> Self {
        Self {
            tokens: RwLock::new(token_scopes),
        }
    }

    /// Add or replace a token.
    pub fn add_token(&self, token: impl Into<String>, scopes: impl Into<String>) {
        self.tokens.write().insert(token.into(), scopes.into());
    }

    /// Remove a token.
    pub fn remove_token(&self, token: &str) {
        self.tokens.write().remove(token);
    }
}

impl TokenVerifier for SimpleTokenVerifier {
    fn verify_token(&self, token: &str) -> AuthenticationResult {
        if token.is_empty() {
            return AuthenticationResult::failure("Token is empty");
        }
        let tokens = self.tokens.read();
        let Some(scopes) = tokens.get(token) else {
            return AuthenticationResult::failure("Invalid token");
        };
        AuthenticationResult {
            authenticated: true,
            context: Some(AuthContext {
                client_id: Some(format!("client-{token}")),
                scopes: Some(scopes.clone()),
            }),
            error_description: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ap_support::testing::{OptionExt, TestResult};
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingVerifier {
        result: AuthenticationResult,
        calls: AtomicUsize,
        last_token: Mutex<String>,
    }

    impl RecordingVerifier {
        fn new(result: AuthenticationResult) -> Arc<Self> {
            Arc::new(Self {
                result,
                calls: AtomicUsize::new(0),
                last_token: Mutex::new(String::new()),
            })
        }
    }

    impl TokenVerifier for RecordingVerifier {
        fn verify_token(&self, token: &str) -> AuthenticationResult {
            self.calls.fetch_add(1, Ordering::SeqCst);
            *self.last_token.lock() = token.to_string();
            self.result.clone()
        }
    }

    #[test]
    fn bearer_provider_skips_empty_token() -> TestResult {
        let mut headers = HashMap::new();
        BearerTokenProvider::default().apply(&mut headers);
        assert!(headers.is_empty());
        Ok(())
    }

    #[test]
    fn bearer_provider_sets_authorization_header() -> TestResult {
        let mut headers = HashMap::new();
        let provider = BearerTokenProvider::new("abc123");
        provider.apply(&mut headers);
        assert_eq!(headers.get(AUTHORIZATION).required()?, "Bearer abc123");
        provider.set_token("new-token");
        assert_eq!(provider.token(), "new-token");
        provider.apply(&mut headers);
        assert_eq!(headers.get(AUTHORIZATION).required()?, "Bearer new-token");
        provider.set_token("bad\r\ntoken");
        let mut fresh = HashMap::new();
        provider.apply(&mut fresh);
        assert!(fresh.is_empty());
        Ok(())
    }

    #[test]
    fn no_auth_always_authenticated_with_context() -> TestResult {
        let result = NoAuthAuthenticator.authenticate(&HashMap::new());
        assert!(result.authenticated);
        assert!(result.context.is_some());
        Ok(())
    }

    #[test]
    fn bearer_authenticator_failures() -> TestResult {
        let missing = BearerTokenAuthenticator::new(None).authenticate(&HashMap::new());
        assert!(!missing.authenticated);
        assert_eq!(
            missing.error_description.as_deref(),
            Some("Token verifier not configured")
        );

        let verifier = RecordingVerifier::new(AuthenticationResult::default());
        let auth = BearerTokenAuthenticator::new(Some(verifier.clone()));
        let r = auth.authenticate(&HashMap::new());
        assert_eq!(
            r.error_description.as_deref(),
            Some("Missing Authorization header")
        );

        let mut headers = HashMap::new();
        headers.insert(AUTHORIZATION.to_string(), "Basic abc".to_string());
        let r = auth.authenticate(&headers);
        assert_eq!(
            r.error_description.as_deref(),
            Some("Authorization header must use Bearer scheme")
        );

        headers.insert(AUTHORIZATION.to_string(), "Bearer ".to_string());
        let r = auth.authenticate(&headers);
        assert_eq!(r.error_description.as_deref(), Some("Bearer token is empty"));
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn bearer_authenticator_forwards_token_case_insensitively() -> TestResult {
        let ok = AuthenticationResult {
            authenticated: true,
            context: Some(AuthContext {
                client_id: Some("client-1".into()),
                scopes: Some("scope:a scope:b".into()),
            }),
            error_description: None,
        };
        let verifier = RecordingVerifier::new(ok);
        let auth = BearerTokenAuthenticator::new(Some(verifier.clone()));
        let mut headers = HashMap::new();
        headers.insert(AUTHORIZATION.to_string(), "Bearer live-token".to_string());
        let r = auth.authenticate(&headers);
        assert!(r.authenticated);
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
        assert_eq!(*verifier.last_token.lock(), "live-token");
        assert_eq!(r.context.required()?.client_id.as_deref(), Some("client-1"));

        headers.insert(AUTHORIZATION.to_string(), "bEaReR MiXeD".to_string());
        let r = auth.authenticate(&headers);
        assert!(r.authenticated);
        assert_eq!(*verifier.last_token.lock(), "MiXeD");
        Ok(())
    }

    #[test]
    fn scope_authorizer_rules() -> TestResult {
        assert!(ScopeBasedAuthorizer::new("").is_err());
        assert!(ScopeBasedAuthorizer::new("  \t ").is_err());
        let authorizer = ScopeBasedAuthorizer::new("read write")?;
        assert!(!authorizer.authorize(&AuthenticationResult::default()));
        let missing_ctx = AuthenticationResult {
            authenticated: true,
            ..Default::default()
        };
        assert!(!authorizer.authorize(&missing_ctx));
        let mut ok = AuthenticationResult {
            authenticated: true,
            context: Some(AuthContext {
                client_id: None,
                scopes: Some("read write delete".into()),
            }),
            error_description: None,
        };
        assert!(authorizer.authorize(&ok));
        ok.context.as_mut().required()?.scopes = Some("read".into());
        assert!(!authorizer.authorize(&ok));
        ok.context.as_mut().required()?.scopes = Some(String::new());
        assert!(!authorizer.authorize(&ok));
        Ok(())
    }

    #[test]
    fn scope_authorizer_updates_required_scopes() -> TestResult {
        let authorizer = ScopeBasedAuthorizer::new("read")?;
        authorizer.set_required_scopes("admin manage")?;
        assert_eq!(authorizer.required_scopes(), "admin manage");
        assert!(authorizer.set_required_scopes(" ").is_err());
        assert_eq!(authorizer.required_scopes(), "admin manage");
        Ok(())
    }

    #[test]
    fn simple_token_verifier() -> TestResult {
        let verifier = SimpleTokenVerifier::default();
        verifier.add_token("t1", "read write");
        assert_eq!(
            verifier.verify_token("").error_description.as_deref(),
            Some("Token is empty")
        );
        assert_eq!(
            verifier.verify_token("nope").error_description.as_deref(),
            Some("Invalid token")
        );
        let ok = verifier.verify_token("t1");
        assert!(ok.authenticated);
        assert_eq!(ok.context.required()?.scopes.as_deref(), Some("read write"));
        verifier.remove_token("t1");
        assert!(!verifier.verify_token("t1").authenticated);
        Ok(())
    }
}
