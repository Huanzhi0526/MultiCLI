//! Backup and restore for this application's own profile.
//!
//! Scope rules, enforced here rather than in the UI:
//!
//! - Only the MultiCLI database inside this profile is exported. CLI
//!   credentials, transcripts, workspace files and other applications' data are
//!   never read, copied or bundled.
//! - A backup is a consistent `VACUUM INTO` snapshot, verified with
//!   `PRAGMA integrity_check` and a SHA-256 checksum before it is listed.
//! - Restoring never replaces a live database behind an open connection. A
//!   restore is *planned* (an atomically written marker) and applied at the
//!   next start, after the current database has been copied aside. A failed
//!   restore leaves the original file in place.
//!
//! Nothing here touches the network or starts a subprocess.

use super::error::{AppError, ErrorCode};
use super::store::SCHEMA_VERSION;
use rusqlite::{params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::{Path, PathBuf};

pub const BACKUP_DIR: &str = "backups";
pub const MANIFEST_VERSION: u32 = 1;
pub const PENDING_FILE: &str = "restore-pending.json";
pub const LAST_RESTORE_FILE: &str = "last-restore.json";

/// Written next to every backup file. Only non-secret metadata is recorded; a
/// backup path is returned to the UI but never embedded in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupManifest {
    pub manifest_version: u32,
    pub created_at: String,
    pub app_version: String,
    pub profile: String,
    /// Schema version of the database inside the backup.
    pub database_schema: i64,
    pub file_name: String,
    pub size_bytes: u64,
    pub sha256: String,
    /// `manual | pre-restore`.
    pub source: String,
    /// Table names only — enough to prove what was exported, never row data.
    pub tables: Vec<String>,
    pub scope_note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BackupRecord {
    pub manifest: BackupManifest,
    pub path: String,
    /// `ok | missing | checksum-mismatch | unreadable | invalid-name`.
    pub integrity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PendingRestore {
    /// Stable id for this concrete request. A new plan always gets a new id,
    /// so an old completion record can never suppress it even if the clock
    /// moved backwards.
    #[serde(default)]
    pub plan_id: String,
    pub backup_file_name: String,
    pub sha256: String,
    pub requested_at: String,
    pub database_schema: i64,
    pub note: String,
    /// Set by the next start when the planned restore could not be applied.
    /// The marker is kept (nothing was replaced) so the user can retry or
    /// cancel instead of silently losing the request.
    #[serde(default)]
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestoreOutcome {
    #[serde(default)]
    pub plan_id: String,
    pub restored_from: String,
    /// SHA-256 of the snapshot that was actually applied. Used to ignore a
    /// marker whose deletion failed after a successful restore.
    #[serde(default)]
    pub restored_sha256: String,
    pub applied_at: String,
    /// Where the database that was replaced was copied to.
    pub previous_copy: Option<String>,
    pub database_schema: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProfileDataView {
    pub profile: String,
    pub data_root: String,
    pub database_file: String,
    pub database_bytes: u64,
    pub database_schema: i64,
    pub expected_schema: i64,
    pub journal_mode: String,
    pub backup_directory: String,
    pub backups: Vec<BackupRecord>,
    pub pending_restore: Option<PendingRestore>,
    pub last_restore: Option<RestoreOutcome>,
    pub scope_note: String,
}

pub fn backup_directory(root: &Path) -> PathBuf {
    root.join(BACKUP_DIR)
}

fn database_file(root: &Path) -> PathBuf {
    root.join("multicli.db")
}

/// One consistent snapshot of the profile database.
pub fn create_backup(
    conn: &Connection,
    root: &Path,
    profile: &str,
    app_version: &str,
    source: &str,
) -> Result<BackupRecord, AppError> {
    let directory = backup_directory(root);
    std::fs::create_dir_all(&directory).map_err(|error| {
        io_error(format!("backup directory could not be created: {error}"))
    })?;
    let directory = checked_backup_directory(root)?;
    let stamp = timestamp_slug();
    let file_name = format!("multicli-{stamp}-{}.db", short_id());
    let final_path = directory.join(&file_name);
    let temp_path = directory.join(format!(".tmp-{stamp}-{}.db", short_id()));

    // `VACUUM INTO` writes a defragmented, fully consistent copy without
    // blocking the reader and without copying a live `-wal` file.
    conn.execute("VACUUM INTO ?1", params![temp_path.to_string_lossy().to_string()])
        .map_err(|error| {
            AppError::new(ErrorCode::Io, format!("backup snapshot failed: {error}"))
                .with_details(json!({ "stage": "backupSnapshot" }))
                .with_retryable(true)
        })?;

    let verified = verify_snapshot(&temp_path);
    let (sha256, size_bytes, tables, database_schema) = match verified {
        Ok(value) => value,
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            return Err(error);
        }
    };
    if let Err(error) = std::fs::rename(&temp_path, &final_path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(io_error(format!("backup could not be finalised: {error}")));
    }

    let manifest = BackupManifest {
        manifest_version: MANIFEST_VERSION,
        created_at: super::usage::model::now_iso8601(),
        app_version: app_version.to_string(),
        profile: profile.to_string(),
        database_schema,
        file_name: file_name.clone(),
        size_bytes,
        sha256,
        source: source.to_string(),
        tables,
        scope_note: scope_note(),
    };
    write_manifest(&directory, &manifest)?;
    Ok(BackupRecord {
        manifest,
        path: final_path.to_string_lossy().to_string(),
        integrity: "ok".to_string(),
    })
}

/// Every readable manifest in this profile's backup directory.
///
/// A manifest that does not parse is reported as `unreadable` instead of being
/// dropped: a backup the user can see on disk must not silently disappear from
/// the list.
pub fn list_backups(root: &Path) -> Result<Vec<BackupRecord>, AppError> {
    let directory = checked_backup_directory(root)?;
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    let entries = std::fs::read_dir(&directory).map_err(|error| {
        io_error(format!("backup directory could not be listed: {error}"))
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        if path.file_name().and_then(|value| value.to_str()) == Some(PENDING_FILE) {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if file_name == LAST_RESTORE_FILE {
            continue;
        }
        if !snapshot_in_scope(&directory, &path) {
            records.push(BackupRecord {
                manifest: BackupManifest {
                    manifest_version: 0,
                    created_at: String::new(),
                    app_version: String::new(),
                    profile: String::new(),
                    database_schema: 0,
                    file_name: file_name.to_string(),
                    size_bytes: 0,
                    sha256: String::new(),
                    source: "unknown".to_string(),
                    tables: Vec::new(),
                    scope_note: String::new(),
                },
                path: path.to_string_lossy().to_string(),
                integrity: "invalid-name".to_string(),
            });
            continue;
        }
        match std::fs::read(&path).ok().and_then(|bytes| {
            serde_json::from_slice::<BackupManifest>(&bytes).ok()
        }) {
            Some(manifest) => {
                // The manifest is user-writable JSON; never join a name that
                // leaves `backups/` or points at another volume, and never hash
                // a file outside this profile's backup directory.
                if validate_backup_file_name(&manifest.file_name).is_err() {
                    records.push(BackupRecord {
                        path: path.to_string_lossy().to_string(),
                        manifest,
                        integrity: "invalid-name".to_string(),
                    });
                    continue;
                }
                let target = directory.join(&manifest.file_name);
                if std::fs::symlink_metadata(&target)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    records.push(BackupRecord {
                        path: target.to_string_lossy().to_string(),
                        manifest,
                        integrity: "missing".to_string(),
                    });
                    continue;
                }
                if !snapshot_in_scope(&directory, &target) {
                    records.push(BackupRecord {
                        path: target.to_string_lossy().to_string(),
                        manifest,
                        integrity: "invalid-name".to_string(),
                    });
                    continue;
                }
                records.push(BackupRecord {
                    integrity: integrity_of(&manifest, &target),
                    path: target.to_string_lossy().to_string(),
                    manifest,
                });
            }
            None => records.push(BackupRecord {
                manifest: BackupManifest {
                    manifest_version: 0,
                    created_at: String::new(),
                    app_version: String::new(),
                    profile: String::new(),
                    database_schema: 0,
                    file_name: file_name.to_string(),
                    size_bytes: 0,
                    sha256: String::new(),
                    source: "unknown".to_string(),
                    tables: Vec::new(),
                    scope_note: String::new(),
                },
                path: path.to_string_lossy().to_string(),
                integrity: "unreadable".to_string(),
            }),
        }
    }
    records.sort_by(|left, right| right.manifest.created_at.cmp(&left.manifest.created_at));
    Ok(records)
}

/// Plans a restore for the next start. The live database is not touched here:
/// replacing it under an open connection is exactly the kind of half-applied
/// operation this design avoids.
pub fn plan_restore(root: &Path, backup_file_name: &str) -> Result<PendingRestore, AppError> {
    validate_backup_file_name(backup_file_name)?;
    let directory = checked_backup_directory(root)?;
    let target = directory.join(backup_file_name);
    if !snapshot_in_scope(&directory, &target) {
        return Err(AppError::invalid_argument(
            "backupFileName",
            "backup file is not a regular file inside this profile's backup directory",
        ));
    }
    let manifest_path = directory.join(format!("{backup_file_name}.json"));
    if !snapshot_in_scope(&directory, &manifest_path) {
        return Err(AppError::invalid_argument(
            "backupManifest",
            "manifest must be a regular file inside this profile's backup directory",
        ));
    }
    let manifest: BackupManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|_| {
            AppError::new(ErrorCode::NotFound, "备份清单不存在，无法计划恢复")
        })?,
    )
    .map_err(|_| AppError::new(ErrorCode::InvalidArgument, "备份清单无法解析"))?;
    if manifest.file_name != backup_file_name {
        return Err(AppError::invalid_argument(
            "backupFileName",
            "manifest names a different file",
        ));
    }
    check_manifest_scope(root, &manifest)?;
    if manifest.database_schema > SCHEMA_VERSION {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "备份来自更新版本的 schema，当前版本不能降级恢复",
        )
        .with_details(json!({
            "backup": manifest.database_schema,
            "current": SCHEMA_VERSION,
        })));
    }
    let (sha, _, _, schema) = verify_snapshot(&target)?;
    if schema > SCHEMA_VERSION {
        // The manifest can claim anything; the snapshot's own
        // `schema_migrations` is what the next open will actually see.
        return Err(AppError::new(
            ErrorCode::Conflict,
            "备份数据库的 schema 比当前版本新，不能降级恢复",
        )
        .with_details(json!({
            "backup": schema,
            "current": SCHEMA_VERSION,
        })));
    }
    if sha != manifest.sha256 {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "备份文件校验和与清单不一致，已拒绝恢复",
        )
        .with_details(json!({ "stage": "restoreVerify" })));
    }
    let pending = PendingRestore {
        plan_id: uuid::Uuid::new_v4().simple().to_string(),
        backup_file_name: backup_file_name.to_string(),
        sha256: sha,
        requested_at: super::usage::model::now_iso8601(),
        database_schema: schema,
        note: "恢复将在下次启动时执行；当前数据库会先复制到 backups/pre-restore-* 目录。"
            .to_string(),
        failed: false,
    };
    write_json_atomic(&root.join(PENDING_FILE), &pending)?;
    Ok(pending)
}

