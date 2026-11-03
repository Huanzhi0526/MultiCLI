//! Codex official usage read (`codex app-server`, JSON-RPC).
//!
//! Only three read methods are ever sent:
//!
//! | method | why |
//! |---|---|
//! | `account/read` | which account the numbers belong to (identity) |
//! | `account/rateLimits/read` | the metered allowance windows |
//! | `account/usage/read` | token activity (separate object from the allowance) |
//!
//! The installed CLI also exposes `account/login/*`, `account/logout`,
//! `account/rateLimitResetCredit/consume` and `account/sendAddCreditsNudgeEmail`.
//! **None of them is called here**: this module never authenticates, logs out,
//! redeems a reset credit, adds credits or sends mail. It reads, then closes its
//! own process.
//!
//! Response shapes come from the installed 0.166.0 schema snapshot
//! (`scratch/s00/protocol-profiles/codex-schema-0.166.0/v2/GetAccount*.json`),
//! not from guesswork. Every optional field stays `Option`: a `null`
//! `rateLimits.primary` is "not reported", never "0 % used".

use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::history::ids::sha256_hex;
use crate::multicli::runtime::process::{LineReader, OwnedProcess, ProcessSpec};

use super::model::{
    now_iso8601, AccountProfile, IdentityStatus, TokenSemantics, UsageSample, UsageSourceKind,
    UsageStatus, UsageWindow,
};

const CALL_TIMEOUT: Duration = Duration::from_secs(20);
const SOURCE: &str = "codex app-server account/rateLimits/read";

/// One read of the account, its allowance and its token activity.
#[derive(Debug, Clone, Default)]
pub struct CodexReading {
    pub identity: Option<CodexIdentity>,
    pub rate_limits: Option<Value>,
    pub usage: Option<Value>,
    /// Methods the CLI answered with an error, kept **per method** so a 429 on
    /// one method is not flattened into a string next to a successful read.
    pub failed_methods: Vec<MethodFailure>,
    /// A transport-level failure (timeout, closed stdout) that stopped the
    /// exchange. Fields read before it stay valid.
    pub transport_error: Option<AppError>,
}

/// A structured per-method RPC failure: which method, which kind of failure,
/// the provider's code and its retry hint. This is what the snapshot stores so
/// the UI and the backoff logic both see the real reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MethodFailure {
    pub method: String,
    /// `rate-limited | unauthenticated | protocol | transport | unknown`.
    pub kind: String,
    pub message: String,
    pub rpc_code: Option<i64>,
    pub retry_after_ms: Option<i64>,
}

impl MethodFailure {
    pub fn from_error(method: &str, error: &AppError) -> Self {
        Self {
            method: method.to_string(),
            kind: error
                .details
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("protocol")
                .to_string(),
            message: error.message.clone(),
            rpc_code: error.details.get("rpcCode").and_then(Value::as_i64),
            retry_after_ms: error.details.get("retryAfterMs").and_then(Value::as_i64),
        }
    }

    pub fn is_rate_limited(&self) -> bool {
        self.kind == "rate-limited"
    }

    pub fn is_authentication(&self) -> bool {
        self.kind == "unauthenticated"
    }

    /// Serialized shape stored in `AppError.details["methods"]`.
    pub fn to_json(&self) -> Value {
        json!({
            "method": self.method,
            "kind": self.kind,
            "message": self.message,
            "rpcCode": self.rpc_code,
            "retryAfterMs": self.retry_after_ms,
        })
    }
}

impl From<&str> for MethodFailure {
    fn from(value: &str) -> Self {
        Self {
            method: "unknown".to_string(),
            kind: "unknown".to_string(),
            message: value.to_string(),
            ..Self::default()
        }
    }
}

impl From<String> for MethodFailure {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexIdentity {
    pub account_type: String,
    pub email_masked: Option<String>,
    pub email_fingerprint: Option<String>,
    pub plan: Option<String>,
    pub workspace: Option<String>,
}

impl CodexIdentity {
    pub fn status(&self) -> IdentityStatus {
        if self.email_fingerprint.is_some() {
            IdentityStatus::Confirmed
        } else {
            IdentityStatus::Unconfirmed
        }
    }

    pub fn label(&self) -> String {
        let mut parts = vec!["Codex".to_string()];
        if let Some(email) = &self.email_masked {
            parts.push(email.clone());
        }
        if let Some(plan) = &self.plan {
            parts.push(plan.clone());
        }
        if let Some(workspace) = &self.workspace {
            parts.push(workspace.clone());
        }
        if self.email_masked.is_none() {
            parts.push(self.account_type.clone());
        }
        parts.join(" · ")
    }
}

/// Masks an e-mail for display: `a***@example.com`. The full address is never
/// stored or shown; the fingerprint is what dedupe uses.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) if !local.is_empty() => {
            let first: String = local.chars().take(1).collect();
            format!("{first}***@{domain}")
        }
        _ => "***".to_string(),
    }
}

