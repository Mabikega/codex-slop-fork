use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;

use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::auth::AuthDotJson;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts;

use crate::status::StatusAccountDisplay;
use crate::status::rate_limit_snapshot_display_for_limit;

pub(crate) const ACCOUNT_LIMITS_VIEW_ID: &str = "slop-fork-account-limits";

#[derive(Debug, Clone)]
pub(crate) struct SavedAccountLimitsOverview {
    pub(crate) popup_context: super::AccountsPopupContext,
    pub(crate) entries: Vec<SavedAccountLimitEntry>,
    pub(crate) due_count: usize,
    pub(crate) refreshable_account_count: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedAccountLimitEntry {
    pub(crate) account_id: String,
    pub(crate) label: String,
    pub(crate) summary: String,
    pub(crate) is_current: bool,
    pub(crate) is_due: bool,
    pub(crate) is_refreshable: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SavedAccountRateLimitsRefreshState {
    pub(crate) _started_at: Instant,
    pub(crate) target: SavedAccountRateLimitsRefreshTarget,
}

impl SavedAccountRateLimitsRefreshState {
    pub(crate) fn description(&self, due_count: usize) -> String {
        match &self.target {
            SavedAccountRateLimitsRefreshTarget::Due => match due_count {
                0 => "Refreshing due saved-account limits in the background.".to_string(),
                1 => "Refreshing 1 due saved account in the background.".to_string(),
                count => format!("Refreshing {count} due saved accounts in the background."),
            },
            SavedAccountRateLimitsRefreshTarget::Accounts(account_ids)
            | SavedAccountRateLimitsRefreshTarget::AllAccounts(account_ids) => {
                match account_ids.len() {
                    0 => "Refreshing saved-account limits in the background.".to_string(),
                    1 => "Refreshing 1 saved account in the background.".to_string(),
                    count => format!("Refreshing {count} saved accounts in the background."),
                }
            }
        }
    }

    pub(crate) fn includes_account(&self, account_id: &str, account_is_due: bool) -> bool {
        match &self.target {
            SavedAccountRateLimitsRefreshTarget::Due => account_is_due,
            SavedAccountRateLimitsRefreshTarget::Accounts(account_ids)
            | SavedAccountRateLimitsRefreshTarget::AllAccounts(account_ids) => {
                account_ids.contains(account_id)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SavedAccountRateLimitsRefreshTarget {
    Due,
    Accounts(HashSet<String>),
    AllAccounts(HashSet<String>),
}

impl SavedAccountRateLimitsRefreshTarget {
    pub(crate) fn requested_account_ids(&self) -> Option<Vec<String>> {
        match self {
            SavedAccountRateLimitsRefreshTarget::Due => None,
            SavedAccountRateLimitsRefreshTarget::Accounts(account_ids)
            | SavedAccountRateLimitsRefreshTarget::AllAccounts(account_ids) => {
                Some(account_ids.iter().cloned().collect())
            }
        }
    }
}

pub(crate) fn load_saved_account_limits_overview(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
) -> Result<SavedAccountLimitsOverview, String> {
    super::accounts::load_saved_account_limits_overview_with_context(
        codex_home,
        auth_credentials_store_mode,
        status_account_display,
    )
}

pub(crate) fn auth_dot_json_is_chatgpt(auth: &AuthDotJson) -> bool {
    auth.openai_api_key.is_none() && auth.tokens.is_some()
}

pub(crate) fn saved_account_rate_limit_refresh_is_due(
    account: &auth_accounts::StoredAccount,
    snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
    now: DateTime<Utc>,
) -> bool {
    auth_dot_json_is_chatgpt(&account.auth)
        && account_rate_limits::rate_limit_refresh_is_due(
            snapshot,
            now,
            account_rate_limits::rate_limit_refresh_stale_interval(),
        )
}

pub(super) fn saved_account_rate_limit_summary(
    account: &auth_accounts::StoredAccount,
    snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
    now: DateTime<Utc>,
) -> String {
    let mut parts = Vec::new();

    if let Some(snapshot) = snapshot {
        if let Some(rate_limit) = snapshot.snapshot.as_ref() {
            let observed_at = snapshot.observed_at.unwrap_or_else(Utc::now);
            let display = rate_limit_snapshot_display_for_limit(
                rate_limit,
                rate_limit
                    .limit_name
                    .clone()
                    .or_else(|| rate_limit.limit_id.clone())
                    .unwrap_or_else(|| "codex".to_string()),
                observed_at.with_timezone(&Local),
            );
            let format_reset = |kind: account_rate_limits::QuotaWindowKind,
                                seconds: Option<i64>| {
                let state = account_rate_limits::quota_window_state(snapshot, kind, now);
                let (reset_at, marker) = match state {
                    account_rate_limits::QuotaWindowState::ResetPassed => (
                        account_rate_limits::quota_window(snapshot, kind)
                            .limit_window_seconds
                            .filter(|seconds| *seconds > 0)
                            .map(|seconds| now + chrono::Duration::seconds(seconds)),
                        '~',
                    ),
                    _ => {
                        let untouched_reset_at =
                            account_rate_limits::quota_window_reset_at_if_untouched(
                                snapshot, kind, now,
                            );
                        let is_untouched = untouched_reset_at.is_some();
                        (
                            untouched_reset_at.or_else(|| {
                                seconds
                                    .and_then(|seconds| DateTime::<Utc>::from_timestamp(seconds, 0))
                            }),
                            if is_untouched { '~' } else { ' ' },
                        )
                    }
                };
                reset_at.map(|reset_at| {
                    let reset_at = reset_at.with_timezone(&Local);
                    format!(
                        "{} on {}{marker}",
                        reset_at.format("%H:%M"),
                        reset_at.format("%-d %b")
                    )
                })
            };
            let mut format_window = |kind: account_rate_limits::QuotaWindowKind,
                                     label: String,
                                     used_percent: f64,
                                     resets_at: Option<i64>| {
                let used_percent =
                    match account_rate_limits::quota_window_state(snapshot, kind, now) {
                        account_rate_limits::QuotaWindowState::ResetPassed => 0.0,
                        account_rate_limits::QuotaWindowState::Unknown
                        | account_rate_limits::QuotaWindowState::Untouched
                        | account_rate_limits::QuotaWindowState::Started => used_percent,
                    };
                let reset = format_reset(kind, resets_at)
                    .map(|reset| format!(" until {reset}"))
                    .unwrap_or_default();
                parts.push(format!("{label} {used_percent:>3.0}%{reset}"));
            };
            if let Some(primary) = display.primary {
                let label = primary
                    .window_minutes
                    .map(crate::chatwidget::get_limits_duration)
                    .unwrap_or_else(|| "5h".to_string());
                format_window(
                    account_rate_limits::QuotaWindowKind::FiveHour,
                    label,
                    primary.used_percent,
                    rate_limit
                        .primary
                        .as_ref()
                        .and_then(|window| window.resets_at),
                );
            }
            if let Some(secondary) = display.secondary {
                let label = secondary
                    .window_minutes
                    .map(crate::chatwidget::get_limits_duration)
                    .unwrap_or_else(|| "weekly".to_string());
                format_window(
                    account_rate_limits::QuotaWindowKind::Weekly,
                    label,
                    secondary.used_percent,
                    rate_limit
                        .secondary
                        .as_ref()
                        .and_then(|window| window.resets_at),
                );
            }
        } else if let Some(reset_at) = account_rate_limits::snapshot_reset_at(snapshot)
            && reset_at > now
        {
            parts.push(format!(
                "limited until {}",
                reset_at.with_timezone(&Local).format("%H:%M on %-d %b")
            ));
        } else if auth_dot_json_is_chatgpt(&account.auth) {
            parts.push("No snapshot yet".to_string());
        }

        if auth_dot_json_is_chatgpt(&account.auth)
            && account_rate_limits::rate_limit_refresh_is_due(
                Some(snapshot),
                now,
                account_rate_limits::rate_limit_refresh_stale_interval(),
            )
            && let Some(observed_at) = snapshot.observed_at
        {
            let age = now.signed_duration_since(observed_at);
            if age > chrono::Duration::zero() {
                parts.push(format!("stale {}m old", age.num_minutes()));
            }
        }
    } else if auth_dot_json_is_chatgpt(&account.auth) {
        parts.push("No snapshot yet".to_string());
    }

    if parts.is_empty() {
        account.id.clone()
    } else {
        parts.join(" · ")
    }
}
