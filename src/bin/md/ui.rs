/// Colour palette — shared from the main ptm source.
#[path = "../../ui/theme.rs"]
pub mod theme;

/// Markdown → ratatui renderer — shared from the main ptm source.
#[path = "../../ui/markdown.rs"]
pub mod markdown;

/// Width-aware text helpers (wrapping, padding, truncation, search matching) —
/// shared from the main ptm source so `ptm` and `md` wrap text identically.
///
/// `md` uses the column measurement, the hint-bar height and the search
/// matcher; the list-oriented wrappers (`char_wrap`, `word_wrap`) are only
/// used by `ptm`'s views, so allow them to sit unused here rather than
/// splitting the shared file.
#[path = "../../ui/text.rs"]
#[allow(dead_code)]
pub mod text;
