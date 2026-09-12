// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Tests that run inside a wasm host (Node) through `wasm-bindgen-test`.
//!
//! ```text
//! rustup target add wasm32-unknown-unknown
//! cargo install wasm-bindgen-cli --version 0.2.128
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner \
//!   cargo test -p agent-protocol-wasm --target wasm32-unknown-unknown
//! ```

#![cfg(target_arch = "wasm32")]

use agent_protocol_wasm::a4p::{
    A4pTrustStore, Ed25519KeyPair, Ed25519Signer, authorize_with_ed25519, canonical_json,
    params_match_intent_scope_js, verify_user_authorization_request,
};
use agent_protocol_wasm::jsonrpc::{SseParser, build_request, parse_message};
use ap_support::testing::{ResultExt, TestResult};
use serde_json::{Value, json};
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

fn js(v: Value) -> TestResult<JsValue> {
    Ok(serde_wasm_bindgen::to_value(&v)?)
}

fn from_js(v: JsValue) -> TestResult<Value> {
    Ok(serde_wasm_bindgen::from_value(v)?)
}

#[wasm_bindgen_test]
fn jsonrpc_roundtrip() -> TestResult {
    let text = build_request(js(json!(7)), "tools/list", js(json!({"cursor": "c"})))?;
    let parsed = from_js(parse_message(&text)?);
    assert_eq!(parsed["kind"], "request");
    assert_eq!(parsed["id"], 7);
    assert_eq!(parsed["params"]["cursor"], "c");
    assert!(parse_message("nope").is_err());
    Ok(())
}

#[wasm_bindgen_test]
fn sse_parser_streams_events() -> TestResult {
    let mut p = SseParser::new();
    let first = from_js(p.feed_text("data: a\n\ndata: b")?);
    assert_eq!(first.as_array()?.len(), 1);
    let tail = from_js(p.finish()?);
    assert_eq!(tail["data"], "b");
    Ok(())
}

#[wasm_bindgen_test]
fn canonical_json_matches_python_layout() -> TestResult {
    let text = canonical_json(js(json!({"b": 1, "a": [true, "é", 2.5]})))?;
    assert_eq!(text, "{\"a\":[true,\"é\",2.5],\"b\":1}");
    Ok(())
}

#[wasm_bindgen_test]
fn intent_scope_precheck() -> TestResult {
    let intent = json!({"actions": [{"name": "delete_note", "params": {"note_id": "note-*"}}]});
    let ok = from_js(params_match_intent_scope_js(
        js(intent.clone()),
        "delete_note",
        js(json!({"note_id": "note-1"})),
    )?);
    assert_eq!(ok["matches"], true);
    let bad = from_js(params_match_intent_scope_js(
        js(intent),
        "delete_note",
        js(json!({"note_id": "x"})),
    )?);
    assert_eq!(bad["matches"], false);
    Ok(())
}

#[wasm_bindgen_test]
fn untrusted_mandate_is_rejected_without_throwing_in_authorize() -> TestResult {
    let kp = Ed25519KeyPair::generate();
    let signer = Ed25519Signer::new(&kp, "cred_1")?;
    let trust = A4pTrustStore::new(js(json!({
        "local://a4p": {"k1": {"alg": "EdDSA", "publicKey": kp.public_key_base64url()}}
    })))?;
    let request = json!({
        "mandate": {
            "type": "a4p/v1/operation-mandate",
            "operationId": "op_x",
            "server": "local://a4p",
            "subject": {"type": "agent", "id": "agent:a"},
            "operation": {"action": "delete_note", "params": {"note_id": "1"}},
            "validTime": {"until": "2999-01-01T00:00:00Z"},
            "userAuthorization": {"required": true, "signatureMethod": "ed25519", "methodPolicy": {}},
            "displayText": "x",
            "signatures": {"server": {"alg": "EdDSA", "keyId": "k1", "signature": "AAAA"}, "user": {}}
        },
        "signingOptions": {}
    });
    let decision = from_js(authorize_with_ed25519(js(request.clone()), &trust, &signer)?);
    assert_eq!(decision["approved"], false);
    assert_eq!(decision["errorCode"], "SERVER_SIGNATURE_INVALID");
    let err = verify_user_authorization_request(js(request), &trust, Some("ed25519".into())).err_or_fail()?;
    let message: String = js_sys::Reflect::get(&err.into(), &"message".into())?.as_string()?;
    assert!(message.starts_with("SERVER_SIGNATURE_INVALID"));
    Ok(())
}

#[wasm_bindgen_test]
fn keypair_seed_roundtrip() -> TestResult {
    let kp = Ed25519KeyPair::generate();
    let back = Ed25519KeyPair::from_seed(&kp.seed_base64url())?;
    assert_eq!(kp.public_key_base64url(), back.public_key_base64url());
    Ok(())
}
