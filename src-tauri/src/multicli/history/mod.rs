//! S03 read-only history index: sources, parsers, cursors, FTS search and
//! resume-target resolution.
//!
//! Hard boundaries for this module:
//! - The configured source root is only ever **read**. No CLI file is modified,
//!   moved, renamed, migrated or cleaned.
//! - A history session has `origin = history-only` and no Run. The absence of
//!   protocol evidence is not papered over with a fabricated native/acp/pty
//!   connection.
//! - Derived data (messages, cursors, FTS rows, provenance) can be rebuilt; user
//!   notes/tags and the stable session ids cannot be touched by a rebuild.
//! - Resume resolution parses a target and returns evidence. It never spawns,
//!   sends, writes or leases.

pub mod engine;
pub mod guard;
pub mod ids;
pub mod model;
mod parser;
pub mod query;
pub mod repo;
pub mod resume;
pub mod scan;
pub mod search;
pub mod tooling;

#[cfg(test)]
mod tests;

pub use engine::HistoryEngine;
pub use model::{
    HistoryCandidateView, HistoryOverallStatus, HistoryRebuildQuery, HistoryRefreshQuery,
    HistorySearchQuery, HistorySourceStatus, HistorySourceUpdate, HistorySourceView,
    HistoryStatusView, MessagePage, MessagePageQuery, ResumeTargetView, SearchPage,
    SessionMetadataView, HISTORY_SCHEMA_VERSION,
};
