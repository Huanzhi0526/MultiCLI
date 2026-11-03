//! User-defined CLIs: `executable + argv[] + cwd`, never a shell string.
//!
//! A custom CLI is a *preset*, not a new launch mechanism. When it is started
//! the preset argv is passed through the same S04 chain as any discovered
//! installation: MSVC quoting for `.exe`, the escaped batch trampoline for
//! `.cmd`, single-quoted literals for `.ps1`. That is also why this module
//! refuses a few shapes before anything is stored:
//!
//! - a relative, missing or directory target (there would be nothing to pin);
//! - a shell or script host (`cmd.exe`, `powershell.exe`, `pwsh.exe`,
//!   `wscript.exe`, `mshta.exe`, `rundll32.exe`, `bash.exe`, `wsl.exe`, …).
//!   Such an executable would turn the argument array back into a command
//!   line, which is exactly what the S04 contract forbids;
//! - `NUL` anywhere, a line break or `"\` in an argument of a `.cmd` target
//!   (the batch parser cannot represent them; S04 refuses them at launch too,
//!   this refuses them at creation so the preset is never half-valid).
//!
//! History and quota stay `unsupported`: a user-defined command carries no
//! product identity, so no history format and no usage endpoint can be claimed.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::multicli::domain::HostPath;
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::path::{canonical_text, normalize_display_path};
use crate::multicli::terminal::model::LaunchChain;

pub const MAX_ARGS: usize = 64;
pub const MAX_ARG_CHARS: usize = 4_096;
pub const MAX_LABEL_CHARS: usize = 120;

/// Executables that interpret their arguments as commands or scripts. Allowing
/// one would defeat the "argv, not shell" contract.
const FORBIDDEN_EXECUTABLES: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "wscript.exe",
    "cscript.exe",
    "mshta.exe",
    "rundll32.exe",
    "regsvr32.exe",
    "bash.exe",
    "wsl.exe",
    "sh.exe",
    "conhost.exe",
    "wt.exe",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomCliInput {
    pub label: String,
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub product_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomCliView {
    pub id: String,
    pub label: String,
    pub executable: HostPath,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub product_hint: Option<String>,
    pub launch_chain: LaunchChain,
    /// `unsupported` for both, by design: no product identity exists.
    pub history_access: String,
    pub quota_access: String,
    /// How this entry came to exist. There is no remote import path.
    pub verification: String,
    pub created_at: String,
}

/// Validated, pinned form of a custom CLI. The executable is pinned by its
/// canonical path; the launch plan records its fingerprint in the terminal log,
/// so a replaced binary is visible there rather than silently trusted here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCustomCli {
    pub id: String,
    pub label: String,
    pub executable_display: String,
    pub executable_canonical: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub product_hint: Option<String>,
    pub chain: LaunchChain,
}

pub fn validate_input(input: &CustomCliInput) -> Result<ResolvedCustomCli, AppError> {
    let label = input.label.trim();
    if label.is_empty() {
        return Err(AppError::invalid_argument("label", "must not be empty"));
    }
    if label.chars().count() > MAX_LABEL_CHARS {
        return Err(AppError::invalid_argument(
            "label",
            format!("must be at most {MAX_LABEL_CHARS} characters"),
        ));
    }
    if input.executable.trim().is_empty() {
        return Err(AppError::invalid_argument(
            "executable",
            "must not be empty",
        ));
    }
    let path = PathBuf::from(input.executable.trim());
    if !path.is_absolute() {
        return Err(AppError::invalid_argument(
            "executable",
            "must be an absolute path to an existing file",
        ));
    }
    if path.is_dir() {
        return Err(AppError::invalid_argument(
            "executable",
            "is a directory; a custom CLI must be a file",
        ));
    }
    if !path.is_file() {
        return Err(AppError::invalid_argument(
            "executable",
            "does not exist or is not a readable file",
        ));
    }
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if FORBIDDEN_EXECUTABLES.contains(&file_name.as_str()) {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "a shell or script host cannot be registered as a custom CLI",
        )
        .with_details(serde_json::json!({
            "stage": "customCliExecutable",
            "reason": "its arguments would become a command line",
            "fileName": file_name,
        })));
    }
    let chain = LaunchChain::for_extension(&path).ok_or_else(|| {
        AppError::new(
            ErrorCode::Unsupported,
            "this executable extension has no implemented launch chain",
        )
        .with_details(serde_json::json!({
            "stage": "customCliExecutable",
            "extension": path.extension().map(|value| value.to_string_lossy().to_string()),
        }))
    })?;
    if input.args.len() > MAX_ARGS {
        return Err(AppError::invalid_argument(
            "args",
            format!("at most {MAX_ARGS} preset arguments are accepted"),
        ));
    }
    for (index, arg) in input.args.iter().enumerate() {
        if arg.contains('\0') {
            return Err(AppError::invalid_argument(
                &format!("args[{index}]"),
                "must not contain NUL",
            ));
        }
        if arg.chars().count() > MAX_ARG_CHARS {
            return Err(AppError::invalid_argument(
                &format!("args[{index}]"),
                format!("must be at most {MAX_ARG_CHARS} characters"),
            ));
        }
        if chain == LaunchChain::Cmd {
            if arg.contains('\r') || arg.contains('\n') {
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "a .cmd target cannot carry an argument containing a line break",
                )
                .with_details(serde_json::json!({
                    "stage": "customCliArgument",
                    "index": index,
                })));
            }
            if arg.contains("\"\\") {
                return Err(AppError::new(
                    ErrorCode::Unsupported,
                    "a .cmd target cannot carry a quote directly followed by a backslash",
                )
                .with_details(serde_json::json!({
                    "stage": "customCliArgument",
                    "index": index,
                })));
            }
        }
    }
    let cwd = match input.cwd.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => {
            let directory = PathBuf::from(value);
            if !directory.is_absolute() {
                return Err(AppError::invalid_argument("cwd", "must be an absolute path"));
            }
            if !directory.is_dir() {
                return Err(AppError::invalid_argument(
                    "cwd",
                    "must be an existing directory",
                ));
            }
            Some(canonical_text(&directory))
        }
        None => None,
    };
    let product_hint = input
        .product_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value.chars().count() > 64 {
                value.chars().take(64).collect::<String>()
            } else {
                value.to_string()
            }
        });
    let canonical = std::fs::canonicalize(&path).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("failed to resolve the executable: {error}"),
        )
        .with_details(serde_json::json!({ "stage": "customCliExecutable" }))
    })?;
    let display = normalize_display_path(&path.to_string_lossy())
        .unwrap_or_else(|_| path.to_string_lossy().to_string());
    Ok(ResolvedCustomCli {
        id: format!(
            "custom-{}",
            crate::multicli::history::ids::sha256_hex(
                format!(
                    "{}\u{1f}{}\u{1f}{}",
                    canonical_text(&canonical),
                    input.args.join("\u{1f}"),
                    cwd.clone().unwrap_or_default()
                )
                .as_bytes()
            )
            .chars()
            .take(24)
            .collect::<String>()
        ),
        label: label.to_string(),
        executable_display: display,
        executable_canonical: canonical_text(&canonical),
        args: input.args.clone(),
        cwd,
        product_hint,
        chain,
    })
}

