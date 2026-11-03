//! Discovery coordinator: background sampling, caching, IPC read models.
//!
//! One engine owns one background thread. The thread samples at the configured
//! interval (default 3 s, bounded 1–60 s), and a manual refresh either joins the
//! in-flight scan or wakes the thread for one immediate scan — it never spawns
//! a second scanner. Stopping the engine ends only its own thread; external
//! CLIs are never signalled, attached to or killed.
//!
//! Installation detection is cached: a static install is not re-probed on every
//! process sample, and version/help is only run for entries that were already
//! recognised as trusted.

use super::classify::{self, InstallationIndex};
use super::detect::{
    scan_installations, DetectedInstallation, InstallStatus, ScannerEnvironment,
    UNCONFIRMED_PROFILE,
};
use super::probe::{build_target, probe, ProbeOutcome, ProbeStatus, DEFAULT_PROBE_TIMEOUT};
use super::signature::{EntryKind, SignatureRegistry};
use super::snapshot::{
    identity_key, mark_total_failure, merge_snapshot, ObservedProcess, ProcessState, Recognition,
    TrackedProcess,
};
use super::view::{
    decode_cursor, encode_cursor, DiscoverySettings, DiscoverySettingsUpdate,
    InstallationDiscoveryView, InstallationQuery, ProbeView, ProcessDiscoveryPage,
    ProcessDiscoveryView, ProcessQuery, ScanView, DEFAULT_SCAN_INTERVAL_MS, MAX_SCAN_INTERVAL_MS,
    MIN_SCAN_INTERVAL_MS, SAMPLING_NOTE,
};
use super::winproc::{BackendError, BackendFailure, HostBackend, ProcessDetails, RawProcess};
use crate::multicli::diagnostics::iso_timestamp;
use crate::multicli::domain::{
    AgentDefinition, AgentInstallation, HostPath, ProcessIdentity, ProcessObservation,
};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::path::{resolve_workspace_path, PathResolutionStatus};
use crate::multicli::repo::DiscoveryProcessRow;
use crate::multicli::store::now_ms;
use crate::multicli::MultiCliService;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const SETTINGS_FILE: &str = "discovery-settings.json";
const INSTALL_RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_PROBE_TTL: Duration = Duration::from_secs(300);
const EXITED_RETENTION_GENERATIONS: u64 = 20;
const REFRESH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SettingsFile {
    schema_version: u32,
    interval_ms: u64,
}

/// A recognised process that could not be turned into a stable identity
/// because the OS creation time was not readable. Shown, never persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnconfirmedCandidate {
    pid: u32,
    parent_pid: Option<u32>,
    agent_id: Option<String>,
    product: Option<String>,
    evidence: Vec<String>,
    executable_display: Option<String>,
    redacted_command: Option<String>,
    reason: String,
    observed_at: String,
}

struct InstallationRecord {
    detected: DetectedInstallation,
    probe: Option<ProbeOutcome>,
    discovered_at: String,
    last_seen_scan: u64,
}

struct EngineState {
    generation: u64,
    running: bool,
    attempt_at: Option<String>,
    success_at: Option<String>,
    error: Option<AppError>,
    install_error: Option<AppError>,
    sampled_processes: u64,
    unreadable_processes: u64,
    installations: Vec<InstallationRecord>,
    install_epoch: u64,
    install_fingerprint: String,
    last_install_scan: Option<Instant>,
    tracked: BTreeMap<String, TrackedProcess>,
    unconfirmed: Vec<UnconfirmedCandidate>,
    probes: HashMap<String, ProbeOutcome>,
    index: InstallationIndex,
}

pub struct DiscoveryEngine {
    host_id: String,
    scope: String,
    service: Arc<MultiCliService>,
    backend: Arc<dyn HostBackend>,
    registry: SignatureRegistry,
    environment: ScannerEnvironment,
    settings_path: PathBuf,
    interval_ms: AtomicU64,
    state: Mutex<EngineState>,
    progress: Mutex<u64>,
    progress_signal: Condvar,
    refresh_pending: Mutex<bool>,
    scan_signal: Condvar,
    stop: AtomicBool,
    started: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    probe_timeout: Duration,
    probe_ttl: Duration,
    /// Test-only fault injection, compiled out of release builds. A fault file
    /// can be rewritten while the app runs so the desktop harness can drive
    /// stale/error states without restarting.
    fault: Option<FaultMode>,
    fault_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultMode {
    Enumeration,
    Partial,
}

impl DiscoveryEngine {
    pub fn new(
        host_id: impl Into<String>,
        data_root: &Path,
        service: Arc<MultiCliService>,
        backend: Arc<dyn HostBackend>,
        environment: ScannerEnvironment,
    ) -> Self {
        let host_id = host_id.into();
        let interval_ms = load_settings(data_root).unwrap_or(DEFAULT_SCAN_INTERVAL_MS);
        let (fault, fault_file) = fault_from_environment();
        Self {
            state: Mutex::new(EngineState {
                generation: 0,
                running: false,
                attempt_at: None,
                success_at: None,
                error: None,
                install_error: None,
                sampled_processes: 0,
                unreadable_processes: 0,
                installations: Vec::new(),
                install_epoch: 0,
                install_fingerprint: String::new(),
                last_install_scan: None,
                tracked: BTreeMap::new(),
                unconfirmed: Vec::new(),
                probes: HashMap::new(),
                index: InstallationIndex::default(),
            }),
            scope: host_id.clone(),
            host_id,
            service,
            backend,
            registry: SignatureRegistry::standard(),
            environment,
            settings_path: data_root.join(SETTINGS_FILE),
            interval_ms: AtomicU64::new(interval_ms),
            progress: Mutex::new(0),
            progress_signal: Condvar::new(),
            refresh_pending: Mutex::new(false),
            scan_signal: Condvar::new(),
            stop: AtomicBool::new(false),
            started: AtomicBool::new(false),
            worker: Mutex::new(None),
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            probe_ttl: DEFAULT_PROBE_TTL,
            fault,
            fault_file,
        }
    }

