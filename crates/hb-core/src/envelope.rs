use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::crypto::{self, HoardbookKeypair};
use crate::error::HbError;
use crate::jcs;

// ---------------------------------------------------------------------------
// DocType
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocType {
    Profile,
    Collection,
    Succession,
    Message,
    Channel,
}

// ---------------------------------------------------------------------------
// SignedEnvelope
// ---------------------------------------------------------------------------

/// The wire format for all signed Hoardbook documents.
///
/// ```json
/// {
///   "doc_type":   "profile",
///   "payload":    { ...typed payload... },
///   "public_key": "hb1_...",
///   "signature":  "<hex Ed25519 sig over JCS(payload)>",
///   "signed_at":  "2026-04-15T00:00:00Z"
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedEnvelope {
    pub doc_type: DocType,
    /// The typed document (Profile / Collection / Succession) as a JSON value.
    pub payload: serde_json::Value,
    /// The author's Hoardbook ID (`hb1_...`).
    pub public_key: String,
    /// Hex-encoded 64-byte Ed25519 signature over JCS(payload).
    pub signature: String,
    pub signed_at: DateTime<Utc>,
}

impl SignedEnvelope {
    /// Create a new signed envelope for any serializable payload.
    pub fn create<T: Serialize>(
        keypair: &HoardbookKeypair,
        doc_type: DocType,
        payload: &T,
    ) -> Result<Self, HbError> {
        let payload_value = serde_json::to_value(payload)?;
        let signature = keypair.sign(&payload_value);
        Ok(Self {
            doc_type,
            payload: payload_value,
            public_key: keypair.hb_id(),
            signature,
            signed_at: Utc::now(),
        })
    }

    /// Verify the envelope's signature. Returns `Ok(())` if valid.
    pub fn verify(&self) -> Result<(), HbError> {
        let pubkey_bytes = crypto::hb_id_decode(&self.public_key)?;
        crypto::verify(&pubkey_bytes, &self.payload, &self.signature)
    }

    /// Deserialize the payload into a concrete type.
    pub fn parse_payload<T: for<'de> Deserialize<'de>>(&self) -> Result<T, HbError> {
        Ok(serde_json::from_value(self.payload.clone())?)
    }

    /// Return the JCS-canonical bytes of the payload (useful for debugging).
    pub fn canonical_payload_bytes(&self) -> Vec<u8> {
        jcs::canonicalize(&self.payload)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::HoardbookKeypair;
    use crate::types::Profile;
    use chrono::Utc;

    fn test_profile() -> Profile {
        Profile {
            display_name: "archivebox_prime".into(),
            bio: Some("Mostly 90s anime".into()),
            tags: vec!["anime".into(), "vhs".into()],
            since: Some(2019),
            est_size: Some("~12TB".into()),
            languages: vec!["en".into()],
            contact_hint: None,
            email: None,
            location: None,
            social_links: vec![],
            updated: Utc::now(),
        }
    }

    #[test]
    fn create_and_verify() {
        let kp = HoardbookKeypair::generate();
        let profile = test_profile();
        let env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        assert_eq!(env.doc_type, DocType::Profile);
        env.verify().expect("signature should be valid");
    }

    #[test]
    fn verify_rejects_tampered_payload() {
        let kp = HoardbookKeypair::generate();
        let profile = test_profile();
        let mut env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        // Tamper with the display_name inside the payload
        env.payload["display_name"] = serde_json::Value::String("hacker".into());
        let result = env.verify();
        assert!(matches!(result, Err(HbError::InvalidSignature)));
    }

    #[test]
    fn parse_payload_roundtrip() {
        let kp = HoardbookKeypair::generate();
        let profile = test_profile();
        let env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        let parsed: Profile = env.parse_payload().unwrap();
        assert_eq!(parsed.display_name, profile.display_name);
        assert_eq!(parsed.bio, profile.bio);
        assert_eq!(parsed.tags, profile.tags);
        assert_eq!(parsed.since, profile.since);
        assert_eq!(parsed.languages, profile.languages);
        assert_eq!(parsed.contact_hint, profile.contact_hint);
        assert_eq!(parsed.email, profile.email);
    }

    #[test]
    fn optional_fields_omitted_not_null() {
        // All Option fields with skip_serializing_if="Option::is_none" must be absent from
        // the JSON payload — not serialized as `null`. This matters for forward compat:
        // an older client receiving an envelope without a new optional field must parse it
        // as None, not fail on an unexpected null.
        let kp = HoardbookKeypair::generate();
        let profile = Profile {
            display_name: "test".into(),
            bio: None,
            tags: vec![],
            since: None,
            est_size: None,
            languages: vec![],
            contact_hint: None,
            email: None,
            location: None,
            social_links: vec![],
            updated: Utc::now(),
        };
        let env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        let payload_json = serde_json::to_string(&env.payload).unwrap();

        assert!(!payload_json.contains("\"bio\""), "absent bio must not appear in JSON");
        assert!(!payload_json.contains("\"since\""), "absent since must not appear in JSON");
        assert!(!payload_json.contains("\"est_size\""), "absent est_size must not appear in JSON");
        assert!(
            !payload_json.contains("\"contact_hint\""),
            "absent contact_hint must not appear in JSON"
        );
        assert!(!payload_json.contains("\"email\""), "absent email must not appear in JSON");
        assert!(
            !payload_json.contains(":null"),
            "no field should be serialized as null"
        );
    }

    #[test]
    fn json_serde_roundtrip() {
        let kp = HoardbookKeypair::generate();
        let profile = test_profile();
        let env = SignedEnvelope::create(&kp, DocType::Profile, &profile).unwrap();
        // Simulate what the relay does: serialize → transmit → deserialize
        let json_str = serde_json::to_string(&env).unwrap();
        let restored: SignedEnvelope = serde_json::from_str(&json_str).unwrap();
        restored.verify().expect("signature should survive JSON roundtrip");
    }
}
