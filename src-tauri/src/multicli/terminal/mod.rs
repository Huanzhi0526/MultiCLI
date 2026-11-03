//! S04 — managed terminal and process lifecycle.
//!
//! Layers:
//! - `model`: the additive `terminal-v1` wire contract (targets, specs, views,
//!   limits, stop reports). Frozen S01 DTOs are reused, never modified.
//! - `launch`: target resolution plus the three verified Windows launch chains
//!   (`.exe`, `.cmd`/`.bat`, `.ps1`). User arguments never become shell text.
//! - `ring`: the bounded per-terminal byte ring; trims only on a character and
//!   escape-sequence boundary and holds back an incomplete tail.
//! - `logfile`: the per-terminal capped, rotating byte log.
//! - `identity`: `hostId + pid + creationTime` capture and re-check.
//! - `session`: one ConPTY + child + job + reader thread; the idempotent
//!   terminal state machine.
//! - `manager`: the registry, the Run/session/process records and the lifecycle
//!   operations the IPC layer exposes.
//!
//! This module only ever controls processes it created itself. External CLIs
//! observed by S02 are not addressable from here: there is no PID entry point.

pub mod identity;
pub mod launch;
pub mod logfile;
pub mod manager;
pub mod model;
pub mod ring;
pub mod session;

#[cfg(test)]
mod tests;

pub use manager::{TerminalManager, LOCAL_SHELL_AGENT_ID};
pub use model::{
    EnvVar, JobGuardMode, JobGuardView, LaunchChain, ShellFlavor, TerminalBuffersView,
    TerminalCloseRequest, TerminalCloseView, TerminalExitView, TerminalLimits,
    TerminalLimitsUpdate, TerminalListView, TerminalNotify, TerminalOpenSpec,
    TerminalQuitCheckView, TerminalReadRequest, TerminalReadView, TerminalResizeRequest,
    TerminalResizeView, TerminalShutdownAllRequest, TerminalShutdownAllView, TerminalState,
    TerminalStopMode, TerminalStopReport, TerminalStopRequest, TerminalStopView,
    TerminalTarget, TerminalTargetKind, TerminalTargetView, TerminalTargetsView, TerminalView,
    TerminalWriteRequest, TerminalWriteView, DEFAULT_BUFFER_BYTES, DEFAULT_LOG_BYTES,
    MAX_READ_BYTES, MAX_TERMINALS, TERMINAL_SCHEMA_VERSION,
};
pub use session::{FinishFn, NotifyFn, TerminalFinish, TerminalSession};
