//! Deterministic identifiers and timestamp normalization for history import.
//!
//! Nothing here is random: a repeated import of the same native record must
//! produce the same session id, message id and source key, otherwise "import
//! twice" would duplicate rows and a rebuild could not recognize its own data.
//! Identifiers are hashes, never paths, so a session id can safely appear in a
//! UI, a log line or a diagnostics file.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Hex-encoded SHA-256 of arbitrary bytes.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// Deterministic textual id: prefix plus the first 40 hex chars (160 bits) of a
/// SHA-256 over length-prefixed parts. Length prefixes stop `("ab","c")` and
/// `("a","bc")` from collapsing into the same identity.
pub fn stable_id(prefix: &str, parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut text = String::with_capacity(prefix.len() + 1 + 40);
    text.push_str(prefix);
    text.push('-');
    for byte in digest.iter().take(20) {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

pub fn session_id(namespace: &str, native_id: &str) -> String {
    stable_id("hist", &[namespace, native_id])
}

pub fn message_id(session_id: &str, source_key: &str) -> String {
    stable_id("hmsg", &[session_id, source_key])
}

pub fn source_id(host_id: &str, agent_id: &str, profile: &str, canonical_root: &str) -> String {
    stable_id("hsrc", &[host_id, agent_id, profile, &fold_path(canonical_root)])
}

/// Windows comparison form of a path. The stored value keeps the case that is
/// on disk; only identity comparisons fold it.
pub fn fold_path(value: &str) -> String {
    value.replace('/', "\\").to_ascii_lowercase()
}

/// Identity of a transcript file: a hash of its folded canonical path.
///
/// The path (not the content) is the file's identity on purpose. A content
/// hash changes when a file is appended to, which would orphan the cursor and
/// force a full re-read of every growing log; a path identity keeps an append
/// incremental. Moves and replacements are still handled: a moved file is
/// re-read once at its new path (message ids dedupe the rows) and a replaced
/// file is detected by the prefix and anchor hashes below. The hash also keeps
/// the absolute path out of `sourceRef`.
pub fn path_identity(canonical_path: &str) -> String {
    format!("path:{}", &sha256_hex(fold_path(canonical_path).as_bytes())[..32])
}

/// Hash of the first `min(4096, len)` bytes. Unlike a whole-head hash it does
/// not change when a small file grows, so an append stays an append.
pub const PREFIX_HASH_BYTES: usize = 4096;

pub fn prefix_hash(head: &[u8], length: u64) -> String {
    let take = head.len().min(PREFIX_HASH_BYTES).min(length as usize);
    sha256_hex(&head[..take])
}

/// Identity of a file, derived from its first bytes rather than from its path.
///
/// A moved file keeps the same identity, so its already-imported prefix is not
/// re-read as if it were a new file. A file shorter than [`HEAD_SAMPLE_BYTES`]
/// mixes in its exact length so two nearly-empty files in different folders do
/// not collide; a longer file relies on the head sample (first lines of every
/// supported format contain the native session id).
pub const HEAD_SAMPLE_BYTES: usize = 32 * 1024;

pub fn file_identity(head: &[u8], total_len: u64) -> String {
    if head.is_empty() {
        return format!("empty:{total_len}");
    }
    if total_len <= HEAD_SAMPLE_BYTES as u64 {
        return format!("short:{}:{total_len}", &sha256_hex(head)[..32]);
    }
    format!("head:{}", &sha256_hex(head)[..40])
}

/// Hash of the bytes immediately before a committed offset. Used as a cheap
/// proof that an append really appended instead of rewriting the prefix.
pub fn anchor_hash(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut text = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        use std::fmt::Write as _;
        let _ = write!(text, "{byte:02x}");
    }
    text
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

/// Parses the date/time forms found in real transcripts into Unix milliseconds.
///
/// Accepted: `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS[.fraction][Z|±HH:MM|±HHMM]`.
/// A missing zone is treated as UTC; every supported producer writes UTC or an
/// explicit offset, and inventing a local zone would silently shift ordering.
pub fn parse_iso_millis(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.len() == 10 {
        let (year, month, day) = parse_date(&text[0..10])?;
        return Some(civil_to_millis(year, month, day, 0, 0, 0, 0));
    }
    let (date, rest) = text.split_once(['T', ' '])?;
    let (year, month, day) = parse_date(date)?;
    let (time, offset_minutes) = split_zone(rest)?;
    let (hour, minute, second, millis) = parse_time(time)?;
    let base = civil_to_millis(year, month, day, hour, minute, second, millis);
    Some(base - (offset_minutes as i64) * 60_000)
}

fn parse_date(text: &str) -> Option<(i32, u32, u32)> {
    let mut parts = text.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

/// Splits `HH:MM:SS.fraction` from an optional zone suffix.
fn split_zone(rest: &str) -> Option<(&str, i32)> {
    if let Some(time) = rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        return Some((time, 0));
    }
    // A trailing +HH:MM / -HH:MM (or +HHMM) is an offset, not part of the time.
    if rest.len() >= 6 {
        let tail = &rest[rest.len() - 6..];
        let sign = tail.as_bytes()[0];
        if (sign == b'+' || sign == b'-') && tail.as_bytes()[3] == b':' {
            let hours: i32 = tail[1..3].parse().ok()?;
            let minutes: i32 = tail[4..6].parse().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let total = hours * 60 + minutes;
            return Some((
                &rest[..rest.len() - 6],
                if sign == b'-' { -total } else { total },
            ));
        }
    }
    if rest.len() >= 5 {
        let tail = &rest[rest.len() - 5..];
        let sign = tail.as_bytes()[0];
        if (sign == b'+' || sign == b'-') && tail[1..].bytes().all(|byte| byte.is_ascii_digit()) {
            let hours: i32 = tail[1..3].parse().ok()?;
            let minutes: i32 = tail[3..5].parse().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let total = hours * 60 + minutes;
            return Some((
                &rest[..rest.len() - 5],
                if sign == b'-' { -total } else { total },
            ));
        }
    }
    Some((rest, 0))
}

fn parse_time(text: &str) -> Option<(u32, u32, u32, u32)> {
    let (clock, fraction) = match text.split_once('.') {
        Some((clock, fraction)) => (clock, Some(fraction)),
        None => (text, None),
    };
    let mut parts = clock.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next()?.parse().ok()?;
    let second: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let millis = match fraction {
        None => 0,
        Some(fraction) => {
            if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
                return None;
            }
            let mut value = 0_u32;
            let mut digits = 0;
            for byte in fraction.bytes().take(3) {
                value = value * 10 + u32::from(byte - b'0');
                digits += 1;
            }
            while digits < 3 {
                value *= 10;
                digits += 1;
            }
            value
        }
    };
    Some((hour, minute, second, millis))
}

/// Days from 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), then seconds within the day.
fn civil_to_millis(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    millis: u32,
) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i32;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era as i64 * 146_097 + day_of_era as i64 - 719_468;
    days * 86_400_000
        + i64::from(hour) * 3_600_000
        + i64::from(minute) * 60_000
        + i64::from(second) * 1_000
        + i64::from(millis)
}

