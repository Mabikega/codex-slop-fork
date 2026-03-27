use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_protocol::Automation;
use codex_app_server_protocol::AutomationDefinition;
use codex_app_server_protocol::AutomationDeleteParams;
use codex_app_server_protocol::AutomationDeleteResponse;
use codex_app_server_protocol::AutomationLimits;
use codex_app_server_protocol::AutomationListParams;
use codex_app_server_protocol::AutomationListResponse;
use codex_app_server_protocol::AutomationMessageSource;
use codex_app_server_protocol::AutomationPolicyCommand;
use codex_app_server_protocol::AutomationScope;
use codex_app_server_protocol::AutomationSetEnabledParams;
use codex_app_server_protocol::AutomationSetEnabledResponse;
use codex_app_server_protocol::AutomationTrigger;
use codex_app_server_protocol::AutomationUpsertParams;
use codex_app_server_protocol::AutomationUpsertResponse;
use codex_app_server_protocol::AutoresearchControlAction;
use codex_app_server_protocol::AutoresearchControlParams;
use codex_app_server_protocol::AutoresearchControlResponse;
use codex_app_server_protocol::AutoresearchMode as ApiAutoresearchMode;
use codex_app_server_protocol::AutoresearchStartParams;
use codex_app_server_protocol::AutoresearchStartResponse;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::LoginAccountParams;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::PilotControlAction;
use codex_app_server_protocol::PilotControlParams;
use codex_app_server_protocol::PilotControlResponse;
use codex_app_server_protocol::PilotCycleKind;
use codex_app_server_protocol::PilotReadParams;
use codex_app_server_protocol::PilotReadResponse;
use codex_app_server_protocol::PilotRun;
use codex_app_server_protocol::PilotStartParams;
use codex_app_server_protocol::PilotStartResponse;
use codex_app_server_protocol::PilotStatus;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SavedAccountActivateParams;
use codex_app_server_protocol::SavedAccountActivateResponse;
use codex_app_server_protocol::SavedAccountRemoveParams;
use codex_app_server_protocol::SavedAccountRemoveResponse;
use codex_core::slop_fork::account_rate_limits;
use codex_core::slop_fork::auth_accounts::StoredAccount;
use codex_core::slop_fork::automation::AutomationMessageSource as CoreAutomationMessageSource;
use codex_core::slop_fork::automation::AutomationScope as CoreAutomationScope;
use codex_core::slop_fork::automation::AutomationSpec;
use codex_core::slop_fork::automation::AutomationTrigger as CoreAutomationTrigger;
use codex_core::slop_fork::autoresearch::AutoresearchJournal;
use codex_core::slop_fork::autoresearch::AutoresearchMode as CoreAutoresearchMode;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::autoresearch::build_init_prompt;
use codex_core::slop_fork::autoresearch::build_open_init_prompt;
use ratatui::style::Stylize;
use ratatui::text::Line;
use uuid::Uuid;

use codex_protocol::user_input::TextElement;

pub(crate) mod account_limits;
pub(crate) mod account_settings_view;
pub(crate) mod account_views;
pub(crate) mod accounts;
pub(crate) mod auto_command;
pub(crate) mod autoresearch_command;
pub(crate) mod pilot_command;
pub(crate) mod rate_limit_poller;
pub(crate) mod schedule_parser;