/// Reads the three account methods from a **new, owned** `codex app-server`
/// process. The process is always killed by this function, including on error.
/// `args` and `env` come from the S04 launch plan, so a `.cmd` installation is
/// started through its trampoline instead of being passed to `CreateProcess`
/// as an image (which would fail for an npm shim).
pub fn read(
    program: &std::path::Path,
    args: &[String],
    cwd: &std::path::Path,
    env: &[(String, String)],
) -> Result<CodexReading, AppError> {
    let spec = ProcessSpec {
        program: program.to_path_buf(),
        args: args.to_vec(),
        cwd: cwd.to_path_buf(),
        env: env.to_vec(),
        env_remove: Vec::new(),
    };
    let (process, stdout) = OwnedProcess::spawn("local-windows", &spec)?;
    let process = std::sync::Arc::new(process);
    let (sender, receiver) = channel::<Value>();
    let (failure_sender, failure_receiver) = channel::<String>();
    std::thread::Builder::new()
        .name("multicli-usage-codex-reader".into())
        .spawn(move || {
            let mut reader = LineReader::new(stdout);
            loop {
                match reader.next_line() {
                    Ok(Some(line)) => {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(trimmed) {
                            Ok(value) => {
                                if sender.send(value).is_err() {
                                    return;
                                }
                            }
                            Err(_) => continue,
                        }
                    }
                    Ok(None) => {
                        let _ = failure_sender.send("the CLI closed stdout".to_string());
                        return;
                    }
                    Err(error) => {
                        let _ = failure_sender.send(error.message);
                        return;
                    }
                }
            }
        })
        .map_err(|error| {
            AppError::new(
                ErrorCode::Io,
                format!("failed to start the usage reader: {error}"),
            )
        })?;

    let outcome = exchange(&process, &receiver, &failure_receiver);
    process.kill_tree();
    let _ = process.wait_timeout(Duration::from_secs(5));
    outcome
}

fn exchange(
    process: &std::sync::Arc<OwnedProcess>,
    receiver: &Receiver<Value>,
    failures: &Receiver<String>,
) -> Result<CodexReading, AppError> {
    // 1. Handshake. `initialize` must be answered before anything else.
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": { "name": "multicli-usage", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": {}
        }
    });
    write(process, &initialize)?;
    match wait_for(process.as_ref(), &receiver, &failures, 1, CALL_TIMEOUT)? {
        Ok(_) => {}
        Err(error) => return Err(error),
    }
    write(process, &json!({ "jsonrpc": "2.0", "method": "initialized" }))?;

    let mut reading = CodexReading::default();
    for (id, method, params) in [
        (2_i64, "account/read", json!({ "refreshToken": false })),
        (3, "account/rateLimits/read", json!({})),
        (4, "account/usage/read", json!({ "threadId": null })),
    ] {
        write(
            process,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )?;
        match wait_for(process.as_ref(), &receiver, &failures, id, CALL_TIMEOUT) {
            Ok(Ok(value)) => match method {
                "account/read" => reading.identity = parse_account(&value)?,
                "account/rateLimits/read" => reading.rate_limits = Some(value),
                _ => reading.usage = Some(value),
            },
            Ok(Err(error)) => {
                reading
                    .failed_methods
                    .push(MethodFailure::from_error(method, &error));
                // An authentication failure is a real, distinct state: it is
                // not an empty success and it is not zero usage.
                if is_authentication(&error) {
                    return Err(error);
                }
            }
            Err(error) => {
                // A transport failure ends the exchange, but whatever was read
                // before it stays valid and is reported as a method failure.
                reading.failed_methods.push(MethodFailure {
                    method: method.to_string(),
                    kind: "transport".to_string(),
                    message: error.message.clone(),
                    rpc_code: None,
                    retry_after_ms: retry_after_ms(&error),
                });
                reading.transport_error = Some(error);
                break;
            }
        }
    }
    Ok(reading)
}

fn write(process: &OwnedProcess, frame: &Value) -> Result<(), AppError> {
    let text = serde_json::to_string(frame)
        .map_err(|error| AppError::new(ErrorCode::Protocol, format!("encode frame: {error}")))?;
    process.write_line(&text)
}

