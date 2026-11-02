//! Bounded installation discovery.
//!
//! Scanned locations are limited to:
//! - the live `PATH` (at most 64 directories, non-recursive),
//! - the install roots declared by each signature (at most 64 roots).
//!
//! There is no full-disk walk. A directory that merely exists is **not** an
//! installation: an entry is only reported when an executable or a parsed shim
//! is found, and a shim whose target is gone is reported as `missing-target`
//! instead of being upgraded to "installed".
//!
//! `%APPDATA%\npm\node_modules\@openai` can survive an uninstall as an empty
//! directory; that is reported as `historical-residue`, not as a product.

use super::signature::{
    extension_lower, file_name_lower, file_stem_lower, normalize_key, EntryKind, InstallRootKind,
    InstallSignature, SignatureRegistry,
};
use super::wrapper::{parse_shim, NodeInterpreter, WrapperIssue};
use crate::multicli::domain::HostPath;
use crate::multicli::path::{canonical_text, normalize_display_path, stable_hash};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MAX_PATH_DIRECTORIES: usize = 64;
pub const MAX_INSTALL_ROOTS: usize = 64;
pub const MAX_ENTRIES_PER_DIRECTORY: usize = 4096;

/// Config profile used when the account/profile of an installation cannot be
/// established read-only. It is explicitly *not* `default` and never carries
/// an executable path.
pub const UNCONFIRMED_PROFILE: &str = "unconfirmed-local";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InstallStatus {
    HistoricalResidue,
    MissingTarget,
    Unverified,
    Installed,
}

impl InstallStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Unverified => "unverified",
            Self::MissingTarget => "missing-target",
            Self::HistoricalResidue => "historical-residue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedInstallation {
    pub agent_id: String,
    pub signature_id: String,
    pub product: String,
    pub status: InstallStatus,
    pub entry_kind: EntryKind,
    pub executable: Option<HostPath>,
    pub runtime: Option<HostPath>,
    pub entry_script: Option<HostPath>,
    pub interpreter: Option<NodeInterpreter>,
    pub issue_code: Option<String>,
    pub issue_reason: Option<String>,
    pub discovered_from: String,
    /// `canonical:<exe>` / `package:<entry script>` / `shim:<shim>` /
    /// `residue:<dir>`; never contains a guessed path.
    pub identity: String,
    pub identity_kind: String,
    pub config_profile_id: String,
    pub install_id: String,
    /// 0 = the product's primary entry name, higher = an alias of the same
    /// package. Used only to choose which alias represents the install.
    pub entry_priority: u8,
}

impl DetectedInstallation {
    pub fn is_runnable(&self) -> bool {
        self.status == InstallStatus::Installed
    }

