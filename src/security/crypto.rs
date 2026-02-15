use aes_gcm::aead::{Aead, KeyInit};
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
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<EncryptedSecret, EncryptionError> {
        let mut nonce_bytes = [0u8; NONCE_SIZE];
        fill_random(&mut nonce_bytes).map_err(|_| EncryptionError::EntropyUnavailable)?;
        let nonce = Nonce::from(nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|_| EncryptionError::EncryptionFailure)?;

        Ok(EncryptedSecret {
            key_id: self.key_id.clone(),
            ciphertext: BASE64.encode(ciphertext),
            nonce: BASE64.encode(nonce_bytes),
            created_at: Utc::now(),
        })
    }

    /// Encrypt the provided UTF-8 string.
    pub fn encrypt_string(&self, value: &str) -> Result<EncryptedSecret, EncryptionError> {
        self.encrypt(value.as_bytes())
    }

    /// Decrypt the provided bundle into raw bytes.
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
            .decrypt(&nonce, ciphertext.as_ref())
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
    fn decrypt_with_wrong_key_id_fails() {
        let encryptor =
            EnvelopeEncryptor::new_from_base64_key("primary", &sample_key()).expect("key");
        let bundle = encryptor.encrypt_string("value").expect("encrypt");

        let other =
            EnvelopeEncryptor::new_from_base64_key("secondary", &sample_key()).expect("key");
        let err = other.decrypt(&bundle).expect_err("should fail");
        matches!(err, EncryptionError::KeyMismatch { .. });
    }
}
