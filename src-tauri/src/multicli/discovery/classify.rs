//! Process → product recognition and OS parent-chain evidence.
//!
//! Rules (all enforced here):
//! - A process is an instance only when its canonical executable is a verified
//!   installation entry, or its Node package entry is a declared one, or its
//!   file name matches a signature **and** it sits in a trusted directory.
//! - A product word in a later argument (prompt text, `echo`, a script path
//!   that is not a declared entry) never matches.
//! - OS parentage is not delegation and not a shared session. The link is kept
//!   only when both identities were read in the same snapshot and the parent
//!   was created no later than the child; otherwise the chain is marked broken
//!   with a reason.
//! - A working directory is unknown unless the CLI was given an explicit
//!   directory flag. Nothing reads the PEB, the environment or process memory
//!   to guess it.

use super::detect::DetectedInstallation;
use super::signature::{
    classify_command, file_name_lower, normalize_key, EntryKind, InstallRootKind,
    SignatureRegistry, TrustedDirectories,
};
use super::winproc::RawProcess;
use crate::multicli::domain::ProcessIdentity;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRef {
    pub installation_id: String,
    pub agent_id: String,
    pub product: String,
    pub signature_id: String,
    pub entry_kind: EntryKind,
    pub known_flags: &'static [&'static str],
    pub cwd_flags: &'static [&'static str],
}

#[derive(Debug, Clone, Default)]
pub struct InstallationIndex {
    /// Verified executable / package entry → install.
    pub by_entry_path: BTreeMap<String, InstallRef>,
    /// Shim path → install (used for evidence, not for instance rows).
    pub by_shim_path: BTreeMap<String, InstallRef>,
    pub trusted: TrustedDirectories,
}

impl InstallationIndex {
    pub fn is_empty(&self) -> bool {
        self.by_entry_path.is_empty()
    }

    pub fn ref_for_entry(&self, path: &str) -> Option<&InstallRef> {
        self.by_entry_path.get(&normalize_key(path))
    }
}

/// Builds the recognition index from a completed installation scan.
pub fn build_index(
    installations: &[DetectedInstallation],
    registry: &SignatureRegistry,
    environment: &super::detect::ScannerEnvironment,
) -> InstallationIndex {
    let mut index = InstallationIndex::default();
    index.trusted.path_entries = environment
        .path_dirs
        .iter()
        .map(|directory| normalize_key(&directory.to_string_lossy()))
        .collect();

    for signature in registry.all() {
        for root in signature.install_roots {
            if root.kind == InstallRootKind::Path {
                continue;
            }
            let base = match root.kind {
                InstallRootKind::UserProfile => environment.user_profile.as_deref(),
                InstallRootKind::LocalAppData => environment.local_app_data.as_deref(),
                InstallRootKind::AppData => environment.app_data.as_deref(),
                InstallRootKind::ProgramFiles => environment.program_files.as_deref(),
                InstallRootKind::Path => None,
            };
            if let Some(base) = base {
                let path = if root.relative.is_empty() {
                    base.to_path_buf()
                } else {
                    base.join(root.relative)
                };
                index
                    .trusted
                    .install_roots
                    .push(normalize_key(&path.to_string_lossy()));
            }
        }
    }

    for installation in installations {
        if !installation.is_runnable() {
            continue;
        }
        let Some(signature) = registry.by_id(&installation.signature_id) else {
            continue;
        };
        let reference = InstallRef {
            installation_id: installation.install_id.clone(),
            agent_id: installation.agent_id.clone(),
            product: installation.product.clone(),
            signature_id: installation.signature_id.clone(),
            entry_kind: installation.entry_kind,
            known_flags: signature.known_flags,
            cwd_flags: signature.cwd_flags,
        };
        match installation.entry_kind {
            EntryKind::NativeExe => {
                if let Some(executable) = &installation.executable {
                    let key = executable
                        .canonical_path
                        .as_deref()
                        .unwrap_or(&executable.display_path);
                    let normalized = normalize_key(key);
                    index.trusted.installation_entries.push(normalized.clone());
                    index.by_entry_path.insert(normalized, reference.clone());
                }
            }
            _ => {
                if let Some(script) = &installation.entry_script {
                    let key = script
                        .canonical_path
                        .as_deref()
                        .unwrap_or(&script.display_path);
                    let normalized = normalize_key(key);
                    index.trusted.installation_entries.push(normalized.clone());
                    index.by_entry_path.insert(normalized, reference.clone());
                }
                if let Some(shim) = &installation.executable {
                    let key = shim.canonical_path.as_deref().unwrap_or(&shim.display_path);
                    index
                        .by_shim_path
                        .insert(normalize_key(key), reference.clone());
                }
            }
        }
    }
    index
}

/// Recognition result for one raw process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recognized {
    pub reference: InstallRef,
    /// How *this* process entered the product (native image vs node entry).
    pub entry_kind: EntryKind,
    pub strength: super::snapshot::RecognitionStrength,
    pub evidence: Vec<String>,
    pub cwd_argument: Option<String>,
}

