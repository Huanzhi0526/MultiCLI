//! One protected-directory policy for both the configured root and every
//! directory reached by the recursive walk.
//!
//! The root check alone was not enough: an allowed root such as
//! `%TEMP%\history` can contain a `.ssh` child, and the walker happily indexed
//! it. The rule is therefore a function of *path components*, applied at
//! configuration time and again during traversal, and it is re-applied to the
//! canonical form so a trailing dot, a different case or an 8.3 alias cannot
//! slip past it.
//!
//! Nothing here reads a credential file. The check is name-based plus a
//! canonical containment test; a directory that is skipped is skipped before
//! any of its entries are opened.

use super::ids;
use crate::multicli::path::canonical_text;
use std::path::{Path, PathBuf};

/// Directory names that are never a history root and never traversed, at any
/// depth. Matched case-insensitively after trimming trailing dots/spaces, which
/// Windows ignores when resolving a name.
pub const PROTECTED_DIRECTORIES: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".password-store",
    "credentials",
    "credential manager",
    "user data",
    "windows",
    "program files",
    "program files (x86)",
    "programdata",
    "$recycle.bin",
    "system volume information",
];

/// Two-component names that are protected as a pair (`Microsoft\Credentials`).
pub const PROTECTED_PAIRS: &[(&str, &str)] = &[("microsoft", "credentials")];

/// Windows forbids trailing dots/spaces in the resolved name but accepts them
/// in input; `.ssh.` and `.ssh ` both resolve to `.ssh`.
pub fn normalize_component(name: &str) -> String {
    name.trim().trim_end_matches(['.', ' ']).to_ascii_lowercase()
}

/// Returns the matched name when this single component is protected.
pub fn protected_component(name: &str) -> Option<String> {
    let normalized = normalize_component(name);
    if normalized.is_empty() {
        return None;
    }
    if PROTECTED_DIRECTORIES.contains(&normalized.as_str()) {
        return Some(normalized);
    }
    None
}

/// Scans a component sequence (root included) for a protected name or pair.
/// Returns the offending name so the caller can report *why*, never a path.
pub fn protected_in(components: &[String]) -> Option<String> {
    for (index, component) in components.iter().enumerate() {
        let normalized = normalize_component(component);
        if PROTECTED_DIRECTORIES.contains(&normalized.as_str()) {
            return Some(normalized);
        }
        if let Some(next) = components.get(index + 1) {
            let next = normalize_component(next);
            if PROTECTED_PAIRS
                .iter()
                .any(|(left, right)| *left == normalized && *right == next)
            {
                return Some(format!("{normalized}\\{next}"));
            }
        }
    }
    None
}

/// Component sequence of a path, lowercased, ignoring the prefix/root.
pub fn components_of(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(name) => Some(name.to_string_lossy().to_string()),
            _ => None,
        })
        .collect()
}

/// True when `candidate` is the root itself or below it.
///
/// Both sides are canonical and folded, so a junction, a short name, a
/// different drive casing or a `..` segment cannot produce a false positive.
pub fn is_within(root_canonical: &str, candidate_canonical: &str) -> bool {
    let root = ids::fold_path(root_canonical)
        .trim_end_matches('\\')
        .to_string();
    let candidate = ids::fold_path(candidate_canonical);
    candidate == root || candidate.starts_with(&format!("{root}\\"))
}

/// Result of resolving one listed entry.
#[derive(Debug)]
pub enum Containment {
    Inside(PathBuf),
    /// Resolved outside the configured root (a link or a lexical escape).
    /// Routine on a tree that contains junctions: skip it.
    Outside,
    /// The path could not be canonicalized at all (a transient sharing or ACL
    /// error). The listing is then *not* authoritative, because a file that is
    /// still there would look deleted and its rows would be purged.
    Unresolved,
}

/// Canonicalizes an entry and proves it still lives below the root.
pub fn contained_canonical(root_canonical: &str, path: &Path) -> Containment {
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return Containment::Unresolved;
    };
    let text = canonical_text(&canonical);
    if !is_within(root_canonical, &text) {
        return Containment::Outside;
    }
    Containment::Inside(PathBuf::from(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_matched_case_insensitively_and_after_trailing_dots() {
        assert!(protected_component(".SSH").is_some());
        assert!(protected_component(".ssh.").is_some());
        assert!(protected_component(" Credentials ").is_some());
        assert!(protected_component("sessions").is_none());
        assert!(protected_component(".codex").is_none());
    }

    #[test]
    fn pairs_and_sequences_are_checked() {
        let pair = vec!["Users".to_string(), "x".into(), "Microsoft".into(), "Credentials".into()];
        assert_eq!(protected_in(&pair).as_deref(), Some("microsoft\\credentials"));
        let plain = vec!["Users".to_string(), "x".into(), ".codex".into(), "sessions".into()];
        assert!(protected_in(&plain).is_none());
    }

    #[test]
    fn containment_respects_separators() {
        assert!(is_within("C:\\a\\b", "C:\\a\\b"));
        assert!(is_within("C:\\a\\b", "c:/a/b/c/d.jsonl"));
        assert!(!is_within("C:\\a\\b", "C:\\a\\bb\\c"));
        assert!(!is_within("C:\\a\\b", "C:\\a"));
    }
}