pub fn pending_restore(root: &Path) -> Option<PendingRestore> {
    let bytes = std::fs::read(root.join(PENDING_FILE)).ok()?;
    let pending: PendingRestore = serde_json::from_slice(&bytes).ok()?;
    // A marker can survive a successful restore when removing it fails (for
    // example a read-only file). The durable completion record is the commit
    // point: never replay a restore that is already applied.
    if let Some(last) = read_last_restore(root) {
        let same_plan = !last.plan_id.is_empty()
            && !pending.plan_id.is_empty()
            && last.plan_id == pending.plan_id;
        if same_plan {
            return None;
        }
        // Compatibility for markers written before plan ids existed. Only
        // fall back to the old file+sha check when both sides lack an id.
        if last.plan_id.is_empty() && pending.plan_id.is_empty() {
            let same_file = last.restored_from == pending.backup_file_name;
            let same_sha = !last.restored_sha256.is_empty()
                && last.restored_sha256 == pending.sha256;
            if same_file && same_sha {
                return None;
            }
        }
    }
    Some(pending)
}

fn read_last_restore(root: &Path) -> Option<RestoreOutcome> {
    let bytes = std::fs::read(root.join(LAST_RESTORE_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Records why the last start could not apply a planned restore. The marker is
/// kept, nothing was replaced, and the data page can now say so instead of
/// repeating “will run on next start”.
pub fn mark_restore_failed(root: &Path, error: &AppError) -> Result<(), AppError> {
    let Some(mut pending) = pending_restore(root) else {
        return Ok(());
    };
    pending.failed = true;
    pending.note = format!(
        "上次启动恢复失败（{}）：{}。备份未应用，当前数据库保持不变。",
        error.code.as_str(),
        error.message
    );
    write_json_atomic(&root.join(PENDING_FILE), &pending)
}

pub fn cancel_pending_restore(root: &Path) -> Result<(), AppError> {
    let path = root.join(PENDING_FILE);
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|error| io_error(format!("pending restore could not be removed: {error}")))?;
    }
    Ok(())
}

pub fn last_restore(root: &Path) -> Option<RestoreOutcome> {
    read_last_restore(root)
}

/// Applies a planned restore **before** the database is opened.
///
/// Order of operations:
/// 1. verify the backup again (checksum + integrity),
/// 2. copy the current database (and its `-wal`/`-shm`) into
///    `backups/pre-restore-<stamp>/`,
/// 3. copy the backup next to the database and rename it into place,
/// 4. remove stale `-wal`/`-shm`, record the outcome, drop the marker.
///
/// Any failure before step 3 leaves the original file untouched; a failure
/// during step 3 restores the pre-restore copy.
pub fn apply_pending_restore(root: &Path) -> Result<Option<RestoreOutcome>, AppError> {
    let Some(pending) = pending_restore(root) else {
        return Ok(None);
    };
    // The marker lives on disk next to user-controlled files; re-validate its
    // name instead of trusting that plan_restore produced it.
    validate_backup_file_name(&pending.backup_file_name)?;
    let directory = checked_backup_directory(root)?;
    let manifest_path = directory.join(format!("{}.json", pending.backup_file_name));
    if !snapshot_in_scope(&directory, &manifest_path) {
        return Err(AppError::invalid_argument(
            "backupManifest",
            "manifest must be a regular file inside this profile's backup directory",
        ));
    }
    let manifest: BackupManifest = serde_json::from_slice(
        &std::fs::read(&manifest_path).map_err(|_| {
            AppError::new(ErrorCode::NotFound, "备份清单不存在，无法应用恢复")
        })?,
    )
    .map_err(|_| AppError::new(ErrorCode::InvalidArgument, "备份清单无法解析"))?;
    check_manifest_scope(root, &manifest)?;
    if manifest.file_name != pending.backup_file_name {
        return Err(AppError::invalid_argument(
            "backupFileName",
            "manifest names a different file",
        ));
    }
    let source = directory.join(&pending.backup_file_name);
    if !snapshot_in_scope(&directory, &source) {
        return Err(AppError::invalid_argument(
            "backupFileName",
            "backup file is not a regular file inside this profile's backup directory",
        ));
    }
    let (sha, _, _, schema) = verify_snapshot(&source)?;
    if sha != pending.sha256 {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "待恢复的备份校验和不一致，未执行恢复",
        )
        .with_details(json!({ "stage": "restoreApply" })));
    }
    if schema > SCHEMA_VERSION {
        // The marker was written by an older build (or the file was swapped
        // after planning). Re-check the real snapshot here so a downgrade can
        // never be applied just because the marker existed.
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "待恢复的备份来自更新版本的 schema，当前版本不能降级恢复",
        )
        .with_details(json!({
            "stage": "restoreApply",
            "backup": schema,
            "current": SCHEMA_VERSION,
        })));
    }
    let database = database_file(root);
    let stamp = timestamp_slug();
    let previous_dir = directory.join(format!("pre-restore-{stamp}-{}", short_id()));
    std::fs::create_dir_all(&previous_dir)
        .map_err(|error| io_error(format!("pre-restore copy directory failed: {error}")))?;
    let mut previous_copy = None;
    if database.is_file() {
        let copy = previous_dir.join("multicli.db");
        std::fs::copy(&database, &copy)
            .map_err(|error| io_error(format!("pre-restore copy failed: {error}")))?;
        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", database.display()));
            if sidecar.is_file() {
                let _ = std::fs::copy(&sidecar, previous_dir.join(format!("multicli.db{suffix}")));
            }
        }
        previous_copy = Some(copy.to_string_lossy().to_string());
    }

    let staging = root.join(format!(".restore-{stamp}.db"));
    std::fs::copy(&source, &staging)
        .map_err(|error| io_error(format!("restore copy failed: {error}")))?;
    // Move the current journals aside instead of deleting them. If anything
    // fails before the new database is in place, they are moved back, so a
    // committed-but-not-checkpointed transaction is never lost. The pre-restore
    // copy above stays as a second safety net.
    let mut moved_sidecars: Vec<(PathBuf, PathBuf)> = Vec::new();
    for suffix in ["-wal", "-shm"] {
        let sidecar = PathBuf::from(format!("{}{suffix}", database.display()));
        if !sidecar.exists() {
            continue;
        }
        let destination = previous_dir.join(format!("multicli.db{suffix}"));
        if let Err(error) = std::fs::rename(&sidecar, &destination) {
            for (original, moved_to) in moved_sidecars.iter().rev() {
                let _ = std::fs::rename(moved_to, original);
            }
            let _ = std::fs::remove_file(&staging);
            return Err(io_error(format!(
                "stale journal could not be moved aside: {error}"
            )));
        }
        moved_sidecars.push((sidecar, destination));
    }
    if let Err(error) = std::fs::rename(&staging, &database) {
        let _ = std::fs::remove_file(&staging);
        // The live database was never replaced; put its journals back exactly
        // where they were so the next open sees the same bytes as before.
        for (original, moved_to) in moved_sidecars.iter().rev() {
            let _ = std::fs::rename(moved_to, original);
        }
        return Err(io_error(format!("restored database could not replace the current one: {error}")));
    }
    let outcome = RestoreOutcome {
        plan_id: pending.plan_id.clone(),
        restored_from: pending.backup_file_name.clone(),
        restored_sha256: sha.clone(),
        applied_at: super::usage::model::now_iso8601(),
        previous_copy: previous_copy.clone(),
        database_schema: schema,
    };
    if let Err(error) = write_json_atomic(&root.join(LAST_RESTORE_FILE), &outcome) {
        // The database was replaced but the completion record is the durable
        // commit point. Roll the database and journals back so an Err really
        // means "the original database is still here".
        rollback_applied_restore(
            &database,
            &previous_dir,
            previous_copy.as_deref(),
            &moved_sidecars,
        )?;
        return Err(io_error(format!(
            "restore completion metadata failed; database rolled back: {error}"
        )));
    }
    // The completion record is durable; a marker that cannot be removed is
    // inert because `pending_restore` suppresses it.
    if let Err(error) = std::fs::remove_file(root.join(PENDING_FILE)) {
        eprintln!("[multicli] restored, but pending marker could not be removed: {error}");
    }
    Ok(Some(outcome))
}

