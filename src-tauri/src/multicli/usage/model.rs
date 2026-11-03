//! Account / quota / usage domain for S06.
//!
//! Three objects are deliberately kept apart:
//!
//! | object | answers | never used for |
//! |---|---|---|
//! | [`AccountProfile`] | *which* account this reading belongs to | a quota number |
//! | [`QuotaPool`] | one shared allowance per **confirmed** account | per-session totals |
//! | [`UsageSnapshot`] + [`UsageSample`] | what was read, from where, when, how fresh | an estimate presented as a bill |
//!
//! Two rules drive the shape of this module:
//!
//! 1. **A missing number is not zero.** Every measured field is an `Option`.
//!    `unknown`, `stale`, `unauthenticated`, `unsupported`, `rate-limited` and
//!    `error` are carried in [`UsageStatus`] and shown as text; no code path
//!    turns them into `0` or into an empty success.
//! 2. **A confirmed account has exactly one pool.** [`QuotaPool::pool_key`] is
//!    derived from the confirmed identity fingerprint, so two sessions of the
//!    same account upsert the same row. An unconfirmed identity keeps a stable
//!    per-installation key and is never merged with anything else.
//!
//! Cached input tokens are counted once: [`UsageSample::total_tokens`] is either
//! what the provider reported as its own total, or a value computed with the
//! declared [`TokenSemantics`] of that provider. `cached_input_tokens` is always
//! a *subset* marker, never an addend on top of `input_tokens`.

use serde::{Deserialize, Serialize};

use crate::multicli::error::AppError;

pub const USAGE_SCHEMA_VERSION: u32 = 1;

/// How long a reading is considered current. Providers may shorten it (a 429
/// pushes the next attempt out), never silently extend it.
pub const DEFAULT_TTL_MS: i64 = 5 * 60 * 1000;
/// Base delay for a retryable failure; doubled per attempt, capped below.
pub const BACKOFF_BASE_MS: i64 = 60 * 1000;
pub const BACKOFF_CAP_MS: i64 = 30 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageStatus {
    /// Every requested field was read.
    Ok,
    /// Some fields were read, others are explicitly unavailable.
    Partial,
    /// The last good reading is past its TTL; it is shown as stale, not as 0.
    Stale,
    /// The CLI is installed but has no usable credential for this read.
    Unauthenticated,
    /// This product has no verified machine-readable path for the field.
    Unsupported,
    /// The provider said "slow down"; `retry_after_ms` carries its hint.
    RateLimited,
    /// The read failed; `error` carries the real `AppError`.
    Error,
    /// Nothing has been read yet.
    Unknown,
}

impl UsageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Stale => "stale",
            Self::Unauthenticated => "unauthenticated",
            Self::Unsupported => "unsupported",
            Self::RateLimited => "rate-limited",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }

    /// `true` only for a reading that may be presented as current data.
    pub fn is_measured(self) -> bool {
        matches!(self, Self::Ok | Self::Partial)
    }

    /// Parses the stored spelling. An unknown value is `None`, never a guess.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(Self::Ok),
            "partial" => Some(Self::Partial),
            "stale" => Some(Self::Stale),
            "unauthenticated" => Some(Self::Unauthenticated),
            "unsupported" => Some(Self::Unsupported),
            "rate-limited" => Some(Self::RateLimited),
            "error" => Some(Self::Error),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IdentityStatus {
    /// The provider itself named the account (machine-readable id/email).
    Confirmed,
    /// An installation exists but the account behind it is not named.
    Unconfirmed,
    /// The account was named before but is not available right now.
    Unknown,
}

impl IdentityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Unconfirmed => "unconfirmed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageSourceKind {
    /// A documented machine interface of the installed CLI.
    OfficialMachine,
    /// A local transcript the CLI wrote; read-only, numbers only.
    LocalTranscript,
    /// The product only offers a human command; recorded as a manual entry.
    ManualEntry,
    /// No verified source exists for this field.
    Unavailable,
}

