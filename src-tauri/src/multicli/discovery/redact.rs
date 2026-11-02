//! Command-line redaction.
//!
//! The full command line is used **in memory only**, to recognise an entry.
//! Everything that leaves the process — the database, the IPC payload, logs,
//! the diagnostics document and fixture dumps — goes through this module.
//!
//! What survives:
//! - the executable base name (never the full path),
//! - structural flags that are declared by the signature,
//! - a marker for how many arguments were withheld.
//!
//! What never survives: prompts, credentials, tokens, URLs (and therefore URL
//! query strings), account names, file paths and arbitrary user arguments.
//! Flag *values* are always replaced, even for known flags, so `--model
//! <value>` keeps the shape but not the value.

use super::signature::known_flags_set;
use std::collections::BTreeSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactedCommand {
    pub text: String,
    pub argument_count: usize,
    pub withheld_count: usize,
}

impl RedactedCommand {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Redacts one argv array. `argv[0]` is reduced to its base name.
pub fn redact_argv(argv: &[String]) -> RedactedCommand {
    redact_argv_with_flags(argv, known_flags_set())
}

/// OS observations use the image path: argv[0] is caller-controlled text.
pub fn redact_process_argv(image: Option<&Path>, argv: &[String]) -> RedactedCommand {
    let program = image
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".into());
    let mut normalized = Vec::with_capacity(argv.len().max(1));
    normalized.push(program);
    normalized.extend(argv.iter().skip(1).cloned());
    redact_argv(&normalized)
}

pub fn redact_argv_with_flags(argv: &[String], known: &BTreeSet<&str>) -> RedactedCommand {
    let program = argv
        .first()
        .map(|value| {
            Path::new(value)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    let mut parts = vec![program];
    let mut withheld = 0usize;
    let mut index = 1usize;
    while index < argv.len() {
        let token = argv[index].as_str();
        let structural = token.starts_with('-') && !token.contains('=') && known.contains(token);
        if structural {
            parts.push(token.to_string());
            let has_value = argv
                .get(index + 1)
                .map(|next| !next.starts_with('-'))
                .unwrap_or(false);
            if has_value {
                parts.push("<value>".to_string());
                index += 2;
                continue;
            }
            index += 1;
            continue;
        }
        withheld += 1;
        index += 1;
    }
    if withheld > 0 {
        parts.push(format!("<{withheld} withheld>"));
    }
    RedactedCommand {
        text: parts.join(" "),
        argument_count: argv.len().saturating_sub(1),
        withheld_count: withheld,
    }
}

/// True when a string looks like something that must never leave the process.
/// Used by tests and by the diagnostics writer as a defensive assertion.
pub fn looks_sensitive(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("token")
        || lower.contains("secret")
        || lower.contains("apikey")
        || lower.contains("api_key")
        || lower.contains("password")
        || lower.contains("authorization")
        || lower.contains("://")
        || lower.contains("?q=")
        || lower.contains("&access")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::multicli::discovery::signature::SignatureRegistry;

    #[test]
    fn process_redaction_uses_the_os_image_not_caller_argv0() {
        let args = argv(&["SOL_PRIVATE_ARGV0_TOKEN", "--model", "SOL_PRIVATE_MODEL"]);
        let result = redact_process_argv(Some(Path::new(r"C:\Tools\codex.exe")), &args);
        assert!(result.text.starts_with("codex.exe"));
        assert!(!result.text.contains("SOL_PRIVATE"));
        assert_eq!(
            redact_process_argv(None, &args).text,
            "unknown --model <value>"
        );
    }

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn prompts_tokens_urls_and_paths_never_survive() {
        let registry = SignatureRegistry::standard();
        let known = registry.known_flags();
        let full = argv(&[
            r"C:\Users\me\AppData\Local\Programs\OpenAI\Codex\bin\codex.exe",
            "--model",
            "gpt-5-secret-token",
            "exec",
            "SENTINEL-PROMPT please refactor C:\\Users\\me\\private",
            "--cd",
            r"C:\Users\me\secret-project",
            "https://example.test/run?token=abc&account=me",
            "sk-live-abcdef123456",
        ]);
        let redacted = redact_argv_with_flags(&full, &known);
        for forbidden in [
            "SENTINEL-PROMPT",
            "secret-project",
            "gpt-5-secret-token",
            "sk-live-abcdef123456",
            "example.test",
            "token=abc",
            "C:\\Users\\me",
        ] {
            assert!(
                !redacted.text.contains(forbidden),
                "redacted command leaked {forbidden}: {}",
                redacted.text
            );
        }
        assert!(redacted.text.starts_with("codex.exe"));
        assert!(redacted.text.contains("--model <value>"));
        assert!(redacted.text.contains("--cd <value>"));
        assert_eq!(redacted.withheld_count, 4);
    }

    #[test]
    fn unknown_flags_are_withheld_instead_of_echoed() {
        let registry = SignatureRegistry::standard();
        let known = registry.known_flags();
        let redacted = redact_argv_with_flags(
            &argv(&["codex.exe", "--evil-flag", "value", "-m", "gpt-5"]),
            &known,
        );
        assert!(!redacted.text.contains("--evil-flag"));
        assert!(redacted.text.contains("codex.exe"));
        assert!(redacted.text.contains("<2 withheld>"));
    }

    #[test]
    fn sensitive_probe_detects_obvious_markers() {
        assert!(looks_sensitive("Authorization: Bearer abc"));
        assert!(looks_sensitive("https://x.test/a?token=1"));
        assert!(!looks_sensitive("codex.exe --version"));
    }
}
