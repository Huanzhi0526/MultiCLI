//! Versioned installation signatures and strict entry recognition.
//!
//! A signature is *data*, not code. It declares:
//! - which executable file names and Node package entry suffixes belong to a
//!   product,
//! - which wrapper (shim) names may be parsed, and how,
//! - which install roots are trusted for that product,
//! - which argument flags are structural (safe to keep in a redacted command),
//! - the positive and negative samples the registry must satisfy.
//!
//! Only Codex, Claude and CodeBuddy are declared in S02. The other product
//! families stay out: adding more brand strings would not make them
//! "supported" and must not be claimed as such.
//!
//! Recognition is deliberately conservative:
//! - A native executable counts only when its path is a *verified* install
//!   entry, or when its file name matches **and** its directory is trusted
//!   (PATH entry / declared install root).
//! - An interpreted runtime (`node`, `python`, `cmd`, `powershell`) counts
//!   only when its entry script / wrapper argument resolves to a declared
//!   package entry. Product names inside later arguments (prompts, `echo`
//!   text) are never consulted.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Bumped when the signature data shape or its semantics change.
pub const SIGNATURE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    NativeExe,
    NodePackage,
    CmdWrapper,
    PowerShellWrapper,
}

impl EntryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NativeExe => "native-exe",
            Self::NodePackage => "node-package",
            Self::CmdWrapper => "cmd-wrapper",
            Self::PowerShellWrapper => "powershell-wrapper",
        }
    }

    pub fn is_wrapper(self) -> bool {
        matches!(self, Self::CmdWrapper | Self::PowerShellWrapper)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallRootKind {
    /// Every directory on the live `PATH`.
    Path,
    UserProfile,
    LocalAppData,
    AppData,
    ProgramFiles,
}

#[derive(Debug, Clone, Copy)]
pub struct InstallRoot {
    pub kind: InstallRootKind,
    /// Relative to the selected base directory; empty for `Path`.
    pub relative: &'static str,
}

/// A directory that proves a product *was* present but no longer runs.
#[derive(Debug, Clone, Copy)]
pub struct ResiduePath {
    pub kind: InstallRootKind,
    pub relative: &'static str,
}

#[derive(Debug, Clone)]
pub struct InstallSignature {
    pub signature_id: &'static str,
    pub agent_id: &'static str,
    pub product: &'static str,
    pub vendor: &'static str,
    /// Lower-case executable file names, including the `.exe` suffix.
    pub native_names: &'static [&'static str],
    /// Lower-case shim base names without extension.
    pub wrapper_names: &'static [&'static str],
    /// Lower-case Node package entry suffixes (`\`-separated).
    pub node_entry_suffixes: &'static [&'static str],
    pub install_roots: &'static [InstallRoot],
    /// Directories that prove a product *was* present but no longer runs.
    pub residue_paths: &'static [ResiduePath],
    /// Structural flags that may appear (without values) in a redacted command.
    pub known_flags: &'static [&'static str],
    /// Flags whose value is an explicit working directory.
    pub cwd_flags: &'static [&'static str],
    /// Markers that must appear in a successful `--version` output.
    pub version_markers: &'static [&'static str],
    pub version_args: &'static [&'static str],
    pub source: &'static str,
    pub positive_samples: &'static [&'static str],
    pub negative_samples: &'static [&'static str],
}

impl InstallSignature {
    pub fn knows_flag(&self, token: &str) -> bool {
        self.known_flags.iter().any(|flag| *flag == token)
    }

    pub fn cwd_flag(&self, token: &str) -> bool {
        self.cwd_flags.iter().any(|flag| *flag == token)
    }

    pub fn matches_native_name(&self, file_name: &str) -> bool {
        let lower = file_name.to_ascii_lowercase();
        self.native_names.iter().any(|name| *name == lower)
    }

