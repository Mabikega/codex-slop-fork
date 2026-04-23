use crate::auth::AuthManager;
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
        let display_labels =
            auth_accounts::load_account_display_labels(auth_manager.codex_home_path());
        let label = stored_auth
            .as_ref()
            .map(|auth| display_labels.label_for_codex_auth(auth))
            .unwrap_or_else(|| "none".to_string());
        if auth_manager.suppress_expected_external_auth_transition_for_fork(
            cached_auth.as_ref(),
            stored_auth.as_ref(),
        ) {
            auth_manager.record_local_auth_switch_notice_for_fork(label);
            return ExternalAuthSyncOutcome::SwitchedAccounts;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use pretty_assertions::assert_eq;
    use serde::Serialize;
    use tempfile::tempdir;

    use crate::AuthCredentialsStoreMode;
    use crate::AuthManager;
    use crate::auth::AuthDotJson;
    use crate::auth::ExternalAuthSwitchNoticeForFork;
    use crate::token_data::IdTokenInfo;
    use crate::token_data::TokenData;
    use codex_app_server_protocol::AuthMode;
    use codex_protocol::auth::KnownPlan;
    use codex_protocol::auth::PlanType;

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
                    chatgpt_account_is_fedramp: false,
                    raw_jwt: fake_jwt(email, plan.raw_value(), account_id),
                },
                access_token: "access".to_string(),
                refresh_token: "refresh".to_string(),
                account_id: Some(account_id.to_string()),
            }),
            last_refresh: None,
            agent_identity: None,
        }
    }

    #[test]
    fn suppresses_notice_for_self_initiated_account_switch() -> anyhow::Result<()> {
        let dir = tempdir()?;
        std::fs::write(
            dir.path().join("config-slop-fork.toml"),
            "follow_external_account_switches = true\n",
        )?;

        let initial_auth = chatgpt_auth("acct-initial", "initial@example.com", KnownPlan::Plus);
        let next_auth = chatgpt_auth("acct-next", "next@example.com", KnownPlan::Pro);
        let next_account_id =
            auth_accounts::stored_account_id(&next_auth).expect("next account id");
        crate::auth::save_auth(dir.path(), &initial_auth, AuthCredentialsStoreMode::File)?;

        let initial_codex_auth = crate::auth::auth_for_saved_account(
            dir.path(),
            initial_auth,
            AuthCredentialsStoreMode::File,
        )?;
        let auth_manager = AuthManager::from_auth_for_testing_with_home(
            initial_codex_auth,
            dir.path().to_path_buf(),
        );

        crate::slop_fork::save_auth_with_account_sync(
            dir.path(),
            &next_auth,
            AuthCredentialsStoreMode::File,
        )?;

        let outcome = sync_external_auth_if_enabled(auth_manager.as_ref());

        assert_eq!(outcome, ExternalAuthSyncOutcome::SwitchedAccounts);
        assert_eq!(
            auth_manager.take_external_auth_switch_notice_for_fork(),
            Some(ExternalAuthSwitchNoticeForFork::Local {
                label: "next@example.com (Pro)".to_string(),
            })
        );
        assert_eq!(
            auth_identity(auth_manager.auth_cached().as_ref()),
            Some(format!("account:{next_account_id}"))
        );
        Ok(())
    }

    #[test]
    fn suppresses_notice_when_session_cache_was_already_stale() -> anyhow::Result<()> {
        let dir = tempdir()?;
        std::fs::write(
            dir.path().join("config-slop-fork.toml"),
            "follow_external_account_switches = true\n",
        )?;

        let shared_auth = chatgpt_auth("acct-shared", "shared@example.com", KnownPlan::Plus);
        let session_auth = chatgpt_auth("acct-session", "session@example.com", KnownPlan::Plus);
        let next_auth = chatgpt_auth("acct-next", "next@example.com", KnownPlan::Pro);
        let next_account_id =
            auth_accounts::stored_account_id(&next_auth).expect("next account id");
        crate::auth::save_auth(dir.path(), &shared_auth, AuthCredentialsStoreMode::File)?;

        let stale_session_auth = crate::auth::auth_for_saved_account(
            dir.path(),
            session_auth,
            AuthCredentialsStoreMode::File,
        )?;
        let auth_manager = AuthManager::from_auth_for_testing_with_home(
            stale_session_auth,
            dir.path().to_path_buf(),
        );

        crate::slop_fork::save_auth_with_account_sync(
            dir.path(),
            &next_auth,
            AuthCredentialsStoreMode::File,
        )?;

        let outcome = sync_external_auth_if_enabled(auth_manager.as_ref());

        assert_eq!(outcome, ExternalAuthSyncOutcome::SwitchedAccounts);
        assert_eq!(
            auth_manager.take_external_auth_switch_notice_for_fork(),
            Some(ExternalAuthSwitchNoticeForFork::Local {
                label: "next@example.com (Pro)".to_string(),
            })
        );
        assert_eq!(
            auth_identity(auth_manager.auth_cached().as_ref()),
            Some(format!("account:{next_account_id}"))
        );
        Ok(())
    }

    #[test]
    fn clears_queued_notice_when_current_session_switches_accounts() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let initial_auth = chatgpt_auth("acct-initial", "initial@example.com", KnownPlan::Plus);
        let next_auth = chatgpt_auth("acct-next", "next@example.com", KnownPlan::Pro);
        let next_account_id =
            auth_accounts::upsert_account(dir.path(), &next_auth)?.expect("next account id");
        crate::auth::save_auth(dir.path(), &initial_auth, AuthCredentialsStoreMode::File)?;

        let initial_codex_auth = crate::auth::auth_for_saved_account(
            dir.path(),
            initial_auth,
            AuthCredentialsStoreMode::File,
        )?;
        let auth_manager = AuthManager::from_auth_for_testing_with_home(
            initial_codex_auth,
            dir.path().to_path_buf(),
        );

        auth_manager.record_external_auth_switch_notice_for_fork("stale queued notice".to_string());
        assert_eq!(auth_manager.activate_saved_account(&next_account_id)?, true);
        assert_eq!(
            auth_manager.take_external_auth_switch_notice_for_fork(),
            None
        );
        Ok(())
    }

    #[test]
    fn suppresses_notice_when_another_auth_manager_is_stale_after_local_switch()
    -> anyhow::Result<()> {
        let dir = tempdir()?;
        std::fs::write(
            dir.path().join("config-slop-fork.toml"),
            "follow_external_account_switches = true\n",
        )?;

        let initial_auth = chatgpt_auth("acct-initial", "initial@example.com", KnownPlan::Plus);
        let next_auth = chatgpt_auth("acct-next", "next@example.com", KnownPlan::Pro);
        let next_account_id =
            auth_accounts::stored_account_id(&next_auth).expect("next account id");
        crate::auth::save_auth(dir.path(), &initial_auth, AuthCredentialsStoreMode::File)?;

        let initial_cached_auth = crate::auth::auth_for_saved_account(
            dir.path(),
            initial_auth,
            AuthCredentialsStoreMode::File,
        )?;
        let switching_auth_manager = AuthManager::from_auth_for_testing_with_home(
            initial_cached_auth.clone(),
            dir.path().to_path_buf(),
        );
        let observing_auth_manager = AuthManager::from_auth_for_testing_with_home(
            initial_cached_auth,
            dir.path().to_path_buf(),
        );

        crate::slop_fork::save_auth_with_account_sync(
            dir.path(),
            &next_auth,
            AuthCredentialsStoreMode::File,
        )?;
        let switched_auth = crate::auth::auth_for_saved_account(
            dir.path(),
            next_auth,
            AuthCredentialsStoreMode::File,
        )?;
        switching_auth_manager.set_cached_auth_for_switch(switched_auth);

        let outcome = sync_external_auth_if_enabled(observing_auth_manager.as_ref());

        assert_eq!(outcome, ExternalAuthSyncOutcome::SwitchedAccounts);
        assert_eq!(
            observing_auth_manager.take_external_auth_switch_notice_for_fork(),
            Some(ExternalAuthSwitchNoticeForFork::Local {
                label: "next@example.com (Pro)".to_string(),
            })
        );
        assert_eq!(
            auth_identity(observing_auth_manager.auth_cached().as_ref()),
            Some(format!("account:{next_account_id}"))
        );
        Ok(())
    }
}
