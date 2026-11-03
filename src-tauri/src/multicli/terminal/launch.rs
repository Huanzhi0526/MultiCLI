//! Target resolution and the three verified Windows launch chains.
//!
//! Nothing here concatenates a user-supplied command line. A terminal is always
//! described as `executable + argv[] + cwd + env delta`, and the two script
//! chains go through a generated trampoline that holds the escaped tokens:
//!
//! | chain | process actually spawned | where user text goes |
//! |---|---|---|
//! | `.exe` | the image itself | `argv[]`, quoted by MSVC rules |
//! | `.cmd`/`.bat` | `cmd.exe /d /s /v:off /c %MULTICLI_CMD_FILE%` | one quoted token per argument inside the trampoline |
//! | `.ps1` | `pwsh.exe (7.3+) ... -File <trampoline.ps1>` | one single-quoted PowerShell literal per argument inside the trampoline |
//!
//! The cmd path is a quoted private environment value, expanded once, so Unicode,
//! spaces and literal percent characters stay inside the selected profile.
//! The trampoline approach exists because `cmd.exe` re-parses its own raw
//! command line and `CommandBuilder` quotes with MSVC rules, so a `cmd /c`
//! payload cannot be passed safely as a single argv element. The trampoline is
//! also what makes the exact command line auditable: it is a plain file under
//! the application's own profile.
//!
//! Batch tokens are written with `"` doubled to `""`, not `\"`. A `.cmd` target
//! is normally a shim that forwards `%*` to a native program, so every argument
//! is parsed twice: once by the trampoline and once by the shim. `""` survives
//! both parses (each toggles the quote state twice and the final CRT collapses
//! the pair back to one `"`), while `\"` closes the token at the shim's parse
//! and lets an `&` inside the value become a command. Measured on Windows 11
//! against a real `cmd.exe`, a forwarding shim and both `echo.exe` and
//! `node.exe` (see `tests` and the S04 delivery §8):
//!
//! - `x"& echo PWNED` arrives as one argv element and `PWNED` is never
//!   executed; the older `\"` form *did* execute it through the shim;
//! - `50%PATH%` (doubled), `bang!PATH!bang` (delayed expansion is disabled, so
//!   the value cannot leak the environment), `caret^v`, `(x)`, `a&b|c<d>e`,
//!   trailing backslashes and `中文 😀` all arrive byte-for-byte;
//! - two shapes cannot be represented and are refused before anything is
//!   created: a line break (`cmd` ends the batch line there) and a `"` directly
//!   followed by `\` (the pair re-splits at the shim's parse);
//! - the batch file is read with the console code page, so it starts with two
//!   ASCII lines and then `chcp 65001` before any non-ASCII text;
//! - `-File` strips quotes from its arguments, so the `.ps1` chain never puts
//!   user text on a command line at all.

use std::path::{Path, PathBuf};

use crate::multicli::domain::HostPath;
use crate::multicli::error::{AppError, ErrorCode};

use super::model::{
    executable_fingerprint, host_path, EnvVar, LaunchChain, ShellFlavor, TerminalTargetKind,
};

/// A shell offered by the target picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellCandidate {
    pub flavor: ShellFlavor,
    pub label: String,
    pub path: PathBuf,
    pub exists: bool,
}

fn system32() -> Option<PathBuf> {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .map(|root| root.join("System32"))
}

/// The shells this host can actually start. Order is stable so the UI list does
/// not reshuffle between scans.
pub fn shell_candidates() -> Vec<ShellCandidate> {
    let mut items = Vec::new();
    if let Some(base) = system32() {
        let cmd = base.join("cmd.exe");
        items.push(ShellCandidate {
            flavor: ShellFlavor::Cmd,
            label: "cmd.exe".into(),
            exists: cmd.is_file(),
            path: cmd,
        });
        let powershell = base
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        items.push(ShellCandidate {
            flavor: ShellFlavor::WindowsPowershell,
            label: "Windows PowerShell".into(),
            exists: powershell.is_file(),
            path: powershell,
        });
    }
    if let Ok(pwsh) = which::which("pwsh") {
        items.push(ShellCandidate {
            flavor: ShellFlavor::Pwsh,
            label: "PowerShell 7 (pwsh)".into(),
            exists: pwsh.is_file(),
            path: pwsh,
        });
    }
    items
}

