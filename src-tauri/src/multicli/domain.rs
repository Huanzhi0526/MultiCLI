//! Frozen v1 domain DTOs (`docs/contracts/domain.schema.json` and
//! `docs/contracts/events.schema.json`).
//!
//! Wire names are camelCase and unknown fields are rejected so a contract
//! change cannot silently pass through the boundary. Internal SQL uses
//! snake_case columns, but the enum spellings below are the single source of
//! truth for both JSON and database text values.

use super::error::{AppError, ErrorCode};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::marker::PhantomData;

macro_rules! db_enum {
    ($ty:ty { $($variant:ident => $text:literal),+ $(,)? }) => {
        impl $ty {
            pub(crate) fn as_str(&self) -> &'static str {
                match self {
                    $(Self::$variant => $text),+
                }
            }

            #[allow(dead_code)]
            pub(crate) fn parse_db(value: &str) -> Result<Self, AppError> {
                match value {
                    $($text => Ok(Self::$variant)),+,
                    _ => Err(AppError::new(
                        ErrorCode::Database,
                        format!("unexpected {} value stored in database: {value}", stringify!($ty)),
                    )),
                }
            }
        }
    };
}

fn require_text(field: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        return Err(AppError::invalid_argument(field, "must not be empty"));
    }
    Ok(())
}

fn require_optional_text(field: &str, value: &Option<String>) -> Result<(), AppError> {
    if let Some(text) = value {
        require_text(field, text)?;
    }
    Ok(())
}

/// Deserializer for a frozen `required` field that may be explicitly null.
///
/// `deserialize_any` is essential: serde's missing-field deserializer
/// implements `deserialize_option` as `visit_none`, which would silently turn
/// an omitted key into an explicit null.
fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    deserializer.deserialize_any(RequiredNullableVisitor(PhantomData))
}

struct RequiredNullableVisitor<T>(PhantomData<T>);

impl<'de, T: Deserialize<'de>> Visitor<'de> for RequiredNullableVisitor<T> {
    type Value = Option<T>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a nullable value")
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(None)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Some)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::BoolDeserializer::new(value)).map(Some)
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::I64Deserializer::new(value)).map(Some)
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::U64Deserializer::new(value)).map(Some)
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::F64Deserializer::new(value)).map(Some)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::StrDeserializer::new(value)).map(Some)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::StringDeserializer::new(value)).map(Some)
    }

    fn visit_bytes<E>(self, value: &[u8]) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        T::deserialize(serde::de::value::BytesDeserializer::new(value)).map(Some)
    }

    fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        T::deserialize(serde::de::value::SeqAccessDeserializer::new(seq)).map(Some)
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Some)
    }
}

// ---------------------------------------------------------------------------
// Host and path
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostKind {
    LocalWindows,
}

db_enum!(HostKind { LocalWindows => "local-windows" });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PathStyle {
    Windows,
}

db_enum!(PathStyle { Windows => "windows" });

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostPath {
    pub host_id: String,
    pub display_path: String,
    #[serde(deserialize_with = "required_nullable")]
    pub canonical_path: Option<String>,
}

impl HostPath {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("hostId", &self.host_id)?;
        require_text("displayPath", &self.display_path)?;
        require_optional_text("canonicalPath", &self.canonical_path)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Host {
    pub id: String,
    pub kind: HostKind,
    pub path_style: PathStyle,
}

impl Host {
    pub fn local(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: HostKind::LocalWindows,
            path_style: PathStyle::Windows,
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)
    }
}

// ---------------------------------------------------------------------------
// Agents and installations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentDefinition {
    pub id: String,
    pub vendor: String,
    pub display_name: String,
}

