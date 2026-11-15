# UI Conventions

## Theme and colour palette

Colors come from `Theme::default()` (defined in `ui/theme.rs`, shared with
`md`). Always use theme fields rather than hard-coding colors, except for
transient states like `Color::DarkGray` for placeholder text.

The target terminal is **Apple Terminal.app on macOS**, which supports
**256-color (xterm-256color)** but **not 24-bit true color**. `$COLORTERM` is
not set in this environment.

- ✅ Use `Color::Indexed(n)` or ratatui named colors (`Color::Red`,
  `Color::LightBlue`, etc.) — these map to the 256-color palette.
- ❌ Do **not** use `Color::Rgb(r, g, b)` — 24-bit sequences are silently
  ignored by this terminal, producing no color change.

## Hint bar

Every screen/focus mode has a hint string. Its height is computed dynamically
by `super::hint_height(hints, area.width)` (wraps at terminal width) so the
main content area shrinks accordingly.

**Always update the hints string when adding or removing keybindings**, and
add the key **only for the modes that accept it**. `n`/`N` in particular mean
"next/previous match" on the Description tab and in the update viewer but "new
item" on the Todos tab. The hint bar's height drives the layout, so a wrong
string is both a lie and a rendering bug; the tests in `project_detail.rs`
assert the advertised keys against `key_help_lines()`.

```rust
// pattern used in every view
let hints = if <editing_mode> { "  Enter:save  ESC:cancel" } else { "  j/k:..." };
let chunks = Layout::default()
    .constraints([Constraint::Min(3), Constraint::Length(super::hint_height(hints, area.width))])
    ...
```

## Text width — always measure in columns

Terminal layout is measured in **display columns**, not characters and never
bytes. A CJK ideograph or an emoji occupies two columns; a combining mark
occupies none. `str::chars().count()` is therefore wrong for any width
decision (CJK titles used to wrap at half the intended width and overflow the
pane), and `str::len()` is wrong twice over.

All wrapping, truncation, padding and row-height arithmetic goes through
`ui/text.rs` (path-included by `md`, so both binaries agree):

| Helper | Use |
|--------|-----|
| `display_width(s)` | columns occupied by `s` |
| `char_display_width(c)` | columns occupied by one `char` |
| `char_wrap(text, w)` | hard-wrap at `w` columns |
| `word_wrap(text, w)` | word-wrap at `w` columns (preserves space runs) |
| `wrap_rows(s, w)` | wrap one already-split string into rows |
| `split_at_width(s, w)` | `(prefix, rest)` at a column budget, space-preferred |
| `pad_to_width(s, w)` | right-pad to exactly `w` columns |
| `ellipsize(s, w)` | truncate to `w` columns, **always** ending in `…` |
| `hint_height(text, w)` | rows a hint string needs at `w` columns |

`format!("{:<w$}")` pads by **characters** and misaligns any column containing
wide text — use `pad_to_width`. `views/mod.rs` re-exports these as `super::…`
for the view modules.

**Case-insensitive matching** lives here too: `find_matches` returns the char
ranges of every occurrence, mapping a case-folded match back onto whole
characters of the original text. Using lowercased offsets directly against the
original is wrong because folding is not length-preserving (`'İ'` lowercases to
two chars). `split_matches` is the segment-level form of the same thing.

## Search highlighting — `ui/search.rs`

`ui/search.rs` (path-included by `md`) turns those ranges into styled
`ratatui` spans. Both binaries use it; **do not reintroduce a local copy** —
`views/project_detail.rs` and `bin/md/main.rs` each had one, and they had
already drifted.

| Function | Use |
|----------|-----|
| `highlight(lines, q, current_idx, cur_style, other_style)` | whole buffer: styles every match, paints the current one, returns `Highlighted { lines, marks }` |
| `highlight_line(line, q, style)` | one row, all matches in one style (lists that do not navigate) |

The `MatchLoc` type carries a match's `(line, occurrence)` coordinate — that is
what `n`/`N` navigation steps through.

Two rules, both of which were live bugs:

* **Match on the whole line, not per span.** `markdown.rs` emits one span per
  word (and separate spans for markup, list markers and table cells), so a
  query containing a space never fits inside a single span: `/We the people`
  highlighted nothing, and the highlight vanished as soon as a space was typed.
  `search::highlight` concatenates a line's spans, matches, then maps the char
  ranges back onto the spans, keeping each span's base style.
