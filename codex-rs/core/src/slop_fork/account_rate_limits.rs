use std::collections::BTreeMap;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::ErrorKind;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use codex_protocol::account::PlanType as AccountPlanType;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::RateLimitWindow;
use serde::Deserialize;
use serde::Serialize;

use super::auth_accounts;

const SNAPSHOT_VERSION: u32 = 2;
const RATE_LIMITS_FILE: &str = ".rate-limits.json";
const LEGACY_RATE_LIMITS_DIR: &str = ".account-rate-limits";
const RESET_PASSED_TOLERANCE_SECS: i64 = 5;
const RATE_LIMIT_REFRESH_STALE_INTERVAL_SECS: i64 = 30 * 60;
const TOUCH_ATTEMPT_COOLDOWN_SECS: i64 = 10 * 60;
const UNTOUCHED_RESET_AFTER_TOLERANCE_SECS: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    FiveHour,
    Weekly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaWindowState {
    Unknown,
    Untouched,
    Started,
    ResetPassed,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawRateLimitSnapshotInput {
    pub primary: Option<RawRateLimitWindowSnapshot>,
    pub secondary: Option<RawRateLimitWindowSnapshot>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawRateLimitWindowSnapshot {
    pub used_percent: i32,
    pub limit_window_seconds: i32,
    pub reset_after_seconds: i32,
    pub reset_at: i32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredQuotaWindow {
    #[serde(default)]
    pub used_percent: Option<i32>,
    #[serde(default)]
    pub limit_window_seconds: Option<i64>,
    #[serde(default)]
    pub reset_after_seconds: Option<i64>,
    #[serde(default)]
    pub reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_touch_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_touch_confirmed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_touch_reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredRateLimitSnapshot {
    pub account_id: String,
    pub plan: Option<String>,
    pub snapshot: Option<RateLimitSnapshot>,
    pub five_hour_window: StoredQuotaWindow,
    pub weekly_window: StoredQuotaWindow,
    pub observed_at: Option<DateTime<Utc>>,
    pub primary_next_reset_at: Option<DateTime<Utc>>,
    pub secondary_next_reset_at: Option<DateTime<Utc>>,
    pub last_refresh_attempt_at: Option<DateTime<Utc>>,
    pub last_usage_limit_hit_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredRateLimitSnapshotsFile {
    version: u32,
    #[serde(default)]
    snapshots: BTreeMap<String, StoredRateLimitSnapshotFile>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StoredRateLimitSnapshotFile {
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    snapshot: Option<RateLimitSnapshot>,
    #[serde(default)]
    five_hour_window: StoredQuotaWindow,
    #[serde(default)]
    weekly_window: StoredQuotaWindow,
    #[serde(default)]
    observed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    primary_next_reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    secondary_next_reset_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_refresh_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    last_usage_limit_hit_at: Option<DateTime<Utc>>,
}

impl StoredRateLimitSnapshotFile {
    fn into_public(self, account_id: String) -> StoredRateLimitSnapshot {
        StoredRateLimitSnapshot {
            account_id,
            plan: self.plan,
            snapshot: self.snapshot,
            five_hour_window: self.five_hour_window,
            weekly_window: self.weekly_window,
            observed_at: self.observed_at,
            primary_next_reset_at: self.primary_next_reset_at,
            secondary_next_reset_at: self.secondary_next_reset_at,
            last_refresh_attempt_at: self.last_refresh_attempt_at,
            last_usage_limit_hit_at: self.last_usage_limit_hit_at,
        }
    }
}

impl StoredRateLimitSnapshotsFile {
    fn new() -> Self {
        Self {
            version: SNAPSHOT_VERSION,
            snapshots: BTreeMap::new(),
        }
    }
}

pub(crate) fn rate_limits_path(codex_home: &Path) -> PathBuf {
    auth_accounts::accounts_dir(codex_home).join(RATE_LIMITS_FILE)
}

pub(crate) fn is_rate_limits_sidecar_file(path: &Path) -> bool {
    path.file_name().and_then(|name| name.to_str()) == Some(RATE_LIMITS_FILE)
}

pub fn rate_limit_refresh_stale_interval() -> Duration {
    Duration::seconds(RATE_LIMIT_REFRESH_STALE_INTERVAL_SECS)
}

pub fn rate_limit_refresh_is_due(
    snapshot: Option<&StoredRateLimitSnapshot>,
    now: DateTime<Utc>,
    stale_interval: Duration,
) -> bool {
    let Some(snapshot) = snapshot else {
        return true;
    };

    rate_limit_refresh_is_due_from_state(
        snapshot.observed_at,
        snapshot_reset_at(snapshot),
        snapshot.last_refresh_attempt_at,
        now,
        stale_interval,
    )
}

pub fn snapshot_used_percent(snapshot: &StoredRateLimitSnapshot) -> Option<f64> {
    let snapshot = snapshot.snapshot.as_ref()?;
    let used_percent = snapshot
        .primary
        .as_ref()
        .map(|window| window.used_percent)
        .unwrap_or(0.0)
        .max(
            snapshot
                .secondary
                .as_ref()
                .map(|window| window.used_percent)
                .unwrap_or(0.0),
        );
    used_percent.is_finite().then_some(used_percent)
}

pub fn snapshot_reset_at(snapshot: &StoredRateLimitSnapshot) -> Option<DateTime<Utc>> {
    [
        snapshot.five_hour_window.reset_at,
        snapshot.weekly_window.reset_at,
        snapshot.primary_next_reset_at,
        snapshot.secondary_next_reset_at,
    ]
    .into_iter()
    .flatten()
    .max()
}

pub fn quota_window(
    snapshot: &StoredRateLimitSnapshot,
    kind: QuotaWindowKind,
) -> &StoredQuotaWindow {
    match kind {
        QuotaWindowKind::FiveHour => &snapshot.five_hour_window,
        QuotaWindowKind::Weekly => &snapshot.weekly_window,
    }
}

pub fn quota_window_state(
    snapshot: &StoredRateLimitSnapshot,
    kind: QuotaWindowKind,
    now: DateTime<Utc>,
) -> QuotaWindowState {
    let window = quota_window(snapshot, kind);
    let Some(used_percent) = window.used_percent else {
        return QuotaWindowState::Unknown;
    };
    let Some(limit_window_seconds) = window.limit_window_seconds else {
        return QuotaWindowState::Unknown;
    };
    if limit_window_seconds <= 0 {
        return QuotaWindowState::Unknown;
    }
    if let Some(reset_at) = window.reset_at
        && reset_at <= now
    {
        return QuotaWindowState::ResetPassed;
    }
    if used_percent > 0 {
        return QuotaWindowState::Started;
    }

    let reset_after_seconds = window.reset_after_seconds.or_else(|| {
        window
            .reset_at
            .zip(snapshot.observed_at)
            .map(|(reset_at, observed_at)| {
                reset_at.signed_duration_since(observed_at).num_seconds()
            })
    });
    let Some(reset_after_seconds) = reset_after_seconds else {
        return QuotaWindowState::Unknown;
    };
    if reset_after_matches_full_window(reset_after_seconds, limit_window_seconds) {
        QuotaWindowState::Untouched
    } else {
        QuotaWindowState::Started
    }
}

fn reset_after_matches_full_window(reset_after_seconds: i64, limit_window_seconds: i64) -> bool {
    (reset_after_seconds - limit_window_seconds).abs() <= UNTOUCHED_RESET_AFTER_TOLERANCE_SECS
}

pub fn quota_window_should_start(
    snapshot: &StoredRateLimitSnapshot,
    kind: QuotaWindowKind,
    now: DateTime<Utc>,
) -> bool {
    let window = quota_window(snapshot, kind);
    let touch_attempt_cooldown = Duration::seconds(TOUCH_ATTEMPT_COOLDOWN_SECS);
    if window.last_touch_attempt_at.is_some_and(|attempted_at| {
        now.signed_duration_since(attempted_at) < touch_attempt_cooldown
    }) {
        return false;
    }
    match quota_window_state(snapshot, kind, now) {
        QuotaWindowState::Untouched => quota_window_is_unconfirmed_untouched(window),
        QuotaWindowState::ResetPassed => true,
        QuotaWindowState::Unknown | QuotaWindowState::Started => false,
    }
}

fn quota_window_is_unconfirmed_untouched(window: &StoredQuotaWindow) -> bool {
    window.last_touch_confirmed_at.is_none() || window.last_touch_reset_at != window.reset_at
}

pub fn quota_window_reset_at_if_untouched(
    snapshot: &StoredRateLimitSnapshot,
    kind: QuotaWindowKind,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let window = quota_window(snapshot, kind);
    if quota_window_state(snapshot, kind, now) != QuotaWindowState::Untouched
        || !quota_window_is_unconfirmed_untouched(window)
    {
        return None;
    }

    window
        .limit_window_seconds
        .filter(|seconds| *seconds > 0)
        .map(|seconds| now + Duration::seconds(seconds))
}

pub fn load_rate_limit_snapshot(
    codex_home: &Path,
    account_id: &str,
) -> std::io::Result<Option<StoredRateLimitSnapshot>> {
    Ok(load_snapshot_store(codex_home)?
        .snapshots
        .remove(account_id)
        .map(|snapshot| snapshot.into_public(account_id.to_string())))
}

pub fn list_rate_limit_snapshots(
    codex_home: &Path,
) -> std::io::Result<Vec<StoredRateLimitSnapshot>> {
    let mut snapshots = load_snapshot_store(codex_home)?
        .snapshots
        .into_iter()
        .map(|(account_id, snapshot)| snapshot.into_public(account_id))
        .collect::<Vec<_>>();
    snapshots.sort_by(|lhs, rhs| lhs.account_id.cmp(&rhs.account_id));
    Ok(snapshots)
}

pub fn snapshot_map_for_accounts(
    codex_home: &Path,
    accounts: &[auth_accounts::StoredAccount],
) -> std::io::Result<HashMap<String, StoredRateLimitSnapshot>> {
    let snapshot_store = load_snapshot_store(codex_home)?;
    let mut snapshots = HashMap::new();

    for account in accounts {
        let Some(snapshot) = auth_accounts::rate_limit_snapshot_lookup_ids(account)
            .into_iter()
            .find_map(|lookup_id| snapshot_store.snapshots.get(&lookup_id).cloned())
        else {
            continue;
        };
        snapshots.insert(account.id.clone(), snapshot.into_public(account.id.clone()));
    }

    Ok(snapshots)
}

pub fn record_rate_limit_snapshot(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    snapshot: &RateLimitSnapshot,
    observed_at: DateTime<Utc>,
) -> std::io::Result<()> {
    record_rate_limit_snapshot_with_raw(
        codex_home,
        account_id,
        plan,
        snapshot,
        /*raw*/ None,
        observed_at,
    )
}

pub fn record_rate_limit_snapshot_with_raw(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    snapshot: &RateLimitSnapshot,
    raw: Option<&RawRateLimitSnapshotInput>,
    observed_at: DateTime<Utc>,
) -> std::io::Result<()> {
    update_snapshot_file(codex_home, account_id, plan, |stored| {
        stored.observed_at = Some(observed_at);
        stored.snapshot = Some(snapshot.clone());
        stored.five_hour_window = stored_quota_window(
            snapshot.primary.as_ref(),
            raw.and_then(|input| input.primary.as_ref()),
            observed_at,
            std::mem::take(&mut stored.five_hour_window),
        );
        stored.weekly_window = stored_quota_window(
            snapshot.secondary.as_ref(),
            raw.and_then(|input| input.secondary.as_ref()),
            observed_at,
            std::mem::take(&mut stored.weekly_window),
        );
        stored.primary_next_reset_at = snapshot
            .primary
            .as_ref()
            .and_then(|window| window.resets_at)
            .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
        stored.secondary_next_reset_at = snapshot
            .secondary
            .as_ref()
            .and_then(|window| window.resets_at)
            .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0));
    })
}

pub fn record_usage_limit_hint(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    reset_at: Option<DateTime<Utc>>,
    observed_at: DateTime<Utc>,
) -> std::io::Result<()> {
    update_snapshot_file(codex_home, account_id, plan, |stored| {
        stored.last_usage_limit_hit_at = Some(observed_at);
        if let Some(reset_at) = reset_at {
            stored.primary_next_reset_at = Some(reset_at);
            stored.secondary_next_reset_at = Some(reset_at);
        }
    })
}

pub fn mark_rate_limit_refresh_attempt_if_due(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    reset_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    stale_interval: Duration,
) -> std::io::Result<bool> {
    let mut should_refresh = false;
    update_snapshot_file(codex_home, account_id, plan, |stored| {
        if rate_limit_refresh_is_due_from_state(
            stored.observed_at,
            reset_at,
            stored.last_refresh_attempt_at,
            now,
            stale_interval,
        ) {
            stored.last_refresh_attempt_at = Some(now);
            should_refresh = true;
        }
    })?;
    Ok(should_refresh)
}

pub fn mark_quota_window_touch_attempt(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    kind: QuotaWindowKind,
    attempted_at: DateTime<Utc>,
) -> std::io::Result<()> {
    update_snapshot_file(codex_home, account_id, plan, |stored| {
        quota_window_mut(stored, kind).last_touch_attempt_at = Some(attempted_at);
    })
}

pub fn mark_quota_window_touch_confirmed(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    kind: QuotaWindowKind,
    reset_at: Option<DateTime<Utc>>,
    confirmed_at: DateTime<Utc>,
) -> std::io::Result<()> {
    update_snapshot_file(codex_home, account_id, plan, |stored| {
        let window = quota_window_mut(stored, kind);
        window.last_touch_confirmed_at = Some(confirmed_at);
        window.last_touch_reset_at = reset_at;
    })
}

fn rate_limit_refresh_is_due_from_state(
    observed_at: Option<DateTime<Utc>>,
    reset_at: Option<DateTime<Utc>>,
    last_refresh_attempt_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    stale_interval: Duration,
) -> bool {
    let reset_tolerance = Duration::seconds(RESET_PASSED_TOLERANCE_SECS);
    let attempted_after_reset = reset_at
        .zip(last_refresh_attempt_at)
        .is_some_and(|(reset_at, last_attempt)| last_attempt >= reset_at);
    let observed_after_reset = reset_at
        .zip(observed_at)
        .is_some_and(|(reset_at, observed_at)| observed_at + reset_tolerance >= reset_at);

    match reset_at {
        Some(reset_at) if now + reset_tolerance < reset_at => false,
        Some(_) if observed_after_reset => false,
        Some(_) if !attempted_after_reset => true,
        Some(_) => last_refresh_attempt_at
            .is_none_or(|last_attempt| now.signed_duration_since(last_attempt) >= stale_interval),
        None => match observed_at {
            Some(observed_at) if now.signed_duration_since(observed_at) < stale_interval => false,
            Some(_) | None => last_refresh_attempt_at.is_none_or(|last_attempt| {
                now.signed_duration_since(last_attempt) >= stale_interval
            }),
        },
    }
}

fn quota_window_mut(
    snapshot: &mut StoredRateLimitSnapshotFile,
    kind: QuotaWindowKind,
) -> &mut StoredQuotaWindow {
    match kind {
        QuotaWindowKind::FiveHour => &mut snapshot.five_hour_window,
        QuotaWindowKind::Weekly => &mut snapshot.weekly_window,
    }
}

fn stored_quota_window(
    window: Option<&RateLimitWindow>,
    raw: Option<&RawRateLimitWindowSnapshot>,
    observed_at: DateTime<Utc>,
    previous: StoredQuotaWindow,
) -> StoredQuotaWindow {
    let reset_at = raw
        .and_then(|raw| DateTime::<Utc>::from_timestamp(i64::from(raw.reset_at), 0))
        .or_else(|| {
            window
                .and_then(|window| window.resets_at)
                .and_then(|reset_at| DateTime::<Utc>::from_timestamp(reset_at, 0))
        });
    let limit_window_seconds = raw
        .map(|raw| i64::from(raw.limit_window_seconds))
        .or_else(|| {
            window
                .and_then(|window| window.window_minutes)
                .map(|minutes| minutes.saturating_mul(60))
        });
    let reset_after_seconds = raw
        .map(|raw| i64::from(raw.reset_after_seconds))
        .or_else(|| {
            reset_at.map(|reset_at| reset_at.signed_duration_since(observed_at).num_seconds())
        });
    let used_percent = raw
        .map(|raw| raw.used_percent)
        .or_else(|| window.map(|window| window.used_percent.round() as i32));
    StoredQuotaWindow {
        used_percent,
        limit_window_seconds,
        reset_after_seconds,
        reset_at,
        last_touch_attempt_at: previous.last_touch_attempt_at,
        last_touch_confirmed_at: previous.last_touch_confirmed_at,
        last_touch_reset_at: previous.last_touch_reset_at,
    }
}

pub fn plan_label(plan: AccountPlanType) -> &'static str {
    match plan {
        AccountPlanType::Free => "free",
        AccountPlanType::Go => "go",
        AccountPlanType::Plus => "plus",
        AccountPlanType::Pro => "pro",
        AccountPlanType::Team => "team",
        AccountPlanType::Business => "business",
        AccountPlanType::Enterprise => "enterprise",
        AccountPlanType::SelfServeBusinessUsageBased => "self_serve_business_usage_based",
        AccountPlanType::EnterpriseCbpUsageBased => "enterprise_cbp_usage_based",
        AccountPlanType::Edu => "edu",
        AccountPlanType::Unknown => "unknown",
    }
}

fn update_snapshot_file<F>(
    codex_home: &Path,
    account_id: &str,
    plan: Option<&str>,
    mut update: F,
) -> std::io::Result<()>
where
    F: FnMut(&mut StoredRateLimitSnapshotFile),
{
    let dir = auth_accounts::accounts_dir(codex_home);
    std::fs::create_dir_all(&dir)?;
    let path = rate_limits_path(codex_home);
    let mut snapshot_store = load_snapshot_store(codex_home)?;
    snapshot_store.version = SNAPSHOT_VERSION;
    let stored = snapshot_store
        .snapshots
        .entry(account_id.to_string())
        .or_default();
    if let Some(plan) = plan {
        stored.plan = Some(plan.to_string());
    }
    update(stored);
    write_snapshot_file(&path, &snapshot_store)
}

fn load_snapshot_store(codex_home: &Path) -> std::io::Result<StoredRateLimitSnapshotsFile> {
    let mut snapshot_store =
        load_shared_snapshot_store(codex_home)?.unwrap_or_else(StoredRateLimitSnapshotsFile::new);
    for (account_id, snapshot) in load_legacy_snapshot_store(codex_home)? {
        snapshot_store
            .snapshots
            .entry(account_id)
            .or_insert(snapshot);
    }
    Ok(snapshot_store)
}

fn load_shared_snapshot_store(
    codex_home: &Path,
) -> std::io::Result<Option<StoredRateLimitSnapshotsFile>> {
    let path = rate_limits_path(codex_home);
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    match serde_json::from_str::<StoredRateLimitSnapshotsFile>(&contents) {
        Ok(snapshot_store) => Ok(Some(snapshot_store)),
        Err(err) => {
            tracing::warn!(
                "ignoring invalid shared account rate-limit file {}: {err}",
                path.display()
            );
            Ok(None)
        }
    }
}

fn load_legacy_snapshot_store(
    codex_home: &Path,
) -> std::io::Result<BTreeMap<String, StoredRateLimitSnapshotFile>> {
    let dir = legacy_rate_limits_dir(codex_home);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(err),
    };

    let mut snapshots = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(account_id) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) => {
                tracing::warn!(
                    "ignoring unreadable legacy account rate-limit snapshot {}: {err}",
                    path.display()
                );
                continue;
            }
        };
        let snapshot = match serde_json::from_str::<StoredRateLimitSnapshotFile>(&contents) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                tracing::warn!(
                    "ignoring invalid legacy account rate-limit snapshot {}: {err}",
                    path.display()
                );
                continue;
            }
        };
        snapshots.insert(account_id, snapshot);
    }
    Ok(snapshots)
}

