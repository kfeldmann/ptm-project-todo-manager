//! Runtime spell-checking backed by Hunspell dictionary files.
//!
//! Dictionary files are **not** bundled with the binary. They must be
//! installed separately in one of the standard system locations or pointed
//! to via the `PTM_DICT_DIR` environment variable.
//!
//! # Dictionary lookup order
//! 1. `$PTM_DICT_DIR`        (explicit override)
//! 2. `/usr/share/hunspell`
//! 3. `/usr/share/myspell/dicts`
//! 4. `/usr/local/share/hunspell`
//! 5. `$XDG_DATA_HOME/hunspell`  (defaults to `~/.local/share/hunspell`)
//!
//! The dictionary name is always `en_US` (files `en_US.aff` + `en_US.dic`).
//! On most Linux systems, `apt install hunspell-en-us` populates
//! `/usr/share/hunspell/` with the required files.

use anyhow::Result;
use std::path::{Path, PathBuf};

const DICT_NAME: &str = "en_US";

// ─── SpellChecker ─────────────────────────────────────────────────────────────

pub struct SpellChecker {
    dict: spellbook::Dictionary,
    user_words_path: PathBuf,
}

impl SpellChecker {
    /// Try to find and load the dictionary.  Returns `None` if no dictionary
    /// files are found; errors are written to the application log.
    pub fn new(user_words_dir: &Path) -> Option<Self> {
        let dict_dir = find_dict_dir()?;

        let aff_path = dict_dir.join(format!("{DICT_NAME}.aff"));
        let dic_path = dict_dir.join(format!("{DICT_NAME}.dic"));

        let aff_bytes = std::fs::read(&aff_path)
            .map_err(|e| {
                crate::log::warn(&format!(
                    "spell: cannot read {}: {e}",
                    aff_path.display()
                ))
            })
            .ok()?;

        let encoding = detect_aff_encoding(&aff_bytes);
        let aff = decode_dict_bytes(aff_bytes, &encoding);

        let dic_bytes = std::fs::read(&dic_path)
            .map_err(|e| {
                crate::log::warn(&format!(
                    "spell: cannot read {}: {e}",
                    dic_path.display()
                ))
            })
            .ok()?;

        let dic = decode_dict_bytes(dic_bytes, &encoding);

        let dict = spellbook::Dictionary::new(&aff, &dic)
            .map_err(|e| {
                crate::log::warn(&format!("spell: failed to parse dictionary: {e}"))
            })
            .ok()?;

        let user_words_path = user_words_dir.join("user_words.txt");
        let mut checker = SpellChecker { dict, user_words_path };
        checker.load_user_words();
        Some(checker)
    }

    fn load_user_words(&mut self) {
        let Ok(content) = std::fs::read_to_string(&self.user_words_path) else {
            return;
        };
        for word in content.lines() {
            let word = word.trim();
            if !word.is_empty() {
                let _ = self.dict.add(word);
            }
        }
    }

    /// `true` if `word` is in the dictionary (case-insensitive via affix rules).
    pub fn check(&self, word: &str) -> bool {
        self.dict.check(word)
    }

    /// Up to 9 correction candidates for `word`, ordered by likelihood.
    pub fn suggest(&self, word: &str) -> Vec<String> {
        let mut out = Vec::new();
        self.dict.suggest(word, &mut out);
        out.truncate(9);
        out
    }

    /// Add `word` to the in-memory dictionary and persist it to the user
    /// word list so it survives restarts.
    pub fn add_word(&mut self, word: &str) {
        let _ = self.dict.add(word);
        if let Err(e) = append_word_to_file(&self.user_words_path, word) {
            crate::log::warn(&format!("spell: failed to save word '{word}': {e}"));
        }
    }
}

// ─── Tokenizer ────────────────────────────────────────────────────────────────

