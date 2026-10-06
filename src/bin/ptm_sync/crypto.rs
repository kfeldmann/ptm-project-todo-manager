//! AES-256-GCM encrypt / decrypt compatible with encrypt.py.
//!
//! File format (matches encrypt.py):
//!   [ MAGIC (4 B) | nonce (12 B) | ciphertext + GCM tag (16 B appended by library) ]

use aes_gcm::{
    aead::{Aead, Generate, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use std::path::Path;

const MAGIC: &[u8; 4] = b"ENC1";
const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32;
const TAG_SIZE: usize = 16;
/// Smallest valid ciphertext: magic + nonce + (empty plaintext → tag only).
const MIN_ENC_SIZE: usize = 4 + NONCE_SIZE + TAG_SIZE;

// ── Key loading ───────────────────────────────────────────────────────────────

/// Load and validate the 32-byte raw key from `key_path`.
pub fn load_key(key_path: &Path) -> Result<Vec<u8>> {
    let key = std::fs::read(key_path)
        .with_context(|| format!("Cannot read key file: {}", key_path.display()))?;
    if key.len() != KEY_SIZE {
        anyhow::bail!(
            "Key file '{}' must be exactly {} bytes (got {}). \
             Generate one with: encrypt.py keygen",
            key_path.display(),
            KEY_SIZE,
            key.len()
        );
    }
    Ok(key)
}

// ── Encrypt ───────────────────────────────────────────────────────────────────

/// Encrypt `plaintext` with AES-256-GCM using the provided 32-byte `key`.
/// Returns `MAGIC || random_nonce || ciphertext_with_tag`.
pub fn encrypt(plaintext: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    debug_assert_eq!(key.len(), KEY_SIZE);
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("Invalid key length (expected {} bytes)", KEY_SIZE))?;
    let nonce = Nonce::generate();

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|_| anyhow::anyhow!("AES-GCM encryption failed"))?;

    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_SIZE + ciphertext.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(nonce.as_ref()); // 12 bytes
    out.extend_from_slice(&ciphertext);   // ciphertext + 16-byte GCM tag
    Ok(out)
}

// ── Decrypt ───────────────────────────────────────────────────────────────────

