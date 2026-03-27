use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_backend_client::Client as BackendClient;
use codex_core::AuthManager;
use codex_core::CodexAuth;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::auth::AuthDotJson;
use codex_core::auth::CLIENT_ID;
use codex_core::auth::login_with_api_key;
use codex_core::slop_fork::SlopForkConfig;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts;
use codex_core::slop_fork::automation::AutomationEvaluationTrigger;
use codex_core::slop_fork::automation::AutomationPolicyDecision;
use codex_core::slop_fork::automation::AutomationPolicyExecutionContext;
use codex_core::slop_fork::automation::AutomationPreparedAction;
use codex_core::slop_fork::automation::AutomationPreparedPolicy;
use codex_core::slop_fork::automation::AutomationRegistry;
use codex_core::slop_fork::automation::AutomationScope;
use codex_core::slop_fork::automation::AutomationSpec;
use codex_core::slop_fork::automation::run_policy_command;
use codex_core::slop_fork::autoresearch::AutoresearchCycleKind;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::load_slop_fork_config;
use codex_core::slop_fork::pilot::PilotCycleKind;
use codex_core::slop_fork::pilot::PilotRuntime;
use codex_core::slop_fork::update_slop_fork_config;
use codex_login::ServerOptions;
use codex_login::ShutdownHandle;
use codex_login::complete_device_code_login;
use codex_login::request_device_code;
use codex_login::run_login_server;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::SandboxPolicy;
use codex_protocol::protocol::SkillMetadata as ProtocolSkillMetadata;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::WidgetRef;
use ratatui::widgets::Wrap;
use tokio::task::JoinHandle;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::exec_cell::spinner;
use crate::history_cell;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::shimmer::shimmer_spans;
use crate::status::rate_limit_snapshot_display_for_limit;
use crate::status_indicator_widget::StatusIndicatorWidget;
use crate::status_indicator_widget::fmt_elapsed_compact;
use crate::tui::FrameRequester;
use crate::wrapping::RtOptions;
use crate::wrapping::word_wrap_lines;

use super::auto_command::AUTO_COMMAND_NAME;
use super::auto_command::AutoCommand;
use super::auto_command::auto_command_skill_conflict_warning;
use super::auto_command::auto_usage;
use super::auto_command::parse_auto_command;
use super::event::LoginFlowKind;
use super::event::LoginPopupKind;
use super::event::LoginSettingsState;
use super::event::SlopForkEvent;
use super::login_settings_view::LoginSettingsView;
use super::schedule_parser::compact_duration_label;

#[path = "ui_automation.rs"]
mod ui_automation;
#[path = "ui_autoresearch.rs"]
mod ui_autoresearch;
#[path = "ui_login.rs"]
mod ui_login;
#[path = "ui_pilot.rs"]
mod ui_pilot;
#[path = "ui_rate_limits.rs"]
mod ui_rate_limits;

pub(crate) use ui_login::PendingChatgptLogin;
#[cfg(test)]
pub(crate) use ui_login::PendingDeviceCodeState;
use ui_rate_limits::SavedAccountRateLimitsRefreshState;
pub(crate) use ui_rate_limits::TouchQuotaMode;
pub(crate) use ui_rate_limits::fetch_rate_limits;
pub(crate) use ui_rate_limits::has_saved_chatgpt_accounts;
pub(crate) use ui_rate_limits::maybe_touch_active_account_cached_quotas;
pub(crate) use ui_rate_limits::refresh_saved_account_rate_limits_once;
#[cfg(test)]
pub(crate) use ui_rate_limits::saved_account_rate_limit_refresh_is_due;
pub(crate) use ui_rate_limits::touch_cached_quotas_for_saved_accounts;

pub(crate) const LOGIN_POPUP_VIEW_ID: &str = "login-popup";
const VIEW_ACCOUNT_LIMITS_ITEM_NAME: &str = "View account limits";

type LoginPopupState = (
    SlopForkConfig,
    Option<String>,
    Option<String>,
    Vec<auth_accounts::StoredAccount>,
    auth_accounts::AccountDisplayLabels,
    Vec<auth_accounts::AccountRenameSuggestion>,
    HashMap<String, account_rate_limits::StoredRateLimitSnapshot>,
);

pub(crate) struct SlopForkUiContext {
    pub(crate) codex_home: PathBuf,
    pub(crate) cwd: PathBuf,
    pub(crate) thread_id: Option<String>,
    pub(crate) task_running: bool,
    pub(crate) chatgpt_base_url: String,
    pub(crate) auth_credentials_store_mode: AuthCredentialsStoreMode,
    pub(crate) forced_chatgpt_workspace_id: Option<String>,
    pub(crate) animations: bool,
    pub(crate) auth_manager: Arc<AuthManager>,
    pub(crate) sandbox_policy: SandboxPolicy,
    pub(crate) file_system_sandbox_policy: FileSystemSandboxPolicy,
    pub(crate) network_sandbox_policy: NetworkSandboxPolicy,
    pub(crate) codex_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) windows_sandbox_level: WindowsSandboxLevel,
    pub(crate) windows_sandbox_private_desktop: bool,
    pub(crate) app_event_tx: AppEventSender,
    pub(crate) frame_requester: FrameRequester,
}

