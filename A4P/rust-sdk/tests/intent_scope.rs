// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_intent_scope.py`.

pub mod common;

use a4p::JsonDict;
use a4p::intent::token::{VerifyIntentToken, params_match_intent_token, verify_intent_token};
use ap_support::testing::{OptionExt, ResultExt, TestResult};
use common::{explicit_server, obj, sign_user_mandate};
use serde_json::{Value, json};

fn token_with_action_params(constraint: Value) -> JsonDict {
    obj(json!({"intent": {"actions": [{"name": "act", "params": constraint}]}}))
}

fn matches(token: &JsonDict, action: &str, params: Value) -> Result<(), String> {
    params_match_intent_token(token, action, Some(&obj(params)))
}

#[test]
fn params_match_intent_token_glob_and_exact() -> TestResult {
    let cases = [
        (
            json!("*"),
            json!({"x": 1}),
            true,
            "whole-params wildcard allows anything",
        ),
        (
            json!({"k": "*"}),
            json!({"k": "anything"}),
            true,
            "per-key wildcard any value",
        ),
        (
            json!({"k": "*"}),
            json!({"k": 123}),
            true,
            "per-key wildcard non-string value",
        ),
        (
            json!({"k": "note-1"}),
            json!({"k": "note-1"}),
            true,
            "exact string match",
        ),
        (
            json!({"k": "note-1"}),
            json!({"k": "note-2"}),
            false,
            "exact string mismatch",
        ),
        (
            json!({"k": "*.md"}),
            json!({"k": "example.md"}),
            true,
            "glob suffix match",
        ),
        (
            json!({"k": "*.md"}),
            json!({"k": "example.txt"}),
            false,
            "glob suffix mismatch",
        ),
        (
            json!({"k": "note-*"}),
            json!({"k": "note-1"}),
            true,
            "glob prefix match",
        ),
        (
            json!({"k": "v?.0"}),
            json!({"k": "v1.0"}),
            true,
            "glob single-char match",
        ),
        (
            json!({"k": "v?.0"}),
            json!({"k": "v12.0"}),
            false,
            "glob single-char mismatch",
        ),
        (
            json!({"k": "*"}),
            json!({}),
            false,
            "missing required param rejected",
        ),
        (
            json!({"k": "*"}),
            json!({"k": "x", "extra": 1}),
            false,
            "extra unauthorized param rejected",
        ),
        (
            json!({"k": 42}),
            json!({"k": 42}),
            true,
            "non-string exact equality",
        ),
        (json!({"k": 42}), json!({"k": 43}), false, "non-string mismatch"),
        (
            json!({"k": "Example.MD"}),
            json!({"k": "example.md"}),
            false,
            "case-sensitive glob",
        ),
    ];
    for (constraint, actual, expected_ok, description) in cases {
        let token = token_with_action_params(constraint.clone());
        let result = matches(&token, "act", actual.clone());
        assert_eq!(
            result.is_ok(),
            expected_ok,
            "{description}: constraint={constraint} actual={actual} -> {result:?}"
        );
    }

    let token = token_with_action_params(json!({"k": "*"}));
    let reason = matches(&token, "other", json!({"k": "x"})).err_or_fail()?;
    assert!(reason.contains("not in token actions"), "{reason}");
    assert_eq!(reason, "Action 'other' not in token actions: ['act']");
    Ok(())
}

#[test]
fn params_match_intent_token_checks_all_same_name_candidates() -> TestResult {
    let cases = [
        (
            json!([
                {"name": "act", "params": {"kind": "first"}},
                {"name": "act", "params": {"kind": "second"}},
            ]),
            json!({"kind": "second"}),
            "value mismatch",
        ),
        (
            json!([
                {"name": "act", "params": {"kind": "first", "required": "yes"}},
                {"name": "act", "params": {"kind": "first"}},
            ]),
            json!({"kind": "first"}),
            "missing required param",
        ),
        (
            json!([
                {"name": "act", "params": {"kind": "first"}},
                {"name": "act", "params": {"kind": "first"}, "allowExtraParams": true},
            ]),
            json!({"kind": "first", "optional": true}),
            "unexpected param",
        ),
        (
            json!([
                {"name": "act", "params": {"kind": "first"}, "allowExtraParams": true},
                {"name": "act", "params": {"kind": "second"}},
            ]),
            json!({"kind": "second"}),
            "allowExtraParams candidate value mismatch",
        ),
    ];
    for (actions, actual, description) in cases {
        let token = obj(json!({"intent": {"actions": actions}}));
        let result = matches(&token, "act", actual);
        assert_eq!(result, Ok(()), "{description}");
    }
    Ok(())
}

