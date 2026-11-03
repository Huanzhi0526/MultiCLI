//! Event envelope production for the unified runtime stream.
//!
//! One `EventJournal` per Run. It owns the frozen `EventEnvelope`
//! (`docs/contracts/events.schema.json`) and three guarantees the S05 card
//! asks for:
//!
//! - **seq is per `(runId, streamId)`** and strictly monotonic; arrival order
//!   across two products is never used as original order;
//! - **duplicates are idempotent**: a re-delivered `nativeEventId` (or a
//!   synthesised `sourceKey`) produces no second envelope, so the UI cannot
//!   print the same delta twice;
//! - **unknown events are kept, not executed**: an unrecognised `kind` is
//!   stored with its payload so a report can explain what arrived, but nothing
//!   downstream interprets it.

use std::collections::{HashSet, VecDeque};

use serde_json::{json, Value};

use crate::multicli::domain::{EventEnvelope, EVENT_SCHEMA_VERSION};
use crate::multicli::error::AppError;
use crate::multicli::history::ids::iso_from_millis;

use super::model::{JOURNAL_MAX_BYTES, JOURNAL_MAX_EVENTS};

/// Event kinds this runtime produces. A payload key that is not listed in
/// `docs/contracts/runtime-v1.md` is still stored; these constants exist so
/// producers cannot drift into spelling variants.
pub mod kind {
    pub const RUN_STATE: &str = "run/state";
    pub const TURN_STARTED: &str = "turn/started";
    pub const TURN_COMPLETED: &str = "turn/completed";
    pub const MESSAGE_DELTA: &str = "message/delta";
    pub const MESSAGE_COMPLETED: &str = "message/completed";
    pub const REASONING_DELTA: &str = "reasoning/delta";
    pub const TOOL_CALL: &str = "tool/call";
    pub const TOOL_UPDATE: &str = "tool/update";
    pub const PERMISSION_REQUESTED: &str = "permission/requested";
    pub const PERMISSION_RESOLVED: &str = "permission/resolved";
    pub const WARNING: &str = "warning";
    pub const UNKNOWN: &str = "protocol/unknown";
}

pub fn now_iso() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_millis() as i64)
        .unwrap_or(0);
    iso_from_millis(millis)
}

pub struct EventJournal {
    run_id: String,
    session_id: String,
    stream_id: String,
    seq: u64,
    events: VecDeque<EventEnvelope>,
    bytes: usize,
    /// The largest seq that was evicted from the front. A client whose cursor
    /// is below this must resync from the run view.
    dropped_before: u64,
    truncated: bool,
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    /// Kept for the report: unknown kinds are listed once with their count.
    unknown_kinds: Vec<(String, u64)>,
}

/// Upper bound on the dedupe window. It only has to cover a protocol's own
/// retry window; anything older than the journal itself could not be replayed
/// anyway.
const SEEN_LIMIT: usize = JOURNAL_MAX_EVENTS + 1024;