pub(crate) use account_limits::SavedAccountLimitsOverview;
pub(crate) use account_limits::SavedAccountRateLimitsRefreshState;
pub(crate) use account_limits::SavedAccountRateLimitsRefreshTarget;
pub(crate) use account_limits::load_saved_account_limits_overview;
pub(crate) use accounts::AccountsPopupContext;
pub(crate) use accounts::AccountsRootOverview;
pub(crate) use accounts::LoginSettingsState;
pub(crate) use accounts::RenameAccountsPopupOverview;
pub(crate) use accounts::SavedAccountsPopupOverview;
pub(crate) use accounts::load_accounts_popup_context;
pub(crate) use accounts::load_accounts_root_overview;
pub(crate) use accounts::load_login_settings_state;
pub(crate) use accounts::load_rename_accounts_popup;
pub(crate) use accounts::load_saved_accounts_popup;
pub(crate) use accounts::rename_all_saved_account_files;
pub(crate) use accounts::rename_saved_account_file;
pub(crate) use accounts::save_login_settings;
pub(crate) use rate_limit_poller::refresh_saved_account_rate_limits_once;
pub(crate) use rate_limit_poller::should_spawn_rate_limit_poller;
pub(crate) use rate_limit_poller::spawn_rate_limit_poller;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SavedAccountMenuMode {
    Activate,
    Remove,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceCodeLoginState {
    Requesting,
    Ready {
        verification_url: String,
        user_code: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SlopForkCommandHistory {
    pub(crate) text: String,
    pub(crate) text_elements: Vec<TextElement>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SavedAccountRateLimitsRefreshCompletionSource {
    Background,
    Requested,
}

#[derive(Debug)]
pub(crate) enum SlopForkEvent {
    OpenAccountsRoot,
    OpenSavedAccounts {
        mode: SavedAccountMenuMode,
    },
    OpenSavedAccountRenames,
    OpenSavedAccountLimits,
    OpenAccountsSettings,
    OpenAccountsApiKeyPrompt,
    StartChatgptLogin,
    StartDeviceCodeLogin,
    CancelPendingDeviceCodeLogin,
    PendingDeviceCodeLoginReady {
        verification_url: String,
        user_code: String,
    },
    FinishDeviceCodeLogin {
        message: String,
        is_error: bool,
    },
    SubmitApiKeyLogin {
        api_key: String,
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
    SaveAccountsSettings {
        settings: LoginSettingsState,
    },
    RefreshSavedAccountRateLimits,
    RefreshAllSavedAccountRateLimits,
    RefreshSavedAccountRateLimit {
        account_id: String,
    },
    SavedAccountRateLimitsRefreshCompleted {
        updated_account_ids: Vec<String>,
        source: SavedAccountRateLimitsRefreshCompletionSource,
    },
    ExecutePilot {
        args: String,
        history: Option<SlopForkCommandHistory>,
    },
    ExecuteAutoresearch {
        args: String,
        history: Option<SlopForkCommandHistory>,
    },
    ExecuteAuto {
        args: String,
        history: Option<SlopForkCommandHistory>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SavedAccountEntry {
    pub(crate) account_id: String,
    pub(crate) label: String,
    pub(crate) description: String,
    pub(crate) is_current: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct SlopForkCommandExecution {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) submit_message: Option<String>,
}

impl SlopForkCommandExecution {
    pub(crate) fn new(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines,
            submit_message: None,
        }
    }

    pub(crate) fn with_submit_message(mut self, submit_message: Option<String>) -> Self {
        self.submit_message = submit_message;
        self
    }
}

pub(crate) fn build_autoresearch_init_prompt(request: &str, open_mode: bool) -> String {
    if open_mode {
        build_open_init_prompt(request)
    } else {
        build_init_prompt(request)
    }
}

pub(crate) async fn start_chatgpt_login(
    request_handle: AppServerRequestHandle,
) -> Result<String, String> {
    let response: LoginAccountResponse = request_handle
        .request_typed(ClientRequest::LoginAccount {
            request_id: next_request_id(),
            params: LoginAccountParams::Chatgpt,
        })
        .await
        .map_err(|err| format!("ChatGPT login failed: {err}"))?;

    match response {
        LoginAccountResponse::Chatgpt { auth_url, .. } => Ok(auth_url),
        other => Err(format!("Unexpected ChatGPT login response: {other:?}")),
    }
}

pub(crate) async fn login_api_key(
    request_handle: AppServerRequestHandle,
    api_key: String,
) -> Result<(), String> {
    let response: LoginAccountResponse = request_handle
        .request_typed(ClientRequest::LoginAccount {
            request_id: next_request_id(),
            params: LoginAccountParams::ApiKey { api_key },
        })
        .await
        .map_err(|err| format!("API key login failed: {err}"))?;

    match response {
        LoginAccountResponse::ApiKey {} => Ok(()),
        other => Err(format!("Unexpected API key login response: {other:?}")),
    }
}

pub(crate) async fn login_chatgpt_auth_tokens(
    request_handle: AppServerRequestHandle,
    access_token: String,
    chatgpt_account_id: String,
    chatgpt_plan_type: Option<String>,
) -> Result<(), String> {
    let response: LoginAccountResponse = request_handle
        .request_typed(ClientRequest::LoginAccount {
            request_id: next_request_id(),
            params: LoginAccountParams::ChatgptAuthTokens {
                access_token,
                chatgpt_account_id,
                chatgpt_plan_type,
            },
        })
        .await
        .map_err(|err| format!("ChatGPT auth-token login failed: {err}"))?;

    match response {
        LoginAccountResponse::ChatgptAuthTokens {} => Ok(()),
        other => Err(format!(
            "Unexpected ChatGPT auth-token login response: {other:?}"
        )),
    }
}

pub(crate) async fn activate_saved_account(
    request_handle: AppServerRequestHandle,
    account_id: String,
) -> Result<bool, String> {
    let response: SavedAccountActivateResponse = request_handle
        .request_typed(ClientRequest::SavedAccountActivate {
            request_id: next_request_id(),
            params: SavedAccountActivateParams { account_id },
        })
        .await
        .map_err(|err| format!("Saved account activation failed: {err}"))?;
    Ok(response.activated)
}

pub(crate) async fn remove_saved_account(
    request_handle: AppServerRequestHandle,
    account_id: String,
) -> Result<bool, String> {
    let response: SavedAccountRemoveResponse = request_handle
        .request_typed(ClientRequest::SavedAccountRemove {
            request_id: next_request_id(),
            params: SavedAccountRemoveParams { account_id },
        })
        .await
        .map_err(|err| format!("Saved account removal failed: {err}"))?;
    Ok(response.removed)
}

pub(crate) async fn execute_pilot_command(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    command: pilot_command::PilotCommand,
) -> Result<SlopForkCommandExecution, String> {
    match command {
        pilot_command::PilotCommand::Help => Ok(SlopForkCommandExecution::new(
            pilot_command::pilot_usage()
                .lines()
                .map(Line::from)
                .collect(),
        )),
        pilot_command::PilotCommand::Status => {
            let response: PilotReadResponse = request_handle
                .request_typed(ClientRequest::PilotRead {
                    request_id: next_request_id(),
                    params: PilotReadParams { thread_id },
                })
                .await
                .map_err(|err| format!("Pilot status request failed: {err}"))?;
            Ok(SlopForkCommandExecution::new(format_pilot_status(
                response.run,
            )))
        }
        pilot_command::PilotCommand::Start { goal, deadline_at } => {
            let response: PilotStartResponse = request_handle
                .request_typed(ClientRequest::PilotStart {
                    request_id: next_request_id(),
                    params: PilotStartParams {
                        thread_id,
                        goal,
                        deadline_at,
                    },
                })
                .await
                .map_err(|err| format!("Pilot start failed: {err}"))?;
            Ok(SlopForkCommandExecution::new(format_pilot_started(
                response.run,
            )))
        }
        pilot_command::PilotCommand::Pause => {
            execute_pilot_control(
                request_handle,
                thread_id,
                PilotControlAction::Pause,
                "Pilot paused.",
            )
            .await
        }
        pilot_command::PilotCommand::Resume => {
            execute_pilot_control(
                request_handle,
                thread_id,
                PilotControlAction::Resume,
                "Pilot resumed.",
            )
            .await
        }
        pilot_command::PilotCommand::WrapUp => {
            execute_pilot_control(
                request_handle,
                thread_id,
                PilotControlAction::WrapUp,
                "Pilot wrap-up queued.",
            )
            .await
        }
        pilot_command::PilotCommand::Stop => {
            execute_pilot_control(
                request_handle,
                thread_id,
                PilotControlAction::Stop,
                "Pilot stopped.",
            )
            .await
        }
    }
}

pub(crate) async fn execute_autoresearch_command(
    request_handle: AppServerRequestHandle,
    codex_home: &Path,
    thread_id: String,
    fallback_workdir: &Path,
    command: autoresearch_command::AutoresearchCommand,
) -> Result<SlopForkCommandExecution, String> {
    match command {
        autoresearch_command::AutoresearchCommand::Help => Ok(SlopForkCommandExecution::new(
            autoresearch_command::autoresearch_usage()
                .lines()
                .map(Line::from)
                .collect(),
        )),
        autoresearch_command::AutoresearchCommand::Init { .. } => {
            Err("Autoresearch init should be handled directly by the chat widget.".to_string())
        }
        autoresearch_command::AutoresearchCommand::Status => Ok(SlopForkCommandExecution::new(
            format_autoresearch_status(codex_home, &thread_id, fallback_workdir)?,
        )),
        autoresearch_command::AutoresearchCommand::Portfolio => Ok(SlopForkCommandExecution::new(
            format_autoresearch_portfolio(codex_home, &thread_id, fallback_workdir)?,
        )),
        autoresearch_command::AutoresearchCommand::Discover { focus } => {
            let response: AutoresearchControlResponse = request_handle
                .request_typed(ClientRequest::AutoresearchControl {
                    request_id: next_request_id(),
                    params: AutoresearchControlParams {
                        thread_id: thread_id.clone(),
                        action: AutoresearchControlAction::Discover,
                        focus,
                    },
                })
                .await
                .map_err(|err| format!("Autoresearch discovery failed: {err}"))?;
            Ok(SlopForkCommandExecution::new(vec![Line::from(
                if response.updated {
                    "Autoresearch discovery queued.".to_string()
                } else {
                    "Autoresearch discovery request was ignored because there is nothing to queue right now."
                        .to_string()
                },
            )]))
        }
        autoresearch_command::AutoresearchCommand::Start {
            goal,
            max_runs,
            mode,
        } => {
            let response: AutoresearchStartResponse = request_handle
                .request_typed(ClientRequest::AutoresearchStart {
                    request_id: next_request_id(),
                    params: AutoresearchStartParams {
                        thread_id: thread_id.clone(),
                        goal: goal.clone(),
                        mode: core_autoresearch_mode_to_api(mode),
                        max_runs,
                    },
                })
                .await
                .map_err(|err| format!("Autoresearch start failed: {err}"))?;
            Ok(SlopForkCommandExecution::new(vec![Line::from(
                if response.updated {
                    format!(
                        "Autoresearch started in {} mode for goal: {goal}",
                        mode.cli_name()
                    )
                } else {
                    "Autoresearch start request was ignored.".to_string()
                },
            )]))
        }
        autoresearch_command::AutoresearchCommand::Pause => {
            execute_autoresearch_control(
                request_handle,
                thread_id,
                AutoresearchControlAction::Pause,
                "Autoresearch paused.",
            )
            .await
        }
        autoresearch_command::AutoresearchCommand::Resume => {
            execute_autoresearch_control(
                request_handle,
                thread_id,
                AutoresearchControlAction::Resume,
                "Autoresearch resumed.",
            )
            .await
        }
        autoresearch_command::AutoresearchCommand::WrapUp => {
            execute_autoresearch_control(
                request_handle,
                thread_id,
                AutoresearchControlAction::WrapUp,
                "Autoresearch wrap-up queued.",
            )
            .await
        }
        autoresearch_command::AutoresearchCommand::Stop => {
            execute_autoresearch_control(
                request_handle,
                thread_id,
                AutoresearchControlAction::Stop,
                "Autoresearch stopped.",
            )
            .await
        }
        autoresearch_command::AutoresearchCommand::Clear => {
            execute_autoresearch_control(
                request_handle,
                thread_id,
                AutoresearchControlAction::Clear,
                "Autoresearch state cleared.",
            )
            .await
        }
    }
}

pub(crate) async fn execute_auto_command(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    command: auto_command::AutoCommand,
) -> Result<SlopForkCommandExecution, String> {
    match command {
        auto_command::AutoCommand::Help => Ok(SlopForkCommandExecution::new(
            auto_command::auto_usage().lines().map(Line::from).collect(),
        )),
        auto_command::AutoCommand::List => {
            let response = automation_list(request_handle, thread_id).await?;
            Ok(SlopForkCommandExecution::new(format_automation_list(
                response.data,
            )))
        }
        auto_command::AutoCommand::Show { runtime_id } => {
            let response = automation_list(request_handle, thread_id).await?;
            let Some(automation) = response
                .data
                .into_iter()
                .find(|item| item.runtime_id == runtime_id)
            else {
                return Err(format!("No automation with runtime id {runtime_id}."));
            };
            Ok(SlopForkCommandExecution::new(format_automation_details(
                automation,
            )))
        }
        auto_command::AutoCommand::Pause { runtime_id } => {
            let response: AutomationSetEnabledResponse = request_handle
                .request_typed(ClientRequest::AutomationSetEnabled {
                    request_id: next_request_id(),
                    params: AutomationSetEnabledParams {
                        thread_id,
                        runtime_id: runtime_id.clone(),
                        enabled: false,
                    },
                })
                .await
                .map_err(|err| format!("Failed to pause automation {runtime_id}: {err}"))?;
            Ok(SlopForkCommandExecution::new(vec![Line::from(
                if response.updated {
                    format!("Automation paused: {runtime_id}")
                } else {
                    format!("No automation found for {runtime_id}.")
                },
            )]))
        }
        auto_command::AutoCommand::Resume { runtime_id } => {
            let response: AutomationSetEnabledResponse = request_handle
                .request_typed(ClientRequest::AutomationSetEnabled {
                    request_id: next_request_id(),
                    params: AutomationSetEnabledParams {
                        thread_id,
                        runtime_id: runtime_id.clone(),
                        enabled: true,
                    },
                })
                .await
                .map_err(|err| format!("Failed to resume automation {runtime_id}: {err}"))?;
            Ok(SlopForkCommandExecution::new(vec![Line::from(
                if response.updated {
                    format!("Automation resumed: {runtime_id}")
                } else {
                    format!("No automation found for {runtime_id}.")
                },
            )]))
        }
        auto_command::AutoCommand::Remove { runtime_id } => {
            let response: AutomationDeleteResponse = request_handle
                .request_typed(ClientRequest::AutomationDelete {
                    request_id: next_request_id(),
                    params: AutomationDeleteParams {
                        thread_id,
                        runtime_id: runtime_id.clone(),
                    },
                })
                .await
                .map_err(|err| format!("Failed to remove automation {runtime_id}: {err}"))?;
            Ok(SlopForkCommandExecution::new(vec![Line::from(
                if response.deleted {
                    format!("Automation removed: {runtime_id}")
                } else {
                    format!("No automation found for {runtime_id}.")
                },
            )]))
        }
        auto_command::AutoCommand::Create {
            scope,
            spec,
            note,
            send_now,
        } => {
            let response: AutomationUpsertResponse = request_handle
                .request_typed(ClientRequest::AutomationUpsert {
                    request_id: next_request_id(),
                    params: AutomationUpsertParams {
                        thread_id,
                        scope: core_automation_scope_to_api(scope),
                        automation: automation_spec_to_api(spec.clone()),
                    },
                })
                .await
                .map_err(|err| format!("Failed to create automation: {err}"))?;
            let mut lines = vec![Line::from(format!(
                "Automation saved: {}",
                response.automation.runtime_id
            ))];
            if let Some(note) = note.filter(|note| !note.trim().is_empty()) {
                lines.push(Line::from(format!("Note: {note}")));
            }
            let submit_message = if send_now {
                automation_submit_message(&spec)
            } else {
                None
            };
            if send_now {
                lines.push(Line::from("Submitting the first automation prompt now."));
            }
            Ok(SlopForkCommandExecution::new(lines).with_submit_message(submit_message))
        }
    }
}

pub(super) fn build_saved_account_description(
    account: &StoredAccount,
    snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
    is_current: bool,
) -> String {
    let mut parts = Vec::new();
    if is_current {
        parts.push("Active".to_string());
    }
    if let Some(plan) = account
        .auth
        .tokens
        .as_ref()
        .and_then(|tokens| tokens.id_token.get_chatgpt_plan_type())
    {
        parts.push(plan);
    }
    if let Some(snapshot) = snapshot {
        if let Some(used_percent) = account_rate_limits::snapshot_used_percent(snapshot) {
            parts.push(format!("{used_percent:.0}% used"));
        }
        if let Some(reset_at) = account_rate_limits::snapshot_reset_at(snapshot) {
            parts.push(format!(
                "resets {}",
                format_datetime(reset_at.with_timezone(&Local))
            ));
        }
    }
    if parts.is_empty() {
        "Saved account".to_string()
    } else {
        parts.join(" · ")
    }
}

fn next_request_id() -> RequestId {
    RequestId::String(Uuid::new_v4().to_string())
}

async fn execute_pilot_control(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    action: PilotControlAction,
    success_message: &'static str,
) -> Result<SlopForkCommandExecution, String> {
    let response: PilotControlResponse = request_handle
        .request_typed(ClientRequest::PilotControl {
            request_id: next_request_id(),
            params: PilotControlParams { thread_id, action },
        })
        .await
        .map_err(|err| format!("Pilot control request failed: {err}"))?;
    let lines = if let Some(run) = response.run {
        format_pilot_control(success_message, run)
    } else if response.updated {
        vec![Line::from(success_message)]
    } else {
        vec![Line::from(
            "Pilot request was ignored because there is no active run.",
        )]
    };
    Ok(SlopForkCommandExecution::new(lines))
}

async fn execute_autoresearch_control(
    request_handle: AppServerRequestHandle,
    thread_id: String,
    action: AutoresearchControlAction,
    success_message: &'static str,
) -> Result<SlopForkCommandExecution, String> {
    let response: AutoresearchControlResponse = request_handle
        .request_typed(ClientRequest::AutoresearchControl {
            request_id: next_request_id(),
            params: AutoresearchControlParams {
                thread_id,
                action,
                focus: None,
            },
        })
        .await
        .map_err(|err| format!("Autoresearch control request failed: {err}"))?;
    Ok(SlopForkCommandExecution::new(vec![Line::from(
        if response.updated {
            success_message.to_string()
        } else {
            "Autoresearch request was ignored because there is no matching active session."
                .to_string()
        },
    )]))
}

async fn automation_list(
    request_handle: AppServerRequestHandle,
    thread_id: String,
) -> Result<AutomationListResponse, String> {
    request_handle
        .request_typed(ClientRequest::AutomationList {
            request_id: next_request_id(),
            params: AutomationListParams { thread_id },
        })
        .await
        .map_err(|err| format!("Automation list request failed: {err}"))
}

fn format_pilot_started(run: PilotRun) -> Vec<Line<'static>> {
    let mut lines = vec!["Pilot".bold().into(), Line::from("Started new pilot run.")];
    lines.extend(format_pilot_run_body(&run));
    lines
}

fn format_pilot_control(message: &str, run: PilotRun) -> Vec<Line<'static>> {
    let mut lines = vec!["Pilot".bold().into(), Line::from(message.to_string())];
    lines.extend(format_pilot_run_body(&run));
    lines
}

fn format_pilot_status(run: Option<PilotRun>) -> Vec<Line<'static>> {
    let Some(run) = run else {
        return vec![
            "Pilot".bold().into(),
            Line::from("No pilot run is active for this thread."),
        ];
    };
    let mut lines = vec!["Pilot".bold().into()];
    lines.extend(format_pilot_run_body(&run));
    lines
}

fn format_pilot_run_body(run: &PilotRun) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("Goal: {}", run.goal)),
        Line::from(format!("Status: {}", pilot_status_label(run.status))),
        Line::from(format!("Started: {}", format_timestamp(run.started_at))),
        Line::from(format!("Iterations: {}", run.iteration_count)),
    ];
    if let Some(deadline_at) = run.deadline_at {
        lines.push(Line::from(format!(
            "Deadline: {}",
            format_timestamp(deadline_at)
        )));
    }
    if let Some(kind) = run.pending_cycle_kind {
        lines.push(Line::from(format!(
            "Queued cycle: {}",
            pilot_cycle_label(kind)
        )));
    }
    if let Some(kind) = run.active_cycle_kind {
        lines.push(Line::from(format!(
            "Active cycle: {}",
            pilot_cycle_label(kind)
        )));
    }
    if run.wrap_up_requested {
        lines.push(Line::from("Wrap-up requested."));
    }
    if let Some(summary) = run
        .last_cycle_summary
        .as_ref()
        .filter(|summary| !summary.is_empty())
    {
        lines.push(Line::from(format!("Last summary: {summary}")));
    }
    if let Some(message) = run
        .status_message
        .as_ref()
        .filter(|message| !message.is_empty())
    {
        lines.push(Line::from(format!("Status message: {message}")));
    }
    if let Some(message) = run
        .last_agent_message
        .as_ref()
        .filter(|message| !message.is_empty())
    {
        lines.push(Line::from(format!("Last agent message: {message}")));
    }
    if let Some(error) = run.last_error.as_ref().filter(|error| !error.is_empty()) {
        lines.push(Line::from(format!("Last error: {error}")));
    }
    lines
}

