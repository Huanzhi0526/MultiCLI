//! Snippet construction for search results.
//!
//! The snippet is plain text. It is never HTML, never a command, and never a
//! link: history content is untrusted input, so the UI renders this string as
//! text. Control characters and bidi overrides are stripped before it leaves
//! the backend.

use serde_json::Value;

/// Finds `needle` in `haystack` ignoring ASCII case, returning byte offsets in
/// the original string. Only ASCII is folded: `to_ascii_lowercase` preserves
/// byte length, so the offsets stay valid UTF-8 boundaries.
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    if let Some(position) = haystack.find(needle) {
        return Some((position, position + needle.len()));
    }
    let lowered: Vec<u8> = haystack.bytes().map(|byte| byte.to_ascii_lowercase()).collect();
    let needle_lower: Vec<u8> = needle.bytes().map(|byte| byte.to_ascii_lowercase()).collect();
    let position = lowered
        .windows(needle_lower.len())
        .position(|window| window == needle_lower.as_slice())?;
    Some((position, position + needle.len()))
}

/// Extracts the readable body from a stored message content object.
pub fn body_text(content: &Value) -> String {
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    if let Some(text) = content.get("content").and_then(Value::as_str) {
        return text.to_string();
    }
    content.to_string()
}

/// Windowed snippet around the first match, with ellipses on the clipped sides.
pub fn snippet(text: &str, query: &str, before: usize, after: usize) -> (String, bool) {
    let text = crate::multicli::history::ids::sanitize_text(text);
    if text.trim().is_empty() {
        return (String::new(), false);
    }
    let Some((start, end)) = find_case_insensitive(&text, query) else {
        let (clipped, truncated) =
            crate::multicli::history::ids::truncate_chars(text.trim(), before + after);
        return (clipped, truncated);
    };
    let match_start_char = text[..start].chars().count();
    let match_chars = text[start..end].chars().count();
    let window_start_char = match_start_char.saturating_sub(before);
    let window_end_char = match_start_char + match_chars + after;
    let start_byte = text
        .char_indices()
        .nth(window_start_char)
        .map(|(offset, _)| offset)
        .unwrap_or(0);
    let end_byte = text
        .char_indices()
        .nth(window_end_char)
        .map(|(offset, _)| offset)
        .unwrap_or(text.len());
    let mut out = String::new();
    if start_byte > 0 {
        out.push('…');
    }
    out.push_str(text[start_byte..end_byte].trim());
    if end_byte < text.len() {
        out.push('…');
    }
    let truncated = start_byte > 0 || end_byte < text.len();
    (out, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snippet_keeps_the_match_and_bounds_the_window() {
        let text = "前".repeat(200) + "历史记录" + &"后".repeat(200);
        let (value, truncated) = snippet(&text, "历史记录", 20, 30);
        assert!(value.contains("历史记录"));
        assert!(truncated);
        assert!(value.chars().count() <= 60);
        assert!(value.starts_with('…') && value.ends_with('…'));
    }

    #[test]
    fn snippet_matches_ascii_case_insensitively() {
        let (value, _) = snippet("A Tool Result with TOOL output", "tool", 10, 10);
        assert!(value.contains("Tool"), "{value}");
    }

    #[test]
    fn snippet_falls_back_to_the_head_when_the_match_is_not_in_text() {
        let (value, truncated) = snippet("abcdef", "zzz", 3, 3);
        assert_eq!(value, "abcdef");
        assert!(!truncated);
        let (value, truncated) = snippet(&"x".repeat(100), "zzz", 10, 10);
        assert_eq!(value.chars().count(), 20);
        assert!(truncated);
    }

    #[test]
    fn snippet_strips_controls_and_bidi_overrides() {
        let (value, _) = snippet("a\u{0}b\u{202e}c 中文", "中文", 5, 5);
        assert_eq!(value, "abc 中文");
    }

    #[test]
    fn body_text_prefers_the_rendered_text_field() {
        assert_eq!(body_text(&json!({ "text": "hello" })), "hello");
        assert_eq!(body_text(&json!({ "content": "hi" })), "hi");
        assert!(body_text(&json!({ "text": "" })).is_empty());
    }
}
