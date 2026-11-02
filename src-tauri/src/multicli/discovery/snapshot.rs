//! Process identity state machine.
//!
//! Identity is `hostId + pid + OS creationTime`; nothing else. Consequences
//! that this module enforces:
//! - A successful snapshot merges idempotently by identity, so scanning twice
//!   never duplicates a row.
//! - PID reuse produces a *new* identity. The previous row is closed as
//!   `exited` (reason `pid-reused`) and never inherits the new process's
//!   associations.
//! - A process whose creation time cannot be read is `unknown`, never a fake
//!   stable identity and never `exited`. Access denied is not an exit.
//! - A total enumeration failure keeps the previous rows, marks them `stale`
//!   and does **not** turn them into exits.
//! - Nothing here infers `awaiting-input` from low CPU or missing output.

use super::signature::EntryKind;
use super::wrapper::NodeInterpreter;
use crate::multicli::domain::{EvidenceLevel, ProcessIdentity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessState {
    Alive,
    Exited,
    Unknown,
}

impl ProcessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alive => "alive",
            Self::Exited => "exited",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Freshness {
    Fresh,
    Stale,
}

impl Freshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecognitionStrength {
    Confirmed,
    Likely,
}

impl RecognitionStrength {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Likely => "likely",
        }
    }

    pub fn evidence_level(self) -> EvidenceLevel {
        match self {
            Self::Confirmed => EvidenceLevel::Observed,
            Self::Likely => EvidenceLevel::Inferred,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognition {
    pub agent_id: String,
    pub product: String,
    pub installation_id: Option<String>,
    pub entry_kind: EntryKind,
    pub strength: RecognitionStrength,
    pub evidence: Vec<String>,
    /// Explicit directory argument from argv, kept in memory only. The engine
    /// resolves it and stores the resulting workspace id, never the path.
    pub cwd_argument: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedProcess {
    pub identity: ProcessIdentity,
    pub executable_display: Option<String>,
    pub executable_canonical: Option<String>,
    pub redacted_command: Option<String>,
    pub recognition: Option<Recognition>,
    pub parent_identity: Option<ProcessIdentity>,
    pub parent_note: Option<String>,
    pub source_program: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_evidence: Option<String>,
    pub interpreter: Option<NodeInterpreter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedProcess {
    pub identity: ProcessIdentity,
    pub agent_id: Option<String>,
    pub installation_id: Option<String>,
    pub product: Option<String>,
    pub entry_kind: Option<EntryKind>,
    pub recognition: Option<RecognitionStrength>,
    pub evidence: Vec<String>,
    pub executable_display: Option<String>,
    pub executable_canonical: Option<String>,
    pub redacted_command: Option<String>,
    pub parent_identity: Option<ProcessIdentity>,
    pub parent_note: Option<String>,
    pub source_program: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_evidence: Option<String>,
    pub state: ProcessState,
    pub freshness: Freshness,
    pub state_reason: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub last_seen_generation: u64,
}

impl TrackedProcess {
    pub fn identity_key(&self) -> String {
        identity_key(&self.identity)
    }

    pub fn is_recognised(&self) -> bool {
        self.agent_id.is_some()
    }
}

pub fn identity_key(identity: &ProcessIdentity) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        identity.host_id, identity.pid, identity.creation_time
    )
}

/// Which PIDs existed in the raw enumeration and which could not be inspected.
#[derive(Debug, Clone, Default)]
pub struct ScanEvidence {
    /// Every PID present in the successful enumeration.
    pub present_pids: BTreeSet<u32>,
    /// PID → path-free reason code for a failed inspect.
    pub unreadable_pids: BTreeMap<u32, String>,
    /// PIDs whose creation time was read and no longer matches the tracked
    /// identity: a verified PID reuse.
    pub reused_pids: BTreeSet<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeReport {
    pub added: usize,
    pub updated: usize,
    pub exited: usize,
    pub unknown: usize,
    pub dropped: usize,
}

/// Merges one successful snapshot into the tracked map.
pub fn merge_snapshot(
    tracked: &mut BTreeMap<String, TrackedProcess>,
    observed: Vec<ObservedProcess>,
    evidence: &ScanEvidence,
    generation: u64,
    observed_at: &str,
    retention_generations: u64,
) -> MergeReport {
    let mut report = MergeReport::default();
    let mut observed_keys = BTreeSet::new();

    for item in observed {
        let key = identity_key(&item.identity);
        observed_keys.insert(key.clone());
        match tracked.get_mut(&key) {
            Some(existing) => {
                apply_observation(existing, item, generation, observed_at);
                report.updated += 1;
            }
            None => {
                let mut entry = TrackedProcess {
                    identity: item.identity.clone(),
                    agent_id: None,
                    installation_id: None,
                    product: None,
                    entry_kind: None,
                    recognition: None,
                    evidence: Vec::new(),
                    executable_display: None,
                    executable_canonical: None,
                    redacted_command: None,
                    parent_identity: None,
                    parent_note: None,
                    source_program: None,
                    workspace_id: None,
                    workspace_evidence: None,
                    state: ProcessState::Alive,
                    freshness: Freshness::Fresh,
                    state_reason: None,
                    first_seen_at: observed_at.to_string(),
                    last_seen_at: observed_at.to_string(),
                    last_seen_generation: generation,
                };
                apply_observation(&mut entry, item, generation, observed_at);
                tracked.insert(key, entry);
                report.added += 1;
            }
        }
    }

    // Anything not observed in this generation is classified carefully.
    for (key, entry) in tracked.iter_mut() {
        if observed_keys.contains(key) {
            continue;
        }
        let pid = entry.identity.pid;
        if evidence.unreadable_pids.contains_key(&pid) {
            entry.state = ProcessState::Unknown;
            entry.freshness = Freshness::Stale;
            entry.state_reason = evidence.unreadable_pids.get(&pid).cloned();
            report.unknown += 1;
        } else if evidence.reused_pids.contains(&pid) {
            entry.state = ProcessState::Exited;
            entry.freshness = Freshness::Stale;
            entry.state_reason = Some("pid-reused".to_string());
            report.exited += 1;
        } else if !evidence.present_pids.contains(&pid) {
            entry.state = ProcessState::Exited;
            entry.freshness = Freshness::Stale;
            entry.state_reason = Some("process-ended".to_string());
            report.exited += 1;
        } else {
            // The PID exists and its creation time still matches, but the
            // process did not qualify as an instance this round (for example
            // its command line became unreadable). That is not an exit.
            entry.state = ProcessState::Unknown;
            entry.freshness = Freshness::Stale;
            entry.state_reason = Some("not-observed".to_string());
            report.unknown += 1;
        }
    }

    // Exit rows converge out of the view after a bounded retention window.
    let before = tracked.len();
    tracked.retain(|_, entry| {
        !(entry.state == ProcessState::Exited
            && generation.saturating_sub(entry.last_seen_generation) > retention_generations)
    });
    report.dropped = before.saturating_sub(tracked.len());
    report
}

fn apply_observation(
    entry: &mut TrackedProcess,
    item: ObservedProcess,
    generation: u64,
    observed_at: &str,
) {
    entry.executable_display = item.executable_display;
    entry.executable_canonical = item.executable_canonical;
    entry.redacted_command = item.redacted_command;
    entry.parent_identity = item.parent_identity;
    entry.parent_note = item.parent_note;
    entry.source_program = item.source_program;
    entry.workspace_id = item.workspace_id;
    entry.workspace_evidence = item.workspace_evidence;
    entry.last_seen_at = observed_at.to_string();
    entry.last_seen_generation = generation;
    match item.recognition {
        Some(recognition) => {
            entry.agent_id = Some(recognition.agent_id);
            entry.product = Some(recognition.product);
            entry.installation_id = recognition.installation_id;
            entry.entry_kind = Some(recognition.entry_kind);
            entry.recognition = Some(recognition.strength);
            entry.evidence = recognition.evidence;
            entry.state = ProcessState::Alive;
            entry.freshness = Freshness::Fresh;
            entry.state_reason = None;
        }
        None => {
            // A previously recognised row keeps its last known product but is
            // no longer fresh: losing the command line is a degradation, not
            // an exit and not a silent re-confirmation.
            if entry.state != ProcessState::Alive {
                entry.state = ProcessState::Alive;
            }
            entry.freshness = Freshness::Stale;
            entry.state_reason = Some("recognition-degraded".to_string());
        }
    }
}

/// Total enumeration failure: keep everything, mark it stale, change no state.
pub fn mark_total_failure(tracked: &mut BTreeMap<String, TrackedProcess>, reason: &str) -> usize {
    let mut affected = 0;
    for entry in tracked.values_mut() {
        entry.freshness = Freshness::Stale;
        entry.state_reason = Some(reason.to_string());
        affected += 1;
    }
    affected
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::domain::ProcessIdentity;

    fn identity(pid: u32, created: &str) -> ProcessIdentity {
        ProcessIdentity {
            host_id: "local-windows".to_string(),
            pid,
            creation_time: created.to_string(),
        }
    }

    fn observed(pid: u32, created: &str) -> ObservedProcess {
        ObservedProcess {
            identity: identity(pid, created),
            executable_display: Some(format!(r"C:\bin\{pid}.exe")),
            executable_canonical: None,
            redacted_command: Some("codex.exe <1 withheld>".to_string()),
            recognition: Some(Recognition {
                agent_id: "codex".to_string(),
                product: "Codex CLI".to_string(),
                installation_id: Some("install-1".to_string()),
                entry_kind: EntryKind::NativeExe,
                strength: RecognitionStrength::Confirmed,
                evidence: vec!["installation-exe".to_string()],
                cwd_argument: None,
            }),
            parent_identity: None,
            parent_note: None,
            source_program: None,
            workspace_id: None,
            workspace_evidence: None,
            interpreter: None,
        }
    }

    fn evidence(pids: &[u32]) -> ScanEvidence {
        ScanEvidence {
            present_pids: pids.iter().copied().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn repeated_snapshots_are_idempotent_and_exit_converges() {
        let mut tracked = BTreeMap::new();
        let first = merge_snapshot(
            &mut tracked,
            vec![observed(10, "T1")],
            &evidence(&[10]),
            1,
            "A",
            3,
        );
        assert_eq!(first.added, 1);
        let second = merge_snapshot(
            &mut tracked,
            vec![observed(10, "T1")],
            &evidence(&[10]),
            2,
            "B",
            3,
        );
        assert_eq!(second.updated, 1);
        assert_eq!(tracked.len(), 1);

        let exit = merge_snapshot(&mut tracked, vec![], &evidence(&[]), 3, "C", 3);
        assert_eq!(exit.exited, 1);
        let entry = tracked.values().next().unwrap();
        assert_eq!(entry.state, ProcessState::Exited);
        assert_eq!(entry.state_reason.as_deref(), Some("process-ended"));
        // Still visible during retention.
        assert_eq!(tracked.len(), 1);
        let dropped = merge_snapshot(&mut tracked, vec![], &evidence(&[]), 8, "D", 3);
        assert_eq!(dropped.dropped, 1);
        assert!(tracked.is_empty());
    }

    #[test]
    fn pid_reuse_never_inherits_old_associations() {
        let mut tracked = BTreeMap::new();
        merge_snapshot(
            &mut tracked,
            vec![observed(42, "T1")],
            &evidence(&[42]),
            1,
            "A",
            30,
        );
        let mut reused = ScanEvidence {
            present_pids: [42].into_iter().collect(),
            ..Default::default()
        };
        reused.reused_pids.insert(42);
        let report = merge_snapshot(&mut tracked, vec![observed(42, "T2")], &reused, 2, "B", 30);
        assert_eq!(report.added, 1);
        assert_eq!(report.exited, 1);
        let old = tracked
            .values()
            .find(|entry| entry.identity.creation_time == "T1")
            .unwrap();
        assert_eq!(old.state, ProcessState::Exited);
        let new = tracked
            .values()
            .find(|entry| entry.identity.creation_time == "T2")
            .unwrap();
        assert_eq!(new.state, ProcessState::Alive);
        assert_eq!(new.first_seen_at, "B");
    }

    #[test]
    fn denied_inspection_is_unknown_not_exit() {
        let mut tracked = BTreeMap::new();
        merge_snapshot(
            &mut tracked,
            vec![observed(7, "T1")],
            &evidence(&[7]),
            1,
            "A",
            30,
        );
        let mut denied = evidence(&[7]);
        denied
            .unreadable_pids
            .insert(7, "access-denied".to_string());
        let report = merge_snapshot(&mut tracked, vec![], &denied, 2, "B", 30);
        assert_eq!(report.unknown, 1);
        assert_eq!(report.exited, 0);
        let entry = tracked.values().next().unwrap();
        assert_eq!(entry.state, ProcessState::Unknown);
        assert_eq!(entry.freshness, Freshness::Stale);
        assert_eq!(entry.state_reason.as_deref(), Some("access-denied"));
    }

    #[test]
    fn total_failure_keeps_state_and_marks_stale() {
        let mut tracked = BTreeMap::new();
        merge_snapshot(
            &mut tracked,
            vec![observed(9, "T1")],
            &evidence(&[9]),
            1,
            "A",
            30,
        );
        let affected = mark_total_failure(&mut tracked, "enumeration-failed");
        assert_eq!(affected, 1);
        let entry = tracked.values().next().unwrap();
        assert_eq!(entry.state, ProcessState::Alive);
        assert_eq!(entry.freshness, Freshness::Stale);
        // The next successful snapshot recovers freshness.
        merge_snapshot(
            &mut tracked,
            vec![observed(9, "T1")],
            &evidence(&[9]),
            2,
            "B",
            30,
        );
        let entry = tracked.values().next().unwrap();
        assert_eq!(entry.freshness, Freshness::Fresh);
        assert_eq!(entry.state_reason, None);
    }

    #[test]
    fn present_but_unmatched_process_is_not_an_exit() {
        let mut tracked = BTreeMap::new();
        merge_snapshot(
            &mut tracked,
            vec![observed(11, "T1")],
            &evidence(&[11]),
            1,
            "A",
            30,
        );
        let report = merge_snapshot(&mut tracked, vec![], &evidence(&[11]), 2, "B", 30);
        assert_eq!(report.exited, 0);
        assert_eq!(report.unknown, 1);
        let entry = tracked.values().next().unwrap();
        assert_eq!(entry.state, ProcessState::Unknown);
        assert_eq!(entry.freshness, Freshness::Stale);
        assert_eq!(entry.state_reason.as_deref(), Some("not-observed"));
    }
}
