use ratatui::style::Stylize;
use ratatui::text::Line;

use crate::app_event::AppEvent;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::bottom_pane::popup_consts::MAX_POPUP_ROWS;
use crate::bottom_pane::popup_consts::standard_popup_hint_line;
use crate::render::renderable::ColumnRenderable;
use crate::render::renderable::Renderable;
use crate::slop_fork::account_limits::ACCOUNT_LIMITS_VIEW_ID;

use super::AccountsPopupContext;
use super::AccountsRootOverview;
use super::DeviceCodeLoginState;
use super::RenameAccountsPopupOverview;
use super::SavedAccountLimitsOverview;
use super::SavedAccountMenuMode;
use super::SavedAccountRateLimitsRefreshState;
use super::SavedAccountsPopupOverview;
use super::SlopForkEvent;

pub(crate) const ACCOUNTS_DEVICE_CODE_VIEW_ID: &str = "slop-fork-accounts-device-code";

pub(crate) fn accounts_root_view_params(overview: AccountsRootOverview) -> SelectionViewParams {
    let mut items = vec![
        SelectionItem {
            name: "Login with ChatGPT".to_string(),
            description: Some("Start browser login and wait for the callback.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::StartChatgptLogin));
            })],
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "Login with device code".to_string(),
            description: Some("Show a one-time code for browser sign-in.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::StartDeviceCodeLogin));
            })],
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "Add API key".to_string(),
            description: Some("Enter an API key and make it the active account.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::OpenAccountsApiKeyPrompt));
            })],
            dismiss_on_select: true,
            ..Default::default()
        },
        SelectionItem {
            name: "Switch account".to_string(),
            description: Some(format!(
                "{} saved account(s) available.",
                overview.saved_account_count
            )),
            is_disabled: overview.saved_account_count == 0,
            disabled_reason: (overview.saved_account_count == 0)
                .then_some("Save another account first.".to_string()),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::OpenSavedAccounts {
                    mode: SavedAccountMenuMode::Activate,
                }));
            })],
            dismiss_on_select: true,
            ..Default::default()
        },
    ];

    if let Some(shared_active_choice) = overview.shared_active_choice {
        items.push(SelectionItem {
            name: "Switch to shared active account".to_string(),
            description: Some(format!(
                "Adopt shared active account {}.",
                shared_active_choice.label
            )),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::ActivateSavedAccount {
                    account_id: shared_active_choice.account_id.clone(),
                }));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
    }

    items.push(SelectionItem {
        name: "Remove account".to_string(),
        description: Some("Delete a saved auth blob from ~/.codex/.accounts/.".to_string()),
        is_disabled: overview.saved_account_count == 0,
        disabled_reason: (overview.saved_account_count == 0)
            .then_some("There are no saved accounts to remove.".to_string()),
        actions: vec![Box::new(|tx| {
            tx.send(AppEvent::SlopFork(SlopForkEvent::OpenSavedAccounts {
                mode: SavedAccountMenuMode::Remove,
            }));
        })],
        dismiss_on_select: true,
        ..Default::default()
    });

    if overview.popup_context.rename_summary.total_count > 0 {
        items.push(SelectionItem {
            name: "Rename account files".to_string(),
            description: Some(format!(
                "{} misnamed file(s) are ignored until renamed.",
                overview.popup_context.rename_summary.total_count
            )),
            actions: vec![Box::new(|tx| {
                tx.send(AppEvent::SlopFork(SlopForkEvent::OpenSavedAccountRenames));
            })],
            dismiss_on_select: true,
            ..Default::default()
        });
    }

    items.push(SelectionItem {
        name: "View account limits".to_string(),
        description: Some(if overview.due_count == 0 {
            "View latest usage and reset times.".to_string()
        } else {
            format!(
                "View latest usage and reset times. {} refresh due.",
                overview.due_count
            )
        }),
        is_disabled: overview.saved_account_count == 0,
        disabled_reason: (overview.saved_account_count == 0)
            .then_some("There are no saved accounts yet.".to_string()),
        actions: vec![Box::new(|tx| {
            tx.send(AppEvent::SlopFork(SlopForkEvent::OpenSavedAccountLimits));
        })],
        dismiss_on_select: true,
        ..Default::default()
    });
    items.push(SelectionItem {
        name: "Settings".to_string(),
        description: Some("Change account settings.".to_string()),
        actions: vec![Box::new(|tx| {
            tx.send(AppEvent::SlopFork(SlopForkEvent::OpenAccountsSettings));
        })],
        dismiss_on_select: true,
        ..Default::default()
    });

    let hidden_count = items.len().saturating_sub(MAX_POPUP_ROWS);

    SelectionViewParams {
        view_id: Some("slop-fork-accounts-root"),
        header: accounts_popup_header(
            "Accounts",
            "Choose an account, auth flow, or fork setting.",
            &overview.popup_context,
        ),
        footer_note: (hidden_count > 0).then_some(Line::from(
            format!("{hidden_count} more option(s) below. Press ↓ to see them.").dim(),
        )),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        ..Default::default()
    }
}

