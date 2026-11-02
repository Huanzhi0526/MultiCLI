//! Bounded, resumable, idempotent history scan.
//!
//! A pass is bounded five ways: directory depth, number of listed files, bytes
//! read per file, wall-clock time for the pass, and wall-clock time for one
//! file's transaction. The last two keep the single SQLite connection
//! available: a huge file is committed in several passes instead of holding the
//! store for the whole budget.
//!
//! Resumability is explicit rather than inferred. A cursor records how many
//! bytes and lines were committed, whether the reader actually reached
//! end-of-file, and the parser checkpoint derived from the file prefix (native
//! session id, cwd, extra roots, pending title, whether a title was already
//! taken). A pass that stopped on its byte budget therefore reports
//! `complete = false` and the next pass continues from the committed offset —
//! it is not mistaken for an unchanged file.
//!
//! Integrity is bounded but eventually complete: each file carries a hash per
//! 256 KiB block, and every pass verifies one window of one file's blocks. A
//! same-size rewrite that preserves the mtime (the case that size + mtime + a
//! 4 KiB head/anchor hash cannot see) is found when the window reaches it, i.e.
//! within one full verification cycle of that file.
//!
//! Verification is its own phase, not a branch of the import loop. When both
//! ran in the same loop, a file that grew on every pass always took the append
//! branch, so its already committed prefix was never compared: an intact-looking
//! file could carry a rewritten middle forever as long as the CLI kept writing.
//! The phase walks the corpus in a persisted rotation and spends a reserved
//! slice of the pass budget, so the bound holds for a continuously appended
//! file and no file in a large corpus is starved by its neighbours.
//!
//! A compressed transcript is verified on its *physical* bytes. It cannot be
//! checked window by window in decoded space without decoding the whole prefix
//! for every window, which is quadratic; refusing to check it left large files
//! permanently unverified. The physical layer detects any change to the file,
//! including a re-encode that preserves size and mtime, in bounded windows.
//!
//! One file commits in one transaction: purge (when the file was truncated or
//! replaced), normalised rows, FTS rows, provenance, block hashes and the new
//! cursor. A crash therefore leaves either the old cursor and old rows, or the
//! new cursor and new rows — never a cursor that promises rows which were
//! rolled back.

use super::guard;
use super::ids;
use super::model::{
    product_for_agent, source_namespace, HistoryProduct, BLOCK_BYTES, FORMAT_PROBE_LINES,
    HISTORY_PARSER_VERSION, MAX_BYTES_PER_FILE_PASS, MAX_COMPRESSED_PHYSICAL_BYTES,
    MAX_DECOMPRESSED_BYTES, MAX_FILES_PER_PASS, MAX_LINE_BYTES, MAX_LINE_SKIP_BYTES,
    MAX_SCAN_DEPTH, VERIFY_COOLDOWN_MS, VERIFY_RESERVE_MS, VERIFY_WINDOW_BYTES,
};
use super::parser::{
    parser_for_agent, HistoryParser, ParseContext, Parsed, ParserCheckpoint, RecordOut, TitleSource,
};
use super::repo::{self, BlockLayer, BlockRow, CursorRow, SourceRow};
use crate::multicli::domain::Workspace;
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::path::{
    canonical_text, normalize_display_path, resolve_workspace_path, workspace_from_resolved,
    PathResolutionStatus,
};
use crate::multicli::MultiCliService;
use rusqlite::TransactionBehavior;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

/// Windows `FILE_ATTRIBUTE_REPARSE_POINT`. A junction below a configured root
/// could otherwise smuggle an unrelated tree (or a credential folder) into the
/// index.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Incremental,
    Rebuild,
}

#[derive(Debug, Clone)]
pub struct ScanOptions {
    pub mode: ScanMode,
    pub budget_ms: u64,
    pub max_files: usize,
    pub max_bytes_per_file: u64,
    /// Index in the sorted listing to start at. Callers pass the previous
    /// report's `next_index`, so a large root is covered in slices instead of
    /// re-checking the same prefix (and starving later files) every pass.
    pub start_index: usize,
    /// Wall-clock budget for one file's transaction.
    pub file_budget_ms: u64,
    /// Bytes of content verification this pass may spend across all files.
    pub verify_budget_bytes: u64,
}