/// Waits for the reply that carries `id`. Server requests (approvals etc.) are
/// answered with a refusal so a read can never hang waiting for a human; any
/// other notification is ignored.
#[allow(clippy::type_complexity)]
fn wait_for(
    process: &OwnedProcess,
    receiver: &Receiver<Value>,
    failures: &Receiver<String>,
    id: i64,
    timeout: Duration,
) -> Result<Result<Value, AppError>, AppError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(AppError::new(
                ErrorCode::Timeout,
                format!("codex app-server did not answer request {id} in time"),
            )
            .with_details(json!({ "stage": "codexUsageTimeout", "requestId": id }))
            .with_retryable(true));
        }
        match receiver.recv_timeout(remaining) {
            Ok(value) => {
                if value.get("method").is_some() && value.get("id").is_some() {
                    // A server request during an account read must not block:
                    // this process exists only to read numbers, so it is
                    // refused explicitly instead of being left waiting.
                    let request_id = value.get("id").cloned().unwrap_or(Value::Null);
                    let _ = write(
                        process,
                        &json!({
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "error": { "code": -32601, "message": "the usage reader does not serve interactive requests" }
                        }),
                    );
                    continue;
                }
                if value.get("id").and_then(Value::as_i64) != Some(id) {
                    continue;
                }
                if let Some(error) = value.get("error") {
                    return Ok(Err(decode_rpc_error(error)));
                }
                return Ok(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(AppError::new(
                    ErrorCode::Timeout,
                    format!("codex app-server did not answer request {id} in time"),
                )
                .with_details(json!({ "stage": "codexUsageTimeout", "requestId": id }))
                .with_retryable(true))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let detail = failures
                    .try_recv()
                    .unwrap_or_else(|_| "the CLI exited".to_string());
                return Err(AppError::new(
                    ErrorCode::Protocol,
                    format!("codex app-server ended before request {id} was answered: {detail}"),
                )
                .with_details(json!({ "stage": "codexUsageClosed" }))
                .with_retryable(true));
            }
        }
    }
}

fn decode_rpc_error(error: &Value) -> AppError {
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(-32603);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error")
        .to_string();
    let lower = message.to_ascii_lowercase();
    let kind = if lower.contains("429")
        || lower.contains("rate limit")
        || lower.contains("rate_limit")
        || lower.contains("too many requests")
    {
        "rate-limited"
    } else if lower.contains("auth")
        || lower.contains("unauthorized")
        || lower.contains("401")
        || lower.contains("login")
    {
        "unauthenticated"
    } else {
        "protocol"
    };
    let retry_after = error
        .get("data")
        .and_then(|data| {
            data.get("retryAfterMs")
                .or_else(|| data.get("retry_after_ms"))
                .or_else(|| data.get("retry_after"))
        })
        .and_then(Value::as_i64);
    let code_kind = match kind {
        "rate-limited" => ErrorCode::Unsupported,
        "unauthenticated" => ErrorCode::AccessDenied,
        _ => ErrorCode::Protocol,
    };
    AppError::new(code_kind, message.clone())
        .with_details(json!({
            "stage": "codexUsageRpc",
            "kind": kind,
            "rpcCode": code,
            "message": message,
            "retryAfterMs": retry_after,
        }))
        .with_retryable(kind != "unauthenticated")
}

/// `true` when the error means "this CLI has no usable credential".
pub fn is_authentication(error: &AppError) -> bool {
    error.details.get("kind").and_then(Value::as_str) == Some("unauthenticated")
}

/// `true` when the provider asked us to slow down.
pub fn is_rate_limited(error: &AppError) -> bool {
    error.details.get("kind").and_then(Value::as_str) == Some("rate-limited")
}