/// Puts the pre-restore database and its journals back after the new file was
/// already renamed into place. The pre-restore copy is the source of truth.
fn rollback_applied_restore(
    database: &Path,
    previous_dir: &Path,
    previous_copy: Option<&str>,
    moved_sidecars: &[(PathBuf, PathBuf)],
) -> Result<(), AppError> {
    let applied_copy = previous_dir.join("applied-new.db");
    if database.exists() {
        std::fs::rename(database, &applied_copy)
            .map_err(|error| io_error(format!("rollback move failed: {error}")))?;
    }
    if let Some(copy) = previous_copy {
        // Same volume: an atomic rename is safer than a copy that can fail
        // halfway and leave the profile without a database.
        std::fs::rename(copy, database).map_err(|error| {
            io_error(format!(
                "rollback could not restore the old database; it remains at {copy}: {error}"
            ))
        })?;
    }
    for (original, moved_to) in moved_sidecars.iter().rev() {
        if moved_to.exists() {
            std::fs::rename(moved_to, original)
                .map_err(|error| io_error(format!("rollback journal failed: {error}")))?;
        }
    }
    Ok(())
}

pub fn profile_view(
    root: &Path,
    profile: &str,
    journal_mode: &str,
    expected_schema: i64,
) -> Result<ProfileDataView, AppError> {
    let database = database_file(root);
    let (database_bytes, database_schema) = if database.is_file() {
        let bytes = std::fs::metadata(&database).map(|value| value.len()).unwrap_or(0);
        let schema = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()
            .and_then(|conn| {
                conn.query_row(
                    "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .ok()
            })
            .unwrap_or(0);
        (bytes, schema)
    } else {
        (0, 0)
    };
    Ok(ProfileDataView {
        profile: profile.to_string(),
        data_root: root.to_string_lossy().to_string(),
        database_file: database.to_string_lossy().to_string(),
        database_bytes,
        database_schema,
        expected_schema,
        journal_mode: journal_mode.to_string(),
        backup_directory: backup_directory(root).to_string_lossy().to_string(),
        backups: list_backups(root)?,
        pending_restore: pending_restore(root),
        last_restore: last_restore(root),
        scope_note: scope_note(),
    })
}

// ---------------------------------------------------------------------------
// Internals
// ---------------------------------------------------------------------------

fn expected_profile(root: &Path) -> Option<&str> {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| matches!(*name, "dev" | "test" | "prod"))
}

