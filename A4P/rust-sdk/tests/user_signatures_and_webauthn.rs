// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Port of `tests/unit/test_user_signatures_and_webauthn.py`.
//!
//! The Python suite mocks the `webauthn` package. Here a software
//! authenticator produces real attestations and assertions so the native
//! verification path is exercised end to end.

pub mod common;

use ap_support::testing::{OptionExt, ResultExt, TestResult};
use std::sync::Arc;

use a4p::credential_store::{A4PCredentialStore, InMemoryCredentialStore, UserCredentialRecord};
use a4p::operation::mandate::{
    CreateOperationMandate, create_operation_mandate, operation_user_signature_context,
};
use a4p::security::generate_ed25519_private_key;
use a4p::user_signature::ed25519::{Ed25519UserSigner, RegisteredEd25519Method, ed25519_public_jwk};
use a4p::user_signature::webauthn::{WebAuthnSignatureMethod, WebAuthnUserSigner, b64url_encode};
use a4p::user_signature::{A4PUserSignatureMethod, A4PUserSigner, UserSignatureContext};
use a4p::{A4PError, A4PServer, JsonDict, approve_user_mandate, sign_user_mandate_with_signer};
use common::{SoftwareAuthenticator, obj, usage_store};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};

fn ed25519_server() -> TestResult<(
    A4PServer,
    Arc<RegisteredEd25519Method>,
    SigningKey,
    Ed25519UserSigner,
)> {
    let private_key = generate_ed25519_private_key();
    let method = Arc::new(RegisteredEd25519Method::new(Arc::new(
        InMemoryCredentialStore::new(),
    )));
    let server = A4PServer::builder()
        .server_id("local://ed25519-test")
        .user_signature_method(method.clone())
        .intent_token_usage_store(usage_store())
        .build()?;
    let registration = server.register_ed25519_credential(&obj(json!({
        "userId": "user-1",
        "publicKey": ed25519_public_jwk(&private_key),
        "metadata": {"label": "test key"},
    })))?;
    let credential_id = registration["credential"]["credentialId"].as_str().required()?;
    let signer = Ed25519UserSigner::new(credential_id, private_key.clone())?;
    Ok((server, method, private_key, signer))
}

fn operation_request(user_id: &str) -> JsonDict {
    obj(json!({
        "agentId": "agent-1",
        "userId": user_id,
        "operation": {"action": "delete_note", "params": {"note_id": "note-1"}},
        "validitySeconds": 60,
    }))
}

fn webauthn_server(method: Arc<WebAuthnSignatureMethod>) -> TestResult<A4PServer> {
    Ok(A4PServer::builder()
        .user_signature_method(method)
        .intent_token_usage_store(usage_store())
        .build()?)
}

/// Register a software authenticator with a method and return its record.
fn enroll(
    method: &WebAuthnSignatureMethod,
    authenticator: &mut SoftwareAuthenticator,
    user_id: &str,
) -> TestResult<UserCredentialRecord> {
    let options = method.registration_options(user_id, None, None)?;
    let challenge = options["options"]["challenge"].as_str().required()?;
    let credential = authenticator.register(challenge)?;
    Ok(method.verify_registration(&obj(json!({
        "userId": user_id,
        "registrationRequestId": options["registrationRequestId"],
        "credential": credential,
    })))?)
}

#[tokio::test]
async fn server_requires_an_explicit_method_for_signed_mode() -> TestResult {
    let error = A4PServer::builder()
        .intent_token_usage_store(usage_store())
        .build()
        .err()
        .required()?;
    assert!(error.to_string().contains("user_signature_method is required"));

    let server = A4PServer::builder()
        .require_user_signature(false)
        .intent_token_usage_store(usage_store())
        .build()?;
    let prepared = server
        .prepare_operation_authorization(operation_request("user-1"))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(mandate["userAuthorization"], json!({"required": false}));
    assert_eq!(mandate["signatures"]["user"], json!({}));
    assert!(prepared.signing_options.is_empty());
    assert_eq!(server.signature_method(), None);
    Ok(())
}

struct UppercaseMethod;

