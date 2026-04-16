pub mod account_rate_limits;
mod account_switching;
pub mod auth_accounts;
mod auth_sync;
pub mod automation;
pub mod autoresearch;
mod config;
pub mod pilot;
mod saved_account_auth;

pub const FORK_DISPLAY_NAME: &str = "Codex Slop Fork";

use std::fs::OpenOptions;
use std::path::Path;
use std::time::Duration;

use account_rate_limits::RawRateLimitSnapshotInput;
use chrono::DateTime;
use chrono::Utc;
use codex_protocol::protocol::RateLimitSnapshot;
use once_cell::sync::Lazy;

use crate::ModelClientSession;
use crate::auth;
use crate::auth::AuthCredentialsStoreMode;
use crate::auth::AuthDotJson;
use crate::auth::AuthManager;
use crate::codex::Session;
use crate::codex::TurnContext;
use codex_protocol::error::CodexErr;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;

pub(crate) use account_switching::RateLimitSwitchState;
pub use auth_sync::ExternalAuthSyncOutcome;
pub use config::SlopForkConfig;
pub(crate) use config::extend_project_doc_paths;
pub(crate) use config::load_project_doc_overlay;
pub use config::load_slop_fork_config;
pub use config::maybe_load_slop_fork_config;
pub use config::update_slop_fork_config;
pub use saved_account_auth::auth_for_saved_account_file;
pub use saved_account_auth::refresh_saved_account_auth_from_authority;

static ACCOUNT_SWITCH_MUTEX: Lazy<tokio::sync::Mutex<()>> =
    Lazy::new(|| tokio::sync::Mutex::new(()));
const ACCOUNT_SWITCH_LOCK_RETRIES: usize = 10;
const ACCOUNT_SWITCH_LOCK_RETRY_SLEEP: Duration = Duration::from_millis(100);

pub(crate) fn save_auth_with_account_sync(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    let storage_mode = auth.storage_mode(auth_credentials_store_mode);
    let previous_auth = match auth::load_auth_dot_json(codex_home, storage_mode) {
        Ok(previous_auth) => previous_auth,
        Err(err) => {
            tracing::warn!("failed to load previous auth before saving updated auth: {err}");
            None
        }
    };
    AuthManager::record_expected_external_auth_transition_for_fork(
        codex_home,
        previous_auth.as_ref(),
        Some(auth),
    );
    if let Err(err) =
        auth_accounts::ensure_current_active_account_saved(codex_home, auth_credentials_store_mode)
    {
        tracing::warn!("failed to preserve current active account in .accounts: {err}");
    }
    auth::save_auth(codex_home, auth, storage_mode)?;
    if let Err(err) = auth_accounts::upsert_account(codex_home, auth) {
        tracing::warn!("failed to mirror saved account into .accounts: {err}");
    }
    Ok(())
}

/// Persist interactive login auth while preserving the previously active account in `.accounts/`.
pub fn persist_login_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    save_auth_with_account_sync(codex_home, auth, auth_credentials_store_mode)
}

pub fn sync_external_auth_if_enabled(auth_manager: &AuthManager) -> ExternalAuthSyncOutcome {
    auth_sync::sync_external_auth_if_enabled(auth_manager)
}

pub fn take_external_auth_switch_notice(auth_manager: &AuthManager) -> Option<String> {
    auth_manager.take_external_auth_switch_notice_for_fork()
}

async fn with_account_switch_lock<T, F>(codex_home: &Path, action: F) -> std::io::Result<T>
where
    F: FnOnce() -> std::io::Result<T>,
{
    let _process_guard = ACCOUNT_SWITCH_MUTEX.lock().await;
    let lock_path = auth_accounts::account_switch_lock_path(codex_home);
    if let Some(parent) = lock_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let lock_file = options.open(&lock_path)?;

    for _ in 0..ACCOUNT_SWITCH_LOCK_RETRIES {
        match lock_file.try_lock() {
            Ok(()) => return action(),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(ACCOUNT_SWITCH_LOCK_RETRY_SLEEP).await;
            }
            Err(err) => return Err(err.into()),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::WouldBlock,
        format!(
            "timed out waiting for account switch lock {}",
            lock_path.display()
        ),
    ))
}

