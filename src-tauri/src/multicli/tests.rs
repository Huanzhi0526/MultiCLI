//! S01-01 data-layer tests.
//!
//! Every test uses a synthetic temporary MultiCLI data root. No real dev,
//! test or prod database, upstream history or CLI configuration is touched.

use super::domain::*;
use super::error::{AppError, ErrorCode};
use super::service::MultiCliService;
use super::store::{MigrationFault, SCHEMA_VERSION};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const HOST_ID: &str = "local-windows";
const T1: &str = "2026-11-01T10:00:00.000Z";
const T2: &str = "2026-11-01T10:00:01.000Z";
const T3: &str = "2026-11-01T10:00:02.000Z";

fn some<T>(value: T) -> Option<T> {
    Some(value)
}

fn none<T>() -> Option<T> {
    None
}

// ---------------------------------------------------------------------------
// Isolation helpers
// ---------------------------------------------------------------------------

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("multicli-s01-{name}-{}", Uuid::new_v4().simple()));
        fs::create_dir_all(&path).expect("create synthetic temp root");
        Self { path }
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(windows)]
fn create_junction(link: &Path, target: &Path) {
    let status = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("run mklink /J");
    assert!(
        status.success(),
        "mklink /J failed: {} -> {}",
        link.display(),
        target.display()
    );
}

fn open(root: &TempRoot) -> MultiCliService {
    MultiCliService::open(&root.path).expect("open isolated store")
}

fn seed_host(service: &MultiCliService) {
    service
        .register_host(&Host::local(HOST_ID))
        .expect("register local host");
}

fn agent(id: &str, vendor: &str, display_name: &str) -> AgentDefinition {
    AgentDefinition {
        id: id.to_string(),
        vendor: vendor.to_string(),
        display_name: display_name.to_string(),
    }
}

fn workspace(id: &str, host_id: &str, path: &str) -> Workspace {
    Workspace {
        id: id.to_string(),
        path: HostPath {
            host_id: host_id.to_string(),
            display_path: path.to_string(),
            canonical_path: some(path.to_string()),
        },
        repo_group_id: none(),
        worktree_id: none(),
    }
}

fn installation(
    id: &str,
    agent_id: &str,
    host_id: &str,
    config_profile_id: &str,
) -> AgentInstallation {
    AgentInstallation {
        id: id.to_string(),
        host_id: host_id.to_string(),
        agent_id: agent_id.to_string(),
        executable: HostPath {
            host_id: host_id.to_string(),
            display_path: "C:\\Tools\\cli.exe".to_string(),
            canonical_path: some("C:\\Tools\\cli.exe".to_string()),
        },
        version: some("1.0.0".to_string()),
        config_profile_id: config_profile_id.to_string(),
    }
}

fn session(id: &str, agent_id: &str, native_id: Option<&str>) -> Session {
    Session {
        id: id.to_string(),
        agent_id: agent_id.to_string(),
        host_id: HOST_ID.to_string(),
        source_namespace: format!("{agent_id}/default/{HOST_ID}"),
        native_id: native_id.map(str::to_string),
        origin: SessionOrigin::Managed,
        title: format!("session {id}"),
    }
}

fn session_on(id: &str, agent_id: &str, host_id: &str) -> Session {
    Session {
        id: id.to_string(),
        agent_id: agent_id.to_string(),
        host_id: host_id.to_string(),
        source_namespace: format!("{agent_id}/synthetic/{host_id}"),
        native_id: some(id.to_string()),
        origin: SessionOrigin::Managed,
        title: format!("session {id}"),
    }
}

fn run_for(
    id: &str,
    session_id: &str,
    installation_id: Option<&str>,
    ownership: RunOwnership,
) -> Run {
    Run {
        id: id.to_string(),
        session_id: session_id.to_string(),
        installation_id: installation_id.map(str::to_string),
        transport: RunTransport::Native,
        ownership,
        started_at: some(T1.to_string()),
        ended_at: none(),
        execution_status: ExecutionStatus::Running,
        interaction_status: InteractionStatus::Working,
        evidence_level: EvidenceLevel::Observed,
        evidence: some("synthetic".to_string()),
    }
}

fn capability_for(
    agent_id: &str,
    installation_id: Option<&str>,
    name: CapabilityName,
) -> Capability {
    Capability {
        agent_id: agent_id.to_string(),
        installation_id: installation_id.map(str::to_string),
        name,
        status: CapabilityStatus::Supported,
        version_range: none(),
        evidence_level: CapabilityEvidenceLevel::Fixture,
        source: "synthetic".to_string(),
        verified_at: none(),
        reason: none(),
    }
}

fn message(
    id: &str,
    session_id: &str,
    source_key: &str,
    native_event_id: Option<&str>,
    seq: u64,
    observed_at: &str,
) -> Message {
    Message {
        id: id.to_string(),
        session_id: session_id.to_string(),
        run_id: none(),
        native_event_id: native_event_id.map(str::to_string),
        source_key: source_key.to_string(),
        seq,
        kind: "assistant-message".to_string(),
        content: json!({ "role": "assistant", "text": format!("message {id}") }),
        source_ref: some(format!("synthetic:{id}")),
        observed_at: observed_at.to_string(),
    }
}

fn message_for(id: &str, session_id: &str, run_id: Option<&str>) -> Message {
    Message {
        id: id.to_string(),
        session_id: session_id.to_string(),
        run_id: run_id.map(str::to_string),
        native_event_id: none(),
        source_key: id.to_string(),
        seq: 1,
        kind: "message".to_string(),
        content: json!({ "text": "synthetic" }),
        source_ref: none(),
        observed_at: T1.to_string(),
    }
}

fn link(session_id: &str, workspace_id: &str, role: SessionWorkspaceRole) -> SessionWorkspace {
    SessionWorkspace {
        session_id: session_id.to_string(),
        workspace_id: workspace_id.to_string(),
        role,
        evidence: format!("synthetic link {session_id} -> {workspace_id}"),
    }
}

// ---------------------------------------------------------------------------
// Frozen schema validation (no external JSON-schema dependency)
// ---------------------------------------------------------------------------

struct FrozenSchema {
    root: Value,
}

impl FrozenSchema {
    fn load(file: &str) -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/contracts")
            .join(file);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        Self {
            root: serde_json::from_str(&text).expect("parse frozen schema"),
        }
    }

    fn check_def(&self, name: &str, instance: &Value) {
        let mut errors = Vec::new();
        validate_instance(
            &self.root["$defs"][name],
            &self.root,
            instance,
            "$",
            &mut errors,
        );
        assert!(
            errors.is_empty(),
            "{name} violates frozen schema: {errors:#?}\ninstance: {instance:#?}"
        );
    }

    fn check_root(&self, instance: &Value) {
        let mut errors = Vec::new();
        validate_instance(&self.root, &self.root, instance, "$", &mut errors);
        assert!(
            errors.is_empty(),
            "instance violates frozen schema: {errors:#?}\ninstance: {instance:#?}"
        );
    }
}

fn validate_instance(
    schema: &Value,
    root: &Value,
    instance: &Value,
    path: &str,
    errors: &mut Vec<String>,
) {
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        if let Some(name) = reference.strip_prefix("#/$defs/") {
            validate_instance(&root["$defs"][name], root, instance, path, errors);
        } else {
            errors.push(format!("{path}: unsupported $ref {reference}"));
        }
        return;
    }
    if let Some(options) = schema.get("anyOf").and_then(Value::as_array) {
        let mut matched = false;
        for option in options {
            let mut local = Vec::new();
            validate_instance(option, root, instance, path, &mut local);
            if local.is_empty() {
                matched = true;
                break;
            }
        }
        if !matched {
            errors.push(format!(
                "{path}: value {instance} does not match any anyOf branch"
            ));
        }
        return;
    }
    if let Some(expected) = schema.get("const") {
        if instance != expected {
            errors.push(format!("{path}: expected const {expected}, got {instance}"));
        }
        return;
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(instance) {
            errors.push(format!("{path}: {instance} is not in the frozen enum"));
        }
        return;
    }
    if let Some(expected) = schema.get("type") {
        let matched = match expected {
            Value::String(kind) => type_matches(kind, instance),
            Value::Array(kinds) => kinds
                .iter()
                .filter_map(Value::as_str)
                .any(|kind| type_matches(kind, instance)),
            _ => true,
        };
        if !matched {
            errors.push(format!("{path}: expected type {expected}, got {instance}"));
            return;
        }
    }
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64) {
        if let Some(text) = instance.as_str() {
            if (text.chars().count() as u64) < min {
                errors.push(format!("{path}: shorter than minLength {min}"));
            }
        }
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64) {
        if let Some(number) = instance.as_f64() {
            if number < min {
                errors.push(format!("{path}: {number} is below minimum {min}"));
            }
        }
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64) {
        if let Some(number) = instance.as_f64() {
            if number > max {
                errors.push(format!("{path}: {number} is above maximum {max}"));
            }
        }
    }
    if let Some(object) = instance.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    errors.push(format!("{path}: missing required property {key}"));
                }
            }
        }
        if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
            for (key, value) in object {
                match properties.get(key) {
                    Some(property) => {
                        validate_instance(property, root, value, &format!("{path}.{key}"), errors)
                    }
                    None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                        errors.push(format!("{path}: unexpected property {key}"));
                    }
                    None => {}
                }
            }
        }
    }
    if let Some(array) = instance.as_array() {
        if let Some(min) = schema.get("minItems").and_then(Value::as_u64) {
            if (array.len() as u64) < min {
                errors.push(format!("{path}: fewer than minItems {min}"));
            }
        }
        if let Some(items) = schema.get("items") {
            for (index, item) in array.iter().enumerate() {
                validate_instance(items, root, item, &format!("{path}[{index}]"), errors);
            }
        }
    }
}