impl A4PUserSignatureMethod for UppercaseMethod {
    fn signature_method(&self) -> &str {
        "WebAuthn"
    }
    fn method_policy(&self) -> JsonDict {
        JsonDict::new()
    }
    fn signing_options(&self, _user_id: &str, _mandate: &JsonDict) -> Result<JsonDict, A4PError> {
        Ok(JsonDict::new())
    }
    fn verify(&self, _context: &UserSignatureContext, _signature: &JsonDict) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn server_rejects_invalid_signature_method_identifier() -> TestResult {
    let error = A4PServer::builder()
        .user_signature_method(Arc::new(UppercaseMethod))
        .intent_token_usage_store(usage_store())
        .build()
        .err()
        .required()?;
    assert!(error.to_string().contains("lowercase identifier"), "{error}");
    Ok(())
}

#[test]
fn ed25519_registration_rejects_invalid_jwk() -> TestResult {
    let cases = [
        (json!({"kty": "EC"}), "kty"),
        (json!({"crv": "X25519"}), "crv"),
        (json!({"alg": "ES256"}), "alg"),
        (json!({"alg": ""}), "alg"),
        (json!({"x": "AA"}), "32 bytes"),
        (json!({"x": "!".repeat(43)}), "base64url"),
        (json!({"d": "private"}), "private key"),
    ];
    let method = RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new()));
    for (mutation, expected) in cases {
        let mut public_key = ed25519_public_jwk(&generate_ed25519_private_key());
        for (key, value) in mutation.as_object().required()? {
            public_key.insert(key.clone(), value.clone());
        }
        let error = method
            .register(&obj(json!({"userId": "user-1", "publicKey": public_key})))
            .err_or_fail()?;
        assert!(error.to_string().contains(expected), "{mutation}: {error}");
    }
    Ok(())
}

#[test]
fn ed25519_registration_is_random_idempotent_and_conflict_safe() -> TestResult {
    let method = RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new()));
    let public_key = ed25519_public_jwk(&generate_ed25519_private_key());
    let first = method.register(&obj(
        json!({"userId": "user-1", "publicKey": public_key, "metadata": {"label": "primary"}}),
    ))?;
    let repeated = method.register(&obj(json!({
        "userId": "user-1",
        "publicKey": public_key,
        "metadata": {"label": "ignored on idempotent retry"},
    })))?;
    assert_eq!(first["created"], true);
    assert_eq!(first["registered"], true);
    let credential_id = first["credential"]["credentialId"].as_str().required()?;
    assert!(credential_id.starts_with("cred_"));
    assert_ne!(credential_id, "user-1");
    assert_eq!(repeated["created"], false);
    assert_eq!(repeated["credential"], first["credential"]);
    assert_eq!(first["credential"]["metadata"], json!({"label": "primary"}));

    let error = method
        .register(&obj(json!({"userId": "user-2", "publicKey": public_key})))
        .err_or_fail()?;
    match error {
        A4PError::Protocol(protocol) => {
            assert_eq!(protocol.code, "CREDENTIAL_KEY_CONFLICT");
            assert_eq!(protocol.http_status, 409);
        }
        other => {
            return Err(ap_support::testing::TestFailure::new(format!("unexpected error {other:?}")).into());
        }
    }
    Ok(())
}

#[test]
fn ed25519_registration_rejects_invalid_request_shape() -> TestResult {
    let cases = [
        (json!({"userId": "", "publicKey": {}}), "userId missing"),
        (json!({"userId": "user-1", "publicKey": null}), "JWK object"),
        (
            json!({
                "userId": "user-1",
                "publicKey": ed25519_public_jwk(&generate_ed25519_private_key()),
                "metadata": [],
            }),
            "metadata must be an object",
        ),
    ];
    for (request, expected) in cases {
        let error = RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new()))
            .register(&obj(request))
            .err_or_fail()?;
        assert!(error.to_string().contains(expected), "{error}");
    }
    Ok(())
}

#[tokio::test]
async fn ed25519_method_rejects_malformed_registered_proofs() -> TestResult {
    let (server, method, _private_key, signer) = ed25519_server()?;
    let prepared = server
        .prepare_operation_authorization(operation_request("user-1"))
        .await?;
    let mandate = prepared.mandate.required()?;
    let context = operation_user_signature_context(&mandate, Some("user-1"))?;

    assert_eq!(
        method.verify(&context, &JsonDict::new()),
        Err("User credentialId missing".into())
    );
    let mut signature = obj(json!({"signatureMethod": "ed25519", "credentialId": signer.credential_id()}));
    assert_eq!(
        method.verify(&context, &signature),
        Err("User signature proof missing".into())
    );
    signature.insert("proof".into(), json!({"alg": "EdDSA", "signature": ""}));
    assert_eq!(
        method.verify(&context, &signature),
        Err("User signature missing".into())
    );

    let store = method.credential_store();
    let record = store.get(signer.credential_id())?.required()?;
    let mut mismatched = record.clone();
    mismatched.signature_method = "webauthn".into();
    store.save(mismatched)?;
    assert_eq!(
        method.verify(&context, &signature),
        Err("User credential signature method mismatch".into())
    );
    let mut bad_key = record.clone();
    bad_key.public_key.insert("x".into(), Value::String("bad".into()));
    store.save(bad_key)?;
    signature.insert("proof".into(), json!({"alg": "EdDSA", "signature": "AAAA"}));
    let reason = method.verify(&context, &signature).err_or_fail()?;
    assert!(
        reason.contains("Registered Ed25519 public key invalid"),
        "{reason}"
    );

    let error = Ed25519UserSigner::new("", generate_ed25519_private_key()).err_or_fail()?;
    assert!(error.to_string().contains("credential_id missing"));
    Ok(())
}

