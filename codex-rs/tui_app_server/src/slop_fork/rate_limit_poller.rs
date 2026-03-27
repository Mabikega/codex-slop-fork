use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use codex_backend_client::Client as BackendClient;
use codex_backend_client::RawRateLimitSnapshotInput as BackendRawRateLimitSnapshotInput;
use codex_backend_client::RawRateLimitWindowSnapshot as BackendRawRateLimitWindowSnapshot;
use codex_backend_client::RequestError;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::auth::AuthDotJson;
use codex_core::auth::CodexAuth;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts;
use codex_core::slop_fork::auth_for_saved_account_file;
use codex_core::slop_fork::refresh_saved_account_auth_from_authority;
use codex_protocol::protocol::RateLimitSnapshot;
use tokio::task::JoinHandle;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;

use super::SlopForkEvent;
use super::account_limits::auth_dot_json_is_chatgpt;

const RATE_LIMIT_POLL_INTERVAL: Duration = Duration::from_secs(60);

pub(crate) fn has_saved_chatgpt_accounts(codex_home: &Path) -> bool {
    auth_accounts::list_accounts(codex_home)
        .map(|accounts| {
            accounts
                .iter()
                .any(|account| auth_dot_json_is_chatgpt(&account.auth))
        })
        .unwrap_or(false)
}

pub(crate) fn should_spawn_rate_limit_poller(
    requires_openai_auth: bool,
    has_chatgpt_account: bool,
    codex_home: &Path,
) -> bool {
    requires_openai_auth && (has_chatgpt_account || has_saved_chatgpt_accounts(codex_home))
}

pub(crate) fn spawn_rate_limit_poller(
    base_url: String,
    app_event_tx: AppEventSender,
    codex_home: PathBuf,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
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
                        source: super::SavedAccountRateLimitsRefreshCompletionSource::Background,
                    },
                ));
            }
            tokio::time::sleep(RATE_LIMIT_POLL_INTERVAL).await;
        }
    })
}

pub(crate) async fn refresh_saved_account_rate_limits_once(
    codex_home: PathBuf,
    base_url: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    include_active: bool,
    requested_account_ids: Option<Vec<String>>,
) -> Vec<String> {
    let now = chrono::Utc::now();
    let active_account_id =
        auth_accounts::current_active_account_id(&codex_home, auth_credentials_store_mode)
            .ok()
            .flatten();
    let accounts = auth_accounts::list_accounts(&codex_home).unwrap_or_default();
    let mut snapshots =
        account_rate_limits::snapshot_map_for_accounts(&codex_home, &accounts).unwrap_or_default();
    let requested_account_ids =
        requested_account_ids.map(|ids| ids.into_iter().collect::<HashSet<_>>());

    let mut due_accounts = Vec::new();
    for account in accounts {
        let is_requested = requested_account_ids
            .as_ref()
            .is_some_and(|ids| ids.contains(&account.id));
        if requested_account_ids
            .as_ref()
            .is_some_and(|ids| !ids.contains(&account.id))
            || !auth_dot_json_is_chatgpt(&account.auth)
            || (!include_active
                && !is_requested
                && active_account_id.as_deref() == Some(account.id.as_str()))
        {
            continue;
        }
        let stored_snapshot = snapshots.remove(&account.id);
        let reset_at = stored_snapshot
            .as_ref()
            .and_then(account_rate_limits::snapshot_reset_at);
        let plan = stored_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.plan.clone())
            .or_else(|| {
                account
                    .auth
                    .tokens
                    .as_ref()
                    .and_then(|tokens| tokens.id_token.get_chatgpt_plan_type())
            });
        let should_refresh = if is_requested {
            true
        } else {
            account_rate_limits::mark_rate_limit_refresh_attempt_if_due(
                &codex_home,
                &account.id,
                plan.as_deref(),
                reset_at,
                now,
                account_rate_limits::rate_limit_refresh_stale_interval(),
            )
            .unwrap_or(false)
        };
        if should_refresh {
            due_accounts.push((account, plan));
        }
    }

    let mut updated_account_ids = Vec::new();
    for (account, plan) in due_accounts {
        let mut session = match SavedAccountBackendSession::new(
            codex_home.clone(),
            base_url.clone(),
            account.id.clone(),
            account.auth.clone(),
            auth_credentials_store_mode,
        ) {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!(
                    "failed to build background auth for saved account {}: {err}",
                    account.id
                );
                continue;
            }
        };
        let snapshots = match session.get_detailed_rate_limits_many().await {
            Ok(snapshots) => snapshots,
            Err(err) => {
                tracing::warn!(
                    "failed to refresh rate limits for saved account {}: {err}",
                    account.id
                );
                continue;
            }
        };
        let Some((snapshot, raw)) = codex_rate_limit_snapshot(&snapshots) else {
            continue;
        };
        if let Err(err) = account_rate_limits::record_rate_limit_snapshot_with_raw(
            &codex_home,
            &account.id,
            plan.as_deref(),
            snapshot,
            Some(raw),
            chrono::Utc::now(),
        ) {
            tracing::warn!(
                "failed to persist rate-limit snapshot for saved account {}: {err}",
                account.id
            );
            continue;
        }
        updated_account_ids.push(account.id);
    }

    updated_account_ids
}

