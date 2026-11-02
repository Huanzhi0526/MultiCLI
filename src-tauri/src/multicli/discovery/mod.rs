//! S02 — installation and external-process discovery (read-only).
//!
//! Layers:
//! - `signature`: versioned product signatures, strict entry classification and
//!   the positive/negative sample matrix.
//! - `wrapper`: data-only parsing of `cmd`/PowerShell shims (never executed).
//! - `detect`: bounded scan of `PATH` and declared install roots.
//! - `probe`: timeout-bounded `--version` probes of trusted entries only.
//! - `winproc`: read-only Windows process backend (image path, argv, PID/PPID,
//!   100 ns creation time).
//! - `snapshot`: identity state machine (`alive` / `exited` / `unknown`,
//!   `fresh` / `stale`).
//! - `classify`: process → product recognition, parent-chain evidence and the
//!   only allowed workspace association (an explicit directory argument).
//! - `engine`: background sampling, caching and the IPC read models.
//! - `view`: wire shapes for `docs/contracts/discovery-v1.md`.
//!
//! Nothing in this module can spawn, attach to, signal or kill an external
//! process. The only process it ever terminates is its own probe child.

pub mod classify;
pub mod detect;
pub mod engine;
pub mod probe;
pub mod redact;
pub mod signature;
pub mod snapshot;
pub mod view;
pub mod winproc;
pub mod wrapper;

pub use engine::DiscoveryEngine;
pub use view::{
    DiscoverySettings, DiscoverySettingsUpdate, InstallationDiscoveryView, InstallationQuery,
    ProbeView, ProcessDiscoveryPage, ProcessDiscoveryView, ProcessQuery, ScanView,
    DISCOVERY_SCHEMA_VERSION, SAMPLING_NOTE,
};

#[cfg(test)]
mod tests;
