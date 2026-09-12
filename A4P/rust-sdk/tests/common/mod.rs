// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! Shared test support: explicit Ed25519 test method, server builders and a
//! software WebAuthn authenticator that produces real attestations and assertions.
use ap_support::testing::{OptionExt, TestResult};
use std::collections::HashMap;
use std::sync::Arc;

use a4p::credential_store::InMemoryCredentialStore;
use a4p::intent::usage_store::InMemoryIntentTokenUsageStore;
use a4p::mandate_security::mandate_identifier;
use a4p::types::JsonDict;
use a4p::user_signature::ed25519::{Ed25519UserSigner, RegisteredEd25519Method, ed25519_public_jwk};
use a4p::user_signature::webauthn::{COSE_ALG_EDDSA, COSE_ALG_ES256, COSE_ALG_RS256, b64url_encode};
use a4p::user_signature::{A4PUserSignatureMethod, UserSignatureContext};
use a4p::{A4PError, A4PServer, A4PServerBuilder, sign_user_mandate_with_signer};
use ciborium::Value as CborValue;
use ed25519_dalek::SigningKey;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

static SIGNERS_BY_MANDATE_ID: Lazy<Mutex<HashMap<String, Ed25519UserSigner>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Signature method that enrolls ephemeral keys for test request users.
pub struct ExplicitTestEd25519Method {
    inner: RegisteredEd25519Method,
    signers_by_user: Mutex<HashMap<String, Ed25519UserSigner>>,
}

impl Default for ExplicitTestEd25519Method {
    fn default() -> Self {
        Self::new()
    }
}

impl ExplicitTestEd25519Method {
    pub fn new() -> Self {
        Self {
            inner: RegisteredEd25519Method::new(Arc::new(InMemoryCredentialStore::new())),
            signers_by_user: Mutex::new(HashMap::new()),
        }
    }

    pub fn inner(&self) -> &RegisteredEd25519Method {
        &self.inner
    }

    fn signer_for_user(&self, user_id: &str) -> Result<Ed25519UserSigner, A4PError> {
        if let Some(signer) = self.signers_by_user.lock().get(user_id) {
            return Ok(signer.clone());
        }
        let private_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let registration = self.inner.register(&obj(json!({
            "userId": user_id,
            "publicKey": ed25519_public_jwk(&private_key),
            "metadata": {"purpose": "test-only"},
        })))?;
        let credential_id = registration["credential"]["credentialId"]
            .as_str()
            .ok_or_else(|| a4p::A4PError::value("registration did not return a credentialId"))?
            .to_string();
        let signer = Ed25519UserSigner::new(&credential_id, private_key)?;
        self.signers_by_user
            .lock()
            .insert(user_id.to_string(), signer.clone());
        Ok(signer)
    }
}

impl A4PUserSignatureMethod for ExplicitTestEd25519Method {
    fn signature_method(&self) -> &str {
        self.inner.signature_method()
    }

    fn method_policy(&self) -> JsonDict {
        self.inner.method_policy()
    }

    fn signing_options(&self, user_id: &str, mandate: &JsonDict) -> Result<JsonDict, A4PError> {
        let signer = self.signer_for_user(user_id)?;
        SIGNERS_BY_MANDATE_ID
            .lock()
            .insert(mandate_identifier(mandate)?, signer);
        self.inner.signing_options(user_id, mandate)
    }

    fn verify(&self, context: &UserSignatureContext, signature: &JsonDict) -> Result<(), String> {
        self.inner.verify(context, signature)
    }

    fn ed25519_registrar(&self) -> Option<&RegisteredEd25519Method> {
        Some(&self.inner)
    }
}

/// Builder pre-configured with an explicit ephemeral test method and an in-memory usage store.
pub fn explicit_server_builder() -> A4PServerBuilder {
    A4PServer::builder()
        .user_signature_method(Arc::new(ExplicitTestEd25519Method::new()))
        .intent_token_usage_store(Arc::new(InMemoryIntentTokenUsageStore::new()))
}

/// Production server configured with an explicit ephemeral test method.
pub fn explicit_server(server_id: &str) -> TestResult<A4PServer> {
    Ok(explicit_server_builder().server_id(server_id).build()?)
}

/// Server with the default server id and the explicit test method.
pub fn default_explicit_server() -> TestResult<A4PServer> {
    Ok(explicit_server_builder().build()?)
}