impl UsageSourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OfficialMachine => "official-machine",
            Self::LocalTranscript => "local-transcript",
            Self::ManualEntry => "manual-entry",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageMetric {
    /// A metered allowance window (percent used / remaining).
    QuotaWindow,
    /// Token counts. Never mixed with a currency amount.
    TokenUsage,
    /// Provider credits.
    CreditBalance,
    /// A locally computed money estimate; `estimated` is always true.
    CostEstimate,
    /// A provider-reported bill; never an estimate.
    Billing,
}

impl UsageMetric {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QuotaWindow => "quota-window",
            Self::TokenUsage => "token-usage",
            Self::CreditBalance => "credit-balance",
            Self::CostEstimate => "cost-estimate",
            Self::Billing => "billing",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
// Kebab-case keeps `usd-micros` identical to the TypeScript union and to
// `as_str`; `lowercase` would silently emit `usdmicros`.
#[serde(rename_all = "kebab-case")]
pub enum UsageUnit {
    Percent,
    Tokens,
    Credits,
    UsdMicros,
    Count,
}

impl UsageUnit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Percent => "percent",
            Self::Tokens => "tokens",
            Self::Credits => "credits",
            Self::UsdMicros => "usd-micros",
            Self::Count => "count",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageWindow {
    /// A short rolling window (typically 5 hours).
    Short,
    Daily,
    Weekly,
    Monthly,
    /// Account lifetime.
    Lifetime,
    /// One session/thread.
    Session,
    /// A window whose length the provider did not disclose.
    Unspecified,
}

impl UsageWindow {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Lifetime => "lifetime",
            Self::Session => "session",
            Self::Unspecified => "unspecified",
        }
    }

    /// Maps a provider-reported duration to a named window. A duration that
    /// fits no bucket stays `Unspecified` — the minutes are still shown.
    pub fn from_minutes(minutes: Option<i64>) -> Self {
        match minutes {
            Some(value) if (240..=360).contains(&value) => Self::Short,
            Some(value) if (1_380..=1_500).contains(&value) => Self::Daily,
            Some(value) if (9_000..=11_000).contains(&value) => Self::Weekly,
            Some(value) if (38_000..=45_000).contains(&value) => Self::Monthly,
            _ => Self::Unspecified,
        }
    }
}

/// Declares how a provider's `input_tokens` relates to its cached tokens, so a
/// total is never computed by adding cached tokens twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TokenSemantics {
    /// `input_tokens` already contains the cached tokens (Codex).
    InputIncludesCached,
    /// `input_tokens` excludes cached creation/read tokens (Claude).
    InputExcludesCached,
}

impl TokenSemantics {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InputIncludesCached => "input-includes-cached",
            Self::InputExcludesCached => "input-excludes-cached",
        }
    }

    /// Total tokens under this provider's semantics. Cached tokens are added
    /// only when the provider's input count excludes them.
    pub fn total(
        self,
        input: Option<u64>,
        cached: Option<u64>,
        cache_creation: Option<u64>,
        output: Option<u64>,
    ) -> Option<u64> {
        let mut sum: u64 = 0;
        let mut any = false;
        if let Some(value) = input {
            sum = sum.saturating_add(value);
            any = true;
        }
        if let Some(value) = output {
            sum = sum.saturating_add(value);
            any = true;
        }
        if self == Self::InputExcludesCached {
            if let Some(value) = cached {
                sum = sum.saturating_add(value);
                any = true;
            }
            if let Some(value) = cache_creation {
                sum = sum.saturating_add(value);
                any = true;
            }
        }
        any.then_some(sum)
    }
}

// ---------------------------------------------------------------------------
// Account
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountProfile {
    pub id: String,
    pub host_id: String,
    pub agent_id: String,
    pub product: String,
    pub vendor: String,
    /// A display label built from non-secret fields (`plan`, masked account).
    pub label: String,
    pub identity_status: IdentityStatus,
    /// Stable, non-reversible fingerprint of the provider's account id. Absent
    /// when the identity is not confirmed.
    pub identity_fingerprint: Option<String>,
    pub plan: Option<String>,
    /// Where the identity came from, e.g. `codex:account/read`.
    pub source: String,
    pub observed_at: String,
}