/// Split `text` into alternating `(text, is_word)` segments.
///
/// A *word* is a maximal run of Unicode-alphabetic characters.  A straight
/// apostrophe (`'`) or right-single-quotation-mark (`'`) that is immediately
/// surrounded by alphabetic characters is included, allowing contractions
/// such as "don't" and "it's" to be treated as single tokens.
pub fn tokenize(text: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0;
    let mut out: Vec<(String, bool)> = Vec::new();

    while i < n {
        if chars[i].is_alphabetic() {
            let mut word = String::new();
            while i < n {
                if chars[i].is_alphabetic() {
                    word.push(chars[i]);
                    i += 1;
                } else if is_apos(chars[i]) && i + 1 < n && chars[i + 1].is_alphabetic() {
                    // Mid-word apostrophe (contraction): include it.
                    word.push(chars[i]);
                    i += 1;
                } else {
                    break;
                }
            }
            out.push((word, true));
        } else {
            let mut piece = String::new();
            while i < n && !chars[i].is_alphabetic() {
                piece.push(chars[i]);
                i += 1;
            }
            out.push((piece, false));
        }
    }

    out
}

// ─── Private helpers ──────────────────────────────────────────────────────────

fn is_apos(c: char) -> bool {
    c == '\'' || c == '\u{2019}' // straight apostrophe or right single quotation mark
}

/// Extract the encoding name from a `SET <encoding>` line in `.aff` bytes.
///
/// The `SET` directive is always ASCII-safe, so scanning the first 512 bytes
/// as ASCII is reliable regardless of the actual file encoding.
fn detect_aff_encoding(aff_bytes: &[u8]) -> String {
    let head = std::str::from_utf8(&aff_bytes[..aff_bytes.len().min(512)]).unwrap_or("");
    for line in head.lines() {
        if let Some(enc) = line.strip_prefix("SET ") {
            return enc.trim().to_uppercase();
        }
    }
    "UTF-8".to_string()
}

/// Decode dictionary file bytes using the encoding name from the `.aff` `SET` line.
///
/// UTF-8 files are validated; any other encoding is treated as ISO-8859-1
/// (Latin-1), whose code points map one-to-one onto the first 256 Unicode
/// scalars, making the conversion lossless for all Hunspell 8-bit encodings.
fn decode_dict_bytes(bytes: Vec<u8>, encoding: &str) -> String {
    if encoding == "UTF-8" {
        match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                crate::log::warn("spell: .aff declares UTF-8 but file contains invalid UTF-8 — re-decoding as Latin-1");
                e.into_bytes().iter().map(|&b| b as char).collect()
            }
        }
    } else {
        // ISO-8859-1 through ISO-8859-15 and similar 8-bit sets:
        // casting each byte to char yields the correct Unicode code point.
        bytes.iter().map(|&b| b as char).collect()
    }
}

fn append_word_to_file(path: &Path, word: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{word}")?;
    Ok(())
}

/// Search candidate directories for `en_US.aff` + `en_US.dic`.
fn find_dict_dir() -> Option<PathBuf> {
    // 1. Explicit override.
    if let Ok(dir) = std::env::var("PTM_DICT_DIR") {
        let p = PathBuf::from(&dir);
        if has_dict_files(&p) {
            return Some(p);
        }
        crate::log::warn(&format!(
            "spell: PTM_DICT_DIR={dir} does not contain {DICT_NAME}.{{aff,dic}} — ignoring"
        ));
    }

    // 2. Standard system paths.
    let mut candidates = vec![
        PathBuf::from("/usr/share/hunspell"),
        PathBuf::from("/usr/share/myspell/dicts"),
        PathBuf::from("/usr/local/share/hunspell"),
    ];

    // 3. XDG user-local path.
    if let Some(base) = xdg_data_home() {
        candidates.push(base.join("hunspell"));
    }

    for dir in &candidates {
        if has_dict_files(dir) {
            return Some(dir.clone());
        }
    }

    crate::log::warn(&format!(
        "spell: {DICT_NAME} dictionary not found. \
         Install hunspell-en-us or set PTM_DICT_DIR."
    ));
    None
}

fn has_dict_files(dir: &Path) -> bool {
    dir.join(format!("{DICT_NAME}.aff")).exists()
        && dir.join(format!("{DICT_NAME}.dic")).exists()
}

fn xdg_data_home() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("XDG_DATA_HOME") {
        return Some(PathBuf::from(v));
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".local").join("share"))
}