fn codex_rate_limit_snapshot(
    snapshots: &[(
        RateLimitSnapshot,
        account_rate_limits::RawRateLimitSnapshotInput,
    )],
) -> Option<&(
    RateLimitSnapshot,
    account_rate_limits::RawRateLimitSnapshotInput,
)> {
    snapshots
        .iter()
        .find(|(snapshot, _raw)| snapshot.limit_id.as_deref() == Some("codex"))
        .or_else(|| snapshots.first())
}

fn backend_raw_rate_limit_snapshot_input(
    raw: BackendRawRateLimitSnapshotInput,
) -> account_rate_limits::RawRateLimitSnapshotInput {
    account_rate_limits::RawRateLimitSnapshotInput {
        primary: raw.primary.map(backend_raw_rate_limit_window),
        secondary: raw.secondary.map(backend_raw_rate_limit_window),
    }
}

fn backend_raw_rate_limit_window(
    raw: BackendRawRateLimitWindowSnapshot,
) -> account_rate_limits::RawRateLimitWindowSnapshot {
    account_rate_limits::RawRateLimitWindowSnapshot {
        used_percent: raw.used_percent,
        limit_window_seconds: raw.limit_window_seconds,
        reset_after_seconds: raw.reset_after_seconds,
        reset_at: raw.reset_at,
    }
}

struct SavedAccountBackendSession {
    base_url: String,
    codex_home: PathBuf,
    account_id: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_dot_json: AuthDotJson,
    auth: CodexAuth,
}

impl SavedAccountBackendSession {
    fn new(
        codex_home: PathBuf,
        base_url: String,
        account_id: String,
        auth_dot_json: AuthDotJson,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> anyhow::Result<Self> {
        let auth = auth_for_saved_account_file(
            &codex_home,
            &account_id,
            auth_dot_json.clone(),
            auth_credentials_store_mode,
        )?;
        Ok(Self {
            base_url,
            codex_home,
            account_id,
            auth_credentials_store_mode,
            auth_dot_json,
            auth,
        })
    }

    async fn get_detailed_rate_limits_many(
        &mut self,
    ) -> anyhow::Result<
        Vec<(
            RateLimitSnapshot,
            account_rate_limits::RawRateLimitSnapshotInput,
        )>,
    > {
        let snapshots = self
            .run_request_with_auth_retry(|client| {
                Box::pin(client.get_detailed_rate_limits_many_detailed())
            })
            .await?;
        Ok(snapshots
            .into_iter()
            .map(|(snapshot, raw)| (snapshot, backend_raw_rate_limit_snapshot_input(raw)))
            .collect())
    }

    fn client(&self) -> anyhow::Result<BackendClient> {
        BackendClient::from_auth(self.base_url.clone(), &self.auth)
    }

    async fn refresh_auth(&mut self) -> anyhow::Result<()> {
        refresh_saved_account_auth_from_authority(&self.auth)
            .await
            .with_context(|| {
                format!(
                    "failed to refresh auth for saved account {}",
                    self.account_id
                )
            })?;
        self.auth = auth_for_saved_account_file(
            &self.codex_home,
            &self.account_id,
            self.auth_dot_json.clone(),
            self.auth_credentials_store_mode,
        )?;
        Ok(())
    }

    async fn run_request_with_auth_retry<T>(
        &mut self,
        request: impl for<'a> Fn(
            &'a BackendClient,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T, RequestError>> + Send + 'a>,
        >,
    ) -> anyhow::Result<T> {
        match request(&self.client()?).await {
            Ok(value) => Ok(value),
            Err(err) if err.is_unauthorized() => {
                self.refresh_auth().await?;
                request(&self.client()?).await.map_err(anyhow::Error::from)
            }
            Err(err) => Err(anyhow::Error::from(err)),
        }
    }
}