impl EventJournal {
    pub fn new(run_id: impl Into<String>, session_id: impl Into<String>) -> Self {
        let run_id = run_id.into();
        Self {
            stream_id: format!("runtime/{run_id}"),
            run_id,
            session_id: session_id.into(),
            seq: 0,
            events: VecDeque::new(),
            bytes: 0,
            dropped_before: 0,
            truncated: false,
            seen: HashSet::new(),
            seen_order: VecDeque::new(),
            unknown_kinds: Vec::new(),
        }
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    pub fn dropped_before(&self) -> u64 {
        self.dropped_before
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn unknown_kinds(&self) -> &[(String, u64)] {
        &self.unknown_kinds
    }

    /// True when this event was already recorded.
    ///
    /// Dedupe used to happen only inside `push`, i.e. *after* the caller had
    /// already mutated the run state. A replayed `turn/started` therefore
    /// reopened a completed turn while the journal silently dropped the event.
    /// Applying a state change is now gated on this check first.
    pub fn is_duplicate(&self, native_event_id: Option<&str>, source_key: &str) -> bool {
        match dedupe_key_of(native_event_id, source_key) {
            Some(key) => self.seen.contains(&key),
            None => false,
        }
    }

    /// Appends one event. Returns `None` when the same native id (or source
    /// key) was already recorded: a duplicate is absorbed, never re-numbered.
    pub fn push(
        &mut self,
        kind: &str,
        native_event_id: Option<&str>,
        source_key: &str,
        payload: Value,
    ) -> Option<EventEnvelope> {
        let dedupe_key = match dedupe_key_of(native_event_id, source_key) {
            Some(key) => key,
            // An event with neither a native id nor a caller-supplied key cannot
            // be deduped; `accept` synthesises one before reaching here.
            None => return self.push_unkeyed(kind, payload),
        };
        if !self.seen.insert(dedupe_key.clone()) {
            return None;
        }
        self.seen_order.push_back(dedupe_key.clone());
        while self.seen_order.len() > SEEN_LIMIT {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
            }
        }

        self.seq += 1;
        // Sanitise instead of rejecting: every producer here is internal, and a
        // numbering gap would be worse than a synthesised key. `validate` above
        // still has to hold for the envelope to be publishable.
        let source_key = if source_key.trim().is_empty() {
            format!("{}#{}", kind, self.seq)
        } else {
            source_key.to_string()
        };
        let kind = if kind.trim().is_empty() {
            kind::WARNING.to_string()
        } else {
            kind.to_string()
        };
        let envelope = EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            session_id: Some(self.session_id.clone()),
            run_id: Some(self.run_id.clone()),
            stream_id: self.stream_id.clone(),
            seq: self.seq,
            timestamp: now_iso(),
            kind,
            source: "multicli-runtime".into(),
            native_event_id: native_event_id
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string),
            source_key,
            payload: if payload.is_object() {
                payload
            } else {
                json!({ "value": payload })
            },
        };
        if let Err(error) = envelope.validate() {
            // Unreachable with the sanitising above; if it ever happens the
            // stream must not carry a malformed frame, so it is dropped and
            // counted as a warning instead.
            self.seq -= 1;
            self.seen.remove(&dedupe_key);
            self.seen_order.pop_back();
            self.push(
                kind::WARNING,
                None,
                &format!("invalidEnvelope#{}", self.seq + 1),
                json!({ "code": "invalidEventRejected", "message": error.message }),
            );
            return None;
        }
        self.bytes += approximate_bytes(&envelope);
        self.events.push_back(envelope.clone());
        self.trim();
        Some(envelope)
    }

    /// Records an event whose kind this build does not understand. The payload
    /// is preserved verbatim and the kind is counted so the delivery report can
    /// name it instead of pretending nothing arrived.
    pub fn push_unknown(
        &mut self,
        method: &str,
        native_event_id: Option<&str>,
        source_key: &str,
        sample: Value,
    ) -> Option<EventEnvelope> {
        if let Some(entry) = self
            .unknown_kinds
            .iter_mut()
            .find(|(name, _)| name == method)
        {
            entry.1 += 1;
        } else {
            self.unknown_kinds.push((method.to_string(), 1));
        }
        self.push(
            kind::UNKNOWN,
            native_event_id,
            source_key,
            json!({ "method": method, "sample": sample, "executed": false }),
        )
    }

    /// Appends without participating in dedupe. Only used when the caller
    /// supplied no identity at all; the run always synthesises one first.
    fn push_unkeyed(&mut self, kind: &str, payload: Value) -> Option<EventEnvelope> {
        self.seq += 1;
        let envelope = EventEnvelope {
            schema_version: EVENT_SCHEMA_VERSION,
            session_id: Some(self.session_id.clone()),
            run_id: Some(self.run_id.clone()),
            stream_id: self.stream_id.clone(),
            seq: self.seq,
            timestamp: now_iso(),
            kind: kind.to_string(),
            source: "multicli-runtime".into(),
            native_event_id: None,
            source_key: format!("{kind}#{}", self.seq),
            payload: if payload.is_object() {
                payload
            } else {
                json!({ "value": payload })
            },
        };
        self.bytes += approximate_bytes(&envelope);
        self.events.push_back(envelope.clone());
        self.trim();
        Some(envelope)
    }

    fn trim(&mut self) {
        while self.events.len() > JOURNAL_MAX_EVENTS || self.bytes > JOURNAL_MAX_BYTES {
            let Some(front) = self.events.front() else {
                break;
            };
            // Never evict the newest event, even if it alone exceeds the budget:
            // a single oversized payload must still be readable once.
            if self.events.len() == 1 {
                break;
            }
            let size = approximate_bytes(front);
            self.dropped_before = front.seq;
            self.truncated = true;
            self.events.pop_front();
            self.bytes = self.bytes.saturating_sub(size);
        }
    }

    /// Reads events strictly after `cursor`. `None` means "everything still
    /// retained", which is what a fresh subscriber asks for.
    pub fn read(&self, cursor: Option<u64>, limit: usize) -> (Vec<EventEnvelope>, u64) {
        let start = cursor.unwrap_or(0);
        let items: Vec<EventEnvelope> = self
            .events
            .iter()
            .filter(|event| event.seq > start)
            .take(limit)
            .cloned()
            .collect();
        let next = items.last().map(|event| event.seq).unwrap_or(start);
        (items, next)
    }

    /// Drops in-memory events but keeps the dedupe window and the seq counter,
    /// so a long-idle run stops holding megabytes while its numbering stays
    /// continuous.
    pub fn compact(&mut self) {
        self.events.clear();
        self.bytes = 0;
        if self.seq > 0 {
            self.dropped_before = self.seq;
            self.truncated = true;
        }
    }
}

