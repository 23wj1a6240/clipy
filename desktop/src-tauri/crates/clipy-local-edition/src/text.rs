//! String helpers shared by the modules.

/// Length in UTF-16 code units, the unit the planner's limits use.
pub(crate) fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

/// At most `max` UTF-16 code units, cut at a character boundary.
pub(crate) fn clip_utf16(s: &str, max: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        used += c.len_utf16();
        if used > max {
            break;
        }
        out.push(c);
    }
    out
}

/// At most `max` bytes of UTF-8, cut at a character boundary.
pub(crate) fn clip_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Trimmed, or None when nothing is left.
pub(crate) fn nonempty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_at_character_boundaries() {
        assert_eq!(clip_utf16("ab😀c", 3), "ab");
        assert_eq!(clip_utf16("ab😀c", 4), "ab😀");
        assert_eq!(utf16_len("ab😀c"), 5);
        assert_eq!(clip_bytes("aé", 2), "a");
        assert_eq!(clip_bytes("aé", 3), "aé");
        assert_eq!(nonempty(Some("  x ")), Some("x".into()));
        assert_eq!(nonempty(Some("   ")), None);
        assert_eq!(nonempty(None), None);
    }
}
