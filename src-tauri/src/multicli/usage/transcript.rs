//! Token usage read from **locally written transcripts** (Claude Code).
//!
//! This is deliberately not a history importer: it never stores message text,
//! never adds a message row and never touches the FTS index. It walks the same
//! roots the user already enabled as read-only history sources, reads only the
//! numeric `message.usage` object of assistant records, and throws the text
//! away.
//!
//! Boundaries:
//!
//! - Only **configured** roots are scanned (S03's rule: no hidden default
//!   root, no full-disk walk).
//! - Budgets: at most [`MAX_FILES`] files, [`MAX_BYTES`] read per pass, depth
//!   [`MAX_DEPTH`], and a single file larger than [`MAX_FILE_BYTES`] is skipped
//!   and **reported as an omission** — never silently half-counted.
//! - An omission (oversized file, pruned directory, unreadable entry, a
//!   truncated usage line) makes the pass **incomplete**: [`TranscriptTotals::complete`]
//!   turns false, window totals become `None`, and the reason is listed. Budgets
//!   are kept as they are: raising them would only hide the omission.
//! - A line that contains no `"usage"` is skipped by a byte search before any
//!   JSON parsing, so a large transcript costs one cheap scan per line.
//! - Only `type: "assistant"` records are provider samples. A `user` record, a
//!   summary or any other record type is not assistant consumption and is never
//!   counted — even when a nested object happens to be called `usage`.
//! - A missing, `null` or non-integer component is **unknown**, not zero. A real
//!   reported `0` stays `0`. A window whose records do not all report the same
//!   components has no total at all: partial sums are not totals.
//! - Records are de-duplicated by `(sessionId, uuid)`: a resumed session copies
//!   earlier records into a new file, and counting those twice would inflate
//!   every number.
//! - Cached tokens are kept separate. Claude's `input_tokens` **excludes**
//!   `cache_read_input_tokens` / `cache_creation_input_tokens`, so the total is
//!   `input + cache_creation + cache_read + output` and nothing is added twice.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use super::model::{TokenSemantics, UsageSample, UsageWindow};

pub const MAX_FILES: usize = 2_000;
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_DEPTH: usize = 8;
/// Wall-clock budget for one pass. A slow or network-backed root must not block
/// the refresh thread forever; when it is hit the pass is incomplete, not
/// "everything I managed to read".
pub const MAX_SCAN_MS: u128 = 20_000;
/// Bound on the dedupe set: beyond this, records keep being counted but the
/// pass reports truncation instead of pretending full coverage.
pub const MAX_RECORDS: usize = 400_000;

/// Sums of one day (or of a window). Every component is `Option`:
///
/// - `Some(value)` — every contributing record reported a valid integer;
/// - `None` — at least one record did not, so the sum is unknown, not zero.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenSums {
    pub input: Option<u64>,
    pub cache_creation: Option<u64>,
    pub cache_read: Option<u64>,
    pub output: Option<u64>,
    /// How many assistant records contributed to this bucket.
    pub messages: u64,
    /// How many of them were missing at least one component.
    pub records_missing_fields: u64,
}

/// Merges one component: two known values add up, anything else is unknown.
/// There is no "first value" special case — a bucket's first value is inserted
/// directly, so `None` always means "unknown", never "empty accumulator".
fn combine(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        // One unknown addend makes the sum unknown; adding zero would invent a
        // total the provider never reported.
        _ => None,
    }
}

impl TokenSums {
    pub fn add(&mut self, other: &TokenSums) {
        self.input = combine(self.input, other.input);
        self.cache_creation = combine(self.cache_creation, other.cache_creation);
        self.cache_read = combine(self.cache_read, other.cache_read);
        self.output = combine(self.output, other.output);
        self.messages = self.messages.saturating_add(other.messages);
        self.records_missing_fields = self
            .records_missing_fields
            .saturating_add(other.records_missing_fields);
    }

