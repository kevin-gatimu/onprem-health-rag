//! Argon2id password hashing and verification.

use crate::error::{AppError, AppResult};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng};
use argon2::Argon2;
use std::sync::LazyLock;

/// A precomputed argon2id hash of a throwaway value. Verifying a supplied password
/// against it costs the same as verifying a real user's hash, so the "no such user"
/// path takes the same time as the "wrong password" path (defeats user enumeration
/// by timing). The value is irrelevant — it only needs to be a valid PHC hash.
static DUMMY_HASH: LazyLock<String> =
    LazyLock::new(|| hash_password("timing-equalizer-not-a-real-password").expect("dummy hash"));

/// Hash a plaintext password with a fresh random salt (argon2id, default params).
pub fn hash_password(plaintext: &str) -> AppResult<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(plaintext.as_bytes(), &salt)
        .map_err(|e| AppError::Internal(format!("password hashing failed: {e}")))?;
    Ok(hash.to_string())
}

/// Verify a plaintext password against a stored PHC-format hash. Returns `false`
/// on any mismatch or malformed hash (never leaks the reason).
pub fn verify_password(plaintext: &str, stored_hash: &str) -> bool {
    match PasswordHash::new(stored_hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(plaintext.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Run a verify against a fixed dummy hash and discard the result. Call on the
/// user-not-found branch of login so its timing matches a real password check.
pub fn dummy_verify(plaintext: &str) {
    let _ = verify_password(plaintext, &DUMMY_HASH);
}