fn format_autoresearch_status(
    codex_home: &Path,
    thread_id: &str,
    fallback_workdir: &Path,
) -> Result<Vec<Line<'static>>, String> {
    let runtime = AutoresearchRuntime::load(codex_home, thread_id)
        .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
    let Some(state) = runtime.state() else {
        return Ok(vec![
            "Autoresearch".bold().into(),
            Line::from("No autoresearch session is active for this thread."),
        ]);
    };

    let journal_summary = load_autoresearch_summary(&state.workdir)
        .or_else(|| load_autoresearch_summary(fallback_workdir));

    let mut lines = vec![
        "Autoresearch".bold().into(),
        Line::from(format!("Goal: {}", state.goal)),
        Line::from(format!("Mode: {}", state.mode.cli_name())),
        Line::from(format!(
            "Status: {}",
            format_autoresearch_status_label(state.status)
        )),
        Line::from(format!("Workdir: {}", state.workdir.display())),
        Line::from(format!("Started: {}", format_timestamp(state.started_at))),
        Line::from(format!("Runs: {}", state.iteration_count)),
        Line::from(format!("Discovery passes: {}", state.discovery_count)),
    ];
    if let Some(max_runs) = state.max_runs {
        lines.push(Line::from(format!("Max runs: {max_runs}")));
    }
    if let Some(kind) = state.pending_cycle_kind {
        lines.push(Line::from(format!(
            "Queued cycle: {}",
            format_autoresearch_cycle(kind)
        )));
    }
    if let Some(kind) = state.active_cycle_kind {
        lines.push(Line::from(format!(
            "Active cycle: {}",
            format_autoresearch_cycle(kind)
        )));
    }
    if let Some(summary) = state
        .last_cycle_summary
        .as_ref()
        .filter(|summary| !summary.is_empty())
    {
        lines.push(Line::from(format!("Last summary: {summary}")));
    }
    if let Some(message) = state
        .status_message
        .as_ref()
        .filter(|message| !message.is_empty())
    {
        lines.push(Line::from(format!("Status message: {message}")));
    }
    if let Some(error) = state.last_error.as_ref().filter(|error| !error.is_empty()) {
        lines.push(Line::from(format!("Last error: {error}")));
    }
    if let Some(summary) = journal_summary {
        lines.push(Line::from(format!(
            "Portfolio: {} approaches across {} families",
            summary.approach_count(),
            summary.family_count()
        )));
        lines.push(Line::from(format!("Kept runs: {}", summary.keep_count())));
        if let Some(best_metric) = summary.best_metric() {
            lines.push(Line::from(format!("Best metric: {best_metric:.4}")));
        }
    }
    Ok(lines)
}