impl ScanOptions {
    pub fn incremental() -> Self {
        Self {
            mode: ScanMode::Incremental,
            budget_ms: super::model::PASS_BUDGET_MS,
            max_files: MAX_FILES_PER_PASS,
            max_bytes_per_file: MAX_BYTES_PER_FILE_PASS,
            start_index: 0,
            file_budget_ms: super::model::FILE_BUDGET_MS,
            verify_budget_bytes: super::model::VERIFY_BUDGET_PER_PASS,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScanProgress {
    pub files_seen: u64,
    pub files_done: u64,
    pub files_skipped: u64,
    pub files_failed: u64,
    /// Files that stopped on a byte/time budget before end-of-file. A pass with
    /// a non-zero value is not complete, whatever the listing says.
    pub files_incomplete: u64,
    pub messages: u64,
    pub bytes: u64,
    pub line_errors: u64,
    pub verified_bytes: u64,
    pub files_verified: u64,
}

#[derive(Debug, Clone)]
pub struct ScanIssue {
    pub code: String,
    pub file_index: u64,
    pub line: Option<u64>,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub progress: ScanProgress,
    pub issues: Vec<ScanIssue>,
    /// True only when the whole configured tree was listed, every listed file
    /// was read to end-of-file or skipped after a complete read, and no file
    /// stopped on a budget.
    pub complete: bool,
    pub cancelled: bool,
    pub listed_files: usize,
    /// Where the next slice should start. `0` after a complete cycle.
    pub next_index: usize,
    pub deleted_cursors: u64,
    pub purged_messages: u64,
    /// Files whose verification cycle has not yet covered the whole file.
    pub pending_verification: u64,
    /// Rejected records still recorded on this source's cursors, including
    /// files that were skipped in this pass. Without it a bad line disappeared
    /// from the status as soon as the next (unchanged) pass skipped the file.
    pub unresolved_line_errors: u64,
}

struct FileCandidate {
    path: PathBuf,
    display: String,
    canonical: String,
    size: u64,
    mtime_ms: i64,
}

/// Walks only the configured root. Symlinks/reparse points are skipped, not
/// followed; protected directories are skipped at any depth; unreadable
/// entries are reported instead of aborting the pass.
fn collect_files(
    root: &Path,
    root_canonical: &str,
    extensions: &[&'static str],
    max_files: usize,
    issues: &mut Vec<ScanIssue>,
) -> (Vec<FileCandidate>, bool, bool) {
    let mut files = Vec::new();
    let mut truncated = false;
    // A directory we could not list means the listing is not authoritative:
    // stale-cursor deletion and the `complete` flag must both stand down.
    let mut incomplete = false;
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut visited = 0_usize;
    let visit_budget = max_files.saturating_mul(8).max(2_000);

    while let Some((directory, depth)) = stack.pop() {
        if visited > visit_budget {
            truncated = true;
            break;
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                incomplete = true;
                issues.push(ScanIssue {
                    code: "directory-unreadable".to_string(),
                    file_index: u64::try_from(visited).unwrap_or(0),
                    line: None,
                    detail: error.kind().to_string(),
                });
                continue;
            }
        };
        for entry in entries {
            visited += 1;
            let Ok(entry) = entry else {
                incomplete = true;
                continue;
            };
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            // The root check protects the configured directory; this protects
            // every directory below it, at any depth and in any letter case.
            if guard::protected_component(&name).is_some() {
                continue;
            }
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                incomplete = true;
                continue;
            };
            if is_reparse_or_symlink(&metadata) {
                // Routine: a link or junction inside the tree is skipped, not a
                // pass failure. Reporting it as an issue made every pass
                // "partial" and could crowd out real errors.
                continue;
            }
            if metadata.is_dir() {
                if depth + 1 <= MAX_SCAN_DEPTH {
                    stack.push((path, depth + 1));
                } else {
                    truncated = true;
                }
                continue;
            }
            if !metadata.is_file() || !matches_extension(&path, extensions) {
                continue;
            }
            if files.len() >= max_files {
                truncated = true;
                break;
            }
            let canonical = match guard::contained_canonical(root_canonical, &path) {
                guard::Containment::Inside(canonical) => canonical_text(&canonical),
                // Resolved outside the root: a junction or a lexical escape.
                // Routine; never read it.
                guard::Containment::Outside => continue,
                // Could not resolve. Treat the listing as incomplete so the
                // stale-cursor purge cannot delete rows of a file that is
                // merely locked for a moment.
                guard::Containment::Unresolved => {
                    incomplete = true;
                    issues.push(ScanIssue {
                        code: "path-unresolved".to_string(),
                        file_index: u64::try_from(visited).unwrap_or(0),
                        line: None,
                        detail: "canonicalize failed".to_string(),
                    });
                    continue;
                }
            };
            // A protected name can also appear in the canonical form (8.3
            // alias, trailing-dot trick); re-check the resolved components.
            if guard::protected_in(&relative_components(root_canonical, &canonical)).is_some() {
                continue;
            }
            let display = normalize_display_path(&path.to_string_lossy())
                .unwrap_or_else(|_| path.to_string_lossy().to_string());
            files.push(FileCandidate {
                path,
                display,
                canonical,
                size: metadata.len(),
                mtime_ms: metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0),
            });
        }
        if truncated {
            break;
        }
    }

    files.sort_by(|left, right| left.canonical.cmp(&right.canonical));
    (files, truncated, incomplete)
}

/// Path components below the configured root, for the protected-name check.
fn relative_components(root_canonical: &str, canonical: &str) -> Vec<String> {
    let root = ids::fold_path(root_canonical);
    let candidate = ids::fold_path(canonical);
    let relative = candidate
        .strip_prefix(root.trim_end_matches('\\'))
        .unwrap_or(&candidate)
        .trim_start_matches('\\');
    relative
        .split('\\')
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn matches_extension(path: &Path, extensions: &[&'static str]) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if extensions
        .iter()
        .any(|extension| name.ends_with(&format!(".{extension}")))
    {
        return true;
    }
    // A rotated transcript keeps the base extension and appends an ordinal
    // (`session.jsonl.1`). Its path identity differs from the live file, so the
    // archived chunk keeps its own rows instead of being purged when the live
    // file is rewritten; overlapping events dedupe by native event id. A chunk
    // that starts mid-file has no session header and is reported as
    // unrecognised rather than merged into an unrelated session.
    if extensions.contains(&"jsonl") {
        if let Some((_, suffix)) = name.rsplit_once(".jsonl.") {
            if !suffix.is_empty()
                && suffix.len() <= 3
                && suffix.bytes().all(|byte| byte.is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

fn is_reparse_or_symlink(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    false
}

fn is_compressed(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase().ends_with(".jsonl.zst"))
        .unwrap_or(false)
}

fn read_head(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; ids::HEAD_SAMPLE_BYTES];
    let mut read = 0_usize;
    while read < buffer.len() {
        let count = file.read(&mut buffer[read..])?;
        if count == 0 {
            break;
        }
        read += count;
    }
    buffer.truncate(read);
    Ok(buffer)
}

fn read_anchor(path: &Path, offset: u64, length: u64) -> std::io::Result<Vec<u8>> {
    if offset == 0 {
        return Ok(Vec::new());
    }
    let start = offset.saturating_sub(length);
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Vec::new();
    file.take(offset - start).read_to_end(&mut buffer)?;
    Ok(buffer)
}

/// Splits a byte stream into fixed-size blocks and hashes each one.
struct BlockHasher {
    index: u64,
    buffer: Vec<u8>,
    blocks: Vec<BlockRow>,
    valid: bool,
}

impl BlockHasher {
    fn new(first_block: u64) -> Self {
        Self {
            index: first_block,
            buffer: Vec::with_capacity(BLOCK_BYTES as usize),
            blocks: Vec::new(),
            valid: true,
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        if !self.valid {
            return;
        }
        while !bytes.is_empty() {
            let room = BLOCK_BYTES as usize - self.buffer.len();
            let take = room.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == BLOCK_BYTES as usize {
                self.flush();
            }
        }
    }

    fn invalidate(&mut self) {
        self.valid = false;
        self.blocks.clear();
        self.buffer.clear();
    }

    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        self.blocks.push(BlockRow {
            index: self.index,
            hash: ids::sha256_hex(&self.buffer),
            bytes: self.buffer.len() as u64,
        });
        self.index += 1;
        self.buffer.clear();
    }

    /// Returns the blocks plus whether the table is trustworthy.
    fn finish(mut self) -> (Vec<BlockRow>, bool) {
        self.flush();
        let valid = self.valid;
        (self.blocks, valid)
    }
}

/// Bounded line reader.
///
/// A single huge line is consumed without allocating more than the cap. The
/// skip beyond the cap is itself bounded: a line longer than
/// `max + MAX_LINE_SKIP_BYTES` aborts the file instead of scanning a
/// pathological record to the end, so a normal exit or a cancel never waits for
/// a gigabyte-long "line".
///
/// The reader also feeds a block hasher with exactly the bytes it commits.
/// Only terminated, non-clipped lines are fed, so the hash covers the committed
/// prefix and never the half-written tail that the cursor does not include.
struct BoundedLines<R> {
    reader: R,
    max: usize,
    hasher: BlockHasher,
}

struct BoundedLine {
    bytes: Vec<u8>,
    truncated: bool,
    consumed: u64,
    /// False when the file ended before a newline: the bytes are a residue of a
    /// record that may still be written.
    terminated: bool,
}

impl<R: BufRead> BoundedLines<R> {
    fn new(reader: R, max: usize, first_block: u64) -> Self {
        Self {
            reader,
            max,
            hasher: BlockHasher::new(first_block),
        }
    }

    /// Reads and discards `skip` bytes, feeding them to the hasher.
    ///
    /// Returns `false` when the pass deadline or a cancel interrupted the skip.
    /// The caller must then not claim any progress.
    fn skip_exact(&mut self, skip: u64, state: &mut StreamState<'_>) -> Result<bool, AppError> {
        let mut remaining = skip;
        let mut buffer = vec![0_u8; 64 * 1024];
        while remaining > 0 {
            if (state.cancel)() || Instant::now() >= *state.deadline {
                return Ok(false);
            }
            let want = remaining.min(buffer.len() as u64) as usize;
            let read = self
                .reader
                .read(&mut buffer[..want])
                .map_err(|error| file_error(format!("zstd:{error}"), None))?;
            if read == 0 {
                return Err(AppError::new(
                    ErrorCode::Io,
                    "the compressed transcript ended before its committed offset",
                )
                .with_details(json!({
                    "stage": "historyScan",
                    "reason": "stream-truncated",
                }))
                .with_retryable(true));
            }
            self.hasher.update(&buffer[..read]);
            state.decompressed = state.decompressed.saturating_add(read as u64);
            if state.decompressed > MAX_DECOMPRESSED_BYTES {
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "the compressed transcript exceeds the decompression cap",
                )
                .with_details(json!({
                    "stage": "historyScan",
                    "reason": "decompressed-too-large",
                }))
                .with_retryable(false));
            }
            remaining -= read as u64;
        }
        Ok(true)
    }

    fn next_line(&mut self) -> std::io::Result<Option<BoundedLine>> {
        let mut bytes = Vec::new();
        let mut consumed = 0_u64;
        let mut truncated = false;
        let mut terminated = false;
        let mut hard = 0_usize;
        let hard_limit = self.max.saturating_add(MAX_LINE_SKIP_BYTES);
        loop {
            let chunk = self.reader.fill_buf()?;
            if chunk.is_empty() {
                break;
            }
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let take = newline.unwrap_or(chunk.len());
            let room = self.max.saturating_sub(bytes.len());
            if room > 0 {
                let copy = take.min(room);
                bytes.extend_from_slice(&chunk[..copy]);
                if copy < take {
                    truncated = true;
                }
            } else if take > 0 {
                truncated = true;
            }
            hard = hard.saturating_add(take);
            if hard > hard_limit {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "line exceeds the bounded skip limit",
                ));
            }
            consumed += take as u64;
            self.reader.consume(take);
            if newline.is_some() {
                self.reader.consume(1);
                consumed += 1;
                terminated = true;
                break;
            }
        }
        if bytes.is_empty() && consumed == 0 {
            return Ok(None);
        }
        // Hash exactly the bytes on disk (CR included), then strip the CR for
        // JSON parsing. Hashing a synthesised line ending would make every
        // block hash disagree with the file and force a full re-read each pass.
        if terminated && !truncated {
            self.hasher.update(&bytes);
            self.hasher.update(b"\n");
        } else if terminated {
            // A clipped line does not hash to the file bytes, so this file's
            // block table is discarded rather than made silently wrong.
            self.hasher.invalidate();
        }
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        Ok(Some(BoundedLine {
            bytes,
            truncated,
            consumed,
            terminated,
        }))
    }

    fn into_blocks(self) -> (Vec<BlockRow>, bool) {
        self.hasher.finish()
    }
}

struct WorkspaceResolver {
    host_id: String,
    cache: HashMap<String, Option<Workspace>>,
}

impl WorkspaceResolver {
    fn new(host_id: &str) -> Self {
        Self {
            host_id: host_id.to_string(),
            cache: HashMap::new(),
        }
    }

    fn resolve(&mut self, path: &str) -> Option<Workspace> {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let resolved = resolve_workspace_path(&self.host_id, path);
        let workspace = if resolved.status == PathResolutionStatus::Resolved {
            workspace_from_resolved(&resolved).ok()
        } else {
            None
        };
        self.cache.insert(path.to_string(), workspace.clone());
        workspace
    }

    fn resolve_many(&mut self, paths: &[String]) -> Vec<Workspace> {
        let mut out: Vec<Workspace> = Vec::new();
        for path in paths.iter().take(8) {
            if let Some(workspace) = self.resolve(path) {
                if !out.iter().any(|existing: &Workspace| {
                    existing.path.canonical_path == workspace.path.canonical_path
                }) {
                    out.push(workspace);
                }
            }
        }
        out
    }
}

pub struct ScanInput<'a> {
    pub service: &'a MultiCliService,
    pub source: &'a SourceRow,
    pub options: ScanOptions,
    /// Must be cheap, non-blocking and must never touch the store: the scanner
    /// can ask while it holds the only database connection.
    pub cancel: &'a dyn Fn() -> bool,
}

