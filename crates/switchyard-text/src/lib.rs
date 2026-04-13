//! Shared UTF-8-safe text utilities for previews, badges, and summaries.

/// Return the first `max_chars` Unicode scalar values from `text`.
///
/// Unlike byte slicing, this is always UTF-8 safe.
pub fn prefix_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// Return a UTF-8-safe preview truncated to `max_chars`, appending `suffix`
/// when truncation occurs.
pub fn preview_chars(text: &str, max_chars: usize, suffix: &str) -> String {
    let mut iter = text.chars();
    let mut truncated = String::new();
    for ch in iter.by_ref().take(max_chars) {
        truncated.push(ch);
    }

    if iter.next().is_some() {
        truncated.push_str(suffix);
        truncated
    } else {
        text.to_string()
    }
}

/// Trim leading/trailing whitespace, then return a UTF-8-safe preview when the
/// remaining content is non-empty.
pub fn preview_trimmed(text: &str, max_chars: usize, suffix: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| preview_chars(trimmed, max_chars, suffix))
}

/// Collapse repeated whitespace to single spaces, trim the result, and return a
/// UTF-8-safe preview.
pub fn preview_collapsed(text: &str, max_chars: usize, suffix: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    preview_trimmed(&collapsed, max_chars, suffix).unwrap_or_default()
}

/// Return the largest valid UTF-8 prefix whose byte length is at most
/// `max_bytes`.
pub fn prefix_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        text
    } else {
        &text[..text.floor_char_boundary(max_bytes)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_chars_is_utf8_safe() {
        assert_eq!(prefix_chars("你好abc", 2), "你好");
        assert_eq!(prefix_chars("abc", 8), "abc");
    }

    #[test]
    fn preview_chars_appends_suffix_only_when_truncated() {
        assert_eq!(preview_chars("你好世界", 2, "..."), "你好...");
        assert_eq!(preview_chars("hello", 10, "..."), "hello");
    }

    #[test]
    fn preview_trimmed_discards_blank_input() {
        assert_eq!(preview_trimmed("   ", 10, "..."), None);
        assert_eq!(
            preview_trimmed("  你好世界  ", 2, "..."),
            Some("你好...".to_string())
        );
    }

    #[test]
    fn preview_collapsed_normalizes_whitespace() {
        assert_eq!(
            preview_collapsed("hello   \n   world  again", 12, "…"),
            "hello world …"
        );
    }

    #[test]
    fn prefix_bytes_stays_on_char_boundary() {
        let value = "这是一次 UTF-8 测试";
        let prefix = prefix_bytes(value, 5);
        assert!(std::str::from_utf8(prefix.as_bytes()).is_ok());
        assert_eq!(prefix, "这");
    }
}