fn format_autoresearch_portfolio(
    codex_home: &Path,
    thread_id: &str,
    fallback_workdir: &Path,
) -> Result<Vec<Line<'static>>, String> {
    let runtime = AutoresearchRuntime::load(codex_home, thread_id)
        .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
    let workdir = runtime
        .state()
        .map(|state| state.workdir.as_path())
        .unwrap_or(fallback_workdir);
    let Some(summary) = load_autoresearch_summary(workdir) else {
        return Ok(vec![
            "Autoresearch Portfolio".bold().into(),
            Line::from("No autoresearch journal has been created yet."),
        ]);
    };
    let mut lines = vec![
        "Autoresearch Portfolio".bold().into(),
        Line::from(format!("Approaches: {}", summary.approach_count())),
        Line::from(format!("Families: {}", summary.family_count())),
        Line::from(format!("Discovery passes: {}", summary.discovery_count())),
        Line::from(format!("Kept runs: {}", summary.keep_count())),
    ];
    if let Some(best_metric) = summary.best_metric() {
        lines.push(Line::from(format!("Best metric: {best_metric:.4}")));
    }
    for approach in summary.current_segment_approaches.iter().take(5) {
        let mut line = format!(
            "{} [{}] runs={} keep={}",
            approach.latest.approach_id,
            approach.latest.family,
            approach.total_runs,
            approach.keep_count,
        );
        if let Some(best_metric) = approach.best_metric {
            line.push_str(&format!(" best={best_metric:.4}"));
        }
        lines.push(Line::from(line));
    }
    Ok(lines)
}

