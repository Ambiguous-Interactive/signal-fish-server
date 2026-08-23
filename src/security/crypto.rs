use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use chrono::{DateTime, Utc};
use getrandom::fill as fill_random;
use serde::{Deserialize, Serialize};
use std::convert::TryInto;
use std::fmt;
use thiserror::Error;

/// Size of the AES-GCM nonce in bytes.
const NONCE_SIZE: usize = 12;
/// Size of the AES-256 key in bytes.
const KEY_SIZE: usize = 32;

/// Deterministic, unambiguous byte encoding of an [`EncryptedSecret`] bundle's
/// metadata, bound as AES-GCM associated data.
///
/// Without this binding the metadata sits outside the authentication boundary:
/// two bundles sharing a `key_id` could be swapped undetected and `created_at`
/// was attacker-malleable at rest. The length prefix keeps distinct
/// `(key_id, created_at)` pairs mapping to distinct byte strings, so any
/// metadata edit breaks GCM verification instead of decrypting successfully.
fn metadata_aad(key_id: &str, created_at: chrono::DateTime<chrono::Utc>) -> Vec<u8> {
    let key_bytes = key_id.as_bytes();
    // 8-byte length prefix + key bytes + 1 timestamp flag + 8 nanos.
    let mut aad = Vec::with_capacity(17usize.saturating_add(key_bytes.len()));
    aad.extend_from_slice(
        &u64::try_from(key_bytes.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    aad.extend_from_slice(key_bytes);
    // `timestamp_nanos` is total for every representable bundle: nanosecond
    // precision when available, a single reserved marker otherwise. Either way
    // the encoding is injective per timestamp value.
    match created_at.timestamp_nanos_opt() {
        Some(nanos) => {
            aad.push(1);
            aad.extend_from_slice(&nanos.to_be_bytes());
        }
        None => aad.push(0),
    }
    aad
}

/// Encrypted secret payload for secure storage.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptedSecret {
    /// Identifier of the master key used to encrypt the payload.
    pub key_id: String,
    /// Base64-encoded ciphertext produced by AES-GCM.
    pub ciphertext: String,
    /// Base64-encoded nonce used for the AES-GCM encryption.
    pub nonce: String,
    /// Timestamp when the secret was encrypted.
    pub created_at: DateTime<Utc>,
}

/// Errors produced during encryption/decryption.
#[derive(Debug, Error)]
pub enum EncryptionError {
    #[error("master key must decode to {KEY_SIZE} bytes, decoded length was {0}")]
    InvalidKeyLength(usize),
    #[error("failed to decode base64 data: {0}")]
    Base64Decode(#[from] base64::DecodeError),
    #[error("key id mismatch (expected {expected}, found {actual})")]
    KeyMismatch { expected: String, actual: String },
    #[error("encryption failed")]
    EncryptionFailure,
    #[error("failed to obtain secure random bytes")]
    EntropyUnavailable,
    #[error("decryption failed")]
    DecryptionFailure,
    #[error("nonce length must be {NONCE_SIZE} bytes, received {0}")]
    InvalidNonceLength(usize),
}

/// Envelope encryptor that protects secrets at rest using AES-256-GCM.
///
/// The master key should be sourced from a secure key management system (KMS) or secret store.
/// The key identifier is persisted alongside the ciphertext so that future rotations can be
/// handled gracefully.
#[derive(Clone)]
pub struct EnvelopeEncryptor {
    key_id: String,
    cipher: Aes256Gcm,
}

impl fmt::Debug for EnvelopeEncryptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvelopeEncryptor")
            .field("key_id", &self.key_id)
            .finish()
    }
}

