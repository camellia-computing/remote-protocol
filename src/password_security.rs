use crate::{config::Config, crypto::secretbox};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};

lazy_static::lazy_static! {
    pub static ref TEMPORARY_PASSWORD:Arc<RwLock<String>> = Arc::new(RwLock::new(get_auto_password()));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerificationMethod {
    OnlyUseTemporaryPassword,
    OnlyUsePermanentPassword,
    UseBothPasswords,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApproveMode {
    Both,
    Password,
    Click,
}

fn get_auto_password() -> String {
    let len = temporary_password_length();
    if Config::get_bool_option(crate::config::keys::OPTION_ALLOW_NUMERNIC_ONE_TIME_PASSWORD) {
        Config::get_auto_numeric_password(len)
    } else {
        Config::get_auto_password(len)
    }
}

// Should only be called in server
pub fn update_temporary_password() {
    *TEMPORARY_PASSWORD.write().unwrap() = get_auto_password();
}

// Should only be called in server
pub fn temporary_password() -> String {
    TEMPORARY_PASSWORD.read().unwrap().clone()
}

fn verification_method() -> VerificationMethod {
    let method = Config::get_option("verification-method");
    if method == "use-temporary-password" {
        VerificationMethod::OnlyUseTemporaryPassword
    } else if method == "use-permanent-password" {
        VerificationMethod::OnlyUsePermanentPassword
    } else {
        VerificationMethod::UseBothPasswords // default
    }
}

pub fn temporary_password_length() -> usize {
    let length = Config::get_option("temporary-password-length");
    if length == "8" {
        8
    } else if length == "10" {
        10
    } else {
        6 // default
    }
}

pub fn temporary_enabled() -> bool {
    verification_method() != VerificationMethod::OnlyUsePermanentPassword
}

pub fn permanent_enabled() -> bool {
    verification_method() != VerificationMethod::OnlyUseTemporaryPassword
}

pub fn has_valid_password() -> bool {
    temporary_enabled() && !temporary_password().is_empty()
        || permanent_enabled() && Config::has_permanent_password()
}

pub fn approve_mode() -> ApproveMode {
    let mode = Config::get_option("approve-mode");
    if mode == "password" {
        ApproveMode::Password
    } else if mode == "click" {
        ApproveMode::Click
    } else {
        ApproveMode::Both
    }
}

pub fn hide_cm() -> bool {
    approve_mode() == ApproveMode::Password
        && verification_method() == VerificationMethod::OnlyUsePermanentPassword
        && crate::config::option2bool("allow-hide-cm", &Config::get_option("allow-hide-cm"))
}

pub(crate) const SECRET_STORAGE_VERSION: &str = "00";
const ENVELOPE_FORMAT: u8 = 1;
const KEY_DERIVATION_DOMAIN: &[u8] = b"camellia-remote/local-secret-storage/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretStorageError {
    #[error("secret exceeds the configured maximum length")]
    ValueTooLong,
    #[error("secret is already encrypted")]
    AlreadyEncrypted,
    #[error("secret storage version is missing or unsupported")]
    UnsupportedVersion,
    #[error("secret storage payload is not valid base64")]
    InvalidEncoding,
    #[error("secret storage envelope is malformed")]
    InvalidEnvelope,
    #[error("secret encryption failed")]
    EncryptionFailed,
    #[error("secret authentication or decryption failed")]
    DecryptionFailed,
    #[error("decrypted secret is not valid UTF-8")]
    InvalidUtf8,
}

pub fn encrypt_str(value: &str, max_len: usize) -> Result<String, SecretStorageError> {
    if value.is_empty() {
        return Ok(String::new());
    }
    if value.chars().count() > max_len {
        return Err(SecretStorageError::ValueTooLong);
    }
    if decrypt_str(value).is_ok() {
        return Err(SecretStorageError::AlreadyEncrypted);
    }

    let encrypted = encrypt_local(value.as_bytes())?;
    Ok(SECRET_STORAGE_VERSION.to_owned() + &BASE64.encode(encrypted))
}

pub fn decrypt_str(storage: &str) -> Result<String, SecretStorageError> {
    if storage.is_empty() {
        return Ok(String::new());
    }
    let payload = storage
        .as_bytes()
        .strip_prefix(SECRET_STORAGE_VERSION.as_bytes())
        .ok_or(SecretStorageError::UnsupportedVersion)?;
    let encrypted = BASE64
        .decode(payload)
        .map_err(|_| SecretStorageError::InvalidEncoding)?;
    let plaintext = decrypt_local(&encrypted)?;
    String::from_utf8(plaintext).map_err(|_| SecretStorageError::InvalidUtf8)
}

pub fn encrypt_vec(value: &[u8], max_len: usize) -> Result<Vec<u8>, SecretStorageError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }
    if value.len() > max_len {
        return Err(SecretStorageError::ValueTooLong);
    }
    if decrypt_vec(value).is_ok() {
        return Err(SecretStorageError::AlreadyEncrypted);
    }

    let mut storage = SECRET_STORAGE_VERSION.as_bytes().to_vec();
    storage.extend(BASE64.encode(encrypt_local(value)?).into_bytes());
    Ok(storage)
}

