//! Background history indexer.
//!
//! One engine owns one worker thread. A pass is scheduled either by an explicit
//! refresh, by a rebuild, or by the periodic sweep (30 s) that catches missed
//! file notifications. Only one pass runs at a time: a second request marks the
//! source again instead of starting a competing scanner.
//!
//! Cancellation is cooperative and bounded: a refresh or rebuild bumps a
//! revision counter, and the running pass stops at its next line/file boundary
//! and commits what it already imported. Nothing is killed mid-transaction.

use super::model::{
    product_for_agent, HistoryOverallStatus, HistoryRebuildQuery, HistoryRefreshQuery,
    HistorySourceStatus, HistorySourceUpdate, HistorySourceView, HISTORY_PARSER_VERSION,
    REBUILD_BUDGET_MS, SWEEP_INTERVAL_MS,
};
use super::repo::{self, IndexStateRow, SourceRow};
use super::scan::{scan_source, ScanInput, ScanOptions, ScanProgress, ScanReport};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::store::now_ms;
use crate::multicli::MultiCliService;
use parking_lot::{Condvar, Mutex};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct HistoryEngine {
    host_id: String,
    service: Arc<MultiCliService>,
    state: Mutex<EngineState>,
    wake: Condvar,
    /// `Condvar::wait_for` needs a mutex. The flag is never read; the engine
    /// state is too coarse to hold while sleeping.
    wake_flag: Mutex<bool>,
    stop: AtomicBool,
    revision: AtomicU64,
    started: AtomicBool,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

#[derive(Default)]
struct EngineState {
    /// Full status per source, seeded from the database on first use.
    runtime: BTreeMap<String, RuntimeSource>,
    scheduled: BTreeSet<String>,
    rebuild: BTreeSet<String>,
    in_flight: Option<String>,
    last_sweep: Option<Instant>,
}

struct RuntimeSource {
    state: IndexStateRow,
    progress: Option<Arc<Mutex<ScanProgress>>>,
    /// Where the next pass starts in the sorted listing. Kept in memory: a
    /// restart simply begins at 0 again.
    next_index: usize,
}

impl HistoryEngine {
    pub fn new(host_id: impl Into<String>, service: Arc<MultiCliService>) -> Self {
        Self {
            host_id: host_id.into(),
            service,
            state: Mutex::new(EngineState::default()),
            wake: Condvar::new(),
            wake_flag: Mutex::new(false),
            stop: AtomicBool::new(false),
            revision: AtomicU64::new(0),
            started: AtomicBool::new(false),
            worker: Mutex::new(None),
        }
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }

    pub fn start(self: &Arc<Self>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        // A previous process may have ended with a pass in flight. This engine
        // is the only writer for its host, so the flag cannot belong to a live
        // pass now; leaving it set would show "indexing" forever and suppress
        // the UI's refresh affordance.
        {
            let mut conn = self.service.store().connection().lock();
            let _ = repo::reset_in_flight(&mut conn, &self.host_id);
        }
        let engine = self.clone();
        match std::thread::Builder::new()
            .name("multicli-history".into())
            .spawn(move || engine.run_loop())
        {
            Ok(handle) => {
                *self.worker.lock() = Some(handle);
            }
            Err(_) => {
                self.started.store(false, Ordering::SeqCst);
            }
        }
    }

    /// Stops only this application's worker. No CLI process is touched.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        self.revision.fetch_add(1, Ordering::SeqCst);
        self.wake.notify_all();
        let handle = self.worker.lock().take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    // -----------------------------------------------------------------------
    // Commands
    // -----------------------------------------------------------------------

    pub fn update_source(
        &self,
        query: &HistorySourceUpdate,
    ) -> Result<HistorySourceView, AppError> {
        let product = query.validate()?;
        let (display_root, canonical_root) = super::query::validate_source_root(&query.root)?;
        super::query::ensure_agent_for_product(&self.service, product.product)?;
        let source = {
            let conn = self.service.store().connection().lock();
            repo::upsert_source(
                &conn,
                &self.host_id,
                product,
                query.installation_id.as_deref(),
                &query.config_profile_id,
                &display_root,
                &canonical_root,
                query.enabled,
                query.source_id.as_deref(),
            )?
        };
        let state = {
            let conn = self.service.store().connection().lock();
            repo::ensure_state(&conn, &source.id)?
        };
        self.remember_status(&source.id, state.clone());

        if source.enabled {
            self.schedule(source.id.clone(), false);
        } else {
            self.deschedule(&source.id);
        }
        Ok(super::query::source_view(&source, Some(&state)))
    }

    pub fn refresh(&self, query: &HistoryRefreshQuery) -> Result<HistoryOverallStatus, AppError> {
        query.validate()?;
        match &query.source_id {
            Some(source_id) => {
                let source = self.require_source(source_id)?;
                if !source.enabled {
                    return Err(AppError::new(
                        ErrorCode::Conflict,
                        "the history source is disabled; enable it before refreshing",
                    )
                    .with_details(json!({ "stage": "historyRefresh", "sourceId": source_id })));
                }
                self.schedule(source.id.clone(), false);
            }
            None => {
                for source in self.list_enabled_sources()? {
                    self.schedule(source.id, false);
                }
            }
        }
        self.status()
    }

    pub fn rebuild(&self, query: &HistoryRebuildQuery) -> Result<HistoryOverallStatus, AppError> {
        query.validate()?;
        let source = self.require_source(&query.source_id)?;
        // Cancels a pass already reading this source; the purge is a single
        // transaction and cannot leave half-deleted rows.
        self.revision.fetch_add(1, Ordering::SeqCst);
        let state = {
            let mut conn = self.service.store().connection().lock();
            repo::purge_source_derived(&mut conn, &source.id)?;
            let mut state = repo::ensure_state(&conn, &source.id)?;
            state.status = "idle".to_string();
            state.in_flight = false;
            state.files_done = 0;
            state.files_skipped = 0;
            state.files_failed = 0;
            state.messages_indexed = 0;
            state.bytes_read = 0;
            state.complete = false;
            state.error_json = None;
            state.note =
                Some("rebuild requested; derived index cleared, user notes and tags kept".to_string());
            repo::write_state(&conn, &state)?;
            state
        };
        self.remember_status(&source.id, state);
        {
            let mut runtime = self.state.lock();
            if let Some(entry) = runtime.runtime.get_mut(&source.id) {
                entry.next_index = 0;
            }
        }
        self.schedule(source.id.clone(), true);
        self.status()
    }

    pub fn status(&self) -> Result<HistoryOverallStatus, AppError> {
        let sources = {
            let conn = self.service.store().connection().lock();
            repo::list_sources(&conn)?
        };
        // Copy the runtime snapshot, then release the engine lock before any
        // database read. Holding both in opposite orders is a deadlock waiting
        // to happen.
        let (runtime, in_flight) = {
            let state = self.state.lock();
            (
                state
                    .runtime
                    .iter()
                    .map(|(id, entry)| (id.clone(), merged_state(entry)))
                    .collect::<BTreeMap<_, _>>(),
                state.in_flight.is_some(),
            )
        };
        let mut items = Vec::with_capacity(sources.len());
        for source in sources {
            let state = match runtime.get(&source.id) {
                Some(state) => Some(state.clone()),
                None => {
                    let conn = self.service.store().connection().lock();
                    repo::read_state(&conn, &source.id)?
                }
            };
            items.push(HistorySourceStatus {
                source_id: source.id.clone(),
                agent_id: source.agent_id.clone(),
                product: source.product.clone(),
                enabled: source.enabled,
                status: super::query::status_view(state.as_ref(), source.parser_version),
            });
        }
        Ok(HistoryOverallStatus {
            schema_version: super::model::HISTORY_SCHEMA_VERSION,
            scope: self.host_id.clone(),
            in_flight,
            sources: items,
            note: super::model::STATUS_NOTE.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // Worker
    // -----------------------------------------------------------------------

    fn run_loop(self: Arc<Self>) {
        // A short delay keeps the window responsive on cold start; the first
        // pass still runs in the background, never on the UI thread.
        std::thread::sleep(Duration::from_millis(1_500));
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            if let Some((source_id, rebuild)) = self.next_scheduled() {
                self.run_source(&source_id, rebuild);
                continue;
            }
            if self.sweep_due() {
                {
                    let mut state = self.state.lock();
                    state.last_sweep = Some(Instant::now());
                }
                if let Ok(sources) = self.list_enabled_sources() {
                    for source in sources {
                        if self.stop.load(Ordering::SeqCst) {
                            break;
                        }
                        self.run_source(&source.id, false);
                    }
                }
                continue;
            }
            let mut guard = self.wake_flag.lock();
            let _ = self.wake.wait_for(&mut guard, Duration::from_millis(1_000));
        }
        self.started.store(false, Ordering::SeqCst);
    }

    fn sweep_due(&self) -> bool {
        let state = self.state.lock();
        state
            .last_sweep
            .map(|last| last.elapsed() >= Duration::from_millis(SWEEP_INTERVAL_MS))
            .unwrap_or(true)
    }

    fn next_scheduled(&self) -> Option<(String, bool)> {
        let mut state = self.state.lock();
        let source_id = state.scheduled.iter().next().cloned()?;
        state.scheduled.remove(&source_id);
        let rebuild = state.rebuild.remove(&source_id);
        Some((source_id, rebuild))
    }

    fn run_source(&self, source_id: &str, rebuild: bool) {
        let source = match self.require_source(source_id) {
            Ok(source) => source,
            Err(_) => return,
        };
        let mut state_row = {
            let conn = self.service.store().connection().lock();
            repo::read_state(&conn, &source.id)
                .ok()
                .flatten()
                .unwrap_or_else(|| IndexStateRow::idle(&source.id, HISTORY_PARSER_VERSION))
        };
        let generation = state_row.generation + 1;
        let attempt_at = super::ids::iso_from_millis(now_ms());
        state_row.generation = generation;
        state_row.status = "indexing".to_string();
        state_row.in_flight = true;
        state_row.last_attempt_at = Some(attempt_at.clone());
        state_row.files_done = 0;
        state_row.files_skipped = 0;
        state_row.files_failed = 0;
        state_row.messages_indexed = 0;
        state_row.bytes_read = 0;
        state_row.complete = false;
        state_row.error_json = None;
        state_row.note = Some(if rebuild {
            "rebuild pass".to_string()
        } else {
            "incremental pass".to_string()
        });
        let progress = Arc::new(Mutex::new(ScanProgress::default()));
        let start_index = {
            let runtime = self.state.lock();
            runtime
                .runtime
                .get(&source.id)
                .map(|entry| entry.next_index)
                .unwrap_or(0)
        };
        {
            let mut runtime = self.state.lock();
            runtime.in_flight = Some(source.id.clone());
            runtime.runtime.insert(
                source.id.clone(),
                RuntimeSource {
                    state: state_row.clone(),
                    progress: Some(progress.clone()),
                    next_index: start_index,
                },
            );
        }
        {
            let conn = self.service.store().connection().lock();
            let _ = repo::write_state(&conn, &state_row);
        }

        let revision = self.revision.load(Ordering::SeqCst);
        let cancel = || {
            self.stop.load(Ordering::SeqCst) || self.revision.load(Ordering::SeqCst) != revision
        };
        let options = ScanOptions {
            mode: if rebuild {
                super::scan::ScanMode::Rebuild
            } else {
                super::scan::ScanMode::Incremental
            },
            budget_ms: if rebuild {
                REBUILD_BUDGET_MS
            } else {
                super::model::PASS_BUDGET_MS
            },
            max_files: super::model::MAX_FILES_PER_PASS,
            max_bytes_per_file: super::model::MAX_BYTES_PER_FILE_PASS,
            start_index,
            file_budget_ms: if rebuild {
                REBUILD_BUDGET_MS
            } else {
                super::model::FILE_BUDGET_MS
            },
            // A rebuild re-reads every byte, so a separate verification cycle
            // would only repeat that work.
            verify_budget_bytes: if rebuild {
                0
            } else {
                super::model::VERIFY_BUDGET_PER_PASS
            },
        };
        let result = scan_source(
            ScanInput {
                service: &self.service,
                source: &source,
                options,
                cancel: &cancel,
            },
            |snapshot| {
                let mut current = progress.lock();
                *current = snapshot.clone();
            },
        );

        let mut next_index = start_index;
        let final_row = match result {
            Ok(report) => {
                next_index = report.next_index;
                finish_success(&source, state_row.clone(), &report, &attempt_at)
            }
            Err(error) => {
                let mut failed = state_row.clone();
                failed.status = "error".to_string();
                failed.in_flight = false;
                failed.error_json = serde_json::to_string(&error).ok();
                failed.note =
                    Some("the pass failed; previously imported rows are unchanged".to_string());
                failed
            }
        };
        {
            let conn = self.service.store().connection().lock();
            let _ = repo::write_state(&conn, &final_row);
        }
        let mut runtime = self.state.lock();
        runtime.in_flight = None;
        runtime.runtime.insert(
            source.id.clone(),
            RuntimeSource {
                state: final_row,
                progress: None,
                next_index,
            },
        );
    }

    /// Deterministic synchronous pass for tests and for the acceptance harness.
    /// Production code always goes through the worker thread.
    #[cfg(test)]
    pub(crate) fn run_source_for_test(&self, source_id: &str, rebuild: bool) {
        self.run_source(source_id, rebuild);
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn require_source(&self, source_id: &str) -> Result<SourceRow, AppError> {
        let conn = self.service.store().connection().lock();
        repo::read_source(&conn, source_id)?
            .ok_or_else(|| AppError::not_found("history source", source_id))
    }

    fn list_enabled_sources(&self) -> Result<Vec<SourceRow>, AppError> {
        let conn = self.service.store().connection().lock();
        Ok(repo::list_sources(&conn)?
            .into_iter()
            .filter(|source| source.enabled)
            .collect())
    }

    fn remember_status(&self, source_id: &str, state: IndexStateRow) {
        let mut runtime = self.state.lock();
        // Keep the slice position: a status refresh while a pass is queued must
        // not send the next pass back to the start of the listing.
        let next_index = runtime
            .runtime
            .get(source_id)
            .map(|entry| entry.next_index)
            .unwrap_or(0);
        runtime.runtime.insert(
            source_id.to_string(),
            RuntimeSource {
                state,
                progress: None,
                next_index,
            },
        );
    }

    fn schedule(&self, source_id: String, rebuild: bool) {
        {
            let mut state = self.state.lock();
            state.scheduled.insert(source_id.clone());
            if rebuild {
                state.rebuild.insert(source_id);
            }
        }
        self.wake.notify_all();
    }

    fn deschedule(&self, source_id: &str) {
        let mut state = self.state.lock();
        state.scheduled.remove(source_id);
        state.rebuild.remove(source_id);
    }
}

fn merged_state(entry: &RuntimeSource) -> IndexStateRow {
    let mut state = entry.state.clone();
    if let Some(progress) = &entry.progress {
        let progress = progress.lock();
        state.files_done = progress.files_done;
        state.files_skipped = progress.files_skipped;
        state.files_failed = progress.files_failed;
        state.messages_indexed = progress.messages;
        state.bytes_read = progress.bytes;
        if progress.files_seen > state.files_total {
            state.files_total = progress.files_seen;
        }
    }
    state
}

fn finish_success(
    source: &SourceRow,
    mut state: IndexStateRow,
    report: &ScanReport,
    attempt_at: &str,
) -> IndexStateRow {
    state.in_flight = false;
    state.files_total = report.listed_files as u64;
    state.files_done = report.progress.files_done;
    state.files_skipped = report.progress.files_skipped;
    state.files_failed = report.progress.files_failed;
    state.messages_indexed = report.progress.messages;
    state.bytes_read = report.progress.bytes;
    state.complete = report.complete;
    state.last_attempt_at = Some(attempt_at.to_string());
    let failed = report.progress.files_failed;
    let line_errors = report.progress.line_errors;
    let incomplete = report.progress.files_incomplete;
    if report.cancelled {
        state.status = "cancelled".to_string();
        state.note = Some(
            "the pass was cancelled; the next pass resumes from the committed cursors".to_string(),
        );
        return state;
    }
    if failed > 0 || !report.issues.is_empty() {
        state.status = "partial".to_string();
        // Report the first blocking cause instead of flattening everything to
        // `io`: a pass whose only rejected file was an unrecognised format must
        // stay `unsupported`/non-retryable, and an unreadable directory must
        // stay `access_denied`/retryable. Every per-issue code stays in
        // `details`. Routine skips (links, foreign files) are no longer issues.
        // Priority, not "the first issue": an unsupported file earlier in the
        // path order must not be downgraded to a retryable `io` because a
        // different file reported a bad line first.
        let code = if report
            .issues
            .iter()
            .any(|issue| matches!(issue.code.as_str(), "unsupported" | "unrecognized-format"))
        {
            ErrorCode::Unsupported
        } else if report
            .issues
            .iter()
            .any(|issue| issue.code == "access_denied")
        {
            ErrorCode::AccessDenied
        } else if report.issues.iter().any(|issue| issue.code == "not_found") {
            ErrorCode::NotFound
        } else {
            ErrorCode::Io
        };
        let message = if failed > 0 {
            format!("{failed} file(s) in this pass could not be imported")
        } else if line_errors > 0 {
            format!("{line_errors} record(s) in this pass were rejected or isolated")
        } else {
            format!(
                "{} path issue(s) were reported in this pass",
                report.issues.len()
            )
        };
        let error = AppError::new(code, message)
        .with_details(json!({
            "stage": "historyScan",
            "sourceId": source.id,
            "filesFailed": failed,
            "filesIncomplete": incomplete,
            "lineErrors": line_errors,
            "filesImported": report.progress.files_done,
            "issues": report
                .issues
                .iter()
                .take(8)
                .map(|issue| json!({
                    "code": issue.code,
                    "fileIndex": issue.file_index,
                    "line": issue.line,
                    "detail": issue.detail,
                }))
                .collect::<Vec<_>>(),
        }))
        // `io` and `access_denied` can both be transient (a locked file, an
        // offline share); `unsupported`/`not_found` cannot.
        .with_retryable(matches!(code, ErrorCode::Io | ErrorCode::AccessDenied));
        state.error_json = serde_json::to_string(&error).ok();
        state.note = Some(
            "some records or files were rejected; finished files are committed and the next pass retries the rest"
                .to_string(),
        );
        return state;
    }
    if report.unresolved_line_errors > 0 {
        // The pass itself may have had nothing to do (the file was skipped as
        // unchanged), but records rejected earlier are still missing. Reporting
        // `ok` here would hide them until the file happens to change again.
        state.status = "partial".to_string();
        let error = AppError::new(
            ErrorCode::Unsupported,
            format!(
                "{} record(s) from this source are still rejected and not in the index",
                report.unresolved_line_errors
            ),
        )
        .with_details(json!({
            "stage": "historyScan",
            "sourceId": source.id,
            "reason": "line-errors",
            "lineErrors": report.unresolved_line_errors,
        }))
        .with_retryable(false);
        state.error_json = serde_json::to_string(&error).ok();
        state.note = Some(
            "records rejected by the parser stay reported until the file changes and is re-read"
                .to_string(),
        );
        return state;
    }
    if !report.complete {
        // A pass that stopped on a budget is not a success, even with no
        // errors: the index is behind the files and the next pass continues.
        state.status = "partial".to_string();
        state.error_json = None;
        state.note = Some(format!(
            "the pass did not finish: {incomplete} file(s) still have unread content; the next pass continues from their committed offsets"
        ));
        return state;
    }
    state.status = "ok".to_string();
    state.error_json = None;
    state.last_success_at = Some(super::ids::iso_from_millis(now_ms()));
    state.note = Some(if report.pending_verification > 0 {
        format!(
            "pass complete: {} file(s) listed, {} imported, {} unchanged; integrity verification continues for {} file(s)",
            report.listed_files,
            report.progress.files_done,
            report.progress.files_skipped,
            report.pending_verification
        )
    } else {
        format!(
            "pass complete: {} file(s) listed, {} imported, {} unchanged",
            report.listed_files, report.progress.files_done, report.progress.files_skipped
        )
    });
    state
}

/// Product lookup used by the UI when it wants the display name for a source.
pub fn product_display(agent_id: &str) -> Option<&'static str> {
    product_for_agent(agent_id).map(|product| product.display_name)
}
