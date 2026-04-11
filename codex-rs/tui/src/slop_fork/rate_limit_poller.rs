use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_core::AuthManager;
use codex_core::CodexAuth;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::slop_fork::load_slop_fork_config;
use tokio::task::JoinHandle;

use crate::app_event::AppEvent;
use crate::app_event::RateLimitRefreshOrigin;
use crate::app_event_sender::AppEventSender;
use crate::history_cell;

use super::SlopForkEvent;
use super::ui::TouchQuotaMode;
use super::ui::fetch_rate_limits;
use super::ui::has_saved_chatgpt_accounts;
use super::ui::maybe_touch_active_account_cached_quotas;
use super::ui::refresh_saved_account_rate_limits_once;
use super::ui::touch_cached_quotas_for_saved_accounts;

const RATE_LIMIT_POLL_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn should_spawn_rate_limit_poller(
    requires_openai_auth: bool,
    auth_manager: &AuthManager,
    codex_home: &Path,
) -> bool {
    if !requires_openai_auth {
        return false;
    }

    auth_manager
        .auth_cached()
        .as_ref()
        .is_some_and(CodexAuth::is_chatgpt_auth)
        || has_saved_chatgpt_accounts(codex_home)
}

pub(crate) fn spawn_rate_limit_poller(
    base_url: String,
    app_event_tx: AppEventSender,
    auth_manager: Arc<AuthManager>,
    codex_home: PathBuf,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(RATE_LIMIT_POLL_INTERVAL);
        let mut first_iteration = true;

        loop {
            let fork_config = load_slop_fork_config(&codex_home).unwrap_or_default();
            let touch_mode = TouchQuotaMode::Automatic {
                start_five_hour: fork_config.auto_start_five_hour_quota,
                start_weekly: fork_config.auto_start_weekly_quota,
            };

            if first_iteration {
                let result = touch_cached_quotas_for_saved_accounts(
                    codex_home.clone(),
                    base_url.clone(),
                    auth_credentials_store_mode,
                    touch_mode,
                )
                .await;
                if result.checked_accounts > 0 {
                    app_event_tx.send(AppEvent::SlopFork(
                        SlopForkEvent::SavedAccountQuotaTouchCompleted {
                            updated_account_ids: result.updated_account_ids,
                            message: result.message,
                        },
                    ));
                }
            }

            if let Some(auth) = auth_manager.auth().await
                && auth.is_chatgpt_auth()
            {
                if first_iteration {
                    let result = maybe_touch_active_account_cached_quotas(
                        codex_home.clone(),
                        base_url.clone(),
                        auth.clone(),
                        touch_mode,
                    )
                    .await;
                    if !result.updated_account_ids.is_empty() {
                        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                            history_cell::new_info_event(result.message, /*hint*/ None),
                        )));
                    }
                }

                let mut fetched_snapshots = Vec::new();
                for (snapshot, raw) in fetch_rate_limits(base_url.clone(), auth.clone()).await {
                    codex_core::slop_fork::record_rate_limit_snapshot_for_auth_with_raw(
                        &codex_home,
                        &auth,
                        &snapshot,
                        Some(&raw),
                    );
                    fetched_snapshots.push(snapshot);
                }
                if !fetched_snapshots.is_empty() {
                    app_event_tx.send(AppEvent::RateLimitsLoaded {
                        origin: RateLimitRefreshOrigin::StartupPrefetch,
                        result: Ok(fetched_snapshots),
                    });
                }

                let result = maybe_touch_active_account_cached_quotas(
                    codex_home.clone(),
                    base_url.clone(),
                    auth.clone(),
                    touch_mode,
                )
                .await;
                if !result.updated_account_ids.is_empty() {
                    app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_info_event(result.message, /*hint*/ None),
                    )));
                }
            }

            let updated_account_ids = refresh_saved_account_rate_limits_once(
                codex_home.clone(),
                base_url.clone(),
                auth_credentials_store_mode,
                /*include_active*/ false,
                /*requested_account_ids*/ None,
            )
            .await;
            if !updated_account_ids.is_empty() {
                app_event_tx.send(AppEvent::SlopFork(
                    SlopForkEvent::SavedAccountRateLimitsRefreshCompleted {
                        updated_account_ids,
                    },
                ));
                let result = touch_cached_quotas_for_saved_accounts(
                    codex_home.clone(),
                    base_url.clone(),
                    auth_credentials_store_mode,
                    touch_mode,
                )
                .await;
                if !result.updated_account_ids.is_empty() {
                    app_event_tx.send(AppEvent::SlopFork(
                        SlopForkEvent::SavedAccountQuotaTouchCompleted {
                            updated_account_ids: result.updated_account_ids,
                            message: result.message,
                        },
                    ));
                }
            }

            first_iteration = false;
            interval.tick().await;
        }
    })
}
