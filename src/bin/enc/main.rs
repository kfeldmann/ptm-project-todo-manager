//! enc — symmetric file encryption (AES-256-GCM), plus key export/import.
//!
//! A Rust replacement for `util/encrypt.py`, producing byte-compatible files
//! and key files, so either tool can read what the other wrote.
//!
//! Commands:
//!   keygen   Generate a new 256-bit key (saved to ~/.encryption_key by default)
//!   encrypt  Encrypt a file  → <file>.enc
//!   decrypt  Decrypt a file  → <file>  (strips the .enc suffix)
//!   export   Print the key as base64 (for safe-keeping / transfer)
//!   import   Read a base64 key and write it to a key file
//!
//! Usage:
//!   enc keygen [--key-file PATH] [--force]
//!   enc encrypt <file> [--key-file PATH] [-o OUTPUT] [--force]
//!   enc decrypt <file.enc> [--key-file PATH] [-o OUTPUT] [--force]
//!   enc export [--key-file PATH]
//!   enc import [--key-file PATH] [--force]

/// Shared AES-256-GCM implementation (path-included from the main ptm source
/// so that `enc` and `ptm-sync` always agree on the file format).
#[path = "../../crypto.rs"]
#[allow(dead_code)] // shared module: each binary uses a different subset
mod crypto;

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "enc",
    about = "Symmetric file encryption — AES-256-GCM",
    after_help = "\
examples:
  enc keygen                             # generate key → ~/.encryption_key
  enc keygen --key-file ./project.key    # custom key location

  enc encrypt report.pdf                 # → report.pdf.enc
  enc encrypt report.pdf -o out.enc      # custom output name
  enc encrypt archive.tar.gz             # → archive.tar.gz.enc

  enc decrypt report.pdf.enc             # → report.pdf
  enc decrypt report.pdf.enc -o /tmp/r   # custom output path

  enc export > key.b64                   # print key as base64
  enc import --key-file ~/.encryption_key < key.b64"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Generate a new AES-256 key
    Keygen(CommonArgs),

    /// Encrypt a file (→ <file>.enc)
    Encrypt {
        /// Plaintext file to encrypt
        file: PathBuf,

        /// Output path (default: <file>.enc)
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,

        #[command(flatten)]
        common: CommonArgs,
    },

    /// Decrypt a file (→ strips the .enc suffix)
    Decrypt {
        /// Encrypted file to decrypt
        file: PathBuf,

        /// Output path (default: <file> with .enc stripped)
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,

        #[command(flatten)]
        common: CommonArgs,
    },

    /// Print the encryption key as base64
    Export(CommonArgs),

    /// Read a base64 key from stdin and write it to a key file
    Import(CommonArgs),
}

#[derive(clap::Args, Debug)]
struct CommonArgs {
    /// Path to key file
    #[arg(short = 'k', long, value_name = "PATH", default_value = "~/.encryption_key")]
    key_file: String,

    /// Overwrite output file without prompting
    #[arg(short = 'f', long)]
    force: bool,
}

impl CommonArgs {
    /// Resolve a leading `~` to `$HOME`.
    fn key_path(&self) -> Result<PathBuf> {
        expand_tilde(&self.key_file)
    }
}

/// Resolve a leading `~` / `~/` to `$HOME`.
///
/// If `$HOME` is unset and `path` needs it, this is an error rather than a
/// silent fallback to the current directory: writing a private key into
/// whatever directory happens to be current is not a safe default.
fn expand_tilde(path: &str) -> Result<PathBuf> {
    if path == "~" || path.starts_with("~/") {
        let home = std::env::var("HOME").map_err(|_| {
            anyhow::anyhow!(
                "HOME is not set, so '{}' cannot be resolved; pass --key-file with an explicit path",
                path
            )
        })?;
        let rest = path.strip_prefix("~/").unwrap_or("");
        return Ok(PathBuf::from(home).join(rest));
    }
    Ok(PathBuf::from(path))
}

// ── Commands ──────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Keygen(common) => keygen(&common),
        Command::Encrypt { file, output, common } => encrypt_cmd(&file, output, &common),
        Command::Decrypt { file, output, common } => decrypt_cmd(&file, output, &common),
        Command::Export(common) => export(&common),
        Command::Import(common) => import(&common),
    }
}

/// `keygen` — write a fresh random key to the key file.
fn keygen(common: &CommonArgs) -> Result<()> {
    let key_path = common.key_path()?;
    if key_path.exists() && !common.force && !confirm_overwrite(&key_path)? {
        println!("Aborted.");
        return Ok(());
    }

    let key = crypto::generate_key()?;
    crypto::write_key(&key_path, &key)?;

    println!("New key written to: {}", key_path.display());
    println!("⚠  Back this file up. Losing the key means losing access to all encrypted data.");
    Ok(())
}

