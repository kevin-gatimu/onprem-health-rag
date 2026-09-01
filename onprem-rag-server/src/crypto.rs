//! Symmetric encryption for secrets stored in DocumentDB (source-DB passwords).
//!
//! AES-256-GCM with a key derived from the server key (`ONPREM_CREDENTIALS_KEY`,
//! falling back to the JWT secret in dev). Ciphertext is stored as
//! `base64(nonce ‖ ciphertext+tag)` — self-describing, so no separate nonce column.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::config::Config;
use crate::error::{AppError, AppResult};

/// 96-bit nonce, the standard size for AES-GCM.
const NONCE_LEN: usize = 12;

/// Derives the 256-bit AES key from the configured secret and encrypts/decrypts
/// credential strings. Cheap to construct per request.
pub struct CredentialCipher {
    cipher: Aes256Gcm,
}

impl CredentialCipher {
    /// Build the cipher from server config. Uses `credentials_key` when set;
    /// otherwise derives from the JWT secret (dev convenience) with a warning.
    pub fn from_config(config: &Config) -> Self {
        let secret = match &config.credentials_key {
            Some(k) => k.as_str(),
            None => {
                tracing::warn!(
                    "ONPREM_CREDENTIALS_KEY not set; deriving credential-encryption key from \
                     the JWT secret. Set a dedicated key in production."
                );
                config.jwt_secret.as_str()
            }
        };
        // Domain-separate from any other use of the same secret before hashing to 32 bytes.
        let mut hasher = Sha256::new();
        hasher.update(b"onprem-rag/credentials/v1:");
        hasher.update(secret.as_bytes());
        let key_bytes = hasher.finalize();
        let key = Key::<Aes256Gcm>::from_slice(&key_bytes);
        CredentialCipher {
            cipher: Aes256Gcm::new(key),
        }
    }

    /// Encrypt a plaintext secret, returning `base64(nonce ‖ ciphertext)`.
    pub fn encrypt(&self, plaintext: &str) -> AppResult<String> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|_| AppError::Internal("failed to encrypt credentials".into()))?;

        let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        combined.extend_from_slice(&nonce_bytes);
        combined.extend_from_slice(&ciphertext);
        Ok(B64.encode(combined))
    }

    /// Decrypt a value produced by [`encrypt`](Self::encrypt).
    pub fn decrypt(&self, encoded: &str) -> AppResult<String> {
        let combined = B64
            .decode(encoded)
            .map_err(|_| AppError::Internal("stored credential is not valid base64".into()))?;
        if combined.len() <= NONCE_LEN {
            return Err(AppError::Internal("stored credential is malformed".into()));
        }
        let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
        let nonce = Nonce::from_slice(nonce_bytes);

        let plaintext = self
            .cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| AppError::Internal("failed to decrypt credentials (wrong key?)".into()))?;
        String::from_utf8(plaintext)
            .map_err(|_| AppError::Internal("decrypted credential is not valid UTF-8".into()))
    }
}
