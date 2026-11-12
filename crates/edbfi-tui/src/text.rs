// SPDX-License-Identifier: AGPL-3.0-or-later
//! Making text safe and fitting it into terminal cells.
//!
//! Paths, error messages and journal entries can hold any character. [`sanitize`] replaces everything that
//! could move the cursor, start an escape sequence or reorder text, and keeps printable Unicode such as
//! accented or Cyrillic folder names. Widths are counted in terminal cells, so a wide character never splits.
#![forbid(unsafe_code)]

use std::borrow::Cow;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Whether `c` reorders text: the Unicode bidirectional controls and marks.
const fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// The replacement for `c`, or `None` to keep it.
const fn replacement(c: char) -> Option<char> {
    match c {
        '\t' | '\n' | '\r' | '\u{2028}' | '\u{2029}' | '\u{85}' => Some(' '),
        c if c.is_control() || is_bidi_control(c) => Some('?'),
        _ => None,
    }
}

/// `text` with every control character, escape, bidirectional control and line separator replaced: line
/// breaks and tabs by a space, the rest by `?`. Printable characters are kept.
#[must_use]
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if text.chars().all(|c| replacement(c).is_none()) {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(text.chars().map(|c| replacement(c).unwrap_or(c)).collect())
    }
}

/// `text` for output that may not be Unicode: [`sanitize`]d, then each non-ASCII character becomes one `?`.
#[must_use]
pub fn ascii(text: &str) -> String {
    sanitize(text)
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect()
}

/// The cells `text` occupies. Call on sanitised text.
#[must_use]
pub fn width(text: &str) -> usize {
    text.width()
}

fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// `text` cut to at most `max` cells, ending in `ellipsis` if anything was cut.
#[must_use]
pub fn fit_end(text: &str, max: usize, ellipsis: &str) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let Some(room) = max.checked_sub(width(ellipsis)) else {
        return String::new();
    };
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w > room {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push_str(ellipsis);
    out
}

/// `text` cut to at most `max` cells by dropping its start, so a path keeps its file name.
#[must_use]
pub fn fit_start(text: &str, max: usize, ellipsis: &str) -> String {
    if width(text) <= max {
        return text.to_owned();
    }
    let Some(room) = max.checked_sub(width(ellipsis)) else {
        return String::new();
    };
    let mut tail = Vec::new();
    let mut used = 0;
    for c in text.chars().rev() {
        let w = char_width(c);
        if used + w > room {
            break;
        }
        used += w;
        tail.push(c);
    }
    let mut out = ellipsis.to_owned();
    out.extend(tail.iter().rev());
    out
}

/// `text` broken into lines of at most `max` cells, at spaces where possible. A word longer than a line is
/// split. Never returns an empty list, and every line fits unless `max` is narrower than one character.
#[must_use]
pub fn wrap(text: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for word in text.split(' ') {
        let w = width(word);
        if used > 0 && used + 1 + w <= max {
            line.push(' ');
            line.push_str(word);
            used += 1 + w;
            continue;
        }
        if used > 0 {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        if w <= max {
            line.push_str(word);
            used = w;
            continue;
        }
        for c in word.chars() {
            let cw = char_width(c);
            if used + cw > max && used > 0 {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(c);
            used += cw;
        }
    }
    lines.push(line);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn controls_escapes_and_bidi_are_replaced() {
        assert_eq!(sanitize("a\x1b[31mred\x07"), "a?[31mred?");
        assert_eq!(sanitize("osc\x1b]0;title\u{9c}"), "osc?]0;title?");
        assert_eq!(
            sanitize("two\nlines\tand\u{2028}more"),
            "two lines and more"
        );
        assert_eq!(sanitize("abc\u{202E}fdp.exe"), "abc?fdp.exe");
        assert_eq!(sanitize("del\x7f"), "del?");
        assert!(matches!(sanitize("plain"), Cow::Borrowed("plain")));
    }

    #[test]
    fn printable_unicode_paths_survive() {
        let path = r"C:\Users\Jérôme\Документы\settings.toml";
        assert_eq!(sanitize(path), path);
        assert_eq!(ascii(path), r"C:\Users\J?r?me\?????????\settings.toml");
    }

    #[test]
    fn fitting_counts_cells_not_bytes() {
        assert_eq!(fit_end("Unlimited ammo", 8, "…"), "Unlimit…");
        assert_eq!(fit_end("short", 8, "…"), "short");
        assert_eq!(fit_end("日本語のパス", 7, "…"), "日本語…");
        assert_eq!(width(&fit_end("日本語のパス", 6, "…")), 5);
        assert_eq!(fit_end("abc", 0, "…"), "");
        assert_eq!(
            fit_start(r"C:\Users\x\AppData\Roaming\edbfi\settings.toml", 18, "…"),
            r"…bfi\settings.toml"
        );
        assert_eq!(fit_start("Ж/settings.toml", 14, "..."), "...ttings.toml");
    }

    #[test]
    fn wrapping_breaks_at_spaces_then_inside_long_words() {
        assert_eq!(
            wrap("Keeps reserve ammo; you still reload.", 16),
            ["Keeps reserve", "ammo; you still", "reload."]
        );
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), [""]);
    }

    proptest! {
        #[test]
        fn sanitized_text_has_no_controls(text in any::<String>()) {
            let clean = sanitize(&text);
            prop_assert!(clean.chars().all(|c| !c.is_control() && !is_bidi_control(c)));
            prop_assert_eq!(clean.chars().count(), text.chars().count());
            prop_assert!(ascii(&text).bytes().all(|b| (0x20..0x7f).contains(&b)));
        }

        #[test]
        fn fitted_and_wrapped_text_fits(text in "\\PC{0,60}", max in 1usize..40) {
            let text = sanitize(&text);
            prop_assert!(width(&fit_end(&text, max, "…")) <= max);
            prop_assert!(width(&fit_start(&text, max, "…")) <= max);
            for line in wrap(&text, max.max(2)) {
                prop_assert!(width(&line) <= max.max(2));
            }
        }
    }
}