/// Sign a prepared mandate using the key enrolled during its prepare call.
pub fn sign_user_mandate(mandate: &JsonDict) -> TestResult<JsonDict> {
    let mandate_id = mandate_identifier(mandate)?;
    let signer = SIGNERS_BY_MANDATE_ID
        .lock()
        .get(&mandate_id)
        .cloned()
        .ok_or_else(|| {
            ap_support::testing::TestFailure::new(format!(
                "No explicit test signer registered for mandate: {mandate_id}"
            ))
        })?;
    Ok(sign_user_mandate_with_signer(mandate, &signer, None)?)
}

/// Convert a `serde_json::json!` object into a `JsonDict`.
pub fn obj(value: Value) -> JsonDict {
    ap_support::json::object(value)
}

/// A user id with a fresh in-memory usage store, for servers that need the default id.
pub fn usage_store() -> Arc<InMemoryIntentTokenUsageStore> {
    Arc::new(InMemoryIntentTokenUsageStore::new())
}

/// PKCS8 RSA-2048 test key used by the RS256 software authenticator.
pub const RSA_TEST_KEY_PEM: &str = "-----BEGIN PRIVATE KEY-----
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQDZGucccujrnliv
2H3sQu9FA6eOD7UhfSUmB2HCpfd8K1wda/L2NCU0nKDmShe0kCOJhiog7U0GgQuh
LqnWnF3+WmuwlWH/skKx924LUCPhu3BxqtCsyOHbZF2CoI6EHnwY4z+xerECeSmK
xA1D47UO8Q91iRevnwprOtoXZ30aO6oQh0lyyT9z7IDa15RsOQrkl8h7/Uxo0YH5
zxfzH5Xk0Qyaj5dBtfzGWJ8/VD3phflU4atRxfuQ5xCvCnkX6eTMDXabAtoza5An
l/LOggqWOegLzoS9f0IJrSp1u4MEGhlUKl1JW97Lv8CFAKcgNEk0L743ji1YpQDp
KqFDH2JFAgMBAAECggEAFT4lI+r4cGmHevk/ZPktqT6Qy/0sd3DjzCNHxQBxRUSG
2FgiJ0X15/51XeMdC61Y1NF8WMlvUnYY+bXzp0yYg9U8GUScmKTMEtbHfTLjt+gb
ufuBPI7RTqK05Z2pJDNJaDQAHPEI1dmeH3ZMZ/qlUidzIEiAOU5h+hkBku2s136V
3rkPtHZ0J4tvIe59suXDS5Jl0Ah1Z4IjlvlgMi0fZ3UAHpbcT/gnR2QXHXgicl6O
pWm8Scp4RTPREseBvh2Hup2yhsR+442xxkUkxlu6imNOErLObUVKJSPECc9SXjf+
JkCaTaQvPH1TdV4/v80LqMzJ7tKoTnEYO/DoFHMQwQKBgQDzDkeXdXBfIcx//Js8
Z6pbmW8oQIQ+swYmaTJ1AgNryFHSehNoPQCyGsfbmfpdgAdg4wYeBOsEKlkxA19v
BaGxvPqRlJMD8uy+3S2hvbnGxNTmyartRCrwjQyuRIiiID8QTWQdlD/FsJ6vFLQ+
c4zAW8QwTcEUChUn+EdweGlWZQKBgQDkqtKC66zjMfv3eMn2Re5j3hWCA5xyai4p
KKgB6g0SHXBffYYwJJtzxKRJLsXPJUmeS2XBBKVje4y5lMUHF6QBoSvhUFTnPJ6f
T0n/fBzNyKhlFU+4ckthDyoMoIbjJN/KjwSqK3WHPnAOUSaobYvAGoh386NqQ8ly
qMhctjmuYQKBgCUXK8OoL0LFNKDfWo0oQK4Dxxu8ZLHwveKEsSd77Cu5gQr+iBGj
JYUIYzFW2QcFr5qQanGQTJDxKXU6T4jwshEehppKsviqTIh/1iPVgREdHmQtqEDW
4zqcO7AoUzVyeE0zkjCVW/n+Dukm3q6dEYCVQGYip3E4bKwRzk0SgvilAoGBANpF
7Rgnmypr5hZ96Fr6uen+bg1jIQ1eKZ4EPwtEvSFTlJayHUsLRpAlXqS0zwFCmJlP
Y1vx8WWa4+OqDMEOYfFkRZyXr9Pi2486gmorsNsF9Sg4RZbNEwMdFIhlGxzrb+vM
xSkivtdQVGp2MC6KEuJW8Xl+ybh/6GVYk5lcIIdBAoGAS0adCzRxE+ZFqF5oBQRh
smNCtPNShMtqjA/BVUmJbs/75dlhMIShSHodUT6toazN10Qfob854yY175YY8EGX
4VDBb65rnK6Xah5AoqN0PxZO6bHvolka/HLw+gggOaJG0Glo7o48if/8hjHTpD+z
/qyZrulL52gK64iui3qL5qE=
-----END PRIVATE KEY-----";

