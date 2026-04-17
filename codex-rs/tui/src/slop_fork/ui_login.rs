use super::ui_rate_limits::SavedAccountRateLimitsRefreshRenderable;
use super::ui_rate_limits::auth_dot_json_is_chatgpt;
use super::ui_rate_limits::saved_account_rate_limit_refresh_is_due;
use super::*;

#[derive(Debug)]
pub(crate) enum PendingChatgptLogin {
    Browser {
        auth_url: String,
        cancel_handle: ShutdownHandle,
        wait_handle: JoinHandle<()>,
    },
    DeviceCode {
        state: PendingDeviceCodeState,
        wait_handle: JoinHandle<()>,
    },
}

#[derive(Debug)]
pub(crate) enum PendingDeviceCodeState {
    Requesting,
    Ready {
        verification_url: String,
        user_code: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SavedAccountPopupMode {
    Use,
    RemoveAll,
    RemoveExpired,
}

impl SlopForkUi {
    #[cfg(test)]
    pub(crate) fn set_pending_chatgpt_login_for_test(
        &mut self,
        pending_login: PendingChatgptLogin,
    ) {
        self.finish_pending_chatgpt_login();
        self.pending_chatgpt_login = Some(pending_login);
    }

    #[cfg(test)]
    pub(crate) fn pending_chatgpt_login(&self) -> Option<&PendingChatgptLogin> {
        self.pending_chatgpt_login.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn open_login_root(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        self.open_login_popup(ctx, LoginPopupKind::Root)
    }

    pub(crate) fn open_login_popup(
        &mut self,
        ctx: &SlopForkUiContext,
        kind: LoginPopupKind,
    ) -> Vec<SlopForkUiEffect> {
        if matches!(kind, LoginPopupKind::Root) && self.pending_chatgpt_login.is_some() {
            return self.pending_login_popup_effects(ctx);
        }
        if !matches!(kind, LoginPopupKind::ConfirmRemoveSavedAccounts) {
            self.pending_saved_account_deletion = None;
        }

        self.active_login_popup_kind = Some(kind);
        match kind {
            LoginPopupKind::Root => self
                .login_root_popup_params(ctx)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::UseAccount => self
                .login_account_popup_params(ctx, SavedAccountPopupMode::Use)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::RemoveAccount => self
                .login_account_popup_params(ctx, SavedAccountPopupMode::RemoveAll)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::RemoveExpiredAccounts => self
                .login_account_popup_params(ctx, SavedAccountPopupMode::RemoveExpired)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::ConfirmRemoveSavedAccounts => self
                .saved_account_delete_confirmation_popup_params(ctx)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::RenameAccountFiles => self
                .login_account_rename_popup_params(ctx)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::AccountLimits => self
                .login_account_limits_popup_params(ctx)
                .map(Box::new)
                .map(SlopForkUiEffect::ShowOrReplaceSelection)
                .map(|effect| vec![effect])
                .unwrap_or_else(|err| {
                    vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to open accounts menu: {err}"
                    ))]
                }),
            LoginPopupKind::Settings => {
                let (
                    fork_config,
                    session_account_id,
                    shared_active_account_id,
                    accounts,
                    display_labels,
                    rename_suggestions,
                    rate_limit_snapshots,
                ) = match self.login_popup_state(ctx) {
                    Ok(state) => state,
                    Err(err) => {
                        return vec![SlopForkUiEffect::AddErrorMessage(format!(
                            "Failed to open accounts menu: {err}"
                        ))];
                    }
                };
                let session_account_label = self.session_login_account_label(
                    ctx,
                    session_account_id.as_deref(),
                    &accounts,
                    &display_labels,
                    &rate_limit_snapshots,
                );
                let shared_active_account_label = self.login_account_label(
                    shared_active_account_id.as_deref(),
                    &accounts,
                    &display_labels,
                    &rate_limit_snapshots,
                );
                let header = self.login_popup_header(
                    "Account Settings",
                    "Fork-specific account switching behavior.",
                    session_account_label,
                    shared_active_account_label,
                    &rename_suggestions,
                );
                vec![SlopForkUiEffect::ShowLoginView(Box::new(
                    LoginSettingsView::new(
                        LoginSettingsState {
                            auto_switch_accounts_on_rate_limit: fork_config
                                .auto_switch_accounts_on_rate_limit,
                            follow_external_account_switches: fork_config
                                .follow_external_account_switches,
                            api_key_fallback_on_all_accounts_limited: fork_config
                                .api_key_fallback_on_all_accounts_limited,
                            auto_start_five_hour_quota: fork_config.auto_start_five_hour_quota,
                            auto_start_weekly_quota: fork_config.auto_start_weekly_quota,
                            show_account_numbers_instead_of_emails: fork_config
                                .show_account_numbers_instead_of_emails,
                            show_average_account_limits_in_status_line: fork_config
                                .show_average_account_limits_in_status_line,
                        },
                        header,
                        ctx.app_event_tx.clone(),
                    ),
                ))]
            }
        }
    }