    pub fn matches_wrapper_name(&self, file_stem: &str) -> bool {
        let lower = file_stem.to_ascii_lowercase();
        self.wrapper_names.iter().any(|name| *name == lower)
    }

    /// Matches a normalized (lower-case, `\`-separated) path against a declared
    /// Node package entry suffix. The suffix must sit on a path boundary.
    pub fn matches_node_entry(&self, normalized_path: &str) -> bool {
        self.node_entry_suffixes.iter().any(|suffix| {
            normalized_path == *suffix
                || normalized_path.ends_with(&format!("\\{suffix}"))
                || normalized_path.ends_with(&format!("/{suffix}"))
        })
    }
}

#[derive(Debug, Clone)]
pub struct SignatureRegistry {
    signatures: Vec<InstallSignature>,
}

const PATH_ROOT: InstallRoot = InstallRoot {
    kind: InstallRootKind::Path,
    relative: "",
};

pub const CODEX_SIGNATURE_ID: &str = "codex-cli";
pub const CLAUDE_SIGNATURE_ID: &str = "claude-code";
pub const CODEBUDDY_SIGNATURE_ID: &str = "codebuddy-code";

impl SignatureRegistry {
    /// The three product families in scope for S02.
    pub fn standard() -> Self {
        let signatures = vec![
            InstallSignature {
                signature_id: CODEX_SIGNATURE_ID,
                agent_id: "codex",
                product: "Codex CLI",
                vendor: "OpenAI",
                native_names: &["codex.exe"],
                wrapper_names: &["codex"],
                node_entry_suffixes: &["node_modules\\@openai\\codex\\bin\\codex.js"],
                install_roots: &[
                    PATH_ROOT,
                    InstallRoot {
                        kind: InstallRootKind::LocalAppData,
                        relative: "Programs\\OpenAI\\Codex\\bin",
                    },
                    InstallRoot {
                        kind: InstallRootKind::AppData,
                        relative: "npm",
                    },
                ],
                residue_paths: &[
                    ResiduePath {
                        kind: InstallRootKind::AppData,
                        relative: "npm\\node_modules\\@openai",
                    },
                    ResiduePath {
                        kind: InstallRootKind::AppData,
                        relative: "npm\\node_modules\\@openai\\codex",
                    },
                    ResiduePath {
                        kind: InstallRootKind::LocalAppData,
                        relative: "Programs\\OpenAI\\Codex",
                    },
                ],
                known_flags: &[
                    "--version",
                    "-V",
                    "--help",
                    "-h",
                    "-c",
                    "--config",
                    "-C",
                    "--cd",
                    "-m",
                    "--model",
                    "-p",
                    "--profile",
                    "-s",
                    "--sandbox",
                    "-a",
                    "--ask-for-approval",
                    "--full-auto",
                    "--json",
                    "--oss",
                    "--search",
                    "--no-alt-screen",
                ],
                cwd_flags: &["-C", "--cd"],
                version_markers: &["codex-cli"],
                version_args: &["--version"],
                source: "Codex CLI 0.166.0 native install layout on this Windows host; \
                         `codex --help` inspected read-only for structural flags. \
                         Node package entry follows the published @openai/codex layout.",
                positive_samples: &[
                    r"C:\Users\me\AppData\Local\Programs\OpenAI\Codex\bin\codex.exe --version",
                    r"C:\Users\me\AppData\Local\Programs\OpenAI\Codex\bin\codex.exe exec refactor the claude wrapper",
                    r"node C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\bin\codex.js --version",
                ],
                negative_samples: &[
                    r"node -e console.log('codex --help')",
                    r"C:\Users\me\Downloads\codex.exe --version",
                    r"python C:\Users\me\scripts\codex_helper.py",
                    r"cmd.exe /c echo codex claude codebuddy",
                    r"C:\Users\me\AppData\Local\Programs\OpenAI\Codex\bin\codex-code-mode-host.exe",
                ],
            },
            InstallSignature {
                signature_id: CLAUDE_SIGNATURE_ID,
                agent_id: "claude",
                product: "Claude Code",
                vendor: "Anthropic",
                native_names: &["claude.exe"],
                wrapper_names: &["claude"],
                node_entry_suffixes: &["node_modules\\@anthropic-ai\\claude-code\\cli.js"],
                install_roots: &[
                    PATH_ROOT,
                    InstallRoot {
                        kind: InstallRootKind::UserProfile,
                        relative: ".local\\bin",
                    },
                    InstallRoot {
                        kind: InstallRootKind::AppData,
                        relative: "npm",
                    },
                ],
                residue_paths: &[
                    ResiduePath {
                        kind: InstallRootKind::AppData,
                        relative: "npm\\node_modules\\@anthropic-ai",
                    },
                    ResiduePath {
                        kind: InstallRootKind::AppData,
                        relative: "npm\\node_modules\\@anthropic-ai\\claude-code",
                    },
                ],
                known_flags: &[
                    "--version",
                    "-v",
                    "--help",
                    "-h",
                    "-p",
                    "--print",
                    "-c",
                    "--continue",
                    "-r",
                    "--resume",
                    "--model",
                    "--add-dir",
                    "--permission-mode",
                    "--output-format",
                    "--input-format",
                    "--debug",
                    "--mcp-config",
                    "--settings",
                    "--bare",
                    "--brief",
                ],
                cwd_flags: &["--add-dir"],
                version_markers: &["Claude Code"],
                version_args: &["--version"],
                source: "Claude Code 2.1.153 native install at %USERPROFILE%\\.local\\bin on \
                         this Windows host; `claude --help` inspected read-only. The \
                         @anthropic-ai/claude-code npm entry is the published package layout.",
                positive_samples: &[
                    r"C:\Users\me\.local\bin\claude.exe --version",
                    r"node C:\Users\me\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\cli.js --version",
                ],
                negative_samples: &[
                    r"C:\Users\me\Documents\claude.exe --version",
                    r"node C:\Users\me\scripts\claude-notes.js",
                    r"node C:\Users\me\node_modules\@anthropic-ai\claude-code-tools\cli.js --version",
                ],
            },
            InstallSignature {
                signature_id: CODEBUDDY_SIGNATURE_ID,
                agent_id: "codebuddy",
                product: "CodeBuddy Code",
                vendor: "Tencent",
                native_names: &["codebuddy.exe"],
                wrapper_names: &["codebuddy", "cbc"],
                node_entry_suffixes: &[
                    "node_modules\\@tencent-ai\\codebuddy-code\\bin\\codebuddy",
                ],
                install_roots: &[
                    PATH_ROOT,
                    InstallRoot {
                        kind: InstallRootKind::AppData,
                        relative: "npm",
                    },
                ],
                residue_paths: &[
                    ResiduePath {
                        kind: InstallRootKind::AppData,
                        relative: "npm\\node_modules\\@tencent-ai",
                    },
                    ResiduePath {
                        kind: InstallRootKind::AppData,
                        relative: "npm\\node_modules\\@tencent-ai\\codebuddy-code",
                    },
                ],
                known_flags: &[
                    "--version",
                    "-V",
                    "-v",
                    "--help",
                    "-h",
                    "-p",
                    "--print",
                    "-c",
                    "--continue",
                    "-r",
                    "--resume",
                    "--model",
                    "--add-dir",
                    "--permission-mode",
                    "--output-format",
                    "--input-format",
                    "--debug",
                    "-d",
                    "--verbose",
                    "--mcp-config",
                    "--serve",
                    "--acp",
                    "--json-schema",
                ],
                cwd_flags: &["--add-dir"],
                version_markers: &[],
                version_args: &["--version"],
                source: "CodeBuddy Code 2.179.0 installed as the @tencent-ai/codebuddy-code \
                         npm package; `%APPDATA%\\npm\\codebuddy.cmd` and `.ps1` shims parsed \
                         as data, never executed. `codebuddy --help` inspected read-only.",
                positive_samples: &[
                    r"node C:\Users\me\AppData\Roaming\npm\node_modules\@tencent-ai\codebuddy-code\bin\codebuddy --version",
                    r"node C:\Users\me\AppData\Roaming\npm\node_modules\@tencent-ai\codebuddy-code\bin\codebuddy -p hello",
                ],
                negative_samples: &[
                    r"node C:\Users\me\AppData\Roaming\npm\node_modules\@tencent-ai\codebuddy-code\bin\other-tool --version",
                    r"cmd.exe /c codebuddy --version",
                    r"node C:\Users\me\AppData\Roaming\npm\node_modules\@tencent-ai\codebuddy-code\README.md",
                ],
            },
        ];
        Self { signatures }
    }