enum AuthenticatorKey {
    P256(p256::ecdsa::SigningKey),
    Ed25519(SigningKey),
    Rsa(rsa::RsaPrivateKey),
}

/// A software WebAuthn authenticator that produces real attestations and assertions.
pub struct SoftwareAuthenticator {
    key: AuthenticatorKey,
    pub rp_id: String,
    pub origin: String,
    pub credential_id: Vec<u8>,
    pub aaguid: [u8; 16],
    pub sign_count: u32,
    pub user_verified: bool,
    pub attestation_format: String,
}

impl SoftwareAuthenticator {
    pub fn new_p256(rp_id: &str, origin: &str) -> Self {
        Self::new(
            AuthenticatorKey::P256(p256::ecdsa::SigningKey::random(&mut rand::rngs::OsRng)),
            rp_id,
            origin,
        )
    }

    pub fn new_ed25519(rp_id: &str, origin: &str) -> Self {
        Self::new(
            AuthenticatorKey::Ed25519(SigningKey::generate(&mut rand::rngs::OsRng)),
            rp_id,
            origin,
        )
    }

    pub fn new_rsa(rp_id: &str, origin: &str) -> TestResult<Self> {
        use rsa::pkcs8::DecodePrivateKey;
        let key = rsa::RsaPrivateKey::from_pkcs8_pem(RSA_TEST_KEY_PEM)?;
        Ok(Self::new(AuthenticatorKey::Rsa(key), rp_id, origin))
    }

    fn new(key: AuthenticatorKey, rp_id: &str, origin: &str) -> Self {
        let mut credential_id = vec![0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut credential_id);
        Self {
            key,
            rp_id: rp_id.to_string(),
            origin: origin.to_string(),
            credential_id,
            aaguid: [0x11; 16],
            sign_count: 0,
            user_verified: true,
            attestation_format: "none".to_string(),
        }
    }

    /// The credential id as unpadded base64url.
    pub fn credential_id_b64(&self) -> String {
        b64url_encode(&self.credential_id)
    }

    /// The CBOR encoded COSE public key.
    pub fn cose_public_key(&self) -> TestResult<Vec<u8>> {
        let int = |v: i64| CborValue::Integer(v.into());
        let map = match &self.key {
            AuthenticatorKey::P256(key) => {
                let point = key.verifying_key().to_encoded_point(false);
                vec![
                    (int(1), int(2)),
                    (int(3), int(COSE_ALG_ES256)),
                    (int(-1), int(1)),
                    (int(-2), CborValue::Bytes(point.x().required()?.to_vec())),
                    (int(-3), CborValue::Bytes(point.y().required()?.to_vec())),
                ]
            }
            AuthenticatorKey::Ed25519(key) => vec![
                (int(1), int(1)),
                (int(3), int(COSE_ALG_EDDSA)),
                (int(-1), int(6)),
                (int(-2), CborValue::Bytes(key.verifying_key().to_bytes().to_vec())),
            ],
            AuthenticatorKey::Rsa(key) => {
                use rsa::traits::PublicKeyParts;
                vec![
                    (int(1), int(3)),
                    (int(3), int(COSE_ALG_RS256)),
                    (int(-1), CborValue::Bytes(key.n().to_bytes_be())),
                    (int(-2), CborValue::Bytes(key.e().to_bytes_be())),
                ]
            }
        };
        let mut out = Vec::new();
        ciborium::into_writer(&CborValue::Map(map), &mut out)?;
        Ok(out)
    }

