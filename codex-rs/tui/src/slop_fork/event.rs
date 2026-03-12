use std::path::PathBuf;

use codex_core::slop_fork::automation::AutomationPolicyDecision;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LoginSettingsState {
    pub(crate) auto_switch_accounts_on_rate_limit: bool,
    pub(crate) follow_external_account_switches: bool,
    pub(crate) api_key_fallback_on_all_accounts_limited: bool,
    pub(crate) auto_start_five_hour_quota: bool,
    pub(crate) auto_start_weekly_quota: bool,
    pub(crate) show_average_account_limits_in_status_line: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginPopupKind {
    Root,
    UseAccount,
    RemoveAccount,
    RenameAccountFiles,
    AccountLimits,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginFlowKind {
    Browser,
    DeviceCode,
}

#[derive(Debug, Clone)]
pub(crate) enum SlopForkEvent {
    OpenLoginPopup {
        kind: LoginPopupKind,
    },
    OpenLoginApiKeyPrompt,
    StartLoginFlow {
        kind: LoginFlowKind,
    },
    CancelPendingLogin,
    SaveLoginApiKey {
        api_key: String,
    },
    RefreshSavedAccountRateLimits,
    RefreshAllSavedAccountRateLimits,
    RefreshAllSavedAccountRateLimitsAndStartQuotas,
    RefreshSavedAccountRateLimit {
        account_id: String,
    },
    PendingDeviceCodeLoginReady {
        verification_url: String,
        user_code: String,
    },
    SavedAccountRateLimitsRefreshCompleted {
        updated_account_ids: Vec<String>,
    },
    SavedAccountQuotaTouchCompleted {
        updated_account_ids: Vec<String>,
        message: String,
    },
    ActivateSavedAccount {
        account_id: String,
    },
    RenameAllSavedAccountFiles,
    RenameSavedAccountFile {
        path: PathBuf,
    },
    RemoveSavedAccount {
        account_id: String,
    },
    AutomationPolicyEvaluated {
        thread_id: String,
        runtime_id: String,
        decision: AutomationPolicyDecision,
    },
    AutomationPolicyFailed {
        thread_id: String,
        runtime_id: String,
        error: String,
    },
    SaveLoginSettings {
        settings: LoginSettingsState,
    },
}
