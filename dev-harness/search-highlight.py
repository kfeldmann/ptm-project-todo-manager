#!/usr/bin/env python3
"""
search-highlight.py — reproduction for finding #17.

`split_span` lowercases the text to find a case-insensitive match, then uses the
*lowercased* byte/char offsets to slice the *original* text. Case folding is not
length-preserving ('İ' lowercases to two chars), so the highlighted range drifts.

This is a pure-function check: it reimplements the loop bound and range
computation from src/ui/views/project_detail.rs:71 (duplicated at
src/bin/md/main.rs:1118) so the behaviour can be asserted without a terminal.

    python3 dev-harness/search-highlight.py
"""

import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent / "lib"))
import ptmdb  # noqa: E402

results = ptmdb.Results("search-highlight")


def split_span(text, query):
    """Model of the current implementation: find in lowercased text, then use
    those indices against the original."""
    lower_text = text.lower()
    lower_query = query.lower()
    idx = lower_text.find(lower_query)
    if idx < 0:
        return None
    # The loop bound uses text_chars.len() (so it does not panic), but the
    # highlighted range is derived from the lowercased offset.
    start = idx
    end = idx + len(lower_query)
    return text[start:end]


def split_span_correct(text, query):
    """What the fix should produce: the matched region of the *original* text."""
    lower_text = text.lower()
    lower_query = query.lower()
    idx = lower_text.find(lower_query)
    if idx < 0:
        return None
    # Map the lowercased offset back to a char offset in the original.
    # Per-char lowercasing keeps this exact for the common cases.
    n = 0
    for i, ch in enumerate(text):
        if n >= idx:
            # Walk forward in the original for the query length in original chars.
            return text[i:i + len(query)]
        n += len(ch.lower())
    return None


def main():
    cases = [
        ("İstanbul", "i", "İ"),
        ("İstanbul", "İ", "İs"),
        ("İstanbul", "istan", "İsta"),
        ("Café", "café", "Café"),
        ("STRASSE", "strasse", "STRASSE"),
    ]
    print("checking split_span case-fold handling\n")
    for text, query, buggy in cases:
        got = split_span(text, query)
        want = split_span_correct(text, query)
        results.check(
            got == want,
            f"search {query!r} in {text!r} highlights {want!r}",
            f"got {got!r}"
            + (f"  (review cites {buggy!r})" if got == buggy else ""),
        )
    return results.finish()


if __name__ == "__main__":
    sys.exit(main())
