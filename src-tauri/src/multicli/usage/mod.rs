//! Unified usage centre: which provider can be read how, and what was read.
//!
//! The centre is the only place that decides three things:
//!
//! 1. **Which provider has a real machine-readable path.** Codex has one and is
//!    read through `codex app-server`. Claude has real token counts in its local
//!    transcripts but no readable balance. CodeBuddy and the extension families
//!    have neither, so they are recorded as `unsupported` together with the
//!    official command the *user* can run — never as `0`.
//! 2. **When to retry.** A retryable failure stores an exponential backoff (a
//!    provider `429` hint wins) and the next refresh reports `skipped` instead
//!    of hammering the CLI.
//! 3. **How accounts are de-duplicated.** A confirmed account has exactly one
//!    pool; an unconfirmed identity is pinned to its installation and is never
//!    summed with anything else.

pub mod codex;
pub mod model;
pub(crate) mod repo;
pub mod transcript;

#[cfg(test)]
mod tests;

pub use model::{
    AccountProfile, IdentityStatus, PoolView, ProviderCatalogEntry, QuotaPool, RefreshOutcome,
    UsageAccountView, UsageMetric, UsageOverviewView, UsageRefreshRequest, UsageRefreshResult,
    UsageSample, UsageSnapshot, UsageSourceKind, UsageStatus, UsageUnit, UsageWindow,
    OVERVIEW_NOTE, USAGE_SCHEMA_VERSION,
};

use std::path::PathBuf;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::multicli::discovery::detect::{scan_installations, InstallStatus, ScannerEnvironment};
use crate::multicli::terminal::launch::{cleanup_trampoline, plan_launch, LaunchPaths, PinnedTarget};
use crate::multicli::terminal::model::{LaunchChain, TerminalTargetKind};
use crate::multicli::discovery::signature::{normalize_key, ProductKind, SignatureRegistry};
use crate::multicli::error::{AppError, ErrorCode};
use crate::multicli::service::MultiCliService;

/// Static description of one product's usage surface. This is *data*, so the
/// UI and the tests read the same claim.
#[derive(Debug, Clone, Copy)]
pub struct ProviderSpec {
    pub agent_id: &'static str,
    pub product: &'static str,
    /// `machine | transcript | manual | none`.
    pub quota_access: &'static str,
    pub token_access: &'static str,
    /// A command **the user** may run; the product never injects it into a
    /// working session.
    pub official_entry: Option<&'static str>,
    pub evidence: &'static str,
    pub limitation: Option<&'static str>,
    pub kind: ProductKind,
}