    #[cfg(test)]
    pub fn with_probe_ttl(mut self, ttl: Duration) -> Self {
        self.probe_ttl = ttl;
        self
    }

    #[cfg(test)]
    pub fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    #[cfg(test)]
    pub fn with_fault(mut self, fault: FaultMode) -> Self {
        self.fault = Some(fault);
        self
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    pub fn interval_ms(&self) -> u64 {
        self.interval_ms.load(Ordering::Relaxed)
    }

    pub fn signature_count(&self) -> usize {
        self.registry.all().len()
    }

    pub fn start(self: &Arc<Self>) {
        let mut worker = self.worker.lock().expect("discovery worker lock");
        if worker.is_some() {
            return;
        }
        self.started.store(true, Ordering::SeqCst);
        let engine = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("multicli-discovery".to_string())
            .spawn(move || engine.worker_loop())
            .expect("spawn discovery worker");
        *worker = Some(handle);
    }

    /// Stops the background thread. Called when the application window exits;
    /// it never touches an external process.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.progress_signal.notify_all();
        self.scan_signal.notify_all();
        if let Ok(mut worker) = self.worker.lock() {
            if let Some(handle) = worker.take() {
                let _ = handle.join();
            }
        }
    }

    /// Fault mode for this scan. A configured fault file wins so the desktop
    /// harness can toggle it live; otherwise the startup value applies.
    fn current_fault(&self) -> Option<FaultMode> {
        if let Some(path) = &self.fault_file {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            return parse_fault(text.trim());
        }
        self.fault
    }

    fn worker_loop(&self) {
        while !self.stop.load(Ordering::SeqCst) {
            self.run_scan();
            let interval = Duration::from_millis(self.interval_ms());
            let pending = self.refresh_pending.lock().expect("refresh lock");
            if *pending {
                continue;
            }
            let (guard, _) = self
                .scan_signal
                .wait_timeout(pending, interval)
                .expect("discovery wait");
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            drop(guard);
        }
    }

    // -----------------------------------------------------------------------
    // IPC surface
    // -----------------------------------------------------------------------

