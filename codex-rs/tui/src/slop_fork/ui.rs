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
use codex_app_server_protocol::Automation as AppServerAutomation;
use codex_app_server_protocol::AutomationDefinition as AppServerAutomationDefinition;
use codex_app_server_protocol::AutomationScope as AppServerAutomationScope;
use codex_app_server_protocol::AutomationUpdateType;
use codex_app_server_protocol::AutoresearchControlAction as AppServerAutoresearchControlAction;
use codex_app_server_protocol::AutoresearchMode as AppServerAutoresearchMode;
use codex_app_server_protocol::AutoresearchRun as AppServerAutoresearchRun;
use codex_app_server_protocol::PilotControlAction as AppServerPilotControlAction;
use codex_app_server_protocol::PilotRun as AppServerPilotRun;
use codex_app_server_protocol::PilotUpdateType;
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
use super::event::SavedAccountDeletionRequest;
use super::event::SlopForkEvent;
use super::login_settings_view::LoginSettingsView;
use super::runtime_event::fallback_autoresearch_status_message;
use super::runtime_event::fallback_pilot_status_message;
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
    pub(crate) remote_app_server: bool,
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
    RefreshRemoteAutomationState {
        thread_id: String,
    },
    SubmitAutoresearchSetupTurn {
        prompt: String,
    },
    RefreshRemotePilotState {
        thread_id: String,
    },
    RefreshRemoteAutoresearchState {
        thread_id: String,
    },
    StartRemotePilot {
        thread_id: String,
        goal: String,
        deadline_at: Option<i64>,
    },
    ControlRemotePilot {
        thread_id: String,
        action: AppServerPilotControlAction,
    },
    StartRemoteAutoresearch {
        thread_id: String,
        goal: String,
        max_runs: Option<u32>,
        mode: AppServerAutoresearchMode,
    },
    ControlRemoteAutoresearch {
        thread_id: String,
        action: AppServerAutoresearchControlAction,
        focus: Option<String>,
    },
    UpsertRemoteAutomation {
        thread_id: String,
        scope: AppServerAutomationScope,
        automation: AppServerAutomationDefinition,
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
    ScheduleFrameIn(Duration),
}

pub(crate) enum SlopForkTurnAbortCause {
    Interrupted,
    Failed,
    #[cfg(test)]
    Replaced,
    #[cfg(test)]
    ReviewEnded,
}

pub(crate) enum SlopForkRuntimeEvent<'a> {
    ControllerTurnStarted {
        turn_id: &'a str,
        from_replay: bool,
    },
    ControllerTurnAborted {
        turn_id: Option<&'a str>,
        cause: SlopForkTurnAbortCause,
        from_replay: bool,
    },
    AutomationUpdated {
        update_type: AutomationUpdateType,
        runtime_id: &'a str,
        automation: Option<Box<AppServerAutomation>>,
        message: Option<String>,
        from_replay: bool,
    },
    AutoresearchUpdated {
        update_type: codex_app_server_protocol::AutoresearchUpdateType,
        run: Option<Box<AppServerAutoresearchRun>>,
        message: Option<String>,
        from_replay: bool,
    },
    PilotUpdated {
        update_type: PilotUpdateType,
        run: Option<Box<AppServerPilotRun>>,
        message: Option<String>,
        from_replay: bool,
    },
}

