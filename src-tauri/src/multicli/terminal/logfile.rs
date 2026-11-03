//! Per-terminal log file with a hard byte budget.
//!
//! The log is a byte log, not a message store: it exists so a user can look at
//! what a terminal actually printed after the 4 MiB ring dropped the history.
//! It is written only under this application's own profile directory, is
//! rotated once (`.1`) and reports both rotations and bytes that never fit, so
//! "the log is complete" is never implied.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::multicli::error::{AppError, ErrorCode};

#[derive(Debug)]
pub struct TerminalLog {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    cap: u64,
    rotations: u64,
    dropped: u64,
    last_error: Option<String>,
    pending: Vec<u8>,
}

impl TerminalLog {
    pub fn create(dir: &Path, terminal_id: &str, cap: u64, header: &str) -> Result<Self, AppError> {
        std::fs::create_dir_all(dir).map_err(|error| {
            AppError::new(
                ErrorCode::Io,
                format!("failed to create the terminal log directory: {error}"),
            )
            .with_details(serde_json::json!({ "stage": "createLogDir" }))
            .with_retryable(true)
        })?;
        let path = dir.join(format!("{terminal_id}.log"));
        let mut log = Self {
            path,
            file: None,
            written: 0,
            cap: cap.max(1024),
            rotations: 0,
            dropped: 0,
            last_error: None,
            pending: Vec::new(),
        };
        log.open()?;
        log.write(header.as_bytes());
        log.write(b"\r\n");
        Ok(log)
    }