/// Recognises one process. Recognition only ever looks at the executable and
/// the *entry* arguments; later arguments are never scanned for brand words.
pub fn recognize(
    raw: &RawProcess,
    index: &InstallationIndex,
    registry: &SignatureRegistry,
) -> Option<Recognized> {
    let executable = raw.executable.as_ref()?;
    let normalized_exe = normalize_key(&executable.to_string_lossy());

    if let Some(reference) = index.by_entry_path.get(&normalized_exe) {
        // The installation's entry kind describes how the product is normally
        // launched; this *process* was launched as its executable image.
        let invocation_kind = if reference.entry_kind == EntryKind::NativeExe {
            EntryKind::NativeExe
        } else {
            EntryKind::NodePackage
        };
        return Some(Recognized {
            reference: reference.clone(),
            entry_kind: invocation_kind,
            strength: super::snapshot::RecognitionStrength::Confirmed,
            evidence: vec!["installation-exe".to_string()],
            cwd_argument: extract_cwd_argument(&raw.argv, reference.cwd_flags),
        });
    }

    let mut argv = raw.argv.clone();
    // argv[0] is caller-controlled on Windows; recognition follows the OS
    // image, never a program name supplied by another process.
    if argv.is_empty() {
        argv.push(String::new());
    }
    argv[0] = executable.to_string_lossy().to_string();
    let matched = classify_command(&argv, registry, &index.trusted)?;
    // A native filename found somewhere on PATH is only an installation
    // candidate. It becomes a product instance after detector confirmation.
    if matched.entry_kind == EntryKind::NativeExe {
        return None;
    }
    let signature = registry.by_id(matched.signature_id)?;

    // Prefer the install behind the matched entry script so the instance is
    // attributable to one installation rather than to a name hit.
    let mut reference = None;
    if matched.entry_kind == EntryKind::NodePackage {
        if let Some(script) = super::signature::node_entry_script(&argv) {
            reference = index.by_entry_path.get(&normalize_key(script)).cloned();
        }
    }
    let reference = reference.unwrap_or_else(|| InstallRef {
        installation_id: String::new(),
        agent_id: matched.agent_id.to_string(),
        product: signature.product.to_string(),
        signature_id: signature.signature_id.to_string(),
        entry_kind: matched.entry_kind,
        known_flags: signature.known_flags,
        cwd_flags: signature.cwd_flags,
    });

    Some(Recognized {
        cwd_argument: extract_cwd_argument(&raw.argv, reference.cwd_flags),
        entry_kind: matched.entry_kind,
        reference,
        strength: if matched.confirmed {
            super::snapshot::RecognitionStrength::Confirmed
        } else {
            super::snapshot::RecognitionStrength::Likely
        },
        evidence: vec![matched.evidence],
    })
}

