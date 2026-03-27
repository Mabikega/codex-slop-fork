use std::ops::Range;

use crate::app_event::AppEvent;
use crate::bottom_pane::custom_prompt_view::CustomPromptView;
use crate::slop_fork::AccountsPopupContext;
use crate::slop_fork::AccountsRootOverview;
use crate::slop_fork::DeviceCodeLoginState;
use crate::slop_fork::LoginSettingsState;
use crate::slop_fork::RenameAccountsPopupOverview;
use crate::slop_fork::SavedAccountLimitsOverview;
use crate::slop_fork::SavedAccountRateLimitsRefreshState;
use crate::slop_fork::SavedAccountRateLimitsRefreshTarget;
use crate::slop_fork::SavedAccountsPopupOverview;
use crate::slop_fork::SlopForkCommandExecution;
use crate::slop_fork::SlopForkCommandHistory;
use crate::slop_fork::SlopForkEvent;
use crate::slop_fork::account_limits::ACCOUNT_LIMITS_VIEW_ID;
use crate::slop_fork::account_settings_view::AccountSettingsView;
use crate::slop_fork::account_views::ACCOUNTS_DEVICE_CODE_VIEW_ID;
use crate::slop_fork::account_views::account_settings_popup_header;
use crate::slop_fork::account_views::accounts_root_view_params;
use crate::slop_fork::account_views::device_code_login_view_params;
use crate::slop_fork::account_views::rename_accounts_view_params;
use crate::slop_fork::account_views::saved_account_limits_selection_view_params;
use crate::slop_fork::account_views::saved_accounts_view_params;

use super::*;

impl ChatWidget {
    pub(crate) fn maybe_dispatch_slop_fork_command(&mut self, user_message: &UserMessage) -> bool {
        if self.try_dispatch_auto_command(user_message) {
            return true;
        }
        if self.try_dispatch_pilot_command(user_message) {
            return true;
        }
        self.try_dispatch_autoresearch_command(user_message)
    }

    pub(crate) fn show_slop_fork_accounts_root(&mut self, overview: AccountsRootOverview) {
        self.bottom_pane
            .show_selection_view(accounts_root_view_params(overview));
        self.request_redraw();
    }

    pub(crate) fn show_slop_fork_saved_accounts(&mut self, overview: SavedAccountsPopupOverview) {
        self.bottom_pane
            .show_selection_view(saved_accounts_view_params(overview));
        self.request_redraw();
    }

    pub(crate) fn show_slop_fork_saved_account_renames(
        &mut self,
        overview: RenameAccountsPopupOverview,
    ) {
        self.bottom_pane
            .show_selection_view(rename_accounts_view_params(overview));
        self.request_redraw();
    }

    pub(crate) fn show_slop_fork_account_settings(
        &mut self,
        settings: LoginSettingsState,
        popup_context: AccountsPopupContext,
    ) {
        self.bottom_pane
            .show_view(Box::new(AccountSettingsView::new(
                settings,
                account_settings_popup_header(&popup_context),
                self.app_event_tx.clone(),
            )));
        self.request_redraw();
    }

    pub(crate) fn show_slop_fork_device_code_login(&mut self, state: DeviceCodeLoginState) {
        let params = device_code_login_view_params(state.clone());
        if !self
            .bottom_pane
            .replace_selection_view_if_active(ACCOUNTS_DEVICE_CODE_VIEW_ID, params)
        {
            self.bottom_pane
                .show_selection_view(device_code_login_view_params(state));
        }
        self.request_redraw();
    }

    pub(crate) fn dismiss_slop_fork_device_code_login(&mut self) {
        let _ = self
            .bottom_pane
            .dismiss_active_view_if_matches(ACCOUNTS_DEVICE_CODE_VIEW_ID);
        self.request_redraw();
    }

    pub(crate) fn show_slop_fork_saved_account_limits(
        &mut self,
        overview: SavedAccountLimitsOverview,
    ) {
        self.render_slop_fork_saved_account_limits(overview, /*open_if_inactive*/ true);
    }