    pub(crate) fn open_login_api_key_prompt(
        &self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        let tx = ctx.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Enter API Key".to_string(),
            "Paste the API key and press Enter".to_string(),
            Some(
                "The key is stored as auth.json and also saved in ~/.codex/.accounts/.".to_string(),
            ),
            Box::new(move |api_key: String| {
                let api_key = api_key.trim().to_string();
                if api_key.is_empty() {
                    tx.send(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_error_event("API key cannot be empty.".to_string()),
                    )));
                    return;
                }
                tx.send(AppEvent::SlopFork(SlopForkEvent::SaveLoginApiKey {
                    api_key,
                }));
            }),
        );
        vec![SlopForkUiEffect::ShowLoginView(Box::new(view))]
    }

    pub(crate) fn start_login_flow(
        &mut self,
        ctx: &SlopForkUiContext,
        kind: LoginFlowKind,
    ) -> Vec<SlopForkUiEffect> {
        match kind {
            LoginFlowKind::Browser => self.start_browser_login(ctx),
            LoginFlowKind::DeviceCode => self.start_device_code_login(ctx),
        }
    }

    pub(crate) fn enter_pending_chatgpt_login(
        &mut self,
        ctx: &SlopForkUiContext,
        pending_login: PendingChatgptLogin,
    ) -> Vec<SlopForkUiEffect> {
        self.finish_pending_chatgpt_login();
        self.pending_chatgpt_login = Some(pending_login);
        self.pending_login_popup_effects(ctx)
    }

    pub(crate) fn cancel_pending_chatgpt_login(&mut self) -> Vec<SlopForkUiEffect> {
        let Some(pending_login) = self.pending_chatgpt_login.take() else {
            return Vec::new();
        };
        match pending_login {
            PendingChatgptLogin::Browser {
                cancel_handle,
                wait_handle,
                ..
            } => {
                cancel_handle.shutdown();
                wait_handle.abort();
            }
            PendingChatgptLogin::DeviceCode { wait_handle, .. } => {
                wait_handle.abort();
            }
        }
        self.active_login_popup_kind = None;
        vec![
            SlopForkUiEffect::DismissLoginView,
            SlopForkUiEffect::AddInfoMessage {
                message: "Cancelled ChatGPT login.".to_string(),
                hint: None,
            },
        ]
    }

    pub(crate) fn on_pending_device_code_login_ready(
        &mut self,
        ctx: &SlopForkUiContext,
        verification_url: String,
        user_code: String,
    ) -> Vec<SlopForkUiEffect> {
        let Some(PendingChatgptLogin::DeviceCode { state, .. }) =
            self.pending_chatgpt_login.as_mut()
        else {
            return Vec::new();
        };
        *state = PendingDeviceCodeState::Ready {
            verification_url,
            user_code,
        };
        self.pending_login_popup_effects(ctx)
    }

    pub(crate) fn on_auth_state_changed(&mut self) -> Vec<SlopForkUiEffect> {
        self.finish_pending_chatgpt_login();
        vec![SlopForkUiEffect::DismissLoginView]
    }

    pub(crate) fn activate_saved_account(
        &mut self,
        ctx: &SlopForkUiContext,
        account_id: &str,
    ) -> Vec<SlopForkUiEffect> {
        let display_labels = auth_accounts::load_account_display_labels(&ctx.codex_home);
        match ctx.auth_manager.activate_saved_account(account_id) {
            Ok(true) => {
                self.active_login_popup_kind = None;
                let label = auth_accounts::find_account(&ctx.codex_home, account_id)
                    .ok()
                    .flatten()
                    .map(|account| display_labels.label_for_account(&account))
                    .unwrap_or_else(|| account_id.to_string());
                vec![SlopForkUiEffect::AuthStateChanged {
                    message: format!("Activated {label}."),
                    is_error: false,
                    is_warning: false,
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "No saved account named {account_id}."
            ))],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to activate account: {err}"
            ))],
        }
    }

    pub(crate) fn remove_saved_account(
        &mut self,
        ctx: &SlopForkUiContext,
        account_id: &str,
    ) -> Vec<SlopForkUiEffect> {
        self.pending_saved_account_deletion = None;
        let display_labels = auth_accounts::load_account_display_labels(&ctx.codex_home);
        let label = auth_accounts::find_account(&ctx.codex_home, account_id)
            .ok()
            .flatten()
            .map(|account| display_labels.label_for_account(&account))
            .unwrap_or_else(|| account_id.to_string());
        match ctx.auth_manager.remove_saved_account(account_id) {
            Ok(true) => {
                self.active_login_popup_kind = None;
                vec![SlopForkUiEffect::AuthStateChanged {
                    message: format!("Removed saved account {label}."),
                    is_error: false,
                    is_warning: false,
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "No saved account named {account_id}."
            ))],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to remove account: {err}"
            ))],
        }
    }

    pub(crate) fn confirm_saved_account_deletion(
        &mut self,
        ctx: &SlopForkUiContext,
        request: SavedAccountDeletionRequest,
    ) -> Vec<SlopForkUiEffect> {
        self.pending_saved_account_deletion = Some(request);
        self.open_login_popup(ctx, LoginPopupKind::ConfirmRemoveSavedAccounts)
    }

    pub(crate) fn remove_saved_accounts(
        &mut self,
        ctx: &SlopForkUiContext,
        account_ids: &[String],
    ) -> Vec<SlopForkUiEffect> {
        self.pending_saved_account_deletion = None;
        let display_labels = auth_accounts::load_account_display_labels(&ctx.codex_home);
        let mut removed_labels = Vec::new();
        let mut missing_count = 0usize;
        for account_id in account_ids {
            let label = auth_accounts::find_account(&ctx.codex_home, account_id)
                .ok()
                .flatten()
                .map(|account| display_labels.label_for_account(&account))
                .unwrap_or_else(|| account_id.clone());
            match ctx.auth_manager.remove_saved_account(account_id) {
                Ok(true) => removed_labels.push(label),
                Ok(false) => missing_count += 1,
                Err(err) => {
                    return vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to remove account: {err}"
                    ))];
                }
            }
        }

        if removed_labels.is_empty() {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "No saved accounts matched the requested deletion.".to_string(),
            )];
        }

        self.active_login_popup_kind = None;
        let message = match removed_labels.as_slice() {
            [label] => format!("Removed saved account {label}."),
            _ => format!("Removed {} saved accounts.", removed_labels.len()),
        };
        let hint = (missing_count > 0).then_some(format!(
            "{missing_count} requested account(s) were already missing."
        ));
        vec![SlopForkUiEffect::AuthStateChanged {
            message: hint.map_or(message.clone(), |hint| format!("{message} {hint}")),
            is_error: false,
            is_warning: false,
        }]
    }

    pub(crate) fn rename_saved_account_file(
        &mut self,
        ctx: &SlopForkUiContext,
        path: &Path,
    ) -> Vec<SlopForkUiEffect> {
        match auth_accounts::rename_account_file(&ctx.codex_home, path) {
            Ok(true) => {
                self.active_login_popup_kind = None;
                let current_name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<invalid>");
                vec![SlopForkUiEffect::AuthStateChanged {
                    message: format!("Renamed ignored account file {current_name}."),
                    is_error: false,
                    is_warning: false,
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "That account file could not be renamed.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to rename account file: {err}"
            ))],
        }
    }

    pub(crate) fn rename_all_saved_account_files(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        match auth_accounts::rename_all_account_files(&ctx.codex_home) {
            Ok(result) => {
                if result.renamed_count == 0 {
                    return vec![SlopForkUiEffect::AddErrorMessage(
                        "There are no misnamed account files that can be renamed.".to_string(),
                    )];
                }

                self.active_login_popup_kind = None;
                let skipped_note = match result.skipped_existing_count {
                    0 => String::new(),
                    skipped_count => format!(
                        " {skipped_count} duplicate file(s) were left alone because the correctly named target already exists."
                    ),
                };
                vec![SlopForkUiEffect::AuthStateChanged {
                    message: format!(
                        "Renamed {} ignored account file(s).{skipped_note}",
                        result.renamed_count
                    ),
                    is_error: false,
                    is_warning: false,
                }]
            }
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to rename account files: {err}"
            ))],
        }
    }

    pub(crate) fn save_login_settings(
        &mut self,
        codex_home: &Path,
        settings: LoginSettingsState,
    ) -> Vec<SlopForkUiEffect> {
        match update_slop_fork_config(codex_home, |config| {
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
        }) {
            Ok(_) => {
                self.active_login_popup_kind = None;
                vec![SlopForkUiEffect::DismissLoginView]
            }
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to save fork settings: {err}"
            ))],
        }
    }

    pub(crate) fn save_login_api_key(
        &mut self,
        ctx: &SlopForkUiContext,
        api_key: String,
    ) -> Vec<SlopForkUiEffect> {
        match login_with_api_key(&ctx.codex_home, &api_key, ctx.auth_credentials_store_mode) {
            Ok(()) => {
                self.active_login_popup_kind = None;
                ctx.auth_manager.reload();
                vec![SlopForkUiEffect::AuthStateChanged {
                    message: "Saved API key and activated it.".to_string(),
                    is_error: false,
                    is_warning: false,
                }]
            }
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to save API key: {err}"
            ))],
        }
    }

    fn login_root_popup_params(
        &self,
        ctx: &SlopForkUiContext,
    ) -> std::io::Result<SelectionViewParams> {
        let (
            _fork_config,
            session_account_id,
            shared_active_account_id,
            accounts,
            display_labels,
            rename_suggestions,
            rate_limit_snapshots,
        ) = self.login_popup_state(ctx)?;
        let session_account_label = self.session_login_account_label(
            ctx,
            session_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let shared_active_account_label = self.login_account_label(
            shared_active_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let saved_accounts = accounts.len();
        let expired_count = accounts
            .iter()
            .filter(|account| {
                auth_accounts::saved_account_subscription_ran_out(
                    account,
                    rate_limit_snapshots.get(&account.id),
                )
            })
            .count();

        let mut items = vec![
            SelectionItem {
                name: "Login with ChatGPT".to_string(),
                description: Some("Start browser login and wait for the callback.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::StartLoginFlow {
                        kind: LoginFlowKind::Browser,
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Login with device code".to_string(),
                description: Some("Show a one-time code for browser sign-in.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::StartLoginFlow {
                        kind: LoginFlowKind::DeviceCode,
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Add API key".to_string(),
                description: Some("Enter an API key and make it the active account.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginApiKeyPrompt));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Switch account".to_string(),
                description: Some(format!("{saved_accounts} saved account(s) available.")),
                is_disabled: accounts.is_empty(),
                disabled_reason: accounts
                    .is_empty()
                    .then_some("Save another account first.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                        kind: LoginPopupKind::UseAccount,
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Remove account".to_string(),
                description: Some(match expired_count {
                    0 => "Delete a saved auth blob from ~/.codex/.accounts/.".to_string(),
                    1 => "Delete a saved auth blob from ~/.codex/.accounts/. 1 saved account subscription ran out."
                        .to_string(),
                    count => format!(
                        "Delete saved auth blobs from ~/.codex/.accounts/. {count} saved account subscriptions ran out."
                    ),
                }),
                is_disabled: accounts.is_empty(),
                disabled_reason: accounts
                    .is_empty()
                    .then_some("There are no saved accounts to remove.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                        kind: LoginPopupKind::RemoveAccount,
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: VIEW_ACCOUNT_LIMITS_ITEM_NAME.to_string(),
                description: Some(
                    "View the latest saved-account usage and reset times.".to_string(),
                ),
                is_disabled: accounts.is_empty(),
                disabled_reason: accounts
                    .is_empty()
                    .then_some("There are no saved accounts yet.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                        kind: LoginPopupKind::AccountLimits,
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Settings".to_string(),
                description: Some("Change account settings.".to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                        kind: LoginPopupKind::Settings,
                    }));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
        ];
        if expired_count > 0 {
            items.insert(
                6,
                SelectionItem {
                    name: "Delete expired accounts".to_string(),
                    description: Some(match expired_count {
                        1 => {
                            "Delete the 1 saved account whose subscription ran out. This cannot be undone."
                                .to_string()
                        }
                        count => format!(
                            "Delete the {count} saved accounts whose subscriptions ran out. This cannot be undone."
                        ),
                    }),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                            kind: LoginPopupKind::RemoveExpiredAccounts,
                        }));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            );
        }

        if let Some(shared_active_account_id) = shared_active_account_id.as_deref()
            && session_account_id.as_deref() != Some(shared_active_account_id)
            && let Some(account) = accounts
                .iter()
                .find(|account| account.id == shared_active_account_id)
        {
            let account_id = account.id.clone();
            let label = self.saved_account_label(
                account,
                &display_labels,
                rate_limit_snapshots.get(&account.id),
            );
            items.insert(
                4,
                SelectionItem {
                    name: "Switch to shared active account".to_string(),
                    description: Some(format!(
                        "Another Codex instance activated {label}. Adopt it in this session."
                    )),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::ActivateSavedAccount {
                            account_id: account_id.clone(),
                        }));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            );
        }

        if !rename_suggestions.is_empty() {
            items.insert(
                5,
                SelectionItem {
                    name: "Rename account files".to_string(),
                    description: Some(format!(
                        "{} misnamed account file(s) are ignored until renamed.",
                        rename_suggestions.len()
                    )),
                    actions: vec![Box::new(|tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                            kind: LoginPopupKind::RenameAccountFiles,
                        }));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                },
            );
        }

        let due_count = accounts
            .iter()
            .filter(|account| {
                saved_account_rate_limit_refresh_is_due(
                    account,
                    rate_limit_snapshots.get(&account.id),
                    Utc::now(),
                )
            })
            .count();
        if let Some(item) = items
            .iter_mut()
            .find(|item| item.name == VIEW_ACCOUNT_LIMITS_ITEM_NAME)
            && due_count > 0
        {
            item.description = Some(format!(
                "View the latest saved-account usage and reset times. {due_count} refresh due."
            ));
        }

        Ok(SelectionViewParams {
            view_id: Some(LOGIN_POPUP_VIEW_ID),
            header: self.login_popup_header(
                "Accounts",
                "Choose an account, auth flow, or fork setting.",
                session_account_label,
                shared_active_account_label,
                &rename_suggestions,
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        })
    }

    fn login_account_popup_params(
        &self,
        ctx: &SlopForkUiContext,
        mode: SavedAccountPopupMode,
    ) -> std::io::Result<SelectionViewParams> {
        let (
            _,
            session_account_id,
            shared_active_account_id,
            accounts,
            display_labels,
            rename_suggestions,
            rate_limit_snapshots,
        ) = self.login_popup_state(ctx)?;
        let session_account_label = self.session_login_account_label(
            ctx,
            session_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let shared_active_account_label = self.login_account_label(
            shared_active_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let now = Utc::now();
        let (title, subtitle) = match mode {
            SavedAccountPopupMode::Use => (
                "Switch Account",
                "Choose which saved account should become ~/.codex/auth.json.",
            ),
            SavedAccountPopupMode::RemoveAll => (
                "Remove Account",
                "Choose which saved account to delete from ~/.codex/.accounts/. This cannot be undone.",
            ),
            SavedAccountPopupMode::RemoveExpired => (
                "Delete Expired Accounts",
                "Choose which expired saved account to delete from ~/.codex/.accounts/. This cannot be undone.",
            ),
        };
        let activate = mode == SavedAccountPopupMode::Use;

        let filtered_accounts = match mode {
            SavedAccountPopupMode::RemoveExpired => accounts
                .into_iter()
                .filter(|account| {
                    auth_accounts::saved_account_subscription_ran_out(
                        account,
                        rate_limit_snapshots.get(&account.id),
                    )
                })
                .collect::<Vec<_>>(),
            SavedAccountPopupMode::Use | SavedAccountPopupMode::RemoveAll => accounts,
        };

        let items = if filtered_accounts.is_empty() {
            vec![SelectionItem {
                name: if mode == SavedAccountPopupMode::RemoveExpired {
                    "No expired saved accounts".to_string()
                } else {
                    "No saved accounts".to_string()
                },
                description: Some(if mode == SavedAccountPopupMode::RemoveExpired {
                    "Refresh saved-account limits if you expected an expired account here."
                        .to_string()
                } else {
                    "Run a login flow first to create one.".to_string()
                }),
                is_disabled: true,
                ..Default::default()
            }]
        } else {
            let mut items = Vec::new();
            if mode == SavedAccountPopupMode::RemoveExpired && filtered_accounts.len() > 1 {
                let expired_account_ids = filtered_accounts
                    .iter()
                    .map(|account| account.id.clone())
                    .collect::<Vec<_>>();
                let count = expired_account_ids.len();
                items.push(SelectionItem {
                    name: "Delete all expired accounts".to_string(),
                    description: Some(format!(
                        "Delete all {count} expired saved accounts permanently."
                    )),
                    selected_description: Some(format!(
                        "Delete all {count} expired saved accounts permanently. Press enter again on the next screen if you really want to remove them."
                    )),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::ConfirmSavedAccountDeletion {
                            request: SavedAccountDeletionRequest {
                                account_ids: expired_account_ids.clone(),
                                return_kind: LoginPopupKind::RemoveExpiredAccounts,
                            },
                        }));
                    })],
                    dismiss_on_select: true,
                    search_value: Some("delete all expired accounts".to_string()),
                    ..Default::default()
                });
            }
            items.extend(filtered_accounts.into_iter().map(|account| {
                    let account_id = account.id.clone();
                    let stored_snapshot = rate_limit_snapshots.get(&account_id);
                    let account_label =
                        self.saved_account_label(&account, &display_labels, stored_snapshot);
                    let summary =
                        self.login_account_rate_limit_summary(&account, stored_snapshot, now);
                    let summary_spans = styled_saved_account_limit_summary(&summary);
                    let subscription_ran_out =
                        auth_accounts::saved_account_subscription_ran_out(&account, stored_snapshot);
                    let is_current = session_account_id.as_deref() == Some(account_id.as_str());
                    let description = if activate {
                        Some(summary.clone())
                    } else {
                        let delete_target = if subscription_ran_out {
                            format!("Delete {account_id} irrecoverably")
                        } else {
                            format!("Delete {account_id}")
                        };
                        Some(format!("{delete_target} · {summary}"))
                    };
                    let (selected_description, selected_description_spans) = if activate {
                        (
                            Some(format!("{summary}. Press enter to switch to this account.")),
                            append_summary_suffix_spans(
                                &summary_spans,
                                ". Press enter to switch to this account.",
                            ),
                        )
                    } else {
                        let selection_hint = if subscription_ran_out && is_current {
                            ". Subscription ran out. Press enter to delete this saved account permanently. If another saved account is available, Codex switches away first."
                        } else if subscription_ran_out {
                            ". Subscription ran out. Press enter to delete this saved account permanently."
                        } else if is_current {
                            ". This account is currently active. Press enter to delete it permanently. This cannot be undone."
                        } else {
                            ". Press enter to delete this saved account permanently. This cannot be undone."
                        };
                        (
                            Some(format!("{summary}{selection_hint}")),
                            append_summary_suffix_spans(&summary_spans, selection_hint),
                        )
                    };
                    let actions: Vec<SelectionAction> = if activate {
                        vec![Box::new({
                            let account_id = account_id.clone();
                            move |tx| {
                                tx.send(AppEvent::SlopFork(SlopForkEvent::ActivateSavedAccount {
                                    account_id: account_id.clone(),
                                }));
                            }
                        })]
                    } else if mode == SavedAccountPopupMode::RemoveExpired {
                        vec![Box::new({
                            let account_id = account_id.clone();
                            move |tx| {
                                tx.send(AppEvent::SlopFork(
                                    SlopForkEvent::ConfirmSavedAccountDeletion {
                                        request: SavedAccountDeletionRequest {
                                            account_ids: vec![account_id.clone()],
                                            return_kind: LoginPopupKind::RemoveExpiredAccounts,
                                        },
                                    },
                                ));
                            }
                        })]
                    } else {
                        vec![Box::new({
                            let account_id = account_id.clone();
                            move |tx| {
                                tx.send(AppEvent::SlopFork(SlopForkEvent::RemoveSavedAccount {
                                    account_id: account_id.clone(),
                                }));
                            }
                        })]
                    };
                    SelectionItem {
                        name: account_label.clone(),
                        description,
                        description_spans: activate.then_some(summary_spans).flatten(),
                        selected_description,
                        selected_description_spans,
                        is_current,
                        actions,
                        dismiss_on_select: true,
                        search_value: Some(format!("{account_id} {account_label} {summary}")),
                        ..Default::default()
                    }
                }));
            items
        };

        Ok(SelectionViewParams {
            view_id: Some(LOGIN_POPUP_VIEW_ID),
            header: self.login_popup_header(
                title,
                subtitle,
                session_account_label,
                shared_active_account_label,
                &rename_suggestions,
            ),
            footer_hint: Some(standard_popup_hint_line()),
            is_searchable: items.len() > 8,
            search_placeholder: Some("Filter accounts".to_string()),
            on_cancel: Some(Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                    kind: LoginPopupKind::Root,
                }));
            })),
            items,
            ..Default::default()
        })
    }

    fn saved_account_delete_confirmation_popup_params(
        &self,
        ctx: &SlopForkUiContext,
    ) -> std::io::Result<SelectionViewParams> {
        let Some(request) = self.pending_saved_account_deletion.as_ref() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "No saved-account deletion is pending.",
            ));
        };
        let (
            _,
            session_account_id,
            shared_active_account_id,
            accounts,
            display_labels,
            rename_suggestions,
            rate_limit_snapshots,
        ) = self.login_popup_state(ctx)?;
        let session_account_label = self.session_login_account_label(
            ctx,
            session_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let shared_active_account_label = self.login_account_label(
            shared_active_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let now = Utc::now();
        let targeted_accounts = request
            .account_ids
            .iter()
            .filter_map(|account_id| {
                accounts
                    .iter()
                    .find(|account| account.id == *account_id)
                    .map(|account| (account, rate_limit_snapshots.get(account_id)))
            })
            .collect::<Vec<_>>();
        let active_is_targeted = session_account_id
            .as_deref()
            .is_some_and(|account_id| request.account_ids.iter().any(|id| id == account_id));
        let count = request.account_ids.len();
        let subtitle = if count == 1 {
            "Delete this expired saved account from ~/.codex/.accounts/. This cannot be undone."
        } else {
            "Delete these expired saved accounts from ~/.codex/.accounts/. This cannot be undone."
        };

        let mut items = Vec::new();
        let confirm_name = if count == 1 {
            "Yes, delete permanently".to_string()
        } else {
            format!("Yes, delete all {count}")
        };
        let confirm_description = if active_is_targeted {
            "Delete the selected expired accounts permanently. If another saved account remains, Codex switches away first."
                .to_string()
        } else {
            "Delete the selected expired accounts permanently. This cannot be undone.".to_string()
        };
        items.push(SelectionItem {
            name: confirm_name,
            description: Some(confirm_description),
            actions: vec![Box::new({
                let account_ids = request.account_ids.clone();
                move |tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::RemoveSavedAccounts {
                        account_ids: account_ids.clone(),
                    }));
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "No, go back".to_string(),
            description: Some(
                "Return to the expired account list without deleting anything.".to_string(),
            ),
            actions: vec![Box::new({
                let return_kind = request.return_kind;
                move |tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                        kind: return_kind,
                    }));
                }
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
        if targeted_accounts.is_empty() {
            items.push(SelectionItem {
                name: "No saved accounts matched this request".to_string(),
                description: Some(
                    "The selected expired accounts are no longer present on disk.".to_string(),
                ),
                is_disabled: true,
                ..Default::default()
            });
        } else {
            items.extend(targeted_accounts.iter().map(|(account, snapshot)| {
                let label = self.saved_account_label(account, &display_labels, *snapshot);
                let summary = self.login_account_rate_limit_summary(account, *snapshot, now);
                SelectionItem {
                    name: label,
                    description: Some(summary),
                    is_disabled: true,
                    is_current: session_account_id.as_deref() == Some(account.id.as_str()),
                    ..Default::default()
                }
            }));
        }

        Ok(SelectionViewParams {
            view_id: Some(LOGIN_POPUP_VIEW_ID),
            header: self.login_popup_header(
                "Confirm Delete",
                subtitle,
                session_account_label,
                shared_active_account_label,
                &rename_suggestions,
            ),
            footer_hint: Some(standard_popup_hint_line()),
            on_cancel: Some(Box::new({
                let return_kind = request.return_kind;
                move |tx| {
                    tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                        kind: return_kind,
                    }));
                }
            })),
            items,
            ..Default::default()
        })
    }

    fn login_account_limits_popup_params(
        &self,
        ctx: &SlopForkUiContext,
    ) -> std::io::Result<SelectionViewParams> {
        let (
            _,
            session_account_id,
            shared_active_account_id,
            accounts,
            display_labels,
            rename_suggestions,
            rate_limit_snapshots,
        ) = self.login_popup_state(ctx)?;
        let refresh_state = self.saved_account_rate_limits_refresh.as_ref();
        let session_account_label = self.session_login_account_label(
            ctx,
            session_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let shared_active_account_label = self.login_account_label(
            shared_active_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let now = Utc::now();
        let refreshable_account_count = accounts
            .iter()
            .filter(|account| auth_dot_json_is_chatgpt(&account.auth))
            .count();
        let due_count = accounts
            .iter()
            .filter(|account| {
                saved_account_rate_limit_refresh_is_due(
                    account,
                    rate_limit_snapshots.get(&account.id),
                    now,
                )
            })
            .count();

        let mut items = if accounts.is_empty() {
            vec![SelectionItem {
                name: "No saved accounts".to_string(),
                description: Some("Run a login flow first to create one.".to_string()),
                is_disabled: true,
                ..Default::default()
            }]
        } else {
            accounts
                .into_iter()
                .map(|account| {
                    let account_id = account.id.clone();
                    let stored_snapshot = rate_limit_snapshots.get(&account.id);
                    let account_label =
                        self.saved_account_label(&account, &display_labels, stored_snapshot);
                    let summary =
                        self.login_account_rate_limit_summary(&account, stored_snapshot, now);
                    let summary_spans = styled_saved_account_limit_summary(&summary);
                    let account_is_due =
                        saved_account_rate_limit_refresh_is_due(&account, stored_snapshot, now);
                    let account_is_refreshing = refresh_state.is_some_and(|refresh_state| {
                        refresh_state.target.includes_account(&account.id, account_is_due)
                    });
                    let is_refresh_in_flight = refresh_state.is_some();
                    let (description, description_spans) = if account_is_refreshing {
                        (
                            format!("{} · Refreshing now.", summary.trim_end()),
                            append_summary_suffix_spans(&summary_spans, " · Refreshing now."),
                        )
                    } else if is_refresh_in_flight {
                        (
                            format!("{} · Refresh already running.", summary.trim_end()),
                            append_summary_suffix_spans(
                                &summary_spans,
                                " · Refresh already running.",
                            ),
                        )
                    } else {
                        (summary.clone(), summary_spans.clone())
                    };
                    let (selected_description, selected_description_spans) = if account_is_refreshing {
                        (
                            Some(
                            "Refreshing this saved account now. Wait for the current refresh to finish before retrying."
                                .to_string(),
                            ),
                            append_summary_suffix_spans(
                                &summary_spans,
                                ". Refreshing this saved account now. Wait for the current refresh to finish before retrying.",
                            ),
                        )
                    } else if is_refresh_in_flight {
                        (
                            Some(
                            "A saved-account refresh is already running. Wait for it to finish before starting another refresh."
                                .to_string(),
                            ),
                            append_summary_suffix_spans(
                                &summary_spans,
                                ". A saved-account refresh is already running. Wait for it to finish before starting another refresh.",
                            ),
                        )
                    } else {
                        (
                            Some(format!(
                                "{}. Press enter to refresh this account now.",
                                summary.trim_end()
                            )),
                            append_summary_suffix_spans(
                                &summary_spans,
                                ". Press enter to refresh this account now.",
                            ),
                        )
                    };
                    let actions = if is_refresh_in_flight {
                        vec![saved_account_refresh_busy_action()]
                    } else {
                        vec![Box::new({
                            let account_id_for_action = account_id.clone();
                            move |tx: &AppEventSender| {
                                tx.send(AppEvent::SlopFork(
                                    SlopForkEvent::RefreshSavedAccountRateLimit {
                                        account_id: account_id_for_action.clone(),
                                    },
                                ));
                            }
                        }) as SelectionAction]
                    };
                    SelectionItem {
                        name: account_label.clone(),
                        description: Some(description),
                        description_spans,
                        selected_description,
                        selected_description_spans,
                        is_current: session_account_id.as_deref() == Some(account.id.as_str()),
                        actions,
                        dismiss_on_select: false,
                        search_value: Some(format!("{account_id} {account_label}")),
                        ..Default::default()
                    }
                })
                .collect::<Vec<_>>()
        };

        items.push(SelectionItem {
            name: "Refresh due account limits".to_string(),
            description: Some(match (refresh_state, due_count) {
                (Some(refresh_state), _) => format!(
                    "{} Wait for it to finish before retrying.",
                    refresh_state.description(due_count)
                ),
                (None, 0) => "No saved ChatGPT account currently needs a refresh.".to_string(),
                (None, 1) => {
                    "Refresh 1 saved ChatGPT account whose limit snapshot is due now.".to_string()
                }
                (None, count) => format!(
                    "Refresh {count} saved ChatGPT accounts whose limit snapshots are due now."
                ),
            }),
            is_disabled: refresh_state.is_some() || due_count == 0,
            disabled_reason: if refresh_state.is_some() {
                Some("A background refresh is already running.".to_string())
            } else if due_count == 0 {
                Some("There are no due saved-account snapshots right now.".to_string())
            } else {
                None
            },
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(
                    SlopForkEvent::RefreshSavedAccountRateLimits,
                ));
            })],
            dismiss_on_select: false,
            ..Default::default()
        });
        items.push(SelectionItem {
            name: "Force refresh all accounts".to_string(),
            description: Some(match (refresh_state, refreshable_account_count) {
                (Some(refresh_state), _) => format!(
                    "{} Wait for it to finish before retrying.",
                    refresh_state.description(due_count)
                ),
                (None, 0) => "There are no saved ChatGPT accounts to refresh.".to_string(),
                (None, 1) => {
                    "Refresh the saved ChatGPT account limit snapshot now, even if it is not due yet."
                        .to_string()
                }
                (None, count) => format!(
                    "Refresh all {count} saved ChatGPT account limit snapshots now, even if they are not due yet."
                ),
            }),
            is_disabled: refresh_state.is_some() || refreshable_account_count == 0,
            disabled_reason: if refresh_state.is_some() {
                Some("A background refresh is already running.".to_string())
            } else if refreshable_account_count == 0 {
                Some("There are no saved ChatGPT accounts to refresh.".to_string())
            } else {
                None
            },
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(
                    SlopForkEvent::RefreshAllSavedAccountRateLimits,
                ));
            })],
            dismiss_on_select: false,
            ..Default::default()
        });

        Ok(SelectionViewParams {
            view_id: Some(LOGIN_POPUP_VIEW_ID),
            header: self.login_account_limits_popup_header(
                ctx,
                session_account_label,
                shared_active_account_label,
                &rename_suggestions,
                due_count,
            ),
            footer_hint: Some(standard_popup_hint_line()),
            is_searchable: items.len() > 10,
            search_placeholder: Some("Filter accounts".to_string()),
            on_cancel: Some(Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                    kind: LoginPopupKind::Root,
                }));
            })),
            items,
            ..Default::default()
        })
    }

    fn login_account_rename_popup_params(
        &self,
        ctx: &SlopForkUiContext,
    ) -> std::io::Result<SelectionViewParams> {
        let (
            _,
            session_account_id,
            shared_active_account_id,
            accounts,
            display_labels,
            rename_suggestions,
            rate_limit_snapshots,
        ) = self.login_popup_state(ctx)?;
        let session_account_label = self.session_login_account_label(
            ctx,
            session_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let shared_active_account_label = self.login_account_label(
            shared_active_account_id.as_deref(),
            &accounts,
            &display_labels,
            &rate_limit_snapshots,
        );
        let renameable_count = rename_suggestions
            .iter()
            .filter(|suggestion| !suggestion.target_exists)
            .count();
        let duplicate_count = rename_suggestions.len() - renameable_count;

        let items = if rename_suggestions.is_empty() {
            vec![SelectionItem {
                name: "No misnamed account files".to_string(),
                description: Some(
                    "Every saved account file already uses the expected name.".to_string(),
                ),
                is_disabled: true,
                ..Default::default()
            }]
        } else {
            let mut items = vec![SelectionItem {
                name: "Rename all usable files".to_string(),
                description: Some(match duplicate_count {
                    0 => format!("Rename all {renameable_count} usable misnamed account file(s)."),
                    _ => format!(
                        "Rename {renameable_count} usable file(s) now. {duplicate_count} duplicate file(s) will stay ignored."
                    ),
                }),
                is_disabled: renameable_count == 0,
                disabled_reason: (renameable_count == 0).then_some(
                    "All remaining misnamed files already have correctly named copies.".to_string(),
                ),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::SlopFork(
                        SlopForkEvent::RenameAllSavedAccountFiles,
                    ));
                })],
                dismiss_on_select: true,
                ..Default::default()
            }];
            items.extend(rename_suggestions.into_iter().map(|suggestion| {
                let path = suggestion.path.clone();
                let current_name = suggestion
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<invalid>")
                    .to_string();
                let suggested_name = suggestion
                    .suggested_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("<invalid>")
                    .to_string();
                let account_label = display_labels.label_for_auth(&suggestion.auth);
                let description = if suggestion.target_exists {
                    format!("{account_label}. Ignored duplicate; {suggested_name} already exists.")
                } else {
                    format!("{account_label}. Rename to {suggested_name} to make it usable.")
                };
                SelectionItem {
                    name: current_name,
                    description: Some(description),
                    is_disabled: suggestion.target_exists,
                    disabled_reason: suggestion
                        .target_exists
                        .then_some("The correctly named file already exists.".to_string()),
                    actions: vec![Box::new(move |tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::RenameSavedAccountFile {
                            path: path.clone(),
                        }));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            }));
            items
        };

        Ok(SelectionViewParams {
            view_id: Some(LOGIN_POPUP_VIEW_ID),
            header: self.login_popup_header(
                "Rename Account Files",
                "Misnamed saved auth files are ignored until they use the expected account id.",
                session_account_label,
                shared_active_account_label,
                &[],
            ),
            footer_hint: Some(standard_popup_hint_line()),
            items,
            ..Default::default()
        })
    }

    pub(super) fn login_popup_state(
        &self,
        ctx: &SlopForkUiContext,
    ) -> std::io::Result<LoginPopupState> {
        let fork_config = load_slop_fork_config(&ctx.codex_home)?;
        let session_account_id = ctx
            .auth_manager
            .auth_cached()
            .as_ref()
            .and_then(auth_accounts::stored_account_id_for_auth);
        let shared_active_account_id = auth_accounts::current_active_account_id(
            &ctx.codex_home,
            ctx.auth_credentials_store_mode,
        )?;
        let accounts = auth_accounts::list_accounts(&ctx.codex_home)?;
        let display_labels =
            auth_accounts::AccountDisplayLabels::from_config(&fork_config, &accounts);
        let rename_suggestions = auth_accounts::list_account_rename_suggestions(&ctx.codex_home)?;
        let rate_limit_snapshots =
            account_rate_limits::snapshot_map_for_accounts(&ctx.codex_home, &accounts)?;
        Ok((
            fork_config,
            session_account_id,
            shared_active_account_id,
            accounts,
            display_labels,
            rename_suggestions,
            rate_limit_snapshots,
        ))
    }

    fn login_popup_header(
        &self,
        title: &str,
        subtitle: &str,
        session_account_label: Option<String>,
        shared_active_account_label: Option<String>,
        rename_suggestions: &[auth_accounts::AccountRenameSuggestion],
    ) -> Box<dyn Renderable> {
        let mut header = ColumnRenderable::new();
        header.push(Line::from(title.to_string().bold()));
        header.push(Line::from(subtitle.to_string().dim()));
        let session_line = session_account_label
            .as_deref()
            .map(|label| format!("This session: {label}"))
            .unwrap_or_else(|| "This session: none".to_string());
        header.push(Line::from(session_line.dim()));
        if shared_active_account_label != session_account_label {
            let shared_line = shared_active_account_label
                .as_deref()
                .map(|label| format!("Shared active account: {label}"))
                .unwrap_or_else(|| "Shared active account: none".to_string());
            header.push(Line::from(shared_line.magenta()));
        }
        if !rename_suggestions.is_empty() {
            let blocked_count = rename_suggestions
                .iter()
                .filter(|suggestion| suggestion.target_exists)
                .count();
            let ready_count = rename_suggestions.len() - blocked_count;
            let message = match (ready_count, blocked_count) {
                (0, blocked) => {
                    format!(
                        "{blocked} ignored duplicate account file(s) already have a correctly named copy."
                    )
                }
                (ready, 0) => {
                    format!("{ready} misnamed account file(s) are ignored until renamed.")
                }
                (ready, blocked) => format!(
                    "{ready} misnamed account file(s) can be renamed; {blocked} duplicate file(s) are ignored."
                ),
            };
            header.push(Line::from(message.magenta()));
        }
        Box::new(header)
    }

    fn login_account_limits_popup_header(
        &self,
        ctx: &SlopForkUiContext,
        session_account_label: Option<String>,
        shared_active_account_label: Option<String>,
        rename_suggestions: &[auth_accounts::AccountRenameSuggestion],
        due_count: usize,
    ) -> Box<dyn Renderable> {
        let mut header = ColumnRenderable::new();
        header.push(Line::from("View Account Limits".bold()));
        header.push(Line::from(
            "Choose a saved account to review or refresh its latest usage and reset times.".dim(),
        ));
        let session_line = session_account_label
            .as_deref()
            .map(|label| format!("This session: {label}"))
            .unwrap_or_else(|| "This session: none".to_string());
        header.push(Line::from(session_line.dim()));
        if shared_active_account_label != session_account_label {
            let shared_line = shared_active_account_label
                .as_deref()
                .map(|label| format!("Shared active account: {label}"))
                .unwrap_or_else(|| "Shared active account: none".to_string());
            header.push(Line::from(shared_line.magenta()));
        }
        if let Some(refresh_state) = self.saved_account_rate_limits_refresh.as_ref() {
            header.push(SavedAccountRateLimitsRefreshRenderable::new(
                refresh_state.started_at,
                refresh_state.description(due_count),
                ctx.frame_requester.clone(),
                ctx.animations,
            ));
        }
        if !rename_suggestions.is_empty() {
            header.push(Line::from(
                format!(
                    "{} misnamed account file(s) are ignored until renamed in /accounts.",
                    rename_suggestions.len()
                )
                .magenta(),
            ));
        }
        Box::new(header)
    }

    fn login_account_label(
        &self,
        account_id: Option<&str>,
        accounts: &[auth_accounts::StoredAccount],
        display_labels: &auth_accounts::AccountDisplayLabels,
        rate_limit_snapshots: &HashMap<String, account_rate_limits::StoredRateLimitSnapshot>,
    ) -> Option<String> {
        account_id.and_then(|account_id| {
            accounts
                .iter()
                .find(|account| account.id == account_id)
                .map(|account| {
                    self.saved_account_label(
                        account,
                        display_labels,
                        rate_limit_snapshots.get(&account.id),
                    )
                })
        })
    }

    fn session_login_account_label(
        &self,
        ctx: &SlopForkUiContext,
        session_account_id: Option<&str>,
        accounts: &[auth_accounts::StoredAccount],
        display_labels: &auth_accounts::AccountDisplayLabels,
        rate_limit_snapshots: &HashMap<String, account_rate_limits::StoredRateLimitSnapshot>,
    ) -> Option<String> {
        self.login_account_label(
            session_account_id,
            accounts,
            display_labels,
            rate_limit_snapshots,
        )
        .or_else(|| {
            ctx.auth_manager
                .auth_cached()
                .as_ref()
                .map(|auth| display_labels.label_for_codex_auth(auth))
        })
    }

    fn saved_account_label(
        &self,
        account: &auth_accounts::StoredAccount,
        display_labels: &auth_accounts::AccountDisplayLabels,
        snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
    ) -> String {
        let label = display_labels.label_for_account(account);
        if auth_accounts::saved_account_subscription_ran_out(account, snapshot) {
            format!("{label} [subscription ran out]")
        } else {
            label
        }
    }

    fn login_account_rate_limit_summary(
        &self,
        account: &auth_accounts::StoredAccount,
        snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
        now: DateTime<Utc>,
    ) -> String {
        let mut parts = Vec::new();

        let subscription_ran_out =
            auth_accounts::saved_account_subscription_ran_out(account, snapshot);
        if subscription_ran_out {
            parts.push("subscription ran out".to_string());
            if snapshot.is_none_or(|snapshot| snapshot.snapshot.is_none()) {
                return parts.join(" · ");
            }
        }

        if let Some(snapshot) = snapshot {
            if let Some(rate_limit) = snapshot.snapshot.as_ref() {
                let observed_at = snapshot.observed_at.unwrap_or_else(Utc::now);
                let display = rate_limit_snapshot_display_for_limit(
                    rate_limit,
                    rate_limit
                        .limit_name
                        .clone()
                        .or_else(|| rate_limit.limit_id.clone())
                        .unwrap_or_else(|| "codex".to_string()),
                    observed_at.with_timezone(&Local),
                );
                let format_reset = |kind: account_rate_limits::QuotaWindowKind,
                                    seconds: Option<i64>| {
                    let state = account_rate_limits::quota_window_state(snapshot, kind, now);
                    let (reset_at, marker) = match state {
                        account_rate_limits::QuotaWindowState::ResetPassed => (
                            account_rate_limits::quota_window(snapshot, kind)
                                .limit_window_seconds
                                .filter(|seconds| *seconds > 0)
                                .map(|seconds| now + chrono::Duration::seconds(seconds)),
                            '~',
                        ),
                        _ => {
                            let untouched_reset_at =
                                account_rate_limits::quota_window_reset_at_if_untouched(
                                    snapshot, kind, now,
                                );
                            let is_untouched = untouched_reset_at.is_some();
                            (
                                untouched_reset_at.or_else(|| {
                                    seconds.and_then(|seconds| {
                                        DateTime::<Utc>::from_timestamp(seconds, 0)
                                    })
                                }),
                                if is_untouched { '~' } else { ' ' },
                            )
                        }
                    };
                    reset_at.map(|reset_at| {
                        let reset_at = reset_at.with_timezone(&Local);
                        format!(
                            "{} on {}{marker}",
                            reset_at.format("%H:%M"),
                            reset_at.format("%-d %b")
                        )
                    })
                };
                let mut format_window =
                    |kind: account_rate_limits::QuotaWindowKind,
                     label: String,
                     used_percent: f64,
                     resets_at: Option<i64>| {
                        let used_percent =
                            match account_rate_limits::quota_window_state(snapshot, kind, now) {
                                account_rate_limits::QuotaWindowState::ResetPassed => 0.0,
                                account_rate_limits::QuotaWindowState::Unknown
                                | account_rate_limits::QuotaWindowState::Untouched
                                | account_rate_limits::QuotaWindowState::Started => used_percent,
                            };
                        let reset = format_reset(kind, resets_at)
                            .map(|reset| format!(" until {reset}"))
                            .unwrap_or_default();
                        parts.push(format!("{label} {used_percent:>3.0}%{reset}"));
                    };
                if let Some(primary) = display.primary {
                    let label = primary
                        .window_minutes
                        .map(crate::chatwidget::get_limits_duration)
                        .unwrap_or_else(|| "5h".to_string());
                    format_window(
                        account_rate_limits::QuotaWindowKind::FiveHour,
                        label,
                        primary.used_percent,
                        rate_limit
                            .primary
                            .as_ref()
                            .and_then(|window| window.resets_at),
                    );
                }
                if let Some(secondary) = display.secondary {
                    let label = secondary
                        .window_minutes
                        .map(crate::chatwidget::get_limits_duration)
                        .unwrap_or_else(|| "weekly".to_string());
                    format_window(
                        account_rate_limits::QuotaWindowKind::Weekly,
                        label,
                        secondary.used_percent,
                        rate_limit
                            .secondary
                            .as_ref()
                            .and_then(|window| window.resets_at),
                    );
                }
            } else if let Some(reset_at) = account_rate_limits::snapshot_reset_at(snapshot)
                && reset_at > now
            {
                parts.push(format!(
                    "limited until {}",
                    reset_at.with_timezone(&Local).format("%H:%M on %-d %b")
                ));
            } else if auth_dot_json_is_chatgpt(&account.auth) {
                parts.push("No snapshot yet".to_string());
            }

            if auth_dot_json_is_chatgpt(&account.auth)
                && account_rate_limits::rate_limit_refresh_is_due(
                    Some(snapshot),
                    now,
                    account_rate_limits::rate_limit_refresh_stale_interval(),
                )
                && let Some(observed_at) = snapshot.observed_at
            {
                let age = now.signed_duration_since(observed_at);
                if age > chrono::Duration::zero() {
                    parts.push(format!("stale {}m old", age.num_minutes()));
                }
            }
        } else if auth_dot_json_is_chatgpt(&account.auth) {
            parts.push("No snapshot yet".to_string());
        }

        if parts.is_empty() {
            account.id.clone()
        } else {
            parts.join(" · ")
        }
    }

    fn start_browser_login(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let mut opts = ServerOptions::new(
            ctx.codex_home.clone(),
            CLIENT_ID.to_string(),
            ctx.forced_chatgpt_workspace_id.clone(),
            ctx.auth_credentials_store_mode,
        );
        opts.open_browser = false;

        match run_login_server(opts) {
            Ok(child) => {
                let auth_url = child.auth_url.clone();
                let cancel_handle = child.cancel_handle();
                let wait_handle = tokio::spawn({
                    let auth_manager = Arc::clone(&ctx.auth_manager);
                    let app_event_tx = ctx.app_event_tx.clone();
                    async move {
                        let event = match child.block_until_done().await {
                            Ok(()) => {
                                auth_manager.reload();
                                AppEvent::AuthStateChanged {
                                    message: "Successfully logged in with ChatGPT.".to_string(),
                                    is_error: false,
                                    is_warning: false,
                                }
                            }
                            Err(err) => AppEvent::AuthStateChanged {
                                message: format!("ChatGPT login failed: {err}"),
                                is_error: true,
                                is_warning: false,
                            },
                        };
                        app_event_tx.send(event);
                    }
                });
                self.enter_pending_chatgpt_login(
                    ctx,
                    PendingChatgptLogin::Browser {
                        auth_url,
                        cancel_handle,
                        wait_handle,
                    },
                )
            }
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to start ChatGPT login: {err}"
            ))],
        }
    }

    fn start_device_code_login(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let mut opts = ServerOptions::new(
            ctx.codex_home.clone(),
            CLIENT_ID.to_string(),
            ctx.forced_chatgpt_workspace_id.clone(),
            ctx.auth_credentials_store_mode,
        );
        opts.open_browser = false;

        let wait_handle = tokio::spawn({
            let auth_manager = Arc::clone(&ctx.auth_manager);
            let app_event_tx = ctx.app_event_tx.clone();
            async move {
                let event = match request_device_code(&opts).await {
                    Ok(device_code) => {
                        app_event_tx.send(AppEvent::SlopFork(
                            SlopForkEvent::PendingDeviceCodeLoginReady {
                                verification_url: device_code.verification_url.clone(),
                                user_code: device_code.user_code.clone(),
                            },
                        ));

                        match complete_device_code_login(opts, device_code).await {
                            Ok(()) => {
                                auth_manager.reload();
                                AppEvent::AuthStateChanged {
                                    message:
                                        "Successfully logged in with ChatGPT using device code."
                                            .to_string(),
                                    is_error: false,
                                    is_warning: false,
                                }
                            }
                            Err(err) => AppEvent::AuthStateChanged {
                                message: format!("Device-code login failed: {err}"),
                                is_error: true,
                                is_warning: false,
                            },
                        }
                    }
                    Err(err) => AppEvent::AuthStateChanged {
                        message: format!("Device-code login failed: {err}"),
                        is_error: true,
                        is_warning: false,
                    },
                };
                app_event_tx.send(event);
            }
        });
        self.enter_pending_chatgpt_login(
            ctx,
            PendingChatgptLogin::DeviceCode {
                state: PendingDeviceCodeState::Requesting,
                wait_handle,
            },
        )
    }

    fn finish_pending_chatgpt_login(&mut self) {
        if let Some(pending_login) = self.pending_chatgpt_login.take() {
            match pending_login {
                PendingChatgptLogin::Browser { wait_handle, .. }
                | PendingChatgptLogin::DeviceCode { wait_handle, .. } => {
                    wait_handle.abort();
                }
            }
        }
        self.active_login_popup_kind = None;
    }

    fn pending_login_popup_effects(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        self.active_login_popup_kind = None;
        let Some(params) = self.pending_login_popup_params(ctx) else {
            return Vec::new();
        };
        vec![SlopForkUiEffect::ShowOrReplaceSelection(Box::new(params))]
    }

    fn pending_login_popup_params(&self, ctx: &SlopForkUiContext) -> Option<SelectionViewParams> {
        let pending_login = self.pending_chatgpt_login.as_ref()?;
        let mut status = StatusIndicatorWidget::new(
            ctx.app_event_tx.clone(),
            ctx.frame_requester.clone(),
            ctx.animations,
        );
        status.set_interrupt_hint_visible(/*visible*/ false);

        let mut header = ColumnRenderable::new();
        let mut items = Vec::new();
        match pending_login {
            PendingChatgptLogin::Browser { auth_url, .. } => {
                status.update_header("Waiting for login".to_string());
                header.push(status);
                header.push(Line::from(
                    "Open the sign-in link and finish the browser flow.".dim(),
                ));
                header.push(
                    Paragraph::new(Line::from(auth_url.clone().cyan().underlined()))
                        .wrap(Wrap { trim: false }),
                );
                items.push(SelectionItem {
                    name: "Open sign-in page in browser".to_string(),
                    actions: vec![Box::new({
                        let auth_url = auth_url.clone();
                        move |tx| {
                            tx.send(AppEvent::OpenUrlInBrowser {
                                url: auth_url.clone(),
                            });
                        }
                    })],
                    dismiss_on_select: false,
                    ..Default::default()
                });
            }
            PendingChatgptLogin::DeviceCode { state, .. } => match state {
                PendingDeviceCodeState::Requesting => {
                    status.update_header("Preparing device code".to_string());
                    header.push(status);
                    header.push(Line::from(
                        "Requesting a one-time code from the server...".dim(),
                    ));
                }
                PendingDeviceCodeState::Ready {
                    verification_url,
                    user_code,
                } => {
                    status.update_header("Waiting for device login".to_string());
                    header.push(status);
                    header.push(Line::from(
                        "Open the device login page and enter this code.".dim(),
                    ));
                    header.push(
                        Paragraph::new(Line::from(verification_url.clone().cyan().underlined()))
                            .wrap(Wrap { trim: false }),
                    );
                    header.push(Line::from(vec![
                        "Code: ".into(),
                        user_code.clone().cyan().bold(),
                    ]));
                    items.push(SelectionItem {
                        name: "Open device login page in browser".to_string(),
                        actions: vec![Box::new({
                            let verification_url = verification_url.clone();
                            move |tx| {
                                tx.send(AppEvent::OpenUrlInBrowser {
                                    url: verification_url.clone(),
                                });
                            }
                        })],
                        dismiss_on_select: false,
                        ..Default::default()
                    });
                }
            },
        }
        items.push(SelectionItem {
            name: "Cancel login".to_string(),
            description: Some("Stop waiting and return to the composer.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::CancelPendingLogin));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });

        Some(SelectionViewParams {
            view_id: Some(LOGIN_POPUP_VIEW_ID),
            header: Box::new(header),
            footer_hint: Some(standard_popup_hint_line()),
            on_cancel: Some(Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::CancelPendingLogin));
            })),
            items,
            ..Default::default()
        })
    }
}