pub const PROVIDERS: &[ProviderSpec] = &[
    ProviderSpec {
        agent_id: "codex",
        product: "Codex CLI",
        quota_access: "machine",
        token_access: "machine",
        official_entry: Some("codex app-server → account/rateLimits/read"),
        evidence: "Installed 0.166.0 app-server schema: account/read, \
                   account/rateLimits/read, account/usage/read. Read-only; login, logout, \
                   reset-credit consumption, credit purchase and mail are never called.",
        limitation: Some(
            "只读取当前 CLI 已有认证范围内的数据；不触发登录、加额、充值或邮件。窗口未报告时不推断。",
        ),
        kind: ProductKind::Core,
    },
    ProviderSpec {
        agent_id: "claude",
        product: "Claude Code",
        quota_access: "none",
        token_access: "transcript",
        official_entry: Some("/cost"),
        evidence: "Local transcripts written by the CLI contain message.usage with \
                   input_tokens / cache_creation_input_tokens / cache_read_input_tokens / \
                   output_tokens. Counted read-only, text discarded, records de-duplicated.",
        limitation: Some(
            "没有已验证的机器可读余额接口；余额不可读取。`/cost` 需用户在自己的会话中手动执行，工作台不注入。",
        ),
        kind: ProductKind::Core,
    },
    ProviderSpec {
        agent_id: "codebuddy",
        product: "CodeBuddy Code",
        quota_access: "none",
        token_access: "none",
        official_entry: Some("/cost"),
        evidence: "No verified machine-readable quota or token endpoint was found for 2.179.3. \
                   The official cost command is a human slash command inside a session.",
        limitation: Some(
            "额度与 token 均不可机器读取；`/cost` 只能由用户手动执行。不抓取会话 cookie 或浏览器存储。",
        ),
        kind: ProductKind::Core,
    },
    ProviderSpec {
        agent_id: "gemini",
        product: "Gemini CLI",
        quota_access: "none",
        token_access: "none",
        official_entry: Some("/stats model"),
        evidence: "The official commands reference documents /stats model output; no stable \
                   machine-readable balance endpoint is documented for the CLI.",
        limitation: Some("未验证机器可读出口；`/stats model` 需用户手动执行。"),
        kind: ProductKind::Extension,
    },
    ProviderSpec {
        agent_id: "opencode",
        product: "OpenCode",
        quota_access: "none",
        token_access: "none",
        official_entry: None,
        evidence: "No documented machine-readable account/quota endpoint was found.",
        limitation: Some("额度不可读取；该产品未提供官方用量命令。"),
        kind: ProductKind::Extension,
    },
    ProviderSpec {
        agent_id: "qwen",
        product: "Qwen Code",
        quota_access: "none",
        token_access: "none",
        official_entry: Some("/stats"),
        evidence: "The CLI documents a /stats command; no machine-readable quota endpoint was \
                   verified.",
        limitation: Some("未验证机器可读出口；`/stats` 需用户手动执行。"),
        kind: ProductKind::Extension,
    },
    ProviderSpec {
        agent_id: "copilot",
        product: "GitHub Copilot CLI",
        quota_access: "none",
        token_access: "none",
        official_entry: None,
        evidence: "GitHub documents CLI setup and authentication only; no local quota endpoint \
                   is documented.",
        limitation: Some("额度与 token 不可读取；订阅额度以 GitHub 账户页面为准。"),
        kind: ProductKind::Extension,
    },
    ProviderSpec {
        agent_id: "kimi",
        product: "Kimi Code",
        quota_access: "none",
        token_access: "none",
        official_entry: None,
        evidence: "No documented machine-readable quota endpoint was found for kimi-code.",
        limitation: Some("额度不可读取；旧 kimi-cli 与新 kimi-code 同名，不互相继承能力。"),
        kind: ProductKind::Extension,
    },
    ProviderSpec {
        agent_id: "aider",
        product: "Aider",
        quota_access: "none",
        token_access: "none",
        official_entry: Some("/tokens"),
        evidence: "Aider reports per-session token cost through its /tokens command; the CLI \
                   does not expose a stable non-interactive usage endpoint.",
        limitation: Some("未验证机器可读出口；`/tokens` 需用户手动执行。"),
        kind: ProductKind::Extension,
    },
    ProviderSpec {
        agent_id: "trae",
        product: "TRAE CLI",
        quota_access: "none",
        token_access: "none",
        official_entry: None,
        evidence: "Observed-only family: traex.exe 0.209.1 exists on this host; no official CLI \
                   documentation or usage endpoint was found.",
        limitation: Some("仅本机观测；无官方用量接口，额度不可读取。"),
        kind: ProductKind::ObservedOnly,
    },
];

pub fn provider_for(agent_id: &str) -> Option<&'static ProviderSpec> {
    PROVIDERS.iter().find(|spec| spec.agent_id == agent_id)
}

pub struct UsageCenter {
    host_id: String,
    service: Arc<MultiCliService>,
    data_root: PathBuf,
    /// One refresh at a time: two concurrent `codex app-server` probes would
    /// double the CLI's startup cost and could interleave their reads.
    gate: Mutex<()>,
}

impl UsageCenter {
    pub fn new(host_id: &str, service: Arc<MultiCliService>, data_root: PathBuf) -> Self {
        Self {
            host_id: host_id.to_string(),
            service,
            data_root,
            gate: Mutex::new(()),
        }
    }

    // -----------------------------------------------------------------------
    // Overview
    // -----------------------------------------------------------------------

