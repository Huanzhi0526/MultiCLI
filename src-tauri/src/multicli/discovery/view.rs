//! Discovery IPC view models (`docs/contracts/discovery-v1.md`).
//!
//! These types are the wire shape for the three discovery commands (plus the
//! bounded settings command). They are deliberately separate from the frozen
//! S01 domain DTOs: `Run.transport` has no "unconnected observation" value, so
//! an external CLI is represented here with `sessionId = null` instead of a
//! fabricated native/acp/pty run.

use crate::multicli::domain::{HostPath, ProcessIdentity, MAX_PAGE_LIMIT};
use crate::multicli::error::AppError;
use super::probe::ProbeOutcome;
use super::snapshot::ProcessState;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const DISCOVERY_SCHEMA_VERSION: u32 = 1;
pub const MIN_SCAN_INTERVAL_MS: u64 = 1_000;
pub const MAX_SCAN_INTERVAL_MS: u64 = 60_000;
pub const DEFAULT_SCAN_INTERVAL_MS: u64 = 3_000;

/// Short, fixed note shown next to every process list.
pub const SAMPLING_NOTE: &str =
    "Sampled periodically; processes shorter than the scan interval can be missed. Not an audit log.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeView {
    pub status: String,
    pub reason: Option<String>,
    pub checked_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub arguments: Vec<String>,
}