/// The provider's own retry hint, if it sent one.
pub fn retry_after_ms(error: &AppError) -> Option<i64> {
    error.details.get("retryAfterMs").and_then(Value::as_i64)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// `account/read` → identity. `None` means the CLI is not signed in.
pub fn parse_account(result: &Value) -> Result<Option<CodexIdentity>, AppError> {
    let account = result.get("account").cloned().unwrap_or(Value::Null);
    if account.is_null() {
        return Ok(None);
    }
    let account_type = account
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let email = account
        .get("email")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let email_fingerprint = email
        .as_deref()
        .map(|value| sha256_hex(format!("codex-account:{}", value.to_lowercase()).as_bytes()));
    Ok(Some(CodexIdentity {
        account_type,
        email_masked: email.as_deref().map(mask_email),
        email_fingerprint,
        plan: account
            .get("planType")
            .and_then(Value::as_str)
            .map(str::to_string),
        workspace: account
            .get("workspaceName")
            .and_then(Value::as_str)
            .map(str::to_string),
    }))
}

/// `account/rateLimits/read` → samples. The multi-bucket view is preferred when
/// present; each bucket keeps its own `limitId` label so two buckets are two
/// rows, never summed.
pub fn parse_rate_limits(result: &Value, captured_at: &str) -> Vec<UsageSample> {
    let mut samples = Vec::new();
    let mut buckets: Vec<(String, Value)> = Vec::new();
    if let Some(map) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        let mut keys: Vec<&String> = map.keys().collect();
        keys.sort();
        for key in keys {
            buckets.push((key.clone(), map[key].clone()));
        }
    }
    if buckets.is_empty() {
        if let Some(single) = result.get("rateLimits").filter(|value| !value.is_null()) {
            buckets.push(("codex".to_string(), single.clone()));
        }
    }
    if buckets.is_empty() {
        return samples;
    }
    for (key, bucket) in buckets {
        let label = bucket
            .get("limitName")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| key.clone());
        for (slot, window_name) in [("primary", "primary"), ("secondary", "secondary")] {
            let Some(window) = bucket.get(slot).filter(|value| !value.is_null()) else {
                continue;
            };
            let used = window
                .get("usedPercent")
                .and_then(Value::as_f64)
                .map(|value| value.clamp(0.0, 100.0));
            let minutes = window.get("windowDurationMins").and_then(Value::as_i64);
            if used.is_none() && minutes.is_none() {
                // An empty window object is "not reported", not a zero.
                continue;
            }
            let resets_at = window
                .get("resetsAt")
                .and_then(Value::as_i64)
                .and_then(epoch_seconds_to_iso);
            let mut sample = UsageSample::quota(
                format!("{label} · {window_name}"),
                used,
                minutes,
                resets_at,
                SOURCE,
            );
            sample.note = Some(match (used, minutes) {
                (None, _) => "提供方未报告已用比例".to_string(),
                (_, None) => "提供方未报告窗口长度".to_string(),
                _ => format!("读取于 {captured_at}"),
            });
            samples.push(sample);
        }
        if let Some(credits) = bucket.get("credits").filter(|value| !value.is_null()) {
            let unlimited = credits
                .get("unlimited")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let has_credits = credits
                .get("hasCredits")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let balance = credits
                .get("balance")
                .and_then(Value::as_str)
                .and_then(|value| value.trim().parse::<f64>().ok());
            let mut sample = UsageSample::quota(format!("{label} · credits"), None, None, None, SOURCE);
            sample.metric = super::model::UsageMetric::CreditBalance;
            sample.unit = super::model::UsageUnit::Credits;
            sample.window = UsageWindow::Lifetime;
            sample.amount = balance;
            sample.note = Some(if unlimited {
                "不限量额度".to_string()
            } else if has_credits {
                "提供方报告有可用额度".to_string()
            } else {
                "提供方报告无可用额度".to_string()
            });
            samples.push(sample);
        }
        if let Some(individual) = bucket.get("individualLimit").filter(|value| !value.is_null()) {
            let remaining = individual
                .get("remainingPercent")
                .and_then(Value::as_f64)
                .map(|value| value.clamp(0.0, 100.0));
            if remaining.is_some() {
                let mut sample = UsageSample::quota(
                    format!("{label} · spend control"),
                    remaining.map(|value| 100.0 - value),
                    None,
                    individual
                        .get("resetsAt")
                        .and_then(Value::as_i64)
                        .and_then(epoch_seconds_to_iso),
                    SOURCE,
                );
                sample.note = Some("提供方 spend-control 剩余比例".to_string());
                samples.push(sample);
            }
        }
    }
    samples
}

/// `account/usage/read` → token samples. Codex's `inputTokens` already contains
/// cached input, so the total is `input + output` and cached tokens are only a
/// subset marker.
pub fn parse_token_usage(result: &Value) -> Vec<UsageSample> {
    let mut samples = Vec::new();
    let semantics = TokenSemantics::InputIncludesCached;
    if let Some(thread) = result.get("threadUsage").filter(|value| !value.is_null()) {
        let groups = thread
            .get("groups")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (index, group) in groups.iter().enumerate() {
            let input = group.get("inputTokens").and_then(Value::as_u64);
            let cached = group.get("cachedInputTokens").and_then(Value::as_u64);
            let output = group.get("outputTokens").and_then(Value::as_u64);
            let total = group.get("totalTokens").and_then(Value::as_u64);
            if input.is_none() && cached.is_none() && output.is_none() && total.is_none() {
                continue;
            }
            let model = group
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown model");
            let mut sample = UsageSample::tokens(
                format!("thread · {model} #{index}"),
                UsageWindow::Session,
                semantics,
                input,
                cached,
                None,
                output,
                total,
                "codex app-server account/usage/read",
            );
            if let Some(micros) = group.get("estimatedUsageCreditsMicros").and_then(Value::as_i64) {
                sample.note = Some(format!("提供方估算 credits · {micros} µcredits"));
            }
            samples.push(sample);
        }
    }
    if let Some(buckets) = result.get("dailyUsageBuckets").and_then(Value::as_array) {
        // Keep the three most recent days. The array order is not part of the
        // contract, so sort by `startDate` instead of trusting it.
        let mut ordered: Vec<&Value> = buckets.iter().collect();
        ordered.sort_by(|left, right| {
            let key = |value: &Value| {
                value
                    .get("startDate")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string()
            };
            key(right).cmp(&key(left))
        });
        for bucket in ordered.into_iter().take(3) {
            let start = bucket
                .get("startDate")
                .and_then(Value::as_str)
                .unwrap_or("unknown day");
            let tokens = bucket.get("tokens").and_then(Value::as_u64);
            if tokens.is_none() {
                continue;
            }
            // `tokens` is the day's **total**. It is not an input count, and the
            // wire data carries no in/cached/out split, so none is invented.
            samples.push(UsageSample::token_total(
                format!("tokens · {start}"),
                UsageWindow::Daily,
                tokens.expect("checked"),
                "codex app-server account/usage/read",
            ));
        }
    }
    if let Some(lifetime) = result
        .get("summary")
        .and_then(|summary| summary.get("lifetimeTokens"))
        .and_then(Value::as_u64)
    {
        samples.push(UsageSample::token_total(
            "tokens · lifetime",
            UsageWindow::Lifetime,
            lifetime,
            "codex app-server account/usage/read",
        ));
    }
    samples
}

