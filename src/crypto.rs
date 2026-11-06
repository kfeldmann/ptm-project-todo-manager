//! AES-256-GCM encrypt / decrypt compatible with `encrypt.py`.
//!
//! This module is shared by the `ptm-sync` and `enc` binaries (path-included,
//! like `src/spell.rs`), so the two always agree on the on-disk format.
//!
//! File format (matches `encrypt.py`):
//!   [ MAGIC (4 B) | nonce (12 B) | ciphertext + GCM tag (16 B appended by library) ]

use aes_gcm::{
    aead::{Aead, Generate, KeyInit},
    Aes256Gcm, Nonce,
};
use anyhow::{Context, Result};
use std::path::Path;

pub const MAGIC: &[u8; 4] = b"ENC1";
pub const NONCE_SIZE: usize = 12;
pub const KEY_SIZE: usize = 32;
pub const TAG_SIZE: usize = 16;
/// Smallest valid ciphertext: magic + nonce + (empty plaintext → tag only).
pub const MIN_ENC_SIZE: usize = MAGIC.len() + NONCE_SIZE + TAG_SIZE;

// ── Key loading ───────────────────────────────────────────────────────────────

/// Load and validate the 32-byte raw key from `key_path`.
pub fn load_key(key_path: &Path) -> Result<Vec<u8>> {
    let key = std::fs::read(key_path)
        .with_context(|| format!("Cannot read key file: {}", key_path.display()))?;
    if key.len() != KEY_SIZE {
        anyhow::bail!(
            "Key file '{}' must be exactly {} bytes (got {}). \
             Generate one with: enc keygen",
            key_path.display(),
            KEY_SIZE,
            key.len()
        );
    }
    Ok(key)
}

/// Generate a fresh 32-byte key from the OS CSPRNG.
pub fn generate_key() -> Result<[u8; KEY_SIZE]> {
    let mut key = [0u8; KEY_SIZE];
    getrandom::fill(&mut key)
        .map_err(|e| anyhow::anyhow!("Failed to read random bytes from the OS: {}", e))?;
    Ok(key)
}

/// Write `key` to `key_path` with owner-only permissions (0600), creating
/// parent directories as needed.
pub fn write_key(key_path: &Path, key: &[u8]) -> Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    if key.len() != KEY_SIZE {
        anyhow::bail!(
            "Refusing to write a {}-byte key; keys must be exactly {} bytes",
            key.len(),
            KEY_SIZE
        );
    }
    if let Some(parent) = key_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Cannot create key directory: {}", parent.display())
            })?;
        }
    }
    // mode(0o600) applies at creation time, so the key is never briefly
    // readable by other users (unlike write-then-chmod).
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(key_path)
        .with_context(|| format!("Cannot write key file: {}", key_path.display()))?;

    // `mode()` is only consulted when the file is *created*, so an existing
    // key keeps whatever mode it already had — `enc keygen --force` over a
    // 0644 file used to leave it world-readable (finding #20).  Re-assert the
    // mode on the open descriptor rather than by path, so there is no window in
    // which the path could be swapped for a different file.
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
        .with_context(|| {
            format!("Cannot set permissions on key file: {}", key_path.display())
        })?;

    f.write_all(key)
        .with_context(|| format!("Cannot write key file: {}", key_path.display()))?;
    Ok(())
}

/// Verify that the key file exists and has no group/other read or write bits.
pub fn check_key_permissions(key_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(key_path)
        .with_context(|| format!("Cannot access encryption key: {}", key_path.display()))?;
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        anyhow::bail!(
            "Encryption key '{}' has insecure permissions ({:04o}). \
             It must be accessible only by the owner (e.g. chmod 600).",
            key_path.display(),
            mode & 0o777
        );
    }
    Ok(())
}

// ── Key text encoding (export / import) ───────────────────────────────────────

/// Encode a raw key as standard (padded) base64 — the format printed by
/// `enc export` and accepted by `enc import`.
pub fn encode_key_base64(key: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(key)
}