pub(crate) fn saved_accounts_view_params(
    overview: SavedAccountsPopupOverview,
) -> SelectionViewParams {
    let title = match overview.mode {
        SavedAccountMenuMode::Activate => "Switch Account",
        SavedAccountMenuMode::Remove => "Remove Account",
    };
    let subtitle = match overview.mode {
        SavedAccountMenuMode::Activate => {
            "Choose which saved account should become ~/.codex/auth.json."
        }
        SavedAccountMenuMode::Remove => {
            "Choose which saved account to delete from ~/.codex/.accounts/."
        }
    };
    let empty_description = match overview.mode {
        SavedAccountMenuMode::Activate => "Run a login flow first to create one.".to_string(),
        SavedAccountMenuMode::Remove => "There are no saved accounts to remove.".to_string(),
    };
    let entry_count = overview.entries.len();
    let items = if overview.entries.is_empty() {
        vec![SelectionItem {
            name: "No saved accounts".to_string(),
            description: Some(empty_description),
            is_disabled: true,
            ..Default::default()
        }]
    } else {
        overview
            .entries
            .into_iter()
            .map(|entry| {
                let account_id = entry.account_id.clone();
                let description = match overview.mode {
                    SavedAccountMenuMode::Activate => entry.description.clone(),
                    SavedAccountMenuMode::Remove => {
                        format!("Delete {} · {}", entry.account_id, entry.description)
                    }
                };
                let selected_description = match overview.mode {
                    SavedAccountMenuMode::Activate => Some(format!(
                        "{}. Press enter to switch to this account.",
                        entry.description
                    )),
                    SavedAccountMenuMode::Remove if entry.is_current => Some(
                        "This account is currently active. Removing it also logs it out."
                            .to_string(),
                    ),
                    SavedAccountMenuMode::Remove => None,
                };
                let actions: Vec<SelectionAction> = match overview.mode {
                    SavedAccountMenuMode::Activate => vec![Box::new(move |tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::ActivateSavedAccount {
                            account_id: account_id.clone(),
                        }));
                    })],
                    SavedAccountMenuMode::Remove => vec![Box::new(move |tx| {
                        tx.send(AppEvent::SlopFork(SlopForkEvent::RemoveSavedAccount {
                            account_id: account_id.clone(),
                        }));
                    })],
                };

                SelectionItem {
                    name: entry.label.clone(),
                    description: Some(description.clone()),
                    selected_description,
                    is_current: entry.is_current,
                    actions,
                    dismiss_on_select: true,
                    search_value: Some(format!(
                        "{} {} {}",
                        entry.account_id, entry.label, description
                    )),
                    ..Default::default()
                }
            })
            .collect()
    };

    SelectionViewParams {
        header: accounts_popup_header(title, subtitle, &overview.popup_context),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        is_searchable: entry_count > 8,
        search_placeholder: Some("Filter accounts".to_string()),
        on_cancel: Some(Box::new(|tx| {
            tx.send(AppEvent::SlopFork(SlopForkEvent::OpenAccountsRoot));
        })),
        ..Default::default()
    }
}