    /// Registry with a single synthetic signature, for tests that need a
    /// product which is not installed on the host.
    #[cfg(test)]
    pub fn with_extra(signature: InstallSignature) -> Self {
        let mut registry = Self::standard();
        registry.signatures.push(signature);
        registry
    }

    pub fn all(&self) -> &[InstallSignature] {
        &self.signatures
    }

    pub fn by_agent(&self, agent_id: &str) -> Option<&InstallSignature> {
        self.signatures
            .iter()
            .find(|signature| signature.agent_id == agent_id)
    }

    pub fn by_id(&self, signature_id: &str) -> Option<&InstallSignature> {
        self.signatures
            .iter()
            .find(|signature| signature.signature_id == signature_id)
    }

    pub fn agent_ids(&self) -> Vec<&'static str> {
        self.signatures
            .iter()
            .map(|signature| signature.agent_id)
            .collect()
    }

    /// Every structural flag across the registry. Used by the redactor so an
    /// unknown flag is never echoed back.
    pub fn known_flags(&self) -> BTreeSet<&'static str> {
        self.signatures
            .iter()
            .flat_map(|signature| signature.known_flags.iter().copied())
            .collect()
    }
}

/// Every structural flag across the registry. Used by the redactor so an
/// unknown flag is never echoed back.
pub fn known_flags_set() -> &'static BTreeSet<&'static str> {
    static KNOWN: OnceLock<BTreeSet<&'static str>> = OnceLock::new();
    KNOWN.get_or_init(|| SignatureRegistry::standard().known_flags())
}