    fn sign(&self, data: &[u8]) -> Vec<u8> {
        match &self.key {
            AuthenticatorKey::P256(key) => {
                use p256::ecdsa::signature::Signer;
                let signature: p256::ecdsa::Signature = key.sign(data);
                signature.to_der().as_bytes().to_vec()
            }
            AuthenticatorKey::Ed25519(key) => {
                use ed25519_dalek::Signer;
                key.sign(data).to_bytes().to_vec()
            }
            AuthenticatorKey::Rsa(key) => {
                use rsa::signature::{SignatureEncoding, Signer};
                let signing_key = rsa::pkcs1v15::SigningKey::<Sha256>::new(key.clone());
                signing_key.sign(data).to_bytes().to_vec()
            }
        }
    }

    fn flags(&self, attested: bool) -> u8 {
        let mut flags = 0x01;
        if self.user_verified {
            flags |= 0x04;
        }
        if attested {
            flags |= 0x40;
        }
        flags
    }

    fn client_data_json(&self, ceremony_type: &str, challenge_b64url: &str) -> TestResult<Vec<u8>> {
        Ok(serde_json::to_vec(&json!({
            "type": ceremony_type,
            "challenge": challenge_b64url,
            "origin": self.origin,
            "crossOrigin": false,
        }))?)
    }

    /// Run a registration ceremony for `challenge_b64url` and return the credential JSON.
    pub fn register(&mut self, challenge_b64url: &str) -> TestResult<JsonDict> {
        let client_data = self.client_data_json("webauthn.create", challenge_b64url)?;
        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&Sha256::digest(self.rp_id.as_bytes()));
        auth_data.push(self.flags(true));
        auth_data.extend_from_slice(&self.sign_count.to_be_bytes());
        auth_data.extend_from_slice(&self.aaguid);
        auth_data.extend_from_slice(&(self.credential_id.len() as u16).to_be_bytes());
        auth_data.extend_from_slice(&self.credential_id);
        auth_data.extend_from_slice(&self.cose_public_key()?);

        let text = |s: &str| CborValue::Text(s.to_string());
        let att_stmt = if self.attestation_format == "packed" {
            let alg = match &self.key {
                AuthenticatorKey::P256(_) => COSE_ALG_ES256,
                AuthenticatorKey::Ed25519(_) => COSE_ALG_EDDSA,
                AuthenticatorKey::Rsa(_) => COSE_ALG_RS256,
            };
            let mut to_sign = auth_data.clone();
            to_sign.extend_from_slice(&Sha256::digest(&client_data));
            vec![
                (text("alg"), CborValue::Integer(alg.into())),
                (text("sig"), CborValue::Bytes(self.sign(&to_sign))),
            ]
        } else {
            Vec::new()
        };
        let attestation = CborValue::Map(vec![
            (text("fmt"), text(&self.attestation_format)),
            (text("attStmt"), CborValue::Map(att_stmt)),
            (text("authData"), CborValue::Bytes(auth_data)),
        ]);
        let mut attestation_bytes = Vec::new();
        ciborium::into_writer(&attestation, &mut attestation_bytes)?;
        Ok(obj(json!({
            "id": self.credential_id_b64(),
            "rawId": self.credential_id_b64(),
            "type": "public-key",
            "response": {
                "clientDataJSON": b64url_encode(&client_data),
                "attestationObject": b64url_encode(&attestation_bytes),
                "transports": ["internal"],
            },
        })))
    }

    /// Run an authentication ceremony for `challenge_b64url` and return the assertion JSON.
    pub fn assert(&mut self, challenge_b64url: &str) -> TestResult<JsonDict> {
        self.sign_count += 1;
        let client_data = self.client_data_json("webauthn.get", challenge_b64url)?;
        let mut auth_data = Vec::new();
        auth_data.extend_from_slice(&Sha256::digest(self.rp_id.as_bytes()));
        auth_data.push(self.flags(false));
        auth_data.extend_from_slice(&self.sign_count.to_be_bytes());
        let mut to_sign = auth_data.clone();
        to_sign.extend_from_slice(&Sha256::digest(&client_data));
        let signature = self.sign(&to_sign);
        Ok(obj(json!({
            "id": self.credential_id_b64(),
            "rawId": self.credential_id_b64(),
            "type": "public-key",
            "response": {
                "clientDataJSON": b64url_encode(&client_data),
                "authenticatorData": b64url_encode(&auth_data),
                "signature": b64url_encode(&signature),
                "userHandle": b64url_encode(b"user-1"),
            },
        })))
    }
}