pub(crate) enum SlopForkUiEffect {
    ShowOrReplaceSelection(Box<SelectionViewParams>),
    ShowLoginView(Box<dyn BottomPaneView>),
    DismissLoginView,
    AddInfoMessage {
        message: String,
        hint: Option<String>,
    },
    AddErrorMessage(String),
    AddPlainHistoryLines(Vec<Line<'static>>),
    AuthStateChanged {
        message: String,
        is_error: bool,
        is_warning: bool,
    },
    QueueAutomationPrompt {
        prompt: String,
        suppress_legacy_notify: bool,
        suppress_terminal_notification: bool,
    },
    SubmitPilotTurn {
        prompt: String,
        cycle_kind: PilotCycleKind,
        notify_on_completion: bool,
    },
    SubmitAutoresearchTurn {
        prompt: String,
        cycle_kind: AutoresearchCycleKind,
        notify_on_completion: bool,
    },
    SubmitAutoresearchSetupTurn {
        prompt: String,
    },
    ScheduleFrameIn(Duration),
}

#[derive(Debug, Default)]
pub(crate) struct SlopForkUi {
    automation_registry: Option<AutomationRegistry>,
    autoresearch_runtime: Option<AutoresearchRuntime>,
    pilot_runtime: Option<PilotRuntime>,
    awaiting_autoresearch_turn_start: bool,
    recovered_autoresearch_turn_start: bool,
    next_autoresearch_turn_requires_started_event: bool,
    awaiting_pilot_turn_start: bool,
    last_manual_user_message: Option<String>,
    pending_chatgpt_login: Option<PendingChatgptLogin>,
    active_login_popup_kind: Option<LoginPopupKind>,
    saved_account_rate_limits_refresh: Option<SavedAccountRateLimitsRefreshState>,
    pending_automation_policies: HashSet<(String, String)>,
    auto_command_skill_conflict_warned: bool,
}

impl SlopForkUi {
    pub(crate) fn note_manual_user_message(&mut self, message: &str) {
        let trimmed = message.trim();
        if !trimmed.is_empty() {
            self.awaiting_autoresearch_turn_start = false;
            self.recovered_autoresearch_turn_start = false;
            self.next_autoresearch_turn_requires_started_event = false;
            self.last_manual_user_message = Some(trimmed.to_string());
        }
    }

    pub(crate) fn last_manual_user_message(&self) -> Option<&str> {
        self.last_manual_user_message.as_deref()
    }

    pub(crate) fn note_successful_outbound_op(&mut self, op: &Op) {
        if matches!(
            op,
            Op::UserInput { .. }
                | Op::UserTurn { .. }
                | Op::Review { .. }
                | Op::RunUserShellCommand { .. }
                | Op::SlopForkPilotTurn { .. }
        ) {
            self.awaiting_autoresearch_turn_start = false;
            self.recovered_autoresearch_turn_start = false;
            self.next_autoresearch_turn_requires_started_event = false;
        }
    }

