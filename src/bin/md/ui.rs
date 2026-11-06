/// Colour palette — shared from the main ptm source.
///
/// `md` builds a `Theme` (the markdown renderer takes one) but does not read
/// any of its fields: `markdown.rs` styles with its own `Style` constants.  The
/// allow keeps that honest instead of reporting seven "field is never read"
/// warnings per `md` build; the fields *are* read by `ptm`.
#[path = "../../ui/theme.rs"]
#[allow(dead_code)]
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