pub(crate) async fn maybe_switch_account_for_rate_limit(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut ModelClientSession,
    state: &mut RateLimitSwitchState,
    failed_auth: Option<&crate::auth::CodexAuth>,
    blocked_until: Option<DateTime<Utc>>,
) -> bool {
    let fork_config = match load_slop_fork_config(&turn_context.config.codex_home) {
        Ok(config) => config,
        Err(err) => {
            tracing::warn!("failed to load fork config for account switching: {err}");
            return false;
        }
    };

    if !fork_config.auto_switch_accounts_on_rate_limit {
        return false;
    }

    let next_account = match with_account_switch_lock(&turn_context.config.codex_home, || {
        account_switching::switch_active_account_on_rate_limit(
            &turn_context.config.codex_home,
            turn_context.config.cli_auth_credentials_store_mode,
            state,
            fork_config.api_key_fallback_on_all_accounts_limited,
            failed_auth,
            blocked_until,
            Utc::now(),
        )
    })
    .await
    {
        Ok(next_account) => next_account,
        Err(err) => {
            tracing::warn!("failed to switch accounts after rate limit: {err}");
            return false;
        }
    };

    let Some(next_account) = next_account else {
        return false;
    };

    match auth::auth_for_saved_account(
        &turn_context.config.codex_home,
        next_account.auth.clone(),
        turn_context.config.cli_auth_credentials_store_mode,
    ) {
        Ok(auth) => {
            sess.services.auth_manager.set_cached_auth_for_switch(auth);
        }
        Err(err) => {
            tracing::warn!("failed to load switched account into auth manager cache: {err}");
            sess.services.auth_manager.reload();
        }
    }
    *client_session = sess.services.model_client.new_session();
    let display_labels =
        auth_accounts::load_account_display_labels(&turn_context.config.codex_home);
    sess.send_event(
        turn_context,
        EventMsg::Warning(WarningEvent {
            message: format!(
                "Switched to saved account {} after hitting a rate limit.",
                display_labels.label_for_account(&next_account)
            ),
        }),
    )
    .await;
    true
}

pub fn record_rate_limit_snapshot_for_auth(
    codex_home: &Path,
    auth: &crate::auth::CodexAuth,
    snapshot: &RateLimitSnapshot,
) {
    record_rate_limit_snapshot_for_auth_with_raw(codex_home, auth, snapshot, /*raw*/ None);
}

pub fn record_rate_limit_snapshot_for_auth_with_raw(
    codex_home: &Path,
    auth: &crate::auth::CodexAuth,
    snapshot: &RateLimitSnapshot,
    raw: Option<&RawRateLimitSnapshotInput>,
) {
    let Some(account_id) = saved_account_id(auth) else {
        return;
    };
    let plan = snapshot
        .plan_type
        .or_else(|| auth.account_plan_type())
        .map(account_rate_limits::plan_label);
    if let Err(err) = account_rate_limits::record_rate_limit_snapshot_with_raw(
        codex_home,
        &account_id,
        plan,
        snapshot,
        raw,
        Utc::now(),
    ) {
        tracing::warn!("failed to persist account rate-limit snapshot: {err}");
    }
}

pub(crate) fn record_active_account_rate_limit_snapshot(
    codex_home: &Path,
    auth_manager: &AuthManager,
    snapshot: &RateLimitSnapshot,
) {
    let Some(auth) = auth_manager.auth_cached() else {
        return;
    };
    record_rate_limit_snapshot_for_auth(codex_home, &auth, snapshot);
}

pub fn record_usage_limit_hint_for_auth(
    codex_home: &Path,
    auth: &crate::auth::CodexAuth,
    reset_at: Option<DateTime<Utc>>,
) {
    let Some(account_id) = saved_account_id(auth) else {
        return;
    };
    let plan = auth
        .account_plan_type()
        .map(account_rate_limits::plan_label);
    if let Err(err) = account_rate_limits::record_usage_limit_hint(
        codex_home,
        &account_id,
        plan,
        reset_at,
        Utc::now(),
    ) {
        tracing::warn!("failed to persist usage-limit hint: {err}");
    }
}

pub(crate) fn record_active_usage_limit_hint(
    codex_home: &Path,
    auth_manager: &AuthManager,
    reset_at: Option<DateTime<Utc>>,
) {
    let Some(auth) = auth_manager.auth_cached() else {
        return;
    };
    record_usage_limit_hint_for_auth(codex_home, &auth, reset_at);
}

pub(crate) async fn handle_usage_limit_error(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut ModelClientSession,
    state: &mut RateLimitSwitchState,
    rate_limits: Option<&RateLimitSnapshot>,
    reset_at: Option<DateTime<Utc>>,
) -> bool {
    let request_auth = client_session.last_request_auth();
    if let Some(rate_limits) = rate_limits {
        sess.update_rate_limits_for_auth(turn_context, rate_limits.clone(), request_auth.as_ref())
            .await;
    }
    record_usage_limit_hint(
        &turn_context.config.codex_home,
        sess.services.auth_manager.as_ref(),
        request_auth.as_ref(),
        reset_at,
    );
    maybe_switch_account_for_rate_limit(
        sess,
        turn_context,
        client_session,
        state,
        request_auth.as_ref(),
        reset_at,
    )
    .await
}