fn format_automation_list(mut automations: Vec<Automation>) -> Vec<Line<'static>> {
    automations.sort_by(|left, right| left.runtime_id.cmp(&right.runtime_id));
    if automations.is_empty() {
        return vec![
            "Automation".bold().into(),
            Line::from("No automations are configured for this thread."),
        ];
    }

    let mut lines = vec!["Automation".bold().into()];
    for automation in automations {
        lines.push(Line::from(format!(
            "{} · {} · {}",
            automation.runtime_id,
            automation_status_label(&automation),
            automation_trigger_label(&automation.trigger),
        )));
    }
    lines
}

fn format_automation_details(automation: Automation) -> Vec<Line<'static>> {
    let mut lines = vec![
        "Automation".bold().into(),
        Line::from(format!("Runtime id: {}", automation.runtime_id)),
        Line::from(format!("Automation id: {}", automation.id)),
        Line::from(format!(
            "Scope: {}",
            automation_scope_label(automation.scope)
        )),
        Line::from(format!("Status: {}", automation_status_label(&automation))),
        Line::from(format!(
            "Trigger: {}",
            automation_trigger_label(&automation.trigger)
        )),
        Line::from(format!(
            "Message: {}",
            automation_message_source_label(&automation.message_source)
        )),
        Line::from(format!("Runs: {}", automation.run_count)),
    ];
    if let Some(next_fire_at) = automation.next_fire_at {
        lines.push(Line::from(format!(
            "Next fire: {}",
            format_timestamp(next_fire_at)
        )));
    }
    if let Some(max_runs) = automation.limits.max_runs {
        lines.push(Line::from(format!("Max runs: {max_runs}")));
    }
    if let Some(until_at) = automation.limits.until_at {
        lines.push(Line::from(format!("Until: {}", format_timestamp(until_at))));
    }
    if let Some(command) = automation.policy_command.as_ref() {
        lines.push(Line::from(format!("Policy: {}", command.command.join(" "))));
    }
    if let Some(error) = automation
        .last_error
        .as_ref()
        .filter(|error| !error.is_empty())
    {
        lines.push(Line::from(format!("Last error: {error}")));
    }
    lines
}

