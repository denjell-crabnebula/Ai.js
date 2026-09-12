# WebAssembly bindings

Crate `agent-protocol-wasm` (`rust/agent-protocol-wasm`) exposes two things to JavaScript:
the JSON-RPC and SSE wire helpers shared by MCP and A2A, and the A4P User Authorizer (trust
store, device key, request verification, signing). It runs in browsers and in Node.

## Building

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128   # must match the pinned wasm-bindgen crate
rust/agent-protocol-wasm/examples/build.sh
```

The script produces `pkg/node` (CommonJS) and `pkg/web` (ES module with a default `init()`
export). To compile without generating bindings:

```bash
cargo build -p agent-protocol-wasm --target wasm32-unknown-unknown --release
```

## Node

```javascript
const ap = require("./pkg/node/agent_protocol_wasm.js");

// Wire helpers
const text = ap.buildRequest(1, "tools/list");
const parsed = ap.parseMessage(text);          // { kind: "request", id: 1, method: "tools/list" }
const sse = new ap.SseParser();
for (const event of sse.feedText("event: message\ndata: {\"a\":1}\n\n")) {
  console.log(event.event, event.data);
}

// A4P User Authorizer
const trust = ap.A4pTrustStore.fromJson(fs.readFileSync(".a4p/ed25519_trusted_server_keys.json", "utf8"));
const key = ap.Ed25519KeyPair.generate();      // register key.publicKeyJwk() with the server first
const signer = new ap.Ed25519Signer(key, credentialId);
const outcome = ap.authorizeWithEd25519(forwardedRequest, trust, signer);
if (outcome.approved) {
  // send outcome.signedMandate to the server's complete endpoint
} else {
  console.error(outcome.errorCode, outcome.rejectReason);
}
```

`examples/node/run.sh` builds the bindings, starts the A4P example server, runs
`examples/node/authorize.cjs` through the complete flow (registration, operation and intent
authorization, replay refusal, tampering detection) and prints `all steps passed`.

## Browser

```bash
rust/agent-protocol-wasm/examples/build.sh
cd rust/agent-protocol-wasm && python3 -m http.server 8080
# open http://localhost:8080/examples/web/
```

`examples/web/index.html` is a User Authorizer page: generate or restore a device key, load a
trust store, verify a forwarded request, review the summary, approve and sign. It works offline.
`examples/web/e2e.cjs` drives the same page in headless Chromium against a live server.

## API summary

| Group | Functions |
|-------|-----------|
| JSON-RPC | `buildRequest`, `buildNotification`, `buildResponse`, `buildErrorResponse`, `parseMessage`, `errorCodes` |
| SSE | `SseParser` (`feed`, `feedText`, `finish`), `formatSseEvent` |
| Trust and keys | `A4pTrustStore`, `Ed25519KeyPair`, `Ed25519Signer` |
| Verification | `verifyUserAuthorizationRequest`, `verifyServerMandate`, `describeMandate`, `deriveUserAuthorizationChallenge` |
| Signing | `signUserMandate`, `attachWebAuthnAssertion`, `authorizeWithEd25519`, `approveUserMandateUnsigned` |
| Helpers | `paramsMatchIntentScope`, `canonicalJson`, `mandateIdentifier` |

Errors thrown by the A4P functions start with the stable protocol code, for example
`MANDATE_EXPIRED: ...`; `authorizeWithEd25519` never throws and reports the code in its result.
The crate README documents every function's arguments.
