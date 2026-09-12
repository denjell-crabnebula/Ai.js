// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

// Headless browser end-to-end test for the web example.
//
// The page acts as the User Authorizer against a live Rust A4P server. This
// script plays the Tool Server and Agent roles over HTTP; the page does key
// generation, verification and signing inside the wasm module.
//
// Prerequisites:
//   examples/build.sh
//   cargo build -p a4p --example run_ed25519_authorization_server
//   npm install playwright-core        (any directory on NODE_PATH)
// Run:
//   CHROME_PATH=/path/to/chrome node examples/web/e2e.cjs
// CHROME_PATH is optional when playwright-core can find a browser itself.
const { chromium } = require("playwright-core");
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const path = require("node:path");
const assert = require("node:assert/strict");

const CRATE = path.resolve(__dirname, "..", "..");
const ROOT = path.resolve(CRATE, "..", "..");
const A4P_PORT = 18962;
const HTTP_PORT = 18080;
const TRUST = path.join(process.env.TMPDIR || "/tmp", "pw_trust.json");

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function post(route, body) {
  const r = await fetch(`http://127.0.0.1:${A4P_PORT}${route}`, {
    method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
  });
  return r.json();
}

(async () => {
  fs.rmSync(TRUST, { force: true });
  const server = spawn(path.join(ROOT, "target/debug/examples/run_ed25519_authorization_server"),
    ["--port", String(A4P_PORT), "--trusted-keys-output", TRUST], { stdio: "ignore" });
  const web = spawn("python3", ["-m", "http.server", String(HTTP_PORT), "--bind", "127.0.0.1"], { cwd: CRATE, stdio: "ignore" });
  const cleanup = () => { server.kill(); web.kill(); };
  process.on("exit", cleanup);
  for (let i = 0; i < 50 && !fs.existsSync(TRUST); i++) await sleep(100);
  await sleep(500);

  const browser = await chromium.launch(process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {});
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  page.on("console", (m) => { if (m.type() === "error" && !/Failed to load resource/.test(m.text())) errors.push(m.text()); });
  page.on("response", (r) => { if (r.status() >= 400 && !/favicon\.ico$/.test(r.url())) errors.push(`HTTP ${r.status()} ${r.url()}`); });
  await page.goto(`http://127.0.0.1:${HTTP_PORT}/examples/web/`);
  await page.waitForFunction(() => document.title.includes("wasm 0.1.0"));

  // 1. Device key.
  await page.click("#gen");
  const jwk = JSON.parse(await page.textContent("#jwk"));
  assert.equal(jwk.kty, "OKP");
  const reg = await post("/a4p/v1/user-credentials/ed25519/register", { userId: "user-1", publicKey: jwk, metadata: { label: "browser" } });
  await page.fill("#cred", reg.credential.credentialId);

  // 2. Trust store from the server's file.
  await page.fill("#trust", fs.readFileSync(TRUST, "utf8"));

  // 3. Forwarded request.
  const operation = { action: "delete_note", params: { note_id: "note-7" } };
  const prepared = await post("/a4p/v1/operation-authorizations/prepare", { agentId: "agent-1", userId: "user-1", operation, validitySeconds: 60 });
  await page.fill("#request", JSON.stringify({ mandate: prepared.mandate, signingOptions: prepared.signingOptions }));
  await page.click("#verify");
  assert.match(await page.textContent("#status"), /verified/);
  const summary = JSON.parse(await page.textContent("#summary"));
  assert.equal(summary.id, prepared.mandate.operationId);
  await page.click("#sign");
  assert.match(await page.textContent("#status"), /approved and signed/);
  const signed = JSON.parse(await page.textContent("#signed"));
  const completed = await post("/a4p/v1/operation-authorizations/complete", { signedMandate: signed, operation });
  assert.equal(completed.approved, true, JSON.stringify(completed));

  // Tampered request is refused by the page.
  const tampered = structuredClone(prepared.mandate); tampered.displayText = "everything";
  await page.fill("#request", JSON.stringify({ mandate: tampered, signingOptions: {} }));
  await page.click("#verify");
  assert.match(await page.textContent("#status"), /^SERVER_SIGNATURE_INVALID/);

  // JSON-RPC and SSE playground.
  await page.click("#build");
  assert.match(await page.textContent("#rpc"), /"method":"initialize"/);
  await page.click("#parse");
  assert.match(await page.textContent("#events"), /notifications\/initialized/);

  assert.deepEqual(errors, []);
  await browser.close();
  cleanup();
  console.log("browser end-to-end passed: operationId", completed.operationId);
})().catch((e) => { console.error("browser e2e failed:", e); process.exit(1); });
