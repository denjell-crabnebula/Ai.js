// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

// Browser example for the agent-protocol wasm bindings.
// Serve the crate directory over HTTP (see README) so the module can be fetched:
//   cd rust/agent-protocol-wasm && python3 -m http.server 8080
//   open http://localhost:8080/examples/web/
import init, * as wasm from "../../pkg/web/agent_protocol_wasm.js";

const $ = (id) => document.getElementById(id);
const show = (id, value) => {
  $(id).textContent = typeof value === "string" ? value : JSON.stringify(value, null, 2);
};
const status = (text, ok) => {
  $("status").textContent = text;
  $("status").className = ok ? "ok" : "err";
};
const parseJson = (id, what) => {
  const text = $(id).value.trim();
  if (!text) throw new Error(`${what} is empty`);
  return JSON.parse(text);
};

let keyPair = null;

function publishKey() {
  $("seed").value = keyPair.seedBase64Url();
  show("jwk", keyPair.publicKeyJwk());
}

function signer() {
  if (!keyPair) throw new Error("generate or restore a key first");
  const credentialId = $("cred").value.trim();
  if (!credentialId) throw new Error("enter the credential id returned by registration");
  return new wasm.Ed25519Signer(keyPair, credentialId);
}

function trustStore() {
  return new wasm.A4pTrustStore(parseJson("trust", "trust store"));
}

function request() {
  const value = parseJson("request", "authorization request");
  if (!value.mandate) throw new Error("request needs a mandate");
  return { mandate: value.mandate, signingOptions: value.signingOptions || {} };
}

function guard(fn) {
  return () => {
    try {
      fn();
    } catch (error) {
      status(error.message || String(error), false);
    }
  };
}

await init();
document.title = `A4P User Authorizer (wasm ${wasm.version()})`;

$("gen").onclick = guard(() => {
  keyPair = wasm.Ed25519KeyPair.generate();
  publishKey();
  status("new key pair generated; register the JWK, then paste the credential id", true);
});

$("restore").onclick = guard(() => {
  keyPair = wasm.Ed25519KeyPair.fromSeed($("seed").value.trim());
  publishKey();
  status("key pair restored from seed", true);
});

$("verify").onclick = guard(() => {
  const req = request();
  const options = wasm.verifyUserAuthorizationRequest(req, trustStore(), "ed25519");
  show("summary", wasm.describeMandate(req.mandate));
  show("options", options);
  status("Server signature, validity and challenge binding verified", true);
});

$("challenge").onclick = guard(() => {
  const req = request();
  show("options", { challenge: wasm.deriveUserAuthorizationChallenge(req.mandate) });
  status("challenge derived from the Server-signed mandate", true);
});

$("sign").onclick = guard(() => {
  const req = request();
  const decision = wasm.authorizeWithEd25519(req, trustStore(), signer());
  if (decision.approved) {
    show("summary", wasm.describeMandate(req.mandate));
    show("signed", decision.signedMandate);
    status("approved and signed; return the signed mandate to the Agent", true);
  } else {
    show("signed", decision);
    status(`${decision.errorCode || "REJECTED"}: ${decision.rejectReason}`, false);
  }
});

$("build").onclick = guard(() => {
  const params = $("params").value.trim() ? JSON.parse($("params").value) : undefined;
  const text = wasm.buildRequest(1, $("method").value, params);
  show("rpc", `${text}\n\nparsed back:\n${JSON.stringify(wasm.parseMessage(text))}`);
});

$("parse").onclick = guard(() => {
  const parser = new wasm.SseParser();
  const events = parser.feedText($("sse").value);
  const tail = parser.finish();
  show("events", tail ? [...events, tail] : events);
});