/// Runs one bounded pass over a configured source.
///
/// The pass has two phases. Imports come first and commit whatever appeared,
/// grew or was replaced. Verification comes second: it compares one window of
/// one file's blocks, walking the corpus in a persisted rotation and spending a
/// reserved slice of the budget. Keeping them apart is what lets a file that is
/// appended to on every pass still be verified — while both shared one loop, a
/// growing file always took the append branch and its committed prefix was
/// never compared.
pub fn scan_source(
    input: ScanInput<'_>,
    mut progress: impl FnMut(&ScanProgress),
) -> Result<ScanReport, AppError> {
    let product = product_for_agent(&input.source.agent_id).ok_or_else(|| {
        AppError::new(
            ErrorCode::Unsupported,
            format!("no history parser for agent {}", input.source.agent_id),
        )
        .with_details(json!({ "stage": "historyScan", "agentId": input.source.agent_id }))
    })?;
    let root = PathBuf::from(&input.source.root_canonical_path);
    // A source can predate the rule (or be restored from a backup). The walk
    // skips protected children, but a root that *is* inside a protected
    // directory must be refused outright.
    if let Some(name) = guard::protected_in(&guard::components_of(&root)) {
        return Err(AppError::new(
            ErrorCode::AccessDenied,
            "this history root is inside a protected directory",
        )
        .with_details(json!({
            "stage": "historyScan",
            "sourceId": input.source.id,
            "reason": "protected-directory",
            "matched": name,
        }))
        .with_retryable(false));
    }
    if !root.is_dir() {
        return Err(AppError::new(
            ErrorCode::Io,
            "the configured history root is not an accessible directory",
        )
        .with_details(json!({
            "stage": "historyScan",
            "sourceId": input.source.id,
            "reason": "rootUnavailable",
        }))
        .with_retryable(true));
    }

    let mut issues: Vec<ScanIssue> = Vec::new();
    // Listing is bounded on its own (`max_files` plus the visit budget) and is
    // deliberately not charged to the parse budget. Charging it here made every
    // pass of a 10k-file root spend most of its 4 s in `read_dir` and import
    // only a couple of hundred files, so the pass could never reach the end.
    let (files, listing_truncated, listing_incomplete) = collect_files(
        &root,
        &input.source.root_canonical_path,
        product.extensions,
        input.options.max_files,
        &mut issues,
    );
    let started = Instant::now();
    let budget = input.options.budget_ms.max(500);
    let deadline = started + Duration::from_millis(budget);
    // An import backlog must not consume the whole pass: without a reserved
    // slice, verification never runs on a corpus that keeps growing and the
    // bound on an unchanged prefix would depend on the user stopping work.
    let reserve = Duration::from_millis(VERIFY_RESERVE_MS.min(budget / 4));
    let import_deadline = deadline.checked_sub(reserve).unwrap_or(started);
    let mut report = ScanReport {
        listed_files: files.len(),
        ..Default::default()
    };
    report.progress.files_seen = files.len() as u64;
    progress(&report.progress);

    let mut resolver = WorkspaceResolver::new(&input.source.host_id);
    // Every listed path, not just the visited slice: a cursor is stale only when
    // its file is gone from the listing, never because this pass started later
    // in the list.
    let listed_paths: Vec<String> = files.iter().map(|file| file.canonical.clone()).collect();
    let mut stopped_early = false;
    let namespace = source_namespace(product, &input.source.config_profile_id);
    let cursors = {
        let conn = input.service.store().connection().lock();
        repo::cursors_for_source(&conn, &input.source.id)?
    };
    let total = files.len();
    let start = if total == 0 {
        0
    } else {
        input.options.start_index % total
    };
    let mut next_index = start;
    let mut traversed_all = total == 0;
    // A positive budget below one block could never compare anything (a block
    // is the smallest unit with a stored hash), which left a file permanently
    // "pending" without ever being checked. Zero still disables verification.
    let mut verify_budget = if input.options.verify_budget_bytes == 0 {
        0
    } else {
        input.options.verify_budget_bytes.max(BLOCK_BYTES)
    };

    for offset in 0..total {
        let index = (start + offset) % total;
        let file = &files[index];
        if (input.cancel)() {
            report.cancelled = true;
            stopped_early = true;
            break;
        }
        if Instant::now() >= import_deadline {
            stopped_early = true;
            break;
        }
        let outcome = process_file(FileInput {
            service: input.service,
            source: input.source,
            product,
            namespace: &namespace,
            file,
            cursor: cursors.get(&file.canonical).cloned(),
            index: index as u64,
            options: &input.options,
            deadline: &import_deadline,
            cancel: input.cancel,
            resolver: &mut resolver,
            progress: &mut report.progress,
            force_full: false,
        });
        match outcome {
            Ok(outcome) => {
                if record_outcome(&mut report, &mut issues, outcome, true) {
                    report.cancelled = true;
                    stopped_early = true;
                }
            }
            Err(error) => record_error(&mut report, &mut issues, index as u64, &error),
        }
        next_index = (index + 1) % total;
        if offset + 1 == total {
            traversed_all = true;
        }
        progress(&report.progress);
    }
    report.next_index = if traversed_all { 0 } else { next_index };

    if traversed_all && !listing_truncated && !listing_incomplete {
        let mut conn = input.service.store().connection().lock();
        let (deleted, purged) =
            repo::purge_missing_cursors(&mut conn, &input.source.id, &listed_paths)?;
        report.deleted_cursors = deleted;
        report.purged_messages += purged;
        drop(conn);
    }

    // ---- phase 2: bounded content verification -----------------------------
    //
    // Files are visited in rotation order, each taking windows up to its fair
    // share of the budget, until the budget or the pass deadline runs out. A
    // changed block forces a transactional rebuild of that file through the
    // same import path, so provenance, FTS rows and session identity stay
    // consistent.
    if verify_budget > 0 {
        let rotation = {
            let conn = input.service.store().connection().lock();
            repo::verify_path(&conn, &input.source.id)?
        };
        let order = verification_order(&files, &cursors, rotation.as_deref());
        // Fair share of one pass for one file, never below a single window.
        let share = (verify_budget / (order.len().max(1) as u64))
            .max(VERIFY_WINDOW_BYTES.min(verify_budget));
        let mut last_visited: Option<String> = None;
        for index in order {
            if verify_budget == 0 || (input.cancel)() || Instant::now() >= deadline {
                break;
            }
            let file = &files[index];
            last_visited = Some(file.canonical.clone());
            let compressed = is_compressed(&file.path);
            let loaded = {
                let conn = input.service.store().connection().lock();
                let Some(cursor) =
                    repo::read_cursor_by_path(&conn, &input.source.id, &file.canonical)?
                else {
                    continue;
                };
                if !cursor.read_complete {
                    // Still catching up on its own bytes: its committed prefix
                    // is not final, so comparing it would race the import.
                    continue;
                }
                let layer = if compressed {
                    BlockLayer::Physical
                } else {
                    BlockLayer::Logical
                };
                let blocks =
                    repo::read_file_blocks(&conn, &input.source.id, &cursor.file_identity, layer)?;
                (cursor, blocks)
            };
            let (cursor, blocks) = loaded;

            // A compressed cursor written before the physical layer has no
            // baseline at all. Re-reading is the only honest way to build one
            // from bytes that were actually decoded; it happens once per file.
            if compressed && blocks.is_empty() && file.size > MAX_COMPRESSED_PHYSICAL_BYTES {
                // Phase 1 already refused it with `compressed-too-large`; asking
                // for a baseline here would report the same failure twice.
                continue;
            }
            if compressed && blocks.is_empty() {
                let outcome = process_file(FileInput {
                    service: input.service,
                    source: input.source,
                    product,
                    namespace: &namespace,
                    file,
                    cursor: Some(cursor),
                    index: index as u64,
                    options: &input.options,
                    deadline: &deadline,
                    cancel: input.cancel,
                    resolver: &mut resolver,
                    progress: &mut report.progress,
                    force_full: true,
                });
                match outcome {
                    Ok(outcome) => {
                        if record_outcome(&mut report, &mut issues, outcome, false) {
                            report.cancelled = true;
                            break;
                        }
                    }
                    Err(error) => record_error(&mut report, &mut issues, index as u64, &error),
                }
                continue;
            }
            if blocks.is_empty() {
                // A file whose block table was discarded (an over-long line
                // made the byte stream unhashable) relies on size, mtime and
                // the prefix hash, and already reports a line diagnostic.
                continue;
            }

            let same_size = file.size == cursor.size_bytes;
            let same_mtime = file.mtime_ms == cursor.mtime_ms;
            // A moved mtime on an unchanged length is the signature of an
            // in-place rewrite, so it never takes the cooldown exit. A file
            // that merely grew keeps its cooldown: the bytes that were just
            // appended were written by us and do not need re-checking now.
            let rewrite_suspected = same_size && !same_mtime;
            let now = crate::multicli::store::now_ms();
            if !rewrite_suspected
                && cursor.verify_offset == 0
                && cursor.verified_at_ms != 0
                && now.saturating_sub(cursor.verified_at_ms) < VERIFY_COOLDOWN_MS
            {
                continue;
            }

            let size = if compressed {
                cursor.size_bytes
            } else {
                cursor.committed_offset
            };
            // One file may take more than one window while the budget lasts,
            // up to its fair share. A single large file would otherwise verify
            // 1 MiB per pass and need hundreds of passes for one cycle, while
            // the rest of the budget sat unused.
            let mut offset = cursor.verify_offset;
            let mut verified_bytes = 0_u64;
            let mut changed = false;
            let mut deferred = false;
            loop {
                let before = verify_budget;
                // One unreadable file must not turn the whole pass into an
                // error: report it and move on to the next file.
                let window = match verify_window(
                    &file.path,
                    size,
                    offset,
                    &blocks,
                    &mut verify_budget,
                    input.cancel,
                    &deadline,
                ) {
                    Ok(window) => window,
                    Err(error) => {
                        record_error(&mut report, &mut issues, index as u64, &error);
                        break;
                    }
                };
                match window {
                    VerifyResult::Clean {
                        next_offset,
                        bytes,
                    } => {
                        {
                            let conn = input.service.store().connection().lock();
                            repo::set_verify_progress(
                                &conn,
                                &input.source.id,
                                &cursor.file_identity,
                                next_offset,
                                file.mtime_ms,
                            )?;
                        }
                        verified_bytes += bytes;
                        offset = next_offset;
                        if next_offset == 0 {
                            // The cycle is complete; `set_verify_progress` has
                            // already stamped `verified_at`.
                            break;
                        }
                        if verify_budget == 0
                            || before.saturating_sub(verify_budget) >= share
                            || Instant::now() >= deadline
                        {
                            break;
                        }
                    }
                    VerifyResult::Changed => {
                        changed = true;
                        break;
                    }
                    VerifyResult::Deferred => {
                        deferred = true;
                        break;
                    }
                    VerifyResult::Unavailable => break,
                }
            }
            if verified_bytes > 0 {
                report.progress.verified_bytes += verified_bytes;
                report.progress.files_verified += 1;
            }
            if deferred {
                if (input.cancel)() {
                    report.cancelled = true;
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                // The rest of the budget may still fit a smaller file.
                continue;
            }
            if changed {
                let outcome = process_file(FileInput {
                    service: input.service,
                    source: input.source,
                    product,
                    namespace: &namespace,
                    file,
                    cursor: Some(cursor),
                    index: index as u64,
                    options: &input.options,
                    deadline: &deadline,
                    cancel: input.cancel,
                    resolver: &mut resolver,
                    progress: &mut report.progress,
                    force_full: true,
                });
                match outcome {
                    Ok(outcome) => {
                        if record_outcome(&mut report, &mut issues, outcome, false) {
                            report.cancelled = true;
                            break;
                        }
                    }
                    Err(error) => record_error(&mut report, &mut issues, index as u64, &error),
                }
            }
        }
        if let Some(path) = last_visited {
            let conn = input.service.store().connection().lock();
            repo::set_verify_path(&conn, &input.source.id, &path)?;
        }
    }

    // Rejected records stay attached to their cursor; a later pass that skips
    // the file must still report them. Read the fresh counters (this pass may
    // have repaired one) and count only files that are still listed — a cursor
    // kept because the listing was incomplete must not pin the source to
    // `partial` forever.
    report.unresolved_line_errors = {
        let conn = input.service.store().connection().lock();
        let fresh = repo::cursors_for_source(&conn, &input.source.id)?;
        let listed: std::collections::HashSet<&str> =
            listed_paths.iter().map(String::as_str).collect();
        fresh
            .values()
            .filter(|cursor| listed.contains(cursor.canonical_path.as_str()))
            .map(|cursor| cursor.error_count)
            .sum()
    };

    // Every cursor that has not completed a verification cycle, whether or not
    // this pass reached it. Counting only the visited files made a large corpus
    // report "nothing pending" while most of it had never been compared.
    report.pending_verification = {
        let conn = input.service.store().connection().lock();
        // A file counts as pending when it has a baseline and its content has
        // changed since that baseline was last fully compared: never verified,
        // a cycle in progress, or a write after the last completed cycle (an
        // append moves `updated_at_ms` without moving `verified_at_ms`).
        let pending: i64 = conn.query_row(
            "SELECT COUNT(*) FROM history_cursors c
             WHERE c.source_id = ?1
               AND EXISTS (
                   SELECT 1 FROM history_file_blocks b
                   WHERE b.source_id = c.source_id
                     AND b.file_identity = c.file_identity
               )
               AND (c.verify_offset != 0
                    OR c.verified_at_ms = 0
                    OR c.verified_at_ms < c.updated_at_ms)",
            [input.source.id.as_str()],
            |row| row.get(0),
        )?;
        u64::try_from(pending).unwrap_or(0)
    };

    report.complete = traversed_all
        && !stopped_early
        && !listing_truncated
        && !listing_incomplete
        && report.progress.files_incomplete == 0
        // A rejected file is not imported either. `complete` means "every
        // listed file is represented in the index", not "the walk finished".
        && report.progress.files_failed == 0
        // A rejected or isolated record means not everything on disk is in the
        // index; the pass must not claim to be complete.
        && report.progress.line_errors == 0
        && report.unresolved_line_errors == 0;
    report.issues = issues;
    Ok(report)
}

/// Folds one file outcome into the report. Returns true when the pass stopped
/// on a cancel, which the caller turns into `stopped_early`.
fn record_outcome(
    report: &mut ScanReport,
    issues: &mut Vec<ScanIssue>,
    outcome: FileOutcome,
    count_as_done: bool,
) -> bool {
    match outcome {
        FileOutcome::Imported {
            messages,
            bytes,
            purged,
            incomplete,
            line_errors,
            issues: file_issues,
        } => {
            // A file rebuilt by the verification phase was already counted when
            // the import phase looked at it; counting it again would report more
            // files than the listing holds.
            if count_as_done {
                report.progress.files_done += 1;
            }
            report.progress.messages += messages;
            report.progress.bytes += bytes;
            report.purged_messages += purged;
            report.progress.line_errors += line_errors;
            if incomplete {
                report.progress.files_incomplete += 1;
            }
            for issue in file_issues {
                if issues.len() < 64 {
                    issues.push(issue);
                }
            }
            false
        }
        FileOutcome::Skipped {
            verified_bytes,
            pending_verification,
        } => {
            report.progress.files_skipped += 1;
            report.progress.verified_bytes += verified_bytes;
            if verified_bytes > 0 {
                report.progress.files_verified += 1;
            }
            if pending_verification {
                report.pending_verification += 1;
            }
            false
        }
        FileOutcome::Cancelled {
            messages,
            bytes,
            purged,
        } => {
            if count_as_done {
                report.progress.files_done += 1;
            }
            report.progress.messages += messages;
            report.progress.bytes += bytes;
            report.purged_messages += purged;
            true
        }
    }
}

fn record_error(
    report: &mut ScanReport,
    issues: &mut Vec<ScanIssue>,
    file_index: u64,
    error: &AppError,
) {
    report.progress.files_failed += 1;
    if issues.len() < 64 {
        issues.push(ScanIssue {
            code: error.code.as_str().to_string(),
            file_index,
            line: error.details.get("line").and_then(Value::as_u64),
            detail: error
                .details
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("file-not-imported")
                .to_string(),
        });
    }
}

/// Files eligible for verification, in rotation order.
///
/// The walk starts strictly after the persisted position and wraps. Without
/// this the shared budget always went to the first paths in the listing: with a
/// small budget the tail of a corpus never got a turn, and one large file whose
/// cycle was in progress could hold the budget on every pass.
fn verification_order(
    files: &[FileCandidate],
    cursors: &HashMap<String, CursorRow>,
    rotation: Option<&str>,
) -> Vec<usize> {
    let mut order: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(_, file)| {
            cursors
                .get(&file.canonical)
                .map(|cursor| cursor.read_complete)
                .unwrap_or(false)
        })
        .map(|(index, _)| index)
        .collect();
    // `files` is sorted by canonical path; make that explicit for the rotation.
    order.sort_by(|left, right| files[*left].canonical.cmp(&files[*right].canonical));
    if let Some(rotation) = rotation {
        if let Some(position) = order
            .iter()
            .position(|index| files[*index].canonical.as_str() > rotation)
        {
            order.rotate_left(position);
        } else {
            // Past the end of the listing: start over.
        }
    }
    order
}