fn write_snapshot_file(
    path: &Path,
    snapshot_store: &StoredRateLimitSnapshotsFile,
) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(snapshot_store)?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(json.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn legacy_rate_limits_dir(codex_home: &Path) -> PathBuf {
    codex_home.join(LEGACY_RATE_LIMITS_DIR)
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use sha2::Digest;
    use tempfile::tempdir;

    use super::*;
    use crate::auth::AuthDotJson;
    use crate::slop_fork::auth_accounts::upsert_account;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::KnownPlan;
    use codex_login::token_data::PlanType as TokenPlanType;
    use codex_login::token_data::TokenData;
    use codex_protocol::protocol::RateLimitWindow;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid timestamp")
    }

    fn sample_snapshot(now: DateTime<Utc>, primary_used_percent: f64) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: primary_used_percent,
                window_minutes: Some(300),
                resets_at: Some((now + Duration::minutes(30)).timestamp()),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 80.0,
                window_minutes: Some(7 * 24 * 60),
                resets_at: Some((now + Duration::hours(3)).timestamp()),
            }),
            credits: None,
            plan_type: Some(AccountPlanType::Pro),
        }
    }

    fn sample_raw_snapshot(
        now: DateTime<Utc>,
        primary_used_percent: i32,
    ) -> RawRateLimitSnapshotInput {
        RawRateLimitSnapshotInput {
            primary: Some(RawRateLimitWindowSnapshot {
                used_percent: primary_used_percent,
                limit_window_seconds: 5 * 60 * 60,
                reset_after_seconds: 5 * 60 * 60,
                reset_at: (now + Duration::hours(5)).timestamp() as i32,
            }),
            secondary: Some(RawRateLimitWindowSnapshot {
                used_percent: 0,
                limit_window_seconds: 7 * 24 * 60 * 60,
                reset_after_seconds: 7 * 24 * 60 * 60,
                reset_at: (now + Duration::days(7)).timestamp() as i32,
            }),
        }
    }

    fn chatgpt_auth(account_id: &str, email: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(codex_app_server_protocol::AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some(email.to_string()),
                    chatgpt_plan_type: Some(TokenPlanType::Known(KnownPlan::Pro)),
                    chatgpt_user_id: None,
                    chatgpt_account_id: Some(account_id.to_string()),
                    raw_jwt: "jwt".to_string(),
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: Some(Utc::now()),
        }
    }

    #[test]
    fn record_and_load_rate_limit_snapshot_round_trips() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 25.0);
        let raw = sample_raw_snapshot(now, /*primary_used_percent*/ 25);

        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;
        assert_eq!(
            rate_limits_path(dir.path()),
            auth_accounts::accounts_dir(dir.path()).join(".rate-limits.json")
        );
        assert!(rate_limits_path(dir.path()).exists());

        let loaded = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            loaded,
            StoredRateLimitSnapshot {
                account_id: "acct-1".to_string(),
                plan: Some("pro".to_string()),
                snapshot: Some(snapshot),
                five_hour_window: StoredQuotaWindow {
                    used_percent: Some(25),
                    limit_window_seconds: Some(5 * 60 * 60),
                    reset_after_seconds: Some(5 * 60 * 60),
                    reset_at: Some(now + Duration::hours(5)),
                    last_touch_attempt_at: None,
                    last_touch_confirmed_at: None,
                    last_touch_reset_at: None,
                },
                weekly_window: StoredQuotaWindow {
                    used_percent: Some(0),
                    limit_window_seconds: Some(7 * 24 * 60 * 60),
                    reset_after_seconds: Some(7 * 24 * 60 * 60),
                    reset_at: Some(now + Duration::days(7)),
                    last_touch_attempt_at: None,
                    last_touch_confirmed_at: None,
                    last_touch_reset_at: None,
                },
                observed_at: Some(now),
                primary_next_reset_at: Some(now + Duration::minutes(30)),
                secondary_next_reset_at: Some(now + Duration::hours(3)),
                last_refresh_attempt_at: None,
                last_usage_limit_hit_at: None,
            }
        );
        Ok(())
    }

    #[test]
    fn multiple_accounts_share_one_rate_limits_file() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();

        record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &sample_snapshot(now, /*primary_used_percent*/ 25.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            "acct-2",
            Some("team"),
            &sample_snapshot(now, /*primary_used_percent*/ 10.0),
            now,
        )?;

        let contents = std::fs::read_to_string(rate_limits_path(dir.path()))?;
        let stored = serde_json::from_str::<StoredRateLimitSnapshotsFile>(&contents)?;
        assert_eq!(stored.snapshots.len(), 2);
        assert_eq!(
            stored.snapshots.keys().cloned().collect::<Vec<_>>(),
            vec!["acct-1".to_string(), "acct-2".to_string()]
        );
        Ok(())
    }

    #[test]
    fn legacy_rate_limit_dir_is_used_as_fallback_until_migrated() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 25.0);
        let legacy_dir = legacy_rate_limits_dir(dir.path());
        std::fs::create_dir_all(&legacy_dir)?;
        let legacy_file = legacy_dir.join("acct-1.json");
        let legacy_contents = serde_json::to_string_pretty(&StoredRateLimitSnapshotFile {
            plan: Some("pro".to_string()),
            snapshot: Some(snapshot.clone()),
            five_hour_window: StoredQuotaWindow::default(),
            weekly_window: StoredQuotaWindow::default(),
            observed_at: Some(now),
            primary_next_reset_at: Some(now + Duration::minutes(30)),
            secondary_next_reset_at: Some(now + Duration::hours(3)),
            last_refresh_attempt_at: None,
            last_usage_limit_hit_at: None,
        })?;
        std::fs::write(&legacy_file, legacy_contents)?;

        let loaded = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(loaded.account_id, "acct-1".to_string());
        assert_eq!(loaded.snapshot, Some(snapshot));

        record_usage_limit_hint(
            dir.path(),
            "acct-2",
            Some("team"),
            Some(now + Duration::minutes(45)),
            now,
        )?;
        assert!(rate_limits_path(dir.path()).exists());
        let migrated = list_rate_limit_snapshots(dir.path())?;
        assert_eq!(migrated.len(), 2);
        Ok(())
    }

    #[test]
    fn usage_limit_hint_updates_reset_and_last_hit() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let reset_at = now + Duration::minutes(45);

        record_usage_limit_hint(dir.path(), "acct-1", Some("team"), Some(reset_at), now)?;

        let loaded = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            loaded,
            StoredRateLimitSnapshot {
                account_id: "acct-1".to_string(),
                plan: Some("team".to_string()),
                snapshot: None,
                five_hour_window: StoredQuotaWindow::default(),
                weekly_window: StoredQuotaWindow::default(),
                observed_at: None,
                primary_next_reset_at: Some(reset_at),
                secondary_next_reset_at: Some(reset_at),
                last_refresh_attempt_at: None,
                last_usage_limit_hit_at: Some(now),
            }
        );
        Ok(())
    }

    #[test]
    fn snapshot_map_for_accounts_uses_legacy_workspace_ids() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth = chatgpt_auth("acct-legacy", "legacy@example.com");
        let saved_account_id = upsert_account(dir.path(), &auth)?.expect("saved account id");
        let account = auth_accounts::StoredAccount {
            id: saved_account_id.clone(),
            path: auth_accounts::accounts_dir(dir.path()).join(format!("{saved_account_id}.json")),
            auth,
            modified_at: None,
        };

        record_rate_limit_snapshot(
            dir.path(),
            "acct-legacy",
            Some("pro"),
            &sample_snapshot(now, /*primary_used_percent*/ 25.0),
            now,
        )?;

        let snapshots = snapshot_map_for_accounts(dir.path(), &[account])?;
        let snapshot = snapshots
            .get(&saved_account_id)
            .expect("legacy snapshot should resolve to saved account id");

        assert_eq!(snapshot.account_id, saved_account_id);
        assert_eq!(snapshot.plan.as_deref(), Some("pro"));
        Ok(())
    }

    #[test]
    fn snapshot_map_for_accounts_uses_legacy_email_derived_saved_ids() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth = chatgpt_auth("acct-legacy", "legacy@example.com");
        let saved_account_id = upsert_account(dir.path(), &auth)?.expect("saved account id");
        let account = auth_accounts::StoredAccount {
            id: saved_account_id.clone(),
            path: auth_accounts::accounts_dir(dir.path()).join(format!("{saved_account_id}.json")),
            auth,
            modified_at: None,
        };
        let legacy_digest = sha2::Sha256::digest(b"chatgpt:acct-legacy:legacy@example.com");
        let legacy_saved_id = format!("chatgpt-{}", &format!("{legacy_digest:x}")[..16]);

        record_rate_limit_snapshot(
            dir.path(),
            &legacy_saved_id,
            Some("pro"),
            &sample_snapshot(now, /*primary_used_percent*/ 25.0),
            now,
        )?;

        let snapshots = snapshot_map_for_accounts(dir.path(), &[account])?;
        let snapshot = snapshots
            .get(&saved_account_id)
            .expect("legacy saved-account snapshot should resolve to current saved account id");

        assert_eq!(snapshot.account_id, saved_account_id);
        assert_eq!(snapshot.plan.as_deref(), Some("pro"));
        Ok(())
    }

    #[test]
    fn mark_refresh_attempt_is_due_when_snapshot_is_stale() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        assert!(mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            /*reset_at*/ None,
            now,
            rate_limit_refresh_stale_interval(),
        )?);
        assert!(!mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            /*reset_at*/ None,
            now + Duration::minutes(5),
            rate_limit_refresh_stale_interval(),
        )?);
        assert!(mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            /*reset_at*/ None,
            now + rate_limit_refresh_stale_interval(),
            rate_limit_refresh_stale_interval(),
        )?);
        Ok(())
    }

    #[test]
    fn mark_refresh_attempt_waits_until_reset_passes() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 12.0);
        record_rate_limit_snapshot(dir.path(), "acct-1", Some("pro"), &snapshot, now)?;
        let reset_at = now + Duration::hours(3);

        assert!(!mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            Some(reset_at),
            now + Duration::hours(2),
            rate_limit_refresh_stale_interval(),
        )?);
        assert!(mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            Some(reset_at),
            reset_at,
            rate_limit_refresh_stale_interval(),
        )?);
        assert!(!mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            Some(reset_at),
            reset_at + Duration::minutes(10),
            rate_limit_refresh_stale_interval(),
        )?);
        Ok(())
    }

    #[test]
    fn refresh_due_helper_matches_recent_failed_attempt_cooldown() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot =
            sample_snapshot(now - Duration::hours(4), /*primary_used_percent*/ 12.0);
        record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            now - Duration::hours(4),
        )?;

        assert!(mark_rate_limit_refresh_attempt_if_due(
            dir.path(),
            "acct-1",
            Some("pro"),
            snapshot_reset_at(
                &load_rate_limit_snapshot(dir.path(), "acct-1")?
                    .expect("stored snapshot should exist"),
            ),
            now,
            rate_limit_refresh_stale_interval(),
        )?);

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert!(!rate_limit_refresh_is_due(
            Some(&stored),
            now + Duration::minutes(5),
            rate_limit_refresh_stale_interval(),
        ));
        assert!(rate_limit_refresh_is_due(
            Some(&stored),
            now + rate_limit_refresh_stale_interval(),
            rate_limit_refresh_stale_interval(),
        ));
        Ok(())
    }

    #[test]
    fn refresh_due_helper_waits_for_stale_interval_when_reset_is_missing() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: 12.0,
                window_minutes: Some(300),
                resets_at: None,
            }),
            secondary: None,
            credits: None,
            plan_type: Some(AccountPlanType::Pro),
        };
        record_rate_limit_snapshot(dir.path(), "acct-1", Some("pro"), &snapshot, now)?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert!(!rate_limit_refresh_is_due(
            Some(&stored),
            now + Duration::minutes(5),
            rate_limit_refresh_stale_interval(),
        ));
        assert!(rate_limit_refresh_is_due(
            Some(&stored),
            now + rate_limit_refresh_stale_interval(),
            rate_limit_refresh_stale_interval(),
        ));
        Ok(())
    }

    #[test]
    fn quota_window_state_detects_untouched_from_raw_delta() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            quota_window_state(&stored, QuotaWindowKind::FiveHour, now),
            QuotaWindowState::Untouched
        );
        assert_eq!(
            quota_window_state(&stored, QuotaWindowKind::Weekly, now),
            QuotaWindowState::Untouched
        );
        assert!(quota_window_should_start(
            &stored,
            QuotaWindowKind::FiveHour,
            now
        ));
        assert!(quota_window_should_start(
            &stored,
            QuotaWindowKind::Weekly,
            now
        ));
        Ok(())
    }

    #[test]
    fn quota_window_state_treats_one_second_reset_after_drift_as_untouched() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let mut raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        raw.secondary = Some(RawRateLimitWindowSnapshot {
            used_percent: 0,
            limit_window_seconds: 7 * 24 * 60 * 60,
            reset_after_seconds: 7 * 24 * 60 * 60 + 1,
            reset_at: (now + Duration::days(7)).timestamp() as i32,
        });
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            quota_window_state(&stored, QuotaWindowKind::Weekly, now),
            QuotaWindowState::Untouched
        );
        assert!(quota_window_should_start(
            &stored,
            QuotaWindowKind::Weekly,
            now
        ));
        Ok(())
    }

    #[test]
    fn quota_window_reset_at_if_untouched_uses_now_plus_full_window() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            quota_window_reset_at_if_untouched(&stored, QuotaWindowKind::FiveHour, now),
            Some(now + Duration::hours(5))
        );
        assert_eq!(
            quota_window_reset_at_if_untouched(&stored, QuotaWindowKind::Weekly, now),
            Some(now + Duration::days(7))
        );
        Ok(())
    }

    #[test]
    fn quota_window_state_detects_started_once_delta_or_usage_moves() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let mut raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        raw.primary = Some(RawRateLimitWindowSnapshot {
            used_percent: 1,
            limit_window_seconds: 5 * 60 * 60,
            reset_after_seconds: 5 * 60 * 60 - 60,
            reset_at: (now + Duration::hours(5) - Duration::minutes(1)).timestamp() as i32,
        });
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            quota_window_state(&stored, QuotaWindowKind::FiveHour, now),
            QuotaWindowState::Started
        );
        assert!(!quota_window_should_start(
            &stored,
            QuotaWindowKind::FiveHour,
            now
        ));
        Ok(())
    }

    #[test]
    fn quota_window_state_detects_reset_passed_from_cache() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let mut raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        raw.secondary = Some(RawRateLimitWindowSnapshot {
            used_percent: 0,
            limit_window_seconds: 7 * 24 * 60 * 60,
            reset_after_seconds: 0,
            reset_at: (now - Duration::seconds(1)).timestamp() as i32,
        });
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert_eq!(
            quota_window_state(&stored, QuotaWindowKind::Weekly, now),
            QuotaWindowState::ResetPassed
        );
        assert!(quota_window_should_start(
            &stored,
            QuotaWindowKind::Weekly,
            now
        ));
        Ok(())
    }

    #[test]
    fn recent_touch_attempt_cools_down_auto_start() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;
        mark_quota_window_touch_attempt(
            dir.path(),
            "acct-1",
            Some("pro"),
            QuotaWindowKind::Weekly,
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert!(!quota_window_should_start(
            &stored,
            QuotaWindowKind::Weekly,
            now
        ));
        assert!(quota_window_should_start(
            &stored,
            QuotaWindowKind::Weekly,
            now + Duration::seconds(TOUCH_ATTEMPT_COOLDOWN_SECS),
        ));
        Ok(())
    }

    #[test]
    fn confirmed_touch_suppresses_repeat_for_same_reset_boundary() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let snapshot = sample_snapshot(now, /*primary_used_percent*/ 0.0);
        let raw = sample_raw_snapshot(now, /*primary_used_percent*/ 0);
        record_rate_limit_snapshot_with_raw(
            dir.path(),
            "acct-1",
            Some("pro"),
            &snapshot,
            Some(&raw),
            now,
        )?;
        let reset_at = now + Duration::days(7);
        mark_quota_window_touch_confirmed(
            dir.path(),
            "acct-1",
            Some("pro"),
            QuotaWindowKind::Weekly,
            Some(reset_at),
            now,
        )?;

        let stored = load_rate_limit_snapshot(dir.path(), "acct-1")?.expect("snapshot");
        assert!(!quota_window_should_start(
            &stored,
            QuotaWindowKind::Weekly,
            now
        ));
        assert_eq!(
            quota_window_reset_at_if_untouched(&stored, QuotaWindowKind::Weekly, now),
            None
        );
        Ok(())
    }
}