    pub fn overview(&self) -> Result<UsageOverviewView, AppError> {
        // Installations come from the service, which takes the same connection
        // guard; read them *before* locking, otherwise a non-reentrant mutex
        // would deadlock this call against itself.
        let installations = self.service.list_all_installations(&self.host_id)?;
        let installed = self.installed_executables();
        let store = self.service.store();
        let conn = store.connection().lock();
        let accounts = repo::all_accounts(&conn, &self.host_id)?;
        let pools = repo::all_pools(&conn, &self.host_id)?;
        let states = repo::all_provider_states(&conn)?;
        let mut pool_views = Vec::new();
        for pool in pools {
            let snapshot = repo::latest_snapshot(&conn, &pool.id)?
                .unwrap_or_else(|| UsageSnapshot::empty(&pool.id, &pool.agent_id));
            let last_good = repo::last_ok_snapshot(&conn, &pool.id)?;
            let installation_ids = repo::installation_ids_for_pool(&conn, &pool.id)?;
            pool_views.push(PoolView {
                pool,
                snapshot,
                last_good,
                installation_ids,
            });
        }
        drop(conn);
        let mut account_views = Vec::new();
        for account in accounts {
            let matching: Vec<PoolView> = pool_views
                .iter()
                .filter(|view| view.pool.account_profile_id.as_deref() == Some(account.id.as_str()))
                .cloned()
                .collect();
            account_views.push(UsageAccountView {
                account,
                pools: matching,
            });
        }
        let unconfirmed_pools: Vec<PoolView> = pool_views
            .into_iter()
            .filter(|view| {
                view.pool.identity_status != IdentityStatus::Confirmed
                    && view.pool.account_profile_id.is_none()
            })
            .collect();
        let last_attempt_at = states
            .iter()
            .filter_map(|state| state.last_attempt_at.clone())
            .max();
        let last_success_at = states
            .iter()
            .filter_map(|state| state.last_success_at.clone())
            .max();
        let catalog = PROVIDERS
            .iter()
            .map(|spec| {
                // A registered row is not enough: an IDE shim or a residue
                // directory also produces one. Only a discovery result with the
                // `installed` status may be presented as installed here.
                let is_installed = installations
                    .iter()
                    .filter(|installation| installation.agent_id == spec.agent_id)
                    .any(|installation| {
                        installation
                            .executable
                            .canonical_path
                            .as_deref()
                            .map(normalize_key)
                            .is_some_and(|path| installed.contains(&path))
                    });
                ProviderCatalogEntry {
                    agent_id: spec.agent_id.to_string(),
                    product: spec.product.to_string(),
                    quota_access: spec.quota_access.to_string(),
                    token_access: spec.token_access.to_string(),
                    official_entry: spec.official_entry.map(str::to_string),
                    status: catalog_status(spec, is_installed),
                    installed: is_installed,
                    evidence: spec.evidence.to_string(),
                    limitation: spec.limitation.map(str::to_string),
                }
            })
            .collect();
        Ok(UsageOverviewView {
            schema_version: USAGE_SCHEMA_VERSION,
            generated_at: model::now_iso8601(),
            accounts: account_views,
            unconfirmed_pools,
            catalog,
            last_attempt_at,
            last_success_at,
            note: OVERVIEW_NOTE.to_string(),
        })
    }

    // -----------------------------------------------------------------------
    // Refresh
    // -----------------------------------------------------------------------

    pub fn refresh(&self, request: &UsageRefreshRequest) -> Result<UsageRefreshResult, AppError> {
        request.validate()?;
        let _gate = self.gate.lock();
        let installed = self.installed_executables();
        let mut outcomes = Vec::new();
        for spec in PROVIDERS {
            if let Some(agent_id) = &request.agent_id {
                if spec.agent_id != agent_id {
                    continue;
                }
            }
            let outcome = match self.refresh_one(spec, request.force, &installed) {
                Ok(outcome) => outcome,
                Err(error) => RefreshOutcome {
                    agent_id: spec.agent_id.to_string(),
                    status: UsageStatus::Error,
                    skipped: false,
                    // The other providers still refresh; this one reports its
                    // real error instead of aborting the whole call.
                    reason: Some(format!("{}: {}", error.code.as_str(), error.message)),
                    retry_after_ms: error.details.get("retryAfterMs").and_then(|v| v.as_i64()),
                },
            };
            outcomes.push(outcome);
        }
        let overview = self.overview()?;
        Ok(UsageRefreshResult { overview, outcomes })
    }

    fn refresh_one(
        &self,
        spec: &'static ProviderSpec,
        force: bool,
        installed: &std::collections::BTreeSet<String>,
    ) -> Result<RefreshOutcome, AppError> {
        let installation = self.installation_for(spec.agent_id, installed);
        if installation.is_none() {
            return Ok(RefreshOutcome {
                agent_id: spec.agent_id.to_string(),
                status: UsageStatus::Unsupported,
                skipped: true,
                reason: Some("本机未注册该产品的安装；不创建空额度池".to_string()),
                retry_after_ms: None,
            });
        }
        let installation = installation.expect("checked");
        if !force {
            let conn = self.service.store().connection().lock();
            let state = repo::provider_state(&conn, spec.agent_id)?;
            drop(conn);
            if let Some(backoff) = state.backoff_until.as_deref() {
                let now = crate::multicli::store::now_ms();
                if let Some(deadline) = model::parse_timestamp(backoff) {
                    if deadline > now {
                        let last_status = state.last_status.as_deref().unwrap_or("unknown");
                        return Ok(RefreshOutcome {
                            agent_id: spec.agent_id.to_string(),
                            // Report what was actually recorded; a skipped
                            // attempt is not a new 429 by itself.
                            status: UsageStatus::parse(last_status)
                                .unwrap_or(UsageStatus::Unknown),
                            skipped: true,
                            reason: Some(format!(
                                "退避中：下次尝试 {backoff}（上次：{last_status}）"
                            )),
                            retry_after_ms: Some(deadline - now),
                        });
                    }
                }
            }
        }

        let mut outcome = match spec.agent_id {
            "codex" => self.refresh_codex(spec, &installation, &installed)?,
            "claude" => self.refresh_transcript(spec, &installation)?,
            _ => self.refresh_unavailable(spec, &installation)?,
        };
        outcome.agent_id = spec.agent_id.to_string();
        self.declare_capability(spec, &outcome.status);
        Ok(outcome)
    }

