//! Restricted parsing of `cmd` / PowerShell shims.
//!
//! A global npm install puts three shims next to each other:
//! `tool.cmd`, `tool.ps1` and a bare `tool` shell script. None of them is the
//! product; they are thin launchers for `node <package entry>`.
//!
//! This module treats those files as **data**:
//! - the file is read, size-capped and never executed,
//! - variable expansion is limited to the shim's own directory (`%~dp0`,
//!   `%dp0%`, `$basedir`, `$PSScriptRoot`) and to the interpreter placeholders,
//! - the recovered entry script must live inside the shim's own directory,
//!   so a tampered shim cannot point at an arbitrary file,
//! - the recovered entry script must look like a Node entry (shebang or a
//!   `.js`/`.mjs`/`.cjs` suffix).
//!
//! Nothing here runs `cmd.exe`, PowerShell, `eval` or a shell.

use super::signature::{extension_lower, EntryKind};
use std::path::{Path, PathBuf};

const MAX_SHIM_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WrapperIssue {
    /// The shim points at a file that no longer exists.
    MissingTarget { reason: String },
    /// The shim shape is not one of the supported patterns.
    Unsupported { reason: String },
    /// The file could not be read as text.
    Unreadable { reason: String },
    /// A script was recovered but is not a Node entry.
    NotANodeEntry { reason: String },
    /// The recovered script sits outside the shim directory.
    EscapesShimDirectory { reason: String },
}

