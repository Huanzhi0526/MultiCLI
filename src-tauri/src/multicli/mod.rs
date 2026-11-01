//! MultiCLI domain layer (S01-01).
//!
//! This module owns the frozen v1 domain DTOs, the isolated SQLite store and
//! its versioned migrations, plus the internal query service used by the
//! navigation UI in S01-02. It deliberately does not register any product IPC,
//! touch the upstream database, or fabricate demo rows at startup.

pub mod domain;
pub mod error;
mod repo;
mod service;
mod store;

#[cfg(test)]
mod tests;

pub use domain::{
    AgentDefinition, AgentInstallation, AgentListItem, Capability, CapabilityEvidenceLevel,
    CapabilityName, CapabilityStatus, EventEnvelope, ExecutionStatus, Host, HostKind, HostPath,
    InteractionStatus, LinkKind, Message, Page, PageRequest, PathStyle, ProcessIdentity,
    ProcessObservation, Run, RunOwnership, RunProcessLink, RunTransport, Session, SessionDetail,
    SessionOrigin, SessionQuery, SessionRelation, SessionRelationKind, SessionSummary,
    SessionWorkspace, SessionWorkspaceRole, Workspace, WorkspaceListItem,
};
pub use error::{AppError, ErrorCode};
pub use service::MultiCliService;
pub use store::{BackupInfo, MigrationReport, MultiCliStore, SCHEMA_VERSION};
