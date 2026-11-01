//! Real filesystem path resolution and Git worktree identity for workspaces.
//!
//! Identity rules:
//! - `displayPath` is a normalized but human-readable path (drive letter
//!   upper-cased, separators unified, `.`/`..` collapsed). It is not used as a
//!   uniqueness key.
//! - `canonicalPath` comes from the operating system canonicalizer, which
//!   resolves junctions/reparse points and returns the case stored on disk.
//!   Only a resolved path can be registered as a workspace.
//! - Missing or inaccessible directories keep `canonicalPath = null` plus an
//!   explicit status/reason; no path is invented.
//! - `repoGroupId` groups real Git worktrees that share one common Git
//!   directory. Each checkout still has its own `worktreeId` and workspace row.
//!
//! This module intentionally depends on `std` only: the acceptance harness
//! compiles the MultiCLI module with a minimal dependency set.

use super::domain::{HostPath, Workspace};
use super::error::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Result of resolving a user-supplied workspace directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolvedWorkspacePath {
    pub path: HostPath,
    pub status: PathResolutionStatus,
    pub error: Option<PathResolutionError>,
    pub repo_group_id: Option<String>,
    pub worktree_id: Option<String>,
    pub git_status: GitStatus,
}

impl ResolvedWorkspacePath {
    pub fn is_resolved(&self) -> bool {
        self.status == PathResolutionStatus::Resolved
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PathResolutionStatus {
    Resolved,
    NotFound,
    AccessDenied,
    Invalid,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PathResolutionError {
    pub code: ErrorCode,
    /// Fixed, path-free reason. Absolute paths are deliberately not repeated
    /// here so the value can be shown in UI and copied into diagnostics.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitStatus {
    Repository,
    NotRepository,
    Unreadable,
}

/// Resolves `display_path` on `host_id` without touching the database.
pub fn resolve_workspace_path(host_id: &str, display_path: &str) -> ResolvedWorkspacePath {
    let mut result = ResolvedWorkspacePath {
        path: HostPath {
            host_id: host_id.to_string(),
            display_path: display_path.to_string(),
            canonical_path: None,
        },
        status: PathResolutionStatus::Invalid,
        error: None,
        repo_group_id: None,
        worktree_id: None,
        git_status: GitStatus::NotRepository,
    };

    if host_id.trim().is_empty() {
        result.error = Some(path_error(
            ErrorCode::InvalidArgument,
            "hostId must not be empty",
        ));
        return result;
    }

    let normalized = match normalize_display_path(display_path) {
        Ok(value) => value,
        Err(error) => {
            result.error = Some(error);
            return result;
        }
    };
    result.path.display_path = normalized.clone();

    let metadata = match std::fs::metadata(&normalized) {
        Ok(metadata) => metadata,
        Err(error) => {
            let (status, code, reason) = match error.kind() {
                std::io::ErrorKind::NotFound => (
                    PathResolutionStatus::NotFound,
                    ErrorCode::NotFound,
                    "directory does not exist",
                ),
                std::io::ErrorKind::PermissionDenied => (
                    PathResolutionStatus::AccessDenied,
                    ErrorCode::AccessDenied,
                    "directory is not accessible",
                ),
                _ => (
                    PathResolutionStatus::Error,
                    ErrorCode::Io,
                    "directory could not be inspected",
                ),
            };
            result.status = status;
            result.error = Some(path_error(code, reason));
            return result;
        }
    };
    if !metadata.is_dir() {
        result.status = PathResolutionStatus::Invalid;
        result.error = Some(path_error(
            ErrorCode::InvalidArgument,
            "path is not a directory",
        ));
        return result;
    }

    let canonical = match std::fs::canonicalize(&normalized) {
        Ok(path) => canonical_text(&path),
        Err(error) => {
            let (status, code, reason) = match error.kind() {
                std::io::ErrorKind::NotFound => (
                    PathResolutionStatus::NotFound,
                    ErrorCode::NotFound,
                    "directory disappeared during resolution",
                ),
                std::io::ErrorKind::PermissionDenied => (
                    PathResolutionStatus::AccessDenied,
                    ErrorCode::AccessDenied,
                    "directory is not accessible",
                ),
                _ => (
                    PathResolutionStatus::Error,
                    ErrorCode::Io,
                    "directory could not be canonicalized",
                ),
            };
            result.status = status;
            result.error = Some(path_error(code, reason));
            return result;
        }
    };

    let canonical = match normalize_display_path(&canonical) {
        Ok(value) => value,
        Err(error) => {
            result.status = PathResolutionStatus::Error;
            result.error = Some(error);
            return result;
        }
    };
    result.path.canonical_path = Some(canonical.clone());
    result.status = PathResolutionStatus::Resolved;

    match git_identity(host_id, Path::new(&canonical)) {
        GitIdentity::Repository {
            repo_group_id,
            worktree_id,
        } => {
            result.git_status = GitStatus::Repository;
            result.repo_group_id = Some(repo_group_id);
            result.worktree_id = Some(worktree_id);
        }
        GitIdentity::NotRepository => {
            result.git_status = GitStatus::NotRepository;
        }
        GitIdentity::Unreadable => {
            result.git_status = GitStatus::Unreadable;
        }
    }
    result
}

/// Builds the `Workspace` row a resolved path should produce.
pub fn workspace_from_resolved(resolved: &ResolvedWorkspacePath) -> Result<Workspace, AppError> {
    if !resolved.is_resolved() || resolved.path.canonical_path.is_none() {
        let reason = resolved
            .error
            .as_ref()
            .map(|error| error.reason.as_str())
            .unwrap_or("path is not resolved");
        return Err(
            AppError::invalid_argument("displayPath", reason).with_details(serde_json::json!({
                "status": resolved.status,
                "reason": reason,
            })),
        );
    }
    let canonical = resolved.path.canonical_path.as_deref().unwrap_or_default();
    Ok(Workspace {
        id: workspace_id(&resolved.path.host_id, canonical),
        path: resolved.path.clone(),
        repo_group_id: resolved.repo_group_id.clone(),
        worktree_id: resolved.worktree_id.clone(),
    })
}

/// Stable, opaque workspace id. The database still enforces the real
/// uniqueness key `(hostId, canonicalPath)`; this id only makes repeated
/// registration idempotent.
pub fn workspace_id(host_id: &str, canonical_path: &str) -> String {
    format!(
        "ws-{}",
        stable_hash(&format!("{host_id}\u{1f}{canonical_path}"))
    )
}

fn path_error(code: ErrorCode, reason: &str) -> PathResolutionError {
    PathResolutionError {
        code,
        reason: reason.to_string(),
    }
}

enum GitIdentity {
    Repository {
        repo_group_id: String,
        worktree_id: String,
    },
    NotRepository,
    Unreadable,
}

fn git_identity(host_id: &str, start: &Path) -> GitIdentity {
    let Some(checkout_root) = find_checkout_root(start) else {
        return GitIdentity::NotRepository;
    };
    let dot_git = checkout_root.join(".git");
    let mut git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        match read_git_dir_file(&dot_git, &checkout_root) {
            Some(path) => path,
            None => return GitIdentity::Unreadable,
        }
    };
    if !git_dir.is_dir() {
        return GitIdentity::Unreadable;
    }
    if let Some(common) = read_common_dir(&git_dir) {
        git_dir = common;
    }
    if !git_dir.join("HEAD").is_file() {
        return GitIdentity::Unreadable;
    }
    let common_text = canonical_text(&git_dir);
    // The checkout root is the worktree identity, not the workspace
    // subdirectory that happened to be opened. Two workspaces under one
    // checkout must share worktreeId while keeping distinct workspace rows.
    let checkout_text = canonical_text(&checkout_root);
    GitIdentity::Repository {
        repo_group_id: format!(
            "git-{}",
            stable_hash(&format!("{host_id}\u{1f}{common_text}"))
        ),
        worktree_id: format!(
            "wt-{}",
            stable_hash(&format!("{host_id}\u{1f}{checkout_text}"))
        ),
    }
}

/// Walks up from the workspace looking for a `.git` file or directory. The
/// enclosing checkout root (not the workspace subdirectory) is where Git
/// metadata is read; the workspace itself remains a separate identity.
fn find_checkout_root(start: &Path) -> Option<PathBuf> {
    for ancestor in start.ancestors() {
        let candidate = ancestor.join(".git");
        if candidate.is_dir() || candidate.is_file() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

fn read_git_dir_file(dot_git: &Path, base: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let line = text.lines().next()?.trim();
    let rest = line.strip_prefix("gitdir:")?.trim();
    if rest.is_empty() {
        return None;
    }
    let candidate = Path::new(rest);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    };
    std::fs::canonicalize(joined).ok()
}

fn read_common_dir(git_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    let line = text.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let candidate = Path::new(line);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        git_dir.join(candidate)
    };
    std::fs::canonicalize(joined).ok()
}

/// Normalizes a path for display without inventing a canonical identity.
pub fn normalize_display_path(raw: &str) -> Result<String, PathResolutionError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(path_error(
            ErrorCode::InvalidArgument,
            "path must not be empty",
        ));
    }
    if trimmed.contains('\0') {
        return Err(path_error(
            ErrorCode::InvalidArgument,
            "path contains a NUL character",
        ));
    }
    #[cfg(windows)]
    {
        normalize_windows(trimmed)
    }
    #[cfg(not(windows))]
    {
        normalize_unix(trimmed)
    }
}