#[test]
fn params_match_intent_token_all_same_name_candidates_mismatch() -> TestResult {
    let token = obj(json!({
        "intent": {
            "actions": [
                {"name": "act", "params": {"kind": "first"}},
                {"name": "act", "params": {"required": "yes"}},
            ]
        }
    }));
    let reason = matches(&token, "act", json!({"kind": "other"})).err_or_fail()?;
    assert_eq!(reason, "Param 'kind' mismatch for action 'act'");
    Ok(())
}

fn token_with_action(constraint: Value, allow_extra: bool) -> JsonDict {
    let mut action = obj(json!({"name": "act", "params": constraint}));
    if allow_extra {
        action.insert("allowExtraParams".into(), Value::Bool(true));
    }
    obj(json!({"intent": {"actions": [action]}}))
}

#[test]
fn params_match_intent_token_allow_extra() -> TestResult {
    let cases = [
        (
            json!({"k": "v"}),
            true,
            json!({"k": "v", "extra": 1}),
            true,
            "allowExtraParams=true allows extra param",
        ),
        (
            json!({"k": "v"}),
            false,
            json!({"k": "v", "extra": 1}),
            false,
            "default rejects extra param",
        ),
        (
            json!({"k": "v"}),
            true,
            json!({"extra": 1}),
            false,
            "required key still enforced with allowExtraParams",
        ),
        (
            json!({"k": "v"}),
            true,
            json!({"k": "other", "extra": 1}),
            false,
            "value mismatch still rejected",
        ),
        (
            json!({"k": "v"}),
            true,
            json!({"k": "v"}),
            true,
            "allowExtraParams=true with no extra params",
        ),
        (
            json!({"k": "*.md"}),
            true,
            json!({"k": "a.md", "extra": 1}),
            true,
            "glob constraint + extra allowed",
        ),
        (
            json!({"k": "*.md"}),
            true,
            json!({"k": "a.txt", "extra": 1}),
            false,
            "glob mismatch + extra still rejected",
        ),
        (
            json!({"command": "ls"}),
            true,
            json!({"command": "ls", "description": "x", "timeout": 10}),
            true,
            "bash scenario: only command declared, extras allowed",
        ),
        (
            json!({"command": "ls"}),
            true,
            json!({"command": "rm", "description": "x"}),
            false,
            "bash scenario: command value mismatch rejected",
        ),
    ];
    for (constraint, allow_extra, actual, expected_ok, description) in cases {
        let token = token_with_action(constraint.clone(), allow_extra);
        let result = matches(&token, "act", actual.clone());
        assert_eq!(
            result.is_ok(),
            expected_ok,
            "{description}: constraint={constraint} allow_extra={allow_extra} actual={actual} -> {result:?}"
        );
    }
    let token = token_with_action(json!({"k": "v"}), false);
    assert_eq!(
        matches(&token, "act", json!({"k": "v", "z": 1, "extra": 1})).err_or_fail()?,
        "Unexpected params for action 'act': ['extra', 'z']"
    );
    assert_eq!(
        matches(&token, "act", json!({})).err_or_fail()?,
        "Required param 'k' missing for action 'act'"
    );
    assert_eq!(
        matches(&token, "  ", json!({})).err_or_fail()?,
        "Expected action missing"
    );
    Ok(())
}

#[tokio::test]
async fn allow_extra_params_field_is_signature_protected() -> TestResult {
    let server = explicit_server("local://test")?;
    let prepared = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "agentPublicKey": {"kty": "OKP", "kid": "agent-key-1", "crv": "Ed25519", "x": "abc"},
            "intent": {
                "actions": [{"name": "bash", "params": {"command": "ls"}, "allowExtraParams": true}]
            },
            "validitySeconds": 60,
        })))
        .await?;
    let signed = sign_user_mandate(prepared.mandate.as_ref().required()?)?;
    let completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": signed})))
        .await?;
    assert!(completed.approved);
    let token = completed.intent_token.required()?;

    let params = obj(json!({"command": "ls", "description": "x", "timeout": 10}));
    let result = verify_intent_token(
        &token,
        VerifyIntentToken {
            action: "bash",
            params: Some(&params),
            expected_agent_key_id: Some("agent-key-1"),
            ..Default::default()
        },
    );
    assert_eq!(result, Ok(()), "original token should allow extra params");

    let mut tampered = token.clone();
    tampered["intent"]["actions"][0]["allowExtraParams"] = Value::Bool(false);
    let params = obj(json!({"command": "ls", "description": "x"}));
    let reason = verify_intent_token(
        &tampered,
        VerifyIntentToken {
            action: "bash",
            params: Some(&params),
            expected_agent_key_id: Some("agent-key-1"),
            ..Default::default()
        },
    )
    .err_or_fail()?;
    assert!(reason.to_lowercase().contains("signature"), "{reason}");
    Ok(())
}