impl WrapperIssue {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingTarget { .. } => "missing-target",
            Self::Unsupported { .. } => "unsupported-wrapper",
            Self::Unreadable { .. } => "unreadable-wrapper",
            Self::NotANodeEntry { .. } => "not-node-entry",
            Self::EscapesShimDirectory { .. } => "unsafe-wrapper",
        }
    }

    /// Path-free reason, safe for IPC, logs and diagnostics.
    pub fn reason(&self) -> &str {
        match self {
            Self::MissingTarget { reason }
            | Self::Unsupported { reason }
            | Self::Unreadable { reason }
            | Self::NotANodeEntry { reason }
            | Self::EscapesShimDirectory { reason } => reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeInterpreter {
    Resolved(PathBuf),
    /// The shim says plain `node`, so the interpreter is resolved from `PATH`
    /// at probe time. No shell is involved.
    FromPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrapperParse {
    pub kind: EntryKind,
    pub shim: PathBuf,
    pub interpreter: NodeInterpreter,
    pub entry_script: PathBuf,
}

/// Parses a `.cmd`, `.bat` or `.ps1` shim without executing it.
pub fn parse_shim(shim: &Path) -> Result<WrapperParse, WrapperIssue> {
    let extension = extension_lower(shim);
    let kind = match extension.as_str() {
        "cmd" | "bat" => EntryKind::CmdWrapper,
        "ps1" => EntryKind::PowerShellWrapper,
        _ => {
            return Err(WrapperIssue::Unsupported {
                reason: "only cmd/bat/ps1 shims are parsed".to_string(),
            })
        }
    };
    let metadata = std::fs::metadata(shim).map_err(|error| WrapperIssue::Unreadable {
        reason: format!("shim metadata unavailable: {}", error.kind()),
    })?;
    if !metadata.is_file() {
        return Err(WrapperIssue::Unreadable {
            reason: "shim is not a regular file".to_string(),
        });
    }
    if metadata.len() > MAX_SHIM_BYTES {
        return Err(WrapperIssue::Unsupported {
            reason: "shim exceeds the 64 KiB data-parsing limit".to_string(),
        });
    }
    let bytes = std::fs::read(shim).map_err(|error| WrapperIssue::Unreadable {
        reason: format!("shim could not be read: {}", error.kind()),
    })?;
    let text = String::from_utf8_lossy(&bytes);
    let directory = shim.parent().unwrap_or_else(|| Path::new("."));
    let expanded = expand_shim_variables(&text, directory);
    let (interpreter_token, script_token) = find_node_pair(&expanded).ok_or_else(|| {
        WrapperIssue::Unsupported {
            reason: "no `node <entry>` command could be recovered from the shim".to_string(),
        }
    })?;

    let script_text = script_token.trim_matches('"').replace('/', "\\");
    let script = PathBuf::from(&script_text);
    if script_text.trim().is_empty() {
        return Err(WrapperIssue::Unsupported {
            reason: "recovered entry script is empty".to_string(),
        });
    }
    let script_canonical = match std::fs::canonicalize(&script) {
        Ok(path) => path,
        Err(_) => {
            return Err(WrapperIssue::MissingTarget {
                reason: "shim target does not exist".to_string(),
            })
        }
    };
    let directory_canonical = std::fs::canonicalize(directory).map_err(|error| {
        WrapperIssue::Unreadable {
            reason: format!("shim directory could not be resolved: {}", error.kind()),
        }
    })?;
    if !script_canonical.starts_with(&directory_canonical) {
        return Err(WrapperIssue::EscapesShimDirectory {
            reason: "shim target escapes the shim directory".to_string(),
        });
    }
    if !script_canonical.is_file() {
        return Err(WrapperIssue::MissingTarget {
            reason: "shim target is not a file".to_string(),
        });
    }
    if !looks_like_node_entry(&script_canonical) {
        return Err(WrapperIssue::NotANodeEntry {
            reason: "shim target is not a Node entry script".to_string(),
        });
    }

    let interpreter = match interpreter_token.as_deref() {
        Some(token) if !token.trim().is_empty() => {
            let candidate = PathBuf::from(token.trim_matches('"').replace('/', "\\"));
            match std::fs::canonicalize(&candidate) {
                Ok(path) if path.starts_with(&directory_canonical) => {
                    NodeInterpreter::Resolved(path)
                }
                _ => NodeInterpreter::FromPath,
            }
        }
        _ => NodeInterpreter::FromPath,
    };

    Ok(WrapperParse {
        kind,
        shim: shim.to_path_buf(),
        interpreter,
        entry_script: script_canonical,
    })
}

/// Expands only the shim-local variables. Every other `%VAR%`/`$VAR` stays
/// literal and is therefore never resolved from the environment.
pub fn expand_shim_variables(text: &str, directory: &Path) -> String {
    let with_separator = format!("{}\\", directory.to_string_lossy().trim_end_matches('\\'));
    let without_separator = directory.to_string_lossy().trim_end_matches('\\').to_string();
    let mut expanded = text.replace("%~dp0", &with_separator);
    expanded = expanded.replace("%dp0%", &with_separator);
    expanded = expanded.replace("$PSScriptRoot", &without_separator);
    expanded = expanded.replace("$basedir", &without_separator);
    expanded = expanded.replace("$exe", "");
    expanded = expanded.replace("%_prog%", "node");
    expanded
}

/// Finds `node <entry>`. Separators (`&`, `|`, parentheses, newlines) split the
/// shim into simple statements; the last matching statement wins because that
/// is the one npm actually runs.
fn find_node_pair(text: &str) -> Option<(Option<String>, String)> {
    let tokens = tokenize(text);
    let mut found: Option<(Option<String>, String)> = None;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index].trim_matches('"');
        if is_node_program(token) {
            let mut lookahead = index + 1;
            while lookahead < tokens.len() {
                let candidate = tokens[lookahead].trim_matches('"');
                if candidate.is_empty() {
                    lookahead += 1;
                    continue;
                }
                if candidate.starts_with('-') || is_placeholder(candidate) {
                    lookahead += 1;
                    continue;
                }
                if candidate.starts_with('%') || candidate.starts_with('$') {
                    lookahead += 1;
                    continue;
                }
                if looks_like_script_token(candidate) {
                    found = Some((Some(tokens[index].clone()), tokens[lookahead].clone()));
                }
                break;
            }
        }
        index += 1;
    }
    found
}

fn is_node_program(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let trimmed = lower.trim_matches('"');
    trimmed == "node" || trimmed == "node.exe" || trimmed.ends_with("\\node.exe") || trimmed.ends_with("/node")
}

fn is_placeholder(token: &str) -> bool {
    matches!(
        token,
        "%*" | "$args" | "$input" | "@args" | "%1" | "%~1" | "(" | ")" | "&" | "|"
    )
}

fn looks_like_script_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.ends_with(".exe") || lower.ends_with(".cmd") || lower.ends_with(".ps1") {
        return false;
    }
    token.contains('\\')
        || token.contains('/')
        || lower.ends_with(".js")
        || lower.ends_with(".mjs")
        || lower.ends_with(".cjs")
}