#[cfg(windows)]
fn normalize_windows(raw: &str) -> Result<String, PathResolutionError> {
    let unified = raw.replace('/', "\\");
    let stripped = if let Some(rest) = unified.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = unified.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        unified
    };

    if let Some(unc) = stripped.strip_prefix(r"\\") {
        let mut parts = unc
            .split('\\')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        if parts.len() < 2 {
            return Err(path_error(
                ErrorCode::InvalidArgument,
                "UNC path must include a server and share",
            ));
        }
        let server = parts.remove(0);
        let share = parts.remove(0);
        let tail = collapse_components(&parts)?;
        let mut output = format!(r"\\{server}\{share}");
        if !tail.is_empty() {
            output.push('\\');
            output.push_str(&tail.join("\\"));
        }
        return Ok(output);
    }

    let bytes = stripped.as_bytes();
    if bytes.len() < 2 || bytes[1] != b':' || !bytes[0].is_ascii_alphabetic() {
        return Err(path_error(
            ErrorCode::InvalidArgument,
            "path must be an absolute Windows path",
        ));
    }
    if bytes.len() == 2 {
        return Err(path_error(
            ErrorCode::InvalidArgument,
            "drive-relative path is not absolute",
        ));
    }
    if bytes[2] != b'\\' {
        return Err(path_error(
            ErrorCode::InvalidArgument,
            "path must be an absolute Windows path",
        ));
    }
    let drive = (bytes[0] as char).to_ascii_uppercase();
    let tail = &stripped[3..];
    let parts = tail
        .split('\\')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let collapsed = collapse_components(&parts)?;
    if collapsed.is_empty() {
        return Ok(format!("{drive}:\\"));
    }
    Ok(format!("{drive}:\\{}", collapsed.join("\\")))
}