enum FileOutcome {
    Imported {
        messages: u64,
        bytes: u64,
        purged: u64,
        incomplete: bool,
        line_errors: u64,
        issues: Vec<ScanIssue>,
    },
    Skipped {
        verified_bytes: u64,
        pending_verification: bool,
    },
    Cancelled {
        messages: u64,
        bytes: u64,
        purged: u64,
    },
}

struct FileInput<'a> {
    service: &'a MultiCliService,
    source: &'a SourceRow,
    product: &'static HistoryProduct,
    namespace: &'a str,
    file: &'a FileCandidate,
    cursor: Option<CursorRow>,
    index: u64,
    options: &'a ScanOptions,
    deadline: &'a Instant,
    cancel: &'a dyn Fn() -> bool,
    resolver: &'a mut WorkspaceResolver,
    progress: &'a mut ScanProgress,
    /// Set by the verification phase when a block no longer matches: the file
    /// is rebuilt from byte zero even though its size and mtime look unchanged.
    force_full: bool,
}

fn process_file(input: FileInput<'_>) -> Result<FileOutcome, AppError> {
    let FileInput {
        service,
        source,
        product,
        namespace,
        file,
        cursor,
        index,
        options,
        deadline,
        cancel,
        resolver,
        progress,
        force_full,
    } = input;

    // The listing skipped links, but a path can be swapped for one between the
    // listing and this read. Re-check without following; a file that became a
    // reparse point is reported and not read through.
    if let Ok(metadata) = std::fs::symlink_metadata(&file.path) {
        if is_reparse_or_symlink(&metadata) {
            return Err(AppError::new(
                ErrorCode::AccessDenied,
                "a history file became a link after it was listed; it is skipped",
            )
            .with_details(json!({ "stage": "historyScan", "reason": "reparse-point" })));
        }
    }
    let compressed = is_compressed(&file.path);
    // A compressed transcript is verified through a baseline built from its
    // physical bytes, which means one sequential read of the file inside the
    // import transaction. Accepting a file above that bound would mean
    // importing it and then never being able to prove it unchanged, so it is
    // refused here, before anything is decoded or purged, and the rows from an
    // earlier import stay as they are.
    if compressed && file.size > MAX_COMPRESSED_PHYSICAL_BYTES {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "the compressed transcript is larger than the supported physical size",
        )
        .with_details(json!({
            "stage": "historyScan",
            "reason": "compressed-too-large",
            "fileIndex": index,
            "limitBytes": MAX_COMPRESSED_PHYSICAL_BYTES,
        }))
        .with_retryable(false));
    }
    let head = if file.size == 0 {
        Vec::new()
    } else {
        read_head(&file.path).map_err(|error| file_error(error.kind().to_string(), None))?
    };
    // Path identity: an append must not orphan the cursor of a growing file.
    let identity = ids::path_identity(&file.canonical);

    let mut conn = service.store().connection().lock();
    if file.size == 0 {
        // Truncating a file to zero removes what the old bytes produced. A file
        // that was always empty has no cursor and is simply skipped.
        let Some(cursor) = cursor else {
            return Ok(FileOutcome::Skipped {
                verified_bytes: 0,
                pending_verification: false,
            });
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let purged = repo::purge_file_derived(&tx, &source.id, &cursor.file_identity)?;
        repo::delete_cursor(&tx, &source.id, &cursor.file_identity)?;
        tx.commit()?;
        return Ok(FileOutcome::Imported {
            messages: 0,
            bytes: 0,
            purged,
            incomplete: false,
            line_errors: 0,
            issues: Vec::new(),
        });
    }
    let mut decision = if force_full {
        Decision::Full
    } else {
        decide(file, &head, cursor.as_ref(), compressed)
    };

    if decision == Decision::Skip {
        // Unchanged by size, mtime and prefix hash. The bytes are still checked
        // by the verification phase; this pass has nothing to import.
        return Ok(FileOutcome::Skipped {
            verified_bytes: 0,
            pending_verification: false,
        });
    }

    if decision == Decision::Append && !compressed {
        // The append starts at `committed_offset`; if the bytes just before it
        // changed, the block that contains them would be re-hashed from the new
        // content and the rewrite would never be seen. Compare first and fall
        // back to a full read.
        //
        // A compressed cursor's offset is a *logical* (decoded) offset, so it
        // cannot be compared against physical file bytes; that path re-decodes
        // and compares whole blocks instead.
        if let Some(cursor_ref) = cursor.as_ref() {
            let blocks = repo::read_file_blocks(
                &conn,
                &source.id,
                &cursor_ref.file_identity,
                BlockLayer::Logical,
            )?;
            if !seed_matches(&file.path, cursor_ref.committed_offset, &blocks)? {
                decision = Decision::Full;
            }
        }
    }

    let full = decision == Decision::Full;
    let start_offset = if full {
        0
    } else {
        cursor
            .as_ref()
            .map(|cursor| cursor.committed_offset)
            .unwrap_or(0)
    };

    let file_deadline = {
        let own = Instant::now() + Duration::from_millis(options.file_budget_ms.max(200));
        if own < *deadline {
            own
        } else {
            *deadline
        }
    };

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut purged = 0_u64;
    if full {
        // The old rows no longer describe the bytes on disk. The purge is inside
        // the same transaction as the re-import, so a failed re-read cannot
        // leave the file emptied out.
        purged += repo::purge_file_derived(&tx, &source.id, &identity)?;
        if let Some(stale) = cursor
            .as_ref()
            .map(|cursor| cursor.file_identity.clone())
            .filter(|stale| stale != &identity)
        {
            purged += repo::purge_file_derived(&tx, &source.id, &stale)?;
            repo::delete_cursor(&tx, &source.id, &stale)?;
        }
    }

    // The physical baseline of a compressed transcript is built before the
    // decode: it is one sequential read of the bytes as they are, and a change
    // between this read and the decode can only make the next pass re-read the
    // file (its size or mtime moved), never silently accept a rewrite.
    let (physical_blocks, physical_bytes) = if compressed {
        match hash_physical(
            &file.path,
            MAX_COMPRESSED_PHYSICAL_BYTES,
            cancel,
            &file_deadline,
        )? {
            PhysicalBaseline::Complete(blocks, covered) => (Some(blocks), covered),
            PhysicalBaseline::TooLarge => {
                // The listing size was stale and the file grew past the accepted
                // size. Nothing is purged.
                tx.rollback()?;
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "the compressed transcript is larger than the supported physical size",
                )
                .with_details(json!({
                    "stage": "historyScan",
                    "reason": "compressed-too-large",
                    "fileIndex": index,
                    "limitBytes": MAX_COMPRESSED_PHYSICAL_BYTES,
                }))
                .with_retryable(false));
            }
            PhysicalBaseline::Interrupted => {
                // Storing a partial baseline would later look like a change, so
                // nothing is committed. This is reported rather than silently
                // retried forever: a file that cannot be hashed within its
                // transaction budget must be visible in the source status.
                tx.rollback()?;
                if (cancel)() {
                    return Ok(FileOutcome::Cancelled {
                        messages: 0,
                        bytes: 0,
                        purged: 0,
                    });
                }
                return Err(AppError::new(
                    ErrorCode::Io,
                    "the integrity baseline of the compressed transcript could not be built in this pass",
                )
                .with_details(json!({
                    "stage": "historyScan",
                    "reason": "physical-baseline-incomplete",
                    "fileIndex": index,
                }))
                .with_retryable(true));
            }
        }
    } else {
        (None, file.size)
    };

    let mut parser = parser_for_agent(&product.agent_id).ok_or_else(|| {
        AppError::new(ErrorCode::Unsupported, "no parser for this agent")
            .with_details(json!({ "stage": "historyScan" }))
    })?;
    // Restore the state the previous pass derived from the file prefix. Without
    // it an append starts with no native session id, invents one from the file
    // name, and splits one session into two.
    // A full read starts at byte zero and must derive all parser state again.
    // Restoring the old title/cwd state here would keep metadata from bytes
    // that were replaced, even while messages themselves were re-imported.
    let checkpoint = if full {
        ParserCheckpoint::default()
    } else {
        cursor
            .as_ref()
            .and_then(parse_checkpoint)
            .unwrap_or_default()
    };
    parser.restore(&checkpoint);

    let file_mtime = ids::iso_from_millis(file.mtime_ms);
    let file_stem = file
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("history")
        .trim_end_matches(".jsonl")
        .to_string();

    let mut state = StreamState {
        source,
        namespace,
        identity: &identity,
        parser: parser.as_mut(),
        file_mtime: &file_mtime,
        file_stem: &file_stem,
        base_offset: start_offset,
        // A full re-read starts at byte 0, so the line counter must restart
        // with it: continuing from the old value would renumber every fallback
        // source key and change message ids on a rewrite.
        line_index: if full {
            0
        } else {
            cursor
                .as_ref()
                .map(|cursor| cursor.lines_committed)
                .unwrap_or(0)
        },
        bytes: 0,
        errors: Vec::new(),
        // A resumed file already passed format detection in an earlier pass.
        format: FormatProbe {
            checked: 0,
            matched: start_offset > 0,
        },
        meta_cwd: checkpoint.cwd.clone(),
        meta_roots: checkpoint.extra_roots.clone(),
        meta_native_id: checkpoint.native_session_id.clone(),
        pending_title: checkpoint
            .pending_title
            .as_ref()
            .and_then(|pending| {
                title_source(&pending.source).map(|source| (pending.title.clone(), source))
            }),
        resolver,
        max_bytes: options.max_bytes_per_file,
        deadline: &file_deadline,
        cancel,
        file_index: index,
        format_error: None,
        decompressed: 0,
    };
    let streamed = stream_file(&tx, &mut state, &file.path, start_offset, compressed)?;

    if let Some(error) = state.format_error.take() {
        tx.rollback()?;
        return Err(error);
    }
    if streamed.interrupted {
        // The pass stopped before reading a byte (a compressed skip hit its
        // deadline or a cancel). Leave the cursor and its block table exactly
        // as they were and report the file as unfinished. A cancel is reported
        // by the caller's own check, so this path must not claim one.
        tx.rollback()?;
        return Ok(FileOutcome::Imported {
            messages: 0,
            bytes: 0,
            purged: 0,
            incomplete: true,
            line_errors: 0,
            issues: if (cancel)() {
                Vec::new()
            } else {
                vec![ScanIssue {
                    code: "compressed-resume-deferred".to_string(),
                    file_index: index,
                    line: None,
                    detail: "the committed prefix could not be decoded within this pass".to_string(),
                }]
            },
        });
    }

    // A full re-read starts from byte 0, so previous line errors describe bytes
    // that no longer exist. Keeping them would make a repaired file look broken
    // forever.
    let mut error_count = if full {
        0
    } else {
        cursor.as_ref().map(|cursor| cursor.error_count).unwrap_or(0)
    };
    let mut last_error = cursor.as_ref().and_then(|cursor| cursor.last_error_json.clone());
    let mut file_issues = Vec::new();
    if !state.errors.is_empty() {
        error_count += state.errors.len() as u64;
        last_error = Some(
            serde_json::to_string(&json!({
                "code": state.errors[0].0,
                "detail": state.errors[0].1,
                "line": state.errors[0].2,
            }))
            .unwrap_or_else(|_| "{}".to_string()),
        );
        // A bad row is a real diagnostic, not a private cursor counter: the
        // source status must not report a clean pass while a line was rejected.
        for (code, detail, line) in state.errors.iter().take(8) {
            file_issues.push(ScanIssue {
                code: (*code).to_string(),
                file_index: index,
                line: Some(*line),
                detail: detail.clone(),
            });
        }
    }

    let committed_offset = start_offset.saturating_add(streamed.consumed);
    if streamed.block_table_valid {
        if let Some(first) = streamed.blocks.first().map(|block| block.index) {
            repo::write_file_blocks(
                &tx,
                &source.id,
                &identity,
                BlockLayer::Logical,
                first,
                &streamed.blocks,
            )?;
        }
    } else {
        // Only the decoded layer is wrong; a physical baseline still describes
        // the file and stays usable.
        repo::delete_block_layer(&tx, &source.id, &identity, BlockLayer::Logical)?;
    }
    if let Some(blocks) = physical_blocks {
        repo::write_file_blocks(&tx, &source.id, &identity, BlockLayer::Physical, 0, &blocks)?;
    }

    // An append re-reads only the new bytes and proved the block before the
    // offset still matches (seed check), so the verification cycle that already
    // covered the prefix stays valid. Resetting it made a continuously growing
    // file restart verification on every pass and never finish a cycle.
    //
    // A compressed re-read rebuilds both baselines from the bytes it just read,
    // so its old cycle no longer refers to anything.
    let (verify_offset, verified_at_ms) = if full || compressed {
        (0, 0)
    } else {
        (
            cursor
                .as_ref()
                .map(|cursor| cursor.verify_offset.min(committed_offset))
                .unwrap_or(0),
            cursor
                .as_ref()
                .map(|cursor| cursor.verified_at_ms)
                .unwrap_or(0),
        )
    };
    let cursor_row = CursorRow {
        source_id: source.id.clone(),
        file_identity: identity.clone(),
        display_path: file.display.clone(),
        canonical_path: file.canonical.clone(),
        // For a compressed file this is the length the physical baseline
        // covers, not the length the directory listing reported.
        size_bytes: physical_bytes,
        mtime_ms: file.mtime_ms,
        head_hash: ids::prefix_hash(&head, file.size),
        anchor_hash: streamed.anchor_hash.clone(),
        committed_offset,
        tail_residue: streamed.residue.clone(),
        tail_residue_truncated: streamed.residue_truncated,
        parser_version: HISTORY_PARSER_VERSION,
        error_count,
        last_error_json: last_error,
        lines_committed: state.line_index,
        read_complete: streamed.eof,
        parser_state_json: serde_json::to_string(&state.checkpoint()).ok(),
        verify_offset,
        verified_at_ms,
    };
    repo::write_cursor(&tx, &cursor_row)?;
    let line_errors = state.errors.len() as u64;
    tx.commit()?;
    drop(conn);

    progress.files_seen = progress.files_seen.max(index + 1);
    if streamed.cancelled {
        return Ok(FileOutcome::Cancelled {
            messages: streamed.messages,
            bytes: streamed.bytes,
            purged,
        });
    }
    Ok(FileOutcome::Imported {
        messages: streamed.messages,
        bytes: streamed.bytes,
        purged,
        incomplete: !streamed.eof,
        line_errors,
        issues: file_issues,
    })
}