fn load_autoresearch_summary(
    workdir: &Path,
) -> Option<codex_core::slop_fork::autoresearch::AutoresearchJournalSummary> {
    AutoresearchJournal::load(workdir)
        .ok()
        .map(|journal| journal.summary())
}

fn core_autoresearch_mode_to_api(mode: CoreAutoresearchMode) -> ApiAutoresearchMode {
    match mode {
        CoreAutoresearchMode::Optimize => ApiAutoresearchMode::Optimize,
        CoreAutoresearchMode::Research => ApiAutoresearchMode::Research,
        CoreAutoresearchMode::Scientist => ApiAutoresearchMode::Scientist,
    }
}

fn core_automation_scope_to_api(scope: CoreAutomationScope) -> AutomationScope {
    match scope {
        CoreAutomationScope::Session => AutomationScope::Session,
        CoreAutomationScope::Repo => AutomationScope::Repo,
        CoreAutomationScope::Global => AutomationScope::Global,
    }
}

fn automation_spec_to_api(spec: AutomationSpec) -> AutomationDefinition {
    AutomationDefinition {
        id: (!spec.id.is_empty()).then_some(spec.id),
        enabled: spec.enabled,
        trigger: automation_trigger_to_api(spec.trigger),
        message_source: automation_message_source_to_api(spec.message_source),
        limits: AutomationLimits {
            max_runs: spec.limits.max_runs,
            until_at: spec.limits.until_at,
        },
        policy_command: spec.policy_command.map(|command| AutomationPolicyCommand {
            command: command.command,
            cwd: command.cwd,
            timeout_ms: command.timeout_ms,
        }),
    }
}

