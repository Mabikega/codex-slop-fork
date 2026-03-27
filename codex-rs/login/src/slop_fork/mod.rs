pub mod auth_accounts;
mod auth_sync;
mod config;

use std::path::Path;

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
    let removed = auth_accounts::remove_account(codex_home, account_id)?;
    if removed && was_active {
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
