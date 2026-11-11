//! Mit einem Master-Passwort verschlüsselter Export ("Vault").
//!
//! * Schlüsselableitung: Argon2id (64 MiB, 3 Iterationen, 1 Lane), 16 Byte Salt
//! * Verschlüsselung: XChaCha20-Poly1305 (AEAD), 24 Byte zufällige Nonce
//! * Der Header (Format, Version, KDF-Parameter) ist als Associated Data authentifiziert.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const VAULT_FORMAT: &str = "remora-vault";
pub const VAULT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("Das Master-Passwort muss mindestens 8 Zeichen lang sein.")]
    WeakPassword,
    #[error("Die Datei ist kein gültiger Remora-Vault.")]
    InvalidFormat,
    #[error("Falsches Master-Passwort oder beschädigte Datei.")]
    Decryption,
    #[error("Kryptografiefehler: {0}")]
    Crypto(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KdfParams {
    pub algorithm: String,
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
    pub salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    pub format: String,
    pub version: u32,
    pub kdf: KdfParams,
    pub nonce: String,
    pub ciphertext: String,
}

impl VaultFile {
    fn associated_data(&self) -> Vec<u8> {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.format,
            self.version,
            self.kdf.algorithm,
            self.kdf.memory_kib,
            self.kdf.iterations,
            self.kdf.parallelism,
            self.kdf.salt
        )
        .into_bytes()
    }
}

fn derive_key(password: &str, kdf: &KdfParams) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    if kdf.algorithm != "argon2id" {
        return Err(VaultError::InvalidFormat);
    }
    // Schutz vor manipulierten Dateien mit absurden Parametern
    if kdf.memory_kib > 1024 * 1024 || kdf.iterations > 20 || kdf.parallelism > 16 {
        return Err(VaultError::InvalidFormat);
    }
    let salt = B64.decode(&kdf.salt).map_err(|_| VaultError::InvalidFormat)?;
    let params = Params::new(kdf.memory_kib, kdf.iterations, kdf.parallelism, Some(32))
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(password.as_bytes(), &salt, key.as_mut())
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    Ok(key)
}

pub fn encrypt(plaintext: &[u8], password: &str) -> Result<VaultFile, VaultError> {
    encrypt_with_params(plaintext, password, 64 * 1024, 3)
}

pub(crate) fn encrypt_with_params(
    plaintext: &[u8],
    password: &str,
    memory_kib: u32,
    iterations: u32,
) -> Result<VaultFile, VaultError> {
    if password.chars().count() < 8 {
        return Err(VaultError::WeakPassword);
    }
    let mut rng = rand::rngs::OsRng;
    let mut salt = [0u8; 16];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0u8; 24];
    rng.fill_bytes(&mut nonce);

    let mut file = VaultFile {
        format: VAULT_FORMAT.to_owned(),
        version: VAULT_VERSION,
        kdf: KdfParams {
            algorithm: "argon2id".to_owned(),
            memory_kib,
            iterations,
            parallelism: 1,
            salt: B64.encode(salt),
        },
        nonce: B64.encode(nonce),
        ciphertext: String::new(),
    };
    let key = derive_key(password, &file.kdf)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|e| VaultError::Crypto(e.to_string()))?;
    let aad = file.associated_data();
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|e| VaultError::Crypto(e.to_string()))?;
    file.ciphertext = B64.encode(ciphertext);
    Ok(file)
}

pub fn decrypt(file: &VaultFile, password: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    if file.format != VAULT_FORMAT || file.version != VAULT_VERSION {
        return Err(VaultError::InvalidFormat);
    }
    let nonce = B64.decode(&file.nonce).map_err(|_| VaultError::InvalidFormat)?;
    if nonce.len() != 24 {
        return Err(VaultError::InvalidFormat);
    }
    let ciphertext = B64.decode(&file.ciphertext).map_err(|_| VaultError::InvalidFormat)?;
    let key = derive_key(password, &file.kdf)?;
    let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|e| VaultError::Crypto(e.to_string()))?;
    let aad = file.associated_data();
    let plain = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| VaultError::Decryption)?;
    Ok(Zeroizing::new(plain))
}

/// Erkennt, ob ein JSON-Dokument ein Vault ist.
pub fn is_vault(json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("format").and_then(|f| f.as_str()).map(|f| f == VAULT_FORMAT))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrong_password() {
        let data = br#"{"secret":"value"}"#;
        let file = encrypt_with_params(data, "correct horse", 1024, 1).unwrap();
        assert_eq!(decrypt(&file, "correct horse").unwrap().as_slice(), data);
        assert!(matches!(decrypt(&file, "wrong password"), Err(VaultError::Decryption)));
    }

    #[test]
    fn tampered_header_fails() {
        let file = encrypt_with_params(b"x", "correct horse", 1024, 1).unwrap();
        let mut tampered = file.clone();
        tampered.kdf.iterations = 2;
        assert!(decrypt(&tampered, "correct horse").is_err());
    }

    #[test]
    fn weak_password_rejected() {
        assert!(matches!(encrypt(b"x", "short"), Err(VaultError::WeakPassword)));
    }

    #[test]
    fn detects_vault_json() {
        let file = encrypt_with_params(b"x", "correct horse", 1024, 1).unwrap();
        let json = serde_json::to_string(&file).unwrap();
        assert!(is_vault(&json));
        assert!(!is_vault(r#"{"format":"remora-export"}"#));
    }
}