fn automation_trigger_to_api(trigger: CoreAutomationTrigger) -> AutomationTrigger {
    match trigger {
        CoreAutomationTrigger::TurnCompleted => AutomationTrigger::TurnCompleted,
        CoreAutomationTrigger::Interval { every_seconds } => {
            AutomationTrigger::Interval { every_seconds }
        }
        CoreAutomationTrigger::Cron { expression } => AutomationTrigger::Cron { expression },
    }
}

fn automation_message_source_to_api(
    source: CoreAutomationMessageSource,
) -> AutomationMessageSource {
    match source {
        CoreAutomationMessageSource::Static { message } => {
            AutomationMessageSource::Static { message }
        }
        CoreAutomationMessageSource::RoundRobin { messages } => {
            AutomationMessageSource::RoundRobin { messages }
        }
    }
}

fn automation_submit_message(spec: &AutomationSpec) -> Option<String> {
    match &spec.message_source {
        CoreAutomationMessageSource::Static { message } => Some(message.clone()),
        CoreAutomationMessageSource::RoundRobin { messages } => messages.first().cloned(),
    }
}

fn automation_status_label(automation: &Automation) -> &'static str {
    if automation.stopped {
        "stopped"
    } else if automation.paused {
        "paused"
    } else if automation.enabled {
        "enabled"
    } else {
        "disabled"
    }
}

