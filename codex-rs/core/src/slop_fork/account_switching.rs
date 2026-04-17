use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use codex_app_server_protocol::AuthMode as ApiAuthMode;

use super::account_rate_limits;
use super::auth_accounts;
use super::auth_accounts::StoredAccount;
use crate::auth::AuthCredentialsStoreMode;
use crate::auth::load_auth_dot_json;

#[derive(Debug, Default)]
pub struct RateLimitSwitchState {
    tried_accounts: HashSet<String>,
    limited_chatgpt_accounts: HashSet<String>,
    blocked_until: HashMap<String, DateTime<Utc>>,
}

impl RateLimitSwitchState {
    pub fn mark_rate_limited(
        &mut self,
        account_id: &str,
        mode: ApiAuthMode,
        blocked_until: Option<DateTime<Utc>>,
    ) {
        self.tried_accounts.insert(account_id.to_string());
        if matches!(mode, ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens) {
            self.limited_chatgpt_accounts.insert(account_id.to_string());
        }
        if let Some(blocked_until) = blocked_until {
            self.blocked_until
                .entry(account_id.to_string())
                .and_modify(|existing| {
                    if blocked_until > *existing {
                        *existing = blocked_until;
                    }
                })
                .or_insert(blocked_until);
        }
    }

    fn blocked_until(&self, account_id: &str) -> Option<DateTime<Utc>> {
        self.blocked_until.get(account_id).copied()
    }

