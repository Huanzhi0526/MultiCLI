//! Redacted diagnostics export.
//!
//! The exported document is built from fixed, whitelisted fields only. It
//! never contains credentials, prompts, message bodies, command lines, session
//! native ids, accounts, raw logs, the database file, or absolute workspace /
//! user paths. The response returned to the UI may name the local file path so
//! the user can find it, but that path is not embedded in the file itself.

use super::error::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

/// Diagnostics document version, independent from the database schema.
pub const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiagnosticsExport {
    pub path: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DiagnosticsCounts {
    pub(crate) hosts: u64,
    pub(crate) agents: u64,
    pub(crate) installations: u64,
    pub(crate) workspaces: u64,
    pub(crate) sessions: u64,
    pub(crate) runs: u64,
    pub(crate) messages: u64,
    pub(crate) capabilities: u64,
    pub(crate) processes: u64,
    pub(crate) run_process_links: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapabilityCounts {
    pub(crate) supported: u64,
    pub(crate) unsupported: u64,
    pub(crate) unverified: u64,
    pub(crate) document: u64,
    pub(crate) fixture: u64,
    pub(crate) live: u64,
}

pub(crate) fn build_diagnostics(
    profile: &str,
    application_version: &str,
    schema_version: i64,
    journal_mode: &str,
    counts: &DiagnosticsCounts,
    capabilities: &BTreeMap<String, CapabilityCounts>,
    generated_at: &str,
) -> Value {
    let by_agent = capabilities
        .iter()
        .enumerate()
        .map(|(index, (_agent_id, counts))| {
            let mut entry = Map::new();
            // Agent ids are opaque external identifiers, not trusted catalog
            // names: they may themselves contain paths or credentials.
            entry.insert("agentId".to_string(), json!(format!("agent-{}", index + 1)));
            entry.insert("supported".to_string(), json!(counts.supported));
            entry.insert("unsupported".to_string(), json!(counts.unsupported));
            entry.insert("unverified".to_string(), json!(counts.unverified));
            entry.insert("document".to_string(), json!(counts.document));
            entry.insert("fixture".to_string(), json!(counts.fixture));
            entry.insert("live".to_string(), json!(counts.live));
            Value::Object(entry)
        })
        .collect::<Vec<_>>();

    json!({
        "schemaVersion": DIAGNOSTICS_SCHEMA_VERSION,
        "generatedAt": generated_at,
        "application": {
            "name": "MultiCLI",
            "version": application_version,
        },
        "platform": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        },
        "profile": profile,
        "database": {
            "schemaVersion": schema_version,
            "journalMode": journal_mode,
        },
        "counts": {
            "hosts": counts.hosts,
            "agents": counts.agents,
            "installations": counts.installations,
            "workspaces": counts.workspaces,
            "sessions": counts.sessions,
            "runs": counts.runs,
            "messages": counts.messages,
            "capabilities": counts.capabilities,
            "processes": counts.processes,
            "runProcessLinks": counts.run_process_links,
        },
        "capabilities": {
            "byAgent": by_agent,
        },
    })
}

pub(crate) fn write_diagnostics(
    data_root: &Path,
    document: &Value,
    epoch_ms: i64,
    created_at: &str,
) -> Result<DiagnosticsExport, AppError> {
    let directory = data_root.join("diagnostics");
    std::fs::create_dir_all(&directory).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("diagnostics directory could not be created: {error}"),
        )
        .with_details(json!({ "stage": "exportDiagnostics" }))
        .with_retryable(true)
    })?;

    let mut path = directory.join(format!("diagnostics-{epoch_ms}.json"));
    let mut suffix = 1u32;
    while path.exists() {
        path = directory.join(format!("diagnostics-{epoch_ms}-{suffix}.json"));
        suffix += 1;
        if suffix > 1000 {
            return Err(AppError::new(
                ErrorCode::Io,
                "diagnostics file name could not be allocated",
            )
            .with_details(json!({ "stage": "exportDiagnostics" }))
            .with_retryable(true));
        }
    }

    let serialized = serde_json::to_vec_pretty(document)?;
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            AppError::new(
                ErrorCode::Io,
                format!("diagnostics file could not be created: {error}"),
            )
            .with_details(json!({ "stage": "exportDiagnostics" }))
            .with_retryable(true)
        })?;
    file.write_all(&serialized).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("diagnostics file could not be written: {error}"),
        )
        .with_details(json!({ "stage": "exportDiagnostics" }))
        .with_retryable(true)
    })?;
    file.flush().map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("diagnostics file could not be flushed: {error}"),
        )
        .with_details(json!({ "stage": "exportDiagnostics" }))
        .with_retryable(true)
    })?;

    Ok(DiagnosticsExport {
        path: path.display().to_string(),
        created_at: created_at.to_string(),
    })
}

/// UTC ISO-8601 timestamp derived from the Unix epoch without extra crates.
pub(crate) fn iso_timestamp(epoch_ms: i64) -> String {
    let seconds = epoch_ms.div_euclid(1000);
    let millis = epoch_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm (days since 1970-01-01).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::iso_timestamp;

    #[test]
    fn iso_timestamp_formats_known_epochs() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_timestamp(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
        assert_eq!(iso_timestamp(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
    }
}