/// Why a command line did or did not match a product.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandMatch {
    pub signature_id: &'static str,
    pub agent_id: &'static str,
    pub entry_kind: EntryKind,
    /// `true` when the entry is a verified install path rather than a name
    /// hit in a trusted directory.
    pub confirmed: bool,
    pub evidence: String,
}

/// Directories that are allowed to carry a product-named executable.
#[derive(Debug, Clone, Default)]
pub struct TrustedDirectories {
    /// Lower-case, `\`-normalized, no trailing separator.
    pub path_entries: Vec<String>,
    /// Lower-case, `\`-normalized prefixes from declared install roots.
    pub install_roots: Vec<String>,
    /// Verified runtime executable paths keyed by normalized identity.
    pub installation_entries: Vec<String>,
}

impl TrustedDirectories {
    pub fn is_path_entry(&self, directory: &str) -> bool {
        let normalized = normalize_key(directory);
        self.path_entries.iter().any(|entry| *entry == normalized)
    }

    pub fn is_under_install_root(&self, directory: &str) -> bool {
        let normalized = normalize_key(directory);
        self.install_roots
            .iter()
            .any(|root| normalized == *root || normalized.starts_with(&format!("{root}\\")))
    }

    pub fn is_installation_entry(&self, path: &str) -> bool {
        let normalized = normalize_key(path);
        self.installation_entries
            .iter()
            .any(|entry| *entry == normalized)
    }
}