    pub fn list_installations(
        &self,
        query: &InstallationQuery,
    ) -> Result<crate::multicli::domain::Page<InstallationDiscoveryView>, AppError> {
        query.validate()?;
        let state = self.state.lock().expect("discovery state lock");
        let cursor = query
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, "installations", state.install_epoch))
            .transpose()?;
        let mut rows = state
            .installations
            .iter()
            .filter(|record| {
                query
                    .agent_id
                    .as_deref()
                    .map(|agent| record.detected.agent_id == agent)
                    .unwrap_or(true)
            })
            .map(|record| installation_view(record))
            .collect::<Vec<_>>();
        rows.sort_by(|left, right| {
            installation_sort_key(left)
                .cmp(&installation_sort_key(right))
                .then_with(|| left.installation_id.cmp(&right.installation_id))
        });
        let total = rows.len() as u64;
        let start = match &cursor {
            Some((key, id)) => rows
                .iter()
                .position(|row| {
                    (
                        installation_sort_key(row),
                        row.installation_id.clone().unwrap_or_default(),
                    ) > (key.clone(), id.clone())
                })
                .unwrap_or(rows.len()),
            None => 0,
        };
        let limit = query.limit as usize;
        let page_rows = rows.into_iter().skip(start).collect::<Vec<_>>();
        let has_more = page_rows.len() > limit;
        let items = page_rows.into_iter().take(limit).collect::<Vec<_>>();
        let next_cursor = if has_more {
            items.last().map(|row| {
                encode_cursor(
                    "installations",
                    &installation_sort_key(row),
                    row.installation_id.as_deref().unwrap_or(""),
                    state.install_epoch,
                )
            })
        } else {
            None
        };
        Ok(crate::multicli::domain::Page {
            items,
            next_cursor,
            total,
        })
    }

    pub fn list_processes(&self, query: &ProcessQuery) -> Result<ProcessDiscoveryPage, AppError> {
        query.validate()?;
        let state = self.state.lock().expect("discovery state lock");
        let scan = self.scan_view_locked(&state);
        let cursor = query
            .cursor
            .as_deref()
            .map(|cursor| decode_cursor(cursor, "processes", state.generation))
            .transpose()?;

        let mut rows = state
            .tracked
            .values()
            .filter(|entry| matches_filter(entry, query))
            .map(process_view)
            .collect::<Vec<_>>();
        // Candidates without a stable identity are always `unknown`; they are
        // shown under the same filters but never persisted or merged.
        for candidate in &state.unconfirmed {
            let view = candidate_view(candidate, &self.host_id);
            if matches_filter_view(&view, query) {
                rows.push(view);
            }
        }
        rows.sort_by(|left, right| {
            sort_key(left)
                .cmp(&sort_key(right))
                .then_with(|| sort_id(left).cmp(&sort_id(right)))
        });

        let total = rows.len() as u64;
        let start = match &cursor {
            Some((key, id)) => rows
                .iter()
                .position(|row| {
                    (sort_key(row).as_str(), sort_id(row).as_str()) > (key.as_str(), id.as_str())
                })
                .unwrap_or(rows.len()),
            None => 0,
        };
        let limit = query.limit as usize;
        let remaining = rows.into_iter().skip(start).collect::<Vec<_>>();
        let has_more = remaining.len() > limit;
        let items = remaining.into_iter().take(limit).collect::<Vec<_>>();
        let next_cursor = if has_more {
            items.last().map(|row| {
                encode_cursor("processes", &sort_key(row), &sort_id(row), state.generation)
            })
        } else {
            None
        };
        Ok(ProcessDiscoveryPage {
            items,
            next_cursor,
            total,
            scan,
        })
    }

    /// Requests one scan and waits for a completed generation. An in-flight
    /// scan is shared instead of starting a second scanner.
    pub fn refresh(&self) -> ScanView {
        if !self.started.load(Ordering::SeqCst) {
            // No worker (tests, or a caller that drives scans directly): run
            // one scan inline instead of waiting for a thread that is not
            // there.
            self.run_scan();
            return self.scan_view();
        }
        let baseline = {
            let state = self.state.lock().expect("discovery state lock");
            state.generation
        };
        {
            let mut pending = self.refresh_pending.lock().expect("refresh lock");
            *pending = true;
            self.scan_signal.notify_all();
        }
        self.wait_for_generation(baseline + 1, REFRESH_TIMEOUT);
        let state = self.state.lock().expect("discovery state lock");
        self.scan_view_locked(&state)
    }

    pub fn settings(&self) -> DiscoverySettings {
        DiscoverySettings::for_interval(self.interval_ms())
    }

    pub fn update_settings(
        &self,
        update: &DiscoverySettingsUpdate,
    ) -> Result<DiscoverySettings, AppError> {
        if !(MIN_SCAN_INTERVAL_MS..=MAX_SCAN_INTERVAL_MS).contains(&update.interval_ms) {
            return Err(AppError::invalid_argument(
                "intervalMs",
                format!("must be between {MIN_SCAN_INTERVAL_MS} and {MAX_SCAN_INTERVAL_MS}"),
            ));
        }
        save_settings(&self.settings_path, update.interval_ms)?;
        self.interval_ms
            .store(update.interval_ms, Ordering::Relaxed);
        // Wake the worker so the new interval takes effect immediately.
        self.scan_signal.notify_all();
        Ok(DiscoverySettings::for_interval(update.interval_ms))
    }

    pub fn scan_view(&self) -> ScanView {
        let state = self.state.lock().expect("discovery state lock");
        self.scan_view_locked(&state)
    }

    fn scan_view_locked(&self, state: &EngineState) -> ScanView {
        ScanView {
            generation: state.generation,
            status: scan_status(state),
            in_flight: state.running,
            last_attempt_at: state.attempt_at.clone(),
            last_success_at: state.success_at.clone(),
            error: state.error.clone(),
            install_error: state.install_error.clone(),
            interval_ms: self.interval_ms(),
            scope: self.scope.clone(),
            sampled_processes: state.sampled_processes,
            unreadable_processes: state.unreadable_processes,
            tracked_processes: state.tracked.len() as u64,
            note: SAMPLING_NOTE.to_string(),
        }
    }

    fn wait_for_generation(&self, target: u64, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut completed = self.progress.lock().expect("progress lock");
        while *completed < target {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            let (next, _) = self
                .progress_signal
                .wait_timeout(completed, remaining)
                .expect("progress wait");
            completed = next;
        }
    }

    // -----------------------------------------------------------------------
    // Scan
    // -----------------------------------------------------------------------

    /// Runs one scan synchronously. Exposed for tests and for the desktop
    /// verification harness; the worker thread calls the same function.
    pub fn run_scan(&self) {
        let generation = {
            let mut state = self.state.lock().expect("discovery state lock");
            state.generation += 1;
            state.running = true;
            state.attempt_at = Some(iso_timestamp(now_ms()));
            {
                let mut pending = self.refresh_pending.lock().expect("refresh lock");
                *pending = false;
            }
            state.generation
        };

        let fault = self.current_fault();
        let enumeration = match fault {
            Some(FaultMode::Enumeration) => Err(BackendError::failed(
                None,
                "injected enumeration failure (test-only fault)",
            )),
            _ => self.backend.enumerate(),
        };

        let mut partial = false;
        let mut unreadable = 0u64;
        let mut sampled = 0u64;
        let mut persist_error: Option<AppError> = None;
        match enumeration {
            Err(error) => {
                let mut state = self.state.lock().expect("discovery state lock");
                mark_total_failure(&mut state.tracked, error.reason.as_str());
                // Keep unresolved candidates as stale evidence too; a failed
                // enumeration cannot establish that any previous row exited.
                state.error = Some(
                    AppError::new(
                        ErrorCode::Io,
                        format!("process enumeration failed: {}", error.reason),
                    )
                    .with_details(serde_json::json!({
                        "stage": "enumerateProcesses",
                        "failure": error.kind,
                    }))
                    .with_retryable(true),
                );
                state.sampled_processes = 0;
                state.unreadable_processes = 0;
                state.running = false;
                drop(state);
                self.publish_progress(generation);
                return;
            }
            Ok(rows) => {
                // Establish trusted entry identities without waiting for
                // version probes, then publish process rows first. A slow
                // version probe must not hide an otherwise readable process.
                let initial = self
                    .state
                    .lock()
                    .expect("discovery state lock")
                    .last_install_scan
                    .is_none();
                if initial {
                    self.rescan_installations(generation, false);
                }
                sampled = rows.len() as u64;
                let outcome =
                    self.absorb_snapshot(rows, generation, fault == Some(FaultMode::Partial));
                partial = outcome.partial;
                unreadable = outcome.unreadable;
                persist_error = outcome.persist_error;
                let mut state = self.state.lock().expect("discovery state lock");
                state.unconfirmed = outcome.candidates;
            }
        }

        // Installation detection is cached and independent from the process
        // sampling rate.
        let install_due = {
            let state = self.state.lock().expect("discovery state lock");
            state
                .last_install_scan
                .map(|at| at.elapsed() >= INSTALL_RESCAN_INTERVAL)
                .unwrap_or(true)
        };
        if install_due {
            self.rescan_installations(generation, true);
        }

        let mut state = self.state.lock().expect("discovery state lock");
        state.sampled_processes = sampled;
        state.unreadable_processes = unreadable;
        state.running = false;
        // A partial inspect and a persistence failure are both real errors; the
        // persistence one wins because the stored rows are behind the view.
        state.error = persist_error.or_else(|| {
            partial.then(|| {
                AppError::new(
                    ErrorCode::AccessDenied,
                    "some processes could not be inspected; their rows are marked unknown",
                )
                .with_details(serde_json::json!({ "stage": "inspectProcess" }))
                .with_retryable(true)
            })
        });
        state.success_at = Some(iso_timestamp(now_ms()));
        drop(state);
        self.publish_progress(generation);
    }

    fn publish_progress(&self, generation: u64) {
        let mut completed = self.progress.lock().expect("progress lock");
        if *completed < generation {
            *completed = generation;
        }
        self.progress_signal.notify_all();
    }

    fn absorb_snapshot(
        &self,
        rows: Vec<RawProcess>,
        generation: u64,
        force_partial: bool,
    ) -> SnapshotOutcome {
        let observed_at = iso_timestamp(now_ms());
        let state = self.state.lock().expect("discovery state lock");
        let installations = state
            .installations
            .iter()
            .map(|record| record.detected.clone())
            .collect::<Vec<_>>();
        let previous = state.tracked.clone();
        drop(state);

        let index = classify::build_index(&installations, &self.registry, &self.environment);
        let by_pid = rows
            .iter()
            .map(|row| (row.pid, row.clone()))
            .collect::<BTreeMap<u32, RawProcess>>();
        let present_pids = by_pid.keys().copied().collect::<BTreeSet<u32>>();
        let mut evidence = super::snapshot::ScanEvidence {
            present_pids: present_pids.clone(),
            ..Default::default()
        };
        let mut details_cache: HashMap<u32, Result<ProcessDetails, BackendError>> = HashMap::new();
        let mut unconfirmed = Vec::new();
        let mut observed = Vec::new();
        let mut partial = false;

        for row in &rows {
            let Some(recognized) = classify::recognize(row, &index, &self.registry) else {
                continue;
            };
            if force_partial {
                // Test-only: pretend the OS refused this instance. It must be
                // reported as an unreadable candidate, never as an exit.
                partial = true;
                evidence
                    .unreadable_pids
                    .insert(row.pid, "access-denied".to_string());
                unconfirmed.push(UnconfirmedCandidate {
                    pid: row.pid,
                    parent_pid: row.parent_pid,
                    agent_id: Some(recognized.reference.agent_id.clone()),
                    product: Some(recognized.reference.product.clone()),
                    evidence: recognized.evidence.clone(),
                    executable_display: row
                        .executable
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string()),
                    redacted_command: Some(
                        super::redact::redact_process_argv(row.executable.as_deref(), &row.argv)
                            .text()
                            .to_string(),
                    ),
                    reason: "access-denied".to_string(),
                    observed_at: observed_at.clone(),
                });
                continue;
            }
            let details = inspect_cached(&mut details_cache, self.backend.as_ref(), row.pid);
            let details = match details {
                Ok(details) => details,
                Err(error) => {
                    partial = true;
                    evidence
                        .unreadable_pids
                        .insert(row.pid, failure_code(&error));
                    unconfirmed.push(UnconfirmedCandidate {
                        pid: row.pid,
                        parent_pid: row.parent_pid,
                        agent_id: Some(recognized.reference.agent_id.clone()),
                        product: Some(recognized.reference.product.clone()),
                        evidence: recognized.evidence.clone(),
                        executable_display: row
                            .executable
                            .as_ref()
                            .map(|path| path.to_string_lossy().to_string()),
                        redacted_command: Some(
                            super::redact::redact_process_argv(
                                row.executable.as_deref(),
                                &row.argv,
                            )
                            .text()
                            .to_string(),
                        ),
                        reason: failure_code(&error),
                        observed_at: observed_at.clone(),
                    });
                    continue;
                }
            };

            // Enumeration and inspect are different OS observations. A PID
            // may have been reused in between, so reclassify the inspected
            // image/argv before assigning any product or workspace.
            let inspected = RawProcess {
                pid: details.pid,
                parent_pid: details.parent_pid,
                name: row.name.clone(),
                executable: details.executable.clone(),
                argv: details.argv.clone(),
                start_time_hint: 0,
            };
            let Some(recognized) = classify::recognize(&inspected, &index, &self.registry) else {
                continue;
            };
            let identity = ProcessIdentity {
                host_id: self.host_id.clone(),
                pid: row.pid,
                creation_time: details.creation_time.clone(),
            };

            let parent_pid = details.parent_pid;
            let parent = parent_pid.and_then(|pid| {
                let parent_details = inspect_cached(&mut details_cache, self.backend.as_ref(), pid);
                match parent_details {
                    Ok(parent) => Some((
                        ProcessIdentity {
                            host_id: self.host_id.clone(),
                            pid,
                            creation_time: parent.creation_time.clone(),
                        },
                        true,
                    )),
                    Err(_) => None,
                }
            });
            let (parent_identity, parent_note) =
                classify::parent_link(&details.creation_time, parent);
            let source_program = parent_identity.as_ref().and_then(|parent| {
                details_cache
                    .get(&parent.pid)
                    .and_then(|result| result.as_ref().ok())
                    .and_then(|details| details.executable.as_ref())
                    .and_then(|path| path.file_name())
                    .map(|name| name.to_string_lossy().to_string())
            });

            let (workspace_id, workspace_evidence) =
                self.associate_workspace(recognized.cwd_argument.as_deref());

            observed.push(ObservedProcess {
                identity,
                executable_display: Some(
                    details
                        .executable
                        .as_ref()
                        .or(row.executable.as_ref())
                        .map(|path| path.to_string_lossy().to_string())
                        .unwrap_or_default(),
                )
                .filter(|value| !value.is_empty()),
                executable_canonical: details
                    .executable
                    .as_ref()
                    .and_then(|path| std::fs::canonicalize(path).ok())
                    .map(|path| crate::multicli::path::canonical_text(&path)),
                redacted_command: Some(
                    super::redact::redact_process_argv(
                        details.executable.as_deref(),
                        &details.argv,
                    )
                    .text()
                    .to_string(),
                ),
                recognition: Some(Recognition {
                    agent_id: recognized.reference.agent_id.clone(),
                    product: recognized.reference.product.clone(),
                    installation_id: Some(recognized.reference.installation_id.clone())
                        .filter(|value| !value.is_empty()),
                    entry_kind: recognized.entry_kind,
                    strength: recognized.strength,
                    evidence: recognized.evidence.clone(),
                    cwd_argument: recognized.cwd_argument.clone(),
                }),
                parent_identity,
                parent_note,
                source_program,
                workspace_id,
                workspace_evidence,
                interpreter: None,
            });
        }

        // Detect verified PID reuse for rows we already track.
        for entry in previous.values() {
            if !present_pids.contains(&entry.identity.pid) {
                continue;
            }
            if evidence.unreadable_pids.contains_key(&entry.identity.pid) {
                continue;
            }
            match inspect_cached(
                &mut details_cache,
                self.backend.as_ref(),
                entry.identity.pid,
            ) {
                Ok(details) => {
                    if details.creation_time != entry.identity.creation_time {
                        evidence.reused_pids.insert(entry.identity.pid);
                    }
                }
                Err(error) => {
                    partial = true;
                    evidence
                        .unreadable_pids
                        .insert(entry.identity.pid, failure_code(&error));
                }
            }
        }

        let mut state = self.state.lock().expect("discovery state lock");
        merge_snapshot(
            &mut state.tracked,
            observed,
            &evidence,
            generation,
            &observed_at,
            EXITED_RETENTION_GENERATIONS,
        );
        state.index = index;
        let snapshot = state
            .tracked
            .values()
            .cloned()
            .collect::<Vec<TrackedProcess>>();
        drop(state);

        let mut persist_error = self.persist(&snapshot);
        let conn = self.service.store().connection().lock();
        if let Err(error) = crate::multicli::repo::reconcile_absent_discovery_processes(
            &conn, &self.host_id, &present_pids,
        ) {
            persist_error.get_or_insert(error.with_retryable(true));
        }
        drop(conn);

        SnapshotOutcome {
            partial,
            unreadable: evidence.unreadable_pids.len() as u64,
            candidates: unconfirmed,
            persist_error,
        }
    }

    fn associate_workspace(&self, cwd: Option<&str>) -> (Option<String>, Option<String>) {
        let Some(cwd) = cwd else {
            return (None, None);
        };
        let resolved = resolve_workspace_path(&self.host_id, cwd);
        if resolved.status != PathResolutionStatus::Resolved {
            return (None, None);
        }
        let Some(canonical) = resolved.path.canonical_path.as_deref() else {
            return (None, None);
        };
        match self
            .service
            .find_workspace_by_canonical(&self.host_id, canonical)
        {
            Ok(Some(workspace)) => (
                Some(workspace.id),
                Some("explicit-directory-argument".to_string()),
            ),
            _ => (None, None),
        }
    }

    /// Returns the first persistence error so the scan status can report it
    /// instead of silently diverging from the database.
    fn persist(&self, snapshot: &[TrackedProcess]) -> Option<AppError> {
        let mut first_error = None;
        for entry in snapshot {
            let observation = ProcessObservation {
                identity: entry.identity.clone(),
                parent_identity: entry.parent_identity.clone(),
                executable: entry.executable_display.as_ref().map(|display| HostPath {
                    host_id: self.host_id.clone(),
                    display_path: display.clone(),
                    canonical_path: entry.executable_canonical.clone(),
                }),
                redacted_command: entry.redacted_command.clone(),
                observed_at: entry.last_seen_at.clone(),
            };
            if let Err(error) = self.service.record_process(&observation) {
                // A persistence failure must not stop sampling, but it must be
                // visible: the live view would otherwise be ahead of the DB.
                if first_error.is_none() {
                    first_error = Some(error.with_retryable(true));
                }
                continue;
            }
            let row = DiscoveryProcessRow {
                host_id: entry.identity.host_id.clone(),
                pid: entry.identity.pid,
                creation_time: entry.identity.creation_time.clone(),
                agent_id: entry.agent_id.clone(),
                installation_id: entry.installation_id.clone(),
                product: entry.product.clone(),
                entry_kind: entry.entry_kind.map(|kind| kind.as_str().to_string()),
                recognition: entry.recognition.map(|value| value.as_str().to_string()),
                evidence_json: serde_json::to_string(&entry.evidence)
                    .unwrap_or_else(|_| "[]".into()),
                state: entry.state.as_str().to_string(),
                freshness: entry.freshness.as_str().to_string(),
                state_reason: entry.state_reason.clone(),
                executable_display_path: entry.executable_display.clone(),
                executable_canonical_path: entry.executable_canonical.clone(),
                redacted_command: entry.redacted_command.clone(),
                parent: entry.parent_identity.clone(),
                parent_note: entry.parent_note.clone(),
                source_program: entry.source_program.clone(),
                workspace_id: entry.workspace_id.clone(),
                workspace_evidence: entry.workspace_evidence.clone(),
                first_seen_at: entry.first_seen_at.clone(),
                last_seen_at: entry.last_seen_at.clone(),
                last_seen_generation: entry.last_seen_generation as i64,
            };
            if let Err(error) = self.service.record_discovery_process(&row) {
                if first_error.is_none() {
                    first_error = Some(error.with_retryable(true));
                }
            }
        }
        first_error
    }

    fn rescan_installations(&self, generation: u64, probe_versions: bool) -> bool {
        let scan = scan_installations(&self.registry, &self.environment, &self.host_id);
        let discovered_at = iso_timestamp(now_ms());
        let mut records = Vec::new();
        let mut install_error = None;

        for detected in scan.installations {
            let probe = if probe_versions && detected.status == InstallStatus::Installed {
                self.probe_installation(&detected)
            } else {
                None
            };
            if detected.status != InstallStatus::HistoricalResidue {
                if let Err(error) = self.register_installation(&detected, probe.as_ref()) {
                    if error.code != ErrorCode::Conflict {
                        install_error = Some(error);
                    }
                }
            }
            records.push(InstallationRecord {
                detected,
                probe,
                discovered_at: discovered_at.clone(),
                last_seen_scan: generation,
            });
        }

        let fingerprint = records
            .iter()
            .map(|record| {
                format!(
                    "{}|{}|{}",
                    record.detected.identity,
                    record.detected.status.as_str(),
                    record
                        .probe
                        .as_ref()
                        .and_then(|probe| probe.version.clone())
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut state = self.state.lock().expect("discovery state lock");
        if state.install_fingerprint != fingerprint {
            state.install_fingerprint = fingerprint;
            state.install_epoch += 1;
        }
        state.installations = records;
        state.last_install_scan = probe_versions.then(Instant::now);
        state.index = classify::build_index(
            &state
                .installations
                .iter()
                .map(|record| record.detected.clone())
                .collect::<Vec<_>>(),
            &self.registry,
            &self.environment,
        );
        state.install_error = install_error;
        true
    }

    fn probe_installation(&self, detected: &DetectedInstallation) -> Option<ProbeOutcome> {
        let cached = {
            let state = self.state.lock().expect("discovery state lock");
            state.probes.get(&detected.identity).cloned()
        };
        if let Some(cached) = cached {
            let age = now_ms().saturating_sub(cached.checked_at_ms);
            if age >= 0 && (age as u64) < self.probe_ttl.as_millis() as u64 {
                return Some(cached);
            }
        }
        let signature = self.registry.by_id(&detected.signature_id)?;
        let native = match detected.entry_kind {
            EntryKind::NativeExe => detected.executable.as_ref().map(path_buf_of),
            _ => None,
        };
        let interpreter = match detected.entry_kind {
            EntryKind::NativeExe => None,
            _ => detected.interpreter.as_ref(),
        };
        let script = match detected.entry_kind {
            EntryKind::NativeExe => None,
            _ => detected.entry_script.as_ref().map(path_buf_of),
        };
        let target = build_target(signature, native.as_deref(), interpreter, script.as_deref())?;
        let outcome = probe(&target, signature, self.probe_timeout);
        let mut state = self.state.lock().expect("discovery state lock");
        state
            .probes
            .insert(detected.identity.clone(), outcome.clone());
        Some(outcome)
    }

    fn register_installation(
        &self,
        detected: &DetectedInstallation,
        probe: Option<&ProbeOutcome>,
    ) -> Result<(), AppError> {
        let Some(signature) = self.registry.by_id(&detected.signature_id) else {
            return Ok(());
        };
        self.service.register_agent(&AgentDefinition {
            id: detected.agent_id.clone(),
            vendor: signature.vendor.to_string(),
            display_name: detected.product.clone(),
        })?;
        let Some(executable) = detected.executable.clone() else {
            return Ok(());
        };
        self.service.register_discovered_installation(
            &AgentInstallation {
                id: detected.install_id.clone(),
                host_id: self.host_id.clone(),
                agent_id: detected.agent_id.clone(),
                executable,
                version: probe.and_then(|outcome| {
                    if outcome.status == ProbeStatus::Ok {
                        outcome.version.clone()
                    } else {
                        None
                    }
                }),
                config_profile_id: UNCONFIRMED_PROFILE.to_string(),
            },
            &detected.identity,
            &detected.identity_kind,
        )?;
        Ok(())
    }
}

struct SnapshotOutcome {
    partial: bool,
    unreadable: u64,
    candidates: Vec<UnconfirmedCandidate>,
    persist_error: Option<AppError>,
}

fn inspect_cached<'a>(
    cache: &'a mut HashMap<u32, Result<ProcessDetails, BackendError>>,
    backend: &dyn HostBackend,
    pid: u32,
) -> Result<ProcessDetails, BackendError> {
    if let Some(cached) = cache.get(&pid) {
        return cached.clone();
    }
    let result = backend.inspect(pid);
    cache.insert(pid, result.clone());
    result
}

fn failure_code(error: &BackendError) -> String {
    match error.kind {
        BackendFailure::AccessDenied => "access-denied",
        BackendFailure::Gone => "process-ended",
        BackendFailure::Timeout => "inspect-timeout",
        BackendFailure::Unsupported => "unsupported",
        BackendFailure::Failed => "inspect-failed",
    }
    .to_string()
}

fn scan_status(state: &EngineState) -> String {
    if state.running {
        return "scanning".to_string();
    }
    if state.error.is_some() && state.success_at.is_none() {
        return "error".to_string();
    }
    if state.error.is_some() {
        return "partial".to_string();
    }
    if state.success_at.is_some() {
        "ok".to_string()
    } else {
        "idle".to_string()
    }
}

fn path_buf_of(path: &HostPath) -> PathBuf {
    PathBuf::from(path.canonical_path.as_deref().unwrap_or(&path.display_path))
}

fn installation_sort_key(view: &InstallationDiscoveryView) -> String {
    format!(
        "{}\u{1f}{}",
        view.agent_id,
        view.executable
            .as_ref()
            .map(|path| path.display_path.to_ascii_lowercase())
            .unwrap_or_default()
    )
}

fn sort_key(view: &ProcessDiscoveryView) -> String {
    format!(
        "{}|{:010}",
        view.agent_id.clone().unwrap_or_else(|| "~".to_string()),
        view.pid.unwrap_or(0)
    )
}

fn sort_id(view: &ProcessDiscoveryView) -> String {
    view.identity
        .as_ref()
        .map(identity_key)
        .unwrap_or_else(|| format!("pid:{}", view.pid.unwrap_or(0)))
}

fn matches_filter(entry: &TrackedProcess, query: &ProcessQuery) -> bool {
    let view = process_view(entry);
    matches_filter_view(&view, query)
}

fn matches_filter_view(view: &ProcessDiscoveryView, query: &ProcessQuery) -> bool {
    if let Some(agent) = &query.agent_id {
        if view.agent_id.as_deref() != Some(agent.as_str()) {
            return false;
        }
    }
    if let Some(workspace) = &query.workspace_id {
        if view.workspace_id.as_deref() != Some(workspace.as_str()) {
            return false;
        }
    }
    if let Some(state) = query.state {
        if view.state != state.as_str() {
            return false;
        }
    }
    true
}

fn process_view(entry: &TrackedProcess) -> ProcessDiscoveryView {
    ProcessDiscoveryView {
        identity: Some(entry.identity.clone()),
        pid: Some(entry.identity.pid),
        agent_id: entry.agent_id.clone(),
        product: entry.product.clone(),
        installation_id: entry.installation_id.clone(),
        entry_kind: entry.entry_kind.map(|kind| kind.as_str().to_string()),
        recognition: entry.recognition.map(|value| value.as_str().to_string()),
        evidence: entry.evidence.clone(),
        state: entry.state.as_str().to_string(),
        freshness: entry.freshness.as_str().to_string(),
        state_reason: entry.state_reason.clone(),
        executable: entry.executable_display.as_ref().map(|display| HostPath {
            host_id: entry.identity.host_id.clone(),
            display_path: display.clone(),
            canonical_path: entry.executable_canonical.clone(),
        }),
        redacted_command: entry.redacted_command.clone(),
        parent_identity: entry.parent_identity.clone(),
        parent_note: entry.parent_note.clone(),
        source_program: entry.source_program.clone(),
        workspace_id: entry.workspace_id.clone(),
        workspace_evidence: entry.workspace_evidence.clone(),
        session_id: None,
        first_seen_at: entry.first_seen_at.clone(),
        last_seen_at: entry.last_seen_at.clone(),
        last_seen_scan: entry.last_seen_generation,
    }
}

fn candidate_view(candidate: &UnconfirmedCandidate, host_id: &str) -> ProcessDiscoveryView {
    ProcessDiscoveryView {
        identity: None,
        pid: Some(candidate.pid),
        agent_id: candidate.agent_id.clone(),
        product: candidate.product.clone(),
        installation_id: None,
        entry_kind: None,
        recognition: None,
        evidence: candidate.evidence.clone(),
        state: ProcessState::Unknown.as_str().to_string(),
        freshness: super::snapshot::Freshness::Stale.as_str().to_string(),
        state_reason: Some(candidate.reason.clone()),
        executable: candidate
            .executable_display
            .as_ref()
            .map(|display| HostPath {
                host_id: host_id.to_string(),
                display_path: display.clone(),
                canonical_path: None,
            }),
        redacted_command: candidate.redacted_command.clone(),
        parent_identity: None,
        parent_note: Some("identity-unconfirmed".to_string()),
        // A candidate has no verified parent identity, so no program name is
        // invented for it.
        source_program: None,
        workspace_id: None,
        workspace_evidence: None,
        session_id: None,
        first_seen_at: candidate.observed_at.clone(),
        last_seen_at: candidate.observed_at.clone(),
        last_seen_scan: 0,
    }
}

fn installation_view(record: &InstallationRecord) -> InstallationDiscoveryView {
    let detected = &record.detected;
    let probe = record.probe.as_ref();
    let version = probe
        .filter(|outcome| outcome.status == ProbeStatus::Ok)
        .and_then(|outcome| outcome.version.clone());
    InstallationDiscoveryView {
        installation_id: Some(detected.install_id.clone()),
        agent_id: detected.agent_id.clone(),
        product: detected.product.clone(),
        signature_id: detected.signature_id.clone(),
        status: detected.status.as_str().to_string(),
        entry_kind: detected.entry_kind.as_str().to_string(),
        executable: detected.executable.clone(),
        runtime: detected.runtime.clone(),
        entry_script: detected.entry_script.clone(),
        version: version.clone(),
        version_source: if version.is_some() { "probe" } else { "none" }.to_string(),
        probe: ProbeView::from_outcome(probe),
        config_profile_id: detected.config_profile_id.clone(),
        config_profile_status: "unconfirmed".to_string(),
        issue_code: detected.issue_code.clone(),
        issue_reason: detected.issue_reason.clone(),
        discovered_from: detected.discovered_from.clone(),
        discovered_at: record.discovered_at.clone(),
        last_seen_scan: record.last_seen_scan,
    }
}

fn load_settings(data_root: &Path) -> Option<u64> {
    let path = data_root.join(SETTINGS_FILE);
    let text = std::fs::read_to_string(path).ok()?;
    let file: SettingsFile = serde_json::from_str(&text).ok()?;
    if file.schema_version != 1 {
        return None;
    }
    Some(
        file.interval_ms
            .clamp(MIN_SCAN_INTERVAL_MS, MAX_SCAN_INTERVAL_MS),
    )
}

fn save_settings(path: &Path, interval_ms: u64) -> Result<(), AppError> {
    let document = SettingsFile {
        schema_version: 1,
        interval_ms,
    };
    let text = serde_json::to_vec_pretty(&document)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("discovery settings could not be written: {}", error.kind()),
        )
        .with_details(serde_json::json!({ "stage": "saveDiscoverySettings" }))
        .with_retryable(true)
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!(
                "discovery settings could not be committed: {}",
                error.kind()
            ),
        )
        .with_details(serde_json::json!({ "stage": "saveDiscoverySettings" }))
        .with_retryable(true)
    })
}

fn parse_fault(value: &str) -> Option<FaultMode> {
    match value {
        "enumerate" => Some(FaultMode::Enumeration),
        "partial" => Some(FaultMode::Partial),
        _ => None,
    }
}

#[cfg(debug_assertions)]
fn fault_from_environment() -> (Option<FaultMode>, Option<PathBuf>) {
    let mode = std::env::var("MULTICLI_DISCOVERY_FAULT")
        .ok()
        .and_then(|value| parse_fault(value.trim()));
    let file = std::env::var_os("MULTICLI_DISCOVERY_FAULT_FILE").map(PathBuf::from);
    (mode, file)
}

#[cfg(not(debug_assertions))]
fn fault_from_environment() -> (Option<FaultMode>, Option<PathBuf>) {
    (None, None)
}