/// Canonical wire timestamp: UTC, milliseconds, trailing `Z`.
pub fn iso_from_millis(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(86_400_000);
    let millis_of_day = epoch_ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    let hour = millis_of_day / 3_600_000;
    let minute = (millis_of_day % 3_600_000) / 60_000;
    let second = (millis_of_day % 60_000) / 1_000;
    let millis = millis_of_day % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month as u32, day as u32)
}

/// Accepts the timestamp spellings actually observed:
/// ISO string (Codex / Claude) and integer epoch in ms (CodeBuddy), with a
/// seconds fallback for older producers.
pub fn normalize_timestamp(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            if let Some(millis) = parse_iso_millis(text) {
                return Some(iso_from_millis(millis));
            }
            let digits = text.trim();
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return epoch_value_to_iso(digits.parse::<i64>().ok()?);
            }
            None
        }
        Value::Number(number) => epoch_value_to_iso(number.as_i64()?),
        _ => None,
    }
}

fn epoch_value_to_iso(value: i64) -> Option<String> {
    let millis = if value >= 100_000_000_000 {
        value
    } else if value >= 1_000_000_000 {
        value * 1_000
    } else {
        return None;
    };
    Some(iso_from_millis(millis))
}

/// Removes C0/C1 controls except tab and newline, plus the bidirectional
/// overrides that can visually reorder a transcript. History text is untrusted
/// input; it is rendered as text, never as markup.
pub fn sanitize_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        if ch == '\n' || ch == '\t' {
            out.push(ch);
            continue;
        }
        if code < 0x20 || (0x7F..=0x9F).contains(&code) {
            continue;
        }
        if (0x202A..=0x202E).contains(&code) || (0x2066..=0x2069).contains(&code) {
            continue;
        }
        out.push(ch);
    }
    out
}

