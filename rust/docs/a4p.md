# A4P SDK

Crate `a4p` (`A4P/rust-sdk`) implements the Agentic Authentication, Authorization and Audit
Protocol: operation mandates, intent mandates and intent tokens, user signatures (Ed25519 and
WebAuthn), the server facade, its HTTP transport and a client. It ports the Python SDK and is
wire and canonical-JSON compatible with it.

## The facade

`A4PServer` is the in-process facade a Tool Server or Agent talks to. Build one with the
builder; every dependency is explicit.

```rust
use std::sync::Arc;

use a4p::credential_store::InMemoryCredentialStore;
use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
use a4p::security::generate_ed25519_private_key;
use a4p::user_signature::ed25519::{ed25519_public_jwk, Ed25519UserSigner, RegisteredEd25519Method};
use a4p::{sign_user_mandate_with_signer, A4PServer};
use ap_support::json::object;
use serde_json::json;

# async fn demo() -> Result<(), Box<dyn std::error::Error>> {
let method = Arc::new(RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new())));
let server = A4PServer::builder()
    .server_id("local://demo")
    .user_signature_method(method)
    .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
    .build()?;

// Enroll the user's Ed25519 public key; the private key stays with the user.
let private_key = generate_ed25519_private_key();
let registration = server.register_ed25519_credential(
    &object(json!({"userId": "user-1", "publicKey": ed25519_public_jwk(&private_key)})),
)?;
let credential_id = registration["credential"]["credentialId"]
    .as_str()
    .ok_or("registration did not return a credentialId")?;
let signer = Ed25519UserSigner::new(credential_id, private_key)?;

// Operation authorization: prepare, the user signs, complete.
let operation = json!({"action": "delete_note", "params": {"note_id": "note-1"}});
let prepared = server
    .prepare_operation_authorization(object(json!({
        "agentId": "agent-1", "userId": "user-1", "operation": operation, "validitySeconds": 60,
    })))
    .await?;
let mandate = prepared.mandate.as_ref().ok_or("no mandate")?;
let signed = sign_user_mandate_with_signer(mandate, &signer, None)?;
let completed = server
    .complete_operation_authorization(object(json!({"signedMandate": signed, "operation": operation})))
    .await?;
assert!(completed.approved);
# Ok(()) }
```

The two flows:

- Operation authorization: a Tool Server prepares a mandate for one concrete operation, the
  user signs it, the Tool Server completes and executes exactly once.
- Intent authorization: an Agent prepares an intent mandate describing a scope of actions, the
  user signs it, and the Agent receives an intent token it presents to Tool Servers, which verify
  it with `verify_intent_token` (scope matching, validity, usage quota through the usage store).

Domain rejections (an invalid mandate, an exhausted quota) come back as a response with
`approved` or `valid` set to `false` and a stable `code`; programming errors and I/O failures are
`A4PError` values.

## HTTP facade and client

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

All endpoints are JSON `POST` under `/a4p/v1/`:

| Endpoint | Caller | Facade method |
|----------|--------|---------------|
| `operation-authorizations/prepare` | Tool Server | `prepare_operation_authorization` |
| `operation-authorizations/complete` | Tool Server | `complete_operation_authorization` |
| `intent-authorizations/prepare` | Agent | `prepare_intent_authorization` |
| `intent-authorizations/complete` | Agent | `complete_intent_authorization` |
| `intent-tokens/verify` | Tool Server | `verify_intent_token` |
| `user-credentials/ed25519/register` | enrollment client | `register_ed25519_credential` |
| `user-credentials/webauthn/register/options` | enrollment client | `webauthn_registration_options` |
| `user-credentials/webauthn/register/verify` | enrollment client | `verify_webauthn_registration` |

Non-POST requests get 405, unknown paths 404, invalid input 400 with
`{"error": "bad_request", "message": ...}`, stable protocol conflicts their own status with
`{"error": CODE, "message": ...}`, and unexpected failures 500.

`A4PClient::new()` reads `A4P_SERVER_BASE_URL` and `A4P_HTTP_TIMEOUT_S`;
`A4PClient::with_env(&env)` reads them from any `ap_support::env::EnvSource`, which is how tests
and embedders avoid the process environment. See [Configuration](configuration.md).

## Keys and environments

Server signing keys come from `INTENT_SERVER_ED25519_PRIVATE_KEY` and
`OPERATION_SERVER_ED25519_PRIVATE_KEY` (PKCS8 PEM, or a 32-byte seed as base64url,
`base64url:`, `base64:` or `hex:`). When a variable is unset, development mode derives a
deterministic key and logs a critical warning; production mode (`A4P_ENV`, `APP_ENV`, `ENV` or
`PYTHON_ENV` set to `prod` or `production`) refuses to start without explicit keys. The
`_in(env)` variants of these loaders (`intent_server_signing_key_in`,
`operation_server_signing_key_in`, `ed25519_private_key_from_env_in`) take an `EnvSource`.

## User Authorizer

The local User Authorizer (browser page or device app) verifies a forwarded authorization
request before the user signs: trust lookup of the Server key, Server signature, validity window,
signature method, challenge re-derivation. The Rust helpers live under `a4p::user_authorizer`
and are also exposed to JavaScript by the [WebAssembly bindings](wasm.md).

## Examples

| Example | Command |
|---------|---------|
| Ed25519 end-to-end | `cargo run -p a4p --example ed25519_authorization` |
| Non-interactive smoke test | `cargo run -p a4p --example smoke_test` |
| Note tool server | `cargo run -p a4p --example note_tool_server -- --port 8962` |
| Authorization server | `cargo run -p a4p --example run_authorization_server -- --port 8961` |
| Browser User Authorizer | `cargo run -p a4p --example run_user_authorizer -- --trusted-server-keys .a4p/trusted_server_keys.json` |
| Browser key enrollment | `cargo run -p a4p --example register_browser_key` |
| Agent simulator | `cargo run -p a4p --example agent_simulator -- --mode intent` |

The browser demo needs the authorization server, the User Authorizer and the note tool server
running, then one enrollment and the simulator in `operation` or `intent` mode. WebAuthn needs a
browser on `http://localhost:8970`.

## Errors and codes

`A4PError` has `value` (bad input), `runtime` (configuration or I/O) and protocol variants. The
stable protocol codes (`MANDATE_INVALID`, `MANDATE_EXPIRED`, `TOKEN_SCOPE_MISMATCH`,
`USER_CREDENTIAL_NOT_REGISTERED`, `SERVER_KEY_UNTRUSTED` and the rest) are listed in the crate
README and are identical to the Python SDK.
