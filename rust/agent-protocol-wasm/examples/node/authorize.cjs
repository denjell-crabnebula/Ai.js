#!/usr/bin/env node
// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

// End-to-end A4P flow with the wasm User Authorizer.
//
// Roles in this script:
//   - "tool"  : the Tool Server, which prepares and completes operation
//               authorizations and verifies intent tokens over HTTP.
//   - "agent" : the Agent, which forwards mandates and signing options.
//   - "user"  : the User Authorizer, implemented by the wasm module. It holds
//               the user's Ed25519 key, verifies the Server signature against
//               the local trust store and signs approved mandates.
//
// Prerequisites (see run.sh, which does all of this):
//   1. examples/build.sh                          -> pkg/node bindings
//   2. cargo run -p a4p --example run_ed25519_authorization_server
//      (writes .a4p/ed25519_trusted_server_keys.json)
//
// Environment:
//   A4P_SERVER_BASE_URL   default http://127.0.0.1:8961
//   A4P_TRUSTED_KEYS      default .a4p/ed25519_trusted_server_keys.json

const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");

const wasm = require(path.join(__dirname, "..", "..", "pkg", "node", "agent_protocol_wasm.js"));

const BASE_URL = process.env.A4P_SERVER_BASE_URL || "http://127.0.0.1:8961";
const TRUSTED_KEYS = process.env.A4P_TRUSTED_KEYS || ".a4p/ed25519_trusted_server_keys.json";
const USER_ID = "user-1";
const AGENT_ID = "agent-1";

