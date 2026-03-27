use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Local;
use color_eyre::eyre::Result;
use tokio::sync::Notify;

use super::*;
use crate::local_chatgpt_auth::load_local_chatgpt_auth;
use crate::slop_fork::DeviceCodeLoginState;
use crate::slop_fork::SavedAccountLimitsOverview;
use crate::slop_fork::SavedAccountMenuMode;
use crate::slop_fork::SavedAccountRateLimitsRefreshCompletionSource;
use crate::slop_fork::SavedAccountRateLimitsRefreshTarget;
use crate::slop_fork::SlopForkCommandExecution;
use crate::slop_fork::SlopForkCommandHistory;
use crate::slop_fork::SlopForkEvent;
use crate::slop_fork::activate_saved_account;
use crate::slop_fork::auto_command;
use crate::slop_fork::autoresearch_command;
use crate::slop_fork::build_autoresearch_init_prompt;
use crate::slop_fork::execute_auto_command;
use crate::slop_fork::execute_autoresearch_command;
use crate::slop_fork::execute_pilot_command;
use crate::slop_fork::load_accounts_root_overview;
use crate::slop_fork::load_login_settings_state;
use crate::slop_fork::load_rename_accounts_popup;
use crate::slop_fork::load_saved_account_limits_overview;
use crate::slop_fork::load_saved_accounts_popup;
use crate::slop_fork::login_api_key;
use crate::slop_fork::login_chatgpt_auth_tokens;
use crate::slop_fork::pilot_command;
use crate::slop_fork::refresh_saved_account_rate_limits_once;
use crate::slop_fork::remove_saved_account;
use crate::slop_fork::rename_all_saved_account_files;
use crate::slop_fork::rename_saved_account_file;
use crate::slop_fork::save_login_settings;
use crate::slop_fork::start_chatgpt_login;
use codex_core::auth::CLIENT_ID;
use codex_core::slop_fork::load_slop_fork_config;
use codex_login::ServerOptions;
use codex_login::complete_device_code_login;
use codex_login::request_device_code;