enum PhysicalBaseline {
    Complete(Vec<BlockRow>, u64),
    /// The pass deadline or a cancel stopped the read.
    Interrupted,
    /// The file is larger than the accepted physical size.
    TooLarge,
}

/// Hashes the physical bytes of a compressed transcript, one block at a time.
///
/// Returns the blocks plus the number of bytes they cover. The cursor stores
/// that length rather than the size seen while listing, so a file written
/// between the listing and this read cannot make the next pass look for a block
/// that was never hashed.
///
/// The read is bounded three ways: by `limit` (the accepted physical size, so a
/// file that grew after the listing cannot be read without bound), by the file
/// transaction's deadline, and by the cancel flag. A partial baseline is never
/// returned — it would later look like a change.
fn hash_physical(
    path: &Path,
    limit: u64,
    cancel: &dyn Fn() -> bool,
    deadline: &Instant,
) -> Result<PhysicalBaseline, AppError> {
    let mut file = File::open(path).map_err(|error| file_error(error.kind().to_string(), None))?;
    let mut hasher = BlockHasher::new(0);
    let mut buffer = vec![0_u8; BLOCK_BYTES as usize];
    let mut covered = 0_u64;
    loop {
        if cancel() || Instant::now() >= *deadline {
            return Ok(PhysicalBaseline::Interrupted);
        }
        let read = file
            .read(&mut buffer)
            .map_err(|error| file_error(error.kind().to_string(), None))?;
        if read == 0 {
            break;
        }
        covered += read as u64;
        if covered > limit {
            return Ok(PhysicalBaseline::TooLarge);
        }
        hasher.update(&buffer[..read]);
    }
    let (blocks, valid) = hasher.finish();
    Ok(if valid {
        PhysicalBaseline::Complete(blocks, covered)
    } else {
        PhysicalBaseline::Interrupted
    })
}

