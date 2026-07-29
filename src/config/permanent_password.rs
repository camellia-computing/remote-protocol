use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest, Sha256};

use crate::{
    crypto::constant_time_eq,
    password_security::{decrypt_local, encrypt_local, SecretStorageError},
};

pub(super) const PERMANENT_PASSWORD_ENC_VERSION: &str = "01";
pub(super) const PERMANENT_PASSWORD_HASH_PREFIX: &str = "00";
const HBBS_PRESET_PASSWORD_HASH_PREFIX: &str = "00";
pub(super) const PERMANENT_PASSWORD_H1_LEN: usize = 32;
pub(super) const DEFAULT_SALT_LEN: usize = 32;
pub const ENCRYPT_MAX_LEN: usize = 128; // used for password, pin, etc, not for all

#[cfg(test)]
pub(super) fn is_permanent_password_hashed_storage(v: &str) -> bool {
    decode_permanent_password_h1_from_hashed_storage(v).is_some()
}

pub fn compute_permanent_password_h1(
    password: &str,
    salt: &str,
) -> [u8; PERMANENT_PASSWORD_H1_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    let out = hasher.finalize();
    let mut h1 = [0u8; PERMANENT_PASSWORD_H1_LEN];
    h1.copy_from_slice(&out[..PERMANENT_PASSWORD_H1_LEN]);
    h1
}

pub(super) fn constant_time_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    constant_time_eq(a, b)
}

pub(super) fn encode_permanent_password_storage_from_h1(
    h1: &[u8; PERMANENT_PASSWORD_H1_LEN],
) -> String {
    PERMANENT_PASSWORD_HASH_PREFIX.to_owned() + &BASE64.encode(h1)
}

pub(super) fn encode_permanent_password_encrypted_storage_from_h1(
    h1: &[u8; PERMANENT_PASSWORD_H1_LEN],
) -> Option<String> {
    let hashed_storage = encode_permanent_password_storage_from_h1(h1);
    encrypt_permanent_password_storage(&hashed_storage)
}

pub(super) fn decode_permanent_password_h1_from_hashed_storage(
    storage: &str,
) -> Option<[u8; PERMANENT_PASSWORD_H1_LEN]> {
    decode_password_h1_after_prefix(storage, PERMANENT_PASSWORD_HASH_PREFIX)
}

fn decode_password_h1_after_prefix(
    storage: &str,
    prefix: &str,
) -> Option<[u8; PERMANENT_PASSWORD_H1_LEN]> {
    let encoded = storage.strip_prefix(prefix)?;

    let v = BASE64.decode(encoded.as_bytes()).ok()?;
    if v.len() != PERMANENT_PASSWORD_H1_LEN {
        return None;
    }
    let mut h1 = [0u8; PERMANENT_PASSWORD_H1_LEN];
    h1.copy_from_slice(&v[..PERMANENT_PASSWORD_H1_LEN]);
    Some(h1)
}

fn encrypt_permanent_password_storage(storage: &str) -> Option<String> {
    if storage.chars().count() > ENCRYPT_MAX_LEN {
        return None;
    }
    let encrypted = encrypt_local(storage.as_bytes()).ok()?;
    Some(PERMANENT_PASSWORD_ENC_VERSION.to_owned() + &BASE64.encode(encrypted))
}

pub(super) fn decrypt_permanent_password_storage(
    storage: &str,
) -> Result<String, SecretStorageError> {
    let encoded = storage
        .as_bytes()
        .strip_prefix(PERMANENT_PASSWORD_ENC_VERSION.as_bytes())
        .ok_or(SecretStorageError::UnsupportedVersion)?;
    let encrypted = BASE64
        .decode(encoded)
        .map_err(|_| SecretStorageError::InvalidEncoding)?;
    let plaintext = decrypt_local(&encrypted)?;
    String::from_utf8(plaintext).map_err(|_| SecretStorageError::InvalidUtf8)
}

pub fn local_permanent_password_storage_is_usable_for_auth(storage: &str, salt: &str) -> bool {
    !salt.is_empty() && decode_permanent_password_h1_from_storage(storage).is_some()
}