impl App {
    pub(super) async fn handle_slop_fork_event(
        &mut self,
        app_server: &AppServerSession,
        event: SlopForkEvent,
    ) -> Result<()> {
        match event {
            SlopForkEvent::OpenAccountsRoot => self.open_slop_fork_accounts_root(),
            SlopForkEvent::OpenSavedAccounts { mode } => self.open_slop_fork_saved_accounts(mode),
            SlopForkEvent::OpenSavedAccountRenames => self.open_slop_fork_saved_account_renames(),
            SlopForkEvent::OpenSavedAccountLimits => self.open_slop_fork_saved_account_limits(),
            SlopForkEvent::OpenAccountsSettings => self.open_slop_fork_account_settings(),
            SlopForkEvent::OpenAccountsApiKeyPrompt => {
                self.chat_widget.show_slop_fork_api_key_prompt();
            }
            SlopForkEvent::StartChatgptLogin => {
                self.start_slop_fork_chatgpt_login(app_server).await;
            }
            SlopForkEvent::StartDeviceCodeLogin => {
                self.start_slop_fork_device_code_login(app_server).await;
            }
            SlopForkEvent::CancelPendingDeviceCodeLogin => {
                self.cancel_slop_fork_device_code_login();
            }
            SlopForkEvent::PendingDeviceCodeLoginReady {
                verification_url,
                user_code,
            } => {
                self.on_slop_fork_device_code_login_ready(verification_url, user_code);
            }
            SlopForkEvent::FinishDeviceCodeLogin { message, is_error } => {
                self.finish_slop_fork_device_code_login(message, is_error);
            }
            SlopForkEvent::SubmitApiKeyLogin { api_key } => {
                self.submit_slop_fork_api_key(app_server, api_key).await;
            }
            SlopForkEvent::ActivateSavedAccount { account_id } => {
                self.activate_slop_fork_saved_account(app_server, account_id)
                    .await;
            }
            SlopForkEvent::RenameAllSavedAccountFiles => {
                self.rename_all_slop_fork_saved_account_files();
            }
            SlopForkEvent::RenameSavedAccountFile { path } => {
                self.rename_slop_fork_saved_account_file(path);
            }
            SlopForkEvent::RemoveSavedAccount { account_id } => {
                self.remove_slop_fork_saved_account(app_server, account_id)
                    .await;
            }
            SlopForkEvent::SaveAccountsSettings { settings } => {
                self.save_slop_fork_account_settings(settings);
            }
            SlopForkEvent::RefreshSavedAccountRateLimits => {
                let overview = match self.load_slop_fork_saved_account_limits_overview() {
                    Ok(overview) => overview,
                    Err(err) => {
                        self.chat_widget.add_error_message(err);
                        return Ok(());
                    }
                };
                self.refresh_slop_fork_saved_account_limits(
                    SavedAccountRateLimitsRefreshTarget::Due,
                    overview,
                )
                .await;
            }
            SlopForkEvent::RefreshAllSavedAccountRateLimits => {
                let overview = match self.load_slop_fork_saved_account_limits_overview() {
                    Ok(overview) => overview,
                    Err(err) => {
                        self.chat_widget.add_error_message(err);
                        return Ok(());
                    }
                };
                let account_ids = overview
                    .entries
                    .iter()
                    .filter(|entry| entry.is_refreshable)
                    .map(|entry| entry.account_id.clone())
                    .collect::<HashSet<_>>();
                self.refresh_slop_fork_saved_account_limits(
                    SavedAccountRateLimitsRefreshTarget::AllAccounts(account_ids),
                    overview,
                )
                .await;
            }
            SlopForkEvent::RefreshSavedAccountRateLimit { account_id } => {
                let overview = match self.load_slop_fork_saved_account_limits_overview() {
                    Ok(overview) => overview,
                    Err(err) => {
                        self.chat_widget.add_error_message(err);
                        return Ok(());
                    }
                };
                self.refresh_slop_fork_saved_account_limits(
                    SavedAccountRateLimitsRefreshTarget::Accounts(
                        [account_id].into_iter().collect::<HashSet<_>>(),
                    ),
                    overview,
                )
                .await;
            }
            SlopForkEvent::SavedAccountRateLimitsRefreshCompleted {
                updated_account_ids,
                source,
            } => {
                self.on_slop_fork_saved_account_limits_refresh_completed(
                    updated_account_ids,
                    source,
                );
            }
            SlopForkEvent::ExecutePilot { args, history } => {
                self.execute_slop_fork_pilot(app_server, args, history)
                    .await;
            }
            SlopForkEvent::ExecuteAutoresearch { args, history } => {
                self.execute_slop_fork_autoresearch(app_server, args, history)
                    .await;
            }
            SlopForkEvent::ExecuteAuto { args, history } => {
                self.execute_slop_fork_auto(app_server, args, history).await;
            }
        }
        Ok(())
    }