fn title_source(text: &str) -> Option<TitleSource> {
    match text {
        "native-title" => Some(TitleSource::NativeTitle),
        "first-user-message" => Some(TitleSource::FirstUserMessage),
        "none" => Some(TitleSource::None),
        _ => None,
    }
}

fn file_error(reason: String, line: Option<u64>) -> AppError {
    AppError::new(ErrorCode::Io, "history file could not be read")
        .with_details(json!({ "stage": "historyScan", "reason": reason, "line": line }))
        .with_retryable(true)
}

#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Unchanged by size, mtime and prefix hash: nothing to import. The bytes
    /// are still compared by the verification phase.
    Skip,
    Append,
    Full,
}

/// Decides whether a file can be appended to or must be read from the start.
///
/// Size/mtime alone are not enough: a same-size rewrite keeps both, and that
/// case is deliberately *not* decided here — it resolves to `Skip` and is
/// caught by the byte comparison in the verification phase. What this function
/// owns is byte-level identity: a changed prefix hash, a shrunk file, a stale
/// parser version and an incomplete previous read.
fn decide(
    file: &FileCandidate,
    head: &[u8],
    cursor: Option<&CursorRow>,
    compressed: bool,
) -> Decision {
    let Some(cursor) = cursor else {
        return Decision::Full;
    };
    if cursor.parser_version != HISTORY_PARSER_VERSION {
        return Decision::Full;
    }
    // A cursor written before v5 has no parser checkpoint and no line counter;
    // re-reading is the only honest option, and it happens once. A checkpoint
    // that cannot be parsed is treated the same way rather than silently
    // falling back to the file name.
    if parse_checkpoint(cursor).is_none() {
        return Decision::Full;
    }
    if compressed {
        if file.size != cursor.size_bytes || file.mtime_ms != cursor.mtime_ms {
            // A compressed stream is decoded as a whole: its logical offset is
            // not a file offset, so any physical change means a re-read.
            return Decision::Full;
        }
        if !cursor.read_complete {
            return Decision::Append;
        }
        return Decision::Skip;
    }
    if cursor.committed_offset > file.size
        || ids::prefix_hash(head, cursor.size_bytes) != cursor.head_hash
    {
        return Decision::Full;
    }
    if !cursor.read_complete {
        // Stopped on a byte or time budget. Continue from the committed offset
        // even though size and mtime are unchanged.
        return Decision::Append;
    }
    if file.size == cursor.size_bytes {
        // Same length: only the bytes can prove it is the same content, and the
        // verification phase is where that happens.
        return Decision::Skip;
    }
    if file.size < cursor.committed_offset {
        return Decision::Full;
    }
    // A rewrite that keeps the mtime but changes the size, or a plain append:
    // both continue from the committed offset. The anchor is not re-checked
    // because the verification block that contains it is re-hashed by the
    // append itself; a middle rewrite behind the offset is caught by the next
    // verification cycle, which restarts at zero for this file.
    Decision::Append
}

/// Parsed resume state, or `None` when the cursor has none or it is corrupt.
fn parse_checkpoint(cursor: &CursorRow) -> Option<ParserCheckpoint> {
    cursor
        .parser_state_json
        .as_deref()
        .and_then(|text| serde_json::from_str::<ParserCheckpoint>(text).ok())
}