    fn open(&mut self) -> Result<(), AppError> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| {
                AppError::new(
                    ErrorCode::Io,
                    format!("failed to open the terminal log: {error}"),
                )
                .with_details(serde_json::json!({ "stage": "openLog" }))
                .with_retryable(true)
            })?;
        self.file = Some(file);
        Ok(())
    }

    /// Rotates to `<name>.1` (replacing a previous rotation) and restarts the
    /// log. A rotation is visible in the view as `logRotations`.
    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        let rotated = self.path.with_extension("log.1");
        match std::fs::remove_file(&rotated) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::fs::rename(&self.path, &rotated)?;
        self.written = 0;
        self.rotations += 1;
        self.open().map_err(std::io::Error::other)
    }

    pub fn write(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.last_error.is_some() {
            self.dropped += bytes.len() as u64;
            return;
        }
        self.pending.extend_from_slice(bytes);
        let cut = super::ring::safe_log_cut(&self.pending);
        // A malformed, unclosed control sequence cannot grow memory forever.
        // Stop this log visibly rather than rotate in the middle of it.
        if self.pending.len() - cut > 16 * 1024 {
            self.dropped += self.pending.len() as u64;
            self.pending.clear();
            self.last_error = Some("terminal log stopped: incomplete control sequence exceeds 16 KiB".into());
            self.file = None;
            return;
        }
        let tail = self.pending.split_off(cut);
        let complete = std::mem::replace(&mut self.pending, tail);
        self.write_complete(&complete);
    }

    fn write_complete(&mut self, bytes: &[u8]) {
        if bytes.is_empty() { return; }
        // Reserve half the configured *terminal-wide* budget for the previous
        // file. Current + .1 can never silently consume twice the UI limit.
        let file_cap = self.cap / 2;
        if bytes.len() as u64 > file_cap {
            // A single chunk larger than the whole budget is never split: it is
            // counted as dropped so the limit stays honest.
            self.dropped += bytes.len() as u64;
            return;
        }
        if self.written + bytes.len() as u64 > file_cap {
            if let Err(error) = self.rotate() {
                // Never reopen the existing file in append mode with a zero
                // counter after a failed rename: that defeats the disk bound.
                self.last_error = Some(format!("terminal log rotation failed: {error}"));
                self.dropped += bytes.len() as u64;
                return;
            }
        }
        let Some(file) = self.file.as_mut() else {
            self.dropped += bytes.len() as u64;
            return;
        };
        match file.write_all(bytes) {
            Ok(()) => {
                self.written += bytes.len() as u64;
            }
            Err(error) => {
                self.dropped += bytes.len() as u64;
                self.file = None;
                self.written = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(self.written);
                self.last_error = Some(format!("terminal log write failed: {error}"));
            }
        }
    }

    /// An EOF cannot complete a partial character or escape. Account for the
    /// omitted tail instead of placing a corrupt fragment in a rotated file.
    pub fn finish(&mut self) {
        if !self.pending.is_empty() {
            self.dropped += self.pending.len() as u64;
            self.pending.clear();
            self.last_error.get_or_insert_with(|| "terminal log omitted an incomplete stream tail".into());
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    pub fn rotations(&self) -> u64 {
        self.rotations
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn capacity(&self) -> u64 {
        self.cap
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "multicli-log-{name}-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writing_past_the_budget_rotates_once_and_keeps_the_bytes() {
        let dir = temp_dir("rotate");
        let mut log = TerminalLog::create(&dir, "t-1", 1024, "header").unwrap();
        for _ in 0..20 {
            log.write(&[b'x'; 100]);
        }
        assert!(log.rotations() >= 1, "the log must rotate at the budget");
        assert!(log.written() <= 1024);
        let rotated = log.path().with_extension("log.1");
        assert!(rotated.is_file(), "a rotation must leave a readable .1 file");
        assert!(log.path().is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_chunk_is_counted_as_dropped_not_silently_truncated() {
        let dir = temp_dir("oversized");
        let mut log = TerminalLog::create(&dir, "t-2", 1024, "header").unwrap();
        log.write(&[b'y'; 4096]);
        assert_eq!(log.dropped(), 4096);
        assert_eq!(log.written(), "header\r\n".len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn logs_stay_inside_the_directory_they_were_given() {
        let dir = temp_dir("scope");
        let log = TerminalLog::create(&dir, "t-3", 4096, "header").unwrap();
        assert!(log.path().starts_with(&dir));
        assert_eq!(log.path().extension().unwrap(), "log");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_rotation_cannot_append_past_the_budget_or_claim_success() {
        let dir = temp_dir("failed-rotate");
        let mut log = TerminalLog::create(&dir, "t-bound", 1024 * 1024, "").unwrap();
        for _ in 0..40 { log.write(&[b'x'; 8192]); }
        std::fs::create_dir(log.path().with_extension("log.1")).unwrap();
        for _ in 0..100 { log.write(&[b'y'; 8192]); }
        assert!(std::fs::metadata(log.path()).unwrap().len() <= log.capacity());
        assert_eq!(std::fs::metadata(log.path()).unwrap().len(), log.written());
        assert!(log.dropped() > 0);
        assert!(log.last_error().is_some());
        assert_eq!(log.rotations(), 0);
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_preserves_split_utf8_and_control_sequences() {
        let dir = temp_dir("boundary");
        let mut log = TerminalLog::create(&dir, "utf8", 2048, "").unwrap();
        log.write(&vec![b'x'; 1020]);
        log.write(&[0xe4, 0xb8]);
        log.write(&[0xad, b'Z']);
        assert_eq!(std::fs::read(log.path()).unwrap(), "中Z".as_bytes());
        assert!(std::str::from_utf8(&std::fs::read(log.path().with_extension("log.1")).unwrap()).is_ok());
        let mut ansi = TerminalLog::create(&dir, "ansi", 2048, "").unwrap();
        ansi.write(&vec![b'x'; 1018]);
        ansi.write(b"\x1b[3");
        ansi.write(b"1mZ");
        assert_eq!(std::fs::read(ansi.path()).unwrap(), b"\x1b[31mZ");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn incomplete_log_tails_are_bounded_and_accounted_for() {
        let dir = temp_dir("tail");
        let mut log = TerminalLog::create(&dir, "tail", 1024, "").unwrap();
        log.write(&[0xe4, 0xb8]);
        log.finish();
        assert_eq!(log.dropped(), 2);
        let mut long = TerminalLog::create(&dir, "long", 1024, "").unwrap();
        long.write(b"\x1b]");
        for _ in 0..20 { long.write(&[b'x'; 1024]); }
        assert!(long.pending.is_empty());
        assert!(long.last_error().is_some());
        assert_eq!(long.dropped(), 20 * 1024 + 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn configured_budget_covers_both_retained_files() {
        let dir = temp_dir("total-budget");
        let mut log = TerminalLog::create(&dir, "total", 1024 * 1024, "").unwrap();
        for _ in 0..400 {
            log.write(&[b'x'; 8192]);
            let active = std::fs::metadata(log.path()).unwrap().len();
            let old = std::fs::metadata(log.path().with_extension("log.1")).map(|m| m.len()).unwrap_or(0);
            assert!(active + old <= log.capacity(), "total disk bytes exceed the terminal budget");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