pub fn preset_permanent_password_storage_is_usable_for_auth(storage: &str, salt: &str) -> bool {
    !salt.is_empty() && decode_preset_password_h1_from_storage(storage).is_some()
}

pub fn decode_preset_password_h1_from_storage(
    storage: &str,
) -> Option<[u8; PERMANENT_PASSWORD_H1_LEN]> {
    decode_password_h1_after_prefix(storage, HBBS_PRESET_PASSWORD_HASH_PREFIX)
}

#[cfg(test)]
fn local_permanent_password_storage_matches_plain(storage: &str, salt: &str, input: &str) -> bool {
    if storage.is_empty() || input.is_empty() {
        return false;
    }
    if !local_permanent_password_storage_is_usable_for_auth(storage, salt) {
        return false;
    }
    if let Some(stored_h1) = decode_permanent_password_h1_from_storage(storage) {
        let h1 = compute_permanent_password_h1(input, salt);
        return constant_time_eq_32(&h1, &stored_h1);
    }
    false
}

pub(super) fn preset_permanent_password_storage_matches_plain(
    storage: &str,
    salt: &str,
    input: &str,
) -> bool {
    if storage.is_empty() || salt.is_empty() || input.is_empty() {
        return false;
    }
    let Some(stored_h1) = decode_preset_password_h1_from_storage(storage) else {
        return false;
    };
    let h1 = compute_permanent_password_h1(input, salt);
    constant_time_eq_32(&h1, &stored_h1)
}