    /// Canonical executables of installations that the discovery registry
    /// itself classified as `installed`. A registered row alone is not enough:
    /// an IDE shim and a residue directory also produce rows, and neither is a
    /// usable product installation.
    fn installed_executables(&self) -> std::collections::BTreeSet<String> {
        let registry = SignatureRegistry::standard();
        let environment = ScannerEnvironment::from_process_env();
        let scan = scan_installations(&registry, &environment, &self.host_id);
        scan.installations
            .iter()
            .filter(|item| item.status == InstallStatus::Installed)
            .filter_map(|item| item.executable.as_ref())
            .filter_map(|path| path.canonical_path.as_deref())
            .map(normalize_key)
            .collect()
    }

    fn installation_for(
        &self,
        agent_id: &str,
        installed: &std::collections::BTreeSet<String>,
    ) -> Option<crate::multicli::AgentInstallation> {
        let installations = self.service.list_all_installations(&self.host_id).ok()?;
        installations
            .into_iter()
            .filter(|installation| installation.agent_id == agent_id)
            .find(|installation| {
                let Some(canonical) = installation.executable.canonical_path.as_deref() else {
                    return false;
                };
                installed.contains(&normalize_key(canonical))
                    && PathBuf::from(canonical).is_file()
            })
    }

    fn codex_program(&self, installed: &std::collections::BTreeSet<String>) -> Option<PathBuf> {
        if let Some(installation) = self.installation_for("codex", installed) {
            let path = PathBuf::from(
                installation
                    .executable
                    .canonical_path
                    .clone()
                    .unwrap_or_else(|| installation.executable.display_path.clone()),
            );
            if path.is_file() {
                return Some(path);
            }
        }
        which::which("codex").ok().filter(|path| path.is_file())
    }

    fn cwd(&self) -> PathBuf {
        dirs::home_dir()
            .filter(|path| path.is_dir())
            .unwrap_or_else(|| self.data_root.clone())
    }

    fn refresh_codex(
        &self,
        spec: &'static ProviderSpec,
        installation: &crate::multicli::AgentInstallation,
        installed: &std::collections::BTreeSet<String>,
    ) -> Result<RefreshOutcome, AppError> {
        let Some(program) = self.codex_program(installed) else {
            let snapshot = self.failure_snapshot(
                spec,
                installation,
                UsageStatus::Unsupported,
                AppError::new(ErrorCode::Unsupported, "codex executable was not found")
                    .with_details(serde_json::json!({ "stage": "resolveCodex" })),
            )?;
            return Ok(self.outcome_from(snapshot, spec, installation, None));
        };
        let cwd = self.cwd();
        // A `.cmd` installation (npm shim) cannot be started as an image: the
        // S04 plan picks the chain and the trampoline, exactly like a terminal.
        let chain = match LaunchChain::for_extension(&program) {
            Some(chain) => chain,
            None => {
                let snapshot = self.failure_snapshot(
                    spec,
                    installation,
                    UsageStatus::Unsupported,
                    AppError::new(
                        ErrorCode::Unsupported,
                        "the codex executable extension has no implemented launch chain",
                    )
                    .with_details(serde_json::json!({ "stage": "resolveCodexChain" })),
                )?;
                return Ok(self.outcome_from(snapshot, spec, installation, None));
            }
        };
        let target = PinnedTarget {
            kind: TerminalTargetKind::Installation,
            label: "codex usage read".into(),
            product: Some(spec.product.to_string()),
            agent_id: spec.agent_id.to_string(),
            installation_id: Some(installation.id.clone()),
            executable: program.clone(),
            chain,
            shell_flavor: None,
            version: None,
            preset_args: Vec::new(),
        };
        let launch_paths = LaunchPaths::resolve(&self.data_root)?;
        let plan = plan_launch(&target, &["app-server".to_string()], "usage-codex", &launch_paths)?;
        let reading = match codex::read(&plan.program, &plan.program_args, &cwd, &plan.launcher_env) {
            Ok(reading) => {
                // The generated trampoline is inert once the process started.
                let _ = cleanup_trampoline(&launch_paths, "usage-codex");
                reading
            }
            Err(error) => {
                let _ = cleanup_trampoline(&launch_paths, "usage-codex");
                let (status, retry_after) = classify_failure(&error);
                let snapshot = self.failure_snapshot_with_retry(
                    spec,
                    installation,
                    status,
                    error.clone(),
                    retry_after,
                )?;
                return Ok(self.outcome_from(snapshot, spec, installation, Some(&error)));
            }
        };
        if reading.identity.is_none() {
            // The CLI answered but names no account: that is "not signed in",
            // a distinct state, not an empty account with zero usage.
            let snapshot = self.failure_snapshot(
                spec,
                installation,
                UsageStatus::Unauthenticated,
                AppError::new(
                    ErrorCode::Unsupported,
                    "codex app-server reported no account for this installation",
                )
                .with_details(serde_json::json!({ "stage": "codexAccount", "source": "account/read" })),
            )?;
            return Ok(self.outcome_from(snapshot, spec, installation, None));
        }
        let account = codex::account_from_reading(&reading, &self.host_id);
        let label = account
            .as_ref()
            .map(|value| value.label.clone())
            .or_else(|| reading.identity.as_ref().map(|value| value.label()))
            .unwrap_or_else(|| spec.product.to_string());
        let pool = self.ensure_pool(spec, installation, account.as_ref(), label)?;
        let snapshot = codex::reading_to_snapshot(&reading, &pool.id, &model::now_iso8601());
        let snapshot = self.persist_snapshot(spec, snapshot)?;
        Ok(RefreshOutcome {
            agent_id: spec.agent_id.to_string(),
            status: snapshot.status,
            skipped: false,
            reason: snapshot.note.clone(),
            retry_after_ms: snapshot.retry_after_ms,
        })
    }

