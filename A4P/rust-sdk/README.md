# a4p

Rust port of the A4P (Agentic Authentication, Authorization and Audit Protocol) Python SDK
from [openJiuwen-ai/agent-protocol](https://github.com/openJiuwen-ai/agent-protocol) (`A4P/`).

A4P puts a user authorization boundary in front of sensitive agent actions:

- **Operation Authorization** approves exactly one `action + params` and is consumed on `complete`.
- **Intent Authorization** approves a set of actions with parameter constraints and issues a
  reusable, Server-signed intent token with an optional atomic `maxExecutions` quota.

The A4P Server signs every mandate and token with Ed25519. The local User Authorizer verifies
the Server signature against a static trust store, shows the verified content, and lets the
user approve with a registered Ed25519 key or a WebAuthn passkey. Wire objects are byte for
byte compatible with the Python SDK: a Rust server verifies mandates signed by the Python
server and vice versa.

## Quick start

```rust
use std::sync::Arc;

use a4p::credential_store::InMemoryCredentialStore;
use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
use a4p::security::generate_ed25519_private_key;
use a4p::user_signature::ed25519::{ed25519_public_jwk, Ed25519UserSigner, RegisteredEd25519Method};
use a4p::{sign_user_mandate_with_signer, A4PServer};
use serde_json::json;

# async fn demo() -> Result<(), a4p::A4PError> {
// One signature method per server instance.
let method = Arc::new(RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new())));
let server = A4PServer::builder()
    .server_id("local://demo")
    .user_signature_method(method)
    .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
    .build()?;

// Enroll the user's Ed25519 public key (the private key stays with the caller).
let private_key = generate_ed25519_private_key();
let registration = server.register_ed25519_credential(
    json!({"userId": "user-1", "publicKey": ed25519_public_jwk(&private_key)}).as_object().unwrap(),
)?;
let credential_id = registration["credential"]["credentialId"].as_str().unwrap();
let signer = Ed25519UserSigner::new(credential_id, private_key)?;

// Operation authorization: prepare, user signs, complete.
let operation = json!({"action": "delete_note", "params": {"note_id": "note-1"}});
let prepared = server
    .prepare_operation_authorization(json!({
        "agentId": "agent-1", "userId": "user-1", "operation": operation, "validitySeconds": 60,
    }))
    .await?;
let signed = sign_user_mandate_with_signer(&prepared.mandate.unwrap(), &signer, None)?;
let completed = server
    .complete_operation_authorization(json!({"signedMandate": signed, "operation": operation}))
    .await?;
assert!(completed.approved);
# Ok(()) }
```

Serve the same facade over HTTP with `A4PHTTPServer` and call it with `A4PClient`:

```rust
# async fn demo(server: a4p::A4PServer) -> Result<(), a4p::A4PError> {
use std::sync::Arc;
let http = a4p::A4PHTTPServer::new(Arc::new(server), Some("127.0.0.1"), Some(0))?;
http.start().await?;
let client = a4p::A4PClient::with_base_url(&format!("http://127.0.0.1:{}", http.port()));
let _ = client.prepare_intent_authorization(serde_json::json!({})).await?;
http.stop().await;
# Ok(()) }
```

## Module map

| Module | Covers |
| --- | --- |
| `types` | `JsonDict` wire objects, request and response structs, `to_payload`, `IntoPayload` |
| `errors` | `A4PError`, `A4PProtocolError` (stable code + HTTP status), `MandateSecurityError` |
| `canonical` | Python compatible canonical JSON (sorted keys, no whitespace, UTF-8, Python float `repr`) |
| `security` | Ed25519 key loading from the environment, signing, verification, development key warning |
| `mandate_security` | Static trust store, challenge derivation, local Server signature and validity checks |
| `credential_store` | `A4PCredentialStore` trait, `InMemoryCredentialStore`, `JsonFileCredentialStore` (schema v2) |
| `user_signature` | `A4PUserSigner` and `A4PUserSignatureMethod` traits, Ed25519 and WebAuthn implementations |
| `intent` | Intent mandate, scope matching, token issue and verify, usage stores, `IntentAuthorizationService` |
| `operation` | Operation mandate and `OperationAuthorizationService` |
| `user_authorizer` | Local processing order, signing helpers, `A4PUserAuthorizer` trait and test authorizers |
| `server` | `A4PServer` facade and builder |
| `http_server` | axum transport with the eight JSON POST endpoints |
| `client` | reqwest client with the Python environment defaults |

## API mapping (Python to Rust)

| Python | Rust |
| --- | --- |
| `A4PServer(server_id=..., user_signature_method=..., require_user_signature=..., intent_token_usage_store=..., *_display_text_renderer=...)` | `A4PServer::builder().server_id(..).user_signature_method(..).require_user_signature(..).intent_token_usage_store(..).intent_display_text_renderer(..).operation_display_text_renderer(..).build()` |
| `A4PServer.prepare_intent_authorization(request)` | `A4PServer::prepare_intent_authorization(impl IntoPayload).await` |
| `A4PServer.complete_intent_authorization(request)` | `A4PServer::complete_intent_authorization(..).await` |
| `A4PServer.verify_intent_token(request)` | `A4PServer::verify_intent_token(..).await` |
| `A4PServer.prepare_operation_authorization(request)` | `A4PServer::prepare_operation_authorization(..).await` |
| `A4PServer.complete_operation_authorization(request)` | `A4PServer::complete_operation_authorization(..).await` |
| `A4PServer.register_ed25519_credential(request)` | `A4PServer::register_ed25519_credential(&JsonDict)` |
| `A4PServer.webauthn_registration_options(request)` | `A4PServer::webauthn_registration_options(&JsonDict)` |
| `A4PServer.verify_webauthn_registration(request)` | `A4PServer::verify_webauthn_registration(&JsonDict)` |
| `A4PServer.server_trust_config()` | `A4PServer::server_trust_config()` |
| `A4PServer.signature_method` | `A4PServer::signature_method()` |
| `server._intent._pending` / `server._operation._pending` | `intent_service().pending_len()`, `operation_service().has_pending()`, `set_pending_expiry()` |
| `A4PClient(base_url=, timeout=)` | `A4PClient::new()`, `A4PClient::with_base_url(url)`, `A4PClient::with_options(base_url, timeout)` |
| `A4PClient.<endpoint method>` | same method names, `async`, returning `Result<_, A4PError>` |
| `default_a4p_base_url()`, `default_a4p_timeout()` | `client::default_a4p_base_url()`, `client::default_a4p_timeout()` |
| `A4PHTTPServer(a4p_server, host=, port=)`, `start()`, `stop()`, `_dispatch()` | `A4PHTTPServer::new(Arc<A4PServer>, host, port)`, `start().await`, `stop().await`, `dispatch(path, payload).await`, `router()` |
| `a4p_http_host()`, `a4p_http_port()` | `http_server::a4p_http_host()`, `http_server::a4p_http_port()` |
| `IntentMandate`, `OperationMandate`, `IntentToken` (TypedDict) | `JsonDict` type aliases `IntentMandate`, `OperationMandate`, `IntentToken` |
| `VerificationResult`, `*Request`, `*Response`, `*Challenge`, `*Result` dataclasses | same names, `serde` camelCase structs; `Option` fields are omitted when `None` |
| `to_payload(value)` | `types::to_payload(&value)` |
| `A4PProtocolError`, `SignatureMethodNotEnabledError`, `CredentialKeyConflictError`, `UserCredentialNotRegisteredError` | `A4PProtocolError` with constructors `signature_method_not_enabled`, `credential_key_conflict`, `user_credential_not_registered` |
| `ValueError` / `RuntimeError` | `A4PError::Value(String)` / `A4PError::Runtime(String)` |
| `MandateSecurityError(code, message)` | `MandateSecurityError { code, message }` |
| `canonical_json(payload)` | `canonical_json(&JsonDict)`, `canonical::canonical_json_value(&Value)` |
| `StaticA4PServerTrustStore(config)`, `.from_json_file(path)`, `.resolve()` | `StaticA4PServerTrustStore::new(&JsonDict)`, `from_json_file(path)`, `resolve(server_id, key_id, alg)` |
| `verify_trusted_server_mandate(mandate, trust_store)` | `verify_trusted_server_mandate(&mandate, &trust_store)` |
| `verify_mandate_valid_time(core)` | `mandate_security::verify_mandate_valid_time(&core)` and `verify_mandate_valid_time_at(&core, now)` |
| `derive_user_authorization_challenge(mandate)` | `derive_user_authorization_challenge(&mandate) -> [u8; 32]` |
| `user_authorization_challenge_base64url(mandate)` | `user_authorization_challenge_base64url(&mandate)` |
| `mandate_identifier(mandate)` | `mandate_identifier(&mandate)` |
| `verify_local_user_authorization_request(request, trust_store=, expected_signature_method=)` | `verify_local_user_authorization_request(&request, &trust_store, expected_method)` |
| `sign_user_mandate_with_signer(mandate, user_signer=, signing_input=)` | `sign_user_mandate_with_signer(&mandate, &dyn A4PUserSigner, signing_input)` |
| `approve_user_mandate(mandate)` | `approve_user_mandate(&mandate)` |
| `A4PUserAuthorizer.authorize()` | `#[async_trait] A4PUserAuthorizer::authorize()` |
| `ApprovingA4PUserAuthorizer`, `RejectingA4PUserAuthorizer` | same names |
| `A4PUserSigner`, `A4PUserSignatureMethod` (Protocols) | traits of the same names; `UserSignatureContext` struct |
| `attach_user_signature`, `sign_user_signature`, `verify_user_signature` | same names in `user_signature` |
| `RegisteredEd25519Method(store)`, `.register()` | `RegisteredEd25519Method::new(Arc<dyn A4PCredentialStore>)`, `register(&JsonDict)` |
| `Ed25519UserSigner(credential_id=, private_key=)` | `Ed25519UserSigner::new(credential_id, SigningKey)` |
| `ed25519_public_jwk(private_key)` | `ed25519_public_jwk(&SigningKey)` |
| `WebAuthnSignatureMethod(store, rp_id=, rp_name=, expected_origin=)` | `WebAuthnSignatureMethod::new(store).with_rp_id(..).with_rp_name(..).with_expected_origin(..)` |
| `.registration_options(user_id=, user_name=, user_display_name=)` | `registration_options(user_id, user_name, user_display_name)` |
| `.verify_registration(payload)` | `verify_registration(&JsonDict) -> UserCredentialRecord` |
| `webauthn.verify_registration_response` / `verify_authentication_response` (py_webauthn) | native `webauthn::verify_registration_response` / `verify_authentication_response` |
| `WebAuthnUserSigner()` | `WebAuthnUserSigner::new()` |
| `b64url_encode`, `b64url_decode` | `webauthn::b64url_encode`, `webauthn::b64url_decode` |
| `UserCredentialRecord(...)`, `asdict(record)` | `UserCredentialRecord { .. }`, `UserCredentialRecord::to_json()` |
| `InMemoryCredentialStore(records)` | `InMemoryCredentialStore::new()`, `with_records(vec)` |
| `JsonFileCredentialStore(path)` | `JsonFileCredentialStore::new(path)` |
| `A4PIntentTokenUsageStore.consume(token_id=, max_executions=, expire_at_epoch=)` | `A4PIntentTokenUsageStore::consume(token_id, max_executions, expire_at_epoch) -> Result<(bool, i64), IntentTokenUsageStoreError>` |
| `SQLiteIntentTokenUsageStore(path, timeout_seconds=)` | `SQLiteIntentTokenUsageStore::new(Option<&Path>)`, `with_timeout(path, seconds)`; plus `InMemoryIntentTokenUsageStore` |
| `default_intent_token_usage_db_path()` | `intent::default_intent_token_usage_db_path()` |
| `create_intent_mandate(**kwargs)` | `create_intent_mandate(CreateIntentMandate { .. })` |
| `verify_intent_mandate(mandate, **kwargs)` | `verify_intent_mandate(&mandate, VerifyIntentMandate { .. }) -> Result<(), String>` |
| `normalize_intent_mandate`, `intent_mandate_core_payload`, `intent_user_signature_context`, `sign_server_mandate` | same names in `intent::mandate` |
| `normalize_intent_scope`, `normalize_execution_policy`, `params_match_intent_scope` | same names in `intent::scope`; `fnmatchcase` for Python glob semantics |
| `issue_intent_token(mandate, user_id=, verified_mandate=, ...)` | `issue_intent_token(&mandate, IssueIntentToken { .. })` |
| `verify_intent_token(token, action=, params=, expected_*=)` | `verify_intent_token(&token, VerifyIntentToken { .. }) -> Result<(), String>` |
| `params_match_intent_token(token, action=, params=)` | `params_match_intent_token(&token, action, params)` |
| `create_operation_mandate(**kwargs)` | `create_operation_mandate(CreateOperationMandate { .. })` |
| `verify_operation_mandate_for_completion(mandate, expected=, ...)` | `verify_operation_mandate_for_completion(&mandate, VerifyOperationMandate { .. })` |
| `normalize_operation_mandate`, `operation_mandate_core_payload`, `operation_user_signature_context` | same names in `operation::mandate` |
| `intent_server_signing_key()`, `operation_server_signing_key()`, `*_trusted_key()` | same names in `intent::signing` and `operation::signing` |
| `ed25519_private_key_from_env`, `ed25519_sign_text`, `ed25519_verify_text`, `ed25519_public_key_{from,to}_base64url` | same names in `security` |
| `authorization_common.error_code(reason, prefix)` | `authorization_common::error_code` |

`IntoPayload` is implemented for `JsonDict`, `serde_json::Value` and the typed request structs,
which mirrors the Python services accepting either a dataclass or a dict.

## HTTP endpoints

All endpoints are JSON `POST`. Non-POST requests get 405, unknown paths 404, invalid JSON and
`ValueError` style rejections 400 (`{"error": "bad_request", "message": ...}`), stable protocol
conflicts their own status (`{"error": CODE, "message": ...}`) and unexpected failures 500.
Domain rejections (invalid mandate, exhausted quota) stay HTTP 200 with `approved` or `valid`
set to `false`.

| Endpoint | Caller | Facade method |
| --- | --- | --- |
| `/a4p/v1/operation-authorizations/prepare` | Tool Server | `prepare_operation_authorization` |
| `/a4p/v1/operation-authorizations/complete` | Tool Server | `complete_operation_authorization` |
| `/a4p/v1/intent-authorizations/prepare` | Agent | `prepare_intent_authorization` |
| `/a4p/v1/intent-authorizations/complete` | Agent | `complete_intent_authorization` |
| `/a4p/v1/intent-tokens/verify` | Tool Server | `verify_intent_token` |
| `/a4p/v1/user-credentials/ed25519/register` | enrollment client | `register_ed25519_credential` |
| `/a4p/v1/user-credentials/webauthn/register/options` | enrollment client | `webauthn_registration_options` |
| `/a4p/v1/user-credentials/webauthn/register/verify` | enrollment client | `verify_webauthn_registration` |

Environment variables: `A4P_SERVER_HOST` (default `127.0.0.1`), `A4P_SERVER_PORT` (default
`8961`), `A4P_SERVER_BASE_URL` (client, default `http://127.0.0.1:8961`), `A4P_HTTP_TIMEOUT_S`
(client, default 300, minimum 1), `A4P_USAGE_DB_PATH` (default `.a4p/intent_token_usage.sqlite3`),
`INTENT_SERVER_ED25519_PRIVATE_KEY` and `OPERATION_SERVER_ED25519_PRIVATE_KEY` (PKCS8 PEM or a
32 byte seed as base64url, `base64url:`, `base64:` or `hex:`), and `A4P_ENV` / `APP_ENV` / `ENV` /
`PYTHON_ENV` (`prod` or `production` refuses the built-in development keys).

## Stable codes

`MANDATE_INVALID`, `AUTHORIZATION_NOT_PENDING`, `MANDATE_PENDING_MISMATCH`, `OPERATION_INVALID`,
`OPERATION_PENDING_MISMATCH`, `OPERATION_MANDATE_MISMATCH`, `MANDATE_SIGNATURE_INVALID`,
`MANDATE_EXPIRED`, `TOKEN_INVALID`, `TOKEN_SIGNATURE_INVALID`, `TOKEN_EXPIRED`,
`TOKEN_SCOPE_MISMATCH`, `TOKEN_USAGE_EXCEEDED`, `TOKEN_USAGE_STORE_ERROR`,
`USER_CREDENTIAL_NOT_REGISTERED`, `SIGNATURE_METHOD_NOT_ENABLED`, `CREDENTIAL_KEY_CONFLICT`,
plus the local User Authorizer codes `SERVER_KEY_UNTRUSTED`, `SERVER_SIGNATURE_INVALID`,
`CHALLENGE_BINDING_INVALID` and `SIGNING_OPTIONS_MISMATCH`. The reason text to code mapping of
`authorization_common.error_code()` is ported verbatim.

## Examples

| Example | Python original | Command |
| --- | --- | --- |
| `ed25519_authorization` | `examples/ed25519_authorization.py` | `cargo run -p a4p --example ed25519_authorization` |
| `smoke_test` | `examples/note_mcp_a4p/smoke_test.py` | `cargo run -p a4p --example smoke_test` |
| `note_tool_server` | `examples/note_mcp_a4p/note_mcp_server.py` (plain HTTP JSON tools, no MCP) | `cargo run -p a4p --example note_tool_server -- --port 8962` |
| `run_authorization_server` | `examples/note_mcp_a4p/run_authorization_server.py` | `cargo run -p a4p --example run_authorization_server -- --port 8961` |
| `run_user_authorizer` | `examples/note_mcp_a4p/run_user_authorizer.py` | `cargo run -p a4p --example run_user_authorizer -- --trusted-server-keys .a4p/trusted_server_keys.json` |
| `register_browser_key` | `examples/note_mcp_a4p/register_browser_key.py` | `cargo run -p a4p --example register_browser_key` |
| `agent_simulator` | `examples/note_mcp_a4p/agent_simulator.py` | `cargo run -p a4p --example agent_simulator -- --mode intent` |

The browser demo needs four terminals in this order: `run_authorization_server`,
`run_user_authorizer`, `note_tool_server`, then `register_browser_key` once and
`agent_simulator --mode operation|intent`. The tool server exposes each tool as
`POST /tools/{name}` with a JSON object of arguments. The User Authorizer serves the original
browser assets from `examples/user_authorizer_assets/` under `/assets/` with the same routes
(`/authorize`, `/register`, `/register/complete`, `/approve`, `/reject`) and JSON contract, so
the original page works unchanged. WebAuthn needs a browser on `http://localhost:8970`.

## Tests

```text
CARGO_TARGET_DIR=target-a4p cargo test -p a4p
```

The suites in `tests/` port the intent of every original pytest file. Where the Python tests
monkeypatch the `webauthn` package, the Rust tests use a software authenticator
(`tests/common/mod.rs`) that builds real `clientDataJSON`, authenticator data, `none` and packed
self-attestation objects and assertions for P-256, Ed25519 and RSA keys, so the native
verification path runs for real. The browser User Authorizer tests live inside the
`run_user_authorizer` example (`[[example]] test = true`).

## Canonical JSON and floats

`canonical_json` reproduces `json.dumps(obj, sort_keys=True, separators=(",", ":"),
ensure_ascii=False, allow_nan=False)` byte for byte: object keys are sorted by code point,
strings keep raw UTF-8 with the same escape set as Python, integers print as integers and floats
use a Python `repr` compatible formatter (shortest round trip digits, fixed notation for decimal
exponents between -4 and 16, otherwise `1e-07` / `1e+16` style with a two digit exponent).
This was checked against CPython on floats such as `1e16`, `1e15`, `1e-7`, `5e-324`, `-0.0` and
`1.7976931348623157e308`. `serde_json::Value` cannot hold NaN or infinity, so those values can
never reach a mandate; the formatter still rejects them with the Python message
`Out of range float values are not JSON compliant`.

## Deviations from the Python SDK

- Mandates and tokens are `serde_json::Map` objects (`JsonDict`), not typed structs, exactly
  like the Python dicts, because signatures cover their canonical JSON and the pending
  comparison is dict equality.
- WebAuthn verification is native. Supported COSE algorithms are ES256 (-7), EdDSA (-8) and
  RS256 (-257), which are also the algorithms advertised in `pubKeyCredParams` (py_webauthn
  advertises nine). Attestation formats are `none` and packed self-attestation; packed with a
  certificate chain, TPM, Android, Apple and FIDO U2F attestations are rejected.
- Verification failures of the Server signing key (production mode without a configured key)
  surface as a reason string (`Server signing key unavailable: ...`) instead of an exception.
- The HTTP client formats a non-2xx error body as JSON text (`A4P HTTP 409: {"error":...}`)
  where Python prints a dict repr.
- Usage store argument validation errors are reported as `IntentTokenUsageStoreError` and
  therefore map to `TOKEN_USAGE_STORE_ERROR` instead of propagating as `ValueError`.
- `A4PServer` methods are `async` for API parity, but they hold no await points; pending
  maps are `parking_lot` mutexes and a `complete` call holds the lock for its whole duration,
  which keeps the "one success per pending authorization" guarantee under concurrency.
- Expired pending operations can be simulated with `OperationAuthorizationService::set_pending_expiry`
  instead of mutating a private dict.
- Python `str()` coercion of non-string JSON scalars is emulated (`5` becomes `"5"`, `true`
  becomes `"True"`).

## Intentionally left out

- The MCP transport of the note demo; the tool server is a plain HTTP JSON server.
- py_webauthn features not needed by the protocol: token binding checks, extension parsing
  beyond skipping, attestation certificate chain validation, ES384/ES512/PS* algorithms.
- The Python test that checks the `webauthn` package imports (`test_webauthn_dependency_exports_are_loadable`).
- Generic `Exception` to HTTP 500 mapping for panics; panics inside handlers are not caught.