async function post(route, body) {
  const response = await fetch(`${BASE_URL}${route}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) {
    throw new Error(`POST ${route} -> HTTP ${response.status}: ${text}`);
  }
  return JSON.parse(text);
}

function step(title) {
  console.log(`\n== ${title}`);
}

// The User Authorizer side. Everything in here runs inside the wasm module.
class WasmUserAuthorizer {
  constructor(trustStore) {
    this.trust = trustStore;
    this.keyPair = wasm.Ed25519KeyPair.generate();
    this.signer = null;
  }

  registrationRequest(label) {
    return { userId: USER_ID, publicKey: this.keyPair.publicKeyJwk(), metadata: { label } };
  }

  bindCredential(credentialId) {
    this.signer = new wasm.Ed25519Signer(this.keyPair, credentialId);
  }

  // Mirrors A4PUserAuthorizer.authorize: verify first, show, then sign.
  authorize(request) {
    // 1. Local trust, Server signature, validity, method and challenge binding.
    const hardenedOptions = wasm.verifyUserAuthorizationRequest(request, this.trust, "ed25519");
    // 2. Only verified content is shown to the user.
    const summary = wasm.describeMandate(request.mandate);
    console.log("   user sees:", summary.displayText);
    console.log("   signing options (hardened):", JSON.stringify(hardenedOptions));
    // 3. The user approves; the device signs the canonical payload.
    return wasm.signUserMandate(request.mandate, this.signer);
  }
}

async function operationFlow(user) {
  step("Operation authorization: delete_note(note-1)");
  const operation = { action: "delete_note", params: { note_id: "note-1" } };

  // Tool Server: prepare. Returns mandate plus signing options.
  const prepared = await post("/a4p/v1/operation-authorizations/prepare", {
    agentId: AGENT_ID,
    userId: USER_ID,
    operation,
    validitySeconds: 60,
  });
  assert.ok(prepared.mandate, `prepare rejected: ${prepared.rejectReason}`);
  console.log("   operationId:", prepared.mandate.operationId);

  // Agent: forward {mandate, signingOptions} to the User Authorizer.
  const signedMandate = user.authorize({ mandate: prepared.mandate, signingOptions: prepared.signingOptions });
  assert.equal(signedMandate.signatures.user.signatureMethod, "ed25519");

  // Tool Server: rebuild the operation from the current request and complete.
  const completed = await post("/a4p/v1/operation-authorizations/complete", { signedMandate, operation });
  assert.equal(completed.approved, true, JSON.stringify(completed));
  console.log("   approved, operationId:", completed.operationId);

  // Replay must fail: the pending authorization was consumed.
  const replay = await post("/a4p/v1/operation-authorizations/complete", { signedMandate, operation });
  assert.equal(replay.approved, false);
  console.log("   replay rejected:", replay.verificationResult?.code || replay.rejectReason);
}

async function intentFlow(user) {
  step("Intent authorization: delete_note(note-*) up to 2 executions");
  const intent = {
    actions: [{ name: "delete_note", params: { note_id: "note-*" }, allowExtraParams: false }],
    executionPolicy: { maxExecutions: 2 },
  };

  // Agent: prepare the intent.
  const prepared = await post("/a4p/v1/intent-authorizations/prepare", {
    agentId: AGENT_ID,
    userId: USER_ID,
    intent,
    validitySeconds: 600,
  });
  assert.ok(prepared.mandate, `prepare rejected: ${JSON.stringify(prepared.verificationResult)}`);

  // One-call authorizer, never throws for protocol outcomes.
  const decision = wasm.authorizeWithEd25519(
    { mandate: prepared.mandate, signingOptions: prepared.signingOptions },
    user.trust,
    user.signer,
  );
  assert.equal(decision.approved, true, JSON.stringify(decision));
  console.log("   mandateId:", prepared.mandate.mandateId, "approved by user");

  // Agent: complete and receive a reusable intent token.
  const completed = await post("/a4p/v1/intent-authorizations/complete", { signedMandate: decision.signedMandate });
  assert.ok(completed.intentToken, JSON.stringify(completed));
  const token = completed.intentToken;
  console.log("   intent token:", token.tokenId);

  // Tool Server: pre-check the scope locally, then verify with the Server.
  for (const noteId of ["note-1", "note-2", "note-3"]) {
    const params = { note_id: noteId };
    const local = wasm.paramsMatchIntentScope(token.intent, "delete_note", params);
    const verified = await post("/a4p/v1/intent-tokens/verify", {
      token,
      expected: { action: "delete_note", params, agentId: token.subject.id, userId: USER_ID },
    });
    console.log(`   delete_note(${noteId}): local=${local.matches} server=${verified.valid} ${verified.code || ""}`);
  }
  const outOfScope = wasm.paramsMatchIntentScope(token.intent, "delete_note", { note_id: "draft-1" });
  assert.equal(outOfScope.matches, false);
  console.log("   delete_note(draft-1): local=false reason:", outOfScope.reason);
}

function tamperingIsDetected(user, mandate) {
  step("Tampering: a forwarded mandate with a changed displayText is refused");
  const tampered = structuredClone(mandate);
  tampered.displayText = "Authorize everything";
  try {
    wasm.verifyUserAuthorizationRequest({ mandate: tampered, signingOptions: {} }, user.trust, "ed25519");
    assert.fail("tampered mandate was accepted");
  } catch (error) {
    console.log("   refused:", error.message);
    assert.match(error.message, /^SERVER_SIGNATURE_INVALID/);
  }
}

function wireHelpers() {
  step("JSON-RPC and SSE helpers (what an MCP or A2A browser client uses)");
  const init = wasm.buildRequest(1, "initialize", {
    protocolVersion: "2025-06-18",
    capabilities: {},
    clientInfo: { name: "wasm-demo", version: "0.1.0" },
  });
  console.log("   request :", init);
  const parsed = wasm.parseMessage('{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}');
  console.log("   parsed  :", JSON.stringify(parsed));
  const sse = new wasm.SseParser();
  const events = [
    ...sse.feedText("event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/"),
    ...sse.feedText("initialized\"}\n\n: keepalive\n"),
  ];
  console.log("   sse     :", JSON.stringify(events));
  assert.equal(events.length, 1);
}

async function main() {
  console.log("wasm module version:", wasm.version());
  console.log("A4P server:", BASE_URL);

  const trustConfig = JSON.parse(fs.readFileSync(TRUSTED_KEYS, "utf8"));
  const user = new WasmUserAuthorizer(new wasm.A4pTrustStore(trustConfig));
  console.log("trusted servers:", Object.keys(trustConfig).join(", "));

  step("Enrollment: register the device's Ed25519 public key");
  const registration = await post("/a4p/v1/user-credentials/ed25519/register", user.registrationRequest("wasm demo key"));
  user.bindCredential(registration.credential.credentialId);
  console.log("   credentialId:", registration.credential.credentialId);

  await operationFlow(user);
  await intentFlow(user);

  const probe = await post("/a4p/v1/operation-authorizations/prepare", {
    agentId: AGENT_ID,
    userId: USER_ID,
    operation: { action: "delete_note", params: { note_id: "note-9" } },
  });
  tamperingIsDetected(user, probe.mandate);
  wireHelpers();

  console.log("\nall steps passed");
}

main().catch((error) => {
  console.error("\nexample failed:", error.message);
  process.exit(1);
});
