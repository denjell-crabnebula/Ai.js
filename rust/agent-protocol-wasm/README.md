# agent-protocol-wasm

WebAssembly bindings for the agent-protocol SDKs, built with `wasm-bindgen`. The module runs in
browsers and in Node and exposes two things:

- **JSON-RPC 2.0 and SSE wire helpers** from `ap-jsonrpc`: build and parse the messages an MCP or
  A2A client exchanges over `fetch`, and parse streamed `text/event-stream` bodies.
- **The A4P User Authorizer** from the `a4p` core: the component the protocol places on the user's
  device. It verifies Server-signed mandates against a local trust store, derives the WebAuthn
  challenge, and produces Ed25519 user signatures. No private key leaves the module unless the
  caller exports the seed explicitly.

Networking stays in JavaScript. The module never opens connections itself, which keeps the surface
small and lets the same build serve browser pages, Node services and workers.

## Building

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128   # must match the pinned wasm-bindgen crate
rust/agent-protocol-wasm/examples/build.sh
```

The script produces `pkg/node` (CommonJS, for `require`) and `pkg/web` (ES module with a default
`init()` export). Both directories are generated and git-ignored.

To compile the core without generating bindings:

```bash
cargo build -p agent-protocol-wasm --target wasm32-unknown-unknown --release
```

This works because `a4p` exposes cargo features `http-server`, `client` and `sqlite` for its
native-only parts. With `--no-default-features` its core (mandates, tokens, scope matching, user
signatures, the User Authorizer helpers) has no native dependency.

## Examples

| Example | What it shows | Run |
|---------|---------------|-----|
| `examples/node/authorize.cjs` | Full A4P flow in Node: register an Ed25519 key, operation authorization (prepare, local verify, sign, complete, replay refused), intent authorization with token verification and local scope pre-check, tampering detection, JSON-RPC and SSE helpers | `examples/node/run.sh` |
| `examples/web/index.html` | Browser User Authorizer page: generate or restore a device key, load a trust store, verify a forwarded request, show the verified summary, approve and sign; plus a JSON-RPC and SSE playground | see below |

`run.sh` builds the bindings, starts `cargo run -p a4p --example run_ed25519_authorization_server`
on a temporary port, runs the Node script against it and stops the server. Expected tail of the
output:

```text
== Tampering: a forwarded mandate with a changed displayText is refused
   refused: SERVER_SIGNATURE_INVALID: Server signature invalid
...
all steps passed
```

The browser page needs the crate directory served over HTTP so the module can be fetched:

```bash
rust/agent-protocol-wasm/examples/build.sh
cd rust/agent-protocol-wasm && python3 -m http.server 8080
# open http://localhost:8080/examples/web/
```

Paste the trust configuration written by the server (`.a4p/ed25519_trusted_server_keys.json`) and
a `{mandate, signingOptions}` object returned by a prepare call. The page works offline.

`examples/web/e2e.cjs` drives the same page in headless Chromium through `playwright-core` against
a live server: register the page's key, verify and sign a prepared mandate, complete it on the
server, and confirm a tampered mandate is refused.

## JavaScript API

### Wire helpers

| Function | Purpose |
|----------|---------|
| `buildRequest(id, method, params?)` | Serialize a JSON-RPC request; `id` is an integer or string |
| `buildNotification(method, params?)` | Serialize a notification |
| `buildResponse(id, result)` | Serialize a success response |
| `buildErrorResponse(id \| null, code, message, data?)` | Serialize an error response |
| `parseMessage(text)` | Parse one message or a batch into `{kind, ...}` objects; `kind` is `request`, `notification` or `response` |
| `errorCodes()` | The standard JSON-RPC error codes |
| `new SseParser()`, `.feed(bytes)`, `.feedText(text)`, `.finish()` | Incremental SSE parsing; returns `{event, data, id, retry}` events |
| `formatSseEvent({event?, data, id?})` | Format an event as wire text |

### A4P User Authorizer

| API | Purpose |
|-----|---------|
| `new A4pTrustStore(config)`, `A4pTrustStore.fromJson(text)` | Trust anchors `{serverId: {keyId: {alg, publicKey}}}`; provision them over a protected channel, never from the Agent |
| `Ed25519KeyPair.generate()`, `.fromSeed(seed)`, `.seedBase64Url()`, `.publicKeyJwk()`, `.publicKeyBase64Url()` | The user's device key; the JWK is what `/a4p/v1/user-credentials/ed25519/register` expects |
| `new Ed25519Signer(keyPair, credentialId)` | Bind the key to the credential id returned by registration |
| `verifyUserAuthorizationRequest(request, trust, expectedMethod?)` | Trust lookup, Server signature, validity, method check, challenge re-derivation and `userVerification=required`; returns hardened signing options or throws `CODE: message` |
| `verifyServerMandate(mandate, trust)` | Server signature only; returns the mandate core |
| `describeMandate(mandate)` | Summary to display after verification |
| `deriveUserAuthorizationChallenge(mandate)` | SHA-256 challenge as base64url, the value a WebAuthn assertion must sign |
| `signUserMandate(mandate, signer)` | Ed25519 user signature over the canonical payload |
| `attachWebAuthnAssertion(mandate, assertion)` | Wrap a `navigator.credentials.get` result as the user signature |
| `authorizeWithEd25519(request, trust, signer)` | One call: verify then sign; returns `{approved, signedMandate}` or `{approved: false, rejectReason, errorCode}` without throwing |
| `approveUserMandateUnsigned(mandate)` | Explicit no-signature mode |
| `paramsMatchIntentScope(intent, action, params?)` | Local scope pre-check for intent tokens, `{matches, reason?}` |
| `canonicalJson(value)` | Python-compatible canonical JSON, the signature input format |
| `mandateIdentifier(mandate)` | `mandateId` or `operationId` |

Errors thrown by the A4P functions carry the stable protocol code at the start of the message, for
example `MANDATE_EXPIRED: ...` or `SERVER_KEY_UNTRUSTED: ...`.

## Tests

Native unit tests cover the key handling. The JavaScript-facing paths are tested inside Node with
`wasm-bindgen-test`:

```bash
cargo test -p agent-protocol-wasm
CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
  cargo test -p agent-protocol-wasm --target wasm32-unknown-unknown
```

The Node example doubles as the end-to-end test against the Rust A4P server.

## Not included

- MCP and A2A typed clients. The `mcp-sdk` and `a2a-sdk` crates depend on tokio and native HTTP
  stacks. Browser clients use the JSON-RPC and SSE helpers here on top of `fetch`.
- WebAuthn registration and assertion creation. Those are browser APIs; the module only derives the
  challenge and wraps the resulting assertion.
- The A4P Server. It stays a native service (`a4p` with default features).