    fn refresh_transcript(
        &self,
        spec: &'static ProviderSpec,
        installation: &crate::multicli::AgentInstallation,
    ) -> Result<RefreshOutcome, AppError> {
        let roots = {
            let conn = self.service.store().connection().lock();
            repo::enabled_roots(&conn, spec.agent_id)?
        };
        if roots.is_empty() {
            let snapshot = self.failure_snapshot(
                spec,
                installation,
                UsageStatus::Unsupported,
                AppError::new(
                    ErrorCode::Unsupported,
                    "no enabled history root is configured for this product",
                )
                .with_details(serde_json::json!({
                    "stage": "usageRoots",
                    "hint": "在「历史」页启用该产品的只读来源根后，才能从本地转录统计 token",
                })),
            )?;
            return Ok(self.outcome_from(snapshot, spec, installation, None));
        }
        let paths: Vec<PathBuf> = roots.iter().map(|(_, path)| PathBuf::from(path)).collect();
        let totals = transcript::scan_roots(&paths);
        let now = model::now_iso8601();
        // Split instead of slicing: the timestamp format is not a slicing
        // contract, and a changed spelling must not panic the refresh thread.
        let today = now.split('T').next().unwrap_or(now.as_str());
        let source = format!(
            "local transcript · {} root · {} files",
            paths.len(),
            totals.files_read
        );
        // A root that exists but produced no transcript is not "0 tokens":
        // there is simply nothing to measure yet. The same holds when files
        // were read but no assistant record carried a usage object — the shape
        // may be unknown, so no number is presented at all.
        let samples = if totals.files_read == 0 || totals.records == 0 {
            Vec::new()
        } else {
            totals.samples(today, &source)
        };
        let has_value = samples.iter().any(|sample| {
            sample.total_tokens.is_some()
                || sample.input_tokens.is_some()
                || sample.output_tokens.is_some()
                || sample.cached_input_tokens.is_some()
                || sample.cache_creation_tokens.is_some()
        });
        let mut snapshot = UsageSnapshot::empty("pending", spec.agent_id);
        snapshot.source_kind = UsageSourceKind::LocalTranscript;
        snapshot.source = source;
        snapshot.captured_at = now;
        snapshot.expires_at = Some(model::iso_after_ms(model::DEFAULT_TTL_MS));
        snapshot.samples = samples;
        // An incomplete scan is `partial` even when it produced numbers: the
        // missing part is unknown, so no window/lifetime total is presented.
        snapshot.status = if totals.files_read == 0 {
            if totals.files_failed > 0 {
                UsageStatus::Error
            } else {
                UsageStatus::Unsupported
            }
        } else if !totals.complete() {
            UsageStatus::Partial
        } else if totals.records == 0 {
            // Files were read but no assistant record carried a usage object.
            // That is not evidence of "0 tokens": the shape may be unknown, so
            // no total is presented.
            UsageStatus::Partial
        } else if has_value && !totals.days.contains_key("unknown") {
            UsageStatus::Ok
        } else {
            UsageStatus::Partial
        };
        if snapshot.status == UsageStatus::Error {
            snapshot.error = Some(
                AppError::new(
                    ErrorCode::Io,
                    "no configured transcript file could be read",
                )
                .with_details(serde_json::json!({
                    "stage": "usageTranscript",
                    "roots": paths.len(),
                    "failed": totals.files_failed,
                })),
            );
        }
        snapshot
            .unavailable_fields
            .extend(totals.incomplete_reasons());
        if totals.records == 0 && totals.files_read > 0 {
            snapshot.unavailable_fields.push(
                "values（已读到的文件里没有带 usage 的 assistant 记录）".to_string(),
            );
        }
        if !has_value && totals.files_read > 0 && totals.records > 0 {
            snapshot
                .unavailable_fields
                .push("values（已读到的记录没有报告可用数值）".to_string());
        }
        snapshot.note = Some(format!(
            "只读本地转录：{} 个文件、{} 条记录{}；正文不落库、不进搜索索引。余额不可读取，官方入口 /cost 需手动执行。",
            totals.files_read,
            totals.records,
            if totals.complete() {
                ""
            } else {
                "（扫描不完整，窗口与累计值不显示）"
            }
        ));
        let pool = self.ensure_pool(spec, installation, None, spec.product.to_string())?;
        snapshot.pool_id = pool.id.clone();
        let snapshot = self.persist_snapshot(spec, snapshot)?;
        Ok(RefreshOutcome {
            agent_id: spec.agent_id.to_string(),
            status: snapshot.status,
            skipped: false,
            reason: snapshot.note.clone(),
            retry_after_ms: snapshot.retry_after_ms,
        })
    }