#[cfg(windows)]
fn collapse_components(parts: &[&str]) -> Result<Vec<String>, PathResolutionError> {
    let mut output: Vec<String> = Vec::new();
    for part in parts {
        match *part {
            "." => {}
            ".." => {
                if output.pop().is_none() {
                    return Err(path_error(
                        ErrorCode::InvalidArgument,
                        "path escapes its root",
                    ));
                }
            }
            value if value.contains(':') => {
                return Err(path_error(
                    ErrorCode::InvalidArgument,
                    "path contains an alternate data stream",
                ));
            }
            value => output.push(value.to_string()),
        }
    }
    Ok(output)
}

#[cfg(not(windows))]
fn normalize_unix(raw: &str) -> Result<String, PathResolutionError> {
    if !raw.starts_with('/') {
        return Err(path_error(
            ErrorCode::InvalidArgument,
            "path must be absolute",
        ));
    }
    let mut output: Vec<&str> = Vec::new();
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if output.pop().is_none() {
                    return Err(path_error(
                        ErrorCode::InvalidArgument,
                        "path escapes its root",
                    ));
                }
            }
            value => output.push(value),
        }
    }
    if output.is_empty() {
        return Ok("/".to_string());
    }
    Ok(format!("/{}", output.join("/")))
}

/// Converts an OS path to the normalized display form, stripping the Windows
/// verbatim prefix added by `canonicalize`.
pub(crate) fn canonical_text(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let stripped = if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw.into_owned()
    };
    normalize_display_path(&stripped).unwrap_or(stripped)
}

/// Stable FNV-1a pair (128 bits of output, 64-bit arithmetic) so ids do not
/// depend on the platform hasher or on absolute paths being exposed.
fn stable_hash(input: &str) -> String {
    let mut left: u64 = 0xcbf2_9ce4_8422_2325;
    let mut right: u64 = 0x8422_2325_cbf2_9ce4;
    for byte in input.as_bytes() {
        left ^= u64::from(*byte);
        left = left.wrapping_mul(0x0000_0100_0000_01b3);
        right = right.rotate_left(5) ^ u64::from(*byte);
        right = right.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{left:016x}{right:016x}")
}

#[cfg(all(test, windows))]
mod unit_tests {
    use super::*;

    #[test]
    fn windows_normalization_keeps_case_but_fixes_drive_and_separators() {
        let normalized = normalize_display_path(r"c:/Work/项目 Alpha/./sub/../").unwrap();
        assert_eq!(normalized, r"C:\Work\项目 Alpha");
        assert_eq!(
            normalize_display_path(r"\\?\C:\Work\Alpha").unwrap(),
            r"C:\Work\Alpha"
        );
        assert_eq!(
            normalize_display_path(r"\\?\UNC\server\share\dir").unwrap(),
            r"\\server\share\dir"
        );
    }

    #[test]
    fn windows_normalization_rejects_relative_and_escaping_paths() {
        assert!(normalize_display_path("Work\\Alpha").is_err());
        assert!(normalize_display_path("C:Work").is_err());
        assert!(normalize_display_path(r"C:\..\Windows").is_err());
        assert!(normalize_display_path(r"C:\Work\file:stream").is_err());
    }

    #[test]
    fn stable_hash_is_deterministic_and_case_sensitive() {
        let first = stable_hash("host\u{1f}C:\\Work\\Alpha");
        let second = stable_hash("host\u{1f}C:\\Work\\Alpha");
        let different = stable_hash("host\u{1f}C:\\Work\\alpha");
        assert_eq!(first, second);
        assert_ne!(first, different);
        assert_eq!(first.len(), 32);
    }
}