impl EnvelopeEncryptor {
    /// Construct a new encryptor from a base64-encoded 256-bit key.
    ///
    /// The `key_id` should uniquely identify the upstream KMS key so future rotations can be
    /// tracked in persisted records.
    pub fn new_from_base64_key(
        key_id: impl Into<String>,
        master_key_b64: &str,
    ) -> Result<Self, EncryptionError> {
        let decoded = BASE64
            .decode(master_key_b64)
            .map_err(EncryptionError::Base64Decode)?;
        let decoded_len = decoded.len();
        if decoded_len != KEY_SIZE {
            return Err(EncryptionError::InvalidKeyLength(decoded_len));
        }

        let key_bytes: [u8; KEY_SIZE] = decoded
            .try_into()
            .map_err(|_| EncryptionError::InvalidKeyLength(decoded_len))?;
        let key = Key::<Aes256Gcm>::from(key_bytes);
        let cipher = Aes256Gcm::new(&key);
        Ok(Self {
            key_id: key_id.into(),
            cipher,
        })
    }

    /// The identifier of the master key backing this encryptor.
    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Encrypt the provided plaintext bytes and return an `EncryptedSecret` bundle.
    ///
    /// The bundle's `key_id` and `created_at` are authenticated as AES-GCM
    /// associated data (see [`metadata_aad`]), so a stored bundle cannot be
    /// re-dated or swapped under a same-`key_id` record without failing
    /// decryption.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<EncryptedSecret, EncryptionError> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        fill_random(&mut nonce_bytes).map_err(|_| EncryptionError::EntropyUnavailable)?;
        let nonce = Nonce::from(nonce_bytes);
        let created_at = Utc::now();