fn saved_account_refresh_busy_action() -> SelectionAction {
    Box::new(|tx| {
        tx.send(AppEvent::InsertHistoryCell(Box::new(
            history_cell::new_info_event(
                "Saved account limits are already refreshing.".to_string(),
                Some("Wait for the current refresh to finish before retrying.".to_string()),
            ),
        )));
    })
}

fn styled_saved_account_limit_summary(summary: &str) -> Option<Vec<Span<'static>>> {
    let mut spans = Vec::new();
    let mut has_highlighted_limit = false;

    for (index, segment) in summary.split(" · ").enumerate() {
        if index > 0 {
            spans.push(" · ".dim());
        }

        if let Some(percent_end) = segment.find('%') {
            let percent_start = segment[..percent_end]
                .rfind(|ch: char| !ch.is_ascii_digit())
                .map_or(0, |idx| idx + 1);
            let (prefix, highlighted_and_suffix) = segment.split_at(percent_start);
            let (highlighted, suffix) =
                highlighted_and_suffix.split_at(percent_end + 1 - percent_start);

            spans.push(prefix.to_string().dim());
            spans.push(highlighted.to_string().underlined());
            if !suffix.is_empty() {
                spans.push(suffix.to_string().dim());
            }
            has_highlighted_limit = true;
        } else {
            spans.push(segment.to_string().dim());
        }
    }

    has_highlighted_limit.then_some(spans)
}

