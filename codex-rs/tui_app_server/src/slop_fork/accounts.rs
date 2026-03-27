use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts;
use codex_core::slop_fork::auth_accounts::AccountDisplayLabels;
use codex_core::slop_fork::auth_accounts::AccountRenameSuggestion;
use codex_core::slop_fork::auth_accounts::StoredAccount;
use codex_core::slop_fork::load_slop_fork_config;
use codex_core::slop_fork::update_slop_fork_config;

use crate::status::StatusAccountDisplay;

use super::SavedAccountEntry;
use super::SavedAccountLimitsOverview;
use super::SavedAccountMenuMode;
use super::account_limits::SavedAccountLimitEntry;
use super::account_limits::auth_dot_json_is_chatgpt;
use super::account_limits::saved_account_rate_limit_refresh_is_due;
use super::build_saved_account_description;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RenameSuggestionsSummary {
    pub(crate) total_count: usize,
    pub(crate) renameable_count: usize,
    pub(crate) blocked_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AccountsPopupContext {
    pub(crate) session_account_label: Option<String>,
    pub(crate) shared_active_account_label: Option<String>,
    pub(crate) rename_summary: RenameSuggestionsSummary,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SharedActiveAccountChoice {
    pub(crate) account_id: String,
    pub(crate) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AccountsRootOverview {
    pub(crate) popup_context: AccountsPopupContext,
    pub(crate) saved_account_count: usize,
    pub(crate) due_count: usize,
    pub(crate) shared_active_choice: Option<SharedActiveAccountChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedAccountsPopupOverview {
    pub(crate) popup_context: AccountsPopupContext,
    pub(crate) mode: SavedAccountMenuMode,
    pub(crate) entries: Vec<SavedAccountEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedAccountRenameEntry {
    pub(crate) path: PathBuf,
    pub(crate) current_name: String,
    pub(crate) suggested_name: String,
    pub(crate) account_label: String,
    pub(crate) target_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenameAccountsPopupOverview {
    pub(crate) popup_context: AccountsPopupContext,
    pub(crate) entries: Vec<SavedAccountRenameEntry>,
    pub(crate) renameable_count: usize,
    pub(crate) blocked_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoginSettingsState {
    pub(crate) auto_switch_accounts_on_rate_limit: bool,
    pub(crate) follow_external_account_switches: bool,
    pub(crate) api_key_fallback_on_all_accounts_limited: bool,
    pub(crate) auto_start_five_hour_quota: bool,
    pub(crate) auto_start_weekly_quota: bool,
    pub(crate) show_account_numbers_instead_of_emails: bool,
    pub(crate) show_average_account_limits_in_status_line: bool,
}

struct AccountsPopupState {
    popup_context: AccountsPopupContext,
    session_account_id: Option<String>,
    shared_active_account_id: Option<String>,
    accounts: Vec<StoredAccount>,
    display_labels: AccountDisplayLabels,
    rename_suggestions: Vec<AccountRenameSuggestion>,
    rate_limit_snapshots: HashMap<String, account_rate_limits::StoredRateLimitSnapshot>,
}

pub(crate) fn load_accounts_popup_context(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
) -> Result<AccountsPopupContext, String> {
    Ok(load_popup_state(
        codex_home,
        auth_credentials_store_mode,
        status_account_display,
    )?
    .popup_context)
}

pub(crate) fn load_accounts_root_overview(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
) -> Result<AccountsRootOverview, String> {
    let state = load_popup_state(
        codex_home,
        auth_credentials_store_mode,
        status_account_display,
    )?;
    let now = Utc::now();
    let due_count = state
        .accounts
        .iter()
        .filter(|account| {
            saved_account_rate_limit_refresh_is_due(
                account,
                state.rate_limit_snapshots.get(&account.id),
                now,
            )
        })
        .count();
    let shared_active_choice = state
        .shared_active_account_id
        .as_deref()
        .and_then(|shared_active_account_id| {
            let should_offer =
                if state.session_account_id.as_deref() == Some(shared_active_account_id) {
                    false
                } else {
                    state.popup_context.session_account_label
                        != state.popup_context.shared_active_account_label
                };
            should_offer.then_some(shared_active_account_id)
        })
        .and_then(|shared_active_account_id| {
            state
                .accounts
                .iter()
                .find(|account| account.id == shared_active_account_id)
                .map(|account| SharedActiveAccountChoice {
                    account_id: account.id.clone(),
                    label: state.display_labels.label_for_account(account),
                })
        });

    Ok(AccountsRootOverview {
        popup_context: state.popup_context,
        saved_account_count: state.accounts.len(),
        due_count,
        shared_active_choice,
    })
}

pub(crate) fn load_saved_accounts_popup(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
    mode: SavedAccountMenuMode,
) -> Result<SavedAccountsPopupOverview, String> {
    let state = load_popup_state(
        codex_home,
        auth_credentials_store_mode,
        status_account_display,
    )?;
    let entries = state
        .accounts
        .into_iter()
        .map(|account| {
            let is_current = state.session_account_id.as_deref() == Some(account.id.as_str());
            let label = state.display_labels.label_for_account(&account);
            let description = build_saved_account_description(
                &account,
                state.rate_limit_snapshots.get(&account.id),
                is_current,
            );
            SavedAccountEntry {
                account_id: account.id,
                label,
                description,
                is_current,
            }
        })
        .collect();

    Ok(SavedAccountsPopupOverview {
        popup_context: state.popup_context,
        mode,
        entries,
    })
}

pub(crate) fn load_rename_accounts_popup(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
) -> Result<RenameAccountsPopupOverview, String> {
    let state = load_popup_state(
        codex_home,
        auth_credentials_store_mode,
        status_account_display,
    )?;
    let renameable_count = state
        .rename_suggestions
        .iter()
        .filter(|suggestion| !suggestion.target_exists)
        .count();
    let blocked_count = state.rename_suggestions.len() - renameable_count;
    let entries = state
        .rename_suggestions
        .into_iter()
        .map(|suggestion| SavedAccountRenameEntry {
            current_name: suggestion
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<invalid>")
                .to_string(),
            suggested_name: suggestion
                .suggested_path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<invalid>")
                .to_string(),
            account_label: state.display_labels.label_for_auth(&suggestion.auth),
            path: suggestion.path,
            target_exists: suggestion.target_exists,
        })
        .collect();

    Ok(RenameAccountsPopupOverview {
        popup_context: state.popup_context,
        entries,
        renameable_count,
        blocked_count,
    })
}

pub(crate) fn load_login_settings_state(codex_home: &Path) -> Result<LoginSettingsState, String> {
    let config = load_slop_fork_config(codex_home)
        .map_err(|err| format!("Failed to load fork config: {err}"))?;
    Ok(LoginSettingsState {
        auto_switch_accounts_on_rate_limit: config.auto_switch_accounts_on_rate_limit,
        follow_external_account_switches: config.follow_external_account_switches,
        api_key_fallback_on_all_accounts_limited: config.api_key_fallback_on_all_accounts_limited,
        auto_start_five_hour_quota: config.auto_start_five_hour_quota,
        auto_start_weekly_quota: config.auto_start_weekly_quota,
        show_account_numbers_instead_of_emails: config.show_account_numbers_instead_of_emails,
        show_average_account_limits_in_status_line: config
            .show_average_account_limits_in_status_line,
    })
}

pub(crate) fn save_login_settings(
    codex_home: &Path,
    settings: LoginSettingsState,
) -> Result<(), String> {
    update_slop_fork_config(codex_home, |config| {
        config.auto_switch_accounts_on_rate_limit = settings.auto_switch_accounts_on_rate_limit;
        config.follow_external_account_switches = settings.follow_external_account_switches;
        config.api_key_fallback_on_all_accounts_limited =
            settings.api_key_fallback_on_all_accounts_limited;
        config.auto_start_five_hour_quota = settings.auto_start_five_hour_quota;
        config.auto_start_weekly_quota = settings.auto_start_weekly_quota;
        config.show_account_numbers_instead_of_emails =
            settings.show_account_numbers_instead_of_emails;
        config.show_average_account_limits_in_status_line =
            settings.show_average_account_limits_in_status_line;
    })
    .map(|_| ())
    .map_err(|err| format!("Failed to save fork settings: {err}"))
}

pub(crate) fn rename_saved_account_file(codex_home: &Path, path: &Path) -> Result<bool, String> {
    auth_accounts::rename_account_file(codex_home, path)
        .map_err(|err| format!("Failed to rename account file: {err}"))
}

pub(crate) fn rename_all_saved_account_files(
    codex_home: &Path,
) -> Result<auth_accounts::RenameAllAccountsResult, String> {
    auth_accounts::rename_all_account_files(codex_home)
        .map_err(|err| format!("Failed to rename account files: {err}"))
}

pub(crate) fn load_saved_account_limits_overview_with_context(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
) -> Result<SavedAccountLimitsOverview, String> {
    let popup_context = load_accounts_popup_context(
        codex_home,
        auth_credentials_store_mode,
        status_account_display,
    )?;
    let accounts = auth_accounts::list_accounts(codex_home)
        .map_err(|err| format!("Failed to load saved accounts: {err}"))?;
    let display_labels = {
        let config = load_slop_fork_config(codex_home)
            .map_err(|err| format!("Failed to load fork config: {err}"))?;
        AccountDisplayLabels::from_config(&config, &accounts)
    };
    let current_account_id =
        auth_accounts::current_active_account_id(codex_home, auth_credentials_store_mode)
            .map_err(|err| format!("Failed to load the active account: {err}"))?;
    let snapshots = account_rate_limits::snapshot_map_for_accounts(codex_home, &accounts)
        .map_err(|err| format!("Failed to load saved-account snapshots: {err}"))?;
    let now = Utc::now();

    let refreshable_account_count = accounts
        .iter()
        .filter(|account| auth_dot_json_is_chatgpt(&account.auth))
        .count();
    let due_count = accounts
        .iter()
        .filter(|account| {
            saved_account_rate_limit_refresh_is_due(account, snapshots.get(&account.id), now)
        })
        .count();
    let entries = accounts
        .into_iter()
        .map(|account| SavedAccountLimitEntry {
            account_id: account.id.clone(),
            label: display_labels.label_for_account(&account),
            summary: super::account_limits::saved_account_rate_limit_summary(
                &account,
                snapshots.get(&account.id),
                now,
            ),
            is_current: current_account_id.as_deref() == Some(account.id.as_str()),
            is_due: saved_account_rate_limit_refresh_is_due(
                &account,
                snapshots.get(&account.id),
                now,
            ),
            is_refreshable: auth_dot_json_is_chatgpt(&account.auth),
        })
        .collect();

    Ok(SavedAccountLimitsOverview {
        popup_context,
        entries,
        due_count,
        refreshable_account_count,
    })
}

fn load_popup_state(
    codex_home: &Path,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    status_account_display: Option<&StatusAccountDisplay>,
) -> Result<AccountsPopupState, String> {
    let config = load_slop_fork_config(codex_home)
        .map_err(|err| format!("Failed to load fork config: {err}"))?;
    let accounts = auth_accounts::list_accounts(codex_home)
        .map_err(|err| format!("Failed to load saved accounts: {err}"))?;
    let display_labels = AccountDisplayLabels::from_config(&config, &accounts);
    let rename_suggestions = auth_accounts::list_account_rename_suggestions(codex_home)
        .map_err(|err| format!("Failed to load saved account rename suggestions: {err}"))?;
    let rate_limit_snapshots =
        account_rate_limits::snapshot_map_for_accounts(codex_home, &accounts)
            .map_err(|err| format!("Failed to load saved-account snapshots: {err}"))?;
    let shared_active_account_id =
        auth_accounts::current_active_account_id(codex_home, auth_credentials_store_mode)
            .map_err(|err| format!("Failed to load the active account: {err}"))?;
    let (session_account_id, session_account_label) =
        resolve_session_account(status_account_display, &accounts, &display_labels);
    let shared_active_account_label = login_account_label(
        shared_active_account_id.as_deref(),
        &accounts,
        &display_labels,
    );
    let popup_context = AccountsPopupContext {
        session_account_label,
        shared_active_account_label,
        rename_summary: summarize_rename_suggestions(&rename_suggestions),
    };

    Ok(AccountsPopupState {
        popup_context,
        session_account_id,
        shared_active_account_id,
        accounts,
        display_labels,
        rename_suggestions,
        rate_limit_snapshots,
    })
}

fn resolve_session_account(
    status_account_display: Option<&StatusAccountDisplay>,
    accounts: &[StoredAccount],
    display_labels: &AccountDisplayLabels,
) -> (Option<String>, Option<String>) {
    let Some(status_account_display) = status_account_display else {
        return (None, None);
    };

    match status_account_display {
        StatusAccountDisplay::ChatGpt { email, .. } => {
            let matched_account = email
                .as_deref()
                .and_then(|email| {
                    accounts.iter().find(|account| {
                        account
                            .auth
                            .tokens
                            .as_ref()
                            .and_then(|tokens| tokens.id_token.email.as_deref())
                            .is_some_and(|saved_email| saved_email.eq_ignore_ascii_case(email))
                    })
                })
                .or_else(|| {
                    (accounts
                        .iter()
                        .filter(|account| account.auth.is_chatgpt_mode())
                        .count()
                        == 1)
                        .then(|| {
                            accounts
                                .iter()
                                .find(|account| account.auth.is_chatgpt_mode())
                        })
                        .flatten()
                });

            if let Some(account) = matched_account {
                return (
                    Some(account.id.clone()),
                    Some(display_labels.label_for_account(account)),
                );
            }
        }
        StatusAccountDisplay::ApiKey => {
            let api_key_accounts = accounts
                .iter()
                .filter(|account| !account.auth.is_chatgpt_mode())
                .collect::<Vec<_>>();
            if let [account] = api_key_accounts.as_slice() {
                return (
                    Some(account.id.clone()),
                    Some(display_labels.label_for_account(account)),
                );
            }
        }
    }

    (
        None,
        Some(format_status_account_display(status_account_display)),
    )
}

fn format_status_account_display(status_account_display: &StatusAccountDisplay) -> String {
    match status_account_display {
        StatusAccountDisplay::ChatGpt { email, plan } => match (email, plan) {
            (Some(email), Some(plan)) => format!("{email} ({plan})"),
            (Some(email), None) => email.clone(),
            (None, Some(plan)) => plan.clone(),
            (None, None) => "ChatGPT".to_string(),
        },
        StatusAccountDisplay::ApiKey => "API key".to_string(),
    }
}

fn login_account_label(
    account_id: Option<&str>,
    accounts: &[StoredAccount],
    display_labels: &AccountDisplayLabels,
) -> Option<String> {
    account_id.and_then(|account_id| {
        accounts
            .iter()
            .find(|account| account.id == account_id)
            .map(|account| display_labels.label_for_account(account))
    })
}

fn summarize_rename_suggestions(
    rename_suggestions: &[AccountRenameSuggestion],
) -> RenameSuggestionsSummary {
    let blocked_count = rename_suggestions
        .iter()
        .filter(|suggestion| suggestion.target_exists)
        .count();
    RenameSuggestionsSummary {
        total_count: rename_suggestions.len(),
        renameable_count: rename_suggestions.len().saturating_sub(blocked_count),
        blocked_count,
    }
}