fn check_manifest_scope(root: &Path, manifest: &BackupManifest) -> Result<(), AppError> {
    if manifest.manifest_version != MANIFEST_VERSION {
        return Err(AppError::new(
            ErrorCode::Conflict,
            "备份清单版本与当前版本不兼容",
        )
        .with_details(json!({
            "manifestVersion": manifest.manifest_version,
            "current": MANIFEST_VERSION,
        })));
    }
    if let Some(expected) = expected_profile(root) {
        if manifest.profile != expected {
            return Err(AppError::new(
                ErrorCode::Conflict,
                "备份属于另一个 profile，不能在当前 profile 恢复",
            )
            .with_details(json!({
                "backupProfile": manifest.profile,
                "current": expected,
            })));
        }
    }
    Ok(())
}

fn checked_backup_directory(root: &Path) -> Result<PathBuf, AppError> {
    let directory = backup_directory(root);
    if !directory.is_dir() {
        return Ok(directory);
    }
    let (Ok(root_canonical), Ok(directory_canonical)) = (
        std::fs::canonicalize(root),
        std::fs::canonicalize(&directory),
    ) else {
        return Err(io_error("backup directory could not be resolved"));
    };
    if !directory_canonical.starts_with(&root_canonical) {
        return Err(AppError::invalid_argument(
            "backupDirectory",
            "backup directory resolves outside this profile",
        ));
    }
    Ok(directory)
}