pub(crate) fn rename_accounts_view_params(
    overview: RenameAccountsPopupOverview,
) -> SelectionViewParams {
    let items = if overview.entries.is_empty() {
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
            description: Some(match overview.blocked_count {
                0 => format!(
                    "Rename all {} usable misnamed account file(s).",
                    overview.renameable_count
                ),
                _ => format!(
                    "Rename {} usable file(s) now. {} duplicate file(s) will stay ignored.",
                    overview.renameable_count, overview.blocked_count
                ),
            }),
            is_disabled: overview.renameable_count == 0,
            disabled_reason: (overview.renameable_count == 0).then_some(
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
        items.extend(overview.entries.into_iter().map(|entry| {
            let path = entry.path.clone();
            let description = if entry.target_exists {
                format!(
                    "{}. Ignored duplicate; {} already exists.",
                    entry.account_label, entry.suggested_name
                )
            } else {
                format!(
                    "{}. Rename to {} to make it usable.",
                    entry.account_label, entry.suggested_name
                )
            };
            SelectionItem {
                name: entry.current_name,
                description: Some(description),
                is_disabled: entry.target_exists,
                disabled_reason: entry
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

    SelectionViewParams {
        header: accounts_popup_header(
            "Rename Account Files",
            "Misnamed saved auth files are ignored until they use the expected account id.",
            &overview.popup_context,
        ),
        footer_hint: Some(standard_popup_hint_line()),
        items,
        on_cancel: Some(Box::new(|tx| {
            tx.send(AppEvent::SlopFork(SlopForkEvent::OpenAccountsRoot));
        })),
        ..Default::default()
    }
}

pub(crate) fn account_settings_popup_header(
    popup_context: &AccountsPopupContext,
) -> Box<dyn Renderable> {
    accounts_popup_header(
        "Account Settings",
        "Fork-specific account switching behavior.",
        popup_context,
    )
}

pub(crate) fn device_code_login_view_params(state: DeviceCodeLoginState) -> SelectionViewParams {
    let mut header = ColumnRenderable::new();
    header.push(Line::from("ChatGPT Device Login".bold()));
    match &state {
        DeviceCodeLoginState::Requesting => {
            header.push(Line::from(
                "Requesting a one-time code from the server...".dim(),
            ));
        }
        DeviceCodeLoginState::Ready {
            verification_url,
            user_code,
        } => {
            header.push(Line::from(
                "Open the device login page and enter this code.".dim(),
            ));
            header.push(Line::from(verification_url.clone().cyan().underlined()));
            header.push(Line::from(vec![
                "Code: ".into(),
                user_code.clone().cyan().bold(),
            ]));
            header.push(Line::from(
                "Device codes are a common phishing target. Never share this code.".dim(),
            ));
        }
    }

    let mut items = Vec::new();
    if let DeviceCodeLoginState::Ready {
        verification_url,
        user_code: _,
    } = &state
    {
        let verification_url = verification_url.clone();
        items.push(SelectionItem {
            name: "Open device login page in browser".to_string(),
            actions: vec![Box::new(move |tx| {
                tx.send(AppEvent::OpenUrlInBrowser {
                    url: verification_url.clone(),
                });
            })],
            dismiss_on_select: false,
            ..Default::default()
        });
    }
    items.push(SelectionItem {
        name: "Cancel login".to_string(),
        description: Some("Stop waiting and return to the composer.".to_string()),
        actions: vec![Box::new(|tx| {
            tx.send(AppEvent::SlopFork(
                SlopForkEvent::CancelPendingDeviceCodeLogin,
            ));
        })],
        dismiss_on_select: true,
        ..Default::default()
    });

    SelectionViewParams {
        view_id: Some(ACCOUNTS_DEVICE_CODE_VIEW_ID),
        header: Box::new(header),
        footer_hint: Some(standard_popup_hint_line()),
        on_cancel: Some(Box::new(|tx| {
            tx.send(AppEvent::SlopFork(
                SlopForkEvent::CancelPendingDeviceCodeLogin,
            ));
        })),
        items,
        ..Default::default()
    }
}

pub(crate) fn saved_account_limits_selection_view_params(
    overview: &SavedAccountLimitsOverview,
    refresh_state: Option<&SavedAccountRateLimitsRefreshState>,
    initial_selected_idx: Option<usize>,
) -> SelectionViewParams {
    let item_count = overview.entries.len() + 2;
    SelectionViewParams {
        view_id: Some(ACCOUNT_LIMITS_VIEW_ID),
        header: account_limits_popup_header(
            &overview.popup_context,
            overview.due_count,
            refresh_state,
        ),
        footer_hint: Some(standard_popup_hint_line()),
        items: saved_account_limit_items(overview, refresh_state),
        is_searchable: item_count > 10,
        search_placeholder: Some("Filter accounts".to_string()),
        initial_selected_idx,
        on_cancel: Some(Box::new(|tx| {
            tx.send(AppEvent::SlopFork(SlopForkEvent::OpenAccountsRoot));
        })),
        ..Default::default()
    }
}

pub(crate) fn accounts_popup_header(
    title: &str,
    subtitle: &str,
    popup_context: &AccountsPopupContext,
) -> Box<dyn Renderable> {
    let mut header = ColumnRenderable::new();
    header.push(Line::from(title.to_string().bold()));
    header.push(Line::from(subtitle.to_string().dim()));
    let session_line = popup_context
        .session_account_label
        .as_deref()
        .map(|label| format!("This session: {label}"))
        .unwrap_or_else(|| "This session: none".to_string());
    header.push(Line::from(session_line.dim()));
    if popup_context.shared_active_account_label != popup_context.session_account_label {
        let shared_line = popup_context
            .shared_active_account_label
            .as_deref()
            .map(|label| format!("Shared active account: {label}"))
            .unwrap_or_else(|| "Shared active account: none".to_string());
        header.push(Line::from(shared_line.magenta()));
    }
    if popup_context.rename_summary.total_count > 0 {
        let rename_message = match (
            popup_context.rename_summary.renameable_count,
            popup_context.rename_summary.blocked_count,
        ) {
            (0, blocked_count) => format!(
                "{blocked_count} ignored duplicate account file(s) already have a correctly named copy."
            ),
            (renameable_count, 0) => {
                format!("{renameable_count} misnamed account file(s) are ignored until renamed.")
            }
            (renameable_count, blocked_count) => format!(
                "{renameable_count} misnamed account file(s) can be renamed; {blocked_count} duplicate file(s) are ignored."
            ),
        };
        header.push(Line::from(rename_message.magenta()));
    }
    Box::new(header)
}

fn account_limits_popup_header(
    popup_context: &AccountsPopupContext,
    due_count: usize,
    refresh_state: Option<&SavedAccountRateLimitsRefreshState>,
) -> Box<dyn Renderable> {
    let mut header = ColumnRenderable::new();
    header.push(Line::from("View Account Limits".bold()));
    header.push(Line::from(
        "Choose a saved account to review or refresh its latest usage and reset times.".dim(),
    ));
    let session_line = popup_context
        .session_account_label
        .as_deref()
        .map(|label| format!("This session: {label}"))
        .unwrap_or_else(|| "This session: none".to_string());
    header.push(Line::from(session_line.dim()));
    if popup_context.shared_active_account_label != popup_context.session_account_label {
        let shared_line = popup_context
            .shared_active_account_label
            .as_deref()
            .map(|label| format!("Shared active account: {label}"))
            .unwrap_or_else(|| "Shared active account: none".to_string());
        header.push(Line::from(shared_line.magenta()));
    }
    if let Some(refresh_state) = refresh_state {
        header.push(Line::from(refresh_state.description(due_count).dim()));
    }
    if popup_context.rename_summary.total_count > 0 {
        header.push(Line::from(
            format!(
                "{} misnamed account file(s) are ignored until renamed in /accounts.",
                popup_context.rename_summary.total_count
            )
            .magenta(),
        ));
    }
    Box::new(header)
}

fn saved_account_limit_items(
    overview: &SavedAccountLimitsOverview,
    refresh_state: Option<&SavedAccountRateLimitsRefreshState>,
) -> Vec<SelectionItem> {
    if overview.entries.is_empty() {
        return vec![SelectionItem {
            name: "No saved accounts".to_string(),
            description: Some("Run a login flow first to create one.".to_string()),
            is_disabled: true,
            actions: Vec::new(),
            ..Default::default()
        }];
    }

    let is_refresh_in_flight = refresh_state.is_some();
    let mut items = overview
        .entries
        .iter()
        .map(|entry| {
            let account_is_refreshing = refresh_state.is_some_and(|refresh_state| {
                refresh_state.includes_account(&entry.account_id, entry.is_due)
            });
            let description = if account_is_refreshing {
                format!("{} · Refreshing now.", entry.summary.trim_end())
            } else if !entry.is_refreshable {
                format!(
                    "{} · Limit snapshots are only available for saved ChatGPT accounts.",
                    entry.summary.trim_end()
                )
            } else if is_refresh_in_flight {
                format!("{} · Refresh already running.", entry.summary.trim_end())
            } else {
                entry.summary.clone()
            };
            let selected_description = if account_is_refreshing {
                Some(
                    "Refreshing this saved account now. Wait for the current refresh to finish before retrying."
                        .to_string(),
                )
            } else if !entry.is_refreshable {
                Some("Only saved ChatGPT accounts expose refreshable limit snapshots.".to_string())
            } else if is_refresh_in_flight {
                Some(
                    "A saved-account refresh is already running. Wait for it to finish before starting another refresh."
                        .to_string(),
                )
            } else {
                Some(format!(
                    "{}. Press enter to refresh this account now.",
                    entry.summary.trim_end()
                ))
            };
            let is_disabled = !entry.is_refreshable || is_refresh_in_flight;
            let disabled_reason = if !entry.is_refreshable {
                Some("Only saved ChatGPT accounts can refresh limit snapshots.".to_string())
            } else if is_refresh_in_flight {
                Some("A saved-account refresh is already running.".to_string())
            } else {
                None
            };
            let actions = if is_disabled {
                Vec::new()
            } else {
                let account_id = entry.account_id.clone();
                vec![Box::new(move |tx: &crate::app_event_sender::AppEventSender| {
                    tx.send(AppEvent::SlopFork(
                        SlopForkEvent::RefreshSavedAccountRateLimit {
                            account_id: account_id.clone(),
                        },
                    ));
                }) as SelectionAction]
            };

            SelectionItem {
                name: entry.label.clone(),
                description: Some(description.clone()),
                selected_description,
                is_current: entry.is_current,
                is_disabled,
                disabled_reason,
                actions,
                dismiss_on_select: false,
                search_value: Some(format!(
                    "{} {} {}",
                    entry.account_id, entry.label, description
                )),
                ..Default::default()
            }
        })
        .collect::<Vec<_>>();

    items.push(SelectionItem {
        name: "Refresh due account limits".to_string(),
        description: Some(match (refresh_state, overview.due_count) {
            (Some(refresh_state), _) => format!(
                "{} Wait for it to finish before retrying.",
                refresh_state.description(overview.due_count)
            ),
            (None, 0) => "No saved ChatGPT account currently needs a refresh.".to_string(),
            (None, 1) => {
                "Refresh 1 saved ChatGPT account whose limit snapshot is due now.".to_string()
            }
            (None, count) => {
                format!("Refresh {count} saved ChatGPT accounts whose limit snapshots are due now.")
            }
        }),
        is_disabled: is_refresh_in_flight || overview.due_count == 0,
        disabled_reason: if is_refresh_in_flight {
            Some("A background refresh is already running.".to_string())
        } else if overview.due_count == 0 {
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
        search_value: Some("refresh due account limits".to_string()),
        ..Default::default()
    });
    items.push(SelectionItem {
        name: "Force refresh all accounts".to_string(),
        description: Some(match (refresh_state, overview.refreshable_account_count) {
            (Some(refresh_state), _) => format!(
                "{} Wait for it to finish before retrying.",
                refresh_state.description(overview.due_count)
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
        is_disabled: is_refresh_in_flight || overview.refreshable_account_count == 0,
        disabled_reason: if is_refresh_in_flight {
            Some("A background refresh is already running.".to_string())
        } else if overview.refreshable_account_count == 0 {
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
        search_value: Some("force refresh all accounts".to_string()),
        ..Default::default()
    });

    items
}