    /// Total under Claude's semantics. All four components are required: a
    /// total computed from a subset would be a fabricated number.
    pub fn total(&self) -> Option<u64> {
        match (self.input, self.cache_read, self.cache_creation, self.output) {
            (Some(input), Some(cache_read), Some(cache_creation), Some(output)) => Some(
                input
                    .saturating_add(cache_read)
                    .saturating_add(cache_creation)
                    .saturating_add(output),
            ),
            _ => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.messages == 0
    }

    /// A window with **no records** in a complete scan is a real zero: the whole
    /// period was covered and nothing was spent. This is the only place a zero
    /// is produced without the provider reporting it.
    fn observed_zero() -> Self {
        Self {
            input: Some(0),
            cache_creation: Some(0),
            cache_read: Some(0),
            output: Some(0),
            messages: 0,
            records_missing_fields: 0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptTotals {
    pub files_read: usize,
    /// Files that exist but were not read because of the per-file size cap.
    pub files_skipped: usize,
    /// Files and configured roots that could not be read or listed.
    pub files_failed: usize,
    /// Directories the depth budget refused to descend into.
    pub dirs_pruned: usize,
    /// Lines that mention `usage` but are not valid JSON (usually a truncated
    /// tail). Their numbers are unknown, so the pass cannot claim completeness.
    pub parse_failures: usize,
    /// Directory entries whose type or size could not be read.
    pub unreadable_entries: usize,
    pub bytes_read: u64,
    pub lines_read: u64,
    /// Assistant records with a usage object (including records that reported
    /// no usable value).
    pub records: u64,
    /// Assistant records whose usage object had no usable number at all.
    pub records_without_values: u64,
    /// True when a budget stopped the pass before the whole tree was read.
    pub truncated: bool,
    /// True when the wall-clock budget stopped the pass.
    pub timed_out: bool,
    /// UTC calendar day (`YYYY-MM-DD`) → sums.
    pub days: BTreeMap<String, TokenSums>,
}

impl TranscriptTotals {
    /// A pass is complete only when nothing was left out. Every omission is
    /// explicit; there is no "probably fine" branch.
    pub fn complete(&self) -> bool {
        !self.truncated
            && !self.timed_out
            && self.files_failed == 0
            && self.files_skipped == 0
            && self.dirs_pruned == 0
            && self.parse_failures == 0
            && self.unreadable_entries == 0
    }

    /// Human-readable, path-free reasons the pass is not a total. Empty when
    /// the pass is complete.
    pub fn incomplete_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();
        if self.days.contains_key("unknown") {
            reasons.push("部分用量记录日期未知；今日及近7/30天数值不可确定，累计值按已知分量保留".to_string());
        }
        if self.files_skipped > 0 {
            reasons.push(format!(
                "{} 个转录文件超过 {} MiB 单文件上限，未计入",
                self.files_skipped,
                MAX_FILE_BYTES / (1024 * 1024)
            ));
        }
        if self.dirs_pruned > 0 {
            reasons.push(format!(
                "{} 个目录超过 {} 层深度上限，未继续遍历",
                self.dirs_pruned, MAX_DEPTH
            ));
        }
        if self.parse_failures > 0 {
            reasons.push(format!(
                "{} 行含 usage 的记录无法解析（可能被截断），其数值未知",
                self.parse_failures
            ));
        }
        if self.files_failed > 0 {
            reasons.push(format!(
                "{} 个来源文件/目录不可读",
                self.files_failed
            ));
        }
        if self.unreadable_entries > 0 {
            reasons.push(format!(
                "{} 个目录条目无法读取类型或大小",
                self.unreadable_entries
            ));
        }
        if self.truncated {
            reasons.push(format!(
                "扫描预算用尽（上限 {} 个文件 / {} MiB / {} 条记录），未覆盖全部转录",
                MAX_FILES,
                MAX_BYTES / (1024 * 1024),
                MAX_RECORDS
            ));
        }
        if self.timed_out {
            reasons.push(format!(
                "扫描超过 {} 秒时间预算，未覆盖全部转录",
                MAX_SCAN_MS / 1000
            ));
        }
        reasons
    }

    /// Window sums relative to `today` (`YYYY-MM-DD`, UTC). `None` unless the
    /// pass was complete, because a partial scan cannot honestly claim a total.
    pub fn window(&self, today: &str, days: u32) -> Option<TokenSums> {
        if !self.complete() || self.days.contains_key("unknown") {
            return None;
        }
        let mut sums: Option<TokenSums> = None;
        for (day, values) in &self.days {
            if within_window(day, today, days) {
                sums = Some(match sums {
                    None => *values,
                    Some(mut accumulated) => {
                        accumulated.add(values);
                        accumulated
                    }
                });
            }
        }
        Some(sums.unwrap_or_else(TokenSums::observed_zero))
    }

    pub fn lifetime(&self) -> Option<TokenSums> {
        if !self.complete() {
            return None;
        }
        let mut sums: Option<TokenSums> = None;
        for values in self.days.values() {
            sums = Some(match sums {
                None => *values,
                Some(mut accumulated) => {
                    accumulated.add(values);
                    accumulated
                }
            });
        }
        Some(sums.unwrap_or_else(TokenSums::observed_zero))
    }

    pub fn samples(&self, today: &str, source: &str) -> Vec<UsageSample> {
        let semantics = TokenSemantics::InputExcludesCached;
        let mut samples = Vec::new();
        for (label, window, days) in [
            ("今日 token", UsageWindow::Daily, 1_u32),
            ("近 7 天 token", UsageWindow::Weekly, 7),
            ("近 30 天 token", UsageWindow::Monthly, 30),
        ] {
            let sums = self.window(today, days);
            samples.push(UsageSample::token_measurement(
                label,
                window,
                semantics,
                sums.and_then(|value| value.input),
                sums.and_then(|value| value.cache_read),
                sums.and_then(|value| value.cache_creation),
                sums.and_then(|value| value.output),
                sums.and_then(|value| value.total()),
                source,
            ));
        }
        let lifetime = self.lifetime();
        samples.push(UsageSample::token_measurement(
            "累计 token",
            UsageWindow::Lifetime,
            semantics,
            lifetime.and_then(|value| value.input),
            lifetime.and_then(|value| value.cache_read),
            lifetime.and_then(|value| value.cache_creation),
            lifetime.and_then(|value| value.output),
            lifetime.and_then(|value| value.total()),
            source,
        ));
        samples
    }
}

fn within_window(day: &str, today: &str, days: u32) -> bool {
    let (Some(day_ms), Some(today_ms)) = (
        crate::multicli::history::ids::parse_iso_millis(day),
        crate::multicli::history::ids::parse_iso_millis(today),
    ) else {
        return false;
    };
    let age_days = (today_ms - day_ms) / 86_400_000;
    (0..i64::from(days)).contains(&age_days)
}

/// Scans the configured roots. A root that does not exist is a failure the
/// caller turns into a visible state, not an empty success.
pub fn scan_roots(roots: &[PathBuf]) -> TranscriptTotals {
    let mut totals = TranscriptTotals::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut visited_dirs: HashSet<PathBuf> = HashSet::new();
    let started = std::time::Instant::now();
    'roots: for root in roots {
        if !root.is_dir() {
            totals.files_failed += 1;
            continue;
        }
        let mut stack: Vec<(PathBuf, usize)> = vec![(root.clone(), 0)];
        while let Some((directory, depth)) = stack.pop() {
            if started.elapsed().as_millis() > MAX_SCAN_MS {
                totals.timed_out = true;
                break 'roots;
            }
            if !visited_dirs.insert(directory.clone()) {
                continue;
            }
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(_) => {
                    totals.files_failed += 1;
                    continue;
                }
            };
            for entry in entries {
                let Ok(entry) = entry else {
                    totals.unreadable_entries += 1;
                    continue;
                };
                let path = entry.path();
                let Ok(file_type) = entry.file_type() else {
                    totals.unreadable_entries += 1;
                    continue;
                };
                if file_type.is_dir() {
                    // Descending is what the budget limits, so the refusal is
                    // recorded here: a pruned subtree may hold records, and a
                    // pass that cannot see it cannot claim a total.
                    if depth + 1 > MAX_DEPTH {
                        totals.dirs_pruned += 1;
                    } else {
                        stack.push((path, depth + 1));
                    }
                    continue;
                }
                if !file_type.is_file() {
                    continue;
                }
                if !path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case("jsonl"))
                {
                    continue;
                }
                if totals.files_read >= MAX_FILES {
                    totals.truncated = true;
                    break 'roots;
                }
                let size = match entry.metadata() {
                    Ok(meta) => meta.len(),
                    Err(_) => {
                        totals.unreadable_entries += 1;
                        continue;
                    }
                };
                if size > MAX_FILE_BYTES {
                    totals.files_skipped += 1;
                    continue;
                }
                if totals.bytes_read.saturating_add(size) > MAX_BYTES {
                    totals.truncated = true;
                    break 'roots;
                }
                match read_file(&path, &mut seen, &mut totals) {
                    Ok(()) => {
                        totals.files_read += 1;
                        totals.bytes_read = totals.bytes_read.saturating_add(size);
                    }
                    Err(()) => totals.files_failed += 1,
                }
            }
        }
    }
    totals
}