fn scope_note() -> String {
    "备份只包含 MultiCLI 自有 profile 的 SQLite 数据库快照；其中包含本应用已索引的会话与消息内容，\
     但不包含任何 CLI 凭据、CLI 原始历史文件、工作区文件或其他应用的数据。"
        .to_string()
}

/// True when `path` resolves to a real file under `directory`. Symlinks and
/// junctions are rejected: a backup list must not read a file outside the
/// profile, even when a manifest or a link points there.
fn snapshot_in_scope(directory: &Path, path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return false;
    }
    let (Ok(root), Ok(candidate)) = (
        std::fs::canonicalize(directory),
        std::fs::canonicalize(path),
    ) else {
        return false;
    };
    candidate.starts_with(&root)
}

/// A backup name must be exactly one ordinary path component inside
/// `backups/`. This rejects `..`, absolute paths, `C:relative` drive prefixes
/// and separators on both platforms before any file is opened.
fn validate_backup_file_name(name: &str) -> Result<(), AppError> {
    let path = Path::new(name);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(part)), None) if part == std::ffi::OsStr::new(name) => {
            Ok(())
        }
        _ => Err(AppError::invalid_argument(
            "backupFileName",
            "must be a plain file name inside this profile's backup directory",
        )),
    }
}

fn integrity_of(manifest: &BackupManifest, path: &Path) -> String {
    if !path.is_file() {
        return "missing".to_string();
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() != manifest.size_bytes => "checksum-mismatch".to_string(),
        Ok(_) => match file_sha256(path) {
            Ok(sha) if sha == manifest.sha256 => "ok".to_string(),
            Ok(_) => "checksum-mismatch".to_string(),
            Err(_) => "unreadable".to_string(),
        },
        Err(_) => "unreadable".to_string(),
    }
}

/// Opens a snapshot read-only and returns `(sha256, bytes, tables, schema)`.
/// Any structural problem rejects the file: a backup that cannot be read is not
/// a backup.
fn verify_snapshot(path: &Path) -> Result<(String, u64, Vec<String>, i64), AppError> {
    if !path.is_file() {
        return Err(AppError::new(ErrorCode::NotFound, "备份文件不存在"));
    }
    let sha = file_sha256(path)?;
    let size = std::fs::metadata(path).map(|value| value.len()).unwrap_or(0);
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| AppError::new(ErrorCode::Io, format!("备份无法打开: {error}")))?;
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|error| AppError::new(ErrorCode::Io, format!("备份完整性检查失败: {error}")))?;
    if check != "ok" {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "备份未通过 SQLite 完整性检查",
        )
        .with_details(json!({ "integrityCheck": check })));
    }
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .map_err(|error| AppError::new(ErrorCode::Io, format!("备份表清单读取失败: {error}")))?;
    let tables = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| AppError::new(ErrorCode::Io, format!("备份表清单读取失败: {error}")))?
        .filter_map(Result::ok)
        .filter(|name| !name.starts_with("sqlite_"))
        .collect::<Vec<_>>();
    if !tables.iter().any(|name| name == "schema_migrations") {
        return Err(AppError::new(
            ErrorCode::InvalidArgument,
            "备份不是 MultiCLI profile 数据库（缺少 schema_migrations）",
        ));
    }
    let schema: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|error| AppError::new(ErrorCode::Io, format!("备份 schema 读取失败: {error}")))?;
    Ok((sha, size, tables, schema))
}