impl AgentDefinition {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)?;
        require_text("vendor", &self.vendor)?;
        require_text("displayName", &self.display_name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentInstallation {
    pub id: String,
    pub host_id: String,
    pub agent_id: String,
    pub executable: HostPath,
    #[serde(deserialize_with = "required_nullable")]
    pub version: Option<String>,
    pub config_profile_id: String,
}

impl AgentInstallation {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)?;
        require_text("hostId", &self.host_id)?;
        require_text("agentId", &self.agent_id)?;
        self.executable.validate()?;
        if self.executable.host_id != self.host_id {
            return Err(AppError::invalid_argument(
                "executable.hostId",
                "must match installation hostId",
            ));
        }
        require_optional_text("version", &self.version)?;
        require_text("configProfileId", &self.config_profile_id)
    }
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Workspace {
    pub id: String,
    pub path: HostPath,
    #[serde(deserialize_with = "required_nullable")]
    pub repo_group_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub worktree_id: Option<String>,
}

impl Workspace {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)?;
        self.path.validate()?;
        require_optional_text("repoGroupId", &self.repo_group_id)?;
        require_optional_text("worktreeId", &self.worktree_id)
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionOrigin {
    Managed,
    ExternalObserved,
    HistoryOnly,
}

db_enum!(SessionOrigin {
    Managed => "managed",
    ExternalObserved => "external-observed",
    HistoryOnly => "history-only",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Session {
    pub id: String,
    pub agent_id: String,
    pub host_id: String,
    pub source_namespace: String,
    #[serde(deserialize_with = "required_nullable")]
    pub native_id: Option<String>,
    pub origin: SessionOrigin,
    pub title: String,
}

impl Session {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)?;
        require_text("agentId", &self.agent_id)?;
        require_text("hostId", &self.host_id)?;
        require_text("sourceNamespace", &self.source_namespace)?;
        require_optional_text("nativeId", &self.native_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionWorkspaceRole {
    Primary,
    Related,
}

db_enum!(SessionWorkspaceRole {
    Primary => "primary",
    Related => "related",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionWorkspace {
    pub session_id: String,
    pub workspace_id: String,
    pub role: SessionWorkspaceRole,
    pub evidence: String,
}

impl SessionWorkspace {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("sessionId", &self.session_id)?;
        require_text("workspaceId", &self.workspace_id)?;
        require_text("evidence", &self.evidence)
    }
}

// ---------------------------------------------------------------------------
// Runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunTransport {
    Native,
    Acp,
    Pty,
}

db_enum!(RunTransport {
    Native => "native",
    Acp => "acp",
    Pty => "pty",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunOwnership {
    Managed,
    ExternalObserved,
}

db_enum!(RunOwnership {
    Managed => "managed",
    ExternalObserved => "external-observed",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionStatus {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
    Unknown,
}

db_enum!(ExecutionStatus {
    Starting => "starting",
    Running => "running",
    Stopping => "stopping",
    Exited => "exited",
    Failed => "failed",
    Unknown => "unknown",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InteractionStatus {
    Working,
    AwaitingPermission,
    AwaitingInput,
    Idle,
    Unknown,
}

db_enum!(InteractionStatus {
    Working => "working",
    AwaitingPermission => "awaiting-permission",
    AwaitingInput => "awaiting-input",
    Idle => "idle",
    Unknown => "unknown",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EvidenceLevel {
    Observed,
    Inferred,
}

db_enum!(EvidenceLevel {
    Observed => "observed",
    Inferred => "inferred",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Run {
    pub id: String,
    pub session_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub installation_id: Option<String>,
    pub transport: RunTransport,
    pub ownership: RunOwnership,
    #[serde(deserialize_with = "required_nullable")]
    pub started_at: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub ended_at: Option<String>,
    pub execution_status: ExecutionStatus,
    pub interaction_status: InteractionStatus,
    pub evidence_level: EvidenceLevel,
    #[serde(deserialize_with = "required_nullable")]
    pub evidence: Option<String>,
}

impl Run {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)?;
        require_text("sessionId", &self.session_id)?;
        require_optional_text("installationId", &self.installation_id)?;
        require_optional_text("startedAt", &self.started_at)?;
        require_optional_text("endedAt", &self.ended_at)?;
        require_optional_text("evidence", &self.evidence)
    }
}

// ---------------------------------------------------------------------------
// Processes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessIdentity {
    pub host_id: String,
    pub pid: u32,
    pub creation_time: String,
}

impl ProcessIdentity {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("hostId", &self.host_id)?;
        require_text("creationTime", &self.creation_time)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessObservation {
    pub identity: ProcessIdentity,
    #[serde(deserialize_with = "required_nullable")]
    pub parent_identity: Option<ProcessIdentity>,
    #[serde(deserialize_with = "required_nullable")]
    pub executable: Option<HostPath>,
    #[serde(deserialize_with = "required_nullable")]
    pub redacted_command: Option<String>,
    pub observed_at: String,
}

impl ProcessObservation {
    pub fn validate(&self) -> Result<(), AppError> {
        self.identity.validate()?;
        if let Some(parent) = &self.parent_identity {
            parent.validate()?;
        }
        if let Some(executable) = &self.executable {
            executable.validate()?;
        }
        require_text("observedAt", &self.observed_at)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Owned,
    Observed,
    Inferred,
}

db_enum!(LinkKind {
    Owned => "owned",
    Observed => "observed",
    Inferred => "inferred",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunProcessLink {
    pub run_id: String,
    pub process_identity: ProcessIdentity,
    pub link_kind: LinkKind,
    pub evidence: String,
}

impl RunProcessLink {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("runId", &self.run_id)?;
        self.process_identity.validate()?;
        require_text("evidence", &self.evidence)
    }
}

// ---------------------------------------------------------------------------
// Session relations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionRelationKind {
    OsParent,
    ExplicitDelegation,
    Inferred,
}

db_enum!(SessionRelationKind {
    OsParent => "os-parent",
    ExplicitDelegation => "explicit-delegation",
    Inferred => "inferred",
});

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRelation {
    pub parent_session_id: String,
    pub child_session_id: String,
    pub kind: SessionRelationKind,
    pub confidence: f64,
    pub evidence: String,
}

impl SessionRelation {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("parentSessionId", &self.parent_session_id)?;
        require_text("childSessionId", &self.child_session_id)?;
        if self.parent_session_id == self.child_session_id {
            return Err(AppError::invalid_argument(
                "childSessionId",
                "a session cannot relate to itself",
            ));
        }
        if !self.confidence.is_finite() || !(0.0..=1.0).contains(&self.confidence) {
            return Err(AppError::invalid_argument(
                "confidence",
                "must be a finite number between 0 and 1",
            ));
        }
        require_text("evidence", &self.evidence)
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CapabilityName {
    InstalledDetection,
    ProcessDetection,
    HistoryRead,
    HistorySearch,
    Resume,
    StructuredChat,
    PermissionFlow,
    Pty,
    QuotaRead,
}

db_enum!(CapabilityName {
    InstalledDetection => "installedDetection",
    ProcessDetection => "processDetection",
    HistoryRead => "historyRead",
    HistorySearch => "historySearch",
    Resume => "resume",
    StructuredChat => "structuredChat",
    PermissionFlow => "permissionFlow",
    Pty => "pty",
    QuotaRead => "quotaRead",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityStatus {
    Supported,
    Unsupported,
    Unverified,
}

db_enum!(CapabilityStatus {
    Supported => "supported",
    Unsupported => "unsupported",
    Unverified => "unverified",
});

/// Capability evidence layer. Kept separate from runtime evidence
/// (`observed` / `inferred`) so a fixture can never be reported as live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CapabilityEvidenceLevel {
    Document,
    Fixture,
    Live,
}

db_enum!(CapabilityEvidenceLevel {
    Document => "document",
    Fixture => "fixture",
    Live => "live",
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Capability {
    pub agent_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub installation_id: Option<String>,
    pub name: CapabilityName,
    pub status: CapabilityStatus,
    #[serde(deserialize_with = "required_nullable")]
    pub version_range: Option<String>,
    pub evidence_level: CapabilityEvidenceLevel,
    pub source: String,
    #[serde(deserialize_with = "required_nullable")]
    pub verified_at: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub reason: Option<String>,
}

impl Capability {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("agentId", &self.agent_id)?;
        require_optional_text("installationId", &self.installation_id)?;
        require_optional_text("versionRange", &self.version_range)?;
        require_text("source", &self.source)?;
        require_optional_text("verifiedAt", &self.verified_at)?;
        require_optional_text("reason", &self.reason)
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    #[serde(deserialize_with = "required_nullable")]
    pub run_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub native_event_id: Option<String>,
    pub source_key: String,
    pub seq: u64,
    pub kind: String,
    pub content: Value,
    #[serde(deserialize_with = "required_nullable")]
    pub source_ref: Option<String>,
    pub observed_at: String,
}

impl Message {
    pub fn validate(&self) -> Result<(), AppError> {
        require_text("id", &self.id)?;
        require_text("sessionId", &self.session_id)?;
        require_optional_text("runId", &self.run_id)?;
        require_optional_text("nativeEventId", &self.native_event_id)?;
        require_text("sourceKey", &self.source_key)?;
        require_text("kind", &self.kind)?;
        if !self.content.is_object() {
            return Err(AppError::invalid_argument(
                "content",
                "must be a JSON object",
            ));
        }
        require_optional_text("sourceRef", &self.source_ref)?;
        require_text("observedAt", &self.observed_at)
    }
}

// ---------------------------------------------------------------------------
// Event envelope
// ---------------------------------------------------------------------------

pub const EVENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EventEnvelope {
    pub schema_version: u32,
    #[serde(deserialize_with = "required_nullable")]
    pub session_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    pub run_id: Option<String>,
    pub stream_id: String,
    pub seq: u64,
    pub timestamp: String,
    pub kind: String,
    pub source: String,
    #[serde(deserialize_with = "required_nullable")]
    pub native_event_id: Option<String>,
    pub source_key: String,
    pub payload: Value,
}

impl EventEnvelope {
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err(AppError::new(
                ErrorCode::Protocol,
                format!(
                    "unsupported event schemaVersion {}; only {} is executable",
                    self.schema_version, EVENT_SCHEMA_VERSION
                ),
            )
            .with_details(serde_json::json!({
                "schemaVersion": self.schema_version,
                "supported": EVENT_SCHEMA_VERSION,
            })));
        }
        require_optional_text("sessionId", &self.session_id)?;
        require_optional_text("runId", &self.run_id)?;
        require_optional_text("nativeEventId", &self.native_event_id)?;
        require_text("streamId", &self.stream_id)?;
        require_text("timestamp", &self.timestamp)?;
        require_text("kind", &self.kind)?;
        require_text("source", &self.source)?;
        require_text("sourceKey", &self.source_key)?;
        if !self.payload.is_object() {
            return Err(AppError::invalid_argument(
                "payload",
                "must be a JSON object",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

pub const MAX_PAGE_LIMIT: u32 = 200;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageRequest {
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl PageRequest {
    pub fn new(limit: u32, cursor: Option<String>) -> Self {
        Self { limit, cursor }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if !(1..=MAX_PAGE_LIMIT).contains(&self.limit) {
            return Err(AppError::invalid_argument(
                "limit",
                format!("must be between 1 and {MAX_PAGE_LIMIT}"),
            ));
        }
        require_optional_text("cursor", &self.cursor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub total: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionQuery {
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub origin: Option<SessionOrigin>,
    pub limit: u32,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl SessionQuery {
    pub fn page_request(&self) -> PageRequest {
        PageRequest {
            limit: self.limit,
            cursor: self.cursor.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        require_optional_text("workspaceId", &self.workspace_id)?;
        require_optional_text("agentId", &self.agent_id)?;
        self.page_request().validate()
    }
}

// ---------------------------------------------------------------------------
// Read models for the internal navigation service
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceListItem {
    pub workspace: Workspace,
    pub session_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentListItem {
    pub agent: AgentDefinition,
    pub installation_count: u64,
    pub session_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionSummary {
    pub session: Session,
    pub primary_workspace_id: Option<String>,
    pub workspace_ids: Vec<String>,
    pub message_count: u64,
    pub run_count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionDetail {
    pub session: Session,
    pub workspaces: Vec<SessionWorkspace>,
    pub runs: Vec<Run>,
    pub messages: Vec<Message>,
    pub message_count: u64,
    pub run_count: u64,
}