    pub(crate) fn refresh_visible_slop_fork_saved_account_limits(
        &mut self,
        overview: SavedAccountLimitsOverview,
    ) {
        self.render_slop_fork_saved_account_limits(overview, /*open_if_inactive*/ false);
    }

    fn render_slop_fork_saved_account_limits(
        &mut self,
        overview: SavedAccountLimitsOverview,
        open_if_inactive: bool,
    ) {
        let refresh_state = self.saved_account_limits_refresh.as_ref();
        let selected_index = self
            .bottom_pane
            .selected_index_for_active_view(ACCOUNT_LIMITS_VIEW_ID);
        let params =
            saved_account_limits_selection_view_params(&overview, refresh_state, selected_index);

        if !self
            .bottom_pane
            .replace_selection_view_if_active(ACCOUNT_LIMITS_VIEW_ID, params)
            && open_if_inactive
        {
            self.bottom_pane
                .show_selection_view(saved_account_limits_selection_view_params(
                    &overview,
                    refresh_state,
                    /*initial_selected_idx*/ None,
                ));
        }
        self.request_redraw();
    }

    pub(crate) fn begin_slop_fork_saved_account_limits_refresh(
        &mut self,
        target: SavedAccountRateLimitsRefreshTarget,
    ) -> bool {
        if self.saved_account_limits_refresh.is_some() {
            return false;
        }
        self.saved_account_limits_refresh = Some(SavedAccountRateLimitsRefreshState {
            _started_at: Instant::now(),
            target,
        });
        true
    }

    pub(crate) fn take_slop_fork_saved_account_limits_refresh(
        &mut self,
    ) -> Option<SavedAccountRateLimitsRefreshState> {
        self.saved_account_limits_refresh.take()
    }