#[test]
fn registration_endpoint_must_match_configured_method() -> TestResult {
    let (ed_server, _method, _key, _signer) = ed25519_server()?;
    let error = ed_server
        .webauthn_registration_options(&obj(json!({"userId": "user-1"})))
        .err_or_fail()?;
    assert_eq!(error.code(), Some("SIGNATURE_METHOD_NOT_ENABLED"));
    assert!(
        error
            .to_string()
            .contains("Signature method 'webauthn' is not enabled; configured method is 'ed25519'")
    );
    let error = ed_server
        .verify_webauthn_registration(&obj(json!({"userId": "user-1"})))
        .err_or_fail()?;
    assert_eq!(error.code(), Some("SIGNATURE_METHOD_NOT_ENABLED"));

    let webauthn_server = webauthn_server(Arc::new(WebAuthnSignatureMethod::new(Arc::new(
        InMemoryCredentialStore::new(),
    ))))?;
    let error = webauthn_server
        .register_ed25519_credential(&obj(json!({
            "userId": "user-1",
            "publicKey": ed25519_public_jwk(&generate_ed25519_private_key()),
        })))
        .err_or_fail()?;
    assert_eq!(error.code(), Some("SIGNATURE_METHOD_NOT_ENABLED"));

    let unsigned = A4PServer::builder()
        .require_user_signature(false)
        .intent_token_usage_store(usage_store())
        .build()?;
    let error = unsigned
        .register_ed25519_credential(&obj(json!({"userId": "user-1"})))
        .err_or_fail()?;
    assert!(error.to_string().contains("configured method is None"), "{error}");
    Ok(())
}

#[tokio::test]
async fn prepare_fails_stably_without_registered_credential_and_adds_no_pending() -> TestResult {
    let method = Arc::new(RegisteredEd25519Method::new(Arc::new(
        InMemoryCredentialStore::new(),
    )));
    let server = A4PServer::builder()
        .user_signature_method(method)
        .intent_token_usage_store(usage_store())
        .build()?;
    let prepared = server
        .prepare_operation_authorization(operation_request("user-1"))
        .await?;
    assert!(prepared.mandate.is_none());
    let result = prepared.verification_result.required()?;
    assert_eq!(result.code.as_deref(), Some("USER_CREDENTIAL_NOT_REGISTERED"));
    assert_eq!(
        prepared.reject_reason.as_deref(),
        Some("No 'ed25519' credential is registered for user: user-1")
    );
    assert_eq!(server.operation_service().pending_len(), 0);

    let intent = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "intent": {"actions": [{"name": "read_note", "params": {}}]},
        })))
        .await?;
    assert!(intent.mandate.is_none());
    assert_eq!(
        intent.verification_result.required()?.code.as_deref(),
        Some("USER_CREDENTIAL_NOT_REGISTERED")
    );
    assert_eq!(server.intent_service().pending_len(), 0);
    Ok(())
}

#[tokio::test]
async fn ed25519_operation_and_intent_use_common_envelope() -> TestResult {
    let (server, _method, _key, signer) = ed25519_server()?;
    let request = operation_request("user-1");
    let prepared = server.prepare_operation_authorization(&request).await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(
        mandate["userAuthorization"],
        json!({"required": true, "signatureMethod": "ed25519", "methodPolicy": {}})
    );
    assert_eq!(
        Value::Object(prepared.signing_options),
        json!({
            "signatureMethod": "ed25519",
            "methodOptions": {"allowedCredentialIds": [signer.credential_id()]},
        })
    );
    let signed = sign_user_mandate_with_signer(&mandate, &signer, None)?;
    assert_eq!(signed["signatures"]["user"]["proof"]["alg"], "EdDSA");
    let mut keys: Vec<&String> = signed["signatures"]["user"]
        .as_object()
        .required()?
        .keys()
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["credentialId", "proof", "signatureMethod"]);
    let completed = server
        .complete_operation_authorization(obj(json!({
            "signedMandate": signed,
            "operation": request["operation"],
        })))
        .await?;
    assert!(completed.approved, "{:?}", completed.reject_reason);

    let intent_prepared = server
        .prepare_intent_authorization(obj(json!({
            "agentId": "agent-1",
            "userId": "user-1",
            "intent": {"actions": [{"name": "read_note", "params": {"note_id": "*"}}]},
            "validitySeconds": 60,
        })))
        .await?;
    let intent_signed = sign_user_mandate_with_signer(&intent_prepared.mandate.required()?, &signer, None)?;
    let intent_completed = server
        .complete_intent_authorization(obj(json!({"signedMandate": intent_signed})))
        .await?;
    assert!(intent_completed.approved);
    assert!(intent_completed.intent_token.is_some());
    Ok(())
}

