pub mod auth_accounts;
mod auth_sync;
mod config;

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use crate::auth::AuthCredentialsStoreMode;
use crate::auth::AuthDotJson;
use crate::auth::AuthManager;

pub use auth_sync::ExternalAuthSyncOutcome;

pub(crate) fn reconcile_saved_accounts_on_startup(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) {
    if let Err(err) =
        auth_accounts::ensure_current_active_account_saved(codex_home, auth_credentials_store_mode)
    {
        tracing::warn!("failed to reconcile active auth into .accounts on startup: {err}");
    }
    if let Err(err) =
        maybe_switch_away_from_expired_active_account(codex_home, auth_credentials_store_mode)
    {
        tracing::warn!("failed to switch away from expired active account on startup: {err}");
    }
}

pub(crate) fn save_auth_with_account_sync(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    if let Err(err) =
        auth_accounts::ensure_current_active_account_saved(codex_home, auth_credentials_store_mode)
    {
        tracing::warn!("failed to preserve current active account in .accounts: {err}");
    }
    crate::auth::save_auth(
        codex_home,
        auth,
        auth.storage_mode(auth_credentials_store_mode),
    )?;
    if let Err(err) = auth_accounts::upsert_account(codex_home, auth) {
        tracing::warn!("failed to mirror saved account into .accounts: {err}");
    }
    Ok(())
}

pub fn persist_login_auth(
    codex_home: &Path,
    auth: &AuthDotJson,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    save_auth_with_account_sync(codex_home, auth, auth_credentials_store_mode)
}

pub(crate) fn activate_saved_account(
    codex_home: &Path,
    account_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<Option<auth_accounts::StoredAccount>> {
    auth_accounts::activate_account(codex_home, account_id, auth_credentials_store_mode)
}

pub(crate) fn remove_saved_account(
    codex_home: &Path,
    account_id: &str,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<bool> {
    let was_active =
        auth_accounts::current_active_account_id(codex_home, auth_credentials_store_mode)?
            .as_deref()
            == Some(account_id);
    if was_active {
        maybe_switch_away_from_expired_account(
            codex_home,
            auth_credentials_store_mode,
            account_id,
        )?;
    }
    let removed = auth_accounts::remove_account(codex_home, account_id)?;
    if removed
        && auth_accounts::current_active_account_id(codex_home, auth_credentials_store_mode)?
            .as_deref()
            == Some(account_id)
    {
        let _ = crate::auth::logout(codex_home, AuthCredentialsStoreMode::Ephemeral)?;
        if auth_credentials_store_mode != AuthCredentialsStoreMode::Ephemeral {
            let _ = crate::auth::logout(codex_home, auth_credentials_store_mode)?;
        }
    }
    Ok(removed)
}

pub(crate) fn sync_refreshed_auth(codex_home: &Path, auth: &AuthDotJson) {
    if let Err(err) = auth_accounts::upsert_account(codex_home, auth) {
        tracing::warn!("failed to mirror refreshed auth into .accounts: {err}");
    }
}

pub fn sync_external_auth_if_enabled(auth_manager: &AuthManager) -> ExternalAuthSyncOutcome {
    auth_sync::sync_external_auth_if_enabled(auth_manager)
}

fn maybe_switch_away_from_expired_active_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
) -> std::io::Result<()> {
    let Some(active_account_id) =
        auth_accounts::current_active_account_id(codex_home, auth_credentials_store_mode)?
    else {
        return Ok(());
    };
    maybe_switch_away_from_expired_account(
        codex_home,
        auth_credentials_store_mode,
        &active_account_id,
    )
}

fn maybe_switch_away_from_expired_account(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    account_id: &str,
) -> std::io::Result<()> {
    let accounts = auth_accounts::list_accounts(codex_home)?;
    let snapshots = load_saved_account_snapshots(codex_home);
    let Some(active_account) = accounts.iter().find(|account| account.id == account_id) else {
        return Ok(());
    };
    let active_snapshot = snapshot_for_account(&snapshots, account_id);
    if !auth_accounts::saved_account_subscription_ran_out_from_plan(
        active_account,
        active_snapshot.and_then(|snapshot| snapshot.plan.as_deref()),
        active_snapshot.is_some_and(|snapshot| snapshot.workspace_deactivated),
        active_snapshot.and_then(|snapshot| snapshot.observed_at),
        active_snapshot.is_some_and(|snapshot| snapshot.snapshot.is_some()),
    ) {
        return Ok(());
    }

    let Some(replacement) = accounts.iter().find(|candidate| {
        let candidate_snapshot = snapshot_for_account(&snapshots, &candidate.id);
        candidate.id != account_id
            && auth_accounts::account_has_credentials(candidate)
            && !auth_accounts::saved_account_subscription_ran_out_from_plan(
                candidate,
                candidate_snapshot.and_then(|snapshot| snapshot.plan.as_deref()),
                candidate_snapshot.is_some_and(|snapshot| snapshot.workspace_deactivated),
                candidate_snapshot.and_then(|snapshot| snapshot.observed_at),
                candidate_snapshot.is_some_and(|snapshot| snapshot.snapshot.is_some()),
            )
    }) else {
        return Ok(());
    };

    let _ =
        auth_accounts::activate_account(codex_home, &replacement.id, auth_credentials_store_mode)?;
    Ok(())
}

#[derive(Default, Deserialize)]
struct StoredRateLimitSnapshotsFile {
    #[serde(default)]
    snapshots: HashMap<String, StoredRateLimitSnapshotFile>,
}

#[derive(Default, Deserialize)]
struct StoredRateLimitSnapshotFile {
    #[serde(default)]
    plan: Option<String>,
    #[serde(default)]
    workspace_deactivated: bool,
    #[serde(default)]
    observed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    snapshot: Option<serde_json::Value>,
}

fn load_saved_account_snapshots(codex_home: &Path) -> HashMap<String, StoredRateLimitSnapshotFile> {
    let path = auth_accounts::accounts_dir(codex_home).join(".rate-limits.json");
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(err) => {
            tracing::warn!(
                "failed to read saved account rate-limit sidecar {}: {err}",
                path.display()
            );
            return HashMap::new();
        }
    };
    let stored = match serde_json::from_str::<StoredRateLimitSnapshotsFile>(&contents) {
        Ok(stored) => stored,
        Err(err) => {
            tracing::warn!(
                "failed to parse saved account rate-limit sidecar {}: {err}",
                path.display()
            );
            return HashMap::new();
        }
    };
    stored.snapshots
}