pub fn shell_path(flavor: ShellFlavor) -> Result<PathBuf, AppError> {
    shell_candidates()
        .into_iter()
        .find(|item| item.flavor == flavor && item.exists)
        .map(|item| item.path)
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::Unsupported,
                "this shell is not installed on the host",
            )
            .with_details(serde_json::json!({
                "stage": "resolveShell",
                "shell": flavor.as_str(),
            }))
        })
}

/// Fixed launcher arguments for an interactive shell target. No user text is
/// ever part of these strings.
pub fn shell_launch_args(flavor: ShellFlavor) -> Vec<String> {
    match flavor {
        ShellFlavor::Cmd => vec![
            "/d".into(),
            "/q".into(),
            "/k".into(),
            "chcp 65001>nul".into(),
        ],
        ShellFlavor::WindowsPowershell | ShellFlavor::Pwsh => {
            vec!["-NoLogo".into(), "-NoProfile".into()]
        }
    }
}

/// A target pinned at creation time: the executable path, its chain and its
/// identity are resolved once and never re-resolved for this terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedTarget {
    pub kind: TerminalTargetKind,
    pub label: String,
    pub product: Option<String>,
    pub agent_id: String,
    pub installation_id: Option<String>,
    pub executable: PathBuf,
    pub chain: LaunchChain,
    /// Set only for a plain shell target; the shell is started interactively
    /// with a fixed launcher argv instead of the target's own argv.
    pub shell_flavor: Option<ShellFlavor>,
    pub version: Option<String>,
    /// A custom CLI's preset argv. It is placed *before* the user's arguments
    /// and goes through the same escaping as any other token, so a preset can
    /// never change the command structure.
    pub preset_args: Vec<String>,
}

impl PinnedTarget {
    pub fn host_path(&self, host_id: &str) -> HostPath {
        host_path(host_id, &self.executable)
    }

    /// The exact argv this target runs: preset first, then the user's tokens.
    pub fn effective_args(&self, user_args: &[String]) -> Vec<String> {
        let mut args = self.preset_args.clone();
        args.extend(user_args.iter().cloned());
        args
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    pub target: PinnedTarget,
    /// What portable-pty spawns.
    pub program: PathBuf,
    /// Fixed launcher arguments; user arguments are already inside the
    /// trampoline for the script chains.
    pub program_args: Vec<String>,
    /// The complete argv, for the view and the delivery report.
    pub argv_preview: Vec<String>,
    /// Generated file that must be removed when the terminal closes.
    pub trampoline: Option<PathBuf>,
    pub fingerprint: String,
    /// Private launcher values applied after the user environment delta.
    pub launcher_env: Vec<(String, String)>,
    pub notes: Vec<String>,
}

/// Directory that holds generated trampolines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPaths {
    pub root: PathBuf,
    pub note: Option<String>,
}

impl LaunchPaths {
    /// Generated scripts stay in the selected profile, including when its path
    /// contains spaces, Unicode or cmd metacharacters. The cmd launcher uses a
    /// fixed environment reference rather than MSVC-quoting the path as argv.
    pub fn resolve(data_root: &Path) -> Result<Self, AppError> {
        let root = data_root.join("terminal").join("launch");
        std::fs::create_dir_all(&root).map_err(|error| {
            AppError::new(ErrorCode::Io, format!("cannot create profile launch directory: {error}"))
                .with_details(serde_json::json!({"stage":"launchPaths"}))
        })?;
        Ok(Self { root, note: None })
    }

    pub fn session_dir(&self, terminal_id: &str) -> PathBuf {
        self.root.join(terminal_id)
    }
}