    fn open_slop_fork_accounts_root(&mut self) {
        match load_accounts_root_overview(
            &self.config.codex_home,
            self.config.cli_auth_credentials_store_mode,
            self.chat_widget.status_account_display(),
        ) {
            Ok(overview) => self.chat_widget.show_slop_fork_accounts_root(overview),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn open_slop_fork_saved_accounts(&mut self, mode: SavedAccountMenuMode) {
        match load_saved_accounts_popup(
            &self.config.codex_home,
            self.config.cli_auth_credentials_store_mode,
            self.chat_widget.status_account_display(),
            mode,
        ) {
            Ok(overview) => self.chat_widget.show_slop_fork_saved_accounts(overview),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn open_slop_fork_saved_account_renames(&mut self) {
        match load_rename_accounts_popup(
            &self.config.codex_home,
            self.config.cli_auth_credentials_store_mode,
            self.chat_widget.status_account_display(),
        ) {
            Ok(overview) => self
                .chat_widget
                .show_slop_fork_saved_account_renames(overview),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn open_slop_fork_account_settings(&mut self) {
        let settings = match load_login_settings_state(&self.config.codex_home) {
            Ok(settings) => settings,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return;
            }
        };
        match crate::slop_fork::load_accounts_popup_context(
            &self.config.codex_home,
            self.config.cli_auth_credentials_store_mode,
            self.chat_widget.status_account_display(),
        ) {
            Ok(popup_context) => self
                .chat_widget
                .show_slop_fork_account_settings(settings, popup_context),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn open_slop_fork_saved_account_limits(&mut self) {
        match self.load_slop_fork_saved_account_limits_overview() {
            Ok(overview) => self
                .chat_widget
                .show_slop_fork_saved_account_limits(overview),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn load_slop_fork_saved_account_limits_overview(
        &self,
    ) -> Result<SavedAccountLimitsOverview, String> {
        load_saved_account_limits_overview(
            &self.config.codex_home,
            self.config.cli_auth_credentials_store_mode,
            self.chat_widget.status_account_display(),
        )
    }

    async fn refresh_slop_fork_saved_account_limits(
        &mut self,
        target: SavedAccountRateLimitsRefreshTarget,
        overview: SavedAccountLimitsOverview,
    ) {
        if !self
            .chat_widget
            .begin_slop_fork_saved_account_limits_refresh(target.clone())
        {
            self.chat_widget.add_info_message(
                "Saved account limits are already refreshing.".to_string(),
                Some("Wait for the current refresh to finish before retrying.".to_string()),
            );
            return;
        }

        self.chat_widget
            .refresh_visible_slop_fork_saved_account_limits(overview);

        let codex_home = self.config.codex_home.clone();
        let base_url = self.config.chatgpt_base_url.clone();
        let auth_credentials_store_mode = self.config.cli_auth_credentials_store_mode;
        let requested_account_ids = target.requested_account_ids();
        let app_event_tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let updated_account_ids = refresh_saved_account_rate_limits_once(
                codex_home,
                base_url,
                auth_credentials_store_mode,
                /*include_active*/ true,
                requested_account_ids,
            )
            .await;
            app_event_tx.send(AppEvent::SlopFork(
                SlopForkEvent::SavedAccountRateLimitsRefreshCompleted {
                    updated_account_ids,
                    source: SavedAccountRateLimitsRefreshCompletionSource::Requested,
                },
            ));
        });
    }

    fn on_slop_fork_saved_account_limits_refresh_completed(
        &mut self,
        updated_account_ids: Vec<String>,
        source: SavedAccountRateLimitsRefreshCompletionSource,
    ) {
        let had_manual_refresh = matches!(
            source,
            SavedAccountRateLimitsRefreshCompletionSource::Requested
        ) && self
            .chat_widget
            .take_slop_fork_saved_account_limits_refresh()
            .is_some();

        if had_manual_refresh {
            let refreshed_count = updated_account_ids.len();
            self.chat_widget.add_info_message(
                match refreshed_count {
                    0 => "No saved account limit snapshots were refreshed.".to_string(),
                    1 => "Refreshed 1 saved account limit snapshot.".to_string(),
                    count => format!("Refreshed {count} saved account limit snapshots."),
                },
                (refreshed_count == 0).then_some(
                    "The selected accounts may still be waiting for fresh usage data.".to_string(),
                ),
            );
        }

        self.refresh_status_line();
        match self.load_slop_fork_saved_account_limits_overview() {
            Ok(overview) => self
                .chat_widget
                .refresh_visible_slop_fork_saved_account_limits(overview),
            Err(err) if had_manual_refresh => self.chat_widget.add_error_message(err),
            Err(_) => {}
        }
    }

    async fn start_slop_fork_chatgpt_login(&mut self, app_server: &AppServerSession) {
        match start_chatgpt_login(app_server.request_handle()).await {
            Ok(auth_url) => {
                self.chat_widget.add_info_message(
                    "Continue the ChatGPT login in your browser.".to_string(),
                    Some(auth_url.clone()),
                );
                self.app_event_tx
                    .send(AppEvent::OpenUrlInBrowser { url: auth_url });
            }
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    async fn start_slop_fork_device_code_login(&mut self, app_server: &AppServerSession) {
        if self.pending_slop_fork_device_code_cancel.is_some() {
            self.chat_widget.add_info_message(
                "A ChatGPT device-code login is already in progress.".to_string(),
                Some("Finish or cancel the current login before starting another one.".to_string()),
            );
            return;
        }

        self.chat_widget
            .show_slop_fork_device_code_login(DeviceCodeLoginState::Requesting);

        let cancel = Arc::new(Notify::new());
        self.pending_slop_fork_device_code_cancel = Some(cancel.clone());

        let request_handle = app_server.request_handle();
        let app_event_tx = self.app_event_tx.clone();
        let codex_home = self.config.codex_home.clone();
        let forced_chatgpt_workspace_id = self.config.forced_chatgpt_workspace_id.clone();
        let auth_credentials_store_mode = self.config.cli_auth_credentials_store_mode;

        tokio::spawn(async move {
            let mut opts = ServerOptions::new(
                codex_home.clone(),
                CLIENT_ID.to_string(),
                forced_chatgpt_workspace_id.clone(),
                auth_credentials_store_mode,
            );
            opts.open_browser = false;

            let device_code = tokio::select! {
                _ = cancel.notified() => return,
                result = request_device_code(&opts) => match result {
                    Ok(device_code) => device_code,
                    Err(err) => {
                        app_event_tx.send(AppEvent::SlopFork(
                            SlopForkEvent::FinishDeviceCodeLogin {
                                message: format!("Device-code login failed: {err}"),
                                is_error: true,
                            },
                        ));
                        return;
                    }
                },
            };

            app_event_tx.send(AppEvent::SlopFork(
                SlopForkEvent::PendingDeviceCodeLoginReady {
                    verification_url: device_code.verification_url.clone(),
                    user_code: device_code.user_code.clone(),
                },
            ));

            tokio::select! {
                _ = cancel.notified() => {}
                result = complete_device_code_login(opts, device_code) => {
                    let event = match result {
                        Ok(()) => match load_local_chatgpt_auth(
                            &codex_home,
                            auth_credentials_store_mode,
                            forced_chatgpt_workspace_id.as_deref(),
                        ) {
                            Ok(local_auth) => match login_chatgpt_auth_tokens(
                                request_handle,
                                local_auth.access_token,
                                local_auth.chatgpt_account_id,
                                local_auth.chatgpt_plan_type,
                            )
                            .await
                            {
                                Ok(()) => SlopForkEvent::FinishDeviceCodeLogin {
                                    message: "Successfully logged in with ChatGPT using device code."
                                        .to_string(),
                                    is_error: false,
                                },
                                Err(err) => SlopForkEvent::FinishDeviceCodeLogin {
                                    message: format!("Device-code login failed: {err}"),
                                    is_error: true,
                                },
                            },
                            Err(err) => SlopForkEvent::FinishDeviceCodeLogin {
                                message: format!("Device-code login failed: {err}"),
                                is_error: true,
                            },
                        },
                        Err(err) => SlopForkEvent::FinishDeviceCodeLogin {
                            message: format!("Device-code login failed: {err}"),
                            is_error: true,
                        },
                    };
                    app_event_tx.send(AppEvent::SlopFork(event));
                }
            }
        });
    }

    fn cancel_slop_fork_device_code_login(&mut self) {
        if let Some(cancel) = self.pending_slop_fork_device_code_cancel.take() {
            cancel.notify_waiters();
            self.chat_widget.dismiss_slop_fork_device_code_login();
            self.chat_widget
                .add_info_message("Cancelled ChatGPT login.".to_string(), /*hint*/ None);
        }
    }

    fn on_slop_fork_device_code_login_ready(
        &mut self,
        verification_url: String,
        user_code: String,
    ) {
        if self.pending_slop_fork_device_code_cancel.is_none() {
            return;
        }

        self.chat_widget
            .show_slop_fork_device_code_login(DeviceCodeLoginState::Ready {
                verification_url,
                user_code,
            });
    }

    fn finish_slop_fork_device_code_login(&mut self, message: String, is_error: bool) {
        self.pending_slop_fork_device_code_cancel = None;
        self.chat_widget.dismiss_slop_fork_device_code_login();
        if is_error {
            self.chat_widget.add_error_message(message);
        } else {
            self.chat_widget.add_info_message(message, /*hint*/ None);
            self.refresh_status_line();
        }
    }

    async fn submit_slop_fork_api_key(&mut self, app_server: &AppServerSession, api_key: String) {
        match login_api_key(app_server.request_handle(), api_key).await {
            Ok(()) => self.chat_widget.add_info_message(
                "Saved API key and activated it.".to_string(),
                /*hint*/ None,
            ),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    async fn activate_slop_fork_saved_account(
        &mut self,
        app_server: &AppServerSession,
        account_id: String,
    ) {
        match activate_saved_account(app_server.request_handle(), account_id.clone()).await {
            Ok(true) => {
                self.chat_widget.add_info_message(
                    format!("Activated account {account_id}."),
                    /*hint*/ None,
                );
                self.refresh_status_line();
            }
            Ok(false) => self
                .chat_widget
                .add_error_message(format!("No saved account named {account_id}.")),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    async fn remove_slop_fork_saved_account(
        &mut self,
        app_server: &AppServerSession,
        account_id: String,
    ) {
        match remove_saved_account(app_server.request_handle(), account_id.clone()).await {
            Ok(true) => {
                self.chat_widget.add_info_message(
                    format!("Removed saved account {account_id}."),
                    /*hint*/ None,
                );
                self.refresh_status_line();
            }
            Ok(false) => self
                .chat_widget
                .add_error_message(format!("No saved account named {account_id}.")),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn rename_slop_fork_saved_account_file(&mut self, path: PathBuf) {
        match rename_saved_account_file(&self.config.codex_home, &path) {
            Ok(true) => {
                let current_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<invalid>");
                self.chat_widget.add_info_message(
                    format!("Renamed ignored account file {current_name}."),
                    /*hint*/ None,
                );
            }
            Ok(false) => self
                .chat_widget
                .add_error_message("That account file could not be renamed.".to_string()),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn rename_all_slop_fork_saved_account_files(&mut self) {
        match rename_all_saved_account_files(&self.config.codex_home) {
            Ok(result) if result.renamed_count == 0 => self.chat_widget.add_error_message(
                "There are no misnamed account files that can be renamed.".to_string(),
            ),
            Ok(result) => {
                let skipped_note = match result.skipped_existing_count {
                    0 => String::new(),
                    skipped_count => format!(
                        " {skipped_count} duplicate file(s) were left alone because the correctly named target already exists."
                    ),
                };
                self.chat_widget.add_info_message(
                    format!(
                        "Renamed {} ignored account file(s).{skipped_note}",
                        result.renamed_count
                    ),
                    /*hint*/ None,
                );
            }
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    fn save_slop_fork_account_settings(&mut self, settings: crate::slop_fork::LoginSettingsState) {
        match save_login_settings(&self.config.codex_home, settings) {
            Ok(()) => self.refresh_status_line(),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }

    async fn execute_slop_fork_pilot(
        &mut self,
        app_server: &AppServerSession,
        args: String,
        history: Option<SlopForkCommandHistory>,
    ) {
        let command = match pilot_command::parse_pilot_command(&args, Local::now()) {
            Ok(command) => command,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return;
            }
        };
        let thread_id = if matches!(command, pilot_command::PilotCommand::Help) {
            String::new()
        } else if let Some(thread_id) = self.require_slop_fork_thread("Pilot") {
            thread_id
        } else {
            return;
        };

        if let Some(history) = history {
            self.chat_widget.add_slop_fork_command_submission(history);
        }
        let result = execute_pilot_command(app_server.request_handle(), thread_id, command).await;
        self.apply_slop_fork_command_result(result);
    }

    async fn execute_slop_fork_autoresearch(
        &mut self,
        app_server: &AppServerSession,
        args: String,
        history: Option<SlopForkCommandHistory>,
    ) {
        let command = match autoresearch_command::parse_autoresearch_command(&args) {
            Ok(command) => command,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return;
            }
        };

        match command {
            autoresearch_command::AutoresearchCommand::Init { request, open_mode } => {
                if self.require_slop_fork_thread("Autoresearch").is_none() {
                    return;
                }
                if let Some(history) = history {
                    self.chat_widget.add_slop_fork_command_submission(history);
                }
                let hint = if open_mode {
                    "This turn will scaffold an evaluation-first workspace for research or scientist mode. Run $autoresearch start --mode research or --mode scientist once the setup looks right."
                } else {
                    "This turn will scaffold autoresearch.md, benchmark scripts, and metric policy. Run $autoresearch start once the setup looks right."
                };
                let message = if open_mode {
                    "Autoresearch open-ended setup requested."
                } else {
                    "Autoresearch setup requested."
                };
                self.chat_widget
                    .add_info_message(message.to_string(), Some(hint.to_string()));
                self.chat_widget.apply_slop_fork_command_execution(
                    SlopForkCommandExecution::new(Vec::new()).with_submit_message(Some(
                        build_autoresearch_init_prompt(&request, open_mode),
                    )),
                );
            }
            other => {
                let thread_id = if matches!(other, autoresearch_command::AutoresearchCommand::Help)
                {
                    String::new()
                } else if let Some(thread_id) = self.require_slop_fork_thread("Autoresearch") {
                    thread_id
                } else {
                    return;
                };
                if let Some(history) = history {
                    self.chat_widget.add_slop_fork_command_submission(history);
                }
                let result = execute_autoresearch_command(
                    app_server.request_handle(),
                    &self.config.codex_home,
                    thread_id,
                    self.config.cwd.as_ref(),
                    other,
                )
                .await;
                self.apply_slop_fork_command_result(result);
            }
        }
    }

    async fn execute_slop_fork_auto(
        &mut self,
        app_server: &AppServerSession,
        args: String,
        history: Option<SlopForkCommandHistory>,
    ) {
        let fork_config = match load_slop_fork_config(&self.config.codex_home) {
            Ok(config) => config,
            Err(err) => {
                self.chat_widget
                    .add_error_message(format!("Failed to load fork config: {err}"));
                return;
            }
        };
        let command = match auto_command::parse_auto_command(
            &args,
            fork_config.automation_default_scope,
            Local::now(),
            self.chat_widget.last_text_user_message(),
        ) {
            Ok(command) => command,
            Err(err) => {
                self.chat_widget.add_error_message(err);
                return;
            }
        };
        let thread_id = if matches!(command, auto_command::AutoCommand::Help) {
            String::new()
        } else if let Some(thread_id) = self.require_slop_fork_thread("Automation") {
            thread_id
        } else {
            return;
        };

        if let Some(history) = history {
            self.chat_widget.add_slop_fork_command_submission(history);
        }
        let result = execute_auto_command(app_server.request_handle(), thread_id, command).await;
        self.apply_slop_fork_command_result(result);
    }

    fn require_slop_fork_thread(&mut self, feature: &str) -> Option<String> {
        self.chat_widget
            .thread_id()
            .map(|thread_id| thread_id.to_string())
            .or_else(|| {
                self.chat_widget
                    .add_error_message(format!("{feature} requires an active session."));
                None
            })
    }

    fn apply_slop_fork_command_result(
        &mut self,
        result: std::result::Result<SlopForkCommandExecution, String>,
    ) {
        match result {
            Ok(execution) => self
                .chat_widget
                .apply_slop_fork_command_execution(execution),
            Err(err) => self.chat_widget.add_error_message(err),
        }
    }
}