fn snapshot_for_account<'a>(
    snapshots: &'a HashMap<String, StoredRateLimitSnapshotFile>,
    account_id: &str,
) -> Option<&'a StoredRateLimitSnapshotFile> {
    snapshots.get(account_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use tempfile::tempdir;

    use crate::token_data::IdTokenInfo;
    use crate::token_data::KnownPlan;
    use crate::token_data::PlanType;
    use crate::token_data::TokenData;
    use codex_app_server_protocol::AuthMode;

    fn fake_jwt(email: &str, plan: &str, account_id: &str) -> String {
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
                "chatgpt_plan_type": plan,
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

    fn chatgpt_auth(account_id: &str, email: &str, plan: KnownPlan) -> AuthDotJson {
        AuthDotJson {
            auth_mode: Some(AuthMode::Chatgpt),
            openai_api_key: None,
            tokens: Some(TokenData {
                id_token: IdTokenInfo {
                    email: Some(email.to_string()),
                    chatgpt_plan_type: Some(PlanType::Known(plan)),
                    chatgpt_user_id: None,
                    chatgpt_account_id: Some(account_id.to_string()),
                    raw_jwt: fake_jwt(email, plan.raw_value(), account_id),
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: None,
        }
    }

    fn write_snapshot_plans(codex_home: &Path, entries: &[(&str, &str)]) -> std::io::Result<()> {
        let path = auth_accounts::accounts_dir(codex_home).join(".rate-limits.json");
        std::fs::create_dir_all(path.parent().expect("snapshot parent"))?;
        let snapshots = entries
            .iter()
            .map(|(account_id, plan)| {
                (
                    (*account_id).to_string(),
                    serde_json::json!({
                        "plan": plan,
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        std::fs::write(
            path,
            serde_json::to_string_pretty(&serde_json::json!({
                "version": 2,
                "snapshots": snapshots,
            }))?,
        )
    }

    #[test]
    fn reconcile_saved_accounts_on_startup_switches_away_from_expired_active_account()
    -> anyhow::Result<()> {
        let dir = tempdir()?;
        let expired = chatgpt_auth("acct-expired", "expired@example.com", KnownPlan::Team);
        let healthy = chatgpt_auth("acct-healthy", "healthy@example.com", KnownPlan::Team);
        let expired_id = auth_accounts::upsert_account(dir.path(), &expired)?.expect("expired id");
        let healthy_id = auth_accounts::upsert_account(dir.path(), &healthy)?.expect("healthy id");
        crate::auth::save_auth(dir.path(), &expired, AuthCredentialsStoreMode::File)?;
        write_snapshot_plans(dir.path(), &[(&expired_id, "free"), (&healthy_id, "team")])?;

        reconcile_saved_accounts_on_startup(dir.path(), AuthCredentialsStoreMode::File);

        assert_eq!(
            auth_accounts::current_active_account_id(dir.path(), AuthCredentialsStoreMode::File)?,
            Some(healthy_id)
        );
        Ok(())
    }

    #[test]
    fn remove_saved_account_switches_away_from_expired_active_account_before_delete()
    -> anyhow::Result<()> {
        let dir = tempdir()?;
        let expired = chatgpt_auth("acct-expired", "expired@example.com", KnownPlan::Team);
        let healthy = chatgpt_auth("acct-healthy", "healthy@example.com", KnownPlan::Team);
        let expired_id = auth_accounts::upsert_account(dir.path(), &expired)?.expect("expired id");
        let healthy_id = auth_accounts::upsert_account(dir.path(), &healthy)?.expect("healthy id");
        crate::auth::save_auth(dir.path(), &expired, AuthCredentialsStoreMode::File)?;
        write_snapshot_plans(dir.path(), &[(&expired_id, "free"), (&healthy_id, "team")])?;

        assert_eq!(
            remove_saved_account(dir.path(), &expired_id, AuthCredentialsStoreMode::File,)?,
            true
        );
        assert_eq!(
            auth_accounts::current_active_account_id(dir.path(), AuthCredentialsStoreMode::File)?,
            Some(healthy_id)
        );
        assert_eq!(auth_accounts::find_account(dir.path(), &expired_id)?, None);
        Ok(())
    }
}