    pub(crate) fn on_session_configured(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        self.last_manual_user_message = None;
        self.pending_automation_policies.clear();
        let preserve_awaiting_autoresearch_turn_start = self.awaiting_autoresearch_turn_start;
        self.recovered_autoresearch_turn_start = false;
        self.next_autoresearch_turn_requires_started_event = false;
        self.awaiting_pilot_turn_start = false;
        let Some(thread_id) = ctx.thread_id.as_deref() else {
            self.awaiting_autoresearch_turn_start = false;
            self.recovered_autoresearch_turn_start = false;
            self.next_autoresearch_turn_requires_started_event = false;
            self.automation_registry = None;
            self.autoresearch_runtime = None;
            self.pilot_runtime = None;
            return Vec::new();
        };
        let preserve_awaiting_autoresearch_turn_start = preserve_awaiting_autoresearch_turn_start
            && ctx.task_running
            && self
                .autoresearch_runtime
                .as_ref()
                .is_some_and(|runtime| runtime.thread_id() == thread_id);
        let mut recover_autoresearch_turn_start = false;
        let mut effects = Vec::new();
        self.awaiting_autoresearch_turn_start = false;
        match AutomationRegistry::load(&ctx.codex_home, &ctx.cwd, thread_id) {
            Ok(registry) => {
                self.automation_registry = Some(registry);
            }
            Err(err) => {
                self.automation_registry = None;
                effects.push(SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load automation state: {err}"
                )));
            }
        }
        match AutoresearchRuntime::load(&ctx.codex_home, thread_id) {
            Ok(runtime) => {
                recover_autoresearch_turn_start =
                    !preserve_awaiting_autoresearch_turn_start && runtime.has_pending_turn_start();
                self.autoresearch_runtime = Some(runtime);
            }
            Err(err) => {
                self.autoresearch_runtime = None;
                effects.push(SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load autoresearch state: {err}"
                )));
            }
        }
        self.awaiting_autoresearch_turn_start = preserve_awaiting_autoresearch_turn_start;
        self.recovered_autoresearch_turn_start = recover_autoresearch_turn_start;
        match PilotRuntime::load(&ctx.codex_home, thread_id) {
            Ok(runtime) => {
                self.pilot_runtime = Some(runtime);
            }
            Err(err) => {
                self.pilot_runtime = None;
                effects.push(SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load pilot state: {err}"
                )));
            }
        }
        effects
    }

    pub(crate) fn on_skills_loaded(&mut self, skills: &[ProtocolSkillMetadata]) -> Option<String> {
        let has_conflict = skills
            .iter()
            .any(|skill| skill.enabled && skill.name == AUTO_COMMAND_NAME);
        let should_warn = has_conflict && !self.auto_command_skill_conflict_warned;
        self.auto_command_skill_conflict_warned = has_conflict;
        should_warn.then(auto_command_skill_conflict_warning)
    }

    pub(crate) fn handle_event(
        &mut self,
        ctx: &SlopForkUiContext,
        event: SlopForkEvent,
    ) -> Vec<SlopForkUiEffect> {
        match event {
            SlopForkEvent::OpenLoginPopup { kind } => self.open_login_popup(ctx, kind),
            SlopForkEvent::OpenLoginApiKeyPrompt => self.open_login_api_key_prompt(ctx),
            SlopForkEvent::StartLoginFlow { kind } => self.start_login_flow(ctx, kind),
            SlopForkEvent::CancelPendingLogin => self.cancel_pending_chatgpt_login(),
            SlopForkEvent::SaveLoginApiKey { api_key } => self.save_login_api_key(ctx, api_key),
            SlopForkEvent::RefreshSavedAccountRateLimits => {
                self.refresh_saved_account_rate_limits(ctx)
            }
            SlopForkEvent::RefreshAllSavedAccountRateLimits => {
                self.refresh_all_saved_account_rate_limits(ctx)
            }
            SlopForkEvent::RefreshSavedAccountRateLimit { account_id } => {
                self.refresh_saved_account_rate_limit(ctx, &account_id)
            }
            SlopForkEvent::PendingDeviceCodeLoginReady {
                verification_url,
                user_code,
            } => self.on_pending_device_code_login_ready(ctx, verification_url, user_code),
            SlopForkEvent::SavedAccountRateLimitsRefreshCompleted { .. }
            | SlopForkEvent::SavedAccountQuotaTouchCompleted { .. } => Vec::new(),
            SlopForkEvent::ActivateSavedAccount { account_id } => {
                self.activate_saved_account(ctx, &account_id)
            }
            SlopForkEvent::RenameAllSavedAccountFiles => self.rename_all_saved_account_files(ctx),
            SlopForkEvent::RenameSavedAccountFile { path } => {
                self.rename_saved_account_file(ctx, &path)
            }
            SlopForkEvent::RemoveSavedAccount { account_id } => {
                self.remove_saved_account(ctx, &account_id)
            }
            SlopForkEvent::AutomationPolicyEvaluated {
                thread_id,
                runtime_id,
                decision,
            } => self.on_automation_policy_evaluated(ctx, &thread_id, &runtime_id, decision),
            SlopForkEvent::AutomationPolicyFailed {
                thread_id,
                runtime_id,
                error,
            } => self.on_automation_policy_failed(ctx, &thread_id, &runtime_id, error),
            SlopForkEvent::SaveLoginSettings { settings } => {
                self.save_login_settings(&ctx.codex_home, settings)
            }
        }
    }

    fn ensure_automation_registry(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Result<&mut AutomationRegistry, String> {
        let Some(thread_id) = ctx.thread_id.as_deref() else {
            return Err("Automations require an active session.".to_string());
        };
        let must_reload = self
            .automation_registry
            .as_ref()
            .is_none_or(|registry| registry.thread_id() != thread_id);
        if must_reload {
            self.automation_registry = Some(
                AutomationRegistry::load(&ctx.codex_home, &ctx.cwd, thread_id)
                    .map_err(|err| format!("Failed to load automation state: {err}"))?,
            );
        }
        self.automation_registry
            .as_mut()
            .ok_or_else(|| "Automations require an active session.".to_string())
    }
}
