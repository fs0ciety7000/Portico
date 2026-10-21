//! Small text-input helpers shared by the forms (key generation, snippets,
//! secret dialogs). Inputs are append-only (type, `Backspace`, `Ctrl-U`), as
//! in the existing prompts: no cursor to lose, nothing to select by mistake.
//!
//! Secret buffers are `Zeroizing` and pre-sized so typing never reallocates
//! (no stray copies of a passphrase left in freed memory).

use zeroize::Zeroizing;

/// Longest single-line value, in characters.
pub(crate) const MAX_LINE: usize = 256;

/// An empty secret buffer that will not reallocate while typed into.
pub(crate) fn secret_buffer() -> Zeroizing<String> {
    Zeroizing::new(String::with_capacity(MAX_LINE * 4))
}

/// Appends `c` unless the value is full or `c` is a control character.
pub(crate) fn push_char(value: &mut String, c: char, max: usize) {
    if !c.is_control() && value.chars().count() < max {
        value.push(c);
    }
}

/// Appends the first line of a paste, without control characters.
pub(crate) fn paste_line(value: &mut String, text: &str, max: usize) {
    let line = text.lines().next().unwrap_or_default();
    for c in line.chars() {
        push_char(value, c, max);
    }
}

/// Removes C0/C1 controls and bidi overrides from text read from files or
/// the agent before it is drawn (a comment must not move the cursor or
/// reorder what is displayed).
pub(crate) fn display_safe(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .collect()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inputs_refuse_controls_and_overflow() {
        let mut v = String::new();
        push_char(&mut v, '\u{1b}', 10);
        push_char(&mut v, 'a', 10);
        paste_line(&mut v, "bc\u{7}d\nsecond", 4);
        assert_eq!(v, "abcd");
        assert_eq!(display_safe("me\u{202E}@lap\ttop"), "me@laptop");
    }
}