pub fn decrypt_vec(storage: &[u8]) -> Result<Vec<u8>, SecretStorageError> {
    if storage.is_empty() {
        return Ok(Vec::new());
    }
    let payload = storage
        .strip_prefix(SECRET_STORAGE_VERSION.as_bytes())
        .ok_or(SecretStorageError::UnsupportedVersion)?;
    let encrypted = BASE64
        .decode(payload)
        .map_err(|_| SecretStorageError::InvalidEncoding)?;
    decrypt_local(&encrypted)
}

fn local_storage_key() -> secretbox::Key {
    let key_pair = Config::get_key_pair();
    let mut hasher = Sha256::new();
    hasher.update(KEY_DERIVATION_DOMAIN);
    hasher.update((key_pair.0.len() as u64).to_be_bytes());
    hasher.update(&key_pair.0);
    let digest = hasher.finalize();
    let mut key = [0u8; secretbox::KEYBYTES];
    key.copy_from_slice(&digest);
    secretbox::Key(key)
}

pub fn encrypt_local(data: &[u8]) -> Result<Vec<u8>, SecretStorageError> {
    let key = local_storage_key();
    let nonce = secretbox::gen_nonce();
    let encrypted =
        secretbox::seal(data, &nonce, &key).map_err(|_| SecretStorageError::EncryptionFailed)?;
    let mut output = Vec::with_capacity(1 + nonce.0.len() + encrypted.len());
    output.push(ENVELOPE_FORMAT);
    output.extend(nonce.0);
    output.extend(encrypted);
    Ok(output)
}

pub fn decrypt_local(data: &[u8]) -> Result<Vec<u8>, SecretStorageError> {
    if data.first() != Some(&ENVELOPE_FORMAT)
        || data.len() < 1 + secretbox::NONCEBYTES + secretbox::MACBYTES
    {
        return Err(SecretStorageError::InvalidEnvelope);
    }

    let mut nonce = [0u8; secretbox::NONCEBYTES];
    nonce.copy_from_slice(&data[1..1 + secretbox::NONCEBYTES]);
    secretbox::open(
        &data[1 + secretbox::NONCEBYTES..],
        &secretbox::Nonce(nonce),
        &local_storage_key(),
    )
    .map_err(|_| SecretStorageError::DecryptionFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_storage_roundtrip_is_authenticated() {
        let storage = encrypt_str("1ü1111", 128).unwrap();

        assert!(storage.starts_with("00"));
        assert_eq!(decrypt_str(&storage).unwrap(), "1ü1111");

        let mut corrupted = storage.into_bytes();
        *corrupted.last_mut().unwrap() ^= 1;
        assert!(decrypt_str(&String::from_utf8(corrupted).unwrap()).is_err());
    }

    #[test]
    fn binary_storage_roundtrip_is_authenticated() {
        let value = [0, 1, 2, 3, 255];
        let storage = encrypt_vec(&value, 128).unwrap();

        assert!(storage.starts_with(SECRET_STORAGE_VERSION.as_bytes()));
        assert_eq!(decrypt_vec(&storage).unwrap(), value);
    }

    #[test]
    fn empty_optional_secrets_remain_empty() {
        assert_eq!(encrypt_str("", 128).unwrap(), "");
        assert_eq!(decrypt_str("").unwrap(), "");
        assert_eq!(encrypt_vec(&[], 128).unwrap(), Vec::<u8>::new());
        assert_eq!(decrypt_vec(&[]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn plaintext_and_unsupported_versions_are_rejected() {
        for value in ["plaintext", "99cGF5bG9hZA==", "00not-base64"] {
            assert!(decrypt_str(value).is_err());
        }
        for value in [
            b"plaintext".as_slice(),
            b"99cGF5bG9hZA==".as_slice(),
            b"00not-base64".as_slice(),
        ] {
            assert!(decrypt_vec(value).is_err());
        }
    }

    #[test]
    fn size_limits_fail_instead_of_returning_plaintext() {
        assert_eq!(
            encrypt_str("too long", 3),
            Err(SecretStorageError::ValueTooLong)
        );
        assert_eq!(
            encrypt_vec(b"too long", 3),
            Err(SecretStorageError::ValueTooLong)
        );
    }

    #[test]
    fn already_encrypted_values_are_rejected() {
        let string_storage = encrypt_str("secret", 128).unwrap();
        let binary_storage = encrypt_vec(b"secret", 128).unwrap();

        assert_eq!(
            encrypt_str(&string_storage, 1024),
            Err(SecretStorageError::AlreadyEncrypted)
        );
        assert_eq!(
            encrypt_vec(&binary_storage, 1024),
            Err(SecretStorageError::AlreadyEncrypted)
        );
    }

    #[test]
    fn local_encryption_uses_a_fresh_nonce() {
        let first = encrypt_local(b"secret").unwrap();
        let second = encrypt_local(b"secret").unwrap();

        assert_eq!(first.first(), Some(&ENVELOPE_FORMAT));
        assert_eq!(second.first(), Some(&ENVELOPE_FORMAT));
        assert_ne!(first, second);
        assert_eq!(decrypt_local(&first).unwrap(), b"secret");
        assert_eq!(decrypt_local(&second).unwrap(), b"secret");
    }

    #[test]
    fn malformed_local_envelopes_are_rejected() {
        assert_eq!(
            decrypt_local(&[ENVELOPE_FORMAT]),
            Err(SecretStorageError::InvalidEnvelope)
        );
        assert_eq!(
            decrypt_local(&[0; secretbox::NONCEBYTES + secretbox::MACBYTES]),
            Err(SecretStorageError::InvalidEnvelope)
        );
    }
}