* **The current-match pointer is a flat index into `marks`.** Style every match
  in one pass and index it — an implementation that only advances its
  occurrence counter while hunting for the current match leaves `n` moving the
  pointer with no match ever painted yellow on a line that holds several.

A match cannot cross a *rendered line* boundary (a word-wrap point or a block
break); lines are searched independently, which keeps the `(line, occ)`
coordinates that `n`/`N` navigation is built on exact.

## Reminder display

Reminder dates are stored as `YYYY-MM-DD` strings and truncated to 10
characters on display. They are rendered with a leading two-space pad:

```rust
.map(|r| format!("  {}", views::date_prefix(r)))
```

Always truncate through `views::date_prefix` (in `ui/views/mod.rs`), **never
with a byte slice** like `&r[..r.len().min(10)]`: that panics when byte 10
lands inside a multi-byte character. `reminder` is a free-text `TEXT` column —
the app's input and `util/import_projects.py` validate `%Y-%m-%d`, but
`--merge` copies whatever the source database holds, so a display helper must
not assume ASCII.

Do **not** add emoji or icons before reminder dates (spacing issues on some
terminals).

## Key Help wrapping

`wrap_key_row` (in `overlay.rs`) wraps a key-help description into the column
budget `desc_width = inner_width - KEY_COL_WIDTH` (14). Two rules:

- **`desc_width` is a column count, so count characters, not bytes.** The
  rows contain `→`; slicing a `&str` at a column offset panics when it lands
  inside a character.
- **`desc_width == 0` means "do not wrap"** — render the row unwrapped and let
  the paragraph clip it. A hard break at column 0 makes no progress and loops
  forever.

`overlay.rs`'s unit tests sweep `desc_width` 0..=60 over every real
`key_help_lines()` row, so adding a description with wide characters is
covered automatically.

## Markdown tables

`markdown.rs` renders a table as a box: every line of it — top rule, header,
row rules, body rows, bottom rule — must be exactly the same display width, or
the box visibly breaks. The width depends on the terminal, so a table that
looks right at 120 columns can break at 60. Two rules keep it that way:

- **A block marker's trailing space belongs to the source, not to the cell
  text.** `parse_block_marker` consumes it (as it does for `> `, `- `, `1. `)
  and `build_cell` re-supplies it as part of the rendered prefix. Leaving it
  in `rest` rendered `| # |` as `│ #  │` beside `│ A │` and widened the column
  by one. A prefix-only cell (`| # |`) must also drop the separator space
  entirely — the cell is exactly as wide as its marker.
- **A prefix may not take the whole column.** `emit_pfx` truncates a prefix
  (`• `, `1. `, `# `, `│ `, indent) to `width - 1` columns when the cell has
  text, so the text always has somewhere to land; otherwise a column squeezed
  below its marker emits an over-wide line, and `render_table`'s
  `w.saturating_sub(content_w)` turns that into a short `rpad`, so the row
  renders *longer* than the rules around it.

`rm_table_every_line_has_the_same_width` sweeps widths 12..=110 over a table
containing every cell kind and asserts the invariant, so both regressions are
covered.

## Overlays

All overlays are rendered centered over the full terminal area by
`overlay::render_overlay`. Only one `Overlay` variant can be active at a time.
The overlay intercepts **all** key events until dismissed.

### Warn / Error Modal (`Overlay::Warn`)

A lightweight, reusable notice overlay for conditions that need user attention
but require no input — missing features, configuration problems, etc.

```rust
self.overlay = Some(Overlay::Warn {
    title: " My Warning Title ".into(),   // shown in the border
    body:  "Line one.\nLine two.".into(), // supports \n-separated lines
});
```

- Dismissed by **any key press**. The dismissing key is **not** swallowed when
  it is safe to replay (see
  [architecture.md](architecture.md#overlay-dismissal-and-redispatch)), so a
  reflexive second `e` at an edit-conflict warning still opens the editor.
- Border is rendered in yellow via `Color::Yellow`.
- Body text is word-wrapped; keep lines under ~58 characters for best fit.
- Prefer this over `crate::log::warn` whenever the condition is interactive
  and the user would benefit from on-screen guidance (see
  [logging.md](logging.md#logwarn-vs-overlaywarn)).