/// `encrypt` — read `src`, write MAGIC || nonce || ciphertext+tag to `dst`.
fn encrypt_cmd(src: &Path, output: Option<PathBuf>, common: &CommonArgs) -> Result<()> {
    require_file(src)?;
    let dst = output.unwrap_or_else(|| default_enc_output(src));
    check_same_path(src, &dst)?;
    confirm_or_abort(&dst, common.force)?;

    let key = load_key(common)?;
    let plaintext = std::fs::read(src)
        .with_context(|| format!("Cannot read '{}'", src.display()))?;
    let encrypted = crypto::encrypt(&plaintext, &key)
        .with_context(|| format!("Failed to encrypt '{}'", src.display()))?;
    write_output(&dst, &encrypted)?;

    println!("Encrypted: {}  →  {}", src.display(), dst.display());
    println!(
        "  {} bytes  →  {} bytes",
        plaintext.len(),
        encrypted.len()
    );
    Ok(())
}

/// `decrypt` — verify + decrypt `src`, write the plaintext to `dst`.
fn decrypt_cmd(src: &Path, output: Option<PathBuf>, common: &CommonArgs) -> Result<()> {
    require_file(src)?;
    let dst = output.unwrap_or_else(|| default_dec_output(src));
    check_same_path(src, &dst)?;
    confirm_or_abort(&dst, common.force)?;

    let key = load_key(common)?;
    let encrypted = std::fs::read(src)
        .with_context(|| format!("Cannot read '{}'", src.display()))?;
    let plaintext = crypto::decrypt(&encrypted, &key).with_context(|| {
        format!("Failed to decrypt '{}'", src.display())
    })?;
    write_output(&dst, &plaintext)?;

    println!("Decrypted: {}  →  {}", src.display(), dst.display());
    println!(
        "  {} bytes  →  {} bytes",
        encrypted.len(),
        plaintext.len()
    );
    Ok(())
}

/// `export` — print the raw key as base64 on stdout.
fn export(common: &CommonArgs) -> Result<()> {
    let key_path = common.key_path()?;
    let key = load_key(common)?;
    let encoded = crypto::encode_key_base64(&key);

    if std::io::stdout().is_terminal() {
        // Interactive: keep the key out of scrollback where possible.
        eprintln!(
            "Key from {} (base64, 32 bytes):",
            key_path.display()
        );
        println!("{encoded}");
        eprintln!("⚠  Treat this string as the key itself — anyone holding it can decrypt your files.");
    } else {
        // Piped: emit only the key so `enc export > key.b64` is clean.
        println!("{encoded}");
    }
    Ok(())
}

/// `import` — read a base64 key from stdin and write it to the key file.
fn import(common: &CommonArgs) -> Result<()> {
    let key_path = common.key_path()?;
    if key_path.exists() && !common.force && !confirm_overwrite(&key_path)? {
        println!("Aborted.");
        return Ok(());
    }

    let encoded = read_key_stdin()?;
    if encoded.trim().is_empty() {
        anyhow::bail!("No base64 key supplied on stdin");
    }
    let key = crypto::decode_key_base64(&encoded)?;
    crypto::write_key(&key_path, &key)?;

    println!("Key written to: {}", key_path.display());
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Load the key file, warning (non-fatally) if its permissions are loose.
fn load_key(common: &CommonArgs) -> Result<Vec<u8>> {
    let path = common.key_path()?;
    let key = crypto::load_key(&path)?;
    if let Err(e) = crypto::check_key_permissions(&path) {
        eprintln!("Warning: {e}");
    }
    Ok(key)
}

/// Read a base64 key from stdin.
///
/// Interactively, only the first line is read so the prompt returns as soon as
/// the key is pasted. When piped, the whole stream is read so keys that were
/// wrapped across several lines (e.g. by `fold`) still import correctly.
fn read_key_stdin() -> Result<String> {
    let interactive = std::io::stdin().is_terminal();
    if interactive {
        eprint!("Paste base64 key: ");
        let _ = std::io::stderr().flush();
    }

    let stdin = std::io::stdin();
    let mut buf = String::new();
    for line in stdin.lock().lines() {
        buf.push_str(&line.context("Cannot read key from stdin")?);
        if interactive {
            break;
        }
    }
    Ok(buf)
}

/// Fail if `src` and `dst` refer to the same file.
fn check_same_path(src: &Path, dst: &Path) -> Result<()> {
    if same_path(src, dst) {
        anyhow::bail!("Source and destination are the same file: {}", src.display());
    }
    Ok(())
}

fn same_path(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (a.canonicalize(), b.canonicalize()) {
        return a == b;
    }
    // `b` does not exist yet: compare its resolved parent plus the file name.
    let parent = b.parent().and_then(|p| p.canonicalize().ok());
    match (a.canonicalize(), parent) {
        (Ok(a), Some(parent)) => parent.join(b.file_name().unwrap_or_default()) == a,
        _ => false,
    }
}

fn require_file(path: &Path) -> Result<()> {
    if !path.exists() {
        anyhow::bail!("'{}' not found", path.display());
    }
    if !path.is_file() {
        anyhow::bail!("'{}' is not a regular file", path.display());
    }
    Ok(())
}

fn write_output(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes)
        .with_context(|| format!("Cannot write '{}'", path.display()))
}