/// Arguments a batch trampoline cannot represent at all.
///
/// Everything else — quotes, `&`, `%`, `!`, `^`, parentheses, Unicode, trailing
/// backslashes — is escaped and measured to round-trip. Two shapes are not
/// representable on a batch line, and both are refused before anything is
/// created instead of being silently rewritten:
///
/// - a line break, which ends the command (`cmd` has no way to quote it);
/// - `"` immediately followed by `\`, where the doubled quote and the
///   backslash run re-split at the shim's second parse.
pub fn validate_batch_args(args: &[String]) -> Result<(), AppError> {
    for (index, arg) in args.iter().enumerate() {
        if arg.contains('\r') || arg.contains('\n') {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "a .cmd target cannot carry an argument containing a line break",
            )
            .with_details(serde_json::json!({
                "stage": "batchArgument",
                "index": index,
                "reason": "cmd.exe ends the batch line at a line break",
            })));
        }
        if arg.contains("\"\\") {
            return Err(AppError::new(
                ErrorCode::Unsupported,
                "a .cmd target cannot carry an argument with a quote directly followed by a backslash",
            )
            .with_details(serde_json::json!({
                "stage": "batchArgument",
                "index": index,
                "reason": "the doubled quote and the backslash run re-split when a shim forwards %*",
            })));
        }
    }
    Ok(())
}

/// Quotes one token for a batch file line.
///
/// Measured rules: wrap in double quotes; write `"` as `""` (survives the
/// shim's second parse, unlike `\"`); double `%` so the batch parser cannot
/// expand it; double a backslash run that precedes the closing quote so it
/// cannot escape that quote. Delayed expansion stays off, so `!` is literal.
pub fn escape_batch_token(value: &str) -> String {
    let mut body = String::with_capacity(value.len() + 4);
    let mut backslashes = 0usize;
    for ch in value.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                body.extend(std::iter::repeat_n('\\', backslashes * 2));
                body.push_str("\"\"");
                backslashes = 0;
            }
            '\r' | '\n' => {
                // Refused by `validate_batch_args`; a space keeps a stray call
                // from turning into a broken line.
                body.extend(std::iter::repeat_n('\\', backslashes));
                body.push(' ');
                backslashes = 0;
            }
            '%' => {
                body.extend(std::iter::repeat_n('\\', backslashes));
                body.push_str("%%");
                backslashes = 0;
            }
            other => {
                body.extend(std::iter::repeat_n('\\', backslashes));
                body.push(other);
                backslashes = 0;
            }
        }
    }
    body.extend(std::iter::repeat_n('\\', backslashes * 2));
    body.push('"');
    body.insert(0, '"');
    body
}

pub fn batch_trampoline(script: &Path, args: &[String]) -> String {
    let mut body = String::new();
    // The first two lines must stay ASCII: the file is UTF-8 and `cmd` reads a
    // batch file with the console code page until `chcp` switches it.
    body.push_str("@echo off\r\n");
    body.push_str("chcp 65001>nul\r\n");
    body.push_str("setlocal DisableDelayedExpansion\r\n");
    body.push_str(&escape_batch_token(&script.display().to_string()));
    for arg in args {
        body.push(' ');
        body.push_str(&escape_batch_token(arg));
    }
    body.push_str("\r\n");
    body
}