fn read_file(
    path: &Path,
    seen: &mut HashSet<String>,
    totals: &mut TranscriptTotals,
) -> Result<(), ()> {
    let bytes = std::fs::read(path).map_err(|_| ())?;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        // Cheap pre-filter: a record without a usage object cannot contribute.
        if !contains(line, b"\"usage\"") {
            continue;
        }
        totals.lines_read += 1;
        let value = match serde_json::from_slice::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(_) => {
                // A usage-bearing line that cannot be parsed is an omission, not
                // a line to skip: its numbers are unknown, so the pass cannot
                // claim a total.
                totals.parse_failures += 1;
                continue;
            }
        };
        if value.get("type").and_then(|item| item.as_str()) != Some("assistant") {
            // A `user` record, a summary or a system record is not assistant
            // consumption, even when it carries an object called `usage`.
            continue;
        }
        let Some(usage) = value
            .get("message")
            .and_then(|message| message.get("usage"))
            .filter(|usage| usage.is_object())
        else {
            continue;
        };
        let key = record_key(&value);
        if let Some(key) = key {
            if !seen.insert(key) {
                continue;
            }
            if seen.len() > MAX_RECORDS {
                totals.truncated = true;
                return Ok(());
            }
        }
        let day = value
            .get("timestamp")
            .and_then(|value| value.as_str())
            .and_then(crate::multicli::history::ids::parse_iso_millis)
            .map(crate::multicli::history::ids::iso_from_millis)
            .and_then(|iso| iso.get(0..10).map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        let input = token_field(usage, "input_tokens");
        let cache_creation = token_field(usage, "cache_creation_input_tokens");
        let cache_read = token_field(usage, "cache_read_input_tokens");
        let output = token_field(usage, "output_tokens");
        let missing = [input, cache_creation, cache_read, output]
            .iter()
            .filter(|value| value.is_none())
            .count();
        totals.records += 1;
        if missing == 4 {
            totals.records_without_values += 1;
        }
        let entry = totals.days.entry(day);
        let record = TokenSums {
            input,
            cache_creation,
            cache_read,
            output,
            messages: 1,
            records_missing_fields: if missing > 0 { 1 } else { 0 },
        };
        match entry {
            // The first record of a day is inserted as-is: its `None`s mean
            // "unknown", and merging them into a default would be ambiguous.
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(record);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                slot.get_mut().add(&record);
            }
        }
    }
    Ok(())
}