impl ProbeView {
    pub fn from_outcome(outcome: Option<&ProbeOutcome>) -> Self {
        match outcome {
            Some(outcome) => Self {
                status: outcome.status.as_str().to_string(),
                reason: outcome.reason.clone(),
                checked_at: Some(outcome.checked_at.clone()),
                duration_ms: Some(outcome.duration_ms),
                arguments: outcome.arguments.clone(),
            },
            None => Self {
                status: "not-attempted".to_string(),
                reason: Some("no probe has run for this entry".to_string()),
                checked_at: None,
                duration_ms: None,
                arguments: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationDiscoveryView {
    pub installation_id: Option<String>,
    pub agent_id: String,
    pub product: String,
    pub signature_id: String,
    pub status: String,
    pub entry_kind: String,
    pub executable: Option<HostPath>,
    pub runtime: Option<HostPath>,
    pub entry_script: Option<HostPath>,
    pub version: Option<String>,
    pub version_source: String,
    pub probe: ProbeView,
    pub config_profile_id: String,
    pub config_profile_status: String,
    pub issue_code: Option<String>,
    pub issue_reason: Option<String>,
    pub discovered_from: String,
    pub discovered_at: String,
    pub last_seen_scan: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessDiscoveryView {
    /// Null only for a row whose identity could never be established.
    pub identity: Option<ProcessIdentity>,
    pub pid: Option<u32>,
    pub agent_id: Option<String>,
    pub product: Option<String>,
    pub installation_id: Option<String>,
    pub entry_kind: Option<String>,
    pub recognition: Option<String>,
    pub evidence: Vec<String>,
    pub state: String,
    pub freshness: String,
    pub state_reason: Option<String>,
    pub executable: Option<HostPath>,
    pub redacted_command: Option<String>,
    pub parent_identity: Option<ProcessIdentity>,
    pub parent_note: Option<String>,
    pub source_program: Option<String>,
    pub workspace_id: Option<String>,
    pub workspace_evidence: Option<String>,
    /// Always null in S02: an observation view never claims a chat session.
    pub session_id: Option<String>,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub last_seen_scan: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScanView {
    pub generation: u64,
    pub status: String,
    pub in_flight: bool,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub error: Option<AppError>,
    pub install_error: Option<AppError>,
    pub interval_ms: u64,
    pub scope: String,
    pub sampled_processes: u64,
    pub unreadable_processes: u64,
    pub tracked_processes: u64,
    pub note: String,
}

impl ScanView {
    pub fn idle(interval_ms: u64, scope: &str) -> Self {
        Self {
            generation: 0,
            status: "idle".to_string(),
            in_flight: false,
            last_attempt_at: None,
            last_success_at: None,
            error: None,
            install_error: None,
            interval_ms,
            scope: scope.to_string(),
            sampled_processes: 0,
            unreadable_processes: 0,
            tracked_processes: 0,
            note: SAMPLING_NOTE.to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessDiscoveryPage {
    pub items: Vec<ProcessDiscoveryView>,
    pub next_cursor: Option<String>,
    pub total: u64,
    pub scan: ScanView,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationQuery {
    #[serde(default)]
    pub agent_id: Option<String>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl InstallationQuery {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_optional_id("agentId", &self.agent_id)?;
        validate_limit(self.limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessQuery {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub state: Option<ProcessState>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl ProcessQuery {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_optional_id("agentId", &self.agent_id)?;
        validate_optional_id("workspaceId", &self.workspace_id)?;
        validate_limit(self.limit)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoverySettings {
    pub interval_ms: u64,
    pub min_interval_ms: u64,
    pub max_interval_ms: u64,
    pub note: String,
}

impl DiscoverySettings {
    pub fn for_interval(interval_ms: u64) -> Self {
        Self {
            interval_ms,
            min_interval_ms: MIN_SCAN_INTERVAL_MS,
            max_interval_ms: MAX_SCAN_INTERVAL_MS,
            note: "The interval bounds the number of scans per minute. A shorter interval does not guarantee that very short-lived processes are observed.".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoverySettingsUpdate {
    pub interval_ms: u64,
}

fn validate_limit(limit: u32) -> Result<(), AppError> {
    if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
        return Err(AppError::invalid_argument(
            "limit",
            format!("must be between 1 and {MAX_PAGE_LIMIT}"),
        ));
    }
    Ok(())
}

fn validate_optional_id(field: &str, value: &Option<String>) -> Result<(), AppError> {
    if let Some(text) = value {
        if text.trim().is_empty() {
            return Err(AppError::invalid_argument(field, "must not be empty"));
        }
    }
    Ok(())
}

/// Cursor payload. `generation` pins a page walk to one scan generation (or to
/// one installation epoch); continuing a walk after the data changed is
/// rejected explicitly instead of silently skipping or duplicating rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CursorPayload {
    sort: String,
    key: String,
    id: String,
    generation: u64,
}

pub fn encode_cursor(sort: &str, key: &str, id: &str, generation: u64) -> String {
    let payload = CursorPayload {
        sort: sort.to_string(),
        key: key.to_string(),
        id: id.to_string(),
        generation,
    };
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap_or_default())
}

pub fn decode_cursor(
    cursor: &str,
    expected_sort: &str,
    current_generation: u64,
) -> Result<(String, String), AppError> {
    if cursor.trim().is_empty() {
        return Err(AppError::invalid_argument("cursor", "must not be empty"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    let payload: CursorPayload = serde_json::from_slice(&bytes)
        .map_err(|_| AppError::invalid_argument("cursor", "is not a valid pagination cursor"))?;
    if payload.sort != expected_sort {
        return Err(AppError::invalid_argument(
            "cursor",
            format!("cursor belongs to {}, not {expected_sort}", payload.sort),
        ));
    }
    if payload.generation != current_generation {
        return Err(AppError::conflict(
            "discovery cursor",
            "the scan generation changed; reload the first page",
        )
        .with_details(serde_json::json!({
            "stage": "cursorGeneration",
            "cursorGeneration": payload.generation,
            "currentGeneration": current_generation,
        })));
    }
    Ok((payload.key, payload.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::error::ErrorCode;

    #[test]
    fn query_bounds_match_the_frozen_page_contract() {
        let ok = InstallationQuery {
            agent_id: None,
            limit: 200,
            cursor: None,
        };
        assert!(ok.validate().is_ok());
        for limit in [0u32, 201] {
            let query = InstallationQuery {
                agent_id: None,
                limit,
                cursor: None,
            };
            assert_eq!(query.validate().unwrap_err().code, ErrorCode::InvalidArgument);
        }
        let process = ProcessQuery {
            agent_id: Some(" ".to_string()),
            workspace_id: None,
            state: None,
            limit: 20,
            cursor: None,
        };
        assert!(process.validate().is_err());
        assert!(
            serde_json::from_value::<ProcessQuery>(serde_json::json!({
                "limit": 20,
                "unexpected": true
            }))
            .is_err()
        );
    }

    #[test]
    fn cursor_is_bound_to_the_scan_generation() {
        let cursor = encode_cursor("processes", "codex", "id-1", 7);
        assert_eq!(
            decode_cursor(&cursor, "processes", 7).unwrap(),
            ("codex".to_string(), "id-1".to_string())
        );
        let error = decode_cursor(&cursor, "processes", 8).unwrap_err();
        assert_eq!(error.code, crate::multicli::error::ErrorCode::Conflict);
        assert_eq!(error.details["cursorGeneration"], 7);
        assert_eq!(error.details["currentGeneration"], 8);
        assert!(decode_cursor(&cursor, "installations", 7).is_err());
        assert!(decode_cursor("not-a-cursor", "processes", 7).is_err());
    }
}