#[tokio::test]
async fn ed25519_authorization_rejects_wrong_proof_fields() -> TestResult {
    type Mutation = fn(&mut JsonDict);
    let cases: [(Mutation, &str); 4] = [
        (
            |signature| {
                signature.insert("signatureMethod".into(), json!("webauthn"));
            },
            "method mismatch",
        ),
        (
            |signature| {
                signature["proof"]["alg"] = json!("ES256");
            },
            "alg must be 'EdDSA'",
        ),
        (
            |signature| {
                signature.insert("credentialId".into(), json!("cred_unknown"));
            },
            "not registered",
        ),
        (
            |signature| {
                signature["proof"]["signature"] = json!("AAAA");
            },
            "signature invalid",
        ),
    ];
    for (mutation, expected) in cases {
        let (server, _method, _key, signer) = ed25519_server()?;
        let prepared = server
            .prepare_operation_authorization(operation_request("user-1"))
            .await?;
        let mut signed = sign_user_mandate_with_signer(&prepared.mandate.required()?, &signer, None)?;
        let mut user_signature = signed["signatures"]["user"].as_object().required()?.clone();
        mutation(&mut user_signature);
        signed["signatures"]["user"] = Value::Object(user_signature);
        let completed = server
            .complete_operation_authorization(obj(json!({
                "signedMandate": signed,
                "operation": operation_request("user-1")["operation"],
            })))
            .await?;
        assert!(!completed.approved);
        let reason = completed.reject_reason.unwrap_or_default();
        assert!(reason.contains(expected), "{reason}");
    }
    Ok(())
}

#[tokio::test]
async fn ed25519_authorization_rejects_wrong_user_and_mandate_tampering() -> TestResult {
    let (server, method, _key, _signer) = ed25519_server()?;
    let attacker_key = generate_ed25519_private_key();
    let attacker_registration = method.register(&obj(
        json!({"userId": "user-2", "publicKey": ed25519_public_jwk(&attacker_key)}),
    ))?;
    let attacker_signer = Ed25519UserSigner::new(
        attacker_registration["credential"]["credentialId"]
            .as_str()
            .required()?,
        attacker_key,
    )?;
    let prepared = server
        .prepare_operation_authorization(operation_request("user-1"))
        .await?;
    let mandate = prepared.mandate.required()?;
    let wrong_user = sign_user_mandate_with_signer(&mandate, &attacker_signer, None)?;
    let rejected = server
        .complete_operation_authorization(obj(json!({
            "signedMandate": wrong_user,
            "operation": operation_request("user-1")["operation"],
        })))
        .await?;
    assert!(!rejected.approved);
    assert!(
        rejected
            .reject_reason
            .unwrap_or_default()
            .contains("user mismatch")
    );

    let mut tampered = mandate.clone();
    tampered.insert("displayText".into(), json!("attacker-controlled"));
    let tampered = sign_user_mandate_with_signer(&tampered, &attacker_signer, None)?;
    let rejected_tamper = server
        .complete_operation_authorization(obj(json!({
            "signedMandate": tampered,
            "operation": operation_request("user-1")["operation"],
        })))
        .await?;
    assert!(!rejected_tamper.approved);
    assert_eq!(
        rejected_tamper.verification_result.required()?.code.as_deref(),
        Some("MANDATE_PENDING_MISMATCH")
    );
    Ok(())
}

#[tokio::test]
async fn ed25519_signing_options_include_multiple_credentials() -> TestResult {
    let method = Arc::new(RegisteredEd25519Method::new(Arc::new(
        InMemoryCredentialStore::new(),
    )));
    let server = A4PServer::builder()
        .user_signature_method(method.clone())
        .intent_token_usage_store(usage_store())
        .build()?;
    let mut credential_ids = Vec::new();
    for _ in 0..2 {
        let registration = method.register(&obj(json!({
            "userId": "user-1",
            "publicKey": ed25519_public_jwk(&generate_ed25519_private_key()),
        })))?;
        credential_ids.push(registration["credential"]["credentialId"].clone());
    }
    let prepared = server
        .prepare_operation_authorization(operation_request("user-1"))
        .await?;
    assert_eq!(
        prepared.signing_options["methodOptions"]["allowedCredentialIds"],
        Value::Array(credential_ids)
    );
    Ok(())
}

