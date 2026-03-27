use std::path::Path;

use codex_core::auth::AuthDotJson;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts;
use codex_core::slop_fork::maybe_load_slop_fork_config;

use crate::bottom_pane::StatusLineItem;
use crate::status::RateLimitWindowDisplay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SavedAccountLimitKind {
    FiveHour,
    Weekly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SavedAccountLimitAverages {
    five_hour_remaining_percent: Option<i64>,
    weekly_remaining_percent: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SavedAccountStatusLineFormatter {
    averages: Option<SavedAccountLimitAverages>,
}

impl SavedAccountStatusLineFormatter {
    pub(super) fn load(codex_home: &Path, items: &[StatusLineItem]) -> Self {
        let averages = saved_account_limit_averages_for_status_line(
            codex_home,
            items.contains(&StatusLineItem::FiveHourLimit),
            items.contains(&StatusLineItem::WeeklyLimit),
        );
        Self { averages }
    }

    pub(super) fn format_limit(
        &self,
        kind: SavedAccountLimitKind,
        window: Option<&RateLimitWindowDisplay>,
        label: &str,
    ) -> Option<String> {
        let window = window?;
        let remaining = (100.0f64 - window.used_percent).clamp(0.0f64, 100.0f64);
        let average = self
            .average_remaining_percent(kind)
            .map(|remaining| format!(" ({remaining}%)"))
            .unwrap_or_default();
        Some(format!("{label} {remaining:.0}%{average}"))
    }

    fn average_remaining_percent(&self, kind: SavedAccountLimitKind) -> Option<i64> {
        match kind {
            SavedAccountLimitKind::FiveHour => self
                .averages
                .and_then(|averages| averages.five_hour_remaining_percent),
            SavedAccountLimitKind::Weekly => self
                .averages
                .and_then(|averages| averages.weekly_remaining_percent),
        }
    }
}

fn saved_account_limit_averages_for_status_line(
    codex_home: &Path,
    include_five_hour: bool,
    include_weekly: bool,
) -> Option<SavedAccountLimitAverages> {
    if !include_five_hour && !include_weekly {
        return None;
    }

    let fork_config = match maybe_load_slop_fork_config(codex_home) {
        Ok(config) => config,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "failed to load saved-account limit averages for status line"
            );
            return None;
        }
    };
    if !fork_config.show_average_account_limits_in_status_line {
        return None;
    }

    let accounts = match auth_accounts::list_accounts(codex_home) {
        Ok(accounts) => accounts,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "failed to list saved accounts for status line averages"
            );
            return None;
        }
    }
    .into_iter()
    .filter(|account| is_chatgpt_account(&account.auth))
    .collect::<Vec<_>>();
    if accounts.len() <= 1 {
        return None;
    }

    let snapshots = match account_rate_limits::snapshot_map_for_accounts(codex_home, &accounts) {
        Ok(snapshots) => snapshots,
        Err(err) => {
            tracing::debug!(
                error = %err,
                "failed to load saved-account snapshots for status line averages"
            );
            return None;
        }
    };
    let now = chrono::Utc::now();
    let remaining_for_window = |snapshot: &account_rate_limits::StoredRateLimitSnapshot,
                                kind: account_rate_limits::QuotaWindowKind,
                                used_percent: Option<f64>| {
        match account_rate_limits::quota_window_state(snapshot, kind, now) {
            account_rate_limits::QuotaWindowState::ResetPassed => Some(100.0),
            account_rate_limits::QuotaWindowState::Unknown
            | account_rate_limits::QuotaWindowState::Untouched
            | account_rate_limits::QuotaWindowState::Started => {
                used_percent.and_then(remaining_percent)
            }
        }
    };
    let averages = SavedAccountLimitAverages {
        five_hour_remaining_percent: include_five_hour
            .then(|| {
                average_remaining_percent(accounts.iter().filter_map(|account| {
                    snapshots.get(&account.id).and_then(|snapshot| {
                        remaining_for_window(
                            snapshot,
                            account_rate_limits::QuotaWindowKind::FiveHour,
                            snapshot
                                .snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.primary.as_ref())
                                .map(|window| window.used_percent),
                        )
                    })
                }))
            })
            .flatten(),
        weekly_remaining_percent: include_weekly
            .then(|| {
                average_remaining_percent(accounts.iter().filter_map(|account| {
                    snapshots.get(&account.id).and_then(|snapshot| {
                        remaining_for_window(
                            snapshot,
                            account_rate_limits::QuotaWindowKind::Weekly,
                            snapshot
                                .snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.secondary.as_ref())
                                .map(|window| window.used_percent),
                        )
                    })
                }))
            })
            .flatten(),
    };

    if averages.five_hour_remaining_percent.is_none() && averages.weekly_remaining_percent.is_none()
    {
        None
    } else {
        Some(averages)
    }
}

fn average_remaining_percent(values: impl Iterator<Item = f64>) -> Option<i64> {
    let mut sum = 0.0;
    let mut count = 0usize;
    for value in values {
        sum += value;
        count += 1;
    }
    if count == 0 {
        return None;
    }
    Some((sum / count as f64).round() as i64)
}

fn remaining_percent(used_percent: f64) -> Option<f64> {
    used_percent
        .is_finite()
        .then_some((100.0 - used_percent).clamp(0.0, 100.0))
}

fn is_chatgpt_account(auth: &AuthDotJson) -> bool {
    auth.openai_api_key.is_none() && auth.tokens.is_some()
}