fn type_matches(kind: &str, instance: &Value) -> bool {
    match kind {
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "string" => instance.is_string(),
        "integer" => instance.as_i64().is_some() || instance.as_u64().is_some(),
        "number" => instance.is_number(),
        "boolean" => instance.is_boolean(),
        "null" => instance.is_null(),
        _ => true,
    }
}

fn assert_required_nullable<T: serde::de::DeserializeOwned>(
    label: &str,
    value: Value,
    fields: &[&str],
) {
    for field in fields {
        let mut missing = value.clone();
        missing
            .as_object_mut()
            .expect("nullable sample must be an object")
            .remove(*field);
        let result = serde_json::from_value::<T>(missing.clone());
        assert!(
            result.is_err(),
            "{label}: missing required nullable field {field} was accepted; wire={missing}"
        );
        let mut explicit_null = value.clone();
        explicit_null[field] = Value::Null;
        assert!(
            serde_json::from_value::<T>(explicit_null).is_ok(),
            "{label}: explicit null {field} was rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// Frozen DTO samples
// ---------------------------------------------------------------------------

fn sample_host_path() -> HostPath {
    HostPath {
        host_id: HOST_ID.to_string(),
        display_path: "C:\\Work\\Alpha".to_string(),
        canonical_path: some("C:\\Work\\Alpha".to_string()),
    }
}

fn sample_installation() -> AgentInstallation {
    AgentInstallation {
        id: "install-codex".to_string(),
        host_id: HOST_ID.to_string(),
        agent_id: "codex".to_string(),
        executable: HostPath {
            host_id: HOST_ID.to_string(),
            display_path: "C:\\Tools\\codex.cmd".to_string(),
            canonical_path: none(),
        },
        version: some("0.166.0".to_string()),
        config_profile_id: "default".to_string(),
    }
}

fn sample_run() -> Run {
    Run {
        id: "run-1".to_string(),
        session_id: "session-1".to_string(),
        installation_id: some("install-codex".to_string()),
        transport: RunTransport::Native,
        ownership: RunOwnership::Managed,
        started_at: some(T1.to_string()),
        ended_at: none(),
        execution_status: ExecutionStatus::Running,
        interaction_status: InteractionStatus::Working,
        evidence_level: EvidenceLevel::Observed,
        evidence: some("synthetic fixture".to_string()),
    }
}

fn sample_process() -> ProcessObservation {
    ProcessObservation {
        identity: ProcessIdentity {
            host_id: HOST_ID.to_string(),
            pid: 4242,
            creation_time: T1.to_string(),
        },
        parent_identity: some(ProcessIdentity {
            host_id: HOST_ID.to_string(),
            pid: 1000,
            creation_time: T1.to_string(),
        }),
        executable: some(sample_host_path()),
        redacted_command: some("codex --version".to_string()),
        observed_at: T1.to_string(),
    }
}

fn sample_capability() -> Capability {
    Capability {
        agent_id: "codex".to_string(),
        installation_id: some("install-codex".to_string()),
        name: CapabilityName::StructuredChat,
        status: CapabilityStatus::Supported,
        version_range: some(">=0.166.0".to_string()),
        evidence_level: CapabilityEvidenceLevel::Live,
        source: "synthetic contract sample; not a real CLI probe".to_string(),
        verified_at: some(T1.to_string()),
        reason: some("fixture only; no real CLI was probed".to_string()),
    }
}

fn sample_message() -> Message {
    message(
        "message-1",
        "session-1",
        "session-1:0",
        Some("native-0"),
        0,
        T1,
    )
}

fn sample_event() -> EventEnvelope {
    EventEnvelope {
        schema_version: 1,
        session_id: some("session-1".to_string()),
        run_id: some("run-1".to_string()),
        stream_id: "stdout".to_string(),
        seq: 0,
        timestamp: T1.to_string(),
        kind: "message".to_string(),
        source: "codex-native".to_string(),
        native_event_id: some("native-0".to_string()),
        source_key: "run-1:stdout:0".to_string(),
        payload: json!({ "text": "hello" }),
    }
}

#[test]
fn domain_samples_conform_to_frozen_schema() {
    let schema = FrozenSchema::load("domain.schema.json");
    schema.check_def(
        "HostPath",
        &serde_json::to_value(sample_host_path()).unwrap(),
    );
    schema.check_def("Host", &serde_json::to_value(Host::local(HOST_ID)).unwrap());
    schema.check_def(
        "AgentDefinition",
        &serde_json::to_value(agent("codex", "openai", "Codex")).unwrap(),
    );
    schema.check_def(
        "AgentInstallation",
        &serde_json::to_value(sample_installation()).unwrap(),
    );
    schema.check_def(
        "Workspace",
        &serde_json::to_value(workspace("ws-1", HOST_ID, "C:\\Work\\Alpha")).unwrap(),
    );
    schema.check_def(
        "Session",
        &serde_json::to_value(session("session-1", "codex", Some("native-1"))).unwrap(),
    );
    schema.check_def(
        "SessionWorkspace",
        &serde_json::to_value(link("session-1", "ws-1", SessionWorkspaceRole::Primary)).unwrap(),
    );
    schema.check_def(
        "ProcessIdentity",
        &serde_json::to_value(sample_process().identity).unwrap(),
    );
    schema.check_def("Run", &serde_json::to_value(sample_run()).unwrap());
    schema.check_def(
        "ProcessObservation",
        &serde_json::to_value(sample_process()).unwrap(),
    );
    schema.check_def(
        "RunProcessLink",
        &serde_json::to_value(RunProcessLink {
            run_id: "run-1".to_string(),
            process_identity: sample_process().identity,
            link_kind: LinkKind::Owned,
            evidence: "spawn handle".to_string(),
        })
        .unwrap(),
    );
    schema.check_def(
        "SessionRelation",
        &serde_json::to_value(SessionRelation {
            parent_session_id: "session-1".to_string(),
            child_session_id: "session-2".to_string(),
            kind: SessionRelationKind::ExplicitDelegation,
            confidence: 0.9,
            evidence: "tool call".to_string(),
        })
        .unwrap(),
    );
    schema.check_def(
        "Capability",
        &serde_json::to_value(sample_capability()).unwrap(),
    );
    schema.check_def("Message", &serde_json::to_value(sample_message()).unwrap());
}

#[test]
fn event_envelope_conforms_and_rejects_unknown_schema_version() {
    let schema = FrozenSchema::load("events.schema.json");
    let event = sample_event();
    schema.check_root(&serde_json::to_value(&event).unwrap());
    event.validate().expect("schemaVersion 1 is executable");

    let mut future = event;
    future.schema_version = 2;
    let error = future.validate().unwrap_err();
    assert_eq!(error.code, ErrorCode::Protocol);
    assert!(error.message.contains("schemaVersion 2"));
    assert_eq!(error.details["supported"], 1);
}

#[test]
fn required_nullable_fields_reject_missing_keys_but_accept_explicit_null() {
    assert_required_nullable::<HostPath>(
        "HostPath",
        json!({
            "hostId": "host-a",
            "displayPath": "C:\\synthetic",
            "canonicalPath": null,
        }),
        &["canonicalPath"],
    );
    assert_required_nullable::<AgentInstallation>(
        "AgentInstallation",
        json!({
            "id": "install-a",
            "hostId": "host-a",
            "agentId": "agent-a",
            "executable": {
                "hostId": "host-a",
                "displayPath": "C:\\synthetic\\cli.exe",
                "canonicalPath": null,
            },
            "version": null,
            "configProfileId": "default",
        }),
        &["version"],
    );
    assert_required_nullable::<Workspace>(
        "Workspace",
        json!({
            "id": "ws-a",
            "path": {
                "hostId": "host-a",
                "displayPath": "C:\\synthetic",
                "canonicalPath": null,
            },
            "repoGroupId": null,
            "worktreeId": null,
        }),
        &["repoGroupId", "worktreeId"],
    );
    assert_required_nullable::<Session>(
        "Session",
        json!({
            "id": "s-a",
            "agentId": "agent-a",
            "hostId": "host-a",
            "sourceNamespace": "agent-a/synthetic/host-a",
            "nativeId": null,
            "origin": "managed",
            "title": "s-a",
        }),
        &["nativeId"],
    );
    assert_required_nullable::<Run>(
        "Run",
        json!({
            "id": "r-a",
            "sessionId": "s-a",
            "installationId": null,
            "transport": "pty",
            "ownership": "managed",
            "startedAt": null,
            "endedAt": null,
            "executionStatus": "running",
            "interactionStatus": "working",
            "evidenceLevel": "observed",
            "evidence": null,
        }),
        &["installationId", "startedAt", "endedAt", "evidence"],
    );
    assert_required_nullable::<ProcessObservation>(
        "ProcessObservation",
        json!({
            "identity": {"hostId": "host-a", "pid": 1, "creationTime": T1},
            "parentIdentity": null,
            "executable": null,
            "redactedCommand": null,
            "observedAt": T1,
        }),
        &["parentIdentity", "executable", "redactedCommand"],
    );
    assert_required_nullable::<Capability>(
        "Capability",
        json!({
            "agentId": "agent-a",
            "installationId": null,
            "name": "structuredChat",
            "status": "supported",
            "versionRange": null,
            "evidenceLevel": "fixture",
            "source": "synthetic",
            "verifiedAt": null,
            "reason": null,
        }),
        &["installationId", "versionRange", "verifiedAt", "reason"],
    );
    assert_required_nullable::<Message>(
        "Message",
        json!({
            "id": "m-a",
            "sessionId": "s-a",
            "runId": null,
            "nativeEventId": null,
            "sourceKey": "m-a",
            "seq": 0,
            "kind": "message",
            "content": {"text": "synthetic"},
            "sourceRef": null,
            "observedAt": T1,
        }),
        &["runId", "nativeEventId", "sourceRef"],
    );
    assert_required_nullable::<EventEnvelope>(
        "EventEnvelope",
        json!({
            "schemaVersion": 1,
            "sessionId": null,
            "runId": null,
            "streamId": "stdout",
            "seq": 0,
            "timestamp": T1,
            "kind": "message",
            "source": "synthetic",
            "nativeEventId": null,
            "sourceKey": "k",
            "payload": {},
        }),
        &["sessionId", "runId", "nativeEventId"],
    );

    // The reproduced supervisor case: missing canonicalPath is not the same
    // as an explicit null.
    let missing = json!({"hostId": "host-a", "displayPath": "C:\\synthetic"});
    assert!(serde_json::from_value::<HostPath>(missing).is_err());
    let explicit_null =
        json!({"hostId": "host-a", "displayPath": "C:\\synthetic", "canonicalPath": null});
    let path = serde_json::from_value::<HostPath>(explicit_null).expect("explicit null is valid");
    path.validate().expect("explicit null passes validation");
    assert!(path.canonical_path.is_none());
}

#[test]
fn app_error_conforms_to_frozen_error_schema() {
    let schema = FrozenSchema::load("error.schema.json");
    let error = AppError::invalid_argument("limit", "must be between 1 and 200");
    let value = serde_json::to_value(&error).unwrap();
    schema.check_root(&value);
    let object = value.as_object().unwrap();
    let keys = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    assert_eq!(
        keys,
        BTreeSet::from(["code", "details", "message", "retryable"])
    );
    assert_eq!(value["code"], "invalid_argument");
    assert_eq!(value["retryable"], false);
    assert!(value["details"].is_object());
}

#[test]
fn wire_enum_spellings_match_the_frozen_schema() {
    assert_eq!(
        serde_json::to_value(HostKind::LocalWindows).unwrap(),
        json!("local-windows")
    );
    assert_eq!(
        serde_json::to_value(PathStyle::Windows).unwrap(),
        json!("windows")
    );
    assert_eq!(
        serde_json::to_value(SessionOrigin::ExternalObserved).unwrap(),
        json!("external-observed")
    );
    assert_eq!(
        serde_json::to_value(RunOwnership::ExternalObserved).unwrap(),
        json!("external-observed")
    );
    assert_eq!(
        serde_json::to_value(InteractionStatus::AwaitingPermission).unwrap(),
        json!("awaiting-permission")
    );
    assert_eq!(
        serde_json::to_value(CapabilityName::StructuredChat).unwrap(),
        json!("structuredChat")
    );
    assert_eq!(
        serde_json::to_value(CapabilityEvidenceLevel::Live).unwrap(),
        json!("live")
    );
    assert_eq!(
        serde_json::to_value(SessionRelationKind::ExplicitDelegation).unwrap(),
        json!("explicit-delegation")
    );
}

#[test]
fn pagination_request_enforces_the_frozen_limit_and_shape() {
    assert!(PageRequest::new(1, None).validate().is_ok());
    assert!(PageRequest::new(200, None).validate().is_ok());
    for limit in [0, 201, u32::MAX] {
        let error = PageRequest::new(limit, None).validate().unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details["field"], "limit");
    }
    let parsed: PageRequest = serde_json::from_value(json!({ "limit": 20 })).unwrap();
    assert_eq!(parsed.cursor, None);
    assert!(serde_json::from_value::<PageRequest>(json!({ "limit": 20, "extra": 1 })).is_err());
    assert!(PageRequest::new(20, Some("  ".into())).validate().is_err());
}

// ---------------------------------------------------------------------------
// Store lifecycle
// ---------------------------------------------------------------------------

#[test]
fn empty_database_created_and_reopen_is_idempotent() {
    let root = TempRoot::new("empty");
    {
        let service = open(&root);
        assert_eq!(service.schema_version(), SCHEMA_VERSION);
        assert_eq!(service.journal_mode().unwrap().to_ascii_lowercase(), "wal");
        assert!(service.foreign_keys_enabled().unwrap());
        assert!(service.database_path().exists());
        assert_eq!(
            service
                .database_path()
                .file_name()
                .and_then(|name| name.to_str()),
            Some("multicli.db")
        );
        assert_eq!(service.migration_report().from_version, 0);
        assert_eq!(service.migration_report().to_version, SCHEMA_VERSION);
        assert!(service.migration_report().backup.is_none());
        let rows: i64 = service
            .lock_for_test()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, SCHEMA_VERSION);
        assert!(!root.path.join("backups").exists());
    }
    {
        let service = open(&root);
        assert_eq!(service.migration_report().from_version, SCHEMA_VERSION);
        assert_eq!(service.migration_report().to_version, SCHEMA_VERSION);
        assert!(service.migration_report().backup.is_none());
        let rows: i64 = service
            .lock_for_test()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, SCHEMA_VERSION);
    }
}