fn file_sha256(path: &Path) -> Result<String, AppError> {
    let bytes = std::fs::read(path)
        .map_err(|error| io_error(format!("备份校验和读取失败: {error}")))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_manifest(directory: &Path, manifest: &BackupManifest) -> Result<(), AppError> {
    let path = directory.join(format!("{}.json", manifest.file_name));
    write_json_atomic(&path, manifest)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), AppError> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let temp = path.with_extension(format!("tmp-{}", short_id()));
    {
        let mut file = std::fs::File::create(&temp)
            .map_err(|error| io_error(format!("metadata file could not be created: {error}")))?;
        file.write_all(&bytes)
            .map_err(|error| io_error(format!("metadata file could not be written: {error}")))?;
        file.flush()
            .map_err(|error| io_error(format!("metadata file could not be flushed: {error}")))?;
    }
    std::fs::rename(&temp, path).map_err(|error| {
        let _ = std::fs::remove_file(&temp);
        io_error(format!("metadata file could not be moved into place: {error}"))
    })
}

fn io_error(message: impl Into<String>) -> AppError {
    AppError::new(ErrorCode::Io, message.into())
        .with_details(json!({ "stage": "backup" }))
        .with_retryable(true)
}

fn short_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// `YYYYMMDD-HHMMSS` in UTC, derived from the same millisecond clock the store
/// uses. Sorting the file names sorts the backups.
fn timestamp_slug() -> String {
    let iso = super::history::ids::iso_from_millis(super::store::now_ms());
    let digits = iso
        .chars()
        .filter(|value| value.is_ascii_digit())
        .take(14)
        .collect::<String>();
    if digits.len() == 14 {
        format!("{}-{}", &digits[..8], &digits[8..])
    } else {
        digits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::service::MultiCliService;
    use crate::multicli::domain::Host;

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "multicli-s07-backup-{name}-{}",
                uuid::Uuid::new_v4().simple()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn open(root: &TempRoot) -> MultiCliService {
        MultiCliService::open(&root.0).expect("open isolated store")
    }

    #[test]
    fn a_backup_contains_only_this_profile_and_passes_integrity_check() {
        let root = TempRoot::new("scope");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        assert_eq!(record.integrity, "ok");
        assert!(record.manifest.tables.iter().any(|name| name == "sessions"));
        assert!(!record.manifest.tables.iter().any(|name| name.starts_with("cli_")));
        assert!(record.manifest.scope_note.contains("不包含任何 CLI 凭据"));
        assert!(Path::new(&record.path).is_file());
        assert_eq!(list_backups(&root.0).unwrap().len(), 1);
    }

    #[test]
    fn a_planned_restore_replaces_the_database_on_the_next_start_only() {
        let root = TempRoot::new("restore");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        // A row written after the backup must disappear after the restore.
        service.register_host(&Host::local("other-host")).unwrap();
        drop(service);

        let planned = plan_restore(&root.0, &record.manifest.file_name).expect("plan");
        assert_eq!(planned.database_schema, SCHEMA_VERSION);
        // Nothing changes until the application restarts.
        assert!(pending_restore(&root.0).is_some());
        assert!(database_file(&root.0).is_file());

        let outcome = apply_pending_restore(&root.0).expect("apply").expect("outcome");
        assert_eq!(outcome.restored_from, record.manifest.file_name);
        assert!(outcome.previous_copy.as_deref().is_some_and(|path| Path::new(path).is_file()));
        assert!(pending_restore(&root.0).is_none());

        let reopened = MultiCliService::open(&root.0).expect("reopen");
        let hosts: i64 = reopened
            .store()
            .connection()
            .lock()
            .query_row("SELECT COUNT(*) FROM hosts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(hosts, 1, "the restored snapshot has the pre-backup rows only");
    }

    #[test]
    fn a_newer_backup_is_refused_instead_of_downgraded() {
        let root = TempRoot::new("newer");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        let manifest_path = backup_directory(&root.0).join(format!("{}.json", record.manifest.file_name));
        let mut manifest: BackupManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.database_schema = SCHEMA_VERSION + 1;
        write_manifest(&backup_directory(&root.0), &manifest).unwrap();
        let error = plan_restore(&root.0, &record.manifest.file_name).expect_err("must refuse");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(pending_restore(&root.0).is_none());
    }

    #[test]
    fn a_tampered_backup_is_refused_before_anything_is_replaced() {
        let root = TempRoot::new("tampered");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        let path = PathBuf::from(&record.path);
        let mut bytes = std::fs::read(&path).unwrap();
        // Flip a byte in the middle of the file.
        let index = bytes.len() / 2;
        bytes[index] ^= 0xff;
        std::fs::write(&path, bytes).unwrap();
        let error = plan_restore(&root.0, &record.manifest.file_name).expect_err("must refuse");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(pending_restore(&root.0).is_none());
        assert!(database_file(&root.0).is_file());
    }

    #[test]
    fn a_manifest_cannot_hide_a_newer_snapshot_schema() {
        let root = TempRoot::new("hidden-newer");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        // Rewrite the snapshot's own migration table to a future version, then
        // make the manifest look consistent (correct hash, schema 7).
        {
            let conn = Connection::open(&record.path).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, description, applied_at_ms) VALUES (?1, ?2, ?3)",
                params![SCHEMA_VERSION + 99, "future", 0i64],
            )
            .unwrap();
        }
        let manifest_path =
            backup_directory(&root.0).join(format!("{}.json", record.manifest.file_name));
        let mut manifest: BackupManifest =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest.sha256 = file_sha256(&std::path::PathBuf::from(&record.path)).unwrap();
        manifest.size_bytes = std::fs::metadata(&record.path).unwrap().len();
        manifest.database_schema = SCHEMA_VERSION; // lie
        write_manifest(&backup_directory(&root.0), &manifest).unwrap();

        let error = plan_restore(&root.0, &record.manifest.file_name).expect_err("must refuse");
        assert_eq!(error.code, ErrorCode::Conflict);
        assert!(pending_restore(&root.0).is_none());
    }

    #[test]
    fn a_pending_restore_rechecks_the_snapshot_schema_before_replacing() {
        let root = TempRoot::new("apply-newer");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        // Simulate a marker written earlier plus a snapshot that has since been
        // swapped for a newer-schema file with a matching checksum.
        {
            let conn = Connection::open(&record.path).unwrap();
            conn.execute(
                "INSERT INTO schema_migrations(version, description, applied_at_ms) VALUES (?1, ?2, ?3)",
                params![SCHEMA_VERSION + 5, "future", 0i64],
            )
            .unwrap();
        }
        let pending = PendingRestore {
            backup_file_name: record.manifest.file_name.clone(),
            sha256: file_sha256(Path::new(&record.path)).unwrap(),
            requested_at: crate::multicli::usage::model::now_iso8601(),
            database_schema: SCHEMA_VERSION + 5,
            note: "test".to_string(),
            failed: false,
            plan_id: "test-plan".to_string(),
        };
        write_json_atomic(&root.0.join(PENDING_FILE), &pending).unwrap();
        let before = std::fs::read(database_file(&root.0)).unwrap();

        let error = apply_pending_restore(&root.0).expect_err("must refuse downgrade");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.details["stage"], "restoreApply");
        assert_eq!(
            std::fs::read(database_file(&root.0)).unwrap(),
            before,
            "the live database must not change when the snapshot is refused"
        );
    }

    #[test]
    fn backup_names_must_be_one_plain_component() {
        let root = TempRoot::new("names");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        for bad in ["", "..", "../x.db", "a/b.db", "a\\b.db", "C:evil.db"] {
            let error = plan_restore(&root.0, bad).expect_err("must reject");
            assert_eq!(error.code, ErrorCode::InvalidArgument, "name: {bad}");
        }
    }

    #[test]
    fn a_failed_start_marks_the_marker_instead_of_repeating_the_promise() {
        let root = TempRoot::new("failed-marker");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        plan_restore(&root.0, &record.manifest.file_name).expect("plan");
        mark_restore_failed(
            &root.0,
            &AppError::new(ErrorCode::Io, "synthetic apply failure"),
        )
        .expect("mark");
        let pending = pending_restore(&root.0).expect("marker stays");
        assert!(pending.failed);
        assert!(pending.note.contains("synthetic apply failure"));
        assert!(database_file(&root.0).is_file(), "nothing was replaced");
    }

    #[test]
    fn a_completion_metadata_failure_rolls_back_instead_of_claiming_success() {
        let root = TempRoot::new("metadata-failure");
        let service = open(&root);
        service.register_host(&Host::local("before-backup")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        service
            .register_host(&Host::local("after-backup-must-survive-failure"))
            .unwrap();
        drop(service);
        plan_restore(&root.0, &record.manifest.file_name).expect("plan");
        // A directory at the completion-record path makes the durable commit
        // point fail after the database has been renamed into place.
        std::fs::create_dir(root.0.join(LAST_RESTORE_FILE)).unwrap();

        let error = apply_pending_restore(&root.0).expect_err("metadata failure");
        assert_eq!(error.code, ErrorCode::Io);
        let database = Connection::open(database_file(&root.0)).unwrap();
        let after: i64 = database
            .query_row(
                "SELECT COUNT(*) FROM hosts WHERE id='after-backup-must-survive-failure'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after, 1, "an Err must leave the original rows in place");
        drop(database);
        assert!(pending_restore(&root.0).is_some(), "the marker may be retried");
        assert!(last_restore(&root.0).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_committed_restore_is_not_replayed_when_the_marker_cannot_be_removed() {
        let root = TempRoot::new("marker-remove-failure");
        let service = open(&root);
        service.register_host(&Host::local("before-backup")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        drop(service);
        plan_restore(&root.0, &record.manifest.file_name).expect("plan");
        let marker = root.0.join(PENDING_FILE);
        // Keep a handle that allows reads but denies delete; this is exactly
        // the "marker cannot be removed" state without touching ACLs.
        use std::os::windows::fs::OpenOptionsExt;
        let _hold = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&marker)
            .unwrap();

        let outcome = apply_pending_restore(&root.0)
            .expect("the restore itself succeeds")
            .expect("outcome");
        assert_eq!(outcome.restored_sha256, record.manifest.sha256);
        assert!(root.0.join(PENDING_FILE).exists(), "the marker could not be removed");
        assert!(last_restore(&root.0).is_some());
        assert!(
            pending_restore(&root.0).is_none(),
            "a completed restore must not stay pending"
        );
        assert!(
            apply_pending_restore(&root.0).unwrap().is_none(),
            "a second start must not replay the same snapshot"
        );
    }

    #[test]
    fn a_manifest_from_another_profile_or_version_is_refused() {
        let root = Path::new("C:/Users/example/AppData/Local/MultiCLI/test");
        let manifest = BackupManifest {
            manifest_version: MANIFEST_VERSION,
            created_at: String::new(),
            app_version: "0.1.0".into(),
            profile: "prod".into(),
            database_schema: SCHEMA_VERSION,
            file_name: "x.db".into(),
            size_bytes: 1,
            sha256: String::new(),
            source: "manual".into(),
            tables: Vec::new(),
            scope_note: String::new(),
        };
        assert_eq!(
            check_manifest_scope(root, &manifest).unwrap_err().code,
            ErrorCode::Conflict
        );
        let mut future = manifest;
        future.profile = "test".into();
        future.manifest_version = MANIFEST_VERSION + 1;
        assert_eq!(
            check_manifest_scope(root, &future).unwrap_err().code,
            ErrorCode::Conflict
        );
    }

    #[test]
    fn a_manifest_cannot_reach_outside_the_backup_directory() {
        let root = TempRoot::new("manifest-escape");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        std::fs::copy(&record.path, root.0.join("outside.db")).unwrap();
        let mut manifest = record.manifest;
        manifest.file_name = "../outside.db".to_string();
        let manifest_path = backup_directory(&root.0).join("scope-escape.db.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let records = list_backups(&root.0).unwrap();
        let escaped = records
            .iter()
            .find(|item| item.manifest.file_name == "../outside.db")
            .expect("the invalid record stays visible");
        assert_ne!(escaped.integrity, "ok");
        assert_eq!(escaped.integrity, "invalid-name");
    }

    #[test]
    fn a_failed_completion_restores_uncheckpointed_journals_too() {
        let root = TempRoot::new("wal-rollback");
        let service = open(&root);
        service.register_host(&Host::local("before-backup")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        drop(service);
        plan_restore(&root.0, &record.manifest.file_name).expect("plan");
        // Synthetic journal bytes stand in for a WAL that still holds a
        // committed-but-not-checkpointed transaction. The restore path must
        // move them aside and put them back on failure, never delete them.
        let wal = PathBuf::from(format!("{}-wal", database_file(&root.0).display()));
        let shm = PathBuf::from(format!("{}-shm", database_file(&root.0).display()));
        std::fs::write(&wal, b"wal-before-failure").unwrap();
        std::fs::write(&shm, b"shm-before-failure").unwrap();
        std::fs::create_dir(root.0.join(LAST_RESTORE_FILE)).unwrap();

        assert!(apply_pending_restore(&root.0).is_err());
        assert_eq!(std::fs::read(&wal).unwrap(), b"wal-before-failure");
        assert_eq!(std::fs::read(&shm).unwrap(), b"shm-before-failure");
    }

    #[test]
    fn a_new_plan_is_not_suppressed_by_an_old_completion() {
        let root = TempRoot::new("new-plan");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        drop(service);
        let first = plan_restore(&root.0, &record.manifest.file_name).expect("first plan");
        apply_pending_restore(&root.0).expect("apply").expect("outcome");
        let second = plan_restore(&root.0, &record.manifest.file_name).expect("second plan");
        assert_ne!(first.plan_id, second.plan_id);
        let pending = pending_restore(&root.0).expect("the new plan must stay visible");
        assert_eq!(pending.plan_id, second.plan_id);
    }

    #[test]
    fn a_missing_snapshot_is_distinct_from_an_invalid_path() {
        let root = TempRoot::new("missing-snapshot");
        let service = open(&root);
        let record = create_backup(
            &service.store().connection().lock(), &root.0, "test", "0.1.0", "manual",
        ).unwrap();
        std::fs::rename(&record.path, root.0.join("moved.db")).unwrap();
        let records = list_backups(&root.0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].integrity, "missing");
    }

    #[test]
    fn planning_and_applying_require_a_regular_in_scope_manifest() {
        let root = TempRoot::new("manifest-not-file");
        let service = open(&root);
        let record = create_backup(
            &service.store().connection().lock(), &root.0, "test", "0.1.0", "manual",
        ).unwrap();
        drop(service);
        plan_restore(&root.0, &record.manifest.file_name).unwrap();
        let path = backup_directory(&root.0).join(format!("{}.json", record.manifest.file_name));
        std::fs::rename(&path, root.0.join("saved-manifest.json")).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert_eq!(plan_restore(&root.0, &record.manifest.file_name).unwrap_err().code, ErrorCode::InvalidArgument);
        assert_eq!(apply_pending_restore(&root.0).unwrap_err().code, ErrorCode::InvalidArgument);
        assert!(pending_restore(&root.0).is_some());
    }

    #[test]
    fn a_pending_restore_can_be_cancelled() {
        let root = TempRoot::new("cancel");
        let service = open(&root);
        service.register_host(&Host::local("local-windows")).unwrap();
        let record = create_backup(
            &service.store().connection().lock(),
            &root.0,
            "test",
            "0.1.0",
            "manual",
        )
        .expect("backup");
        plan_restore(&root.0, &record.manifest.file_name).expect("plan");
        cancel_pending_restore(&root.0).expect("cancel");
        assert!(pending_restore(&root.0).is_none());
        assert!(apply_pending_restore(&root.0).expect("noop").is_none());
    }
}