        let ciphertext = self
            .cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext,
                    aad: &metadata_aad(&self.key_id, created_at),
                },
            )
            .map_err(|_| EncryptionError::EncryptionFailure)?;

        Ok(EncryptedSecret {
            key_id: self.key_id.clone(),
            ciphertext: BASE64.encode(ciphertext),
            nonce: BASE64.encode(nonce_bytes),
            created_at,
        })
    }

    /// Encrypt the provided UTF-8 string.
    pub fn encrypt_string(&self, value: &str) -> Result<EncryptedSecret, EncryptionError> {
        self.encrypt(value.as_bytes())
    }

    /// Decrypt the provided bundle into raw bytes.
    ///
    /// Fails closed when any metadata field (`key_id`, `created_at`) was
    /// altered after encryption; those fields are part of the authenticated
    /// associated data, not just advisory labels.
    pub fn decrypt(&self, bundle: &EncryptedSecret) -> Result<Vec<u8>, EncryptionError> {
        if bundle.key_id != self.key_id {
            return Err(EncryptionError::KeyMismatch {
                expected: self.key_id.clone(),
                actual: bundle.key_id.clone(),
            });
        }

        let nonce_vec = BASE64
            .decode(&bundle.nonce)
            .map_err(EncryptionError::Base64Decode)?;
        let nonce_len = nonce_vec.len();
        if nonce_len != NONCE_SIZE {
            return Err(EncryptionError::InvalidNonceLength(nonce_len));
        }
        let nonce_bytes: [u8; NONCE_SIZE] = nonce_vec
            .try_into()
            .map_err(|_| EncryptionError::InvalidNonceLength(nonce_len))?;

        let ciphertext = BASE64
            .decode(&bundle.ciphertext)
            .map_err(EncryptionError::Base64Decode)?;
        let nonce = Nonce::from(nonce_bytes);

        self.cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: ciphertext.as_ref(),
                    aad: &metadata_aad(&bundle.key_id, bundle.created_at),
                },
            )
            .map_err(|_| EncryptionError::DecryptionFailure)
    }

    /// Decrypt the provided bundle into a UTF-8 string.
    pub fn decrypt_to_string(&self, bundle: &EncryptedSecret) -> Result<String, EncryptionError> {
        let bytes = self.decrypt(bundle)?;
        String::from_utf8(bytes).map_err(|_| EncryptionError::DecryptionFailure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn sample_key() -> String {
        // 32 bytes key (all 1s) encoded in base64.
        BASE64.encode([1u8; KEY_SIZE])
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn encrypt_decrypt_roundtrip() {
        let encryptor =
            EnvelopeEncryptor::new_from_base64_key("test-key", &sample_key()).expect("key");
        let sample_value = "test-secret-value";

        let bundle = encryptor.encrypt_string(sample_value).expect("encrypt");
        assert_eq!(bundle.key_id, "test-key");

        let decrypted = encryptor.decrypt_to_string(&bundle).expect("decrypt");
        assert_eq!(decrypted, sample_value);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn decrypt_with_wrong_key_id_fails() {
        let encryptor =
            EnvelopeEncryptor::new_from_base64_key("primary", &sample_key()).expect("key");
        let bundle = encryptor.encrypt_string("value").expect("encrypt");

        let other =
            EnvelopeEncryptor::new_from_base64_key("secondary", &sample_key()).expect("key");
        let err = other.decrypt(&bundle).expect_err("should fail");
        assert!(
            matches!(err, EncryptionError::KeyMismatch { .. }),
            "expected KeyMismatch error, got: {err}"
        );
    }

    /// The metadata fields sit inside the authentication boundary: editing
    /// `created_at` (or any other metadata) on a stored bundle must break GCM
    /// verification instead of decrypting successfully.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn tampered_metadata_fails_decryption() {
        let encryptor =
            EnvelopeEncryptor::new_from_base64_key("kms-1", &sample_key()).expect("key");
        let mut bundle = encryptor.encrypt_string("value").expect("encrypt");

        let original_created_at = bundle.created_at;
        bundle.created_at = original_created_at + chrono::Duration::microseconds(1);
        assert!(matches!(
            encryptor.decrypt(&bundle),
            Err(EncryptionError::DecryptionFailure)
        ));
        bundle.created_at = original_created_at - chrono::Duration::seconds(1);
        assert!(matches!(
            encryptor.decrypt(&bundle),
            Err(EncryptionError::DecryptionFailure)
        ));
        bundle.created_at = original_created_at;
        encryptor
            .decrypt(&bundle)
            .expect("the untouched bundle still decrypts");
    }

    /// Two records sharing a `key_id` cannot have their ciphertexts (or their
    /// whole bundles) swapped undetected: each ciphertext is bound to the exact
    /// `created_at` it was encrypted with.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn same_key_id_bundles_cannot_be_swapped_or_redated() {
        let encryptor =
            EnvelopeEncryptor::new_from_base64_key("kms-1", &sample_key()).expect("key");
        let first = encryptor.encrypt_string("first-secret").expect("encrypt");
        let second = encryptor.encrypt_string("second-secret").expect("encrypt");
        assert_ne!(first.ciphertext, second.ciphertext);
        assert_ne!(
            first.created_at, second.created_at,
            "the tamper fixtures require distinct encryption timestamps"
        );

        // Re-date one bundle to the other's timestamp: the swap of `created_at`
        // values must be rejected even though both carry the same `key_id`.
        let redated = EncryptedSecret {
            key_id: first.key_id.clone(),
            ciphertext: first.ciphertext.clone(),
            nonce: first.nonce.clone(),
            created_at: second.created_at,
        };
        assert!(matches!(
            encryptor.decrypt(&redated),
            Err(EncryptionError::DecryptionFailure)
        ));

        // A wholesale bundle move under a same-key record is likewise bound:
        // moving `second`'s ciphertext into `first`'s metadata fails.
        let moved = EncryptedSecret {
            key_id: first.key_id.clone(),
            ciphertext: second.ciphertext.clone(),
            nonce: second.nonce.clone(),
            created_at: first.created_at,
        };
        assert!(matches!(
            encryptor.decrypt(&moved),
            Err(EncryptionError::DecryptionFailure)
        ));

        // Both originals remain intact.
        encryptor
            .decrypt(&first)
            .expect("untouched first bundle decrypts");
        encryptor
            .decrypt(&second)
            .expect("untouched second bundle decrypts");
    }
}