/// PowerShell single-quoted literal: only `'` needs doubling, and a newline is
/// legal inside the literal.
pub fn powershell_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for ch in value.chars() {
        if ch == '\'' {
            out.push_str("''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

pub fn powershell_trampoline(script: &Path, args: &[String]) -> String {
    let mut body = String::new();
    body.push_str("$ErrorActionPreference = 'Stop'\r\n");
    body.push_str("if ($PSVersionTable.PSVersion -lt [Version]'7.3') { throw 'MultiCLI .ps1 requires PowerShell 7.3+ for literal native argv' }\r\n");
    body.push_str("$PSNativeCommandArgumentPassing = 'Standard'\r\n");
    body.push_str("& ");
    body.push_str(&powershell_literal(&script.display().to_string()));
    for arg in args {
        body.push(' ');
        body.push_str(&powershell_literal(arg));
    }
    body.push_str("\r\n");
    body.push_str("exit $LASTEXITCODE\r\n");
    body
}

fn write_trampoline(path: &Path, contents: &str, bom: bool) -> Result<(), AppError> {
    let mut bytes: Vec<u8> = Vec::with_capacity(contents.len() + 3);
    if bom {
        bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
    }
    bytes.extend_from_slice(contents.as_bytes());
    std::fs::write(path, bytes).map_err(|error| {
        AppError::new(
            ErrorCode::Io,
            format!("failed to write the launch trampoline: {error}"),
        )
        .with_details(serde_json::json!({ "stage": "writeTrampoline" }))
        .with_retryable(true)
    })
}

/// Builds the concrete launch plan for one pinned target.
pub fn plan_launch(
    target: &PinnedTarget,
    args: &[String],
    terminal_id: &str,
    paths: &LaunchPaths,
) -> Result<LaunchPlan, AppError> {
    if !target.executable.is_file() {
        return Err(AppError::new(
            ErrorCode::Unsupported,
            "the pinned executable no longer exists",
        )
        .with_details(serde_json::json!({
            "stage": "planLaunch",
            "chain": target.chain.as_str(),
        })));
    }
    let fingerprint = executable_fingerprint(&target.executable)?;
    let mut notes = Vec::new();
    if let Some(flavor) = target.shell_flavor {
        // An interactive shell never receives user arguments: its argv is a
        // fixed launcher form, so no user text can become a shell command.
        if !args.is_empty() {
            return Err(AppError::invalid_argument(
                "args",
                "an interactive shell target does not accept arguments",
            ));
        }
        let program_args = shell_launch_args(flavor);
        notes.push("shell 目标：固定 launcher argv，不接受用户参数。".into());
        return Ok(LaunchPlan {
            target: target.clone(),
            program: target.executable.clone(),
            argv_preview: std::iter::once(target.executable.display().to_string())
                .chain(program_args.iter().cloned())
                .collect(),
            program_args,
            trampoline: None,
            fingerprint,
            launcher_env: Vec::new(),
            notes,
        });
    }
    match target.chain {
        LaunchChain::Exe => Ok(LaunchPlan {
            target: target.clone(),
            program: target.executable.clone(),
            program_args: args.to_vec(),
            argv_preview: std::iter::once(target.executable.display().to_string())
                .chain(args.iter().cloned())
                .collect(),
            trampoline: None,
            fingerprint,
            launcher_env: Vec::new(),
            notes,
        }),
        LaunchChain::Cmd => {
            validate_batch_args(args)?;
            let dir = paths.session_dir(terminal_id);
            std::fs::create_dir_all(&dir).map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to create the launch directory: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "createLaunchDir" }))
            })?;
            let file = dir.join("run.cmd");
            write_trampoline(
                &file,
                &batch_trampoline(&target.executable, args),
                false,
            )?;
            let cmd = shell_path(ShellFlavor::Cmd)?;
            if let Some(note) = &paths.note {
                notes.push(note.clone());
            }
            notes.push(
                "cmd 链：参数以 `\"` 双写、`%` 双写写入自有 trampoline，未拼接到用户可编辑的 shell 行。"
                    .into(),
            );
            Ok(LaunchPlan {
                target: target.clone(),
                program: cmd.clone(),
                program_args: vec![
                    "/d".into(),
                    "/s".into(),
                    "/v:off".into(),
                    "/c".into(),
                    "%MULTICLI_CMD_FILE%".into(),
                ],
                argv_preview: std::iter::once(cmd.display().to_string())
                    .chain(["/d", "/s", "/c"].iter().map(|value| value.to_string()))
                    .chain(std::iter::once(file.display().to_string()))
                    .chain(args.iter().cloned())
                    .collect(),
                trampoline: Some(file.clone()),
                fingerprint,
                launcher_env: vec![("MULTICLI_CMD_FILE".into(), format!("\"{}\"", file.display()))],
                notes,
            })
        }
        LaunchChain::Powershell => {
            let dir = paths.session_dir(terminal_id);
            std::fs::create_dir_all(&dir).map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to create the launch directory: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "createLaunchDir" }))
            })?;
            let file = dir.join("run.ps1");
            // PowerShell 5.1 reads a .ps1 as ANSI unless it has a UTF-8 BOM.
            write_trampoline(&file, &powershell_trampoline(&target.executable, args), true)?;
            let powershell = shell_path(ShellFlavor::Pwsh).map_err(|_| {
                AppError::new(ErrorCode::Unsupported, "the verified .ps1 chain requires PowerShell 7.3+ (pwsh); use an .exe/.cmd installation when unavailable")
                    .with_details(serde_json::json!({"stage":"resolvePs1Runtime"}))
            })?;
            notes.push(
                "ps1 链：参数以 PowerShell 单引号字面量写入 trampoline，-File 不把用户文本交给命令解析器。"
                    .into(),
            );
            Ok(LaunchPlan {
                target: target.clone(),
                program: powershell.clone(),
                program_args: vec![
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    file.display().to_string(),
                ],
                argv_preview: vec![
                    powershell.display().to_string(),
                    "-NoLogo".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    file.display().to_string(),
                ],
                trampoline: Some(file),
                fingerprint,
                launcher_env: Vec::new(),
                notes,
            })
        }
    }
}