pub fn normalize_key(path: &str) -> String {
    path.replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase()
}

pub fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn file_stem_lower(path: &Path) -> String {
    path.file_stem()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn extension_lower(path: &Path) -> String {
    path.extension()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Runtimes that never carry a product identity by themselves.
pub fn is_generic_runtime(file_name: &str) -> bool {
    matches!(
        file_name,
        "node.exe"
            | "node"
            | "cmd.exe"
            | "cmd"
            | "powershell.exe"
            | "pwsh.exe"
            | "powershell"
            | "pwsh"
            | "python.exe"
            | "python"
            | "python3.exe"
            | "conhost.exe"
            | "windowsterminal.exe"
            | "wt.exe"
            | "bash.exe"
            | "wsl.exe"
            | "sh.exe"
    )
}

/// Node flags that consume the following token. Anything else beginning with
/// `-` makes argv[1] a flag rather than an entry script.
fn node_flag_takes_value(flag: &str) -> bool {
    matches!(
        flag,
        "-r" | "--require"
            | "--loader"
            | "--experimental-loader"
            | "--import"
            | "-e"
            | "--eval"
            | "-p"
            | "--print"
            | "--input-type"
            | "--conditions"
            | "--inspect-port"
            | "--title"
            | "--max-old-space-size"
    )
}

/// Returns the Node entry script when `args` is a plain `node <script> ...`
/// invocation. `node -e ...`, `node -p ...` and flag-only invocations return
/// `None`, so an inline expression can never be mistaken for a product.
pub fn node_entry_script(args: &[String]) -> Option<&str> {
    let mut index = 1;
    while index < args.len() {
        let token = args[index].as_str();
        if token == "--" {
            index += 1;
            break;
        }
        if token.starts_with('-') {
            if token == "-e"
                || token == "--eval"
                || token == "-p"
                || token == "--print"
                || token == "--inspect-brk"
                || token.starts_with("--inspect")
            {
                return None;
            }
            if node_flag_takes_value(token) {
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        return Some(token);
    }
    if index < args.len() {
        let token = args[index].as_str();
        if !token.starts_with('-') {
            return Some(token);
        }
    }
    None
}

/// Classifies a single command line without touching the filesystem.
///
/// `args` must contain the program as the first element (argv[0] as reported
/// by the OS), matching what `std::process::Command` and Windows report.
pub fn classify_command(
    args: &[String],
    registry: &SignatureRegistry,
    trusted: &TrustedDirectories,
) -> Option<CommandMatch> {
    let program = args.first()?;
    let program_path = Path::new(program);
    let file_name = file_name_lower(program_path);
    let normalized_program = normalize_key(program);

    // 1. A verified install entry always wins, whatever its file name.
    for signature in registry.all() {
        if trusted.is_installation_entry(&normalized_program) {
            return Some(CommandMatch {
                signature_id: signature.signature_id,
                agent_id: signature.agent_id,
                entry_kind: if signature.matches_native_name(&file_name) {
                    EntryKind::NativeExe
                } else {
                    EntryKind::NodePackage
                },
                confirmed: true,
                evidence: "installation-exe".to_string(),
            });
        }
    }

    // 2. A native executable name inside a trusted directory.
    if !is_generic_runtime(&file_name) {
        let directory = program_path
            .parent()
            .map(|parent| parent.to_string_lossy().to_string())
            .unwrap_or_default();
        let directory_trusted = trusted.is_path_entry(&directory)
            || trusted.is_under_install_root(&directory);
        for signature in registry.all() {
            if signature.matches_native_name(&file_name) && directory_trusted {
                return Some(CommandMatch {
                    signature_id: signature.signature_id,
                    agent_id: signature.agent_id,
                    entry_kind: EntryKind::NativeExe,
                    confirmed: false,
                    evidence: "path-entry+filename".to_string(),
                });
            }
        }
    }

    // 3. An interpreted runtime whose entry script is a declared package entry.
    if is_generic_runtime(&file_name) {
        if file_name.starts_with("node") {
            if let Some(script) = node_entry_script(args) {
                let normalized_script = normalize_key(script);
                for signature in registry.all() {
                    if signature.matches_node_entry(&normalized_script)
                        && trusted_directory_for_script(script, trusted)
                    {
                        return Some(CommandMatch {
                            signature_id: signature.signature_id,
                            agent_id: signature.agent_id,
                            entry_kind: EntryKind::NodePackage,
                            confirmed: true,
                            evidence: "node-package-entry".to_string(),
                        });
                    }
                }
            }
        }
        // cmd / powershell wrappers are recognised by their own shim parser in
        // `wrapper.rs`; a bare product word in argv is never enough here.
    }

    None
}

/// The package entry must live inside a trusted install root (or be a
/// declared installation entry) so a random copy of the same file name does
/// not become a product.
fn trusted_directory_for_script(script: &str, trusted: &TrustedDirectories) -> bool {
    let normalized = normalize_key(script);
    if trusted.is_installation_entry(&normalized) {
        return true;
    }
    let path = Path::new(script);
    let mut current = path.parent();
    while let Some(directory) = current {
        let directory_text = directory.to_string_lossy().to_string();
        if trusted.is_path_entry(&directory_text) || trusted.is_under_install_root(&directory_text)
        {
            return true;
        }
        current = directory.parent();
    }
    false
}

/// Splits a sample command line into argv, honouring double and single quotes.
pub fn split_sample(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for character in line.chars() {
        match quote {
            Some(active) if character == active => {
                quote = None;
            }
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => {
                quote = Some(character);
                started = true;
            }
            None if character.is_whitespace() => {
                if started || !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            None => {
                current.push(character);
                started = true;
            }
        }
    }
    if started || !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Result of evaluating the declared sample matrix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleOutcome {
    pub signature_id: &'static str,
    pub line: String,
    pub expected: Option<&'static str>,
    pub actual: Option<&'static str>,
}

impl SampleOutcome {
    pub fn passed(&self) -> bool {
        self.expected == self.actual
    }
}

/// Runs every declared positive/negative sample against the classifier. Used
/// by the unit tests so a signature cannot drift away from its samples.
pub fn check_samples(
    registry: &SignatureRegistry,
    trusted: &TrustedDirectories,
) -> Vec<SampleOutcome> {
    let mut outcomes = Vec::new();
    for signature in registry.all() {
        for line in signature.positive_samples {
            let args = split_sample(line);
            let actual = classify_command(&args, registry, trusted).map(|m| m.signature_id);
            outcomes.push(SampleOutcome {
                signature_id: signature.signature_id,
                line: (*line).to_string(),
                expected: Some(signature.signature_id),
                actual,
            });
        }
        for line in signature.negative_samples {
            let args = split_sample(line);
            let actual = classify_command(&args, registry, trusted).map(|m| m.signature_id);
            outcomes.push(SampleOutcome {
                signature_id: signature.signature_id,
                line: (*line).to_string(),
                expected: None,
                actual,
            });
        }
    }
    outcomes
}

/// Builds the trusted directory set used by the sample matrix and by the real
/// scanner (which merges the same rules with verified installation entries).
pub fn sample_trusted_directories(registry: &SignatureRegistry) -> TrustedDirectories {
    let mut trusted = TrustedDirectories {
        path_entries: vec![
            normalize_key(r"C:\Users\me\AppData\Roaming\npm"),
            normalize_key(r"C:\Users\me\.local\bin"),
            normalize_key(r"C:\Users\me\AppData\Local\Programs\OpenAI\Codex\bin"),
        ],
        install_roots: Vec::new(),
        installation_entries: Vec::new(),
    };
    for signature in registry.all() {
        for root in signature.install_roots {
            let text = match root.kind {
                InstallRootKind::Path => continue,
                InstallRootKind::UserProfile => format!(r"C:\Users\me\{}", root.relative),
                InstallRootKind::LocalAppData => {
                    format!(r"C:\Users\me\AppData\Local\{}", root.relative)
                }
                InstallRootKind::AppData => format!(r"C:\Users\me\AppData\Roaming\{}", root.relative),
                InstallRootKind::ProgramFiles => format!(r"C:\Program Files\{}", root.relative),
            };
            trusted.install_roots.push(normalize_key(&text));
        }
    }
    trusted
}

/// Resolves the `PATH`-style base directory for a declared install root.
pub fn base_directory(kind: InstallRootKind) -> Option<PathBuf> {
    match kind {
        InstallRootKind::Path => None,
        InstallRootKind::UserProfile => std::env::var_os("USERPROFILE").map(PathBuf::from),
        InstallRootKind::LocalAppData => std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
        InstallRootKind::AppData => std::env::var_os("APPDATA").map(PathBuf::from),
        InstallRootKind::ProgramFiles => std::env::var_os("ProgramFiles").map(PathBuf::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_positive_and_negative_samples_all_match() {
        let registry = SignatureRegistry::standard();
        let trusted = sample_trusted_directories(&registry);
        let outcomes = check_samples(&registry, &trusted);
        let failures = outcomes
            .iter()
            .filter(|outcome| !outcome.passed())
            .collect::<Vec<_>>();
        assert!(
            failures.is_empty(),
            "signature samples drifted: {failures:#?}"
        );
        // Four signatures' worth of coverage is not the claim: only the three
        // in-scope products are declared.
        assert_eq!(registry.all().len(), 3);
        assert_eq!(outcomes.len(), (3 + 5) + (2 + 3) + (2 + 3));
        assert_eq!(SIGNATURE_SCHEMA_VERSION, 1);
    }

    #[test]
    fn product_words_in_later_arguments_never_match() {
        let registry = SignatureRegistry::standard();
        let trusted = sample_trusted_directories(&registry);
        for line in [
            r#"node -e "console.log('launch codex or claude or codebuddy')""#,
            r"cmd.exe /c echo codex claude codebuddy",
            r"python -c print('codex')",
            r"notepad.exe C:\Users\me\notes\codex-prompts.txt",
        ] {
            let args = split_sample(line);
            assert!(
                classify_command(&args, &registry, &trusted).is_none(),
                "false positive for {line}"
            );
        }
    }

    #[test]
    fn inline_node_evaluation_is_never_a_package_entry() {
        assert!(node_entry_script(&["node".into(), "-e".into(), "1".into()]).is_none());
        assert!(node_entry_script(&["node".into(), "--print".into(), "1".into()]).is_none());
        assert_eq!(
            node_entry_script(&[
                "node".into(),
                "--no-warnings".into(),
                "C:\\x\\bin\\tool".into()
            ]),
            Some("C:\\x\\bin\\tool")
        );
        assert_eq!(
            node_entry_script(&["node".into(), "-r".into(), "preload".into(), "s.js".into()]),
            Some("s.js")
        );
    }

    #[test]
    fn wrapper_and_native_names_are_case_insensitive() {
        let registry = SignatureRegistry::standard();
        let codebuddy = registry.by_agent("codebuddy").unwrap();
        assert!(codebuddy.matches_wrapper_name("CodeBuddy"));
        assert!(codebuddy.matches_wrapper_name("cbc"));
        let codex = registry.by_agent("codex").unwrap();
        assert!(codex.matches_native_name("CODEX.EXE"));
        assert!(!codex.matches_native_name("codex-code-mode-host.exe"));
        assert!(codex.matches_node_entry(
            r"c:\users\me\appdata\roaming\npm\node_modules\@openai\codex\bin\codex.js"
        ));
        assert!(!codex.matches_node_entry(r"c:\tmp\codex.js"));
    }
}