/// Compares the last partially filled block before `committed_offset` with the
/// stored hash for that block.
///
/// Returns false when the block is missing or its bytes changed, which makes
/// the caller re-read the file. A zero-length seed (the offset sits exactly on
/// a block boundary) has nothing to compare and is accepted.
fn seed_matches(
    path: &Path,
    committed_offset: u64,
    blocks: &[BlockRow],
) -> Result<bool, AppError> {
    if committed_offset == 0 {
        return Ok(true);
    }
    let index = committed_offset / BLOCK_BYTES;
    let block_start = index * BLOCK_BYTES;
    let length = committed_offset - block_start;
    if length == 0 {
        return Ok(true);
    }
    let Some(stored) = blocks.iter().find(|block| block.index == index) else {
        return Ok(false);
    };
    if stored.bytes != length {
        return Ok(false);
    }
    let mut file = File::open(path).map_err(|error| file_error(error.kind().to_string(), None))?;
    let hash = hash_range(&mut file, block_start, length)?;
    Ok(hash == stored.hash)
}

enum VerifyResult {
    Clean { next_offset: u64, bytes: u64 },
    Changed,
    Deferred,
    /// No block table exists (a file whose over-long line made its byte stream
    /// unhashable keeps no logical table), so a byte-level comparison is
    /// impossible; the file relies on size, mtime and the prefix hash, and
    /// already reports a line diagnostic.
    Unavailable,
}

/// Compares one bounded window of a file's blocks.
///
/// `size` is the number of bytes the block table covers: the committed offset
/// for a plain transcript, the physical length for a compressed one. Both are
/// read straight from the file at block boundaries, so the same window model
/// applies to either.
fn verify_window(
    path: &Path,
    size: u64,
    verify_offset: u64,
    blocks: &[BlockRow],
    budget: &mut u64,
    cancel: &dyn Fn() -> bool,
    deadline: &Instant,
) -> Result<VerifyResult, AppError> {
    if blocks.is_empty() {
        return Ok(VerifyResult::Unavailable);
    }
    // Only bytes covered by the table are compared. `size_bytes` can be larger
    // than the table when a file grew between listing and read; a missing block
    // then reports `Changed`, which is the honest answer.
    if size == 0 {
        return Ok(VerifyResult::Clean {
            next_offset: 0,
            bytes: 0,
        });
    }
    let start = (verify_offset / BLOCK_BYTES) * BLOCK_BYTES;
    if start >= size {
        return Ok(VerifyResult::Clean {
            next_offset: 0,
            bytes: 0,
        });
    }
    // A small budget shrinks the window to whole blocks rather than deferring
    // forever: a caller that grants less than one window must still make
    // progress, otherwise verification depends on the budget being generous.
    let cap = (*budget).min(VERIFY_WINDOW_BYTES);
    let mut end = (start + cap).min(size);
    if end < size {
        end = (end / BLOCK_BYTES) * BLOCK_BYTES;
    }
    if end <= start {
        return Ok(VerifyResult::Deferred);
    }
    let cost = end - start;
    if *budget < cost {
        return Ok(VerifyResult::Deferred);
    }
    let mut file = File::open(path).map_err(|error| file_error(error.kind().to_string(), None))?;
    let by_index: HashMap<u64, &BlockRow> =
        blocks.iter().map(|block| (block.index, block)).collect();
    let mut index = start / BLOCK_BYTES;
    while index * BLOCK_BYTES < end {
        if cancel() || Instant::now() >= *deadline {
            return Ok(VerifyResult::Deferred);
        }
        let Some(expected) = by_index.get(&index) else {
            return Ok(VerifyResult::Changed);
        };
        let block_start = index * BLOCK_BYTES;
        if block_start + expected.bytes > size {
            return Ok(VerifyResult::Changed);
        }
        let hash = hash_range(&mut file, block_start, expected.bytes)?;
        if hash != expected.hash {
            return Ok(VerifyResult::Changed);
        }
        index += 1;
    }
    *budget -= cost;
    Ok(VerifyResult::Clean {
        next_offset: if end >= size { 0 } else { end },
        bytes: cost,
    })
}

fn hash_range(file: &mut File, start: u64, len: u64) -> Result<String, AppError> {
    file.seek(SeekFrom::Start(start))
        .map_err(|error| file_error(error.kind().to_string(), None))?;
    let mut buffer = vec![0_u8; len as usize];
    file.read_exact(&mut buffer)
        .map_err(|error| file_error(error.kind().to_string(), None))?;
    Ok(ids::sha256_hex(&buffer))
}

struct StreamState<'a> {
    source: &'a SourceRow,
    namespace: &'a str,
    identity: &'a str,
    parser: &'a mut dyn HistoryParser,
    file_mtime: &'a str,
    file_stem: &'a str,
    base_offset: u64,
    line_index: u64,
    bytes: u64,
    /// (code, detail, line) — never the line content.
    errors: Vec<(&'static str, String, u64)>,
    format: FormatProbe,
    meta_cwd: Option<String>,
    meta_roots: Vec<String>,
    meta_native_id: Option<String>,
    pending_title: Option<(String, TitleSource)>,
    resolver: &'a mut WorkspaceResolver,
    max_bytes: u64,
    deadline: &'a Instant,
    cancel: &'a dyn Fn() -> bool,
    file_index: u64,
    format_error: Option<AppError>,
    decompressed: u64,
}

impl StreamState<'_> {
    fn checkpoint(&self) -> ParserCheckpoint {
        let parser = self.parser.checkpoint();
        ParserCheckpoint {
            native_session_id: self.meta_native_id.clone().or(parser.native_session_id),
            cwd: self.meta_cwd.clone().or(parser.cwd),
            extra_roots: if self.meta_roots.is_empty() {
                parser.extra_roots
            } else {
                self.meta_roots.clone()
            },
            pending_title: self.pending_title.as_ref().map(|(title, source)| {
                super::parser::PendingTitle {
                    title: title.clone(),
                    source: source.as_str().to_string(),
                }
            }),
            title_seen: parser.title_seen,
        }
    }
}

/// Bounded format detection. A file is only rejected after the probe window or
/// at end-of-file produced no recognisable record, so a single corrupt line
/// cannot hide a valid transcript behind it.
#[derive(Default)]
struct FormatProbe {
    checked: u32,
    matched: bool,
}

struct Streamed {
    /// True when the pass stopped before reading anything (the compressed
    /// skip was interrupted). Nothing may be committed in that case.
    interrupted: bool,
    consumed: u64,
    messages: u64,
    bytes: u64,
    anchor_hash: String,
    residue: String,
    residue_truncated: bool,
    cancelled: bool,
    eof: bool,
    blocks: Vec<BlockRow>,
    block_table_valid: bool,
}

fn stream_file(
    tx: &rusqlite::Transaction<'_>,
    state: &mut StreamState<'_>,
    path: &Path,
    start_offset: u64,
    compressed: bool,
) -> Result<Streamed, AppError> {
    if compressed {
        let file = File::open(path).map_err(|error| file_error(error.kind().to_string(), None))?;
        let decoder = zstd::stream::read::Decoder::new(BufReader::new(file))
            .map_err(|error| file_error(format!("zstd:{error}"), None))?;
        let mut lines = BoundedLines::new(
            BufReader::with_capacity(64 * 1024, decoder),
            MAX_LINE_BYTES,
            0,
        );
        // The logical checkpoint is an offset in the decoded stream, so resume
        // means decoding from the start and discarding what was already
        // committed. The skip is bounded by the pass deadline and can be
        // cancelled at any moment.
        if start_offset > 0 && !lines.skip_exact(start_offset, state)? {
            return Ok(Streamed {
                interrupted: true,
                ..empty_stream(true)
            });
        }
        stream_lines(tx, state, lines)
    } else {
        let mut file =
            File::open(path).map_err(|error| file_error(error.kind().to_string(), None))?;
        let first_block = start_offset / BLOCK_BYTES;
        // Seed the block table with the bytes already committed before this
        // run; they are re-read from the block boundary, never re-parsed.
        let seeded = if start_offset > 0 {
            let block_start = first_block * BLOCK_BYTES;
            match read_anchor(path, start_offset, start_offset - block_start) {
                Ok(seed) if seed.len() as u64 == start_offset - block_start => Some(seed),
                _ => None,
            }
        } else {
            None
        };
        if start_offset > 0 {
            file.seek(SeekFrom::Start(start_offset))
                .map_err(|error| file_error(error.kind().to_string(), None))?;
        }
        let mut lines = BoundedLines::new(
            BufReader::with_capacity(64 * 1024, file),
            MAX_LINE_BYTES,
            first_block,
        );
        match seeded {
            Some(seed) => lines.hasher.update(&seed),
            None if start_offset > 0 => lines.hasher.invalidate(),
            None => {}
        }
        stream_lines(tx, state, lines)
    }
}

fn empty_stream(cancelled: bool) -> Streamed {
    Streamed {
        interrupted: false,
        consumed: 0,
        messages: 0,
        bytes: 0,
        anchor_hash: ids::anchor_hash(&[]),
        residue: String::new(),
        residue_truncated: false,
        cancelled,
        eof: false,
        blocks: Vec::new(),
        block_table_valid: false,
    }
}

