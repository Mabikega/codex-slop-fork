use std::path::Path;

use codex_core::auth::AuthDotJson;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts;
use codex_core::slop_fork::maybe_load_slop_fork_config;

use crate::bottom_pane::StatusLineItem;
use crate::status::RateLimitWindowDisplay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SavedAccountLimitKind {
    FiveHour,
    Weekly,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SavedAccountLimitAverages {
    five_hour_remaining_percent: Option<i64>,
    weekly_remaining_percent: Option<i64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SavedAccountStatusLineFormatter {
    averages: Option<SavedAccountLimitAverages>,
}

impl SavedAccountStatusLineFormatter {
    pub(crate) fn load(codex_home: &Path, items: &[StatusLineItem]) -> Self {
        let averages = saved_account_limit_averages_for_status_line(
            codex_home,
            items.contains(&StatusLineItem::FiveHourLimit),
            items.contains(&StatusLineItem::WeeklyLimit),
        );
        Self { averages }
    }

    pub(crate) fn format_limit(
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

#[cfg(test)]
mod tests {
    use super::SavedAccountLimitAverages;
    use super::SavedAccountLimitKind;
    use super::SavedAccountStatusLineFormatter;
    use super::saved_account_limit_averages_for_status_line;
    use base64::Engine as _;
    use chrono::Utc;
    use codex_app_server_protocol::AuthMode;
    use codex_core::auth::AuthDotJson;
    use codex_core::slop_fork::account_rate_limits;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::TokenData;
    use codex_protocol::protocol::RateLimitSnapshot;
    use codex_protocol::protocol::RateLimitWindow;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use crate::bottom_pane::StatusLineItem;
    use crate::status::RateLimitWindowDisplay;

    fn fake_jwt(email: &str, account_id: &str) -> String {
        #[derive(serde::Serialize)]
        struct Header {
            alg: &'static str,
            typ: &'static str,
        }

        let header = Header {
            alg: "none",
            typ: "JWT",
        };
        let payload = serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": {
                "chatgpt_plan_type": "pro",
                "chatgpt_account_id": account_id,
            }
        });

        fn b64url_no_pad(bytes: &[u8]) -> String {
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        }

        let header_b64 = b64url_no_pad(&serde_json::to_vec(&header).expect("header"));
        let payload_b64 = b64url_no_pad(&serde_json::to_vec(&payload).expect("payload"));
        let signature_b64 = b64url_no_pad(b"sig");
        format!("{header_b64}.{payload_b64}.{signature_b64}")
    }

    fn chatgpt_auth_dot_json(account_id: &str, email: &str) -> AuthDotJson {
        let mut id_token = IdTokenInfo::default();
        id_token.email = Some(email.to_string());
        id_token.chatgpt_account_id = Some(account_id.to_string());
        id_token.raw_jwt = fake_jwt(email, account_id);

        AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token,
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: Some(Utc::now()),
        }
    }

    fn rate_limit_snapshot(
        primary_used_percent: f64,
        secondary_used_percent: f64,
    ) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: primary_used_percent,
                window_minutes: Some(300),
                resets_at: None,
            }),
            secondary: Some(RateLimitWindow {
                used_percent: secondary_used_percent,
                window_minutes: Some(10_080),
                resets_at: None,
            }),
            credits: None,
            plan_type: None,
        }
    }

    #[test]
    fn does_not_report_average_for_single_chatgpt_account() {
        let dir = tempdir().expect("temp dir");
        let auth = chatgpt_auth_dot_json("acct-1", "one@example.com");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &auth)
            .expect("save account");
        codex_core::slop_fork::update_slop_fork_config(dir.path(), |config| {
            config.show_average_account_limits_in_status_line = true;
        })
        .expect("enable status line averages");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &rate_limit_snapshot(
                /*primary_used_percent*/ 5.0, /*secondary_used_percent*/ 1.0,
            ),
            Utc::now(),
        )
        .expect("record rate limits");

        assert_eq!(
            saved_account_limit_averages_for_status_line(
                dir.path(),
                /*include_five_hour*/ true,
                /*include_weekly*/ true
            ),
            None
        );
    }

    #[test]
    fn averages_remaining_percent_across_saved_chatgpt_accounts() {
        let dir = tempdir().expect("temp dir");
        let first = chatgpt_auth_dot_json("acct-1", "one@example.com");
        let second = chatgpt_auth_dot_json("acct-2", "two@example.com");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &first)
            .expect("save first account");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &second)
            .expect("save second account");
        codex_core::slop_fork::update_slop_fork_config(dir.path(), |config| {
            config.show_average_account_limits_in_status_line = true;
        })
        .expect("enable status line averages");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &rate_limit_snapshot(
                /*primary_used_percent*/ 40.0, /*secondary_used_percent*/ 80.0,
            ),
            Utc::now(),
        )
        .expect("record first rate limits");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-2",
            Some("pro"),
            &rate_limit_snapshot(
                /*primary_used_percent*/ 20.0, /*secondary_used_percent*/ 60.0,
            ),
            Utc::now(),
        )
        .expect("record second rate limits");

        assert_eq!(
            saved_account_limit_averages_for_status_line(
                dir.path(),
                /*include_five_hour*/ true,
                /*include_weekly*/ true
            ),
            Some(SavedAccountLimitAverages {
                five_hour_remaining_percent: Some(70),
                weekly_remaining_percent: Some(30),
            })
        );
    }

    #[test]
    fn averages_only_accounts_with_available_window_data() {
        let dir = tempdir().expect("temp dir");
        let first = chatgpt_auth_dot_json("acct-1", "one@example.com");
        let second = chatgpt_auth_dot_json("acct-2", "two@example.com");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &first)
            .expect("save first account");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &second)
            .expect("save second account");
        codex_core::slop_fork::update_slop_fork_config(dir.path(), |config| {
            config.show_average_account_limits_in_status_line = true;
        })
        .expect("enable status line averages");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &rate_limit_snapshot(
                /*primary_used_percent*/ 10.0, /*secondary_used_percent*/ 30.0,
            ),
            Utc::now(),
        )
        .expect("record first rate limits");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-2",
            Some("pro"),
            &RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: Some("codex".to_string()),
                primary: None,
                secondary: None,
                credits: None,
                plan_type: None,
            },
            Utc::now(),
        )
        .expect("record second rate limits");

        assert_eq!(
            saved_account_limit_averages_for_status_line(
                dir.path(),
                /*include_five_hour*/ true,
                /*include_weekly*/ false
            ),
            Some(SavedAccountLimitAverages {
                five_hour_remaining_percent: Some(90),
                weekly_remaining_percent: None,
            })
        );
    }

    #[test]
    fn reset_passed_windows_count_as_fully_remaining_in_average() {
        let dir = tempdir().expect("temp dir");
        let first = chatgpt_auth_dot_json("acct-1", "one@example.com");
        let second = chatgpt_auth_dot_json("acct-2", "two@example.com");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &first)
            .expect("save first account");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &second)
            .expect("save second account");
        codex_core::slop_fork::update_slop_fork_config(dir.path(), |config| {
            config.show_average_account_limits_in_status_line = true;
        })
        .expect("enable status line averages");

        let now = Utc::now();
        let expired_observed_at = now - chrono::Duration::hours(6);
        let expired_reset_at = now - chrono::Duration::hours(1);
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: Some("codex".to_string()),
                primary: Some(RateLimitWindow {
                    used_percent: 100.0,
                    window_minutes: Some(300),
                    resets_at: Some(expired_reset_at.timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: None,
            },
            expired_observed_at,
        )
        .expect("record expired rate limits");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-2",
            Some("pro"),
            &RateLimitSnapshot {
                limit_id: Some("codex".to_string()),
                limit_name: Some("codex".to_string()),
                primary: Some(RateLimitWindow {
                    used_percent: 20.0,
                    window_minutes: Some(300),
                    resets_at: Some((now + chrono::Duration::hours(5)).timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: None,
            },
            now,
        )
        .expect("record active rate limits");

        assert_eq!(
            saved_account_limit_averages_for_status_line(
                dir.path(),
                /*include_five_hour*/ true,
                /*include_weekly*/ false
            ),
            Some(SavedAccountLimitAverages {
                five_hour_remaining_percent: Some(90),
                weekly_remaining_percent: None,
            })
        );
    }

    #[test]
    fn formats_saved_account_limit_suffix_via_formatter() {
        let dir = tempdir().expect("temp dir");
        let first = chatgpt_auth_dot_json("acct-1", "one@example.com");
        let second = chatgpt_auth_dot_json("acct-2", "two@example.com");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &first)
            .expect("save first account");
        codex_core::slop_fork::auth_accounts::upsert_account(dir.path(), &second)
            .expect("save second account");
        codex_core::slop_fork::update_slop_fork_config(dir.path(), |config| {
            config.show_average_account_limits_in_status_line = true;
        })
        .expect("enable status line averages");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-1",
            Some("pro"),
            &rate_limit_snapshot(
                /*primary_used_percent*/ 40.0, /*secondary_used_percent*/ 80.0,
            ),
            Utc::now(),
        )
        .expect("record first rate limits");
        account_rate_limits::record_rate_limit_snapshot(
            dir.path(),
            "acct-2",
            Some("pro"),
            &rate_limit_snapshot(
                /*primary_used_percent*/ 20.0, /*secondary_used_percent*/ 60.0,
            ),
            Utc::now(),
        )
        .expect("record second rate limits");

        let formatter = SavedAccountStatusLineFormatter::load(
            dir.path(),
            &[StatusLineItem::FiveHourLimit, StatusLineItem::WeeklyLimit],
        );
        let window = RateLimitWindowDisplay {
            used_percent: 5.0,
            resets_at: None,
            window_minutes: Some(300),
        };

        assert_eq!(
            formatter.format_limit(SavedAccountLimitKind::FiveHour, Some(&window), "5h"),
            Some("5h 95% (70%)".to_string())
        );
    }
}
