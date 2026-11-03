//! Windows Job Object guard for spawned CLIs.
//!
//! The existing cleanup (`taskkill /PID <pid> /T /F`) walks the live process
//! tree from the direct child down. A grandchild that outlives its parent
//! escapes that walk entirely: claude.exe exits while a PowerShell-tool
//! pwsh.exe is still running, the orphan re-parents to nothing together with
//! its hidden conhost.exe, and every later cleanup finds no tree to walk.
//! Turns accumulate one orphan family each until the machine crawls
//! (thousands of pwsh.exe/conhost.exe reported in the field).
//!
//! A Job Object tracks membership in the kernel instead of by parentage:
//! anything the child spawns joins the job, and with
//! JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE the kernel kills every surviving
//! member the moment our last job handle closes. Normal settle (RunContext
//! drop after `wait()`), Stop/terminal-error teardown, and an app crash all
//! converge on the same sweep, with no pid-reuse hazard.

#![cfg(windows)]

use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};

/// Owning handle to a kill-on-close job. The handle is only ever closed
/// (from `Drop`); nothing reads or writes it concurrently.
pub(crate) struct KillOnCloseJob(HANDLE);

unsafe impl Send for KillOnCloseJob {}
unsafe impl Sync for KillOnCloseJob {}

impl Drop for KillOnCloseJob {
    fn drop(&mut self) {
        // Dropping the last handle is what triggers the kernel sweep of any
        // surviving members — that is the whole point of the guard.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

impl KillOnCloseJob {
    /// Ends every live member of the job. Used by the managed terminal when a
    /// gentle stop timed out; it cannot reach a process outside this job.
    pub(crate) fn terminate(&self) {
        unsafe {
            let _ = TerminateJobObject(self.0, 1);
        }
    }
}

/// Why a process could not be put under a job guard. The managed terminal
/// treats this as a hard failure: running without the tree constraint would
/// silently allow an orphan family, which is exactly what S04 forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobFailure {
    pub stage: &'static str,
    pub reason: String,
}

fn job_for_process(process: HANDLE) -> Result<Arc<KillOnCloseJob>, JobFailure> {
    unsafe {
        let job = match CreateJobObjectW(None, None) {
            Ok(job) if !job.is_invalid() => job,
            _ => {
                return Err(JobFailure {
                    stage: "createJobObject",
                    reason: "CreateJobObjectW failed".into(),
                })
            }
        };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if configured.is_err() {
            let _ = CloseHandle(job);
            return Err(JobFailure {
                stage: "setInformationJobObject",
                reason: "SetInformationJobObject failed".into(),
            });
        }
        if AssignProcessToJobObject(job, process).is_err() {
            let _ = CloseHandle(job);
            return Err(JobFailure {
                stage: "assignProcessToJobObject",
                reason: "AssignProcessToJobObject failed (the process may already be in a \
                         non-nesting job or may have exited)"
                    .into(),
            });
        }
        Ok(Arc::new(KillOnCloseJob(job)))
    }
}

/// Job guard for a process that is not a `tokio::process::Child` (the managed
/// terminal owns a portable-pty child, which exposes only its PID).
pub(crate) fn job_for_pid(pid: u32) -> Result<Arc<KillOnCloseJob>, JobFailure> {
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };
    unsafe {
        let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, false, pid).map_err(
            |error| JobFailure {
                stage: "openProcess",
                reason: format!("OpenProcess failed with code {}", error.code().0),
            },
        )?;
        let result = job_for_process(process);
        let _ = CloseHandle(process);
        result
    }
}

/// Put `child` into a fresh kill-on-close job and return the shared guard.
/// The sweep fires when every clone of the returned Arc is gone.
///
/// `None` (with a log line) when the platform refuses — the caller then
/// keeps the taskkill-based cleanup as its only backstop, exactly as today.
/// Failure modes: the child already exited between spawn and assignment, or
/// it sits inside a job that forbids nesting (rare since Windows 8).
pub(crate) fn assign_kill_on_close(child: &tokio::process::Child) -> Option<Arc<KillOnCloseJob>> {
    let raw = child.raw_handle()?;
    match job_for_process(HANDLE(raw)) {
        Ok(guard) => Some(guard),
        Err(failure) => {
            eprintln!(
                "[engine] {}: {}; orphan sweep disabled for this run",
                failure.stage, failure.reason
            );
            None
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

    /// The field scenario: the direct child (cmd) exits while a detached
    /// grandchild (ping) keeps running, so `taskkill /T` has no tree left
    /// to walk. Dropping the guard must still sweep the orphan via job
    /// membership.
    #[tokio::test]
    async fn job_close_sweeps_orphaned_grandchild() {
        let mut child = tokio::process::Command::new("cmd")
            .args(["/c", "start /b ping -n 60 127.0.0.1 > NUL"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn cmd child");
        let cmd_pid = child.id().unwrap_or(0);
        let guard = assign_kill_on_close(&child).expect("job assignment succeeds");

        // `start /b` detaches ping; cmd exits immediately afterwards.
        let _ = child.wait().await;
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        // The orphan still carries the dead cmd's pid as its parent field.
        let orphans: std::collections::HashSet<Pid> = sys
            .processes()
            .iter()
            .filter(|(_, p)| p.parent() == Some(Pid::from_u32(cmd_pid)))
            .map(|(pid, _)| *pid)
            .collect();
        assert!(!orphans.is_empty(), "expected a surviving ping grandchild");

        drop(guard);

        let mut alive = orphans;
        for _ in 0..50 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            sys.refresh_processes_specifics(
                ProcessesToUpdate::All,
                true,
                ProcessRefreshKind::nothing(),
            );
            alive.retain(|pid| sys.process(*pid).is_some());
            if alive.is_empty() {
                break;
            }
        }
        assert!(
            alive.is_empty(),
            "kill-on-close job left orphan survivors alive: {alive:?}"
        );
    }
}