    fn has_tried(&self, account_id: &str) -> bool {
        self.tried_accounts.contains(account_id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct CandidateScore {
    used_percent: f64,
    weekly_reset_bucket: Option<i64>,
}

// Coarsen weekly reset ordering so small timestamp differences do not churn selection.
const WEEKLY_RESET_BUCKET_SECONDS: i64 = 21_600;

pub fn switch_active_account_on_rate_limit(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    state: &mut RateLimitSwitchState,
    allow_api_key_fallback: bool,
    failed_auth: Option<&crate::auth::CodexAuth>,
    blocked_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> std::io::Result<Option<StoredAccount>> {
    let Some(rate_limited_account) =
        load_rate_limited_account(codex_home, auth_credentials_store_mode, failed_auth)?
    else {
        return Ok(None);
    };
    state.mark_rate_limited(
        &rate_limited_account.account_id,
        rate_limited_account.mode,
        blocked_until,
    );

    let accounts = auth_accounts::list_accounts(codex_home)?;
    let snapshot_map = account_rate_limits::snapshot_map_for_accounts(codex_home, &accounts)?;
    let active_account_id =
        auth_accounts::current_active_account_id(codex_home, auth_credentials_store_mode)?;

    if let Some(active_account_id) = active_account_id.as_deref()
        && active_account_id != rate_limited_account.account_id
        && !state.has_tried(active_account_id)
        && let Some(active_account) = accounts.iter().find(|account| {
            account.id == active_account_id
                && account_has_credentials(account)
                && !auth_accounts::saved_account_subscription_ran_out(
                    account,
                    snapshot_map.get(active_account_id),
                )
                && !is_blocked(
                    now,
                    account_blocked_until(state, &snapshot_map, active_account_id),
                )
        })
    {
        return Ok(Some(active_account.clone()));
    }

    let Some(next_account) = select_next_account(
        &accounts,
        &snapshot_map,
        state,
        allow_api_key_fallback,
        now,
        &rate_limited_account.account_id,
    ) else {
        return Ok(None);
    };

    if active_account_id.as_deref() != Some(next_account.id.as_str()) {
        auth_accounts::activate_account(codex_home, &next_account.id, auth_credentials_store_mode)?;
    }
    Ok(Some(next_account))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RateLimitedAccount {
    account_id: String,
    mode: ApiAuthMode,
}

fn load_rate_limited_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    failed_auth: Option<&crate::auth::CodexAuth>,
) -> std::io::Result<Option<RateLimitedAccount>> {
    if let Some(auth) = failed_auth
        && let Some(account_id) = auth
            .auth_dot_json()
            .and_then(|auth| auth_accounts::stored_account_id(&auth))
            .or_else(|| auth.get_account_id())
    {
        return Ok(Some(RateLimitedAccount {
            account_id,
            mode: auth.api_auth_mode(),
        }));
    }

    let Some(auth) = load_current_active_auth(codex_home, auth_credentials_store_mode)? else {
        return Ok(None);
    };
    let Some(account_id) = auth_accounts::stored_account_id(&auth) else {
        return Ok(None);
    };
    Ok(Some(RateLimitedAccount {
        account_id,
        mode: auth.resolved_mode(),
    }))
}

fn select_next_account(
    accounts: &[StoredAccount],
    snapshot_map: &HashMap<String, account_rate_limits::StoredRateLimitSnapshot>,
    state: &RateLimitSwitchState,
    allow_api_key_fallback: bool,
    now: DateTime<Utc>,
    rate_limited_account_id: &str,
) -> Option<StoredAccount> {
    let mut chatgpt_accounts = Vec::new();
    let mut api_key_accounts = Vec::new();

    for account in accounts {
        if !account_has_credentials(account)
            || auth_accounts::saved_account_subscription_ran_out(
                account,
                snapshot_map.get(&account.id),
            )
        {
            continue;
        }

        match account.auth.resolved_mode() {
            ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => {
                chatgpt_accounts.push(account.clone());
            }
            ApiAuthMode::ApiKey => api_key_accounts.push(account.clone()),
        }
    }

    chatgpt_accounts.sort_by(|lhs, rhs| lhs.id.cmp(&rhs.id));
    api_key_accounts.sort_by(|lhs, rhs| lhs.id.cmp(&rhs.id));

    let mut best_chatgpt: Option<(StoredAccount, CandidateScore)> = None;
    for account in &chatgpt_accounts {
        if account.id == rate_limited_account_id || state.has_tried(&account.id) {
            continue;
        }

        let blocked_until = account_blocked_until(state, snapshot_map, &account.id);
        if is_blocked(now, blocked_until) {
            continue;
        }

        let score = CandidateScore {
            used_percent: snapshot_map
                .get(&account.id)
                .and_then(account_rate_limits::snapshot_used_percent)
                .unwrap_or(0.0),
            weekly_reset_bucket: snapshot_map
                .get(&account.id)
                .and_then(|snapshot| snapshot.weekly_window.reset_at)
                .map(|reset_at| reset_at.timestamp().div_euclid(WEEKLY_RESET_BUCKET_SECONDS)),
        };
        match &best_chatgpt {
            None => best_chatgpt = Some((account.clone(), score)),
            Some((best_account, best_score)) => {
                let score_is_better = if score.used_percent < best_score.used_percent {
                    true
                } else if score.used_percent > best_score.used_percent {
                    false
                } else {
                    match (score.weekly_reset_bucket, best_score.weekly_reset_bucket) {
                        (Some(score_bucket), Some(best_bucket)) if score_bucket != best_bucket => {
                            score_bucket < best_bucket
                        }
                        _ => account.id < best_account.id,
                    }
                };
                if score_is_better {
                    best_chatgpt = Some((account.clone(), score));
                }
            }
        }
    }

    if let Some((account, _)) = best_chatgpt {
        return Some(account);
    }

    if !allow_api_key_fallback {
        return None;
    }

    let all_chatgpt_unavailable = chatgpt_accounts.iter().all(|account| {
        let blocked_until = account_blocked_until(state, snapshot_map, &account.id);
        let blocked = is_blocked(now, blocked_until);
        blocked
            || (state.has_tried(&account.id)
                && state.limited_chatgpt_accounts.contains(&account.id))
    });
    if !chatgpt_accounts.is_empty() && !all_chatgpt_unavailable {
        return None;
    }

    for account in api_key_accounts {
        if account.id == rate_limited_account_id || state.has_tried(&account.id) {
            continue;
        }
        return Some(account);
    }

    None
}

fn snapshot_blocked_until(
    snapshot: &account_rate_limits::StoredRateLimitSnapshot,
) -> Option<DateTime<Utc>> {
    // Only consider the account blocked from stored snapshot data if it actually
    // hit a usage limit within the current rate-limit window. Without this check,
    // every account with a future window-reset time (e.g. a weekly window that
    // resets in ~7 days) would appear "blocked" even at 0% usage.
    let hit_at = snapshot.last_usage_limit_hit_at?;
    let reset_at = account_rate_limits::snapshot_reset_at(snapshot)?;
    (hit_at < reset_at).then_some(reset_at)
}

fn account_blocked_until(
    state: &RateLimitSwitchState,
    snapshot_map: &HashMap<String, account_rate_limits::StoredRateLimitSnapshot>,
    account_id: &str,
) -> Option<DateTime<Utc>> {
    state
        .blocked_until(account_id)
        .into_iter()
        .chain(
            snapshot_map
                .get(account_id)
                .and_then(snapshot_blocked_until),
        )
        .max()
}

fn account_has_credentials(account: &StoredAccount) -> bool {
    match account.auth.resolved_mode() {
        ApiAuthMode::Chatgpt | ApiAuthMode::ChatgptAuthTokens => account.auth.tokens.is_some(),
        ApiAuthMode::ApiKey => account.auth.openai_api_key.is_some(),
    }
}

fn is_blocked(now: DateTime<Utc>, blocked_until: Option<DateTime<Utc>>) -> bool {
    blocked_until.is_some_and(|blocked_until| blocked_until > now)
}

fn load_current_active_auth(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<crate::auth::AuthDotJson>> {
    if let Some(auth) = load_auth_dot_json(codex_home, AuthCredentialsStoreMode::Ephemeral)? {
        return Ok(Some(auth));
    }
    if auth_credentials_store_mode == AuthCredentialsStoreMode::Ephemeral {
        return Ok(None);
    }
    load_auth_dot_json(codex_home, auth_credentials_store_mode)
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use chrono::Duration;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::auth::AuthDotJson;
    use crate::slop_fork::account_rate_limits::record_rate_limit_snapshot;
    use crate::slop_fork::account_rate_limits::record_usage_limit_hint;
    use crate::slop_fork::auth_accounts::upsert_account;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::TokenData;
    use codex_protocol::account::PlanType;
    use codex_protocol::protocol::RateLimitSnapshot;
    use codex_protocol::protocol::RateLimitWindow;

    fn fake_jwt(email: &str, account_id: &str) -> String {
        #[derive(Serialize)]
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

    fn chatgpt_auth(account_id: &str, email: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(ApiAuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some(email.to_string()),
                    chatgpt_plan_type: None,
                    chatgpt_user_id: None,
                    chatgpt_account_id: Some(account_id.to_string()),
                    raw_jwt: fake_jwt(email, account_id),
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: Some(Utc::now()),
            agent_identity: None,
        }
    }

    fn api_key_auth(suffix: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(ApiAuthMode::ApiKey),
            openai_api_key: Some(format!("sk-test-{suffix}")),
            tokens: None,
            last_refresh: None,
            agent_identity: None,
        }
    }

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid timestamp")
    }

    fn sample_snapshot(now: DateTime<Utc>, used_percent: f64) -> RateLimitSnapshot {
        sample_snapshot_with_weekly_reset(now, used_percent, now + Duration::hours(5))
    }

    fn sample_snapshot_with_weekly_reset(
        now: DateTime<Utc>,
        used_percent: f64,
        weekly_reset_at: DateTime<Utc>,
    ) -> RateLimitSnapshot {
        RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent,
                window_minutes: Some(300),
                resets_at: Some((now + Duration::hours(1)).timestamp()),
            }),
            secondary: Some(RateLimitWindow {
                used_percent,
                window_minutes: Some(7 * 24 * 60),
                resets_at: Some(weekly_reset_at.timestamp()),
            }),
            credits: None,
            plan_type: Some(PlanType::Pro),
        }
    }

    #[test]
    fn switches_to_lowest_usage_chatgpt_account() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_c = chatgpt_auth("acct-c", "c@example.com");
        let account_a = upsert_account(dir.path(), &auth_a)?.expect("account a");
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        let account_c = upsert_account(dir.path(), &auth_c)?.expect("account c");
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_a,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 60.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_b,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 20.0),
            now,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_c);
        Ok(())
    }

    #[test]
    fn prefers_earlier_weekly_reset_bucket_when_usage_is_tied() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_z = chatgpt_auth("acct-z", "z@example.com");
        let account_a = upsert_account(dir.path(), &auth_a)?.expect("account a");
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        let account_z = upsert_account(dir.path(), &auth_z)?.expect("account z");
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_a,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 90.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_b,
            Some("pro"),
            &sample_snapshot_with_weekly_reset(
                now,
                /*used_percent*/ 20.0,
                now + Duration::hours(20),
            ),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_z,
            Some("pro"),
            &sample_snapshot_with_weekly_reset(
                now,
                /*used_percent*/ 20.0,
                now + Duration::hours(10),
            ),
            now,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_z);
        Ok(())
    }

    #[test]
    fn treats_weekly_resets_in_same_six_hour_bucket_as_tied() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_z = chatgpt_auth("acct-z", "z@example.com");
        let account_a = upsert_account(dir.path(), &auth_a)?.expect("account a");
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        let account_z = upsert_account(dir.path(), &auth_z)?.expect("account z");
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_a,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 90.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_b,
            Some("pro"),
            &sample_snapshot_with_weekly_reset(
                now,
                /*used_percent*/ 20.0,
                now + Duration::hours(11),
            ),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_z,
            Some("pro"),
            &sample_snapshot_with_weekly_reset(
                now,
                /*used_percent*/ 20.0,
                now + Duration::hours(10),
            ),
            now,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_b);
        Ok(())
    }

    #[test]
    fn does_not_switch_to_chatgpt_account_whose_subscription_ran_out() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let active_auth = chatgpt_auth("acct-active", "active@example.com");
        let expired_auth = chatgpt_auth("acct-expired", "expired@example.com");
        let active_id = upsert_account(dir.path(), &active_auth)?.expect("active id");
        let expired_id = upsert_account(dir.path(), &expired_auth)?.expect("expired id");
        crate::auth::save_auth(dir.path(), &active_auth, AuthCredentialsStoreMode::File)?;
        record_rate_limit_snapshot(
            dir.path(),
            &active_id,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 80.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &expired_id,
            Some("free"),
            &sample_snapshot(now, /*used_percent*/ 0.0),
            now,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            now,
        )?;

        assert_eq!(next, None);
        Ok(())
    }

    #[test]
    fn falls_back_to_api_key_only_when_all_chatgpt_accounts_are_unavailable() -> anyhow::Result<()>
    {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_api = api_key_auth("9999");
        upsert_account(dir.path(), &auth_a)?;
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        let account_api = upsert_account(dir.path(), &auth_api)?.expect("api account");
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;

        let mut state = RateLimitSwitchState::default();
        state.mark_rate_limited(
            &account_b,
            ApiAuthMode::Chatgpt,
            Some(now + Duration::hours(1)),
        );

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut state,
            /*allow_api_key_fallback*/ true,
            /*failed_auth*/ None,
            Some(now + Duration::hours(1)),
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_api);
        Ok(())
    }

    #[test]
    fn does_not_fall_back_to_api_key_while_chatgpt_candidate_is_still_available()
    -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_api = api_key_auth("9999");
        upsert_account(dir.path(), &auth_a)?;
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        upsert_account(dir.path(), &auth_api)?;
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ true,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_b);
        Ok(())
    }

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => unsafe {
                    std::env::set_var(self.key, previous);
                },
                None => unsafe {
                    std::env::remove_var(self.key);
                },
            }
        }
    }

    #[test]
    #[serial]
    fn switches_saved_chatgpt_accounts_even_when_codex_api_key_env_is_set() -> anyhow::Result<()> {
        let _guard = ScopedEnvVar::set(crate::auth::CODEX_API_KEY_ENV_VAR, "sk-env");
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        upsert_account(dir.path(), &auth_a)?;
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            Some(now + Duration::hours(1)),
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_b);
        Ok(())
    }

    #[test]
    fn switches_when_all_accounts_have_stored_snapshots_with_future_resets() -> anyhow::Result<()> {
        // Regression: pre-populated rate-limit snapshots (e.g. from a TUI
        // refresh) set primary/secondary_next_reset_at in the future even for
        // accounts at 0% usage. The switcher must NOT treat those as "blocked".
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_c = chatgpt_auth("acct-c", "c@example.com");
        let account_a = upsert_account(dir.path(), &auth_a)?.expect("account a");
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        let account_c = upsert_account(dir.path(), &auth_c)?.expect("account c");
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;

        // All three accounts have stored snapshots with future reset times.
        // Account A (active) is at 100%, B at 5%, C at 0%.
        record_rate_limit_snapshot(
            dir.path(),
            &account_a,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 100.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_b,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 5.0),
            now,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_c,
            Some("pro"),
            &sample_snapshot(now, /*used_percent*/ 0.0),
            now,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            now,
        )?
        .expect("should switch despite all accounts having future reset times");

        // Account C has the lowest usage (0%) and should be selected.
        assert_eq!(next.id, account_c);
        Ok(())
    }

    #[test]
    fn switches_to_account_after_refreshed_snapshot_clears_expired_limit_hint() -> anyhow::Result<()>
    {
        let dir = tempdir()?;
        let hint_at = fixed_now();
        let refreshed_at = hint_at + Duration::hours(1);
        let hinted_reset_at = hint_at + Duration::minutes(45);
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        upsert_account(dir.path(), &auth_a)?;
        crate::auth::save_auth(dir.path(), &auth_a, AuthCredentialsStoreMode::File)?;

        record_usage_limit_hint(
            dir.path(),
            &account_b,
            Some("pro"),
            Some(hinted_reset_at),
            hint_at,
        )?;
        record_rate_limit_snapshot(
            dir.path(),
            &account_b,
            Some("pro"),
            &sample_snapshot(refreshed_at, /*used_percent*/ 0.0),
            refreshed_at,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            /*failed_auth*/ None,
            /*blocked_until*/ None,
            refreshed_at,
        )?
        .expect("should switch after refreshed snapshot clears expired limit hint");

        assert_eq!(next.id, account_b);
        Ok(())
    }

    #[test]
    fn reuses_active_account_switched_by_another_turn() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth_a = chatgpt_auth("acct-a", "a@example.com");
        let auth_b = chatgpt_auth("acct-b", "b@example.com");
        let auth_c = chatgpt_auth("acct-c", "c@example.com");
        upsert_account(dir.path(), &auth_a)?;
        let account_b = upsert_account(dir.path(), &auth_b)?.expect("account b");
        upsert_account(dir.path(), &auth_c)?;
        crate::auth::save_auth(dir.path(), &auth_b, AuthCredentialsStoreMode::File)?;
        let failed_auth = crate::auth::CodexAuth::from_saved_account(
            dir.path(),
            auth_a,
            AuthCredentialsStoreMode::File,
        )?;

        let next = switch_active_account_on_rate_limit(
            dir.path(),
            AuthCredentialsStoreMode::File,
            &mut RateLimitSwitchState::default(),
            /*allow_api_key_fallback*/ false,
            Some(&failed_auth),
            Some(now + Duration::hours(1)),
            now,
        )?
        .expect("switched");

        assert_eq!(next.id, account_b);
        assert_eq!(
            auth_accounts::current_active_account_id(dir.path(), AuthCredentialsStoreMode::File)?,
            Some(account_b),
        );
        Ok(())
    }
}