fn append_summary_suffix_spans(
    summary_spans: &Option<Vec<Span<'static>>>,
    suffix: &str,
) -> Option<Vec<Span<'static>>> {
    summary_spans.as_ref().map(|summary_spans| {
        let mut spans = summary_spans.clone();
        if let Some(last) = spans.last_mut() {
            let trimmed = last.content.trim_end_matches(' ');
            if trimmed.len() != last.content.len() {
                *last = Span::styled(trimmed.to_string(), last.style);
            }
        }
        spans.push(suffix.to_string().dim());
        spans
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use codex_app_server_protocol::AuthMode;
    use codex_core::auth::AuthDotJson;
    use codex_core::slop_fork::account_rate_limits::StoredQuotaWindow;
    use codex_core::slop_fork::account_rate_limits::StoredRateLimitSnapshot;
    use codex_core::slop_fork::auth_accounts::StoredAccount;
    use codex_login::token_data::IdTokenInfo;
    use codex_login::token_data::TokenData;
    use codex_protocol::auth::KnownPlan;
    use codex_protocol::auth::PlanType;
    use codex_protocol::protocol::RateLimitSnapshot;
    use codex_protocol::protocol::RateLimitWindow;
    use std::path::PathBuf;

    #[test]
    fn expired_subscription_summary_is_rendered_first() {
        let ui = SlopForkUi::default();
        let now = Utc
            .with_ymd_and_hms(2026, 3, 19, 9, 0, 0)
            .single()
            .expect("valid timestamp");
        let mut id_token = IdTokenInfo::default();
        id_token.email = Some("expired@example.com".to_string());
        id_token.chatgpt_plan_type = Some(PlanType::Known(KnownPlan::Team));
        let account = StoredAccount {
            id: "acct-expired".to_string(),
            path: PathBuf::from("acct-expired.json"),
            auth: AuthDotJson {
                auth_mode: Some(AuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(TokenData {
                    id_token,
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    account_id: Some("acct-expired".to_string()),
                }),
                last_refresh: None,
                agent_identity: None,
            },
            modified_at: None,
        };
        let snapshot = StoredRateLimitSnapshot {
            account_id: "acct-expired".to_string(),
            plan: Some("free".to_string()),
            workspace_deactivated: false,
            snapshot: None,
            five_hour_window: StoredQuotaWindow::default(),
            weekly_window: StoredQuotaWindow::default(),
            observed_at: None,
            primary_next_reset_at: None,
            secondary_next_reset_at: None,
            last_refresh_attempt_at: None,
            last_usage_limit_hit_at: None,
        };

        assert_eq!(
            ui.login_account_rate_limit_summary(&account, Some(&snapshot), now),
            "subscription ran out"
        );
    }

    #[test]
    fn untouched_window_summary_uses_projected_reset_suffix() {
        let ui = SlopForkUi::default();
        let observed_at = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 4, 5)
            .single()
            .expect("valid timestamp");
        let now = observed_at + chrono::Duration::minutes(2);
        let mut id_token = IdTokenInfo::default();
        id_token.email = Some("limits@example.com".to_string());
        let account = StoredAccount {
            id: "acct-1".to_string(),
            path: PathBuf::from("acct-1.json"),
            auth: AuthDotJson {
                auth_mode: Some(AuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(TokenData {
                    id_token,
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    account_id: Some("acct-1".to_string()),
                }),
                last_refresh: None,
                agent_identity: None,
            },
            modified_at: None,
        };
        let snapshot = StoredRateLimitSnapshot {
            account_id: "acct-1".to_string(),
            plan: Some("pro".to_string()),
            workspace_deactivated: false,
            snapshot: Some(RateLimitSnapshot {
                limit_id: None,
                limit_name: None,
                primary: Some(RateLimitWindow {
                    used_percent: 0.0,
                    window_minutes: Some(300),
                    resets_at: Some((observed_at + chrono::Duration::minutes(30)).timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: None,
            }),
            five_hour_window: StoredQuotaWindow {
                used_percent: Some(0),
                limit_window_seconds: Some(5 * 60 * 60),
                reset_after_seconds: Some(5 * 60 * 60),
                reset_at: Some(observed_at + chrono::Duration::hours(5)),
                last_touch_attempt_at: None,
                last_touch_confirmed_at: None,
                last_touch_reset_at: None,
            },
            weekly_window: StoredQuotaWindow::default(),
            observed_at: Some(observed_at),
            primary_next_reset_at: None,
            secondary_next_reset_at: None,
            last_refresh_attempt_at: None,
            last_usage_limit_hit_at: None,
        };

        let summary = ui.login_account_rate_limit_summary(&account, Some(&snapshot), now);
        let expected_reset = format!(
            "{} on {}~",
            (now + chrono::Duration::hours(5))
                .with_timezone(&Local)
                .format("%H:%M"),
            (now + chrono::Duration::hours(5))
                .with_timezone(&Local)
                .format("%-d %b")
        );

        assert!(summary.contains(&format!("until {expected_reset}")));
        assert!(!summary.contains("until 04:34 on 2 Jan "));
    }

    #[test]
    fn reset_passed_summary_projects_fresh_window_and_zero_usage() {
        let ui = SlopForkUi::default();
        let observed_at = Utc
            .with_ymd_and_hms(2026, 3, 18, 13, 0, 0)
            .single()
            .expect("valid timestamp");
        let now = Utc
            .with_ymd_and_hms(2026, 3, 19, 9, 0, 0)
            .single()
            .expect("valid timestamp");
        let mut id_token = IdTokenInfo::default();
        id_token.email = Some("expired@example.com".to_string());
        let account = StoredAccount {
            id: "acct-expired".to_string(),
            path: PathBuf::from("acct-expired.json"),
            auth: AuthDotJson {
                auth_mode: Some(AuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(TokenData {
                    id_token,
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    account_id: Some("acct-expired".to_string()),
                }),
                last_refresh: None,
                agent_identity: None,
            },
            modified_at: None,
        };
        let expired_reset_at = observed_at + chrono::Duration::hours(5);
        let snapshot = StoredRateLimitSnapshot {
            account_id: "acct-expired".to_string(),
            plan: Some("team".to_string()),
            workspace_deactivated: false,
            snapshot: Some(RateLimitSnapshot {
                limit_id: None,
                limit_name: None,
                primary: Some(RateLimitWindow {
                    used_percent: 100.0,
                    window_minutes: Some(300),
                    resets_at: Some(expired_reset_at.timestamp()),
                }),
                secondary: None,
                credits: None,
                plan_type: None,
            }),
            five_hour_window: StoredQuotaWindow {
                used_percent: Some(100),
                limit_window_seconds: Some(5 * 60 * 60),
                reset_after_seconds: Some(0),
                reset_at: Some(expired_reset_at),
                last_touch_attempt_at: None,
                last_touch_confirmed_at: None,
                last_touch_reset_at: None,
            },
            weekly_window: StoredQuotaWindow::default(),
            observed_at: Some(observed_at),
            primary_next_reset_at: None,
            secondary_next_reset_at: None,
            last_refresh_attempt_at: None,
            last_usage_limit_hit_at: Some(observed_at),
        };

        let summary = ui.login_account_rate_limit_summary(&account, Some(&snapshot), now);
        let expected_reset = format!(
            "{} on {}~",
            (now + chrono::Duration::hours(5))
                .with_timezone(&Local)
                .format("%H:%M"),
            (now + chrono::Duration::hours(5))
                .with_timezone(&Local)
                .format("%-d %b")
        );
        let stale_reset = format!(
            "{} on {} ",
            expired_reset_at.with_timezone(&Local).format("%H:%M"),
            expired_reset_at.with_timezone(&Local).format("%-d %b")
        );

        assert!(summary.contains("5h   0%"));
        assert!(summary.contains(&format!("until {expected_reset}")));
        assert!(!summary.contains("100%"));
        assert!(!summary.contains(&format!("until {stale_reset}")));
    }

    #[test]
    fn stale_limit_hint_does_not_render_limited_until_past_reset() {
        let ui = SlopForkUi::default();
        let observed_at = Utc
            .with_ymd_and_hms(2026, 3, 18, 13, 0, 0)
            .single()
            .expect("valid timestamp");
        let now = Utc
            .with_ymd_and_hms(2026, 3, 19, 9, 0, 0)
            .single()
            .expect("valid timestamp");
        let mut id_token = IdTokenInfo::default();
        id_token.email = Some("hint@example.com".to_string());
        let account = StoredAccount {
            id: "acct-hint".to_string(),
            path: PathBuf::from("acct-hint.json"),
            auth: AuthDotJson {
                auth_mode: Some(AuthMode::Chatgpt),
                openai_api_key: None,
                tokens: Some(TokenData {
                    id_token,
                    access_token: "access".to_string(),
                    refresh_token: "refresh".to_string(),
                    account_id: Some("acct-hint".to_string()),
                }),
                last_refresh: None,
                agent_identity: None,
            },
            modified_at: None,
        };
        let snapshot = StoredRateLimitSnapshot {
            account_id: "acct-hint".to_string(),
            plan: Some("team".to_string()),
            workspace_deactivated: false,
            snapshot: None,
            five_hour_window: StoredQuotaWindow {
                used_percent: None,
                limit_window_seconds: None,
                reset_after_seconds: None,
                reset_at: Some(observed_at + chrono::Duration::hours(5)),
                last_touch_attempt_at: None,
                last_touch_confirmed_at: None,
                last_touch_reset_at: None,
            },
            weekly_window: StoredQuotaWindow::default(),
            observed_at: Some(observed_at),
            primary_next_reset_at: None,
            secondary_next_reset_at: None,
            last_refresh_attempt_at: None,
            last_usage_limit_hit_at: Some(observed_at),
        };

        let summary = ui.login_account_rate_limit_summary(&account, Some(&snapshot), now);

        assert_eq!(summary, "No snapshot yet · stale 1200m old");
    }
}
