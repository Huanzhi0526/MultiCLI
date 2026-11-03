//! Process identity for an owned terminal child.
//!
//! A PID alone is not an identity: Windows reuses PIDs, and acting on a reused
//! PID would signal an unrelated process. Every terminal therefore records
//! `hostId + pid + creationTime` immediately after spawn, and every stop path
//! re-checks that triple before it touches the tree.
//!
//! The reader is the same OS inspection used by S02 discovery
//! (`PROCESS_QUERY_LIMITED_INFORMATION` + `GetProcessTimes`), reused instead of
//! duplicated, and it never reads the environment or memory of another process.

use crate::multicli::discovery::winproc::{BackendError, HostBackend, WindowsProcessBackend};
use crate::multicli::domain::ProcessIdentity;
use crate::multicli::error::{AppError, ErrorCode};

/// Result of re-checking a recorded identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityCheck {
    /// The PID still belongs to the process this terminal started.
    Same,
    /// The PID exists but belongs to a different process (PID reuse).
    Reused,
    /// No such process any more.
    Gone,
    /// The OS refused to answer; the caller must not act on the tree.
    Unknown { reason: String },
}

pub fn capture(host_id: &str, pid: u32) -> Result<ProcessIdentity, AppError> {
    let backend = WindowsProcessBackend::new(host_id);
    let details = backend.inspect(pid).map_err(|error: BackendError| {
        AppError::new(
            ErrorCode::Io,
            format!("cannot read the owned process identity: {}", error.reason),
        )
        .with_details(serde_json::json!({
            "stage": "captureIdentity",
            "pid": pid,
            "kind": format!("{:?}", error.kind),
        }))
        .with_retryable(true)
    })?;
    Ok(ProcessIdentity {
        host_id: host_id.to_string(),
        pid,
        creation_time: details.creation_time,
    })
}

pub fn check(identity: &ProcessIdentity) -> IdentityCheck {
    let backend = WindowsProcessBackend::new(&identity.host_id);
    match backend.inspect(identity.pid) {
        Ok(details) => {
            if details.creation_time == identity.creation_time {
                IdentityCheck::Same
            } else {
                IdentityCheck::Reused
            }
        }
        Err(error) => match error.kind {
            crate::multicli::discovery::winproc::BackendFailure::Gone => IdentityCheck::Gone,
            other => IdentityCheck::Unknown {
                reason: format!("{other:?}: {}", error.reason),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_child_identity_is_captured_and_verified() {
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "ping -n 10 127.0.0.1 > NUL"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn a real child");
        let pid = child.id();
        let identity = capture("local-windows", pid).expect("capture identity");
        assert_eq!(identity.pid, pid);
        assert!(identity.creation_time.ends_with('Z'));
        assert_eq!(check(&identity), IdentityCheck::Same);

        // A different creation time for the same PID is PID reuse, never "same".
        let mut forged = identity.clone();
        forged.creation_time = "2000-01-01T00:00:00.0000000Z".into();
        assert_eq!(check(&forged), IdentityCheck::Reused);

        let _ = child.kill();
        let _ = child.wait();
        // Windows keeps a zombie until the handle is closed; `check` is allowed
        // to say Gone or Same for a just-killed process, never Reused.
        assert_ne!(check(&identity), IdentityCheck::Reused);
    }

    #[test]
    fn capturing_a_pid_that_does_not_exist_is_an_error_not_a_fake_identity() {
        let error = capture("local-windows", 0xFFFF_FFF0).unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(error.retryable);
    }
}
