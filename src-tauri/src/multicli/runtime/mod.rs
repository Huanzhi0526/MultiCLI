//! S05 — structured protocol runtime for the three core CLIs.
//!
//! Layers:
//!
//! - `model`: the additive `runtime-v1` wire contract (start/resume/send/
//!   answer/cancel/read, run and capability views, limits);
//! - `envelope`: the frozen `EventEnvelope` journal — per-`(runId, streamId)`
//!   seq, idempotent duplicates, preserved-but-not-executed unknown events;
//! - `process`: an owned child with a kill-on-close Job, its identity and a
//!   bounded NDJSON reader;
//! - `run`: one connection's state machine (turns, messages, tools,
//!   permissions, cancel, stop) plus durable persistence;
//! - `adapters`: `codex` (app-server JSON-RPC), `claude` (structured CLI),
//!   `codebuddy` (ACP), over one shared JSON-RPC peer;
//! - `launch`: argv planning, reusing S04's verified Windows launch chains;
//! - `manager`: the registry, target resolution, resume and the permission
//!   sweeper.
//!
//! This module never reads terminal bytes, never attaches to an existing
//! process and never answers a permission request on the user's behalf.

pub mod adapters;
pub mod envelope;
pub mod launch;
pub mod manager;
pub mod model;
pub mod process;
pub mod run;

#[cfg(test)]
mod tests;

pub use manager::RuntimeManager;
pub use model::{
    RuntimeAnswerRequest, RuntimeCancelRequest, RuntimeCapabilitiesView, RuntimeCapabilityView,
    RuntimeEventPage, RuntimeListView, RuntimeMessageView, RuntimeNotify, RuntimePermissionOption,
    RuntimePermissionView, RuntimeProduct, RuntimeReadRequest, RuntimeResumeRequest,
    RuntimeRunView, RuntimeSendRequest, RuntimeStartRequest, RuntimeStopRequest, RuntimeToolView,
    RuntimeTurnView, DEFAULT_PERMISSION_TIMEOUT_MS, MAX_RUNS, RUNTIME_SCHEMA_VERSION,
};
pub use run::{
    Adapter, AdapterEvent, EventMeta, NotifyFn as RuntimeNotifyFn, RuntimeRun,
};