/// Extracts an explicit working-directory argument. Only signature-declared
/// flags are honoured; the value must be an absolute path.
pub fn extract_cwd_argument(argv: &[String], cwd_flags: &[&'static str]) -> Option<String> {
    if cwd_flags.is_empty() {
        return None;
    }
    let mut index = 1usize;
    while index < argv.len() {
        let token = argv[index].as_str();
        if let Some((flag, value)) = token.split_once('=') {
            if cwd_flags.contains(&flag) && is_absolute_directory(value) {
                return Some(value.to_string());
            }
        }
        if cwd_flags.contains(&token) {
            if let Some(value) = argv.get(index + 1) {
                if is_absolute_directory(value) {
                    return Some(value.clone());
                }
            }
        }
        index += 1;
    }
    None
}

fn is_absolute_directory(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 1024 {
        return false;
    }
    let path = Path::new(trimmed);
    path.is_absolute() && !trimmed.contains('\0')
}

/// Validates an OS parent link. Returns the identity plus a break reason.
pub fn parent_link(
    child_creation: &str,
    parent: Option<(ProcessIdentity, bool)>,
) -> (Option<ProcessIdentity>, Option<String>) {
    match parent {
        None => (None, Some("parent-not-found".to_string())),
        Some((identity, verified)) => {
            if !verified {
                return (None, Some("parent-unverified".to_string()));
            }
            // Both values are fixed-width UTC strings, so lexicographic order
            // is chronological order; a parent created after its child means
            // the PID was recycled.
            if identity.creation_time > child_creation.to_string() {
                return (None, Some("parent-newer-than-child".to_string()));
            }
            (Some(identity), None)
        }
    }
}

pub fn source_program(raw: &RawProcess, parents: &BTreeMap<u32, RawProcess>) -> Option<String> {
    let parent = raw.parent_pid.and_then(|pid| parents.get(&pid))?;
    if parent.name.is_empty() {
        None
    } else {
        Some(parent.name.clone())
    }
}

/// The program name used in the discovery view. Only the base name is exposed.
pub fn display_program_name(path: &Path) -> String {
    let name = file_name_lower(path);
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::discovery::detect::{
        DetectedInstallation, InstallStatus, ScannerEnvironment,
    };
    use crate::multicli::discovery::signature::SignatureRegistry;
    use crate::multicli::discovery::snapshot::RecognitionStrength;
    use std::path::PathBuf;

    fn raw(executable: &str, argv: &[&str]) -> RawProcess {
        RawProcess {
            pid: 100,
            parent_pid: Some(1),
            name: "x".to_string(),
            executable: Some(PathBuf::from(executable)),
            argv: argv.iter().map(|item| item.to_string()).collect(),
            start_time_hint: 0,
        }
    }

    fn install(
        agent: &str,
        signature: &str,
        exe: &str,
        canonical: Option<&str>,
    ) -> DetectedInstallation {
        DetectedInstallation {
            agent_id: agent.to_string(),
            signature_id: signature.to_string(),
            product: "P".to_string(),
            status: InstallStatus::Installed,
            entry_kind: EntryKind::NativeExe,
            executable: Some(crate::multicli::domain::HostPath {
                host_id: "local-windows".to_string(),
                display_path: exe.to_string(),
                canonical_path: canonical.map(|value| value.to_string()),
            }),
            runtime: None,
            entry_script: None,
            interpreter: None,
            issue_code: None,
            issue_reason: None,
            discovered_from: "path".to_string(),
            identity: format!("canonical:{exe}"),
            identity_kind: "canonical".to_string(),
            config_profile_id: "unconfirmed-local".to_string(),
            install_id: format!("install-{agent}"),
            entry_priority: 0,
        }
    }

    #[test]
    fn verified_installations_win_over_name_matches() {
        let registry = SignatureRegistry::standard();
        let environment = ScannerEnvironment::default();
        let index = build_index(
            &[install(
                "codex",
                "codex-cli",
                r"C:\Programs\Codex\bin\codex.exe",
                Some(r"C:\Programs\Codex\bin\codex.exe"),
            )],
            &registry,
            &environment,
        );
        let matched = recognize(
            &raw(
                r"C:\Programs\Codex\bin\codex.exe",
                &[r"C:\Programs\Codex\bin\codex.exe", "--version"],
            ),
            &index,
            &registry,
        )
        .unwrap();
        assert_eq!(matched.strength, RecognitionStrength::Confirmed);
        assert_eq!(matched.entry_kind, EntryKind::NativeExe);
        assert_eq!(matched.evidence, vec!["installation-exe".to_string()]);
        assert_eq!(matched.reference.agent_id, "codex");
    }

    #[test]
    fn untrusted_same_name_binary_is_not_a_product() {
        let registry = SignatureRegistry::standard();
        let index = build_index(&[], &registry, &ScannerEnvironment::default());
        assert!(recognize(
            &raw(
                r"C:\Users\me\Downloads\codex.exe",
                &[r"C:\Users\me\Downloads\codex.exe"]
            ),
            &index,
            &registry
        )
        .is_none());
    }

    #[test]
    fn cwd_flag_is_the_only_workspace_evidence() {
        let flags: &[&'static str] = &["-C", "--cd"];
        assert_eq!(
            extract_cwd_argument(
                &[
                    "codex.exe".into(),
                    "-C".into(),
                    r"C:\Work\Alpha".into(),
                    "exec".into(),
                ],
                flags
            ),
            Some(r"C:\Work\Alpha".to_string())
        );
        assert_eq!(
            extract_cwd_argument(&["codex.exe".into(), "--cd=C:\\Work\\Beta".into()], flags),
            Some(r"C:\Work\Beta".to_string())
        );
        assert_eq!(
            extract_cwd_argument(
                &["codex.exe".into(), "exec".into(), "C:\\Work\\Gamma".into()],
                flags
            ),
            None
        );
        assert_eq!(
            extract_cwd_argument(&["claude.exe".into(), "--add-dir".into(), "x".into()], &[]),
            None
        );
    }

    #[test]
    fn parent_links_require_verification_and_time_order() {
        let parent = ProcessIdentity {
            host_id: "local-windows".into(),
            pid: 10,
            creation_time: "2026-11-02T01:00:00.0000000Z".into(),
        };
        assert_eq!(
            parent_link("2026-11-02T01:00:05.0000000Z", Some((parent.clone(), true))),
            (Some(parent.clone()), None)
        );
        assert_eq!(
            parent_link("2026-11-02T00:59:00.0000000Z", Some((parent.clone(), true))).1,
            Some("parent-newer-than-child".to_string())
        );
        assert_eq!(
            parent_link("2026-11-02T01:00:05.0000000Z", Some((parent, false))).1,
            Some("parent-unverified".to_string())
        );
        assert_eq!(
            parent_link("2026-11-02T01:00:05.0000000Z", None).1,
            Some("parent-not-found".to_string())
        );
    }
}