fn epoch_seconds_to_iso(value: i64) -> Option<String> {
    (value > 0)
        .then(|| value.checked_mul(1000))
        .flatten()
        .map(crate::multicli::history::ids::iso_from_millis)
}

/// Converts a raw reading into the account/pool/snapshot triple the centre
/// stores. Split out from the process code so every branch is unit-testable
/// without a CLI.
///
/// Status precedence, from the most to the least severe:
///
/// 1. `unauthenticated` — no credential, nothing else matters;
/// 2. `rate-limited` — a provider 429 wins even when another method succeeded,
///    because it is what the backoff must follow;
/// 3. `error` — every read method failed and nothing was measured;
/// 4. `partial` — some method failed, or the provider answered with no values;
/// 5. `ok` — every method answered and at least one number was produced.
pub fn reading_to_snapshot(
    reading: &CodexReading,
    pool_id: &str,
    now: &str,
) -> super::model::UsageSnapshot {
    let mut snapshot = super::model::UsageSnapshot::empty(pool_id, "codex");
    snapshot.captured_at = now.to_string();
    snapshot.source_kind = UsageSourceKind::OfficialMachine;
    snapshot.source = SOURCE.to_string();
    snapshot.expires_at = Some(super::model::iso_after_ms(super::model::DEFAULT_TTL_MS));
    let mut samples = Vec::new();
    if let Some(rate_limits) = &reading.rate_limits {
        samples.extend(parse_rate_limits(rate_limits, now));
    }
    if let Some(usage) = &reading.usage {
        samples.extend(parse_token_usage(usage));
    }
    let read_quota = reading.rate_limits.is_some();
    let read_tokens = reading.usage.is_some();
    let empty = !samples.iter().any(UsageSample::has_numeric_value);
    snapshot.samples = samples;

    let unauthenticated = reading.failed_methods.iter().any(MethodFailure::is_authentication);
    let rate_limited = reading
        .failed_methods
        .iter()
        .find(|failure| failure.is_rate_limited());
    let all_failed = !read_quota && !read_tokens && !reading.failed_methods.is_empty();

    snapshot.status = if unauthenticated {
        UsageStatus::Unauthenticated
    } else if rate_limited.is_some() {
        // A successful token read never hides a quota 429: the allowance is
        // unknown and the provider asked us to slow down.
        UsageStatus::RateLimited
    } else if all_failed {
        // Every requested method failed. That is a failure, not a "partial"
        // measured reading with nothing in it.
        UsageStatus::Error
    } else if !reading.failed_methods.is_empty() || empty {
        UsageStatus::Partial
    } else {
        UsageStatus::Ok
    };

    snapshot.retry_after_ms = rate_limited.and_then(|failure| failure.retry_after_ms);
    // Methods that answered but reported no number are listed by name; a
    // missing field is never rendered as 0.
    if empty {
        snapshot
            .unavailable_fields
            .push("values（提供方未返回任何数值）".to_string());
    }
    if !read_quota {
        snapshot.unavailable_fields.push("rateLimits".to_string());
    }
    if !read_tokens {
        snapshot.unavailable_fields.push("tokenUsage".to_string());
    }
    for failure in &reading.failed_methods {
        snapshot
            .unavailable_fields
            .push(format!("{}({})", failure.method, failure.kind));
    }

    if !reading.failed_methods.is_empty() || reading.transport_error.is_some() {
        // Every method failure is preserved with its own kind, code and retry
        // hint; the note is a short, readable summary of the same facts.
        let methods: Vec<Value> = reading
            .failed_methods
            .iter()
            .map(MethodFailure::to_json)
            .collect();
        let summary = reading
            .failed_methods
            .iter()
            .map(|failure| format!("{}: {} ({})", failure.method, failure.message, failure.kind))
            .collect::<Vec<_>>()
            .join("; ");
        let error = reading
            .transport_error
            .clone()
            .unwrap_or_else(|| {
                AppError::new(
                    ErrorCode::Protocol,
                    if all_failed {
                        "codex app-server answered no usage method"
                    } else {
                        "codex app-server failed part of the usage read"
                    },
                )
            })
            .with_details(json!({
                "stage": "codexUsageRead",
                "kind": rate_limited.map(|failure| failure.kind.clone()),
                "retryAfterMs": snapshot.retry_after_ms,
                "failedMethods": summary,
                "methods": methods,
            }));
        snapshot.note = Some(if all_failed {
            format!("全部读取方法失败：{summary}")
        } else {
            format!("部分方法失败：{summary}")
        });
        // For a partial reading the error documents what is missing; for a
        // failed one it is the reason there are no numbers at all.
        snapshot.error = Some(error);
    }
    snapshot
}