pub(crate) async fn handle_too_many_requests_retry_limit(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut ModelClientSession,
    state: &mut RateLimitSwitchState,
) -> bool {
    let request_auth = client_session.last_request_auth();
    record_usage_limit_hint(
        &turn_context.config.codex_home,
        sess.services.auth_manager.as_ref(),
        request_auth.as_ref(),
        /*reset_at*/ None,
    );
    maybe_switch_account_for_rate_limit(
        sess,
        turn_context,
        client_session,
        state,
        request_auth.as_ref(),
        /*blocked_until*/ None,
    )
    .await
}

pub(crate) async fn maybe_switch_account_for_retryable_limit_error(
    sess: &Session,
    turn_context: &TurnContext,
    client_session: &mut ModelClientSession,
    state: &mut RateLimitSwitchState,
    err: &CodexErr,
) -> bool {
    match err {
        CodexErr::UsageLimitReached(usage_limit) => {
            handle_usage_limit_error(
                sess,
                turn_context,
                client_session,
                state,
                usage_limit.rate_limits.as_deref(),
                usage_limit.resets_at,
            )
            .await
        }
        CodexErr::RetryLimit(retry) if retry.status == reqwest::StatusCode::TOO_MANY_REQUESTS => {
            handle_too_many_requests_retry_limit(sess, turn_context, client_session, state).await
        }
        _ => false,
    }
}

fn record_usage_limit_hint(
    codex_home: &Path,
    auth_manager: &AuthManager,
    auth: Option<&crate::auth::CodexAuth>,
    reset_at: Option<DateTime<Utc>>,
) {
    if let Some(auth) = auth {
        record_usage_limit_hint_for_auth(codex_home, auth, reset_at);
    } else {
        record_active_usage_limit_hint(codex_home, auth_manager, reset_at);
    }
}

fn saved_account_id(auth: &crate::auth::CodexAuth) -> Option<String> {
    auth.auth_dot_json()
        .and_then(|auth| auth_accounts::stored_account_id(&auth))
        .or_else(|| auth.get_account_id())
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    use super::*;
    use crate::slop_fork::auth_accounts::upsert_account;
    use codex_app_server_protocol::AuthMode;
    use codex_login::AuthManager;
    use codex_login::CodexAuth;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::TokenData;
    use codex_protocol::account::PlanType as AccountPlanType;
    use codex_protocol::auth::KnownPlan;
    use codex_protocol::auth::PlanType;
    use codex_protocol::protocol::RateLimitWindow;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid timestamp")
    }

    fn chatgpt_auth(account_id: &str, email: &str) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some(email.to_string()),
                    chatgpt_plan_type: Some(PlanType::Known(KnownPlan::Team)),
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

    #[tokio::test]
    async fn account_switch_lock_is_created_under_accounts_dir() -> anyhow::Result<()> {
        let dir = tempdir()?;

        with_account_switch_lock(dir.path(), || Ok::<_, std::io::Error>(())).await?;

        assert!(
            auth_accounts::account_switch_lock_path(dir.path()).exists(),
            "expected account switch lock file to be created in .accounts"
        );
        Ok(())
    }

    #[test]
    fn active_account_snapshots_are_stored_under_saved_account_ids() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = fixed_now();
        let auth = chatgpt_auth("acct-team", "team@example.com");
        let saved_account_id = upsert_account(dir.path(), &auth)?.expect("saved account id");
        let auth_manager = AuthManager::from_auth_for_testing_with_home(
            CodexAuth::from_saved_account(dir.path(), auth, AuthCredentialsStoreMode::File)?,
            dir.path().to_path_buf(),
        );
        let rate_limits = RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: Some("codex".to_string()),
            primary: Some(RateLimitWindow {
                used_percent: 95.0,
                window_minutes: Some(300),
                resets_at: Some((now + chrono::Duration::minutes(30)).timestamp()),
            }),
            secondary: None,
            credits: None,
            plan_type: Some(AccountPlanType::Team),
        };

        record_active_account_rate_limit_snapshot(dir.path(), auth_manager.as_ref(), &rate_limits);
        record_active_usage_limit_hint(
            dir.path(),
            auth_manager.as_ref(),
            Some(now + chrono::Duration::minutes(45)),
        );

        let stored = account_rate_limits::load_rate_limit_snapshot(dir.path(), &saved_account_id)?
            .expect("saved-account snapshot should exist");

        assert_eq!(stored.account_id, saved_account_id);
        assert_eq!(stored.plan.as_deref(), Some("team"));
        assert_eq!(stored.snapshot, Some(rate_limits));
        assert!(stored.last_usage_limit_hit_at.is_some());
        assert!(account_rate_limits::load_rate_limit_snapshot(dir.path(), "acct-team")?.is_none());
        Ok(())
    }
}