#[derive(Debug, Default)]
pub(crate) struct SlopForkUi {
    automation_registry: Option<AutomationRegistry>,
    remote_automations: Vec<AppServerAutomation>,
    remote_automation_bootstrap_touched_runtime_ids: HashSet<String>,
    autoresearch_runtime: Option<AutoresearchRuntime>,
    pilot_runtime: Option<PilotRuntime>,
    remote_automation_bootstrap_pending: bool,
    remote_automation_state_loaded: bool,
    remote_autoresearch_run: Option<AppServerAutoresearchRun>,
    remote_autoresearch_bootstrap_pending: bool,
    remote_autoresearch_readback_pending: bool,
    remote_autoresearch_state_loaded: bool,
    remote_pilot_run: Option<AppServerPilotRun>,
    remote_pilot_bootstrap_pending: bool,
    remote_pilot_readback_pending: bool,
    remote_pilot_state_loaded: bool,
    awaiting_autoresearch_turn_start: bool,
    recovered_autoresearch_turn_start: bool,
    next_autoresearch_turn_requires_started_event: bool,
    awaiting_pilot_turn_start: bool,
    last_manual_user_message: Option<String>,
    pending_chatgpt_login: Option<PendingChatgptLogin>,
    active_login_popup_kind: Option<LoginPopupKind>,
    pending_saved_account_deletion: Option<SavedAccountDeletionRequest>,
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
            self.remote_automations.clear();
            self.remote_automation_bootstrap_touched_runtime_ids.clear();
            self.autoresearch_runtime = None;
            self.pilot_runtime = None;
            self.remote_automation_bootstrap_pending = false;
            self.remote_automation_state_loaded = false;
            self.remote_autoresearch_run = None;
            self.remote_autoresearch_bootstrap_pending = false;
            self.remote_autoresearch_readback_pending = false;
            self.remote_autoresearch_state_loaded = false;
            self.remote_pilot_run = None;
            self.remote_pilot_bootstrap_pending = false;
            self.remote_pilot_readback_pending = false;
            self.remote_pilot_state_loaded = false;
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
        self.remote_automations.clear();
        self.remote_automation_bootstrap_touched_runtime_ids.clear();
        self.remote_automation_state_loaded = false;
        self.remote_autoresearch_run = None;
        self.remote_autoresearch_bootstrap_pending = ctx.remote_app_server;
        self.remote_autoresearch_readback_pending = false;
        self.remote_autoresearch_state_loaded = false;
        if ctx.remote_app_server {
            self.automation_registry = None;
        } else {
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
        }
        if ctx.remote_app_server {
            self.autoresearch_runtime = None;
        } else {
            match AutoresearchRuntime::load(&ctx.codex_home, thread_id) {
                Ok(runtime) => {
                    recover_autoresearch_turn_start = !preserve_awaiting_autoresearch_turn_start
                        && runtime.has_pending_turn_start();
                    self.autoresearch_runtime = Some(runtime);
                }
                Err(err) => {
                    self.autoresearch_runtime = None;
                    effects.push(SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to load autoresearch state: {err}"
                    )));
                }
            }
        }
        self.awaiting_autoresearch_turn_start = preserve_awaiting_autoresearch_turn_start;
        self.recovered_autoresearch_turn_start = recover_autoresearch_turn_start;
        self.remote_pilot_run = None;
        self.remote_automation_bootstrap_pending = ctx.remote_app_server;
        self.remote_pilot_bootstrap_pending = ctx.remote_app_server;
        self.remote_pilot_readback_pending = false;
        self.remote_pilot_state_loaded = false;
        if ctx.remote_app_server {
            self.pilot_runtime = None;
        } else {
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
        }
        if ctx.remote_app_server {
            effects.push(SlopForkUiEffect::RefreshRemoteAutomationState {
                thread_id: thread_id.to_string(),
            });
            effects.push(SlopForkUiEffect::RefreshRemoteAutoresearchState {
                thread_id: thread_id.to_string(),
            });
            effects.push(SlopForkUiEffect::RefreshRemotePilotState {
                thread_id: thread_id.to_string(),
            });
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

    pub(crate) fn on_controller_turn_started(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        let mut effects = self.on_pilot_turn_started(ctx, turn_id, from_replay);
        effects.extend(self.on_autoresearch_turn_started(ctx, turn_id, from_replay));
        effects
    }

    pub(crate) fn on_controller_turn_aborted(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: Option<&str>,
        reason: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        let mut effects = self.on_pilot_turn_aborted(ctx, turn_id, reason, from_replay);
        effects.extend(self.on_autoresearch_turn_aborted(ctx, turn_id, reason, from_replay));
        effects
    }

    pub(crate) fn on_automation_updated(
        &mut self,
        ctx: &SlopForkUiContext,
        update_type: AutomationUpdateType,
        runtime_id: &str,
        automation: Option<AppServerAutomation>,
        message: Option<String>,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        if ctx.remote_app_server {
            self.automation_registry = None;
            self.remote_automation_state_loaded = true;
            if self.remote_automation_bootstrap_pending {
                self.remote_automation_bootstrap_touched_runtime_ids
                    .insert(runtime_id.to_string());
            }
            match update_type {
                AutomationUpdateType::Deleted => {
                    self.remote_automations
                        .retain(|entry| entry.runtime_id != runtime_id);
                }
                _ => {
                    if let Some(automation) = automation {
                        if let Some(existing) = self
                            .remote_automations
                            .iter_mut()
                            .find(|entry| entry.runtime_id == automation.runtime_id)
                        {
                            *existing = automation;
                        } else {
                            self.remote_automations.push(automation);
                        }
                    }
                }
            }
        }
        message
            .map(|message| SlopForkUiEffect::AddInfoMessage {
                message: format!("Automation {runtime_id}: {message}"),
                hint: None,
            })
            .into_iter()
            .collect()
    }

    pub(crate) fn on_pilot_updated(
        &mut self,
        ctx: &SlopForkUiContext,
        update_type: PilotUpdateType,
        run: Option<AppServerPilotRun>,
        message: Option<String>,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        self.remote_pilot_run = run.clone();
        if ctx.remote_app_server {
            self.pilot_runtime = None;
            if !from_replay && !self.remote_pilot_readback_pending {
                self.remote_pilot_bootstrap_pending = false;
            }
            self.remote_pilot_state_loaded = true;
            self.awaiting_pilot_turn_start = false;
            if from_replay {
                return Vec::new();
            }
        } else if from_replay {
            return Vec::new();
        } else if let Some(thread_id) = ctx.thread_id.as_deref() {
            self.pilot_runtime = PilotRuntime::load(&ctx.codex_home, thread_id).ok();
        } else {
            self.pilot_runtime = None;
            self.remote_pilot_state_loaded = false;
        }
        let message = message.or_else(|| fallback_pilot_status_message(update_type, run.as_ref()));
        message
            .map(|message| SlopForkUiEffect::AddInfoMessage {
                message,
                hint: None,
            })
            .into_iter()
            .collect()
    }

    pub(crate) fn handle_runtime_event(
        &mut self,
        ctx: &SlopForkUiContext,
        event: SlopForkRuntimeEvent<'_>,
    ) -> Vec<SlopForkUiEffect> {
        match event {
            SlopForkRuntimeEvent::ControllerTurnStarted {
                turn_id,
                from_replay,
            } => self.on_controller_turn_started(ctx, turn_id, from_replay),
            SlopForkRuntimeEvent::ControllerTurnAborted {
                turn_id,
                cause,
                from_replay,
            } => {
                let reason = match cause {
                    SlopForkTurnAbortCause::Interrupted => {
                        "Controller-owned turn interrupted by the user."
                    }
                    SlopForkTurnAbortCause::Failed => {
                        "Controller-owned turn failed before completion."
                    }
                    #[cfg(test)]
                    SlopForkTurnAbortCause::Replaced => {
                        "Controller-owned turn was replaced by another task."
                    }
                    #[cfg(test)]
                    SlopForkTurnAbortCause::ReviewEnded => {
                        "Controller-owned turn ended because review mode finished."
                    }
                };
                self.on_controller_turn_aborted(ctx, turn_id, reason, from_replay)
            }
            SlopForkRuntimeEvent::AutomationUpdated {
                update_type,
                runtime_id,
                automation,
                message,
                from_replay,
            } => self.on_automation_updated(
                ctx,
                update_type,
                runtime_id,
                automation.map(|automation| *automation),
                message,
                from_replay,
            ),
            SlopForkRuntimeEvent::AutoresearchUpdated {
                update_type,
                run,
                message,
                from_replay,
            } => self.on_autoresearch_updated(
                ctx,
                update_type,
                run.map(|run| *run),
                message,
                from_replay,
            ),
            SlopForkRuntimeEvent::PilotUpdated {
                update_type,
                run,
                message,
                from_replay,
            } => self.on_pilot_updated(ctx, update_type, run.map(|run| *run), message, from_replay),
        }
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
            SlopForkEvent::ConfirmSavedAccountDeletion { request } => {
                self.confirm_saved_account_deletion(ctx, request)
            }
            SlopForkEvent::RenameAllSavedAccountFiles => self.rename_all_saved_account_files(ctx),
            SlopForkEvent::RenameSavedAccountFile { path } => {
                self.rename_saved_account_file(ctx, &path)
            }
            SlopForkEvent::RemoveSavedAccount { account_id } => {
                self.remove_saved_account(ctx, &account_id)
            }
            SlopForkEvent::RemoveSavedAccounts { account_ids } => {
                self.remove_saved_accounts(ctx, &account_ids)
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
            SlopForkEvent::FetchRemoteAutomationState { .. }
            | SlopForkEvent::RemoteAutomationStateLoaded { .. }
            | SlopForkEvent::FetchRemoteAutoresearchState { .. }
            | SlopForkEvent::RemoteAutoresearchStateLoaded { .. }
            | SlopForkEvent::FetchRemotePilotState { .. }
            | SlopForkEvent::RemotePilotStateLoaded { .. }
            | SlopForkEvent::StartRemotePilot { .. }
            | SlopForkEvent::ControlRemotePilot { .. }
            | SlopForkEvent::StartRemoteAutoresearch { .. }
            | SlopForkEvent::ControlRemoteAutoresearch { .. }
            | SlopForkEvent::UpsertRemoteAutomation { .. }
            | SlopForkEvent::SetRemoteAutomationEnabled { .. }
            | SlopForkEvent::DeleteRemoteAutomation { .. }
            | SlopForkEvent::RemoteActionFailed { .. } => Vec::new(),
            SlopForkEvent::SaveLoginSettings { settings } => {
                self.save_login_settings(&ctx.codex_home, settings)
            }
        }
    }

    fn ensure_automation_registry(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Result<&mut AutomationRegistry, String> {
        if ctx.remote_app_server {
            return Err(
                "Automation runtime is server-owned when connected to a remote app-server."
                    .to_string(),
            );
        }
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

    #[cfg(test)]
    pub(crate) fn cached_pilot_runtime_state(
        &self,
    ) -> Option<&codex_core::slop_fork::pilot::PilotRunState> {
        self.pilot_runtime
            .as_ref()
            .and_then(|runtime| runtime.state())
    }

    #[cfg(test)]
    pub(crate) fn cached_remote_pilot_run(&self) -> Option<&AppServerPilotRun> {
        self.remote_pilot_run.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn remote_pilot_state_loaded(&self) -> bool {
        self.remote_pilot_state_loaded
    }

    pub(crate) fn on_remote_automation_state_loaded(
        &mut self,
        automations: Vec<AppServerAutomation>,
    ) -> bool {
        if !self.remote_automation_bootstrap_pending {
            return false;
        }
        self.automation_registry = None;
        let touched_runtime_ids =
            std::mem::take(&mut self.remote_automation_bootstrap_touched_runtime_ids);
        if touched_runtime_ids.is_empty() {
            self.remote_automations = automations;
        } else {
            let mut merged = std::mem::take(&mut self.remote_automations);
            for automation in automations {
                if touched_runtime_ids.contains(&automation.runtime_id) {
                    continue;
                }
                if let Some(existing) = merged
                    .iter_mut()
                    .find(|entry| entry.runtime_id == automation.runtime_id)
                {
                    *existing = automation;
                } else {
                    merged.push(automation);
                }
            }
            self.remote_automations = merged;
        }
        self.remote_automation_bootstrap_pending = false;
        self.remote_automation_state_loaded = true;
        true
    }

    pub(crate) fn on_remote_pilot_state_loaded(
        &mut self,
        run: Option<AppServerPilotRun>,
        authoritative: bool,
    ) -> bool {
        if !self.remote_pilot_bootstrap_pending {
            return false;
        }
        if self.remote_pilot_readback_pending
            && !authoritative
            && self.remote_pilot_run.is_some()
            && run.is_none()
        {
            self.remote_pilot_bootstrap_pending = false;
            self.remote_pilot_readback_pending = false;
            return false;
        }
        if self.remote_pilot_readback_pending
            && let (Some(current), Some(candidate)) = (self.remote_pilot_run.as_ref(), run.as_ref())
            && if authoritative {
                candidate.updated_at < current.updated_at
            } else {
                candidate.updated_at <= current.updated_at
            }
        {
            self.remote_pilot_bootstrap_pending = false;
            self.remote_pilot_readback_pending = false;
            return false;
        }
        self.remote_pilot_run = run;
        self.pilot_runtime = None;
        self.remote_pilot_bootstrap_pending = false;
        self.remote_pilot_readback_pending = false;
        self.remote_pilot_state_loaded = true;
        true
    }

    pub(crate) fn on_remote_pilot_state_load_failed(&mut self) -> bool {
        if !self.remote_pilot_bootstrap_pending {
            return false;
        }
        self.remote_pilot_run = None;
        self.pilot_runtime = None;
        self.remote_pilot_bootstrap_pending = false;
        self.remote_pilot_readback_pending = false;
        self.remote_pilot_state_loaded = false;
        self.awaiting_pilot_turn_start = false;
        true
    }

    pub(crate) fn arm_remote_pilot_state_reload(&mut self) {
        self.remote_pilot_bootstrap_pending = true;
        self.remote_pilot_readback_pending = true;
    }

    pub(crate) fn on_remote_autoresearch_state_loaded(
        &mut self,
        run: Option<AppServerAutoresearchRun>,
        authoritative: bool,
    ) -> bool {
        if !self.remote_autoresearch_bootstrap_pending {
            return false;
        }
        if self.remote_autoresearch_readback_pending
            && !authoritative
            && self.remote_autoresearch_run.is_some()
            && run.is_none()
        {
            self.remote_autoresearch_bootstrap_pending = false;
            self.remote_autoresearch_readback_pending = false;
            return false;
        }
        if self.remote_autoresearch_readback_pending
            && let (Some(current), Some(candidate)) =
                (self.remote_autoresearch_run.as_ref(), run.as_ref())
            && if authoritative {
                candidate.updated_at < current.updated_at
            } else {
                candidate.updated_at <= current.updated_at
            }
        {
            self.remote_autoresearch_bootstrap_pending = false;
            self.remote_autoresearch_readback_pending = false;
            return false;
        }
        self.autoresearch_runtime = None;
        self.remote_autoresearch_run = run;
        self.remote_autoresearch_bootstrap_pending = false;
        self.remote_autoresearch_readback_pending = false;
        self.remote_autoresearch_state_loaded = true;
        self.awaiting_autoresearch_turn_start = false;
        self.recovered_autoresearch_turn_start = false;
        self.next_autoresearch_turn_requires_started_event = false;
        true
    }

    pub(crate) fn on_remote_autoresearch_state_load_failed(&mut self) -> bool {
        if !self.remote_autoresearch_bootstrap_pending {
            return false;
        }
        self.remote_autoresearch_run = None;
        self.autoresearch_runtime = None;
        self.remote_autoresearch_bootstrap_pending = false;
        self.remote_autoresearch_readback_pending = false;
        self.remote_autoresearch_state_loaded = false;
        self.awaiting_autoresearch_turn_start = false;
        self.recovered_autoresearch_turn_start = false;
        self.next_autoresearch_turn_requires_started_event = false;
        true
    }

    pub(crate) fn arm_remote_autoresearch_state_reload(&mut self) {
        self.remote_autoresearch_bootstrap_pending = true;
        self.remote_autoresearch_readback_pending = true;
    }

    #[cfg(test)]
    pub(crate) fn cached_remote_automations(&self) -> &[AppServerAutomation] {
        &self.remote_automations
    }

    #[cfg(test)]
    pub(crate) fn cached_remote_autoresearch_run(&self) -> Option<&AppServerAutoresearchRun> {
        self.remote_autoresearch_run.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn remote_autoresearch_state_loaded(&self) -> bool {
        self.remote_autoresearch_state_loaded
    }
}