#[test]
fn legacy_upstream_database_is_not_opened_or_modified() {
    let root = TempRoot::new("legacy");
    let legacy_path = root.path.join("app.db");
    {
        let conn = Connection::open(&legacy_path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions(id TEXT PRIMARY KEY, title TEXT);
             INSERT INTO sessions(id, title) VALUES ('legacy-1', 'do not touch');",
        )
        .unwrap();
    }
    let before = fs::read(&legacy_path).unwrap();
    let service = open(&root);
    assert!(root.path.join("multicli.db").exists());
    assert_eq!(fs::read(&legacy_path).unwrap(), before);
    assert_ne!(service.database_path(), legacy_path.as_path());
    let service_tables: i64 = service
        .lock_for_test()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'sessions'
             AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(service_tables, 1, "synthetic domain table must exist");
    let legacy = Connection::open(&legacy_path).unwrap();
    let legacy_title: String = legacy
        .query_row(
            "SELECT title FROM sessions WHERE id = 'legacy-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(legacy_title, "do not touch");
}

// ---------------------------------------------------------------------------
// Constraints and relationships
// ---------------------------------------------------------------------------

#[test]
fn foreign_key_violation_is_reported_not_swallowed() {
    let root = TempRoot::new("foreign-key");
    let service = open(&root);
    seed_host(&service);
    let dangling = session("session-dangling", "missing-agent", Some("native-1"));
    let error = service.ensure_session(&dangling).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("foreign key"), "{}", error.message);
    let page = service
        .list_sessions(&SessionQuery {
            workspace_id: None,
            agent_id: None,
            origin: None,
            limit: 10,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
}

#[test]
fn workspace_canonical_path_is_scoped_by_host_and_not_guessed() {
    let root = TempRoot::new("workspace-unique");
    let service = open(&root);
    service.register_host(&Host::local("host-a")).unwrap();
    service.register_host(&Host::local("host-b")).unwrap();

    let alpha_a = workspace("ws-alpha-a", "host-a", "C:\\Work\\Alpha");
    let alpha_b = workspace("ws-alpha-b", "host-b", "C:\\Work\\Alpha");
    service.register_workspace(&alpha_a).unwrap();
    service.register_workspace(&alpha_b).unwrap();

    let duplicate = workspace("ws-alpha-a-2", "host-a", "C:\\Work\\Alpha");
    let error = service.register_workspace(&duplicate).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);

    // An unresolved canonical path is stored as NULL, never guessed, and is
    // therefore not silently merged with another unresolved path.
    let mut unresolved_one = workspace("ws-unresolved-1", "host-a", "X:\\Missing\\One");
    unresolved_one.path.canonical_path = none();
    let mut unresolved_two = workspace("ws-unresolved-2", "host-a", "X:\\Missing\\Two");
    unresolved_two.path.canonical_path = none();
    service.register_workspace(&unresolved_one).unwrap();
    service.register_workspace(&unresolved_two).unwrap();

    let page = service
        .list_workspaces(&PageRequest::new(20, None))
        .unwrap();
    assert_eq!(page.total, 4);
}

#[test]
fn session_native_identity_is_idempotent_and_namespace_scoped() {
    let root = TempRoot::new("session-native");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();

    let first = service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();
    let same_native = service
        .ensure_session(&session("session-2", "codex", Some("native-1")))
        .unwrap();
    assert_eq!(first.id, "session-1");
    assert_eq!(same_native.id, "session-1");

    let mut other_namespace = session("session-3", "codex", Some("native-1"));
    other_namespace.source_namespace = "codex/other-profile/local-windows".to_string();
    let other_namespace = service.ensure_session(&other_namespace).unwrap();
    assert_eq!(other_namespace.id, "session-3");

    let no_native_one = service
        .ensure_session(&session("session-4", "codex", None))
        .unwrap();
    let no_native_two = service
        .ensure_session(&session("session-5", "codex", None))
        .unwrap();
    assert_eq!(no_native_one.id, "session-4");
    assert_eq!(no_native_two.id, "session-5");

    let conflicting = Session {
        source_namespace: "someone-else/default/local-windows".to_string(),
        ..session("session-1", "codex", Some("native-1"))
    };
    assert_eq!(
        service.ensure_session(&conflicting).unwrap_err().code,
        ErrorCode::Conflict
    );

    let page = service
        .list_sessions(&SessionQuery {
            workspace_id: None,
            agent_id: None,
            origin: None,
            limit: 50,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.total, 4);
    let ids = page
        .items
        .iter()
        .map(|item| item.session.id.as_str())
        .collect::<HashSet<_>>();
    assert_eq!(ids.len(), 4);
}

#[test]
fn workspace_and_agent_directions_agree_on_one_session() {
    let root = TempRoot::new("directions");
    let service = open(&root);
    seed_host(&service);
    for (id, vendor, name) in [
        ("codex", "openai", "Codex"),
        ("claude", "anthropic", "Claude Code"),
        ("codebuddy", "tencent", "CodeBuddy"),
    ] {
        service.register_agent(&agent(id, vendor, name)).unwrap();
    }
    service
        .register_workspace(&workspace("ws-a", HOST_ID, "C:\\Work\\Alpha"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-b", HOST_ID, "C:\\Work\\Beta"))
        .unwrap();

    let cross = session("session-cross", "codex", Some("native-cross"));
    service.ensure_session(&cross).unwrap();
    service
        .attach_workspace(&link(
            "session-cross",
            "ws-a",
            SessionWorkspaceRole::Primary,
        ))
        .unwrap();
    service
        .attach_workspace(&link(
            "session-cross",
            "ws-b",
            SessionWorkspaceRole::Related,
        ))
        .unwrap();
    service
        .ensure_session(&session("session-claude", "claude", Some("native-claude")))
        .unwrap();
    service
        .attach_workspace(&link(
            "session-claude",
            "ws-a",
            SessionWorkspaceRole::Primary,
        ))
        .unwrap();
    service
        .ensure_session(&session("session-buddy", "codebuddy", Some("native-buddy")))
        .unwrap();
    service
        .attach_workspace(&link(
            "session-buddy",
            "ws-b",
            SessionWorkspaceRole::Primary,
        ))
        .unwrap();

    for (index, timestamp) in [T1, T2, T3].iter().enumerate() {
        service
            .record_message(&message(
                &format!("m-cross-{index}"),
                "session-cross",
                &format!("session-cross:{index}"),
                Some(&format!("native-cross-{index}")),
                index as u64,
                timestamp,
            ))
            .unwrap();
    }
    service
        .record_message(&message(
            "m-claude-0",
            "session-claude",
            "session-claude:0",
            Some("native-claude-0"),
            0,
            T1,
        ))
        .unwrap();

    let by_workspace = service
        .list_sessions(&SessionQuery {
            workspace_id: Some("ws-a".into()),
            agent_id: None,
            origin: None,
            limit: 20,
            cursor: None,
        })
        .unwrap();
    assert_eq!(by_workspace.total, 2);
    let cross_from_workspace = by_workspace
        .items
        .iter()
        .find(|item| item.session.id == "session-cross")
        .expect("cross-workspace session visible from workspace A");
    assert_eq!(cross_from_workspace.message_count, 3);
    assert_eq!(
        cross_from_workspace.workspace_ids,
        vec!["ws-a".to_string(), "ws-b".to_string()]
    );
    assert_eq!(
        cross_from_workspace.primary_workspace_id.as_deref(),
        Some("ws-a")
    );

    let by_agent = service
        .list_sessions(&SessionQuery {
            workspace_id: None,
            agent_id: Some("codex".into()),
            origin: None,
            limit: 20,
            cursor: None,
        })
        .unwrap();
    assert_eq!(by_agent.total, 1);
    let cross_from_agent = &by_agent.items[0];
    assert_eq!(cross_from_agent.session.id, "session-cross");
    assert_eq!(cross_from_agent.message_count, 3);

    let detail = service.get_session("session-cross").unwrap();
    assert_eq!(detail.session.id, "session-cross");
    assert_eq!(detail.message_count, 3);
    assert_eq!(detail.messages.len(), 3);
    assert_eq!(detail.run_count, 0);
    assert_eq!(detail.workspaces.len(), 2);
    assert_eq!(detail.workspaces[0].role, SessionWorkspaceRole::Primary);
    assert_eq!(detail.workspaces[0].workspace_id, "ws-a");
    let message_ids = detail
        .messages
        .iter()
        .map(|item| item.id.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        message_ids,
        BTreeSet::from(["m-cross-0", "m-cross-1", "m-cross-2"])
    );

    let workspaces = service
        .list_workspaces(&PageRequest::new(20, None))
        .unwrap();
    let counts = workspaces
        .items
        .iter()
        .map(|item| (item.workspace.id.as_str(), item.session_count))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(counts["ws-a"], 2);
    assert_eq!(counts["ws-b"], 2);

    let agents = service.list_agents(&PageRequest::new(20, None)).unwrap();
    let agent_counts = agents
        .items
        .iter()
        .map(|item| (item.agent.id.as_str(), item.session_count))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(agent_counts["codex"], 1);
    assert_eq!(agent_counts["claude"], 1);
    assert_eq!(agent_counts["codebuddy"], 1);
    assert!(agents.items.iter().all(|item| item.installation_count == 0));
}

#[test]
fn session_pagination_is_stable_with_equal_sort_keys() {
    let root = TempRoot::new("session-pagination");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .register_agent(&agent("claude", "anthropic", "Claude"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-cursor-1", HOST_ID, "C:\\Cursor\\One"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-cursor-2", HOST_ID, "C:\\Cursor\\Two"))
        .unwrap();

    let shared_created_at = 1_700_000_000_000_i64;
    for index in 1..=7 {
        service
            .ensure_session_at(
                &session(
                    &format!("s-{index:02}"),
                    "codex",
                    Some(&format!("native-{index}")),
                ),
                shared_created_at,
            )
            .unwrap();
    }
    for index in 1..=3 {
        service
            .ensure_session_at(
                &session(
                    &format!("c-{index:02}"),
                    "claude",
                    Some(&format!("cnative-{index}")),
                ),
                shared_created_at,
            )
            .unwrap();
    }

    let mut cursor = None;
    let mut seen = Vec::new();
    for _ in 0..10 {
        let page = service
            .list_sessions(&SessionQuery {
                workspace_id: None,
                agent_id: Some("codex".into()),
                origin: None,
                limit: 2,
                cursor: cursor.clone(),
            })
            .unwrap();
        assert_eq!(page.total, 7, "total is the filtered count, not page size");
        seen.extend(page.items.iter().map(|item| item.session.id.clone()));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        seen,
        vec!["s-07", "s-06", "s-05", "s-04", "s-03", "s-02", "s-01"]
    );
    assert_eq!(seen.iter().collect::<HashSet<_>>().len(), 7);

    let mut bad_cursor = session_query_with_cursor(
        service
            .list_workspaces(&PageRequest::new(1, None))
            .unwrap()
            .next_cursor
            .expect("workspace cursor"),
    );
    bad_cursor.agent_id = Some("codex".into());
    let error = service.list_sessions(&bad_cursor).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "cursor");
}

fn session_query_with_cursor(cursor: String) -> SessionQuery {
    SessionQuery {
        workspace_id: None,
        agent_id: None,
        origin: None,
        limit: 2,
        cursor: Some(cursor),
    }
}

#[test]
fn join_rows_do_not_inflate_total_or_duplicate_sessions() {
    let root = TempRoot::new("join-total");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-a", HOST_ID, "C:\\Work\\Alpha"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-b", HOST_ID, "C:\\Work\\Beta"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();
    service
        .attach_workspace(&link("session-1", "ws-a", SessionWorkspaceRole::Primary))
        .unwrap();
    service
        .attach_workspace(&link("session-1", "ws-b", SessionWorkspaceRole::Related))
        .unwrap();

    for workspace_id in [Some("ws-a"), Some("ws-b"), None] {
        let page = service
            .list_sessions(&SessionQuery {
                workspace_id: workspace_id.map(str::to_string),
                agent_id: None,
                origin: None,
                limit: 20,
                cursor: None,
            })
            .unwrap();
        assert_eq!(page.total, 1, "workspace filter {workspace_id:?}");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].session.id, "session-1");
        assert_eq!(page.items[0].workspace_ids.len(), 2);
    }
}

#[test]
fn workspace_and_agent_lists_paginate_without_loss() {
    let root = TempRoot::new("list-pagination");
    let service = open(&root);
    for host in ["host-a", "host-b"] {
        service.register_host(&Host::local(host)).unwrap();
    }
    service
        .register_workspace(&workspace("ws-a", "host-a", "C:\\A"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-b", "host-a", "C:\\B"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-c", "host-b", "C:\\C"))
        .unwrap();
    service.register_agent(&agent("a-1", "v", "Alpha")).unwrap();
    service.register_agent(&agent("a-2", "v", "Beta")).unwrap();
    service.register_agent(&agent("a-3", "v", "Gamma")).unwrap();
    service
        .register_installation(&AgentInstallation {
            id: "install-a-1".to_string(),
            host_id: "host-a".to_string(),
            agent_id: "a-1".to_string(),
            executable: HostPath {
                host_id: "host-a".to_string(),
                display_path: "C:\\Tools\\a1.cmd".to_string(),
                canonical_path: none(),
            },
            version: some("1.0.0".to_string()),
            config_profile_id: "default".to_string(),
        })
        .unwrap();

    let first = service.list_workspaces(&PageRequest::new(2, None)).unwrap();
    assert_eq!(first.total, 3);
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.workspace.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ws-a", "ws-b"]
    );
    let second = service
        .list_workspaces(&PageRequest::new(2, first.next_cursor))
        .unwrap();
    assert_eq!(second.total, 3);
    assert_eq!(
        second
            .items
            .iter()
            .map(|item| item.workspace.id.as_str())
            .collect::<Vec<_>>(),
        vec!["ws-c"]
    );
    assert!(second.next_cursor.is_none());

    let first = service.list_agents(&PageRequest::new(2, None)).unwrap();
    assert_eq!(first.total, 3);
    assert_eq!(
        first
            .items
            .iter()
            .map(|item| item.agent.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-1", "a-2"]
    );
    assert_eq!(first.items[0].installation_count, 1);
    assert_eq!(first.items[1].installation_count, 0);
    let second = service
        .list_agents(&PageRequest::new(2, first.next_cursor))
        .unwrap();
    assert_eq!(
        second
            .items
            .iter()
            .map(|item| item.agent.id.as_str())
            .collect::<Vec<_>>(),
        vec!["a-3"]
    );
    assert!(second.next_cursor.is_none());
}

#[test]
fn installation_identity_conflicts_are_rejected_without_writes() {
    let root = TempRoot::new("installation-conflict");
    let service = open(&root);
    for host in ["host-a", "host-b"] {
        service.register_host(&Host::local(host)).unwrap();
    }
    for agent_id in ["agent-a", "agent-b"] {
        service
            .register_agent(&agent(agent_id, "synthetic", agent_id))
            .unwrap();
    }

    let original = installation("same-id", "agent-a", "host-a", "default");
    service.register_installation(&original).unwrap();

    let conflicts = [
        installation("same-id", "agent-b", "host-b", "default"),
        installation("same-id", "agent-a", "host-b", "default"),
        installation("same-id", "agent-a", "host-a", "other-profile"),
        installation("other-id", "agent-a", "host-a", "default"),
    ];
    for candidate in conflicts {
        let error = service.register_installation(&candidate).unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::Conflict,
            "candidate {} must conflict",
            candidate.id
        );
    }
    // Exact same id + identity remains idempotent.
    let stored = service.register_installation(&original).unwrap();
    assert_eq!(stored.id, "same-id");
    assert_eq!(stored.agent_id, "agent-a");
    assert_eq!(stored.host_id, "host-a");

    let (count, agent_id, host_id, profile): (i64, String, String, String) = service
        .lock_for_test()
        .query_row(
            "SELECT COUNT(*), MIN(agent_id), MIN(host_id), MIN(config_profile_id)
             FROM agent_installations",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(count, 1);
    assert_eq!(agent_id, "agent-a");
    assert_eq!(host_id, "host-a");
    assert_eq!(profile, "default");
}

#[test]
fn runs_are_traceable_and_status_axes_stay_separate() {
    let root = TempRoot::new("runs");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();

    let mut first = sample_run();
    first.id = "run-1".to_string();
    first.session_id = "session-1".to_string();
    first.installation_id = none();
    first.execution_status = ExecutionStatus::Starting;
    first.interaction_status = InteractionStatus::Working;
    service.start_run(&first).unwrap();

    let mut second = sample_run();
    second.id = "run-2".to_string();
    second.session_id = "session-1".to_string();
    second.installation_id = none();
    second.execution_status = ExecutionStatus::Exited;
    second.interaction_status = InteractionStatus::Idle;
    second.ended_at = some(T2.to_string());
    service.start_run(&second).unwrap();

    let detail = service.get_session("session-1").unwrap();
    assert_eq!(detail.run_count, 2);
    assert_eq!(detail.runs.len(), 2);
    assert_eq!(detail.runs[0].execution_status, ExecutionStatus::Starting);
    assert_eq!(
        detail.runs[0].interaction_status,
        InteractionStatus::Working
    );
    assert_eq!(detail.runs[1].execution_status, ExecutionStatus::Exited);
    assert_eq!(detail.runs[1].interaction_status, InteractionStatus::Idle);
    assert_eq!(detail.messages.len(), 0);

    // Exact same run identity is idempotent; a different ownership reusing the
    // id is a conflict and must not add or mutate a row.
    assert_eq!(service.start_run(&first).unwrap().id, "run-1");
    let mut conflicting = first.clone();
    conflicting.ownership = RunOwnership::ExternalObserved;
    let error = service.start_run(&conflicting).unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert_eq!(service.get_session("session-1").unwrap().runs.len(), 2);

    let mut orphan = second;
    orphan.id = "run-orphan".to_string();
    orphan.session_id = "missing-session".to_string();
    assert_eq!(
        service.start_run(&orphan).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn process_identity_includes_creation_time_and_keeps_first_observation() {
    let root = TempRoot::new("processes");
    let service = open(&root);
    seed_host(&service);

    let mut first = sample_process();
    first.identity.creation_time = T1.to_string();
    first.observed_at = T1.to_string();
    first.redacted_command = some("codex --version".to_string());
    service.record_process(&first).unwrap();

    let mut second = sample_process();
    second.identity.creation_time = T2.to_string();
    second.observed_at = T2.to_string();
    second.redacted_command = some("codex exec".to_string());
    service.record_process(&second).unwrap();

    let stored_first = service
        .get_process(&first.identity)
        .unwrap()
        .expect("first process observation");
    assert_eq!(stored_first.observed_at, T1);
    assert_eq!(
        stored_first.redacted_command.as_deref(),
        Some("codex --version")
    );
    let stored_second = service
        .get_process(&second.identity)
        .unwrap()
        .expect("second process observation");
    assert_eq!(stored_second.observed_at, T2);
    assert_ne!(
        stored_first.identity.creation_time,
        stored_second.identity.creation_time
    );

    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();
    let mut run = sample_run();
    run.id = "run-1".to_string();
    run.session_id = "session-1".to_string();
    run.installation_id = none();
    service.start_run(&run).unwrap();
    service
        .link_process(&RunProcessLink {
            run_id: "run-1".to_string(),
            process_identity: first.identity.clone(),
            link_kind: LinkKind::Owned,
            evidence: "spawn handle".to_string(),
        })
        .unwrap();
    service
        .link_process(&RunProcessLink {
            run_id: "run-1".to_string(),
            process_identity: second.identity.clone(),
            link_kind: LinkKind::Observed,
            evidence: "process scan".to_string(),
        })
        .unwrap();
    let links = service.links_for_run("run-1").unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(
        links
            .iter()
            .map(|link| link.link_kind)
            .collect::<HashSet<_>>()
            .len(),
        2
    );
}

#[test]
fn external_observed_run_is_never_rewritten_as_managed() {
    let root = TempRoot::new("external");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("claude", "anthropic", "Claude"))
        .unwrap();
    service
        .ensure_session(&session("session-ext", "claude", None))
        .unwrap();

    let mut external = sample_run();
    external.id = "run-external".to_string();
    external.session_id = "session-ext".to_string();
    external.installation_id = none();
    external.transport = RunTransport::Native;
    external.ownership = RunOwnership::ExternalObserved;
    external.execution_status = ExecutionStatus::Unknown;
    external.interaction_status = InteractionStatus::Unknown;
    external.evidence_level = EvidenceLevel::Inferred;
    external.evidence = some("process observation".to_string());
    service.start_run(&external).unwrap();

    let mut process = sample_process();
    process.identity.pid = 7777;
    service.record_process(&process).unwrap();
    service
        .link_process(&RunProcessLink {
            run_id: "run-external".to_string(),
            process_identity: process.identity.clone(),
            link_kind: LinkKind::Observed,
            evidence: "process scan".to_string(),
        })
        .unwrap();
    let mut inferred_process = sample_process();
    inferred_process.identity.pid = 7778;
    inferred_process.identity.creation_time = T2.to_string();
    service.record_process(&inferred_process).unwrap();
    service
        .link_process(&RunProcessLink {
            run_id: "run-external".to_string(),
            process_identity: inferred_process.identity.clone(),
            link_kind: LinkKind::Inferred,
            evidence: "parent tree inference".to_string(),
        })
        .unwrap();
    // A later claim of ownership must be rejected and must not overwrite the
    // existing observed/inferred links or the run's ownership.
    let error = service
        .link_process(&RunProcessLink {
            run_id: "run-external".to_string(),
            process_identity: process.identity.clone(),
            link_kind: LinkKind::Owned,
            evidence: "conflicting claim".to_string(),
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AccessDenied);
    let links = service.links_for_run("run-external").unwrap();
    assert_eq!(links.len(), 2);
    assert_eq!(
        links
            .iter()
            .map(|link| link.link_kind)
            .collect::<HashSet<_>>(),
        HashSet::from([LinkKind::Observed, LinkKind::Inferred])
    );
    assert!(links
        .iter()
        .all(|link| link.evidence != "conflicting claim"));

    let detail = service.get_session("session-ext").unwrap();
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(detail.runs[0].ownership, RunOwnership::ExternalObserved);
    assert_eq!(detail.runs[0].transport, RunTransport::Native);
    assert_eq!(detail.runs[0].execution_status, ExecutionStatus::Unknown);
    assert_eq!(
        detail.runs[0].interaction_status,
        InteractionStatus::Unknown
    );
    assert_eq!(detail.runs[0].evidence_level, EvidenceLevel::Inferred);
    assert_eq!(
        detail.runs[0].evidence.as_deref(),
        Some("process observation")
    );
}

#[test]
fn external_observed_run_cannot_claim_owned_link_on_first_insert() {
    let root = TempRoot::new("external-first-owned");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("claude", "anthropic", "Claude"))
        .unwrap();
    service
        .ensure_session(&session("session-ext", "claude", None))
        .unwrap();
    let mut external = sample_run();
    external.id = "run-ext-first".to_string();
    external.session_id = "session-ext".to_string();
    external.installation_id = none();
    external.ownership = RunOwnership::ExternalObserved;
    external.execution_status = ExecutionStatus::Unknown;
    external.interaction_status = InteractionStatus::Unknown;
    service.start_run(&external).unwrap();
    let process = sample_process();
    service.record_process(&process).unwrap();

    let error = service
        .link_process(&RunProcessLink {
            run_id: "run-ext-first".to_string(),
            process_identity: process.identity,
            link_kind: LinkKind::Owned,
            evidence: "first insert must be rejected".to_string(),
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AccessDenied);
    assert!(service.links_for_run("run-ext-first").unwrap().is_empty());
    let detail = service.get_session("session-ext").unwrap();
    assert_eq!(detail.runs.len(), 1);
    assert_eq!(detail.runs[0].ownership, RunOwnership::ExternalObserved);
    let link_rows: i64 = service
        .lock_for_test()
        .query_row("SELECT COUNT(*) FROM run_process_links", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(link_rows, 0);
}

#[test]
fn capability_status_and_evidence_are_independent() {
    let root = TempRoot::new("capabilities");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .register_agent(&agent("claude", "anthropic", "Claude"))
        .unwrap();

    let mut supported = sample_capability();
    supported.agent_id = "codex".to_string();
    supported.installation_id = none();
    supported.name = CapabilityName::StructuredChat;
    supported.status = CapabilityStatus::Supported;
    supported.evidence_level = CapabilityEvidenceLevel::Document;
    supported.source = "synthetic documentation fixture".to_string();
    supported.reason = some("synthetic only; not a real CLI probe".to_string());
    service.declare_capability(&supported).unwrap();

    let mut unsupported = supported.clone();
    unsupported.name = CapabilityName::QuotaRead;
    unsupported.status = CapabilityStatus::Unsupported;
    unsupported.evidence_level = CapabilityEvidenceLevel::Document;
    unsupported.reason = some("synthetic absence fixture; no official quota API".to_string());
    unsupported.verified_at = none();
    service.declare_capability(&unsupported).unwrap();

    let mut unverified = supported.clone();
    unverified.name = CapabilityName::PermissionFlow;
    unverified.status = CapabilityStatus::Unverified;
    unverified.evidence_level = CapabilityEvidenceLevel::Fixture;
    unverified.reason = some("synthetic fixture only; live round trip NOT_RUN".to_string());
    service.declare_capability(&unverified).unwrap();

    let mut live_label = supported.clone();
    live_label.name = CapabilityName::HistoryRead;
    live_label.status = CapabilityStatus::Unverified;
    live_label.evidence_level = CapabilityEvidenceLevel::Live;
    live_label.source = "synthetic live-label round trip; not a real CLI probe".to_string();
    live_label.reason = some("synthetic only; no real CLI was probed".to_string());
    service.declare_capability(&live_label).unwrap();

    let mut other_agent = supported.clone();
    other_agent.agent_id = "claude".to_string();
    other_agent.evidence_level = CapabilityEvidenceLevel::Document;
    service.declare_capability(&other_agent).unwrap();

    let capabilities = service.list_capabilities("codex").unwrap();
    assert_eq!(capabilities.len(), 4);
    let by_name = capabilities
        .iter()
        .map(|item| (item.name.as_str(), item))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(
        by_name["structuredChat"].status,
        CapabilityStatus::Supported
    );
    assert_eq!(
        by_name["structuredChat"].evidence_level,
        CapabilityEvidenceLevel::Document
    );
    assert_eq!(by_name["quotaRead"].status, CapabilityStatus::Unsupported);
    assert_eq!(
        by_name["quotaRead"].evidence_level,
        CapabilityEvidenceLevel::Document
    );
    assert_eq!(
        by_name["permissionFlow"].status,
        CapabilityStatus::Unverified
    );
    assert_eq!(
        by_name["permissionFlow"].evidence_level,
        CapabilityEvidenceLevel::Fixture
    );
    assert_eq!(by_name["historyRead"].status, CapabilityStatus::Unverified);
    assert_eq!(
        by_name["historyRead"].evidence_level,
        CapabilityEvidenceLevel::Live
    );
    assert!(by_name
        .values()
        .all(|item| item.source.contains("synthetic") || item.source.contains("fixture")));
    assert_eq!(service.list_capabilities("claude").unwrap().len(), 1);
}

#[test]
fn messages_deduplicate_by_source_key_and_native_event() {
    let root = TempRoot::new("messages");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();

    let first = message("m-1", "session-1", "session-1:0", Some("native-0"), 0, T1);
    assert_eq!(service.record_message(&first).unwrap().id, "m-1");

    let duplicate_source = message(
        "m-2",
        "session-1",
        "session-1:0",
        Some("native-other"),
        1,
        T2,
    );
    assert_eq!(service.record_message(&duplicate_source).unwrap().id, "m-1");

    let duplicate_native = message(
        "m-3",
        "session-1",
        "session-1:other",
        Some("native-0"),
        2,
        T3,
    );
    assert_eq!(service.record_message(&duplicate_native).unwrap().id, "m-1");

    let fresh = message(
        "m-4",
        "session-1",
        "session-1:other",
        Some("native-1"),
        3,
        T3,
    );
    assert_eq!(service.record_message(&fresh).unwrap().id, "m-4");

    let detail = service.get_session("session-1").unwrap();
    assert_eq!(detail.message_count, 2);
    assert_eq!(detail.messages.len(), 2);
    assert_eq!(
        detail
            .messages
            .iter()
            .map(|item| item.id.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["m-1", "m-4"])
    );

    let mut invalid = fresh;
    invalid.id = "m-5".to_string();
    invalid.source_key = "session-1:invalid".to_string();
    invalid.content = json!("not an object");
    assert_eq!(
        service.record_message(&invalid).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn message_run_must_belong_to_the_same_session() {
    let root = TempRoot::new("message-run-owner");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    for id in ["s-a", "s-b"] {
        service
            .ensure_session(&session(id, "codex", Some(&format!("native-{id}"))))
            .unwrap();
    }
    service
        .start_run(&run_for("r-b", "s-b", None, RunOwnership::Managed))
        .unwrap();

    let error = service
        .record_message(&message_for("m-cross", "s-a", Some("r-b")))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "runId");
    assert_eq!(service.get_session("s-a").unwrap().messages.len(), 0);
    let rows: i64 = service
        .lock_for_test()
        .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 0);

    // Legal null and same-session run remain usable.
    service
        .record_message(&message_for("m-null", "s-a", None))
        .unwrap();
    service
        .start_run(&run_for("r-a", "s-a", None, RunOwnership::Managed))
        .unwrap();
    service
        .record_message(&message_for("m-same", "s-a", Some("r-a")))
        .unwrap();
    let error = service
        .record_message(&message_for("m-missing", "s-a", Some("r-missing")))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "runId");
    assert_eq!(service.get_session("s-a").unwrap().messages.len(), 2);
}

#[test]
fn run_installation_must_match_the_session_agent_and_host() {
    let root = TempRoot::new("run-installation-owner");
    let service = open(&root);
    for host in ["host-a", "host-b"] {
        service.register_host(&Host::local(host)).unwrap();
    }
    for agent_id in ["agent-a", "agent-b"] {
        service
            .register_agent(&agent(agent_id, "synthetic", agent_id))
            .unwrap();
    }
    service
        .register_installation(&installation("i-a", "agent-a", "host-a", "default"))
        .unwrap();
    service
        .register_installation(&installation("i-b", "agent-b", "host-b", "default"))
        .unwrap();
    service
        .register_installation(&installation("i-a-hostb", "agent-a", "host-b", "default"))
        .unwrap();
    service
        .ensure_session(&session_on("s-a", "agent-a", "host-a"))
        .unwrap();
    service
        .ensure_session(&session_on("s-b", "agent-b", "host-b"))
        .unwrap();

    let error = service
        .start_run(&run_for(
            "r-cross",
            "s-a",
            Some("i-b"),
            RunOwnership::Managed,
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "installationId");
    assert!(service.get_session("s-a").unwrap().runs.is_empty());

    // Same agent/host installation and null installation are legal.
    service
        .start_run(&run_for(
            "r-same",
            "s-a",
            Some("i-a"),
            RunOwnership::Managed,
        ))
        .unwrap();
    service
        .start_run(&run_for("r-null", "s-a", None, RunOwnership::Managed))
        .unwrap();
    let error = service
        .start_run(&run_for(
            "r-wrong-host",
            "s-b",
            Some("i-a"),
            RunOwnership::Managed,
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // Host-only mismatch: same agent, different host.
    let error = service
        .start_run(&run_for(
            "r-host-only",
            "s-a",
            Some("i-a-hostb"),
            RunOwnership::Managed,
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "installationId");
    // Agent-only mismatch: same host, different agent.
    let error = service
        .start_run(&run_for(
            "r-agent-only",
            "s-b",
            Some("i-a-hostb"),
            RunOwnership::Managed,
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "installationId");
    assert!(service.get_session("s-b").unwrap().runs.is_empty());
    let error = service
        .start_run(&run_for(
            "r-missing-install",
            "s-a",
            Some("i-missing"),
            RunOwnership::Managed,
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(service.get_session("s-a").unwrap().runs.len(), 2);
}

#[test]
fn capability_installation_must_match_the_agent() {
    let root = TempRoot::new("capability-installation-owner");
    let service = open(&root);
    for host in ["host-a", "host-b"] {
        service.register_host(&Host::local(host)).unwrap();
    }
    for agent_id in ["agent-a", "agent-b"] {
        service
            .register_agent(&agent(agent_id, "synthetic", agent_id))
            .unwrap();
    }
    service
        .register_installation(&installation("i-a", "agent-a", "host-a", "default"))
        .unwrap();
    service
        .register_installation(&installation("i-b", "agent-b", "host-b", "default"))
        .unwrap();

    let error = service
        .declare_capability(&capability_for(
            "agent-a",
            Some("i-b"),
            CapabilityName::StructuredChat,
        ))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.details["field"], "installationId");
    assert!(service.list_capabilities("agent-a").unwrap().is_empty());

    // Same-agent installation and null installation are legal.
    service
        .declare_capability(&capability_for(
            "agent-a",
            Some("i-a"),
            CapabilityName::StructuredChat,
        ))
        .unwrap();
    service
        .declare_capability(&capability_for(
            "agent-a",
            None,
            CapabilityName::PermissionFlow,
        ))
        .unwrap();
    // An invalid update must not mutate the existing same-agent capability.
    let mut conflicting_update =
        capability_for("agent-a", Some("i-b"), CapabilityName::StructuredChat);
    conflicting_update.status = CapabilityStatus::Unsupported;
    conflicting_update.source = "must not persist".to_string();
    assert_eq!(
        service
            .declare_capability(&conflicting_update)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let stored = service.list_capabilities("agent-a").unwrap();
    let structured = stored
        .iter()
        .find(|capability| capability.name == CapabilityName::StructuredChat)
        .expect("structuredChat remains");
    assert_eq!(structured.installation_id.as_deref(), Some("i-a"));
    assert_eq!(structured.status, CapabilityStatus::Supported);
    assert_eq!(structured.source, "synthetic");
    assert_eq!(stored.len(), 2);
}

#[test]
fn session_workspace_has_exactly_one_primary() {
    let root = TempRoot::new("primary");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-a", HOST_ID, "C:\\Work\\Alpha"))
        .unwrap();
    service
        .register_workspace(&workspace("ws-b", HOST_ID, "C:\\Work\\Beta"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();

    service
        .attach_workspace(&link("session-1", "ws-a", SessionWorkspaceRole::Primary))
        .unwrap();
    service
        .attach_workspace(&link("session-1", "ws-a", SessionWorkspaceRole::Primary))
        .unwrap();
    service
        .attach_workspace(&link("session-1", "ws-a", SessionWorkspaceRole::Related))
        .unwrap();
    service
        .attach_workspace(&link("session-1", "ws-b", SessionWorkspaceRole::Primary))
        .unwrap();
    let error = service
        .attach_workspace(&link("session-1", "ws-a", SessionWorkspaceRole::Primary))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);

    let detail = service.get_session("session-1").unwrap();
    assert_eq!(detail.workspaces.len(), 2);
    assert_eq!(detail.workspaces[0].workspace_id, "ws-b");
    assert_eq!(detail.workspaces[0].role, SessionWorkspaceRole::Primary);
    assert_eq!(detail.workspaces[1].workspace_id, "ws-a");
    assert_eq!(detail.workspaces[1].role, SessionWorkspaceRole::Related);
}

#[test]
fn session_relation_validates_confidence_and_identity() {
    let root = TempRoot::new("relations");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    for id in ["session-1", "session-2"] {
        service
            .ensure_session(&session(id, "codex", Some(&format!("native-{id}"))))
            .unwrap();
    }
    let relation = SessionRelation {
        parent_session_id: "session-1".to_string(),
        child_session_id: "session-2".to_string(),
        kind: SessionRelationKind::ExplicitDelegation,
        confidence: 0.85,
        evidence: "explicit tool delegation".to_string(),
    };
    service.relate_sessions(&relation).unwrap();
    service.relate_sessions(&relation).unwrap();

    let mut invalid = relation.clone();
    invalid.confidence = 1.5;
    assert_eq!(
        service.relate_sessions(&invalid).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let mut self_relation = relation;
    self_relation.child_session_id = "session-1".to_string();
    assert_eq!(
        service.relate_sessions(&self_relation).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let count: i64 = service
        .lock_for_test()
        .query_row("SELECT COUNT(*) FROM session_relations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
}

// ---------------------------------------------------------------------------
// Migration failure and recovery
// ---------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn data_root_junction_to_legacy_is_rejected_before_writes() {
    let root = TempRoot::new("data-root-junction");
    let legacy = root.path.join(".ccgui-next");
    fs::create_dir_all(&legacy).unwrap();
    let alias = root.path.join("alias");
    create_junction(&alias, &legacy);

    let error = match MultiCliService::open(&alias) {
        Ok(_) => panic!("legacy junction alias must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(!legacy.join("multicli.db").exists());
    assert!(!alias.join("multicli.db").exists());

    let nested = alias.join("nested");
    let error = match MultiCliService::open(&nested) {
        Ok(_) => panic!("nested alias under a legacy junction must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(!legacy.join("nested").join("multicli.db").exists());

    // A junction to an ordinary directory is still a normal synthetic data
    // root; the guard must reject the legacy target, not all reparse points.
    let real = root.path.join("real");
    fs::create_dir_all(&real).unwrap();
    let normal_alias = root.path.join("normal-alias");
    create_junction(&normal_alias, &real);
    let service = MultiCliService::open(&normal_alias).expect("ordinary junction is allowed");
    assert!(real.join("multicli.db").exists());
    drop(service);
    fs::remove_dir(&normal_alias).unwrap();
    fs::remove_dir(&alias).unwrap();
}

#[test]
fn migration_failure_preserves_version_notes_tags_and_backup() {
    let root = TempRoot::new("migration-failure");
    let db_path = root.path.join("multicli.db");

    // Build a real v1 database with product data.
    {
        let service = MultiCliService::open_for_test(
            &root.path,
            MigrationFault {
                stop_after_version: Some(1),
                ..Default::default()
            },
        )
        .expect("open v1 database");
        assert_eq!(service.schema_version(), 1);
        seed_host(&service);
        service
            .register_agent(&agent("codex", "openai", "Codex"))
            .unwrap();
        service
            .register_workspace(&workspace("ws-1", HOST_ID, "C:\\Work\\Alpha"))
            .unwrap();
        service
            .ensure_session(&session("session-1", "codex", Some("native-1")))
            .unwrap();
        service
            .attach_workspace(&link("session-1", "ws-1", SessionWorkspaceRole::Primary))
            .unwrap();
        service
            .record_message(&message(
                "m-1",
                "session-1",
                "session-1:0",
                Some("native-0"),
                0,
                T1,
            ))
            .unwrap();
    }
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO user_session_notes(session_id, note, updated_at_ms) VALUES (?1, ?2, ?3)",
            params![
                "session-1",
                "user note that must survive",
                1_700_000_000_000_i64
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO user_session_tags(session_id, tag, created_at_ms) VALUES (?1, ?2, ?3)",
            params!["session-1", "keep-me", 1_700_000_000_000_i64],
        )
        .unwrap();
    }

    // Fail after the first v2 statement has run; the transaction must roll
    // back, leaving version 1 and every user row untouched.
    let error = match MultiCliService::open_for_test(
        &root.path,
        MigrationFault {
            fail_at: Some((2, 1)),
            ..Default::default()
        },
    ) {
        Ok(_) => panic!("injected migration failure did not fail"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Database);
    assert_eq!(error.details["stage"], "migration");
    assert_eq!(error.details["fromVersion"], 1);
    assert_eq!(error.details["failedVersion"], 2);
    assert_eq!(error.details["databaseVersion"], 1);
    let backup_path = PathBuf::from(
        error.details["backupPath"]
            .as_str()
            .expect("backup path in error details"),
    );
    assert!(backup_path.exists(), "consistent backup must exist");
    let manifest_path = backup_path.with_extension("json");
    assert!(manifest_path.exists());
    assert!(
        db_path.exists(),
        "failed migration must not delete the database"
    );

    let index_count: i64 = {
        let conn = Connection::open(&db_path).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 1, "failed migration must not advance the version");
        let note: String = conn
            .query_row(
                "SELECT note FROM user_session_notes WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note, "user note that must survive");
        let tag: String = conn
            .query_row(
                "SELECT tag FROM user_session_tags WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag, "keep-me");
        let message_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))
            .unwrap();
        assert_eq!(message_count, 1);
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'ix_messages_session_order'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        index_count
    };
    assert_eq!(
        index_count, 0,
        "partially created v2 index must be rolled back"
    );

    // The backup is a readable, consistent v1 snapshot with the same rows.
    {
        let backup = Connection::open(&backup_path).unwrap();
        let version: i64 = backup
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 1);
        let note: String = backup
            .query_row(
                "SELECT note FROM user_session_notes WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note, "user note that must survive");
        let tag: String = backup
            .query_row(
                "SELECT tag FROM user_session_tags WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag, "keep-me");
    }
    export_evidence(
        "s01-01-migration-failure",
        &[&db_path, &backup_path, &manifest_path],
    );

    // A normal reopen completes v1 -> v2 without losing user data.
    {
        let service = open(&root);
        assert_eq!(service.schema_version(), SCHEMA_VERSION);
        assert_eq!(service.get_session("session-1").unwrap().message_count, 1);
        let conn = service.lock_for_test();
        let note: String = conn
            .query_row(
                "SELECT note FROM user_session_notes WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(note, "user note that must survive");
        let tag: String = conn
            .query_row(
                "SELECT tag FROM user_session_tags WHERE session_id = 'session-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tag, "keep-me");
        let index_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'ix_messages_session_order'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_count, 1);
    }
    export_evidence("s01-01-migration-recovered", &[&db_path]);
}

#[test]
fn future_database_is_rejected_without_bytes_mode_or_data_changes() {
    let root = TempRoot::new("future-delete-journal");
    let db = root.path.join("multicli.db");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=DELETE;
             CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY);
             INSERT INTO schema_migrations(version) VALUES (999);
             CREATE TABLE user_notes(note TEXT);
             INSERT INTO user_notes(note) VALUES ('keep');",
        )
        .unwrap();
    }
    let before = fs::read(&db).unwrap();
    let error = match MultiCliService::open(&root.path) {
        Ok(_) => panic!("future database must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Database);
    assert_eq!(error.details["databaseVersion"], 999);
    let after = fs::read(&db).unwrap();
    assert_eq!(before, after, "rejection must not change database bytes");
    assert!(!root.path.join("multicli.db-wal").exists());
    assert!(!root.path.join("multicli.db-shm").exists());
    assert!(!root.path.join("backups").exists());

    let conn = Connection::open(&db).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_ascii_lowercase(), "delete");
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 999);
    let note: String = conn
        .query_row("SELECT note FROM user_notes", [], |row| row.get(0))
        .unwrap();
    assert_eq!(note, "keep");
    let violations: i64 = conn
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(violations, 0);
}

#[test]
fn newer_database_version_is_refused_without_writes() {
    let root = TempRoot::new("newer-version");
    let db_path;
    {
        let service = open(&root);
        seed_host(&service);
        service
            .register_agent(&agent("codex", "openai", "Codex"))
            .unwrap();
        service
            .ensure_session(&session("session-1", "codex", Some("native-1")))
            .unwrap();
        db_path = service.database_path().to_path_buf();
        drop(service);
    }
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO user_session_notes(session_id, note, updated_at_ms) VALUES (?1, ?2, ?3)",
            params!["session-1", "future note", 1_700_000_000_000_i64],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO schema_migrations(version, description, applied_at_ms)
             VALUES (999, 'future schema', 0)",
            [],
        )
        .unwrap();
    }
    let error = match MultiCliService::open(&root.path) {
        Ok(_) => panic!("newer schema must be refused"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Database);
    assert_eq!(error.details["databaseVersion"], 999);
    assert_eq!(error.details["supportedVersion"], SCHEMA_VERSION);
    let conn = Connection::open(&db_path).unwrap();
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(version, 999);
    let note: String = conn
        .query_row(
            "SELECT note FROM user_session_notes WHERE session_id = 'session-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(note, "future note");
}

#[test]
fn list_failure_is_an_error_not_an_empty_page() {
    let root = TempRoot::new("list-failure");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();
    service
        .execute_raw("PRAGMA foreign_keys = OFF; DROP TABLE messages; DROP TABLE sessions;")
        .unwrap();
    let error = service
        .list_sessions(&SessionQuery {
            workspace_id: None,
            agent_id: None,
            origin: None,
            limit: 10,
            cursor: None,
        })
        .expect_err("missing table must not become an empty page");
    assert_eq!(error.code, ErrorCode::Database);
    assert!(error.message.contains("sessions"), "{}", error.message);
}

#[test]
fn filter_by_unknown_workspace_and_agent_is_a_valid_empty_page() {
    let root = TempRoot::new("empty-filter");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "openai", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", Some("native-1")))
        .unwrap();
    let page = service
        .list_sessions(&SessionQuery {
            workspace_id: Some("no-such-workspace".into()),
            agent_id: None,
            origin: None,
            limit: 10,
            cursor: None,
        })
        .unwrap();
    assert_eq!(page.total, 0);
    assert!(page.items.is_empty());
}

fn export_evidence(test_name: &str, files: &[&Path]) {
    let Ok(directory) = std::env::var("MULTICLI_S01_EVIDENCE_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory).expect("create evidence directory");
    for file in files {
        let Some(name) = file.file_name() else {
            continue;
        };
        let target = directory.join(format!("{test_name}-{}", name.to_string_lossy()));
        fs::copy(file, target).expect("copy evidence artifact");
    }
}

#[test]
fn installation_executable_host_mismatch_is_rejected_without_insertion() {
    let root = TempRoot::new("sol-installation-host");
    let service = open(&root);
    seed_host(&service);
    service
        .register_agent(&agent("codex", "synthetic", "Codex"))
        .unwrap();
    let mut installation = sample_installation();
    installation.executable.host_id = "other-host".into();
    assert_eq!(
        service
            .register_installation(&installation)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let count: i64 = service
        .lock_for_test()
        .query_row("SELECT COUNT(*) FROM agent_installations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    installation.executable.host_id = HOST_ID.into();
    assert_eq!(
        service.register_installation(&installation).unwrap(),
        installation
    );
}

#[test]
fn cross_host_workspace_link_is_rejected_without_partial_write() {
    let root = TempRoot::new("sol-workspace-host");
    let service = open(&root);
    seed_host(&service);
    service.register_host(&Host::local("other-host")).unwrap();
    service
        .register_agent(&agent("codex", "synthetic", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("s", "codex", None))
        .unwrap();
    service
        .register_workspace(&workspace("foreign", "other-host", "D:\\fixture"))
        .unwrap();
    assert_eq!(
        service
            .attach_workspace(&link("s", "foreign", SessionWorkspaceRole::Primary))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert!(service.get_session("s").unwrap().workspaces.is_empty());
    service
        .register_workspace(&workspace("local", HOST_ID, "D:\\fixture"))
        .unwrap();
    service
        .attach_workspace(&link("s", "local", SessionWorkspaceRole::Primary))
        .unwrap();
    assert_eq!(
        service.get_session("s").unwrap().workspaces[0].workspace_id,
        "local"
    );
}

#[test]
fn cross_host_process_link_is_rejected_without_claiming_the_process() {
    let root = TempRoot::new("sol-process-host");
    let service = open(&root);
    seed_host(&service);
    service.register_host(&Host::local("other-host")).unwrap();
    service
        .register_agent(&agent("codex", "synthetic", "Codex"))
        .unwrap();
    service
        .ensure_session(&session("session-1", "codex", None))
        .unwrap();
    let mut run = sample_run();
    run.session_id = "session-1".into();
    run.installation_id = None;
    service.start_run(&run).unwrap();
    let mut process = sample_process();
    process.identity.host_id = "other-host".into();
    process.parent_identity = None;
    process.executable = None;
    service.record_process(&process).unwrap();
    let mut association = RunProcessLink {
        run_id: run.id.clone(),
        process_identity: process.identity.clone(),
        link_kind: LinkKind::Owned,
        evidence: "synthetic".into(),
    };
    assert_eq!(
        service.link_process(&association).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert!(service.links_for_run(&run.id).unwrap().is_empty());
    process.identity.host_id = HOST_ID.into();
    service.record_process(&process).unwrap();
    association.process_identity = process.identity;
    assert_eq!(service.link_process(&association).unwrap(), association);
}

#[test]
fn unknown_nonempty_database_is_rejected_without_changing_its_bytes() {
    let root = TempRoot::new("sol-unknown-db");
    let path = root.path.join("multicli.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE unrelated_notes(note TEXT); INSERT INTO unrelated_notes VALUES ('keep');",
        )
        .unwrap();
    }
    let before = fs::read(&path).unwrap();
    let error = match MultiCliService::open(&root.path) {
        Ok(_) => panic!("foreign database was adopted"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Database);
    assert_eq!(error.details["reason"], "unrecognizedDatabase");
    assert_eq!(before, fs::read(&path).unwrap());
    assert!(!root.path.join("multicli.db-wal").exists());
    assert!(!root.path.join("backups").exists());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        conn.query_row("SELECT note FROM unrelated_notes", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "keep"
    );
}
