//! Read-only host process backend.
//!
//! Scope of what this module reads from another process:
//! - the image path (`QueryFullProcessImageNameW`),
//! - the command line (used in memory for recognition only),
//! - PID / PPID and the OS creation time (`GetProcessTimes`, 100 ns).
//!
//! It deliberately does **not** read the process environment, the PEB working
//! directory, memory, handles or credentials. A working directory is therefore
//! unknown unless the CLI was given an explicit directory argument.
//!
//! Blocking enumeration is kept behind this seam so callers can run it on a
//! dedicated thread. No `spawn` / kill / attach capability is exposed: an
//! external process is observed, never controlled.

use crate::multicli::diagnostics::civil_from_days;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawProcess {
    pub pid: u32,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub executable: Option<PathBuf>,
    pub argv: Vec<String>,
    /// Seconds since the Unix epoch as reported by the OS enumeration, used
    /// only as a cheap PID-reuse pre-check. Never persisted as an identity.
    pub start_time_hint: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessDetails {
    pub pid: u32,
    /// Normalized OS creation time, UTC with 100 ns precision. This is the
    /// only value allowed to identify a process together with `pid`.
    pub creation_time: String,
    pub executable: Option<PathBuf>,
    pub argv: Vec<String>,
    pub parent_pid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendFailure {
    Unsupported,
    AccessDenied,
    Gone,
    Timeout,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError {
    pub kind: BackendFailure,
    /// Path-free, output-free reason.
    pub reason: String,
    pub pid: Option<u32>,
}

impl BackendError {
    pub fn access_denied(pid: u32) -> Self {
        Self {
            kind: BackendFailure::AccessDenied,
            reason: "process is not readable with normal user rights".to_string(),
            pid: Some(pid),
        }
    }

    pub fn gone(pid: u32) -> Self {
        Self {
            kind: BackendFailure::Gone,
            reason: "process no longer exists".to_string(),
            pid: Some(pid),
        }
    }

    pub fn failed(pid: Option<u32>, reason: impl Into<String>) -> Self {
        Self {
            kind: BackendFailure::Failed,
            reason: reason.into(),
            pid,
        }
    }
}

/// The seam the discovery engine talks to. Real Windows snapshot and test
/// doubles both implement it.
pub trait HostBackend: Send + Sync {
    fn enumerate(&self) -> Result<Vec<RawProcess>, BackendError>;
    fn inspect(&self, pid: u32) -> Result<ProcessDetails, BackendError>;
    fn resolve_executable(&self, pid: u32) -> Result<Option<PathBuf>, BackendError>;
}

/// Converts a Windows `FILETIME` to the identity string used everywhere else.
///
/// Windows counts 100 ns intervals since 1601-01-01; the Unix epoch offset is
/// 116444736000000000 ticks. Seven fractional digits keep the full OS
/// precision, so two processes started in the same second never collide.
pub fn filetime_to_iso(ticks: u64) -> Option<String> {
    const EPOCH_OFFSET: u64 = 116_444_736_000_000_000;
    if ticks < EPOCH_OFFSET {
        return None;
    }
    let since_epoch = ticks - EPOCH_OFFSET;
    let seconds = (since_epoch / 10_000_000) as i64;
    let fraction = since_epoch % 10_000_000;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{fraction:07}Z"
    ))
}

#[cfg(windows)]
mod windows_backend {
    use super::*;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, FILETIME};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };

    pub struct WindowsProcessBackend {
        host_id: String,
    }

    impl WindowsProcessBackend {
        pub fn new(host_id: impl Into<String>) -> Self {
            Self {
                host_id: host_id.into(),
            }
        }

        pub fn host_id(&self) -> &str {
            &self.host_id
        }

        fn snapshot(&self, only: Option<&[Pid]>) -> System {
            let mut system = System::new();
            let kind = ProcessRefreshKind::nothing()
                .with_exe(UpdateKind::Always)
                .with_cmd(UpdateKind::Always);
            let target = match only {
                Some(pids) => ProcessesToUpdate::Some(pids),
                None => ProcessesToUpdate::All,
            };
            system.refresh_processes_specifics(target, true, kind);
            system
        }

        fn raw_from_process(process: &sysinfo::Process) -> RawProcess {
            RawProcess {
                pid: process.pid().as_u32(),
                parent_pid: process.parent().map(|parent| parent.as_u32()),
                name: process.name().to_string_lossy().to_string(),
                executable: process.exe().map(|path| path.to_path_buf()),
                argv: process
                    .cmd()
                    .iter()
                    .map(|value| value.to_string_lossy().to_string())
                    .collect(),
                start_time_hint: process.start_time(),
            }
        }
    }

    impl HostBackend for WindowsProcessBackend {
        fn enumerate(&self) -> Result<Vec<RawProcess>, BackendError> {
            let system = self.snapshot(None);
            if system.processes().is_empty() {
                return Err(BackendError::failed(
                    None,
                    "process enumeration returned no rows",
                ));
            }
            Ok(system
                .processes()
                .values()
                .map(Self::raw_from_process)
                .collect())
        }

        fn inspect(&self, pid: u32) -> Result<ProcessDetails, BackendError> {
            let creation = process_creation_time(pid)?;
            let executable = query_image_path(pid)?;
            let pids = [Pid::from_u32(pid)];
            let system = self.snapshot(Some(&pids));
            let (argv, parent_pid) = system
                .processes()
                .get(&Pid::from_u32(pid))
                .map(|process| {
                    (
                        process
                            .cmd()
                            .iter()
                            .map(|value| value.to_string_lossy().to_string())
                            .collect::<Vec<_>>(),
                        process.parent().map(|parent| parent.as_u32()),
                    )
                })
                .unwrap_or_default();
            if process_creation_time(pid)? != creation {
                return Err(BackendError::failed(
                    Some(pid),
                    "process identity changed during inspection",
                ));
            }
            Ok(ProcessDetails {
                pid,
                creation_time: creation,
                executable,
                argv,
                parent_pid,
            })
        }

        fn resolve_executable(&self, pid: u32) -> Result<Option<PathBuf>, BackendError> {
            query_image_path(pid)
        }
    }

    fn map_open_error(pid: u32, error: windows::core::Error) -> BackendError {
        let code = error.code().0 as u32;
        match code {
            5 => BackendError::access_denied(pid),
            87 | 1168 => BackendError::gone(pid),
            _ => BackendError::failed(Some(pid), format!("OpenProcess failed with code {code}")),
        }
    }

    fn process_creation_time(pid: u32) -> Result<String, BackendError> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .map_err(|error| map_open_error(pid, error))?;
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            let result = GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user);
            let _ = CloseHandle(handle);
            result.map_err(|error| {
                BackendError::failed(
                    Some(pid),
                    format!("GetProcessTimes failed with code {}", error.code().0),
                )
            })?;
            let ticks = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
            filetime_to_iso(ticks).ok_or_else(|| {
                BackendError::failed(Some(pid), "creation time is not a valid FILETIME")
            })
        }
    }

    fn query_image_path(pid: u32) -> Result<Option<PathBuf>, BackendError> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)
                .map_err(|error| map_open_error(pid, error))?;
            let mut buffer = [0u16; 2048];
            let mut size = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut size,
            );
            let _ = CloseHandle(handle);
            match result {
                Ok(()) => {
                    let text = String::from_utf16_lossy(&buffer[..size as usize]);
                    if text.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(PathBuf::from(text)))
                    }
                }
                Err(error) => {
                    let code = error.code().0 as u32;
                    if code == 5 {
                        Err(BackendError::access_denied(pid))
                    } else if code == 87 || code == 1168 {
                        Err(BackendError::gone(pid))
                    } else {
                        Err(BackendError::failed(
                            Some(pid),
                            format!("QueryFullProcessImageNameW failed with code {code}"),
                        ))
                    }
                }
            }
        }
    }
}

