//! MultiCLI domain layer (S01).
//!
//! This module owns the frozen v1 domain DTOs, the isolated SQLite store and
//! its versioned migrations, real path/worktree resolution, the internal
//! query service used by the navigation UI, and the redacted diagnostics
//! document. Product IPC is registered in `multicli_bootstrap`.

pub mod custom;
pub mod diagnostics;
pub mod discovery;
pub mod domain;
pub mod error;
pub mod history;
pub mod path;
pub mod runtime;
pub mod terminal;
pub mod usage;
mod repo;
mod service;
mod store;

#[cfg(test)]
mod tests;

pub use diagnostics::{DiagnosticsExport, DIAGNOSTICS_SCHEMA_VERSION};
pub use domain::{
    AgentDefinition, AgentInstallation, AgentListItem, Capability, CapabilityEvidenceLevel,
    CapabilityName, CapabilityQuery, CapabilityStatus, EventEnvelope, EvidenceLevel,
    ExecutionStatus, Host, HostKind, HostPath, InteractionStatus, LinkKind, Message, Page,
    PageRequest, PathStyle, ProcessIdentity, ProcessObservation, Run, RunOwnership, RunProcessLink,
    RunTransport, Session, SessionDetail, SessionOrigin, SessionQuery, SessionRelation,
    SessionRelationKind, SessionSummary, SessionWorkspace, SessionWorkspaceRole, Workspace,
    WorkspaceListItem,
};
pub use error::{AppError, ErrorCode};
pub use path::{
    workspace_from_resolved, GitStatus, PathResolutionError, PathResolutionStatus,
    ResolvedWorkspacePath,
};
pub use custom::{CustomCliInput, CustomCliView};
pub use service::MultiCliService;
pub use usage::{
    ProviderCatalogEntry, QuotaPool, RefreshOutcome, UsageAccountView, UsageCenter,
    UsageOverviewView, UsageRefreshRequest, UsageRefreshResult, UsageSample, UsageSnapshot,
    UsageStatus,
};
pub use store::{BackupInfo, MigrationReport, MultiCliStore, SCHEMA_VERSION};