#[test]
fn webauthn_registration_options_verify_and_replay() -> TestResult {
    let store = Arc::new(InMemoryCredentialStore::new());
    let method = WebAuthnSignatureMethod::new(store.clone())
        .with_rp_id("example.test")
        .with_rp_name("Example")
        .with_expected_origin("https://example.test");
    let mut authenticator = SoftwareAuthenticator::new_p256("example.test", "https://example.test");
    authenticator.sign_count = 1;

    let options = method.registration_options("user-1", None, None)?;
    assert!(
        options["registrationRequestId"]
            .as_str()
            .required()?
            .starts_with("webauthn_registration_")
    );
    assert_eq!(options["userId"], "user-1");
    let creation = options["options"].as_object().required()?;
    assert_eq!(creation["rp"], json!({"name": "Example", "id": "example.test"}));
    assert_eq!(
        creation["user"],
        json!({"id": b64url_encode(b"user-1"), "name": "user-1", "displayName": "user-1"})
    );
    assert_eq!(
        creation["pubKeyCredParams"][0],
        json!({"type": "public-key", "alg": -7})
    );
    assert_eq!(creation["timeout"], 60000);
    assert_eq!(creation["excludeCredentials"], json!([]));
    assert_eq!(
        creation["authenticatorSelection"],
        json!({"residentKey": "required", "requireResidentKey": true, "userVerification": "required"})
    );
    assert_eq!(creation["attestation"], "none");
    assert_eq!(method.pending_registration_count(), 1);

    let credential = authenticator.register(creation["challenge"].as_str().required()?)?;
    let record = method.verify_registration(&obj(json!({
        "userId": "user-1",
        "registrationRequestId": options["registrationRequestId"],
        "credential": credential,
    })))?;
    assert_eq!(record.signature_method, "webauthn");
    assert_eq!(record.credential_id, authenticator.credential_id_b64());
    assert_eq!(
        Value::Object(record.public_key.clone()),
        json!({"format": "cose", "value": b64url_encode(&authenticator.cose_public_key()?)})
    );
    assert_eq!(record.details["signCount"], 1);
    assert_eq!(record.details["transports"], json!(["internal"]));
    assert_eq!(record.details["rpId"], "example.test");
    assert_eq!(record.details["origin"], "https://example.test");
    assert_eq!(record.details["aaguid"], "11111111-1111-1111-1111-111111111111");
    assert_eq!(record.details["fmt"], "none");
    assert_eq!(record.details["credentialDeviceType"], "single_device");
    assert_eq!(record.details["credentialBackedUp"], false);
    assert!(store.get(&record.credential_id)?.is_some());
    assert_eq!(method.pending_registration_count(), 0);

    let error = method
        .verify_registration(&obj(json!({
            "userId": "user-1",
            "registrationRequestId": options["registrationRequestId"],
            "credential": credential,
        })))
        .err_or_fail()?;
    assert!(
        error.to_string().contains("No WebAuthn registration challenge"),
        "{error}"
    );

    // A second registration for the same user excludes the existing credential.
    let second = method.registration_options("user-1", Some("Alice"), Some("Alice Example"))?;
    assert_eq!(
        second["options"]["excludeCredentials"],
        json!([{"id": authenticator.credential_id_b64(), "type": "public-key"}])
    );
    assert_eq!(second["options"]["user"]["name"], "Alice");
    assert_eq!(second["options"]["user"]["displayName"], "Alice Example");
    Ok(())
}

#[test]
fn webauthn_registration_verifies_eddsa_rsa_and_packed_self_attestation() -> TestResult {
    let method = WebAuthnSignatureMethod::new(Arc::new(InMemoryCredentialStore::new()))
        .with_rp_id("example.test")
        .with_expected_origin("https://example.test");
    let mut authenticators = [
        SoftwareAuthenticator::new_ed25519("example.test", "https://example.test"),
        SoftwareAuthenticator::new_rsa("example.test", "https://example.test")?,
        SoftwareAuthenticator::new_p256("example.test", "https://example.test"),
    ];
    authenticators[2].attestation_format = "packed".into();
    for (index, authenticator) in authenticators.iter_mut().enumerate() {
        let record = enroll(&method, authenticator, &format!("user-{index}"))?;
        assert_eq!(record.details["fmt"], authenticator.attestation_format);
        // The registered key verifies a real assertion.
        let mandate = create_operation_mandate(CreateOperationMandate {
            user_signature_method: Some("webauthn"),
            ..CreateOperationMandate::new(&obj(json!({"action": "read_note", "params": {}})), "local://test")
        })?;
        let context = operation_user_signature_context(&mandate, Some(&format!("user-{index}")))?;
        let challenge = a4p::user_authorization_challenge_base64url(&mandate)?;
        let assertion = authenticator.assert(&challenge)?;
        let signed = sign_user_mandate_with_signer(
            &mandate,
            &WebAuthnUserSigner::new(),
            Some(&obj(json!({"assertion": assertion}))),
        )?;
        let user_signature = signed["signatures"]["user"].as_object().required()?;
        assert_eq!(
            method.verify(&context, user_signature),
            Ok(()),
            "authenticator {index}"
        );
    }
    Ok(())
}