/// Truncates on a character boundary and reports whether anything was dropped.
pub fn truncate_chars(text: &str, limit: usize) -> (String, bool) {
    if text.chars().count() <= limit {
        return (text.to_string(), false);
    }
    let mut out = String::with_capacity(limit * 2);
    for ch in text.chars().take(limit) {
        out.push(ch);
    }
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn stable_ids_are_stable_and_part_sensitive() {
        assert_eq!(session_id("ns", "native"), session_id("ns", "native"));
        assert_ne!(session_id("ns", "native"), session_id("ns", "native2"));
        assert_ne!(stable_id("x", &["ab", "c"]), stable_id("x", &["a", "bc"]));
        assert!(session_id("ns", "n").starts_with("hist-"));
        assert_eq!(session_id("ns", "n").len(), 45);
    }

    #[test]
    fn timestamps_round_trip_in_canonical_form() {
        assert_eq!(
            normalize_timestamp(&json!("2026-11-02T01:30:14.123Z")).as_deref(),
            Some("2026-11-02T01:30:14.123Z")
        );
        assert_eq!(
            normalize_timestamp(&json!("2026-11-02T01:30:14.1234567Z")).as_deref(),
            Some("2026-11-02T01:30:14.123Z")
        );
        assert_eq!(
            normalize_timestamp(&json!("2026-11-02T09:30:14+08:00")).as_deref(),
            Some("2026-11-02T01:30:14.000Z")
        );
        assert_eq!(
            normalize_timestamp(&json!(1_761_000_000_000_i64)).as_deref(),
            Some("2025-10-20T22:40:00.000Z")
        );
        assert_eq!(
            normalize_timestamp(&json!(1_761_000_000_i64)).as_deref(),
            Some("2025-10-20T22:40:00.000Z")
        );
        assert_eq!(normalize_timestamp(&json!("not a date")), None);
        assert_eq!(normalize_timestamp(&json!(true)), None);
        for value in [
            "1970-01-01T00:00:00.000Z",
            "2000-02-29T23:59:59.999Z",
            "2026-12-31T23:59:59.999Z",
        ] {
            let millis = parse_iso_millis(value).unwrap();
            assert_eq!(iso_from_millis(millis), value);
        }
    }

    #[test]
    fn text_sanitization_drops_controls_and_keeps_unicode() {
        assert_eq!(sanitize_text("a\u{0}b\u{202e}c\n中文"), "abc\n中文");
        assert_eq!(truncate_chars("中文abc", 2), ("中文".to_string(), true));
        assert_eq!(truncate_chars("ab", 2), ("ab".to_string(), false));
    }

    #[test]
    fn file_identity_follows_content_not_path() {
        let head = vec![b'x'; HEAD_SAMPLE_BYTES];
        assert_eq!(file_identity(&head, 1_000_000), file_identity(&head, 2_000_000));
        assert_ne!(file_identity(b"abc", 3), file_identity(b"abd", 3));
        assert_eq!(file_identity(b"", 0), "empty:0");
    }
}