    fn refresh_unavailable(
        &self,
        spec: &'static ProviderSpec,
        installation: &crate::multicli::AgentInstallation,
    ) -> Result<RefreshOutcome, AppError> {
        // Built here rather than through `failure_snapshot`: that helper writes
        // the row it returns, and writing it twice would collide on the id.
        let pool = self.ensure_pool(spec, installation, None, spec.product.to_string())?;
        let mut snapshot = UsageSnapshot::empty(&pool.id, spec.agent_id);
        snapshot.status = UsageStatus::Unsupported;
        snapshot.source_kind = UsageSourceKind::Unavailable;
        snapshot.source = format!("{} · no machine-readable source", spec.product);
        snapshot.captured_at = model::now_iso8601();
        snapshot.expires_at = Some(model::iso_after_ms(model::DEFAULT_TTL_MS));
        snapshot.unavailable_fields = vec!["quota".to_string(), "tokens".to_string()];
        snapshot.error = Some(
            AppError::new(
                ErrorCode::Unsupported,
                "this product has no verified machine-readable usage source",
            )
            .with_details(serde_json::json!({
                "stage": "usageSource",
                "officialEntry": spec.official_entry,
            })),
        );
        snapshot.note = Some(match spec.official_entry {
            Some(entry) => format!(
                "额度不可机器读取；官方入口 `{entry}` 需用户在自己的会话中手动执行，工作台不注入斜杠命令。"
            ),
            None => "额度不可机器读取；该产品未提供官方用量命令。".to_string(),
        });
        {
            let mut conn = self.service.store().connection().lock();
            repo::insert_snapshot(&mut conn, &snapshot)?;
            repo::record_neutral_attempt(&conn, spec.agent_id, UsageStatus::Unsupported)?;
        }
        Ok(RefreshOutcome {
            agent_id: spec.agent_id.to_string(),
            status: UsageStatus::Unsupported,
            skipped: false,
            reason: snapshot.note.clone(),
            retry_after_ms: None,
        })
    }

    fn ensure_pool(
        &self,
        spec: &'static ProviderSpec,
        installation: &crate::multicli::AgentInstallation,
        account: Option<&AccountProfile>,
        label: String,
    ) -> Result<QuotaPool, AppError> {
        let identity_status = account
            .map(|value| value.identity_status)
            .unwrap_or(IdentityStatus::Unconfirmed);
        let fingerprint = account.and_then(|value| value.identity_fingerprint.clone());
        let pool_key = QuotaPool::key_for(
            spec.agent_id,
            identity_status,
            fingerprint.as_deref(),
            Some(&installation.id),
        );
        let conn = self.service.store().connection().lock();
        if account.is_none() {
            // A failed read does not erase the account: keep writing on the
            // confirmed pool of this installation instead of opening a second,
            // unconfirmed one that would look like a different account.
            if let Some(existing) = repo::confirmed_pool_for_installation(
                &conn,
                &self.host_id,
                spec.agent_id,
                &installation.id,
            )? {
                return Ok(existing);
            }
        }
        let stored_account = match account {
            Some(account) => Some(repo::upsert_account(&conn, account)?),
            None => None,
        };
        if stored_account.is_some() {
            // The installation now belongs to a confirmed account; the earlier
            // unconfirmed row for the same installation is superseded.
            let _ = repo::delete_unconfirmed_pool_for_installation(
                &conn,
                &self.host_id,
                spec.agent_id,
                &installation.id,
            );
        }
        let pool = QuotaPool {
            id: format!("pool-{}", uuid::Uuid::new_v4().simple()),
            pool_key,
            host_id: self.host_id.clone(),
            agent_id: spec.agent_id.to_string(),
            product: spec.product.to_string(),
            account_profile_id: stored_account.as_ref().map(|value| value.id.clone()),
            identity_status,
            label,
            installation_count: 0,
            created_at: model::now_iso8601(),
            updated_at: model::now_iso8601(),
        };
        let stored = repo::upsert_pool(&conn, &pool, Some(&installation.id))?;
        drop(conn);
        Ok(stored)
    }