fn stream_lines<R: BufRead>(
    tx: &rusqlite::Transaction<'_>,
    state: &mut StreamState<'_>,
    mut lines: BoundedLines<R>,
) -> Result<Streamed, AppError> {
    let mut consumed = 0_u64;
    let mut messages = 0_u64;
    let mut anchor_window: Vec<u8> = Vec::with_capacity(4096);
    let mut cancelled = false;
    let mut residue = String::new();
    let mut residue_truncated = false;
    let mut lines_since_check = 0_u32;
    let mut eof = false;

    loop {
        if (state.cancel)() {
            cancelled = true;
            break;
        }
        if state.bytes >= state.max_bytes {
            break;
        }
        lines_since_check += 1;
        if lines_since_check >= 64 {
            lines_since_check = 0;
            if Instant::now() >= *state.deadline {
                break;
            }
        }
        let line = match lines.next_line() {
            Ok(Some(line)) => line,
            Ok(None) => {
                eof = true;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                // The bounded skip limit was exceeded: report and stop the file
                // instead of reading an unbounded record.
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "a single record is larger than the bounded read limit",
                )
                .with_details(json!({
                    "stage": "historyScan",
                    "reason": "line-too-large",
                    "line": state.line_index + 1,
                    "fileIndex": state.file_index,
                }))
                .with_retryable(false));
            }
            Err(error) => {
                return Err(file_error(
                    error.kind().to_string(),
                    Some(state.line_index + 1),
                ));
            }
        };
        if !line.terminated {
            // An unterminated tail may still be written. Import nothing, keep
            // the cursor before it, and remember the residue for diagnostics.
            let (text, truncated) =
                ids::truncate_chars(&String::from_utf8_lossy(&line.bytes), 4096);
            residue = text;
            residue_truncated = line.truncated || truncated;
            // A half-written line is still end-of-file for this pass: the file
            // is caught up, and the next pass re-reads from the cursor.
            eof = true;
            break;
        }
        if line.bytes.is_empty() {
            consumed += line.consumed;
            state.bytes += line.consumed;
            update_anchor(&mut anchor_window, &line.bytes);
            continue;
        }
        let byte_offset = state.base_offset + consumed;
        state.line_index += 1;
        let line_index = state.line_index;

        if line.truncated {
            // A single line above the cap cannot be parsed as JSON. It is
            // isolated as an explicit marker row instead of being dropped, and
            // the rest of the file continues.
            let native = state
                .meta_native_id
                .clone()
                .unwrap_or_else(|| ids::stable_id("history-file", &[state.file_stem]));
            let marker = RecordOut {
                native_session_id: native,
                native_event_id: Some(format!("oversized:{line_index}")),
                source_key: format!("oversized:{line_index}"),
                kind: "unknown".to_string(),
                seq: line_index,
                timestamp: Some(state.file_mtime.to_string()),
                content: json!({
                    "recordType": "oversized-line",
                    "text": "",
                    "lineBytes": line.consumed,
                    "truncated": true,
                    "unknownFields": [],
                }),
                search_text: String::new(),
                cwd: state.meta_cwd.clone(),
                extra_roots: Vec::new(),
                title_update: None,
            };
            messages += ingest_parsed(tx, state, marker, byte_offset, line_index);
            if state.errors.len() < 64 {
                state.errors.push((
                    "line-too-large",
                    "a record above the line cap was isolated as a marker".to_string(),
                    line_index,
                ));
            }
            consumed += line.consumed;
            state.bytes += line.consumed;
            update_anchor(&mut anchor_window, &line.bytes);
            continue;
        }
        let value: Value = match serde_json::from_slice(&line.bytes) {
            Ok(value) => value,
            Err(_error) => {
                // A malformed line is isolated, not fatal: the rest of the file
                // may still be a valid transcript. The format probe decides
                // later whether *nothing* in this file was recognisable.
                state.format.checked += 1;
                if state.errors.len() < 64 {
                    state.errors.push((
                        "invalid-json",
                        "line is not valid JSON".to_string(),
                        line_index,
                    ));
                }
                if !state.format.matched && state.format.checked > FORMAT_PROBE_LINES {
                    state.format_error = Some(unrecognized_format(state));
                    return Ok(empty_stream(cancelled));
                }
                consumed += line.consumed;
                state.bytes += line.consumed;
                update_anchor(&mut anchor_window, &line.bytes);
                continue;
            }
        };

        if !state.format.matched {
            if state.parser.matches_first_line(&value) {
                state.format.matched = true;
            } else {
                state.format.checked += 1;
                if state.format.checked > FORMAT_PROBE_LINES {
                    state.format_error = Some(unrecognized_format(state));
                    return Ok(empty_stream(cancelled));
                }
            }
        }

        let context = ParseContext {
            line_index,
            byte_offset,
            file_mtime: state.file_mtime,
            file_stem: state.file_stem,
            line_truncated: line.truncated,
        };
        match state.parser.parse_line(&value, &context) {
            Ok(parsed) => {
                for item in parsed {
                    match item {
                        Parsed::Meta(meta) => {
                            if let Some(cwd) = meta.cwd.clone() {
                                state.meta_cwd = Some(cwd);
                            }
                            state.meta_roots = meta.extra_roots.clone();
                            state.meta_native_id = Some(meta.native_id.clone());
                        }
                        Parsed::Ignored => {}
                        Parsed::Title { title, source } => {
                            let native = state.meta_native_id.clone().unwrap_or_else(|| {
                                ids::stable_id("history-file", &[state.file_stem])
                            });
                            let session_id = ids::session_id(state.namespace, &native);
                            let applied =
                                repo::apply_native_title(tx, &session_id, &state.source.id, &title)
                                    .unwrap_or(false);
                            if !applied {
                                // The title record arrived before the session
                                // existed; it is applied with the next record.
                                state.pending_title = Some((title, source));
                            }
                        }
                        Parsed::Record(record) => {
                            messages += ingest_parsed(tx, state, record, byte_offset, line_index);
                        }
                    }
                }
            }
            Err(error) => {
                if state.errors.len() < 64 {
                    state.errors.push((error.code, error.detail, line_index));
                }
            }
        }
        consumed += line.consumed;
        state.bytes += line.consumed;
        update_anchor(&mut anchor_window, &line.bytes);
    }

    // End-of-file without a single recognisable record is a wrong product, not
    // an empty transcript.
    if eof && !state.format.matched && state.format.checked > 0 {
        state.format_error = Some(unrecognized_format(state));
        return Ok(empty_stream(cancelled));
    }

    let (blocks, block_table_valid) = lines.into_blocks();
    Ok(Streamed {
        interrupted: false,
        consumed,
        messages,
        bytes: consumed,
        anchor_hash: ids::anchor_hash(&anchor_window),
        residue,
        residue_truncated,
        cancelled,
        eof,
        blocks,
        block_table_valid,
    })
}

fn unrecognized_format(state: &StreamState<'_>) -> AppError {
    AppError::new(
        ErrorCode::Unsupported,
        "the file does not match the selected product format",
    )
    .with_details(json!({
        "stage": "historyScan",
        "fileIndex": state.file_index,
        "reason": "unrecognized-format",
        "agentId": state.source.agent_id,
    }))
    .with_retryable(false)
}

fn ingest_parsed(
    tx: &rusqlite::Transaction<'_>,
    state: &mut StreamState<'_>,
    mut record: RecordOut,
    byte_offset: u64,
    line_index: u64,
) -> u64 {
    if state.meta_native_id.is_none() {
        state.meta_native_id = Some(record.native_session_id.clone());
    }
    if record.title_update.is_none() {
        record.title_update = state.pending_title.clone();
        if record.title_update.is_some() {
            state.pending_title = None;
        }
    }
    let cwd = record.cwd.clone().or_else(|| state.meta_cwd.clone());
    let primary = cwd.as_deref().and_then(|path| state.resolver.resolve(path));
    let related = state.resolver.resolve_many(&state.meta_roots);
    let observed_at = record
        .timestamp
        .clone()
        .unwrap_or_else(|| state.file_mtime.to_string());
    let ingest = repo::IngestContext {
        source: state.source,
        namespace: state.namespace,
        file_identity: state.identity,
        byte_offset,
        line_index,
        observed_at: &observed_at,
        primary_workspace: primary.as_ref(),
        related_workspaces: &related,
    };
    match repo::ingest_record(tx, &ingest, &record) {
        Ok(outcome) => u64::from(outcome.inserted),
        Err(error) => {
            if state.errors.len() < 64 {
                state.errors.push((
                    "ingest-rejected",
                    error
                        .details
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("row rejected")
                        .to_string(),
                    line_index,
                ));
            }
            0
        }
    }
}

fn update_anchor(window: &mut Vec<u8>, line: &[u8]) {
    window.extend_from_slice(line);
    window.push(b'\n');
    if window.len() > 4096 {
        let excess = window.len() - 4096;
        window.drain(..excess);
    }
}

#[cfg(test)]
mod listing_tests {
    use super::collect_files;

    #[test]
    fn a_root_that_cannot_be_listed_is_reported_as_incomplete() {
        let missing = std::env::temp_dir().join(format!(
            "multicli-scan-missing-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&missing);
        let mut issues = Vec::new();
        let (files, truncated, incomplete) = collect_files(
            &missing,
            &missing.to_string_lossy(),
            &["jsonl"],
            10,
            &mut issues,
        );
        assert!(files.is_empty());
        assert!(!truncated);
        assert!(
            incomplete,
            "a directory that cannot be read is not an authoritative empty listing"
        );
        assert!(issues
            .iter()
            .any(|issue| issue.code == "directory-unreadable"));
    }
}