/// Decrypt data produced by `encrypt()` (or encrypt.py).
/// Verifies the magic header and GCM authentication tag before returning plaintext.
pub fn decrypt(data: &[u8], key: &[u8]) -> Result<Vec<u8>> {
    debug_assert_eq!(key.len(), KEY_SIZE);
    if data.len() < MIN_ENC_SIZE {
        anyhow::bail!(
            "Encrypted data is too short ({} bytes); minimum is {}",
            data.len(),
            MIN_ENC_SIZE
        );
    }
    if &data[..MAGIC.len()] != MAGIC {
        anyhow::bail!(
            "Bad magic header — this does not look like a ptm-sync encrypted file"
        );
    }

    // Build a fixed-size nonce from the byte slice (avoids the deprecated from_slice).
    let mut nonce_arr = [0u8; NONCE_SIZE];
    nonce_arr.copy_from_slice(&data[MAGIC.len()..MAGIC.len() + NONCE_SIZE]);
    let nonce = Nonce::from(nonce_arr);
    let ciphertext = &data[MAGIC.len() + NONCE_SIZE..];

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| anyhow::anyhow!("Invalid key length (expected {} bytes)", KEY_SIZE))?;
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| {
            anyhow::anyhow!(
                "Decryption failed: wrong key or the file has been corrupted / tampered with"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// A deterministic 32-byte key for tests.
    fn fixed_key() -> Vec<u8> {
        (0u8..32).collect()
    }

    // ── load_key ──────────────────────────────────────────────────────────────

    #[test]
    fn load_key_reads_valid_32_byte_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("key.bin");
        let expected: Vec<u8> = (0u8..32).collect();
        fs::write(&path, &expected).unwrap();
        assert_eq!(load_key(&path).unwrap(), expected);
    }

    #[test]
    fn load_key_rejects_file_shorter_than_32_bytes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("short.bin");
        fs::write(&path, &[0u8; 16]).unwrap();
        assert!(load_key(&path).is_err());
    }

    #[test]
    fn load_key_rejects_file_longer_than_32_bytes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("long.bin");
        fs::write(&path, &[0u8; 64]).unwrap();
        assert!(load_key(&path).is_err());
    }

    #[test]
    fn load_key_returns_error_for_missing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("no_such_file.bin");
        assert!(load_key(&path).is_err());
    }

    // ── encrypt / decrypt ─────────────────────────────────────────────────────

    #[test]
    fn round_trip_nonempty_plaintext() {
        let key = fixed_key();
        let plaintext = b"hello, ptm-sync!";
        let ct = encrypt(plaintext, &key).unwrap();
        let recovered = decrypt(&ct, &key).unwrap();
        assert_eq!(recovered.as_slice(), plaintext.as_slice());
    }

    #[test]
    fn round_trip_empty_plaintext() {
        let key = fixed_key();
        let ct = encrypt(b"", &key).unwrap();
        let recovered = decrypt(&ct, &key).unwrap();
        assert!(recovered.is_empty());
    }

    #[test]
    fn round_trip_large_payload() {
        let key = fixed_key();
        // 100 KB of cycling bytes — exercises streaming-like behaviour
        let plaintext: Vec<u8> = (0u8..=255).cycle().take(100_000).collect();
        let ct = encrypt(&plaintext, &key).unwrap();
        let recovered = decrypt(&ct, &key).unwrap();
        assert_eq!(recovered, plaintext);
    }

    #[test]
    fn ciphertext_starts_with_magic_header() {
        let ct = encrypt(b"test", &fixed_key()).unwrap();
        assert_eq!(&ct[..4], b"ENC1");
    }

    #[test]
    fn ciphertext_length_is_overhead_plus_plaintext() {
        let key = fixed_key();
        let plaintext = b"hello";
        let ct = encrypt(plaintext, &key).unwrap();
        // Expected layout: MAGIC(4) + nonce(12) + plaintext + GCM tag(16)
        assert_eq!(ct.len(), MAGIC.len() + NONCE_SIZE + plaintext.len() + TAG_SIZE);
    }

    #[test]
    fn each_encrypt_call_produces_unique_nonce() {
        let key = fixed_key();
        let ct1 = encrypt(b"same input", &key).unwrap();
        let ct2 = encrypt(b"same input", &key).unwrap();
        // Nonce occupies bytes [4..16]; must differ between independent calls
        assert_ne!(
            &ct1[MAGIC.len()..MAGIC.len() + NONCE_SIZE],
            &ct2[MAGIC.len()..MAGIC.len() + NONCE_SIZE],
            "Two encryptions of identical plaintext must use different nonces"
        );
    }

    #[test]
    fn wrong_key_fails_to_decrypt() {
        let key1 = fixed_key();
        let mut key2 = fixed_key();
        key2[0] ^= 0xFF;
        let ct = encrypt(b"secret data", &key1).unwrap();
        assert!(decrypt(&ct, &key2).is_err());
    }

    #[test]
    fn tampered_ciphertext_body_fails_authentication() {
        let key = fixed_key();
        let mut ct = encrypt(b"important payload", &key).unwrap();
        // Flip a bit in the ciphertext body (past magic + nonce)
        let idx = MAGIC.len() + NONCE_SIZE + 1;
        ct[idx] ^= 0x01;
        assert!(decrypt(&ct, &key).is_err());
    }

    #[test]
    fn tampered_auth_tag_fails_authentication() {
        let key = fixed_key();
        let mut ct = encrypt(b"important payload", &key).unwrap();
        // Corrupt the last byte (part of the 16-byte GCM tag)
        *ct.last_mut().unwrap() ^= 0xFF;
        assert!(decrypt(&ct, &key).is_err());
    }

    #[test]
    fn corrupted_magic_header_is_rejected() {
        let key = fixed_key();
        let mut ct = encrypt(b"test", &key).unwrap();
        ct[0] = b'X';
        assert!(decrypt(&ct, &key).is_err());
    }

    #[test]
    fn too_short_input_is_rejected() {
        let key = fixed_key();
        let short = vec![0u8; MIN_ENC_SIZE - 1];
        let err = decrypt(&short, &key).unwrap_err();
        assert!(
            err.to_string().contains("too short"),
            "unexpected error message: {}",
            err
        );
    }

    #[test]
    fn exactly_min_size_decrypts_to_empty_plaintext() {
        // encrypt(b"") must produce exactly MIN_ENC_SIZE bytes
        let key = fixed_key();
        let ct = encrypt(b"", &key).unwrap();
        assert_eq!(ct.len(), MIN_ENC_SIZE);
        assert!(decrypt(&ct, &key).unwrap().is_empty());
    }
}