    fn dedupe_key(&self) -> (String, u8, u8, u8) {
        (
            self.identity.clone(),
            self.status as u8,
            match self.entry_kind {
                EntryKind::NativeExe => 4,
                EntryKind::NodePackage => 3,
                EntryKind::CmdWrapper => 2,
                EntryKind::PowerShellWrapper => 1,
            },
            10u8.saturating_sub(self.entry_priority.min(10)),
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct ScannerEnvironment {
    pub path_dirs: Vec<PathBuf>,
    pub user_profile: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub app_data: Option<PathBuf>,
    pub program_files: Option<PathBuf>,
}

impl ScannerEnvironment {
    pub fn from_process_env() -> Self {
        let path_dirs = std::env::var_os("PATH")
            .map(|value| {
                std::env::split_paths(&value)
                    .take(MAX_PATH_DIRECTORIES)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            path_dirs,
            user_profile: std::env::var_os("USERPROFILE").map(PathBuf::from),
            local_app_data: std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
            app_data: std::env::var_os("APPDATA").map(PathBuf::from),
            program_files: std::env::var_os("ProgramFiles").map(PathBuf::from),
        }
    }

    fn base(&self, kind: InstallRootKind) -> Option<&Path> {
        match kind {
            InstallRootKind::Path => None,
            InstallRootKind::UserProfile => self.user_profile.as_deref(),
            InstallRootKind::LocalAppData => self.local_app_data.as_deref(),
            InstallRootKind::AppData => self.app_data.as_deref(),
            InstallRootKind::ProgramFiles => self.program_files.as_deref(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct InstallationScan {
    pub installations: Vec<DetectedInstallation>,
    pub scanned_roots: usize,
    pub entries_examined: usize,
    pub unreadable_roots: usize,
    pub truncated: bool,
}

struct RootCandidate {
    directory: PathBuf,
    source: &'static str,
}

/// Scans the bounded installation locations for the given registry.
pub fn scan_installations(
    registry: &SignatureRegistry,
    environment: &ScannerEnvironment,
    host_id: &str,
) -> InstallationScan {
    let mut scan = InstallationScan::default();
    let mut roots: Vec<RootCandidate> = Vec::new();
    let mut seen_roots = std::collections::BTreeSet::new();

    for directory in environment.path_dirs.iter().take(MAX_PATH_DIRECTORIES) {
        let key = normalize_key(&directory.to_string_lossy());
        if seen_roots.insert(key) {
            roots.push(RootCandidate {
                directory: directory.clone(),
                source: "path",
            });
        }
    }
    for signature in registry.all() {
        for root in signature.install_roots {
            if root.kind == InstallRootKind::Path {
                continue;
            }
            let Some(base) = environment.base(root.kind) else {
                continue;
            };
            let directory = if root.relative.is_empty() {
                base.to_path_buf()
            } else {
                base.join(root.relative)
            };
            let key = normalize_key(&directory.to_string_lossy());
            if seen_roots.insert(key) {
                roots.push(RootCandidate {
                    directory,
                    source: match root.kind {
                        InstallRootKind::AppData => "app-data-root",
                        InstallRootKind::LocalAppData => "local-app-data-root",
                        InstallRootKind::UserProfile => "user-profile-root",
                        InstallRootKind::ProgramFiles => "program-files-root",
                        InstallRootKind::Path => "path",
                    },
                });
            }
            if roots.len() >= MAX_INSTALL_ROOTS {
                scan.truncated = true;
                break;
            }
        }
    }

    let mut best: BTreeMap<String, DetectedInstallation> = BTreeMap::new();
    for root in &roots {
        if !root.directory.is_dir() {
            // A missing root is not an error and not an installation.
            continue;
        }
        scan.scanned_roots += 1;
        let entries = match std::fs::read_dir(&root.directory) {
            Ok(entries) => entries,
            Err(_) => {
                scan.unreadable_roots += 1;
                continue;
            }
        };
        for entry in entries.flatten().take(MAX_ENTRIES_PER_DIRECTORY) {
            let path = entry.path();
            let Some(file_type) = entry.file_type().ok() else {
                continue;
            };
            if !file_type.is_file() && !file_type.is_symlink() {
                continue;
            }
            scan.entries_examined += 1;
            for signature in registry.all() {
                if let Some(mut candidate) = examine_entry(&path, signature, root.source, host_id) {
                    if candidate.entry_kind == EntryKind::NativeExe
                        && candidate.is_runnable()
                        && !declared_native_location(&path, signature, environment)
                    {
                        candidate.status = InstallStatus::Unverified;
                        candidate.issue_code = Some("unverified-native-location".into());
                        candidate.issue_reason = Some(
                            "a matching filename on PATH is not product identity evidence".into(),
                        );
                    }
                    merge_candidate(&mut best, candidate);
                }
            }
        }
    }

    for signature in registry.all() {
        for residue in residue_entries(environment, host_id, signature, &best) {
            merge_candidate(&mut best, residue);
        }
    }

    let mut installations = best.into_values().collect::<Vec<_>>();
    installations.sort_by(|left, right| {
        left.agent_id
            .cmp(&right.agent_id)
            .then_with(|| status_rank(right).cmp(&status_rank(left)))
            .then_with(|| left.identity.cmp(&right.identity))
    });
    scan.installations = installations;
    scan
}

fn declared_native_location(
    path: &Path,
    signature: &InstallSignature,
    environment: &ScannerEnvironment,
) -> bool {
    let Some(parent) = path.parent().and_then(|p| std::fs::canonicalize(p).ok()) else {
        return false;
    };
    signature
        .install_roots
        .iter()
        .filter(|r| r.kind != InstallRootKind::Path)
        .any(|root| {
            environment
                .base(root.kind)
                .and_then(|base| std::fs::canonicalize(base.join(root.relative)).ok())
                .is_some_and(|known| {
                    normalize_key(&canonical_text(&known))
                        == normalize_key(&canonical_text(&parent))
                })
        })
}

fn status_rank(installation: &DetectedInstallation) -> u8 {
    installation.status as u8
}

fn merge_candidate(
    best: &mut BTreeMap<String, DetectedInstallation>,
    candidate: DetectedInstallation,
) {
    let key = candidate.identity.clone();
    match best.get(&key) {
        Some(existing) if existing.dedupe_key() >= candidate.dedupe_key() => {}
        _ => {
            best.insert(key, candidate);
        }
    }
}

fn examine_entry(
    path: &Path,
    signature: &InstallSignature,
    source: &'static str,
    host_id: &str,
) -> Option<DetectedInstallation> {
    let extension = extension_lower(path);
    let file_name = file_name_lower(path);
    let stem = file_stem_lower(path);

    if extension == "exe" {
        if !signature.matches_native_name(&file_name) {
            return None;
        }
        return Some(native_candidate(path, signature, source, host_id));
    }

    if matches!(extension.as_str(), "cmd" | "bat" | "ps1") {
        if !signature.matches_wrapper_name(&stem) {
            return None;
        }
        return Some(wrapper_candidate(path, signature, source, host_id));
    }

    // A bare npm shim (`codebuddy` with no extension) is a POSIX shell script.
    // Windows does not run it through `PATH`, and this stage does not parse
    // POSIX shims, so it is skipped rather than guessed.
    None
}

fn native_candidate(
    path: &Path,
    signature: &InstallSignature,
    source: &'static str,
    host_id: &str,
) -> DetectedInstallation {
    let (executable, identity, identity_kind, status, issue_code, issue_reason) =
        match std::fs::canonicalize(path) {
            Ok(canonical) => {
                let identity = format!("canonical:{}", normalize_key(&canonical_text(&canonical)));
                (
                    Some(host_path(host_id, path, Some(&canonical))),
                    identity,
                    "canonical".to_string(),
                    InstallStatus::Installed,
                    None,
                    None,
                )
            }
            Err(_) => (
                Some(host_path(host_id, path, None)),
                format!(
                    "unresolved-display:{}",
                    normalize_key(&path.to_string_lossy())
                ),
                "unresolved-display".to_string(),
                InstallStatus::MissingTarget,
                Some("missing-target".to_string()),
                Some("install entry could not be resolved".to_string()),
            ),
        };
    DetectedInstallation {
        agent_id: signature.agent_id.to_string(),
        signature_id: signature.signature_id.to_string(),
        product: signature.product.to_string(),
        status,
        entry_kind: EntryKind::NativeExe,
        executable,
        runtime: None,
        entry_script: None,
        interpreter: None,
        issue_code,
        issue_reason,
        discovered_from: source.to_string(),
        install_id: installation_id(host_id, signature.agent_id, &identity),
        identity,
        identity_kind,
        config_profile_id: UNCONFIRMED_PROFILE.to_string(),
        entry_priority: 0,
    }
}

fn wrapper_candidate(
    path: &Path,
    signature: &InstallSignature,
    source: &'static str,
    host_id: &str,
) -> DetectedInstallation {
    match parse_shim(path) {
        Ok(parsed) => {
            let script_text = canonical_text(&parsed.entry_script);
            if !signature.matches_node_entry(&normalize_key(&script_text)) {
                let identity = format!("package:{}", normalize_key(&script_text));
                return DetectedInstallation {
                    agent_id: signature.agent_id.to_string(),
                    signature_id: signature.signature_id.to_string(),
                    product: signature.product.to_string(),
                    status: InstallStatus::Unverified,
                    entry_kind: parsed.kind,
                    executable: Some(host_path(
                        host_id,
                        path,
                        std::fs::canonicalize(path).ok().as_deref(),
                    )),
                    runtime: interpreter_path(host_id, parsed.interpreter.clone()),
                    entry_script: Some(host_path(
                        host_id,
                        &parsed.entry_script,
                        Some(&parsed.entry_script),
                    )),
                    interpreter: Some(parsed.interpreter),
                    issue_code: Some("unexpected-entry-script".to_string()),
                    issue_reason: Some(
                        "shim does not point at the declared package entry".to_string(),
                    ),
                    discovered_from: source.to_string(),
                    install_id: installation_id(host_id, signature.agent_id, &identity),
                    identity,
                    identity_kind: "package".to_string(),
                    config_profile_id: UNCONFIRMED_PROFILE.to_string(),
                    entry_priority: wrapper_priority(path, signature),
                };
            }
            let identity = format!("package:{}", normalize_key(&script_text));
            DetectedInstallation {
                agent_id: signature.agent_id.to_string(),
                signature_id: signature.signature_id.to_string(),
                product: signature.product.to_string(),
                status: InstallStatus::Installed,
                entry_kind: parsed.kind,
                executable: Some(host_path(
                    host_id,
                    path,
                    std::fs::canonicalize(path).ok().as_deref(),
                )),
                runtime: interpreter_path(host_id, parsed.interpreter.clone()),
                entry_script: Some(host_path(
                    host_id,
                    &parsed.entry_script,
                    Some(&parsed.entry_script),
                )),
                interpreter: Some(parsed.interpreter),
                issue_code: None,
                issue_reason: None,
                discovered_from: source.to_string(),
                install_id: installation_id(host_id, signature.agent_id, &identity),
                identity,
                identity_kind: "package".to_string(),
                config_profile_id: UNCONFIRMED_PROFILE.to_string(),
                entry_priority: wrapper_priority(path, signature),
            }
        }
        Err(issue) => {
            let display = path.to_string_lossy().to_string();
            let identity = match &issue {
                WrapperIssue::MissingTarget { .. } => {
                    format!("shim:{}", normalize_key(&display))
                }
                _ => format!("unresolved-shim:{}", normalize_key(&display)),
            };
            let status = match &issue {
                WrapperIssue::MissingTarget { .. } => InstallStatus::MissingTarget,
                _ => InstallStatus::Unverified,
            };
            DetectedInstallation {
                agent_id: signature.agent_id.to_string(),
                signature_id: signature.signature_id.to_string(),
                product: signature.product.to_string(),
                status,
                entry_kind: if extension_lower(path) == "ps1" {
                    EntryKind::PowerShellWrapper
                } else {
                    EntryKind::CmdWrapper
                },
                executable: Some(host_path(
                    host_id,
                    path,
                    std::fs::canonicalize(path).ok().as_deref(),
                )),
                runtime: None,
                entry_script: None,
                interpreter: None,
                issue_code: Some(issue.code().to_string()),
                issue_reason: Some(issue.reason().to_string()),
                discovered_from: source.to_string(),
                install_id: installation_id(host_id, signature.agent_id, &identity),
                identity,
                identity_kind: "unresolved-display".to_string(),
                config_profile_id: UNCONFIRMED_PROFILE.to_string(),
                entry_priority: wrapper_priority(path, signature),
            }
        }
    }
}

fn residue_entries(
    environment: &ScannerEnvironment,
    host_id: &str,
    signature: &InstallSignature,
    best: &BTreeMap<String, DetectedInstallation>,
) -> Vec<DetectedInstallation> {
    let mut entries = Vec::new();
    for residue in signature.residue_paths {
        let Some(base) = environment.base(residue.kind) else {
            continue;
        };
        let directory = base.join(residue.relative);
        if !directory.is_dir() {
            continue;
        }
        let normalized = normalize_key(&directory.to_string_lossy());
        // An existing installation under this directory proves it is not a
        // residue.
        let covered = best.values().any(|installation| {
            if installation.agent_id != signature.agent_id {
                return false;
            }
            let under =
                |path: &HostPath| normalize_key(&path.display_path).starts_with(&normalized);
            installation.executable.as_ref().map(under).unwrap_or(false)
                || installation
                    .entry_script
                    .as_ref()
                    .map(under)
                    .unwrap_or(false)
        });
        if covered {
            continue;
        }
        let identity = format!("residue:{normalized}");
        entries.push(DetectedInstallation {
            agent_id: signature.agent_id.to_string(),
            signature_id: signature.signature_id.to_string(),
            product: signature.product.to_string(),
            status: InstallStatus::HistoricalResidue,
            entry_kind: EntryKind::NodePackage,
            executable: Some(host_path(host_id, &directory, None)),
            runtime: None,
            entry_script: None,
            interpreter: None,
            issue_code: Some("historical-residue".to_string()),
            issue_reason: Some(
                "install directory is present but contains no runnable entry".to_string(),
            ),
            discovered_from: "declared-residue".to_string(),
            install_id: installation_id(host_id, signature.agent_id, &identity),
            identity,
            identity_kind: "residue".to_string(),
            config_profile_id: UNCONFIRMED_PROFILE.to_string(),
            entry_priority: 9,
        });
    }
    entries
}

/// `0` when the shim uses the signature's primary name (`codebuddy`), `1` for
/// a declared alias (`cbc`), `2` for anything else. Only the preferred alias is
/// kept when several shims point at the same package.
fn wrapper_priority(path: &Path, signature: &InstallSignature) -> u8 {
    let stem = file_stem_lower(path);
    match signature.wrapper_names.first() {
        Some(primary) if *primary == stem => 0,
        _ if signature.matches_wrapper_name(&stem) => 1,
        _ => 2,
    }
}

fn interpreter_path(host_id: &str, interpreter: NodeInterpreter) -> Option<HostPath> {
    match interpreter {
        NodeInterpreter::Resolved(path) => Some(host_path(host_id, &path, Some(&path))),
        NodeInterpreter::FromPath => super::probe::resolve_node_from_path()
            .map(|path| host_path(host_id, &path, Some(&path))),
    }
}

fn host_path(host_id: &str, path: &Path, canonical: Option<&Path>) -> HostPath {
    let display = normalize_display_path(&path.to_string_lossy())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());
    HostPath {
        host_id: host_id.to_string(),
        display_path: display,
        canonical_path: canonical.map(canonical_text),
    }
}

/// Stable installation id derived from the complete identity. Re-scanning the
/// same physical install always produces the same id; a version bump never
/// creates a new one.
pub fn installation_id(host_id: &str, agent_id: &str, identity: &str) -> String {
    format!(
        "install-{}",
        stable_hash(&format!(
            "{host_id}\u{1f}{agent_id}\u{1f}{UNCONFIRMED_PROFILE}\u{1f}{identity}"
        ))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_root(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "multicli-detect-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn registry() -> SignatureRegistry {
        SignatureRegistry::standard()
    }

    fn env_with(root: &Path) -> ScannerEnvironment {
        ScannerEnvironment {
            path_dirs: vec![root.to_path_buf()],
            user_profile: Some(root.join("profile")),
            local_app_data: Some(root.join("local")),
            app_data: Some(root.join("roaming")),
            program_files: Some(root.join("programs")),
        }
    }

    #[test]
    fn existing_directories_alone_are_not_installations() {
        let root = temp_root("empty");
        fs::create_dir_all(root.join("roaming/npm/node_modules/@openai")).unwrap();
        fs::create_dir_all(root.join("local/Programs/OpenAI/Codex")).unwrap();
        let scan = scan_installations(&registry(), &env_with(&root), "local-windows");
        assert!(scan
            .installations
            .iter()
            .all(|item| item.status == InstallStatus::HistoricalResidue));
        assert!(scan
            .installations
            .iter()
            .any(|item| item.issue_code.as_deref() == Some("historical-residue")));
        assert!(!scan
            .installations
            .iter()
            .any(|item| item.status == InstallStatus::Installed));
    }

    #[test]
    fn broken_shim_is_missing_target_and_never_installed() {
        let root = temp_root("broken-shim");
        fs::write(
            root.join("codex.cmd"),
            r#"@echo off
"%dp0%\node.exe"  "%dp0%\node_modules\@openai\codex\bin\codex.js" %*
"#,
        )
        .unwrap();
        let scan = scan_installations(&registry(), &env_with(&root), "local-windows");
        let codex = scan
            .installations
            .iter()
            .find(|item| item.agent_id == "codex")
            .expect("broken shim is still reported");
        assert_eq!(codex.status, InstallStatus::MissingTarget);
        assert_eq!(codex.issue_code.as_deref(), Some("missing-target"));
        assert!(codex.entry_script.is_none());
    }

    #[test]
    fn two_shims_for_one_package_are_one_install_and_two_packages_are_two() {
        let root = temp_root("dedupe");
        let first = root.join("node_modules/@tencent-ai/codebuddy-code/bin");
        fs::create_dir_all(&first).unwrap();
        fs::write(first.join("codebuddy"), "#!/usr/bin/env node\n").unwrap();
        for shim in ["codebuddy.cmd", "codebuddy.ps1"] {
            fs::write(
                root.join(shim),
                r#"@echo off
"%dp0%\node.exe"  "%dp0%\node_modules\@tencent-ai\codebuddy-code\bin\codebuddy" %*
"#,
            )
            .unwrap();
        }
        let scan = scan_installations(&registry(), &env_with(&root), "local-windows");
        let codebuddy = scan
            .installations
            .iter()
            .filter(|item| item.agent_id == "codebuddy" && item.status == InstallStatus::Installed)
            .collect::<Vec<_>>();
        assert_eq!(codebuddy.len(), 1, "cmd+ps1 shims must not double count");

        // A second, separate package copy is a second installation.
        let second = root.join("second/node_modules/@tencent-ai/codebuddy-code/bin");
        fs::create_dir_all(&second).unwrap();
        fs::write(second.join("codebuddy"), "#!/usr/bin/env node\n").unwrap();
        fs::write(
            root.join("second/codebuddy.cmd"),
            r#"@echo off
"%dp0%\node.exe"  "%dp0%\node_modules\@tencent-ai\codebuddy-code\bin\codebuddy" %*
"#,
        )
        .unwrap();
        let mut environment = env_with(&root);
        environment.path_dirs.push(root.join("second"));
        let scan = scan_installations(&registry(), &environment, "local-windows");
        let count = scan
            .installations
            .iter()
            .filter(|item| item.agent_id == "codebuddy" && item.status == InstallStatus::Installed)
            .count();
        assert_eq!(count, 2, "two physical packages are two installations");
        // Repeating the scan must not introduce duplicates.
        let again = scan_installations(&registry(), &environment, "local-windows");
        assert_eq!(again.installations.len(), scan.installations.len());
        let mut ids = again
            .installations
            .iter()
            .map(|item| item.install_id.clone())
            .collect::<Vec<_>>();
        let unique = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), unique);
    }

    #[test]
    fn native_entry_and_unrelated_same_name_are_distinguished() {
        let root = temp_root("native");
        let trusted = root.join("local/Programs/OpenAI/Codex/bin");
        fs::create_dir_all(&trusted).unwrap();
        fs::write(trusted.join("codex.exe"), b"MZ").unwrap();
        let unrelated = root.join("downloads");
        fs::create_dir_all(&unrelated).unwrap();
        fs::write(unrelated.join("codex.exe"), b"MZ").unwrap();

        let scan = scan_installations(&registry(), &env_with(&root), "local-windows");
        let codex = scan
            .installations
            .iter()
            .filter(|item| item.agent_id == "codex")
            .collect::<Vec<_>>();
        assert_eq!(codex.len(), 1, "only the trusted root is scanned");
        assert_eq!(codex[0].status, InstallStatus::Installed);
        assert_eq!(codex[0].entry_kind, EntryKind::NativeExe);
        assert!(codex[0].identity.starts_with("canonical:"));
    }
}