pub fn decode_permanent_password_h1_from_storage(
    storage: &str,
) -> Option<[u8; PERMANENT_PASSWORD_H1_LEN]> {
    if storage.starts_with(PERMANENT_PASSWORD_ENC_VERSION) {
        let hashed_storage = decrypt_permanent_password_storage(storage).ok()?;
        return decode_permanent_password_h1_from_hashed_storage(&hashed_storage);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_hbbs_preset_password_storage_from_h1(h1: &[u8; PERMANENT_PASSWORD_H1_LEN]) -> String {
        HBBS_PRESET_PASSWORD_HASH_PREFIX.to_owned() + &BASE64.encode(h1)
    }

    #[test]
    fn test_permanent_password_h1_storage_roundtrip() {
        let salt = "salt123";
        let password = "p@ssw0rd";
        let h1 = compute_permanent_password_h1(password, salt);
        let stored = encode_permanent_password_storage_from_h1(&h1);
        assert!(stored.starts_with(PERMANENT_PASSWORD_HASH_PREFIX));
        assert!(is_permanent_password_hashed_storage(&stored));
        let decoded = decode_permanent_password_h1_from_hashed_storage(&stored).unwrap();
        assert_eq!(&decoded[..], &h1[..]);
    }

    #[test]
    fn test_permanent_password_encrypted_storage_uses_01_outer_and_00_inner() {
        let h1 = compute_permanent_password_h1("p@ssw0rd", "salt123");
        let storage = encode_permanent_password_encrypted_storage_from_h1(&h1).unwrap();

        assert!(storage.starts_with(PERMANENT_PASSWORD_ENC_VERSION));
        assert!(!is_permanent_password_hashed_storage(&storage));

        let inner = decrypt_permanent_password_storage(&storage).unwrap();
        assert!(inner.starts_with(PERMANENT_PASSWORD_HASH_PREFIX));
        assert_eq!(
            decode_permanent_password_h1_from_storage(&storage),
            Some(h1)
        );
    }

    #[test]
    fn test_encrypted_hashed_password_storage_matches_plain_with_salt() {
        let salt = "salt123";
        let h1 = compute_permanent_password_h1("p@ssw0rd", salt);
        let storage = encode_permanent_password_encrypted_storage_from_h1(&h1).unwrap();

        assert!(local_permanent_password_storage_is_usable_for_auth(
            &storage, salt
        ));
        assert!(local_permanent_password_storage_matches_plain(
            &storage, salt, "p@ssw0rd"
        ));
        assert!(!local_permanent_password_storage_matches_plain(
            &storage, salt, "wrong"
        ));
    }

    #[test]
    fn test_hbbs_00_hashed_preset_password_storage_is_decoded_for_preset_auth() {
        let h1 = compute_permanent_password_h1("p@ssw0rd", "salt123");
        let storage = encode_hbbs_preset_password_storage_from_h1(&h1);

        assert_eq!(decode_preset_password_h1_from_storage(&storage), Some(h1));
    }

    #[test]
    fn test_hbbs_00_hashed_preset_password_storage_matches_plain_with_salt() {
        let salt = "salt123";
        let h1 = compute_permanent_password_h1("p@ssw0rd", salt);
        let storage = encode_hbbs_preset_password_storage_from_h1(&h1);

        assert!(preset_permanent_password_storage_is_usable_for_auth(
            &storage, salt
        ));
        assert!(preset_permanent_password_storage_matches_plain(
            &storage, salt, "p@ssw0rd"
        ));
        assert!(!preset_permanent_password_storage_matches_plain(
            &storage, salt, "wrong"
        ));
    }

    #[test]
    fn test_encrypted_hash_storage_is_not_accepted_as_preset_storage() {
        let salt = "salt123";
        let h1 = compute_permanent_password_h1("p@ssw0rd", salt);
        let storage = encode_permanent_password_encrypted_storage_from_h1(&h1).unwrap();

        assert!(!preset_permanent_password_storage_is_usable_for_auth(
            &storage, salt
        ));
        assert!(!preset_permanent_password_storage_matches_plain(
            &storage, salt, "p@ssw0rd"
        ));
    }

    #[test]
    fn test_hashed_preset_password_without_salt_is_rejected() {
        let h1 = compute_permanent_password_h1("p@ssw0rd", "salt123");
        let storage = encode_hbbs_preset_password_storage_from_h1(&h1);

        assert!(!preset_permanent_password_storage_is_usable_for_auth(
            &storage, ""
        ));
        assert!(!preset_permanent_password_storage_matches_plain(
            &storage, "", &storage
        ));
        assert!(!preset_permanent_password_storage_matches_plain(
            &storage, "", "p@ssw0rd"
        ));
    }

    #[test]
    fn test_hashed_preset_password_storage_without_salt_is_not_usable() {
        let h1 = compute_permanent_password_h1("p@ssw0rd", "salt123");
        let storage = encode_permanent_password_storage_from_h1(&h1);

        assert!(!local_permanent_password_storage_is_usable_for_auth(
            &storage, ""
        ));
        assert!(!local_permanent_password_storage_matches_plain(
            &storage, "", "p@ssw0rd"
        ));
    }

    #[test]
    fn test_plaintext_preset_without_salt_is_rejected() {
        let storage = "01not-a-valid-hash";

        assert!(!preset_permanent_password_storage_is_usable_for_auth(
            storage, ""
        ));
        assert!(!preset_permanent_password_storage_matches_plain(
            storage,
            "",
            "01not-a-valid-hash"
        ));
    }

    #[test]
    fn test_malformed_preset_password_with_salt_is_not_usable_for_auth() {
        for storage in ["01not-a-valid-hash", "00not-a-valid-hash"] {
            assert!(!preset_permanent_password_storage_is_usable_for_auth(
                storage,
                "preset-salt"
            ));
            assert!(!preset_permanent_password_storage_matches_plain(
                storage,
                "preset-salt",
                storage
            ));
        }
    }

    #[test]
    fn test_invalid_current_version_storage_is_not_usable_for_auth() {
        let encrypted = encrypt_local(b"not-a-hash").unwrap();
        let encrypted_non_hash =
            PERMANENT_PASSWORD_ENC_VERSION.to_owned() + &BASE64.encode(encrypted);

        assert!(!local_permanent_password_storage_is_usable_for_auth(
            &encrypted_non_hash,
            "salt123"
        ));
        assert!(!local_permanent_password_storage_matches_plain(
            &encrypted_non_hash,
            "salt123",
            &encrypted_non_hash
        ));
    }

    #[test]
    fn test_unencrypted_hash_shaped_local_storage_is_rejected() {
        let h1 = compute_permanent_password_h1("plain-looking-hash", "salt123");
        let storage = encode_permanent_password_storage_from_h1(&h1);

        assert!(!local_permanent_password_storage_is_usable_for_auth(
            &storage, ""
        ));
        assert!(!local_permanent_password_storage_matches_plain(
            &storage, "", &storage
        ));
    }
}