pub fn view(
    row: &super::usage::repo::CustomCliRow,
) -> Result<CustomCliView, AppError> {
    let chain = LaunchChain::for_extension(Path::new(&row.executable_canonical_path)).ok_or_else(|| {
        AppError::new(
            ErrorCode::Unsupported,
            "a stored custom CLI has an extension that is no longer supported",
        )
        .with_details(serde_json::json!({ "stage": "customCliView", "id": row.id }))
    })?;
    Ok(CustomCliView {
        id: row.id.clone(),
        label: row.label.clone(),
        executable: HostPath {
            host_id: row.host_id.clone(),
            display_path: row.executable_display_path.clone(),
            canonical_path: Some(row.executable_canonical_path.clone()),
        },
        args: row.args.clone(),
        cwd: row.cwd.clone(),
        product_hint: row.product_hint.clone(),
        launch_chain: chain,
        history_access: "unsupported".to_string(),
        quota_access: "unsupported".to_string(),
        verification: "user-local-import".to_string(),
        created_at: row.created_at.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_exe(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "multicli-custom-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join(name);
        std::fs::write(&path, b"MZ").unwrap();
        path
    }

    fn input(executable: &Path) -> CustomCliInput {
        CustomCliInput {
            label: "我的 CLI".into(),
            executable: executable.display().to_string(),
            args: Vec::new(),
            cwd: None,
            product_hint: None,
        }
    }

    #[test]
    fn a_shell_host_is_refused_and_never_stored() {
        let cmd = PathBuf::from(
            std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
                .join("System32\\cmd.exe"),
        );
        if !cmd.is_file() {
            return;
        }
        let error = validate_input(&input(&cmd)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(
            error.details.get("stage").and_then(|value| value.as_str()),
            Some("customCliExecutable")
        );
    }

    #[test]
    fn a_missing_or_relative_target_is_refused() {
        let missing = std::env::temp_dir().join("multicli-custom-missing.exe");
        assert!(validate_input(&input(&missing)).is_err());
        let relative = CustomCliInput {
            executable: "bin\\tool.exe".into(),
            ..input(&missing)
        };
        assert_eq!(
            validate_input(&relative).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn chinese_quotes_and_special_arguments_are_preserved_verbatim() {
        let exe = temp_exe("tool.exe");
        let input = CustomCliInput {
            label: "中文 标签".into(),
            executable: exe.display().to_string(),
            args: vec![
                "中文 参数".into(),
                "say \"hi\"".into(),
                "a&b|c<d>e".into(),
                "50%".into(),
            ],
            cwd: None,
            product_hint: Some("自定义".into()),
        };
        let resolved = validate_input(&input).unwrap();
        assert_eq!(resolved.args, input.args);
        assert_eq!(resolved.chain, LaunchChain::Exe);
        assert!(resolved.label.contains("中文"));
        let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    }

    #[test]
    fn a_cmd_preset_with_a_line_break_is_refused_early() {
        let exe = temp_exe("tool.cmd");
        let mut preset = input(&exe);
        preset.args = vec!["a\nb".into()];
        let error = validate_input(&preset).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(
            error.details.get("stage").and_then(|value| value.as_str()),
            Some("customCliArgument")
        );
        let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    }

    #[test]
    fn an_unsupported_extension_has_no_launch_chain() {
        let exe = temp_exe("tool.lnk");
        let error = validate_input(&input(&exe)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    }
}