    fn failure_snapshot(
        &self,
        spec: &'static ProviderSpec,
        installation: &crate::multicli::AgentInstallation,
        status: UsageStatus,
        error: AppError,
    ) -> Result<UsageSnapshot, AppError> {
        self.failure_snapshot_with_retry(spec, installation, status, error, None)
    }

    fn failure_snapshot_with_retry(
        &self,
        spec: &'static ProviderSpec,
        installation: &crate::multicli::AgentInstallation,
        status: UsageStatus,
        error: AppError,
        retry_after_ms: Option<i64>,
    ) -> Result<UsageSnapshot, AppError> {
        let pool = self.ensure_pool(spec, installation, None, spec.product.to_string())?;
        let mut snapshot = UsageSnapshot::empty(&pool.id, spec.agent_id);
        snapshot.status = status;
        snapshot.source_kind = match spec.quota_access {
            "machine" => UsageSourceKind::OfficialMachine,
            "transcript" => UsageSourceKind::LocalTranscript,
            "manual" => UsageSourceKind::ManualEntry,
            _ => UsageSourceKind::Unavailable,
        };
        snapshot.source = format!("{} · {}", spec.product, spec.quota_access);
        snapshot.captured_at = model::now_iso8601();
        snapshot.expires_at = Some(model::iso_after_ms(model::DEFAULT_TTL_MS));
        snapshot.retry_after_ms = retry_after_ms;
        snapshot.samples = Vec::new();
        snapshot.note = Some(
            "本次未取得数值：不显示缓存数字，避免把旧读数当成当前状态。".to_string(),
        );
        snapshot.error = Some(error.clone());
        self.persist_snapshot(spec, snapshot)
    }

    /// Stores one snapshot and records the matching provider attempt.
    ///
    /// The backoff rule lives here so no caller can forget it: a retryable
    /// status always writes a backoff derived from the provider hint (if any)
    /// and the attempt count, **even when another method of the same reading
    /// succeeded**. A token success must never erase a quota 429.
    fn persist_snapshot(
        &self,
        spec: &'static ProviderSpec,
        mut snapshot: UsageSnapshot,
    ) -> Result<UsageSnapshot, AppError> {
        let retryable = matches!(
            snapshot.status,
            UsageStatus::Error | UsageStatus::RateLimited
        );
        if retryable {
            let attempts = {
                let conn = self.service.store().connection().lock();
                let state = repo::provider_state(&conn, spec.agent_id)?;
                state.attempts.saturating_add(1)
            };
            snapshot.attempts = attempts;
            snapshot.next_attempt_at =
                Some(model::iso_after_ms(model::backoff_ms(attempts, snapshot.retry_after_ms)));
        }
        let measured = snapshot.status.is_measured()
            && snapshot.samples.iter().any(UsageSample::has_numeric_value);
        let backoff = if retryable {
            snapshot.next_attempt_at.clone()
        } else {
            None
        };
        let mut conn = self.service.store().connection().lock();
        repo::insert_snapshot(&mut conn, &snapshot)?;
        if retryable {
            repo::record_provider_attempt(
                &conn,
                spec.agent_id,
                snapshot.status,
                false,
                backoff.as_deref(),
                snapshot.error.as_ref(),
            )?;
        } else if measured {
            repo::record_provider_attempt(&conn, spec.agent_id, snapshot.status, true, None, None)?;
        } else {
            // Not measured and not retryable (`unauthenticated`, `unsupported`,
            // or an honest partial with no numbers): the attempt is recorded so
            // the user sees when it happened, but it neither clears nor grows a
            // backoff it never earned.
            repo::record_neutral_attempt(&conn, spec.agent_id, snapshot.status)?;
        }
        drop(conn);
        Ok(snapshot)
    }

    fn outcome_from(
        &self,
        snapshot: UsageSnapshot,
        _spec: &'static ProviderSpec,
        _installation: &crate::multicli::AgentInstallation,
        error: Option<&AppError>,
    ) -> RefreshOutcome {
        RefreshOutcome {
            agent_id: snapshot.agent_id.clone(),
            status: snapshot.status,
            skipped: false,
            reason: snapshot
                .note
                .clone()
                .or_else(|| error.map(|value| value.message.clone())),
            retry_after_ms: snapshot.retry_after_ms,
        }
    }