/// Splits shim text into tokens, treating shell separators as boundaries.
fn tokenize(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    for character in text.chars() {
        match quote {
            Some(active) if character == active => {
                current.push(character);
                quote = None;
            }
            Some(_) => current.push(character),
            None if character == '"' || character == '\'' => {
                current.push(character);
                quote = Some(character);
            }
            None if character.is_whitespace()
                || character == '&'
                || character == '|'
                || character == '('
                || character == ')'
                || character == '\n'
                || character == '\r' =>
            {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn looks_like_node_entry(script: &Path) -> bool {
    let extension = extension_lower(script);
    if matches!(extension.as_str(), "js" | "mjs" | "cjs" | "ts" | "mts" | "cts") {
        return true;
    }
    // Extension-less npm bin entries carry a `#!/usr/bin/env node` shebang.
    let Ok(bytes) = std::fs::read(script) else {
        return false;
    };
    let head = &bytes[..bytes.len().min(256)];
    let text = String::from_utf8_lossy(head);
    text.starts_with("#!") && text.lines().next().unwrap_or_default().contains("node")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "multicli-wrapper-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn real_npm_shaped_cmd_shim_resolves_without_executing() {
        let root = temp_dir("cmd");
        let package = root.join("node_modules/@tencent-ai/codebuddy-code/bin");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("codebuddy"), "#!/usr/bin/env node\nconsole.log(1);\n").unwrap();
        fs::write(
            root.join("codebuddy.cmd"),
            r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0
IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
)
endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@tencent-ai\codebuddy-code\bin\codebuddy" %*
"#,
        )
        .unwrap();

        let parsed = parse_shim(&root.join("codebuddy.cmd")).expect("shim parses");
        assert_eq!(parsed.kind, EntryKind::CmdWrapper);
        assert_eq!(parsed.interpreter, NodeInterpreter::FromPath);
        assert_eq!(
            parsed.entry_script,
            fs::canonicalize(package.join("codebuddy")).unwrap()
        );
    }

    #[test]
    fn real_npm_shaped_ps1_shim_resolves() {
        let root = temp_dir("ps1");
        let package = root.join("node_modules/@tencent-ai/codebuddy-code/bin");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("codebuddy"), "#!/usr/bin/env node\n").unwrap();
        fs::write(
            root.join("codebuddy.ps1"),
            r#"#!/usr/bin/env pwsh
$basedir=Split-Path $MyInvocation.MyCommand.Definition -Parent
$exe=""
if ($PSVersionTable.PSVersion -lt "6.0" -or $IsWindows) {
  $exe=".exe"
}
& "$basedir/node$exe"  "$basedir/node_modules/@tencent-ai/codebuddy-code/bin/codebuddy" $args
"#,
        )
        .unwrap();
        let parsed = parse_shim(&root.join("codebuddy.ps1")).expect("ps1 parses");
        assert_eq!(parsed.kind, EntryKind::PowerShellWrapper);
        assert!(parsed.entry_script.ends_with("codebuddy"));
    }

    #[test]
    fn missing_target_and_escaping_shims_are_reported_not_executed() {
        let root = temp_dir("broken");
        fs::write(
            root.join("codex.cmd"),
            r#"@echo off
"%dp0%\node.exe"  "%dp0%\node_modules\@openai\codex\bin\codex.js" %*
"#,
        )
        .unwrap();
        let error = parse_shim(&root.join("codex.cmd")).unwrap_err();
        assert_eq!(error.code(), "missing-target");

        let outside = temp_dir("outside");
        fs::write(outside.join("real.js"), "console.log(1)").unwrap();
        let escaped = root.join("escape.cmd");
        fs::write(
            &escaped,
            format!(
                r#"@echo off
node "{}" %*
"#,
                outside.join("real.js").display()
            ),
        )
        .unwrap();
        let error = parse_shim(&escaped).unwrap_err();
        assert_eq!(error.code(), "unsafe-wrapper");
    }

    #[test]
    fn non_node_targets_and_oversized_shims_are_rejected() {
        let root = temp_dir("non-node");
        fs::create_dir_all(root.join("payload")).unwrap();
        fs::write(root.join("payload/data.txt"), "not a script").unwrap();
        fs::write(
            root.join("tool.cmd"),
            r#"@echo off
node "%dp0%\payload\data.txt" %*
"#,
        )
        .unwrap();
        let error = parse_shim(&root.join("tool.cmd")).unwrap_err();
        assert_eq!(error.code(), "not-node-entry");

        fs::write(root.join("big.ps1"), "x".repeat(70 * 1024)).unwrap();
        let error = parse_shim(&root.join("big.ps1")).unwrap_err();
        assert_eq!(error.code(), "unsupported-wrapper");
    }

    #[test]
    fn environment_variables_are_never_expanded() {
        let expanded = expand_shim_variables(
            r#""%USERPROFILE%\secret" "%TOKEN%" "$env:API_KEY""#,
            Path::new(r"C:\npm"),
        );
        assert!(expanded.contains("%USERPROFILE%"));
        assert!(expanded.contains("%TOKEN%"));
        assert!(expanded.contains("$env:API_KEY"));
    }
}