    pub(crate) fn show_slop_fork_api_key_prompt(&mut self) {
        let tx = self.app_event_tx.clone();
        let view = CustomPromptView::new(
            "Enter API key".to_string(),
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
                tx.send(AppEvent::SlopFork(SlopForkEvent::SubmitApiKeyLogin {
                    api_key,
                }));
            }),
        );
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn add_slop_fork_command_submission(&mut self, submission: SlopForkCommandHistory) {
        self.add_to_history(history_cell::new_user_prompt(
            submission.text,
            submission.text_elements,
            Vec::new(),
            Vec::new(),
        ));
        self.request_redraw();
    }

    pub(crate) fn apply_slop_fork_command_execution(
        &mut self,
        execution: SlopForkCommandExecution,
    ) {
        if !execution.lines.is_empty() {
            self.add_plain_history_lines(execution.lines);
        }
        if let Some(submit_message) = execution
            .submit_message
            .filter(|message| !message.is_empty())
        {
            let user_message: UserMessage = submit_message.into();
            let should_submit_now =
                self.is_session_configured() && !self.is_plan_streaming_in_tui();
            if should_submit_now {
                self.reasoning_buffer.clear();
                self.full_reasoning_buffer.clear();
                self.set_status_header(String::from("Working"));
                self.submit_user_message(user_message);
            } else {
                self.queue_user_message(user_message);
            }
        }
        self.request_redraw();
    }

    pub(crate) fn last_text_user_message(&self) -> Option<&str> {
        self.last_rendered_user_message_event
            .as_ref()
            .map(|event| event.message.trim())
            .filter(|message| !message.is_empty())
    }

    fn try_dispatch_auto_command(&mut self, user_message: &UserMessage) -> bool {
        let Some((first_token, range)) =
            crate::slop_fork::auto_command::first_token(&user_message.text)
        else {
            return false;
        };
        let bound_path = slop_fork_binding_path(
            &user_message.text,
            &user_message.text_elements,
            &user_message.mention_bindings,
            range,
        );
        if !crate::slop_fork::auto_command::should_dispatch_auto_command(first_token, bound_path) {
            return false;
        }
        if !user_message.local_images.is_empty() || !user_message.remote_image_urls.is_empty() {
            self.add_error_message("$auto does not accept image attachments.".to_string());
            return true;
        }
        let args = crate::slop_fork::auto_command::parse_auto_command_args(&user_message.text)
            .unwrap_or_default()
            .to_string();
        if self.bottom_pane.is_task_running()
            && crate::slop_fork::auto_command::auto_command_requires_idle_session(&args)
        {
            self.add_error_message("'$auto' is disabled while a task is in progress.".to_string());
            return true;
        }
        let history = crate::slop_fork::auto_command::should_record_auto_command_in_history(&args)
            .then(|| SlopForkCommandHistory {
                text: user_message.text.clone(),
                text_elements: user_message.text_elements.clone(),
            });
        self.app_event_tx
            .send(AppEvent::SlopFork(SlopForkEvent::ExecuteAuto {
                args,
                history,
            }));
        true
    }

    fn try_dispatch_pilot_command(&mut self, user_message: &UserMessage) -> bool {
        let Some((first_token, range)) =
            crate::slop_fork::pilot_command::first_token(&user_message.text)
        else {
            return false;
        };
        let bound_path = slop_fork_binding_path(
            &user_message.text,
            &user_message.text_elements,
            &user_message.mention_bindings,
            range,
        );
        if !crate::slop_fork::pilot_command::should_dispatch_pilot_command(first_token, bound_path)
        {
            return false;
        }
        if !user_message.local_images.is_empty() || !user_message.remote_image_urls.is_empty() {
            self.add_error_message("$pilot does not accept image attachments.".to_string());
            return true;
        }
        let args = crate::slop_fork::pilot_command::parse_pilot_command_args(&user_message.text)
            .unwrap_or_default()
            .to_string();
        let history = crate::slop_fork::pilot_command::should_record_pilot_command_in_history(
            &args,
        )
        .then(|| SlopForkCommandHistory {
            text: user_message.text.clone(),
            text_elements: user_message.text_elements.clone(),
        });
        self.app_event_tx
            .send(AppEvent::SlopFork(SlopForkEvent::ExecutePilot {
                args,
                history,
            }));
        true
    }

    fn try_dispatch_autoresearch_command(&mut self, user_message: &UserMessage) -> bool {
        let Some((first_token, range)) =
            crate::slop_fork::autoresearch_command::first_token(&user_message.text)
        else {
            return false;
        };
        let bound_path = slop_fork_binding_path(
            &user_message.text,
            &user_message.text_elements,
            &user_message.mention_bindings,
            range,
        );
        if !crate::slop_fork::autoresearch_command::should_dispatch_autoresearch_command(
            first_token,
            bound_path,
        ) {
            return false;
        }
        if !user_message.local_images.is_empty() || !user_message.remote_image_urls.is_empty() {
            self.add_error_message("$autoresearch does not accept image attachments.".to_string());
            return true;
        }
        let args = crate::slop_fork::autoresearch_command::parse_autoresearch_command_args(
            &user_message.text,
        )
        .unwrap_or_default()
        .to_string();
        let history =
            crate::slop_fork::autoresearch_command::should_record_autoresearch_command_in_history(
                &args,
            )
            .then(|| SlopForkCommandHistory {
                text: user_message.text.clone(),
                text_elements: user_message.text_elements.clone(),
            });
        self.app_event_tx
            .send(AppEvent::SlopFork(SlopForkEvent::ExecuteAutoresearch {
                args,
                history,
            }));
        true
    }
}

fn slop_fork_binding_path<'a>(
    text: &str,
    text_elements: &[TextElement],
    mention_bindings: &'a [MentionBinding],
    target_range: Range<usize>,
) -> Option<&'a str> {
    let mut ordered_mentions: Vec<&TextElement> = text_elements
        .iter()
        .filter(|element| {
            element
                .placeholder(text)
                .is_some_and(|placeholder| placeholder.starts_with('$'))
        })
        .collect();
    ordered_mentions.sort_by_key(|element| element.byte_range.start);

    for (mention_index, element) in ordered_mentions.into_iter().enumerate() {
        let path = mention_bindings
            .get(mention_index)
            .map(|binding| binding.path.as_str());
        if element.byte_range.start == target_range.start
            && element.byte_range.end == target_range.end
        {
            return path;
        }
    }

    None
}