fn automation_scope_label(scope: AutomationScope) -> &'static str {
    match scope {
        AutomationScope::Session => "session",
        AutomationScope::Repo => "repo",
        AutomationScope::Global => "global",
    }
}

fn automation_trigger_label(trigger: &AutomationTrigger) -> String {
    match trigger {
        AutomationTrigger::TurnCompleted => "on-complete".to_string(),
        AutomationTrigger::Interval { every_seconds } => format!("every {every_seconds}s"),
        AutomationTrigger::Cron { expression } => format!("cron {expression}"),
    }
}

fn automation_message_source_label(message_source: &AutomationMessageSource) -> String {
    match message_source {
        AutomationMessageSource::Static { message } => message.clone(),
        AutomationMessageSource::RoundRobin { messages } => messages.join(" | "),
    }
}

fn pilot_status_label(status: PilotStatus) -> &'static str {
    match status {
        PilotStatus::Running => "running",
        PilotStatus::Paused => "paused",
        PilotStatus::Stopped => "stopped",
        PilotStatus::Completed => "completed",
    }
}

fn pilot_cycle_label(kind: PilotCycleKind) -> &'static str {
    match kind {
        PilotCycleKind::Continue => "continue",
        PilotCycleKind::WrapUp => "wrap-up",
    }
}

fn format_autoresearch_status_label(
    status: codex_core::slop_fork::autoresearch::AutoresearchStatus,
) -> &'static str {
    match status {
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Running => "running",
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Paused => "paused",
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Stopped => "stopped",
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Completed => "completed",
    }
}

fn format_autoresearch_cycle(
    kind: codex_core::slop_fork::autoresearch::AutoresearchCycleKind,
) -> &'static str {
    match kind {
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Continue => "continue",
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Research => "research",
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Discovery => "discovery",
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::WrapUp => "wrap-up",
    }
}

fn format_timestamp(timestamp: i64) -> String {
    DateTime::<Utc>::from_timestamp(timestamp, 0)
        .map(|dt| format_datetime(dt.with_timezone(&Local)))
        .unwrap_or_else(|| format!("unix:{timestamp}"))
}

fn format_datetime(datetime: DateTime<Local>) -> String {
    datetime.format("%Y-%m-%d %H:%M").to_string()
}