#[test]
fn webauthn_registration_rejects_bad_ceremonies() -> TestResult {
    let method = WebAuthnSignatureMethod::new(Arc::new(InMemoryCredentialStore::new()))
        .with_rp_id("example.test")
        .with_expected_origin("https://example.test");
    let verify = |credential: JsonDict, options: &JsonDict| {
        method
            .verify_registration(&obj(json!({
                "userId": "user-1",
                "registrationRequestId": options["registrationRequestId"],
                "credential": credential,
            })))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
    };

    let options = method.registration_options("user-1", None, None)?;
    let mut wrong_origin = SoftwareAuthenticator::new_p256("example.test", "https://evil.test");
    let error = verify(
        wrong_origin.register(options["options"]["challenge"].as_str().required()?)?,
        &options,
    );
    assert!(error.contains("Unexpected client data origin"), "{error}");

    let options = method.registration_options("user-1", None, None)?;
    let mut wrong_rp = SoftwareAuthenticator::new_p256("other.test", "https://example.test");
    let error = verify(
        wrong_rp.register(options["options"]["challenge"].as_str().required()?)?,
        &options,
    );
    assert!(error.contains("Unexpected RP ID hash"), "{error}");

    let options = method.registration_options("user-1", None, None)?;
    let mut good = SoftwareAuthenticator::new_p256("example.test", "https://example.test");
    let error = verify(good.register(&b64url_encode(b"another-challenge"))?, &options);
    assert!(
        error.contains("Client data challenge was not expected challenge"),
        "{error}"
    );

    let options = method.registration_options("user-1", None, None)?;
    let mut no_uv = SoftwareAuthenticator::new_p256("example.test", "https://example.test");
    no_uv.user_verified = false;
    let error = verify(
        no_uv.register(options["options"]["challenge"].as_str().required()?)?,
        &options,
    );
    assert!(error.contains("User verification is required"), "{error}");

    let options = method.registration_options("user-1", None, None)?;
    let mut unsupported = SoftwareAuthenticator::new_p256("example.test", "https://example.test");
    unsupported.attestation_format = "tpm".into();
    let error = verify(
        unsupported.register(options["options"]["challenge"].as_str().required()?)?,
        &options,
    );
    assert!(error.contains("Unsupported attestation type"), "{error}");

    let options = method.registration_options("user-1", None, None)?;
    let mut mismatched_id = good.register(options["options"]["challenge"].as_str().required()?)?;
    mismatched_id.insert("id".into(), json!("different"));
    let error = verify(mismatched_id, &options);
    assert!(error.contains("id and raw_id were not equivalent"), "{error}");

    let options = method.registration_options("user-1", None, None)?;
    let error = verify(obj(json!({"id": "x"})), &options);
    assert!(error.contains("Credential missing required rawId"), "{error}");
    Ok(())
}

#[test]
fn webauthn_registration_rejects_invalid_state_and_payload() -> TestResult {
    let method = WebAuthnSignatureMethod::new(Arc::new(InMemoryCredentialStore::new()));
    let error = method.verify_registration(&JsonDict::new()).err_or_fail()?;
    assert!(error.to_string().contains("userId missing"));
    let error = method
        .verify_registration(&obj(json!({"userId": "user-1"})))
        .err_or_fail()?;
    assert!(error.to_string().contains("registrationRequestId missing"));

    let mismatch = method.registration_options("user-1", None, None)?;
    let error = method
        .verify_registration(&obj(json!({
            "userId": "user-2",
            "registrationRequestId": mismatch["registrationRequestId"],
            "credential": {},
        })))
        .err_or_fail()?;
    assert!(
        error.to_string().contains("registration user mismatch"),
        "{error}"
    );

    let missing_credential = method.registration_options("user-1", None, None)?;
    let error = method
        .verify_registration(&obj(json!({
            "userId": "user-1",
            "registrationRequestId": missing_credential["registrationRequestId"],
        })))
        .err_or_fail()?;
    assert!(error.to_string().contains("credential missing"), "{error}");
    Ok(())
}

#[test]
fn webauthn_prepare_requires_registered_method_credential() -> TestResult {
    let method = WebAuthnSignatureMethod::new(Arc::new(InMemoryCredentialStore::with_records(vec![
        UserCredentialRecord::new("user-1", "ed-credential", "ed25519", JsonDict::new()),
    ])));
    let mandate = create_operation_mandate(CreateOperationMandate {
        user_signature_method: Some("webauthn"),
        ..CreateOperationMandate::new(&obj(json!({"action": "read_note", "params": {}})), "local://test")
    })?;
    let error = method.signing_options("user-1", &mandate).err_or_fail()?;
    assert!(
        error
            .to_string()
            .contains("No 'webauthn' credential is registered"),
        "{error}"
    );
    assert_eq!(error.code(), Some("USER_CREDENTIAL_NOT_REGISTERED"));
    Ok(())
}

