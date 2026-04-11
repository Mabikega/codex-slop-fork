use std::path::PathBuf;

use codex_app_server_protocol::Automation;
use codex_app_server_protocol::AutomationDefinition;
use codex_app_server_protocol::AutomationScope as AppServerAutomationScope;
use codex_app_server_protocol::AutoresearchControlAction;
use codex_app_server_protocol::AutoresearchMode as AppServerAutoresearchMode;
use codex_app_server_protocol::AutoresearchRun;
use codex_app_server_protocol::PilotControlAction;
use codex_app_server_protocol::PilotRun;
use codex_core::slop_fork::automation::AutomationPolicyDecision;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RemoteStateLoadSource {
    Bootstrap,
    ActionResponse,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoginPopupKind {
    Root,
    UseAccount,
    RemoveAccount,
    RemoveExpiredAccounts,
    ConfirmRemoveSavedAccounts,
    RenameAccountFiles,
    AccountLimits,
    Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedAccountDeletionRequest {
    pub(crate) account_ids: Vec<String>,
    pub(crate) return_kind: LoginPopupKind,
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
    ConfirmSavedAccountDeletion {
        request: SavedAccountDeletionRequest,
    },
    RenameAllSavedAccountFiles,
    RenameSavedAccountFile {
        path: PathBuf,
    },
    RemoveSavedAccount {
        account_id: String,
    },
    RemoveSavedAccounts {
        account_ids: Vec<String>,
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
    FetchRemoteAutomationState {
        thread_id: String,
    },
    RemoteAutomationStateLoaded {
        thread_id: String,
        request_nonce: u64,
        result: Result<Vec<Automation>, String>,
    },
    FetchRemotePilotState {
        thread_id: String,
    },
    RemotePilotStateLoaded {
        thread_id: String,
        request_nonce: u64,
        source: RemoteStateLoadSource,
        report_error: bool,
        result: Result<Option<PilotRun>, String>,
    },
    FetchRemoteAutoresearchState {
        thread_id: String,
    },
    RemoteAutoresearchStateLoaded {
        thread_id: String,
        request_nonce: u64,
        source: RemoteStateLoadSource,
        report_error: bool,
        result: Result<Option<AutoresearchRun>, String>,
    },
    StartRemotePilot {
        thread_id: String,
        goal: String,
        deadline_at: Option<i64>,
    },
    ControlRemotePilot {
        thread_id: String,
        action: PilotControlAction,
    },
    StartRemoteAutoresearch {
        thread_id: String,
        goal: String,
        max_runs: Option<u32>,
        mode: AppServerAutoresearchMode,
    },
    ControlRemoteAutoresearch {
        thread_id: String,
        action: AutoresearchControlAction,
        focus: Option<String>,
    },
    UpsertRemoteAutomation {
        thread_id: String,
        scope: AppServerAutomationScope,
        automation: AutomationDefinition,
    },
    SetRemoteAutomationEnabled {
        thread_id: String,
        runtime_id: String,
        enabled: bool,
    },
    DeleteRemoteAutomation {
        thread_id: String,
        runtime_id: String,
    },
    RemoteActionFailed {
        message: String,
    },
    SaveLoginSettings {
        settings: LoginSettingsState,
    },
}