fn record_key(value: &serde_json::Value) -> Option<String> {
    let uuid = value.get("uuid").and_then(|item| item.as_str())?;
    let session = value
        .get("sessionId")
        .and_then(|item| item.as_str())
        .unwrap_or("");
    Some(format!("{session}\u{1f}{uuid}"))
}

/// One token component. `null`, a string, a float or a negative number is
/// "not reported", never a zero.
fn token_field(value: &serde_json::Value, field: &str) -> Option<u64> {
    value.get(field).and_then(|item| item.as_u64())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "multicli-usage-transcript-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    const ASSISTANT: &str = r#"{"type":"assistant","sessionId":"s-1","uuid":"u-1","timestamp":"2026-11-03T08:00:00.000Z","message":{"role":"assistant","usage":{"input_tokens":100,"cache_creation_input_tokens":10,"cache_read_input_tokens":40,"output_tokens":7}}}"#;

    #[test]
    fn unknown_day_cannot_prove_a_zero_window_but_keeps_lifetime() {
        let root = temp_root("unknown-day-window");
        let mut record: serde_json::Value = serde_json::from_str(ASSISTANT).unwrap();
        record.as_object_mut().unwrap().remove("timestamp");
        fs::write(root.join("a.jsonl"), record.to_string()).unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert!(totals.window("2026-11-03", 1).is_none());
        assert_eq!(totals.lifetime().unwrap().total(), Some(157));
        assert!(!totals.incomplete_reasons().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn only_numeric_usage_is_read_and_cached_tokens_are_not_double_counted() {
        let root = temp_root("basic");
        fs::write(
            root.join("a.jsonl"),
            format!(
                "{ASSISTANT}\n{}\n",
                r#"{"type":"user","sessionId":"s-1","uuid":"u-2","message":{"role":"user","content":"secret text"}}"#
            ),
        )
        .unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert_eq!(totals.files_read, 1);
        assert_eq!(totals.records, 1);
        assert!(totals.complete());
        let day = totals.days.get("2026-11-03").expect("one day");
        assert_eq!(day.input, Some(100));
        assert_eq!(day.cache_read, Some(40));
        assert_eq!(day.cache_creation, Some(10));
        assert_eq!(day.output, Some(7));
        assert_eq!(day.total(), Some(157));
        let samples = totals.samples("2026-11-03", "test");
        let today = samples
            .iter()
            .find(|sample| sample.window == UsageWindow::Daily)
            .unwrap();
        assert_eq!(today.total_tokens, Some(157));
        assert_eq!(today.cached_input_tokens, Some(40));
        assert_eq!(
            today.token_semantics,
            Some(TokenSemantics::InputExcludesCached)
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_resumed_session_that_copies_records_is_counted_once() {
        let root = temp_root("dedupe");
        fs::write(root.join("a.jsonl"), format!("{ASSISTANT}\n")).unwrap();
        fs::write(root.join("b.jsonl"), format!("{ASSISTANT}\n")).unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert_eq!(totals.records, 1, "the same uuid is one record");
        assert_eq!(totals.days["2026-11-03"].total(), Some(157));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_root_is_a_failure_not_an_empty_success() {
        let missing = std::env::temp_dir().join("multicli-usage-definitely-missing");
        let totals = scan_roots(&[missing]);
        assert_eq!(totals.files_failed, 1);
        assert_eq!(totals.files_read, 0);
        assert!(totals.days.is_empty());
        assert!(!totals.complete());
        // An unreadable root cannot claim a window total of zero.
        assert!(totals.window("2026-11-03", 7).is_none());
        assert!(totals.lifetime().is_none());
    }

    #[test]
    fn a_truncated_pass_reports_none_instead_of_a_partial_total() {
        let mut totals = TranscriptTotals {
            truncated: true,
            ..TranscriptTotals::default()
        };
        totals.days.insert(
            "2026-11-03".into(),
            TokenSums {
                input: Some(10),
                ..TokenSums::default()
            },
        );
        assert!(totals.window("2026-11-03", 7).is_none());
        assert!(totals.lifetime().is_none());
        let samples = totals.samples("2026-11-03", "test");
        assert!(samples.iter().all(|sample| sample.total_tokens.is_none()));
    }

    #[test]
    fn the_window_is_bounded_by_the_reported_day() {
        let mut totals = TranscriptTotals::default();
        totals.days.insert(
            "2026-11-03".into(),
            TokenSums {
                input: Some(100),
                ..TokenSums::default()
            },
        );
        totals.days.insert(
            "2026-10-01".into(),
            TokenSums {
                input: Some(900),
                ..TokenSums::default()
            },
        );
        assert_eq!(totals.window("2026-11-03", 1).unwrap().input, Some(100));
        assert_eq!(totals.window("2026-11-03", 7).unwrap().input, Some(100));
        assert_eq!(totals.lifetime().unwrap().input, Some(1000));
    }

    // -----------------------------------------------------------------------
    // R1: completeness and value semantics
    // -----------------------------------------------------------------------

    #[test]
    fn an_oversized_file_makes_the_pass_incomplete_with_a_reason() {
        let root = temp_root("oversized");
        fs::write(root.join("valid.jsonl"), ASSISTANT).unwrap();
        let large = fs::File::create(root.join("large.jsonl")).unwrap();
        large.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(large);
        let totals = scan_roots(&[root.clone()]);
        assert_eq!(totals.files_skipped, 1);
        assert!(!totals.complete(), "an unread usage file cannot prove a total");
        assert!(totals.window("2026-11-03", 7).is_none());
        assert!(totals
            .incomplete_reasons()
            .iter()
            .any(|reason| reason.contains("单文件上限")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_pruned_directory_makes_the_pass_incomplete_with_a_reason() {
        let root = temp_root("depth");
        fs::write(root.join("valid.jsonl"), ASSISTANT).unwrap();
        let mut deep = root.clone();
        for _ in 0..MAX_DEPTH + 2 {
            deep = deep.join("nested");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("hidden.jsonl"), ASSISTANT).unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert!(totals.dirs_pruned > 0);
        assert!(!totals.complete(), "a depth-pruned tree cannot prove a total");
        assert!(totals
            .incomplete_reasons()
            .iter()
            .any(|reason| reason.contains("深度上限")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_truncated_usage_line_makes_the_pass_incomplete() {
        let root = temp_root("broken-line");
        fs::write(
            root.join("broken.jsonl"),
            format!("{ASSISTANT}\n{{\"message\":{{\"usage\":"),
        )
        .unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert_eq!(totals.parse_failures, 1);
        assert!(!totals.complete());
        assert!(totals
            .incomplete_reasons()
            .iter()
            .any(|reason| reason.contains("无法解析")));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_usage_object_is_unknown_not_four_measured_zeros() {
        let root = temp_root("empty-usage");
        fs::write(
            root.join("empty.jsonl"),
            r#"{"type":"assistant","uuid":"empty","sessionId":"s","timestamp":"2026-11-03T00:00:00Z","message":{"role":"assistant","usage":{}}}"#,
        )
        .unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert!(totals.complete(), "the scan itself read everything");
        assert_eq!(totals.records, 1);
        assert_eq!(totals.records_without_values, 1);
        let samples = totals.samples("2026-11-03", "test");
        assert!(
            samples.iter().all(|sample| sample.total_tokens.is_none()),
            "an empty usage object is not a measured zero"
        );
        assert!(samples.iter().all(|sample| sample.input_tokens.is_none()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_user_record_is_never_counted_as_assistant_usage() {
        let root = temp_root("nonassistant");
        fs::write(root.join("user.jsonl"), ASSISTANT.replace("assistant", "user")).unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert_eq!(totals.records, 0, "only assistant usage is a provider sample");
        assert!(totals.complete());
        // A complete scan that found no assistant record is a real zero, but it
        // must not be built from the user record's numbers.
        assert_eq!(totals.lifetime().unwrap().total(), Some(0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_component_makes_the_day_total_unknown_but_keeps_known_parts() {
        let root = temp_root("partial-fields");
        fs::write(
            root.join("a.jsonl"),
            r#"{"type":"assistant","uuid":"u-1","sessionId":"s","timestamp":"2026-11-03T08:00:00Z","message":{"role":"assistant","usage":{"input_tokens":10,"cache_read_input_tokens":3,"cache_creation_input_tokens":1}}}"#,
        )
        .unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert!(totals.complete());
        let day = totals.days.get("2026-11-03").unwrap();
        assert_eq!(day.input, Some(10), "a reported component stays known");
        assert_eq!(day.output, None, "a missing component is not zero");
        assert_eq!(day.total(), None, "no total from a partial set");
        let sample = totals
            .samples("2026-11-03", "test")
            .into_iter()
            .find(|sample| sample.window == UsageWindow::Daily)
            .unwrap();
        assert_eq!(sample.total_tokens, None);
        assert_eq!(sample.input_tokens, Some(10));
        assert_eq!(sample.output_tokens, None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_real_reported_zero_is_kept_as_zero() {
        let root = temp_root("real-zero");
        fs::write(
            root.join("a.jsonl"),
            r#"{"type":"assistant","uuid":"u-1","sessionId":"s","timestamp":"2026-11-03T08:00:00Z","message":{"role":"assistant","usage":{"input_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":0}}}"#,
        )
        .unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert!(totals.complete());
        assert_eq!(totals.days["2026-11-03"].total(), Some(0));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_time_budget_stop_is_incomplete_and_explained() {
        let mut totals = TranscriptTotals {
            timed_out: true,
            ..TranscriptTotals::default()
        };
        totals.days.insert(
            "2026-11-03".into(),
            TokenSums {
                input: Some(10),
                cache_creation: Some(1),
                cache_read: Some(1),
                output: Some(1),
                messages: 1,
                records_missing_fields: 0,
            },
        );
        assert!(!totals.complete());
        assert!(totals.window("2026-11-03", 7).is_none());
        assert!(totals
            .incomplete_reasons()
            .iter()
            .any(|reason| reason.contains("时间预算")));
    }

    #[test]
    fn an_empty_window_in_a_complete_scan_is_a_real_zero() {
        let root = temp_root("empty-window");
        fs::write(
            root.join("a.jsonl"),
            r#"{"type":"assistant","uuid":"u-1","sessionId":"s","timestamp":"2026-10-01T08:00:00Z","message":{"role":"assistant","usage":{"input_tokens":10,"cache_read_input_tokens":1,"cache_creation_input_tokens":1,"output_tokens":2}}}"#,
        )
        .unwrap();
        let totals = scan_roots(&[root.clone()]);
        assert!(totals.complete());
        let today = totals.window("2026-11-03", 1).unwrap();
        assert_eq!(today.total(), Some(0), "nothing happened today — a real 0");
        assert_eq!(totals.window("2026-11-03", 7).unwrap().total(), Some(0));
        assert_eq!(totals.lifetime().unwrap().total(), Some(14));
        let _ = fs::remove_dir_all(&root);
    }
}