    /// Writes the `quotaRead` capability row that the existing capability view
    /// already renders. `live` is used only after a real machine read produced
    /// numbers; everything else stays `document`.
    fn declare_capability(&self, spec: &'static ProviderSpec, status: &UsageStatus) {
        let registry = SignatureRegistry::standard();
        let vendor = registry
            .by_agent(spec.agent_id)
            .map(|signature| signature.vendor)
            .unwrap_or("unknown");
        let _ = self.service.register_agent(&crate::multicli::AgentDefinition {
            id: spec.agent_id.to_string(),
            vendor: vendor.to_string(),
            display_name: spec.product.to_string(),
        });
        let measured = *status == UsageStatus::Ok && spec.quota_access == "machine";
        if !measured && spec.quota_access != "none" {
            // A failed or partial read says nothing about the product's
            // capability: keep the previous row instead of downgrading it.
            return;
        }
        let capability_status = if measured {
            crate::multicli::CapabilityStatus::Supported
        } else {
            crate::multicli::CapabilityStatus::Unsupported
        };
        let _ = self.service.declare_capability(&crate::multicli::Capability {
            agent_id: spec.agent_id.to_string(),
            installation_id: None,
            name: crate::multicli::CapabilityName::QuotaRead,
            status: capability_status,
            version_range: None,
            evidence_level: if measured {
                crate::multicli::CapabilityEvidenceLevel::Live
            } else {
                crate::multicli::CapabilityEvidenceLevel::Document
            },
            source: "S06 usage catalogue".to_string(),
            verified_at: measured.then(model::now_iso8601),
            reason: spec.limitation.map(str::to_string),
        });
    }
}

fn catalog_status(spec: &'static ProviderSpec, installed: bool) -> String {
    if !installed {
        return "unsupported".to_string();
    }
    if spec.quota_access == "none" && spec.token_access == "none" {
        // Nothing at all can be read for this product, installed or not.
        return "unsupported".to_string();
    }
    match spec.agent_id {
        "codex" | "claude" => "supported".to_string(),
        _ => "unverified".to_string(),
    }
}

/// Maps a read failure onto the visible state and the provider's own retry
/// hint. A 429 is never shown as a generic error and never as zero usage.
fn classify_failure(error: &AppError) -> (UsageStatus, Option<i64>) {
    if codex::is_authentication(error) {
        return (UsageStatus::Unauthenticated, None);
    }
    if codex::is_rate_limited(error) {
        return (UsageStatus::RateLimited, codex::retry_after_ms(error));
    }
    (UsageStatus::Error, None)
}

#[cfg(test)]
mod catalogue_tests {
    use super::*;

    #[test]
    fn every_installed_family_has_a_catalogue_entry_and_no_duplicate() {
        let registry = SignatureRegistry::standard();
        let mut listed: Vec<&str> = PROVIDERS.iter().map(|spec| spec.agent_id).collect();
        listed.sort_unstable();
        let mut signatures: Vec<&str> = registry
            .all()
            .iter()
            .map(|signature| signature.agent_id)
            .collect();
        signatures.sort_unstable();
        assert_eq!(listed, signatures, "catalogue and signatures must agree");
        let mut deduped = listed.clone();
        deduped.dedup();
        assert_eq!(deduped.len(), listed.len(), "no duplicate provider entry");
        assert_eq!(PROVIDERS.len(), 10);
    }

    #[test]
    fn only_codex_claims_a_machine_quota_path() {
        for spec in PROVIDERS {
            let machine_quota = spec.quota_access == "machine";
            assert_eq!(
                machine_quota,
                spec.agent_id == "codex",
                "unexpected machine quota claim for {}",
                spec.agent_id
            );
            if spec.quota_access == "none" {
                assert!(
                    spec.limitation.is_some(),
                    "{} must explain why quota is unreadable",
                    spec.agent_id
                );
            }
        }
    }

    #[test]
    fn a_failed_read_is_mapped_to_a_distinct_state() {
        let auth = AppError::new(ErrorCode::AccessDenied, "x")
            .with_details(serde_json::json!({ "kind": "unauthenticated" }));
        assert_eq!(classify_failure(&auth).0, UsageStatus::Unauthenticated);
        let limited = AppError::new(ErrorCode::Unsupported, "x").with_details(
            serde_json::json!({ "kind": "rate-limited", "retryAfterMs": 12_000 }),
        );
        assert_eq!(
            classify_failure(&limited),
            (UsageStatus::RateLimited, Some(12_000))
        );
        let other = AppError::new(ErrorCode::Protocol, "boom");
        assert_eq!(classify_failure(&other).0, UsageStatus::Error);
    }

    #[test]
    fn catalog_status_never_upgrades_an_uninstalled_product() {
        let codex = provider_for("codex").unwrap();
        assert_eq!(catalog_status(codex, false), "unsupported");
        assert_eq!(catalog_status(codex, true), "supported");
        let gemini = provider_for("gemini").unwrap();
        assert_eq!(
            catalog_status(gemini, true),
            "unsupported",
            "a product with no readable surface stays unsupported even when installed"
        );
        let copilot = provider_for("copilot").unwrap();
        assert_eq!(catalog_status(copilot, false), "unsupported");
    }
}