/// Decode a base64 key string (whitespace tolerated, standard alphabet).
/// The result is validated to be exactly 32 bytes.
pub fn decode_key_base64(s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    // Accept keys that were copy/pasted across wrapped lines or with a
    // trailing newline from `enc export | ...`.
    let cleaned: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let key = base64::engine::general_purpose::STANDARD
        .decode(&cleaned)
        .context("Key is not valid base64")?;
    if key.len() != KEY_SIZE {
        anyhow::bail!(
            "Decoded key is {} bytes; a valid key must be exactly {} bytes",
            key.len(),
            KEY_SIZE
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
            "Not a recognised encrypted file (expected header {:?})",
            String::from_utf8_lossy(MAGIC)
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
        fs::write(&path, [0u8; 16]).unwrap();
        assert!(load_key(&path).is_err());
    }

    #[test]
    fn load_key_rejects_file_longer_than_32_bytes() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("long.bin");
        fs::write(&path, [0u8; 64]).unwrap();
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

    // ── generate_key / write_key ──────────────────────────────────────────────

    #[test]
    fn generate_key_returns_32_bytes() {
        assert_eq!(generate_key().unwrap().len(), KEY_SIZE);
    }

    #[test]
    fn generate_key_is_random() {
        assert_ne!(generate_key().unwrap(), generate_key().unwrap());
    }

    #[test]
    fn write_key_creates_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dir").join("key.bin");
        let key = generate_key().unwrap();
        write_key(&path, &key).unwrap();

        assert_eq!(fs::read(&path).unwrap(), key);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be created owner-only");
        assert!(check_key_permissions(&path).is_ok());
    }

    #[test]
    fn write_key_overwrites_existing_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("key.bin");
        write_key(&path, &[1u8; KEY_SIZE]).unwrap();
        write_key(&path, &[2u8; KEY_SIZE]).unwrap();
        assert_eq!(fs::read(&path).unwrap(), vec![2u8; KEY_SIZE]);
    }

    /// Finding #20: `OpenOptions::mode()` is only honoured when the file is
    /// created, so overwriting a 0644 key with `enc keygen --force` left it
    /// world-readable.  The mode must be re-asserted on every write.
    #[test]
    fn write_key_restores_owner_only_mode_when_overwriting() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("key.bin");

        // Pre-create the file world-readable, as a user's `cp` or `cat >`
        // would.
        fs::write(&path, [0u8; KEY_SIZE]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);

        write_key(&path, &[7u8; KEY_SIZE]).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "overwriting must restore owner-only permissions");
        assert_eq!(fs::read(&path).unwrap(), vec![7u8; KEY_SIZE]);
        assert!(check_key_permissions(&path).is_ok());
    }

    #[test]
    fn write_key_rejects_wrong_length() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("key.bin");
        assert!(write_key(&path, &[0u8; 16]).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn written_key_round_trips_through_load_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("key.bin");
        let key = generate_key().unwrap();
        write_key(&path, &key).unwrap();
        assert_eq!(load_key(&path).unwrap(), key.to_vec());
    }

    // ── base64 export / import ────────────────────────────────────────────────

    #[test]
    fn base64_key_round_trip() {
        let key = generate_key().unwrap();
        let encoded = encode_key_base64(&key);
        assert_eq!(decode_key_base64(&encoded).unwrap(), key.to_vec());
    }

    #[test]
    fn base64_encoding_is_padded_standard_alphabet() {
        // 32 bytes → ceil(32/3)*4 = 44 chars, including padding.
        let key = [0xFFu8; KEY_SIZE];
        let encoded = encode_key_base64(&key);
        assert_eq!(encoded.len(), 44);
        assert!(encoded.ends_with('='));
        assert!(encoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    }

    #[test]
    fn decode_key_tolerates_whitespace() {
        let key = generate_key().unwrap();
        let encoded = encode_key_base64(&key);
        let wrapped = format!("  {}\n\t{}\n", &encoded[..20], &encoded[20..]);
        assert_eq!(decode_key_base64(&wrapped).unwrap(), key.to_vec());
    }

    #[test]
    fn decode_key_rejects_invalid_base64() {
        assert!(decode_key_base64("not base64!!!").is_err());
    }

    #[test]
    fn decode_key_rejects_wrong_length() {
        assert!(decode_key_base64(&encode_key_base64(&[0u8; 16])).is_err());
        assert!(decode_key_base64(&encode_key_base64(&[0u8; 64])).is_err());
        assert!(decode_key_base64("").is_err());
    }

    // ── check_key_permissions ─────────────────────────────────────────────────

    #[test]
    fn check_key_permissions_rejects_group_readable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("key_640.bin");
        fs::write(&path, [0u8; KEY_SIZE]).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(check_key_permissions(&path).is_err());
    }
}