impl AccountProfile {
    pub fn validate(&self) -> Result<(), AppError> {
        for (field, value) in [
            ("id", &self.id),
            ("hostId", &self.host_id),
            ("agentId", &self.agent_id),
            ("product", &self.product),
            ("vendor", &self.vendor),
            ("source", &self.source),
            ("observedAt", &self.observed_at),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::invalid_argument(field, "must not be empty"));
            }
        }
        match (self.identity_status, &self.identity_fingerprint) {
            (IdentityStatus::Confirmed, None) => Err(AppError::invalid_argument(
                "identityFingerprint",
                "a confirmed identity needs a fingerprint",
            )),
            (IdentityStatus::Confirmed, Some(value)) if value.trim().is_empty() => Err(
                AppError::invalid_argument("identityFingerprint", "must not be empty"),
            ),
            _ => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Pool
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuotaPool {
    pub id: String,
    /// Dedupe key. For a confirmed account it is derived only from the
    /// fingerprint, so every session of that account shares one pool.
    pub pool_key: String,
    pub host_id: String,
    pub agent_id: String,
    pub product: String,
    pub account_profile_id: Option<String>,
    pub identity_status: IdentityStatus,
    pub label: String,
    /// How many installations/sessions map onto this pool. `>1` is the visible
    /// proof that two sessions share one allowance.
    pub installation_count: u32,
    pub created_at: String,
    pub updated_at: String,
}

impl QuotaPool {
    /// Builds the dedupe key. A confirmed account ignores the installation
    /// entirely; an unconfirmed one is pinned to its installation so it can
    /// never be silently merged with a second, unknown account.
    pub fn key_for(
        agent_id: &str,
        identity_status: IdentityStatus,
        fingerprint: Option<&str>,
        installation_scope: Option<&str>,
    ) -> String {
        match identity_status {
            IdentityStatus::Confirmed => format!(
                "confirmed:{}:{}",
                agent_id,
                fingerprint.unwrap_or("missing")
            ),
            _ => format!(
                "unconfirmed:{}:{}",
                agent_id,
                installation_scope.unwrap_or("no-installation")
            ),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        for (field, value) in [
            ("id", &self.id),
            ("poolKey", &self.pool_key),
            ("hostId", &self.host_id),
            ("agentId", &self.agent_id),
            ("product", &self.product),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::invalid_argument(field, "must not be empty"));
            }
        }
        if self.identity_status == IdentityStatus::Confirmed && self.account_profile_id.is_none() {
            return Err(AppError::invalid_argument(
                "accountProfileId",
                "a confirmed pool must reference its account profile",
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Samples
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSample {
    pub metric: UsageMetric,
    pub unit: UsageUnit,
    pub window: UsageWindow,
    pub label: String,
    /// `0..=100`, or `None` when the provider did not report it.
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    /// Scalar amount in `unit` (tokens/credits/usd-micros). For token samples
    /// this equals `total_tokens` and exists only for display convenience.
    pub amount: Option<f64>,
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub token_semantics: Option<TokenSemantics>,
    pub resets_at: Option<String>,
    pub window_minutes: Option<u32>,
    /// True for a locally computed money estimate; false for a bill.
    pub estimated: bool,
    pub source: String,
    pub note: Option<String>,
}

impl UsageSample {
    /// Metadata (a window length or a source label) is not a measured value.
    pub fn has_numeric_value(&self) -> bool {
        self.used_percent.is_some() || self.remaining_percent.is_some()
            || self.amount.is_some() || self.total_tokens.is_some()
            || self.input_tokens.is_some() || self.cached_input_tokens.is_some()
            || self.cache_creation_tokens.is_some() || self.output_tokens.is_some()
    }

    pub fn quota(
        label: impl Into<String>,
        used_percent: Option<f64>,
        window_minutes: Option<i64>,
        resets_at: Option<String>,
        source: impl Into<String>,
    ) -> Self {
        let remaining = used_percent.map(|value| (100.0 - value).clamp(0.0, 100.0));
        Self {
            metric: UsageMetric::QuotaWindow,
            unit: UsageUnit::Percent,
            window: UsageWindow::from_minutes(window_minutes),
            label: label.into(),
            used_percent: used_percent.map(|value| value.clamp(0.0, 100.0)),
            remaining_percent: remaining,
            amount: None,
            total_tokens: None,
            input_tokens: None,
            cached_input_tokens: None,
            cache_creation_tokens: None,
            output_tokens: None,
            token_semantics: None,
            resets_at,
            window_minutes: window_minutes.and_then(|value| u32::try_from(value).ok()),
            estimated: false,
            source: source.into(),
            note: None,
        }
    }

    pub fn tokens(
        label: impl Into<String>,
        window: UsageWindow,
        semantics: TokenSemantics,
        input: Option<u64>,
        cached: Option<u64>,
        cache_creation: Option<u64>,
        output: Option<u64>,
        reported_total: Option<u64>,
        source: impl Into<String>,
    ) -> Self {
        let total =
            reported_total.or_else(|| semantics.total(input, cached, cache_creation, output));
        Self {
            metric: UsageMetric::TokenUsage,
            unit: UsageUnit::Tokens,
            window,
            label: label.into(),
            used_percent: None,
            remaining_percent: None,
            amount: total.map(|value| value as f64),
            total_tokens: total,
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_tokens: cache_creation,
            output_tokens: output,
            token_semantics: Some(semantics),
            resets_at: None,
            window_minutes: None,
            estimated: false,
            source: source.into(),
            note: None,
        }
    }

    /// A measurement whose total is **exactly** what the caller computed from a
    /// complete record set. Unlike [`UsageSample::tokens`] it never derives a
    /// total from a partial set: `None` means "unknown", not "sum the parts and
    /// hope". Used by the transcript reader, where one record with a missing
    /// component makes the whole window's total unknown.
    #[allow(clippy::too_many_arguments)]
    pub fn token_measurement(
        label: impl Into<String>,
        window: UsageWindow,
        semantics: TokenSemantics,
        input: Option<u64>,
        cached: Option<u64>,
        cache_creation: Option<u64>,
        output: Option<u64>,
        total: Option<u64>,
        source: impl Into<String>,
    ) -> Self {
        Self {
            metric: UsageMetric::TokenUsage,
            unit: UsageUnit::Tokens,
            window,
            label: label.into(),
            used_percent: None,
            remaining_percent: None,
            amount: total.map(|value| value as f64),
            total_tokens: total,
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_tokens: cache_creation,
            output_tokens: output,
            token_semantics: Some(semantics),
            resets_at: None,
            window_minutes: None,
            estimated: false,
            source: source.into(),
            note: None,
        }
    }

    /// A provider-reported **total only** — no per-component breakdown. The
    /// total is real; `inputTokens` stays `None` because the provider never
    /// reported an input count. Filling `input = total` would fabricate a
    /// breakdown the wire data does not contain.
    pub fn token_total(
        label: impl Into<String>,
        window: UsageWindow,
        total: u64,
        source: impl Into<String>,
    ) -> Self {
        Self {
            metric: UsageMetric::TokenUsage,
            unit: UsageUnit::Tokens,
            window,
            label: label.into(),
            used_percent: None,
            remaining_percent: None,
            amount: Some(total as f64),
            total_tokens: Some(total),
            input_tokens: None,
            cached_input_tokens: None,
            cache_creation_tokens: None,
            output_tokens: None,
            token_semantics: None,
            resets_at: None,
            window_minutes: None,
            estimated: false,
            source: source.into(),
            note: Some("提供方只报告总量，未提供 in/cached/out 明细".to_string()),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.label.trim().is_empty() {
            return Err(AppError::invalid_argument("label", "must not be empty"));
        }
        if self.source.trim().is_empty() {
            return Err(AppError::invalid_argument("source", "must not be empty"));
        }
        if let Some(value) = self.used_percent {
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(AppError::invalid_argument(
                    "usedPercent",
                    "must be a finite percentage between 0 and 100",
                ));
            }
        }
        if let Some(value) = self.remaining_percent {
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(AppError::invalid_argument(
                    "remainingPercent",
                    "must be a finite percentage between 0 and 100",
                ));
            }
        }
        if self.metric == UsageMetric::CostEstimate && !self.estimated {
            return Err(AppError::invalid_argument(
                "estimated",
                "a cost estimate must be marked as estimated",
            ));
        }
        if self.metric == UsageMetric::Billing && self.estimated {
            return Err(AppError::invalid_argument(
                "estimated",
                "a provider bill is not an estimate",
            ));
        }
        if self.metric == UsageMetric::TokenUsage {
            if self.unit != UsageUnit::Tokens {
                return Err(AppError::invalid_argument(
                    "unit",
                    "a token sample is measured in tokens",
                ));
            }
            if let Some(total) = self.total_tokens {
                if let Some(input) = self.input_tokens {
                    if total < input {
                        return Err(AppError::invalid_argument(
                            "totalTokens",
                            "cannot be smaller than input tokens",
                        ));
                    }
                }
                if let (Some(cached), Some(input)) =
                    (self.cached_input_tokens, self.input_tokens)
                {
                    if self.token_semantics == Some(TokenSemantics::InputIncludesCached)
                        && cached > input
                    {
                        return Err(AppError::invalid_argument(
                            "cachedInputTokens",
                            "is a subset of input tokens under these semantics",
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageSnapshot {
    pub id: String,
    pub pool_id: String,
    pub agent_id: String,
    pub status: UsageStatus,
    /// `fresh | stale | unknown` computed at read time from `captured_at`.
    pub freshness: String,
    pub source_kind: UsageSourceKind,
    /// Human-readable provenance, e.g. `codex app-server account/rateLimits/read`.
    pub source: String,
    pub captured_at: String,
    pub expires_at: Option<String>,
    /// Real failure, kept next to the (possibly stale) numbers.
    pub error: Option<AppError>,
    pub retry_after_ms: Option<i64>,
    pub next_attempt_at: Option<String>,
    pub attempts: u32,
    pub samples: Vec<UsageSample>,
    pub note: Option<String>,
    /// Fields this reading could not provide, by name. Shown instead of `0`.
    pub unavailable_fields: Vec<String>,
}

impl UsageSnapshot {
    pub fn empty(pool_id: &str, agent_id: &str) -> Self {
        Self {
            id: format!("snapshot-{}", uuid::Uuid::new_v4().simple()),
            pool_id: pool_id.to_string(),
            agent_id: agent_id.to_string(),
            status: UsageStatus::Unknown,
            freshness: "unknown".to_string(),
            source_kind: UsageSourceKind::Unavailable,
            source: "none".to_string(),
            captured_at: now_iso8601(),
            expires_at: None,
            error: None,
            retry_after_ms: None,
            next_attempt_at: None,
            attempts: 0,
            samples: Vec::new(),
            note: None,
            unavailable_fields: Vec::new(),
        }
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.id.trim().is_empty() || self.pool_id.trim().is_empty() {
            return Err(AppError::invalid_argument(
                "snapshot.id",
                "id and poolId must not be empty",
            ));
        }
        if self.source.trim().is_empty() {
            return Err(AppError::invalid_argument("source", "must not be empty"));
        }
        for sample in &self.samples {
            sample.validate()?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProviderCatalogEntry {
    pub agent_id: String,
    pub product: String,
    /// `machine | transcript | manual | none` for quota.
    pub quota_access: String,
    /// `machine | transcript | manual | none` for token usage.
    pub token_access: String,
    /// The official command a user may run themselves, if one exists.
    pub official_entry: Option<String>,
    /// `supported | unverified | unsupported` for this build/host.
    pub status: String,
    /// `true` when an installation of this product is registered on the host.
    pub installed: bool,
    pub evidence: String,
    pub limitation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageAccountView {
    pub account: AccountProfile,
    pub pools: Vec<PoolView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PoolView {
    pub pool: QuotaPool,
    pub snapshot: UsageSnapshot,
    /// Newest snapshot that actually measured something. Shown beside a failed
    /// refresh so an old number is never mistaken for the current state.
    pub last_good: Option<UsageSnapshot>,
    /// Installations the pool was observed from. Two entries means two
    /// sessions of one confirmed account share this single row.
    pub installation_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageOverviewView {
    pub schema_version: u32,
    pub generated_at: String,
    pub accounts: Vec<UsageAccountView>,
    /// Pools whose account could not be confirmed. Kept separate on purpose:
    /// they must never be summed with a confirmed pool.
    pub unconfirmed_pools: Vec<PoolView>,
    pub catalog: Vec<ProviderCatalogEntry>,
    pub last_attempt_at: Option<String>,
    pub last_success_at: Option<String>,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageRefreshRequest {
    /// Empty means "every provider that has a confirmed source".
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Refresh even when the cached reading is still fresh.
    #[serde(default)]
    pub force: bool,
}

impl UsageRefreshRequest {
    pub fn validate(&self) -> Result<(), AppError> {
        if let Some(agent_id) = &self.agent_id {
            if agent_id.trim().is_empty() {
                return Err(AppError::invalid_argument("agentId", "must not be empty"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UsageRefreshResult {
    pub overview: UsageOverviewView,
    /// Per-provider outcome, including providers that were skipped because
    /// their backoff has not elapsed.
    pub outcomes: Vec<RefreshOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RefreshOutcome {
    pub agent_id: String,
    pub status: UsageStatus,
    pub skipped: bool,
    pub reason: Option<String>,
    pub retry_after_ms: Option<i64>,
}

pub const OVERVIEW_NOTE: &str = "额度按已确认账户去重：同一账户的多个会话只显示一份池记录；身份未确认的池单独列出，不与任何账户相加。未知/陈旧/未认证/不支持均显示状态，不显示为 0。费用估算与真实账单分属不同指标，缓存 token 只按提供方语义计入一次。";

/// Current UTC time as `RFC 3339` with milliseconds, matching the rest of the
/// store's timestamps. Reuses the history module's formatter so the whole
/// product writes one timestamp spelling and needs no extra date dependency.
pub fn now_iso8601() -> String {
    crate::multicli::history::ids::iso_from_millis(crate::multicli::store::now_ms())
}

/// Parses a stored timestamp; a value that cannot be parsed is *unknown*, never
/// silently treated as "just now".
pub fn parse_timestamp(value: &str) -> Option<i64> {
    crate::multicli::history::ids::parse_iso_millis(value)
}

/// `fresh` while inside the TTL, `stale` after it, `unknown` when the timestamp
/// itself is unusable.
pub fn freshness_at(captured_at: &str, ttl_ms: i64, now_ms: i64) -> &'static str {
    match parse_timestamp(captured_at) {
        Some(captured) => {
            let age = now_ms - captured;
            if age < 0 {
                // A clock jump must not turn a reading into a permanent
                // "fresh" claim; report it as unknown instead.
                "unknown"
            } else if age <= ttl_ms.max(0) {
                "fresh"
            } else {
                "stale"
            }
        }
        None => "unknown",
    }
}

/// Exponential backoff with a provider hint taking precedence.
pub fn backoff_ms(attempts: u32, retry_after_ms: Option<i64>) -> i64 {
    if let Some(hint) = retry_after_ms {
        return hint.clamp(5_000, 60 * 60 * 1000);
    }
    let exponent = attempts.saturating_sub(1).min(10);
    (BACKOFF_BASE_MS.saturating_mul(1_i64 << exponent)).min(BACKOFF_CAP_MS)
}

/// RFC 3339 timestamp `ms` milliseconds from now.
pub fn iso_after_ms(ms: i64) -> String {
    crate::multicli::history::ids::iso_from_millis(crate::multicli::store::now_ms() + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_is_not_zero_and_status_is_explicit() {
        let snapshot = UsageSnapshot::empty("pool-1", "codex");
        assert_eq!(snapshot.status, UsageStatus::Unknown);
        assert!(snapshot.samples.is_empty());
        assert!(!snapshot.status.is_measured());
        // The serialized form must not invent a numeric zero.
        let encoded = serde_json::to_value(&snapshot).unwrap();
        assert!(encoded["samples"].as_array().unwrap().is_empty());
        assert_eq!(encoded["status"], "unknown");
    }

    #[test]
    fn confirmed_accounts_share_one_pool_and_unconfirmed_never_merge() {
        let a = QuotaPool::key_for("codex", IdentityStatus::Confirmed, Some("abc"), Some("i-1"));
        let b = QuotaPool::key_for("codex", IdentityStatus::Confirmed, Some("abc"), Some("i-2"));
        assert_eq!(a, b, "two installations of one confirmed account share a pool");
        let c = QuotaPool::key_for("codex", IdentityStatus::Unconfirmed, None, Some("i-1"));
        let d = QuotaPool::key_for("codex", IdentityStatus::Unconfirmed, None, Some("i-2"));
        assert_ne!(c, d, "unconfirmed identities are never merged");
        assert_ne!(a, c);
    }

    #[test]
    fn cached_tokens_are_counted_once_under_both_semantics() {
        // Codex: input already contains cached.
        assert_eq!(
            TokenSemantics::InputIncludesCached.total(Some(100), Some(60), None, Some(20)),
            Some(120)
        );
        // Claude: input excludes cache read/creation.
        assert_eq!(
            TokenSemantics::InputExcludesCached.total(Some(100), Some(60), Some(10), Some(20)),
            Some(190)
        );
        assert_eq!(
            TokenSemantics::InputExcludesCached.total(None, None, None, None),
            None,
            "no numbers is not a total of zero"
        );
    }

    #[test]
    fn token_sample_rejects_a_total_smaller_than_its_parts() {
        let mut sample = UsageSample::tokens(
            "today",
            UsageWindow::Daily,
            TokenSemantics::InputExcludesCached,
            Some(100),
            Some(10),
            None,
            Some(5),
            None,
            "test",
        );
        assert!(sample.validate().is_ok());
        sample.total_tokens = Some(50);
        assert!(sample.validate().is_err());
    }

    #[test]
    fn freshness_never_upgrades_an_unparseable_timestamp() {
        let now = crate::multicli::store::now_ms();
        assert_eq!(freshness_at("not-a-date", DEFAULT_TTL_MS, now), "unknown");
        assert_eq!(freshness_at(&iso_after_ms(0), DEFAULT_TTL_MS, now), "fresh");
        let old = iso_after_ms(-30 * 60 * 1000);
        assert_eq!(freshness_at(&old, DEFAULT_TTL_MS, now), "stale");
        let future = iso_after_ms(5 * 60 * 1000);
        assert_eq!(freshness_at(&future, DEFAULT_TTL_MS, now), "unknown");
    }

    #[test]
    fn backoff_doubles_and_respects_the_provider_hint() {
        assert_eq!(backoff_ms(1, None), 60_000);
        assert_eq!(backoff_ms(3, None), 240_000);
        assert_eq!(backoff_ms(30, None), BACKOFF_CAP_MS);
        assert_eq!(backoff_ms(1, Some(2_000)), 5_000, "clamped up to the floor");
        assert_eq!(backoff_ms(1, Some(10 * 60 * 60 * 1000)), 3_600_000);
    }

    #[test]
    fn window_names_come_from_reported_minutes_only() {
        assert_eq!(UsageWindow::from_minutes(Some(300)), UsageWindow::Short);
        assert_eq!(UsageWindow::from_minutes(Some(1_440)), UsageWindow::Daily);
        assert_eq!(UsageWindow::from_minutes(Some(10_080)), UsageWindow::Weekly);
        assert_eq!(UsageWindow::from_minutes(Some(123)), UsageWindow::Unspecified);
        assert_eq!(UsageWindow::from_minutes(None), UsageWindow::Unspecified);
    }

    #[test]
    fn a_cost_estimate_must_be_marked_and_a_bill_must_not() {
        let mut sample = UsageSample::quota("x", Some(1.0), Some(300), None, "test");
        sample.metric = UsageMetric::CostEstimate;
        sample.estimated = false;
        assert!(sample.validate().is_err());
        sample.estimated = true;
        assert!(sample.validate().is_ok());
        sample.metric = UsageMetric::Billing;
        assert!(sample.validate().is_err(), "a bill is never an estimate");
    }
}