#[cfg(windows)]
pub use windows_backend::WindowsProcessBackend;

#[cfg(not(windows))]
pub struct WindowsProcessBackend {
    host_id: String,
}

#[cfg(not(windows))]
impl WindowsProcessBackend {
    pub fn new(host_id: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
        }
    }

    pub fn host_id(&self) -> &str {
        &self.host_id
    }
}

#[cfg(not(windows))]
impl HostBackend for WindowsProcessBackend {
    fn enumerate(&self) -> Result<Vec<RawProcess>, BackendError> {
        Err(BackendError {
            kind: BackendFailure::Unsupported,
            reason: "local process discovery is implemented for Windows only".to_string(),
            pid: None,
        })
    }

    fn inspect(&self, pid: u32) -> Result<ProcessDetails, BackendError> {
        Err(BackendError {
            kind: BackendFailure::Unsupported,
            reason: "local process discovery is implemented for Windows only".to_string(),
            pid: Some(pid),
        })
    }

    fn resolve_executable(&self, pid: u32) -> Result<Option<PathBuf>, BackendError> {
        Err(BackendError {
            kind: BackendFailure::Unsupported,
            reason: "local process discovery is implemented for Windows only".to_string(),
            pid: Some(pid),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_keeps_100ns_precision_and_converts_correctly() {
        // 2023-11-14T22:13:20.1234567Z
        let ticks = 116_444_736_000_000_000u64 + 1_700_000_000 * 10_000_000 + 1_234_567;
        assert_eq!(
            filetime_to_iso(ticks).as_deref(),
            Some("2023-11-14T22:13:20.1234567Z")
        );
        assert!(filetime_to_iso(0).is_none());
        // Two processes started 100 ns apart never share an identity string.
        let first = filetime_to_iso(ticks).unwrap();
        let second = filetime_to_iso(ticks + 1).unwrap();
        assert_ne!(first, second);
    }

    #[cfg(windows)]
    #[test]
    fn real_enumerate_and_inspect_return_os_creation_times() {
        let backend = WindowsProcessBackend::new("local-windows");
        let rows = backend.enumerate().expect("real enumeration");
        assert!(rows.len() > 4, "expected a real process table");

        let self_pid = std::process::id();
        let details = backend.inspect(self_pid).expect("inspect own process");
        assert_eq!(details.pid, self_pid);
        assert!(
            details.creation_time.contains('.') && details.creation_time.ends_with('Z'),
            "creation time must keep sub-second precision: {}",
            details.creation_time
        );
        assert!(details
            .executable
            .as_ref()
            .map(|path| path.is_file())
            .unwrap_or(false));

        // Inspecting a PID that cannot exist reports `Gone`, not a fake row.
        let error = backend.inspect(0xFFFF_FFF0).unwrap_err();
        assert!(matches!(
            error.kind,
            BackendFailure::Gone | BackendFailure::AccessDenied | BackendFailure::Failed
        ));
    }
}