#[tokio::test]
async fn webauthn_signing_options_assertion_and_sign_count_update() -> TestResult {
    let store = Arc::new(InMemoryCredentialStore::new());
    let method = Arc::new(
        WebAuthnSignatureMethod::new(store.clone())
            .with_rp_id("example.test")
            .with_expected_origin("https://example.test"),
    );
    let mut authenticator = SoftwareAuthenticator::new_p256("example.test", "https://example.test");
    authenticator.sign_count = 1;
    let record = enroll(&method, &mut authenticator, "user-1")?;
    let credential_id = record.credential_id.clone();
    let server = webauthn_server(method.clone())?;
    let prepared = server
        .prepare_operation_authorization(operation_request("user-1"))
        .await?;
    let mandate = prepared.mandate.required()?;
    assert_eq!(
        mandate["userAuthorization"],
        json!({"required": true, "signatureMethod": "webauthn", "methodPolicy": {"userVerification": "required"}})
    );
    let method_options = prepared.signing_options["methodOptions"].as_object().required()?;
    assert_eq!(prepared.signing_options["signatureMethod"], "webauthn");
    assert_eq!(
        method_options["allowCredentials"],
        json!([{"id": credential_id, "type": "public-key"}])
    );
    assert_eq!(method_options["userVerification"], "required");
    assert_eq!(method_options["rpId"], "example.test");
    assert_eq!(
        method_options["challenge"],
        Value::String(a4p::user_authorization_challenge_base64url(&mandate)?)
    );

    let assertion = authenticator.assert(method_options["challenge"].as_str().required()?)?;
    let signed = sign_user_mandate_with_signer(
        &mandate,
        &WebAuthnUserSigner::new(),
        Some(&obj(json!({"assertion": assertion}))),
    )?;
    let context = operation_user_signature_context(&signed, Some("user-1"))?;
    let user_signature = signed["signatures"]["user"].as_object().required()?.clone();
    assert_eq!(method.verify(&context, &user_signature), Ok(()));
    let updated = store.get(&credential_id)?.required()?;
    assert_eq!(updated.details["signCount"], 2);

    // Replaying the same assertion fails the sign count check.
    let reason = method.verify(&context, &user_signature).err_or_fail()?;
    assert!(reason.contains("sign count"), "{reason}");

    let completed = server
        .complete_operation_authorization(obj(json!({
            "signedMandate": signed,
            "operation": operation_request("user-1")["operation"],
        })))
        .await?;
    assert!(!completed.approved);
    assert_eq!(
        completed.verification_result.required()?.code.as_deref(),
        Some("MANDATE_SIGNATURE_INVALID")
    );

    // A fresh assertion completes the still pending authorization.
    let assertion = authenticator.assert(method_options["challenge"].as_str().required()?)?;
    let signed = sign_user_mandate_with_signer(
        &mandate,
        &WebAuthnUserSigner::new(),
        Some(&obj(json!({"assertion": assertion}))),
    )?;
    let completed = server
        .complete_operation_authorization(obj(json!({
            "signedMandate": signed,
            "operation": operation_request("user-1")["operation"],
        })))
        .await?;
    assert!(completed.approved, "{:?}", completed.reject_reason);
    assert_eq!(store.get(&credential_id)?.required()?.details["signCount"], 3);
    Ok(())
}

#[test]
fn webauthn_verify_rejects_credential_binding_and_verifier_failures() -> TestResult {
    let credential_id = b64url_encode(b"credential-id");
    let mandate = create_operation_mandate(CreateOperationMandate {
        user_signature_method: Some("webauthn"),
        ..CreateOperationMandate::new(&obj(json!({"action": "read_note", "params": {}})), "local://test")
    })?;
    let context = operation_user_signature_context(&mandate, Some("user-1"))?;
    let base_signature = obj(json!({
        "signatureMethod": "webauthn",
        "credentialId": credential_id,
        "proof": {"assertion": {"id": credential_id}},
    }));

    let empty_method = WebAuthnSignatureMethod::new(Arc::new(InMemoryCredentialStore::new()));
    assert_eq!(
        empty_method.verify(&context, &JsonDict::new()),
        Err("WebAuthn credentialId missing".into())
    );
    assert_eq!(
        empty_method.verify(&context, &base_signature),
        Err(format!("WebAuthn credential not registered: {credential_id}"))
    );

    let mismatched_method = UserCredentialRecord::new("user-1", &credential_id, "ed25519", JsonDict::new());
    let method = WebAuthnSignatureMethod::new(Arc::new(InMemoryCredentialStore::with_records(vec![
        mismatched_method.clone(),
    ])));
    assert_eq!(
        method.verify(&context, &base_signature),
        Err("WebAuthn credential signature method mismatch".into())
    );

    let mut valid_record = mismatched_method.clone();
    valid_record.user_id = "user-2".into();
    valid_record.signature_method = "webauthn".into();
    valid_record.public_key = obj(json!({"format": "cose", "value": b64url_encode(b"key")}));
    let store = Arc::new(InMemoryCredentialStore::with_records(vec![valid_record.clone()]));
    let method = WebAuthnSignatureMethod::new(store.clone());
    let reason = method.verify(&context, &base_signature).err_or_fail()?;
    assert!(reason.contains("credential user mismatch"), "{reason}");

    let mut owned = valid_record.clone();
    owned.user_id = "user-1".into();
    store.save(owned)?;
    let reason = method.verify(&context, &base_signature).err_or_fail()?;
    assert!(reason.starts_with("WebAuthn signature invalid: "), "{reason}");

    // A real assertion without user verification is rejected after signature checks.
    let store = Arc::new(InMemoryCredentialStore::new());
    let method = WebAuthnSignatureMethod::new(store.clone())
        .with_rp_id("example.test")
        .with_expected_origin("https://example.test");
    let mut authenticator = SoftwareAuthenticator::new_p256("example.test", "https://example.test");
    enroll(&method, &mut authenticator, "user-1")?;
    authenticator.user_verified = false;
    let challenge = a4p::user_authorization_challenge_base64url(&mandate)?;
    let assertion = authenticator.assert(&challenge)?;
    let signature = obj(json!({
        "signatureMethod": "webauthn",
        "credentialId": authenticator.credential_id_b64(),
        "proof": {"assertion": assertion},
    }));
    let reason = method.verify(&context, &signature).err_or_fail()?;
    assert!(reason.contains("User verification is required"), "{reason}");
    Ok(())
}