/// Builds the account profile from a reading, or `None` when the CLI is not
/// signed in (that becomes `unauthenticated`, not an empty account).
/// Builds the account profile only when the provider actually named the
/// account. An unconfirmed identity has no account row: its pool is keyed by the
/// installation, so two unknown accounts can never be merged.
pub fn account_from_reading(reading: &CodexReading, host_id: &str) -> Option<AccountProfile> {
    let identity = reading.identity.as_ref()?;
    let status = identity.status();
    if status != super::model::IdentityStatus::Confirmed {
        return None;
    }
    Some(AccountProfile {
        id: format!(
            "account-codex-{}",
            identity
                .email_fingerprint
                .as_deref()
                .map(|value| value.chars().take(16).collect::<String>())
                .unwrap_or_else(|| "unconfirmed".to_string())
        ),
        host_id: host_id.to_string(),
        agent_id: "codex".to_string(),
        product: "Codex CLI".to_string(),
        vendor: "OpenAI".to_string(),
        label: identity.label(),
        identity_status: status,
        identity_fingerprint: identity.email_fingerprint.clone(),
        plan: identity.plan.clone(),
        source: "codex app-server account/read".to_string(),
        observed_at: now_iso8601(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_metadata_without_values_is_partial() {
        let reading = CodexReading {
            rate_limits: Some(json!({"rateLimits":{"primary":{"usedPercent":null,"windowDurationMins":300}}})),
            usage: Some(json!({})),
            ..Default::default()
        };
        let snapshot = reading_to_snapshot(&reading, "pool", "2026-11-03T00:00:00Z");
        assert_eq!(snapshot.status, UsageStatus::Partial);
        assert!(!snapshot.samples.iter().any(UsageSample::has_numeric_value));
        assert!(!snapshot.unavailable_fields.is_empty());
    }

    fn rate_limits_payload() -> Value {
        json!({
            "accountId": "acc-1",
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex",
                "limitName": "Codex",
                "planType": "plus",
                "primary": { "usedPercent": 42, "windowDurationMins": 300, "resetsAt": 1_800_000_000 },
                "secondary": { "usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1_800_500_000 },
                "credits": { "hasCredits": true, "unlimited": false, "balance": "12.5" }
            },
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "limitName": "Codex",
                    "primary": { "usedPercent": 42, "windowDurationMins": 300, "resetsAt": 1_800_000_000 },
                    "secondary": { "usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1_800_500_000 },
                    "credits": { "hasCredits": true, "unlimited": false, "balance": "12.5" }
                }
            }
        })
    }

    #[test]
    fn rate_limit_buckets_become_percent_samples_with_units_and_windows() {
        let samples = parse_rate_limits(&rate_limits_payload(), "2026-11-03T10:00:00.000Z");
        let primary = samples
            .iter()
            .find(|sample| sample.label.ends_with("primary"))
            .expect("primary window");
        assert_eq!(primary.used_percent, Some(42.0));
        assert_eq!(primary.remaining_percent, Some(58.0));
        assert_eq!(primary.unit, super::super::model::UsageUnit::Percent);
        assert_eq!(primary.window, UsageWindow::Short);
        assert_eq!(primary.window_minutes, Some(300));
        assert!(primary.resets_at.is_some());
        let secondary = samples
            .iter()
            .find(|sample| sample.label.ends_with("secondary"))
            .expect("secondary window");
        assert_eq!(secondary.window, UsageWindow::Weekly);
        let credits = samples
            .iter()
            .find(|sample| sample.metric == super::super::model::UsageMetric::CreditBalance)
            .expect("credits");
        assert_eq!(credits.amount, Some(12.5));
        // Two windows of one bucket must stay two rows, never summed.
        assert_eq!(
            samples
                .iter()
                .filter(|sample| sample.metric == super::super::model::UsageMetric::QuotaWindow)
                .count(),
            2
        );
    }

    #[test]
    fn a_missing_window_or_reset_is_reported_as_missing_not_zero() {
        let payload = json!({
            "rateLimits": {
                "primary": { "usedPercent": 0 },
                "secondary": null
            }
        });
        let samples = parse_rate_limits(&payload, "2026-11-03T10:00:00.000Z");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].used_percent, Some(0.0), "a real 0 is kept");
        assert_eq!(samples[0].resets_at, None, "a missing reset is not invented");
        assert_eq!(samples[0].window, UsageWindow::Unspecified);
        assert_eq!(samples[0].window_minutes, None);
        assert!(samples[0].note.as_deref().unwrap().contains("窗口长度"));

        // A null percentage keeps `None`; it is never rewritten to 0.
        let payload = json!({ "rateLimits": { "primary": { "windowDurationMins": 300 } } });
        let samples = parse_rate_limits(&payload, "2026-11-03T10:00:00.000Z");
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].used_percent, None);
        assert_eq!(samples[0].remaining_percent, None);
    }

    #[test]
    fn an_empty_payload_produces_no_samples_rather_than_a_zero() {
        assert!(parse_rate_limits(&json!({ "rateLimits": {} }), "now").is_empty());
        assert!(parse_rate_limits(&json!({}), "now").is_empty());
        assert!(parse_rate_limits(&json!({ "rateLimits": null }), "now").is_empty());
    }

    #[test]
    fn account_identity_is_confirmed_only_with_a_provider_named_account() {
        let chatgpt = json!({
            "account": { "type": "chatgpt", "email": "alice@example.com", "planType": "plus", "workspaceName": "Team A" },
            "requiresOpenaiAuth": true
        });
        let identity = parse_account(&chatgpt).unwrap().unwrap();
        assert_eq!(identity.status(), IdentityStatus::Confirmed);
        assert_eq!(identity.email_masked.as_deref(), Some("a***@example.com"));
        assert!(!identity.label().contains("alice@example.com"));
        assert!(identity.email_fingerprint.is_some());

        let api_key = json!({ "account": { "type": "apiKey" }, "requiresOpenaiAuth": true });
        let identity = parse_account(&api_key).unwrap().unwrap();
        assert_eq!(
            identity.status(),
            IdentityStatus::Unconfirmed,
            "an API-key account names no account id"
        );

        assert!(parse_account(&json!({ "account": null, "requiresOpenaiAuth": true }))
            .unwrap()
            .is_none());
    }

    #[test]
    fn cached_tokens_are_not_added_twice_for_codex() {
        let usage = json!({
            "threadUsage": {
                "threadId": "t-1",
                "estimatedUsageCreditsMicros": 1200,
                "groups": [{
                    "model": "gpt-5",
                    "inputTokens": 1000,
                    "cachedInputTokens": 400,
                    "outputTokens": 100,
                    "totalTokens": 1100
                }]
            },
            "summary": { "lifetimeTokens": 5000 },
            "dailyUsageBuckets": [ { "startDate": "2026-11-03", "tokens": 900 } ]
        });
        let samples = parse_token_usage(&usage);
        let group = samples
            .iter()
            .find(|sample| sample.window == UsageWindow::Session)
            .unwrap();
        assert_eq!(group.input_tokens, Some(1000));
        assert_eq!(group.cached_input_tokens, Some(400));
        assert_eq!(group.total_tokens, Some(1100));
        assert_eq!(
            group.token_semantics,
            Some(TokenSemantics::InputIncludesCached)
        );
        assert!(
            group.total_tokens.unwrap() < 1000 + 400 + 100,
            "cached tokens must not be added to a total that already contains them"
        );
        assert!(samples.iter().any(|sample| sample.window == UsageWindow::Daily));
        assert!(samples
            .iter()
            .any(|sample| sample.window == UsageWindow::Lifetime));
        // The daily bucket's `tokens` and the summary's `lifetimeTokens` are
        // totals. Filling `inputTokens` with them would fabricate a breakdown
        // the provider never sent.
        for sample in samples
            .iter()
            .filter(|sample| sample.window != UsageWindow::Session)
        {
            assert_eq!(sample.input_tokens, None, "a total is not an input count");
            assert_eq!(sample.cached_input_tokens, None);
            assert_eq!(sample.output_tokens, None);
            assert!(sample.total_tokens.is_some());
            assert!(
                sample.token_semantics.is_none(),
                "a total-only sample declares no component semantics"
            );
        }
    }

    #[test]
    fn a_total_only_usage_read_never_invents_an_input_breakdown() {
        let samples = parse_token_usage(&json!({
            "dailyUsageBuckets": [{ "startDate": "2026-11-03", "tokens": 123 }],
            "summary": { "lifetimeTokens": 456 }
        }));
        assert_eq!(samples.len(), 2);
        assert!(samples.iter().all(|sample| sample.input_tokens.is_none()));
        assert!(samples.iter().all(|sample| sample.total_tokens.is_some()));
    }

    #[test]
    fn a_missing_usage_read_is_partial_not_ok() {
        let reading = CodexReading {
            identity: Some(CodexIdentity {
                account_type: "chatgpt".into(),
                email_masked: Some("a***@example.com".into()),
                email_fingerprint: Some("fingerprint".into()),
                plan: Some("plus".into()),
                workspace: None,
            }),
            rate_limits: Some(rate_limits_payload()),
            usage: None,
            failed_methods: vec!["account/usage/read: timed out".into()],
            transport_error: None,
        };
        let snapshot = reading_to_snapshot(&reading, "pool-1", "2026-11-03T10:00:00.000Z");
        assert_eq!(snapshot.status, UsageStatus::Partial);
        assert!(snapshot.unavailable_fields.contains(&"tokenUsage".to_string()));
        assert!(snapshot.note.unwrap().contains("account/usage/read"));
        assert!(!snapshot.samples.is_empty());
    }

    #[test]
    fn a_read_with_no_values_is_partial_and_never_ok() {
        let reading = CodexReading {
            identity: None,
            rate_limits: Some(json!({ "rateLimits": {} })),
            usage: Some(json!({ "summary": { "lifetimeTokens": null } })),
            failed_methods: Vec::new(),
            transport_error: None,
        };
        let snapshot = reading_to_snapshot(&reading, "pool-1", "2026-11-03T10:00:00.000Z");
        assert_eq!(snapshot.status, UsageStatus::Partial);
        assert!(snapshot.samples.is_empty());
        assert!(snapshot
            .unavailable_fields
            .iter()
            .any(|field| field.contains("values")));
    }

    #[test]
    fn every_failed_read_is_an_error_not_an_empty_partial() {
        let reading = CodexReading {
            failed_methods: vec![
                "account/rateLimits/read: offline".into(),
                "account/usage/read: offline".into(),
            ],
            ..Default::default()
        };
        let snapshot = reading_to_snapshot(&reading, "synthetic-pool", "2026-11-03T00:00:00Z");
        assert_eq!(snapshot.status, UsageStatus::Error);
        assert!(snapshot.samples.is_empty());
        let error = snapshot.error.expect("a real error");
        let methods = error.details.get("methods").and_then(Value::as_array).unwrap();
        assert_eq!(methods.len(), 2, "each method keeps its own record");
    }

    #[test]
    fn a_quota_429_is_not_hidden_by_a_successful_token_read() {
        let reading = CodexReading {
            rate_limits: None,
            usage: Some(json!({ "summary": { "lifetimeTokens": 12 } })),
            failed_methods: vec![MethodFailure {
                method: "account/rateLimits/read".into(),
                kind: "rate-limited".into(),
                message: "rate limited".into(),
                rpc_code: Some(429),
                retry_after_ms: Some(120_000),
            }],
            ..Default::default()
        };
        let snapshot = reading_to_snapshot(&reading, "synthetic-pool", "2026-11-03T00:00:00Z");
        assert_eq!(snapshot.status, UsageStatus::RateLimited);
        assert_eq!(
            snapshot.retry_after_ms,
            Some(120_000),
            "429 must keep its structured retry hint for provider backoff"
        );
        assert!(
            !snapshot.samples.is_empty(),
            "the successful token read is kept"
        );
        let error = snapshot.error.expect("the 429 is still an error");
        assert_eq!(error.details["kind"], "rate-limited");
        assert_eq!(
            error.details["methods"][0]["retryAfterMs"],
            json!(120_000),
            "per-method detail is structured, not a free-form string"
        );
        assert!(snapshot
            .unavailable_fields
            .iter()
            .any(|field| field.starts_with("account/rateLimits/read")));
    }

    #[test]
    fn a_transport_failure_keeps_what_was_read_before_it() {
        let reading = CodexReading {
            identity: Some(CodexIdentity {
                account_type: "chatgpt".into(),
                email_masked: Some("a***@example.com".into()),
                email_fingerprint: Some("fingerprint".into()),
                plan: Some("plus".into()),
                workspace: None,
            }),
            rate_limits: Some(rate_limits_payload()),
            usage: None,
            failed_methods: vec![MethodFailure {
                method: "account/usage/read".into(),
                kind: "transport".into(),
                message: "timed out".into(),
                rpc_code: None,
                retry_after_ms: None,
            }],
            transport_error: Some(AppError::new(ErrorCode::Timeout, "timed out")),
        };
        let snapshot = reading_to_snapshot(&reading, "synthetic-pool", "2026-11-03T00:00:00Z");
        assert_eq!(snapshot.status, UsageStatus::Partial);
        assert!(!snapshot.samples.is_empty(), "the quota read is kept");
        assert!(snapshot.error.is_some());
    }

    #[test]
    fn an_api_key_account_is_not_promoted_to_a_confirmed_profile() {
        let reading = CodexReading {
            identity: Some(CodexIdentity {
                account_type: "apiKey".into(),
                email_masked: None,
                email_fingerprint: None,
                plan: None,
                workspace: None,
            }),
            rate_limits: None,
            usage: None,
            failed_methods: Vec::new(),
            transport_error: None,
        };
        assert!(
            account_from_reading(&reading, "local-windows").is_none(),
            "an unnamed account must stay installation-scoped"
        );
    }
}