/// The dedupe identity of an event: the protocol's own id when there is one,
/// otherwise the caller's stable key. `None` means "no identity", which the run
/// never produces because it synthesises a key first.
fn dedupe_key_of(native_event_id: Option<&str>, source_key: &str) -> Option<String> {
    match native_event_id.filter(|value| !value.trim().is_empty()) {
        Some(id) => Some(format!("n:{id}")),
        None if !source_key.trim().is_empty() => Some(format!("s:{source_key}")),
        None => None,
    }
}

fn approximate_bytes(envelope: &EventEnvelope) -> usize {
    // Cheap and stable: the serialised size is what the IPC frame costs, and a
    // second full serialise per event is not worth the accuracy.
    serde_json::to_string(envelope)
        .map(|text| text.len())
        .unwrap_or(256)
}

/// Validates an envelope that came from a producer outside this module (an
/// adapter building its own event). A rejected envelope becomes an `AppError`
/// rather than being silently dropped.
pub fn validate_envelope(envelope: &EventEnvelope) -> Result<(), AppError> {
    envelope.validate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_is_monotonic_and_duplicates_are_absorbed() {
        let mut journal = EventJournal::new("run-1", "session-1");
        let first = journal
            .push(kind::MESSAGE_DELTA, Some("n-1"), "k-1", json!({"text": "a"}))
            .unwrap();
        assert_eq!(first.seq, 1);
        assert_eq!(first.stream_id, "runtime/run-1");

        // Same native id, different source key: still one event.
        assert!(journal
            .push(kind::MESSAGE_DELTA, Some("n-1"), "k-2", json!({"text": "a"}))
            .is_none());
        // Same source key, no native id: absorbed too.
        let second = journal
            .push(kind::MESSAGE_DELTA, None, "k-2", json!({"text": "b"}))
            .unwrap();
        assert_eq!(second.seq, 2);
        assert!(journal
            .push(kind::MESSAGE_DELTA, None, "k-2", json!({"text": "b"}))
            .is_none());

        assert_eq!(journal.last_seq(), 2);
        let (items, next) = journal.read(None, 10);
        assert_eq!(items.len(), 2);
        assert_eq!(next, 2);
        let (items, next) = journal.read(Some(1), 10);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].seq, 2);
        assert_eq!(next, 2);
    }

    #[test]
    fn duplicate_probe_sees_a_key_before_anything_is_applied() {
        let mut journal = EventJournal::new("run-1", "session-1");
        assert!(!journal.is_duplicate(Some("n-1"), "k-1"));
        journal
            .push(kind::TURN_STARTED, Some("n-1"), "k-1", json!({}))
            .unwrap();
        assert!(journal.is_duplicate(Some("n-1"), "k-1"));
        // An event that carried a native id is identified by that id; a later
        // frame with only a source key is a different identity, not the same
        // event, so it is not reported as a duplicate.
        assert!(!journal.is_duplicate(None, "k-1"));
        assert!(!journal.is_duplicate(None, "k-2"));
        journal
            .push(kind::TURN_STARTED, None, "k-1", json!({}))
            .unwrap();
        assert!(journal.is_duplicate(None, "k-1"));
        // No identity at all is never treated as a duplicate: it cannot be
        // compared, and the run synthesises a key before this point.
        assert!(!journal.is_duplicate(None, ""));
    }

    #[test]
    fn unknown_events_are_preserved_and_counted() {
        let mut journal = EventJournal::new("run-1", "session-1");
        let event = journal
            .push_unknown("acme/thing", Some("x-1"), "k-1", json!({"a": 1}))
            .unwrap();
        assert_eq!(event.kind, kind::UNKNOWN);
        assert_eq!(event.payload["executed"], false);
        assert_eq!(event.payload["method"], "acme/thing");
        journal.push_unknown("acme/thing", Some("x-2"), "k-2", json!({}));
        assert_eq!(journal.unknown_kinds(), [("acme/thing".to_string(), 2)]);
    }

    #[test]
    fn journal_is_bounded_and_reports_a_dropped_frontier() {
        let mut journal = EventJournal::new("run-1", "session-1");
        for index in 0..(JOURNAL_MAX_EVENTS + 50) {
            journal
                .push(
                    kind::MESSAGE_DELTA,
                    Some(&format!("n-{index}")),
                    &format!("k-{index}"),
                    json!({ "text": "x" }),
                )
                .unwrap();
        }
        let items_len = journal.events.len();
        assert!(items_len <= JOURNAL_MAX_EVENTS);
        assert!(journal.is_truncated());
        assert!(journal.dropped_before() > 0);
        assert_eq!(journal.last_seq(), (JOURNAL_MAX_EVENTS + 50) as u64);
        let (items, _) = journal.read(Some(0), JOURNAL_MAX_EVENTS);
        assert_eq!(items.len(), items_len);
        assert_eq!(items[0].seq, journal.dropped_before() + 1);
    }

    #[test]
    fn compact_keeps_numbering_continuous() {
        let mut journal = EventJournal::new("run-1", "session-1");
        journal
            .push(kind::MESSAGE_DELTA, Some("n-1"), "k-1", json!({}))
            .unwrap();
        journal.compact();
        assert_eq!(journal.last_seq(), 1);
        assert_eq!(journal.dropped_before(), 1);
        assert!(journal.read(None, 10).0.is_empty());
        let next = journal
            .push(kind::MESSAGE_DELTA, Some("n-2"), "k-2", json!({}))
            .unwrap();
        assert_eq!(next.seq, 2);
    }

    #[test]
    fn non_object_payloads_are_wrapped_not_rejected() {
        let mut journal = EventJournal::new("run-1", "session-1");
        let event = journal.push(kind::WARNING, None, "k", json!("plain")).unwrap();
        assert!(event.payload.is_object());
    }
}
