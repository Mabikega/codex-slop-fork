use crate::AuthManager;
use crate::auth::CodexAuth;

use super::auth_accounts;
use super::config::maybe_load_slop_fork_config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalAuthSyncOutcome {
    Disabled,
    NoChange,
    Updated,
    SwitchedAccounts,
}

pub fn sync_external_auth_if_enabled(auth_manager: &AuthManager) -> ExternalAuthSyncOutcome {
    let follow_external_account_switches =
        match maybe_load_slop_fork_config(auth_manager.codex_home_path()) {
            Ok(config) => config.follow_external_account_switches,
            Err(err) => {
                tracing::warn!("failed to load fork config for external auth sync: {err}");
                false
            }
        };
    if !follow_external_account_switches {
        return ExternalAuthSyncOutcome::Disabled;
    }

    let cached_auth = auth_manager.auth_cached();
    let stored_auth = auth_manager.load_auth_from_storage_for_fork();
    if AuthManager::auths_equal_for_refresh(cached_auth.as_ref(), stored_auth.as_ref()) {
        return ExternalAuthSyncOutcome::NoChange;
    }

    auth_manager.set_cached_auth_from_fork(stored_auth.clone());

    if auth_identity(cached_auth.as_ref()) != auth_identity(stored_auth.as_ref()) {
        if auth_manager.suppress_expected_external_auth_switch_for_fork(stored_auth.as_ref()) {
            return ExternalAuthSyncOutcome::SwitchedAccounts;
        }
        let display_labels =
            auth_accounts::load_account_display_labels(auth_manager.codex_home_path());
        let label = stored_auth
            .as_ref()
            .map(|auth| display_labels.label_for_codex_auth(auth))
            .unwrap_or_else(|| "none".to_string());
        auth_manager.record_external_auth_switch_notice_for_fork(label);
        ExternalAuthSyncOutcome::SwitchedAccounts
    } else {
        ExternalAuthSyncOutcome::Updated
    }
}

fn auth_identity(auth: Option<&CodexAuth>) -> Option<String> {
    let auth = auth?;
    auth_accounts::stored_account_id_for_auth(auth)
        .or_else(|| auth.get_account_id())
        .map(|id| format!("account:{id}"))
}