#[test]
fn webauthn_user_signer_requires_assertion_and_credential_id() -> TestResult {
    let mandate = create_operation_mandate(CreateOperationMandate {
        user_signature_method: Some("webauthn"),
        ..CreateOperationMandate::new(&obj(json!({"action": "read_note", "params": {}})), "local://test")
    })?;
    let context = operation_user_signature_context(&mandate, None)?;
    let signer = WebAuthnUserSigner::new();
    let error = signer.sign(&context, None).err_or_fail()?;
    assert!(error.to_string().contains("assertion missing"));
    let error = signer
        .sign(&context, Some(&obj(json!({"assertion": {}}))))
        .err_or_fail()?;
    assert!(error.to_string().contains("credentialId missing"));

    let ed_signer = Ed25519UserSigner::new("cred", generate_ed25519_private_key())?;
    let error = sign_user_mandate_with_signer(&mandate, &ed_signer, None).err_or_fail()?;
    assert!(
        error.to_string().contains("User signer method mismatch"),
        "{error}"
    );
    let mut unknown = mandate.clone();
    unknown.insert("type".into(), json!("a4p/v9/mandate"));
    let error = sign_user_mandate_with_signer(&unknown, &signer, None).err_or_fail()?;
    assert!(
        error
            .to_string()
            .contains("Unsupported A4P mandate type: 'a4p/v9/mandate'"),
        "{error}"
    );
    Ok(())
}

#[tokio::test]
async fn webauthn_rejects_rp_origin_uv_and_bad_assertion() -> TestResult {
    let credential_id = b64url_encode(b"credential-id");
    let mut record = UserCredentialRecord::new(
        "user-1",
        &credential_id,
        "webauthn",
        obj(json!({"format": "cose", "value": b64url_encode(b"key")})),
    );
    record.details = obj(json!({"signCount": 0, "rpId": "wrong.example", "origin": "https://wrong.example"}));
    let store = Arc::new(InMemoryCredentialStore::with_records(vec![record]));
    let method = Arc::new(
        WebAuthnSignatureMethod::new(store.clone())
            .with_rp_id("example.test")
            .with_expected_origin("https://example.test"),
    );
    let mandate = webauthn_server(method.clone())?
        .prepare_operation_authorization(operation_request("user-1"))
        .await?
        .mandate
        .required()?;
    let context = operation_user_signature_context(&mandate, Some("user-1"))?;
    let mut signature = obj(json!({
        "signatureMethod": "webauthn",
        "credentialId": credential_id,
        "proof": {"assertion": {"id": credential_id}},
    }));

    let reason = method.verify(&context, &signature).err_or_fail()?;
    assert!(reason.contains("RP ID mismatch"), "{reason}");

    let method = WebAuthnSignatureMethod::new(store.clone())
        .with_rp_id("wrong.example")
        .with_expected_origin("https://example.test");
    let reason = method.verify(&context, &signature).err_or_fail()?;
    assert!(reason.contains("origin mismatch"), "{reason}");

    let method = WebAuthnSignatureMethod::new(store)
        .with_rp_id("wrong.example")
        .with_expected_origin("https://wrong.example");
    signature.insert("proof".into(), json!({}));
    assert_eq!(
        method.verify(&context, &signature),
        Err("WebAuthn assertion missing".into())
    );
    Ok(())
}

#[tokio::test]
async fn no_signature_mode_rejects_nonempty_user_signature() -> TestResult {
    let server = A4PServer::builder()
        .require_user_signature(false)
        .intent_token_usage_store(usage_store())
        .build()?;
    let request = operation_request("user-1");
    let prepared = server.prepare_operation_authorization(&request).await?;
    let mut unsigned = approve_user_mandate(&prepared.mandate.required()?);
    unsigned["signatures"]["user"] = json!({"signatureMethod": "ed25519"});
    let result = server
        .complete_operation_authorization(obj(json!({
            "signedMandate": unsigned,
            "operation": request["operation"],
        })))
        .await?;
    assert!(!result.approved);
    assert_eq!(
        result.reject_reason.as_deref(),
        Some("User signature must be empty")
    );
    Ok(())
}