/// `foo.pdf` → `foo.pdf.enc`
fn default_enc_output(src: &Path) -> PathBuf {
    let mut name = src.file_name().unwrap_or_default().to_os_string();
    name.push(".enc");
    src.with_file_name(name)
}

/// `foo.pdf.enc` → `foo.pdf`  |  `foo.enc` → `foo`  |  anything else → `foo.dec`
fn default_dec_output(src: &Path) -> PathBuf {
    if src.extension().map(|e| e == "enc").unwrap_or(false) {
        if let Some(stripped) = src.file_stem() {
            // Guard against a bare ".enc" filename.
            if !stripped.is_empty() {
                return src.with_file_name(stripped);
            }
        }
    }
    let mut name = src.file_name().unwrap_or_default().to_os_string();
    name.push(".dec");
    src.with_file_name(name)
}

/// Prompt before overwriting an existing key file; false if the user declines.
fn confirm_overwrite(path: &Path) -> Result<bool> {
    Ok(confirm(
        &format!("Key file '{}' already exists. Overwrite?", path.display()),
    ))
}

fn confirm_or_abort(dst: &Path, force: bool) -> Result<()> {
    if dst.exists()
        && !force
        && !confirm(&format!(
            "Output file '{}' already exists. Overwrite?",
            dst.display()
        ))
    {
        anyhow::bail!("Aborted (output file already exists; use --force to overwrite)");
    }
    Ok(())
}

/// Ask a yes/no question on stderr. Without a terminal on stdin there is
/// nobody to answer, so treat it as "no" (safe default: never clobber).
fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!("{question} (no terminal; use --force to overwrite)");
        return false;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    answer.trim().eq_ignore_ascii_case("y")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn default_enc_output_appends_suffix() {
        assert_eq!(
            default_enc_output(Path::new("report.pdf")),
            PathBuf::from("report.pdf.enc")
        );
        assert_eq!(
            default_enc_output(Path::new("archive.tar.gz")),
            PathBuf::from("archive.tar.gz.enc")
        );
        assert_eq!(
            default_enc_output(Path::new("noext")),
            PathBuf::from("noext.enc")
        );
    }

    #[test]
    fn default_dec_output_strips_enc_suffix() {
        assert_eq!(
            default_dec_output(Path::new("report.pdf.enc")),
            PathBuf::from("report.pdf")
        );
        assert_eq!(
            default_dec_output(Path::new("archive.tar.gz.enc")),
            PathBuf::from("archive.tar.gz")
        );
        assert_eq!(default_dec_output(Path::new("foo.enc")), PathBuf::from("foo"));
    }

    #[test]
    fn default_dec_output_falls_back_to_dec_suffix() {
        assert_eq!(
            default_dec_output(Path::new("mystery.bin")),
            PathBuf::from("mystery.bin.dec")
        );
        // A file literally named ".enc" has no usable stem.
        assert_eq!(default_dec_output(Path::new(".enc")), PathBuf::from(".enc.dec"));
    }

    #[test]
    fn expand_tilde_expands_home() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(expand_tilde("~/key").unwrap(), PathBuf::from(&home).join("key"));
        assert_eq!(expand_tilde("~").unwrap(), PathBuf::from(&home));
        assert_eq!(expand_tilde("/abs/key").unwrap(), PathBuf::from("/abs/key"));
        assert_eq!(
            expand_tilde("relative/key").unwrap(),
            PathBuf::from("relative/key")
        );
        // A bare "~" inside a name must not be expanded.
        assert_eq!(expand_tilde("a/~/b").unwrap(), PathBuf::from("a/~/b"));
    }

    #[test]
    fn expand_tilde_errors_when_home_is_unset() {
        // Only meaningful when the variable is actually absent; otherwise
        // exercise the error branch by temporarily unsetting it.
        let saved = std::env::var("HOME").ok();
        // SAFETY: single-threaded test process for this variable's use below.
        unsafe { std::env::remove_var("HOME") };
        let result = expand_tilde("~/key");
        let plain = expand_tilde("/abs/key");
        if let Some(home) = saved {
            unsafe { std::env::set_var("HOME", home) };
        }
        assert!(result.is_err(), "expected an error when HOME is unset");
        assert!(plain.is_ok(), "absolute paths must not need HOME");
    }

    #[test]
    fn same_path_detects_identical_paths() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = dir.path().join("a.txt");
        fs::write(&a, b"x").unwrap();
        assert!(same_path(&a, &a));
        // Same file via a different spelling.
        let dotted = dir.path().join(".").join("a.txt");
        assert!(same_path(&a, &dotted));
    }

    #[test]
    fn same_path_distinguishes_different_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.txt");
        fs::write(&a, b"x").unwrap();
        fs::write(&b, b"y").unwrap();
        assert!(!same_path(&a, &b));
    }

    #[test]
    fn require_file_rejects_missing_and_directories() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(require_file(&dir.path().join("nope")).is_err());
        assert!(require_file(dir.path()).is_err());
        let f = dir.path().join("f");
        fs::write(&f, b"").unwrap();
        assert!(require_file(&f).is_ok());
    }
}