/// Removes a generated trampoline directory. Best effort: a leftover file is
/// inert and is reported by the caller instead of failing a close.
pub fn cleanup_trampoline(paths: &LaunchPaths, terminal_id: &str) -> Result<(), String> {
    let dir = paths.session_dir(terminal_id);
    if !dir.exists() {
        return Ok(());
    }
    std::fs::remove_dir_all(&dir).map_err(|error| format!("remove {}: {error}", dir.display()))
}

impl PinnedTarget {
    pub fn env_pairs(env: &[EnvVar]) -> Vec<(String, String)> {
        env.iter()
            .map(|item| (item.name.clone(), item.value.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_tokens_match_the_measured_rules() {
        assert_eq!(escape_batch_token("plain"), "\"plain\"");
        assert_eq!(escape_batch_token("a b"), "\"a b\"");
        assert_eq!(escape_batch_token("c&d"), "\"c&d\"");
        assert_eq!(escape_batch_token("50%"), "\"50%%\"");
        assert_eq!(escape_batch_token("%PATH%"), "\"%%PATH%%\"");
        // `""`, not `\"`: the pair survives the shim's second parse.
        assert_eq!(escape_batch_token("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_batch_token("C:\\dir\\"), "\"C:\\dir\\\\\"");
        assert_eq!(escape_batch_token("a\\\"b"), "\"a\\\\\"\"b\"");
        assert_eq!(escape_batch_token("bang!PATH!"), "\"bang!PATH!\"");
        assert_eq!(escape_batch_token("a\r\nb"), "\"a  b\"");
        assert_eq!(escape_batch_token("中文 😀"), "\"中文 😀\"");
    }

    #[test]
    fn powershell_literals_only_double_single_quotes() {
        assert_eq!(powershell_literal("plain"), "'plain'");
        assert_eq!(powershell_literal("it's"), "'it''s'");
        assert_eq!(powershell_literal("a\"b"), "'a\"b'");
        assert_eq!(powershell_literal("中文 & %"), "'中文 & %'");
    }

    #[test]
    fn batch_trampoline_starts_with_ascii_lines_then_code_page_switch() {
        let body = batch_trampoline(
            Path::new(r"C:\Program Files\x\a&b.cmd"),
            &["a b".into(), "say \"hi\"".into()],
        );
        let mut lines = body.split("\r\n");
        assert_eq!(lines.next(), Some("@echo off"));
        assert_eq!(lines.next(), Some("chcp 65001>nul"));
        assert_eq!(lines.next(), Some("setlocal DisableDelayedExpansion"));
        let command = lines.next().unwrap();
        assert!(command.starts_with("\"C:\\Program Files\\x\\a&b.cmd\""));
        assert!(command.contains("\"a b\""));
        assert!(command.contains("\"say \"\"hi\"\"\""));
        assert!(body.ends_with("\r\n"));
    }

    #[test]
    fn a_batch_argument_that_cannot_be_represented_is_refused_not_rewritten() {
        assert!(validate_batch_args(&["a\"b".into(), "c&d".into(), "e\\f".into()]).is_ok());
        for value in ["a\nb", "a\rb", "a\"\\b"] {
            let error = validate_batch_args(&[value.to_string()]).unwrap_err();
            assert_eq!(error.code, ErrorCode::Unsupported, "value {value:?}");
            assert_eq!(
                error.details.get("stage").and_then(|item| item.as_str()),
                Some("batchArgument"),
                "value {value:?}"
            );
        }
    }

    #[test]
    fn powershell_trampoline_is_a_single_invocation() {
        let body = powershell_trampoline(
            Path::new(r"C:\Program Files\x\a.ps1"),
            &["it's".into(), "中文".into()],
        );
        assert!(body.contains("& 'C:\\Program Files\\x\\a.ps1'"));
        assert!(body.contains("'it''s'"));
        assert!(body.contains("'中文'"));
        assert!(body.contains("exit $LASTEXITCODE"));
    }

    #[test]
    fn launch_paths_keep_unicode_and_spaces_inside_profile() {
        let root = std::env::temp_dir().join(format!("multicli-中文 & %PATH%-{}", uuid::Uuid::new_v4()));
        let paths = LaunchPaths::resolve(&root).unwrap();
        assert_eq!(paths.root, root.join("terminal").join("launch"));
        assert!(paths.root.is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn shell_candidates_are_stable_and_exist_on_this_host() {
        let items = shell_candidates();
        assert!(items.len() >= 2, "cmd and Windows PowerShell must be listed");
        assert_eq!(items[0].flavor, ShellFlavor::Cmd);
        assert_eq!(items[1].flavor, ShellFlavor::WindowsPowershell);
        for item in &items {
            assert_eq!(item.exists, item.path.is_file(), "{}", item.path.display());
        }
        assert!(shell_launch_args(ShellFlavor::Cmd).contains(&"chcp 65001>nul".to_string()));
    }

    #[test]
    fn planning_an_exe_target_never_generates_a_trampoline() {
        let exe = shell_path(ShellFlavor::Cmd).unwrap();
        let target = PinnedTarget {
            kind: TerminalTargetKind::Installation,
            label: "cmd".into(),
            product: None,
            agent_id: "local-shell".into(),
            installation_id: None,
            executable: exe.clone(),
            chain: LaunchChain::Exe,
            shell_flavor: None,
            version: None,
            preset_args: Vec::new(),
        };
        let root = std::env::temp_dir().join(format!("multicli-launch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let paths = LaunchPaths {
            root: root.clone(),
            note: None,
        };
        let plan = plan_launch(&target, &["a".into()], "t-1", &paths).unwrap();
        assert!(plan.trampoline.is_none());
        assert_eq!(plan.program, exe);
        assert_eq!(plan.argv_preview.len(), 2);
        assert!(plan.fingerprint.contains('|'));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn planning_a_missing_target_is_an_explicit_failure() {
        let target = PinnedTarget {
            kind: TerminalTargetKind::Installation,
            label: "missing".into(),
            product: None,
            agent_id: "a".into(),
            installation_id: None,
            executable: PathBuf::from(r"C:\definitely\missing\thing.exe"),
            chain: LaunchChain::Exe,
            shell_flavor: None,
            version: None,
            preset_args: Vec::new(),
        };
        let paths = LaunchPaths {
            root: std::env::temp_dir(),
            note: None,
        };
        let error = plan_launch(&target, &[], "t-2", &paths).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
    }
}
