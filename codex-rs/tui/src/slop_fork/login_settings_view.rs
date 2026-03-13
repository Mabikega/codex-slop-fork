use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use ratatui::buffer::Buffer;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Block;
use ratatui::widgets::Widget;

use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::GenericDisplayRow;
use crate::bottom_pane::ScrollState;
use crate::bottom_pane::measure_rows_height;
use crate::bottom_pane::render_rows;
use crate::key_hint;
use crate::render::Insets;
use crate::render::RectExt as _;
use crate::render::renderable::Renderable;
use crate::slop_fork::LoginPopupKind;
use crate::slop_fork::LoginSettingsState;
use crate::slop_fork::SlopForkEvent;
use crate::style::user_message_style;

use crate::app_event::AppEvent;
use crate::bottom_pane::CancellationEvent;
use crate::bottom_pane::popup_consts::MAX_POPUP_ROWS;

const LOGIN_POPUP_VIEW_ID: &str = "login-popup";

pub(crate) struct LoginSettingsView {
    settings: LoginSettingsState,
    state: ScrollState,
    complete: bool,
    app_event_tx: AppEventSender,
    header: Box<dyn Renderable>,
    footer_hint: Line<'static>,
}

impl LoginSettingsView {
    pub(crate) fn new(
        settings: LoginSettingsState,
        header: Box<dyn Renderable>,
        app_event_tx: AppEventSender,
    ) -> Self {
        let mut view = Self {
            settings,
            state: ScrollState::new(),
            complete: false,
            app_event_tx,
            header,
            footer_hint: login_settings_popup_hint_line(),
        };
        view.initialize_selection();
        view
    }

    fn initialize_selection(&mut self) {
        if self.visible_len() == 0 {
            self.state.selected_idx = None;
        } else if self.state.selected_idx.is_none() {
            self.state.selected_idx = Some(0);
        }
    }

    fn visible_len(&self) -> usize {
        7
    }

    fn build_rows(&self) -> Vec<GenericDisplayRow> {
        let rows = [
            (
                "Auto switch accounts",
                "Switch saved accounts when the active one is rate-limited.",
                self.settings.auto_switch_accounts_on_rate_limit,
            ),
            (
                "Follow external switch",
                "Adopt account changes written by another Codex instance without restarting.",
                self.settings.follow_external_account_switches,
            ),
            (
                "API key fallback",
                "Allow API-key accounts only after every ChatGPT account is exhausted.",
                self.settings.api_key_fallback_on_all_accounts_limited,
            ),
            (
                "Auto start 5h quota",
                "Automatically send one tiny request when cached data says the 5-hour window is untouched.",
                self.settings.auto_start_five_hour_quota,
            ),
            (
                "Auto start weekly quota",
                "Automatically send one tiny request when cached data says the 7-day window is untouched.",
                self.settings.auto_start_weekly_quota,
            ),
            (
                "Number account labels",
                "Show saved ChatGPT accounts as Account N, ordered by UID when available, instead of exposing email addresses.",
                self.settings.show_account_numbers_instead_of_emails,
            ),
            (
                "Avg limits in status line",
                "Show average saved-account 5-hour and weekly limits in the status line.",
                self.settings.show_average_account_limits_in_status_line,
            ),
        ];

        rows.into_iter()
            .enumerate()
            .map(|(idx, (name, description, enabled))| {
                let prefix = if self.state.selected_idx == Some(idx) {
                    '›'
                } else {
                    ' '
                };
                let marker = if enabled { 'x' } else { ' ' };
                GenericDisplayRow {
                    name: format!("{prefix} [{marker}] {name}"),
                    description: Some(description.to_string()),
                    ..Default::default()
                }
            })
            .collect()
    }

    fn move_up(&mut self) {
        let len = self.visible_len();
        self.state.move_up_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn move_down(&mut self) {
        let len = self.visible_len();
        self.state.move_down_wrap(len);
        self.state.ensure_visible(len, MAX_POPUP_ROWS.min(len));
    }

    fn toggle_selected(&mut self) {
        match self.state.selected_idx {
            Some(0) => {
                self.settings.auto_switch_accounts_on_rate_limit =
                    !self.settings.auto_switch_accounts_on_rate_limit;
            }
            Some(1) => {
                self.settings.follow_external_account_switches =
                    !self.settings.follow_external_account_switches;
            }
            Some(2) => {
                self.settings.api_key_fallback_on_all_accounts_limited =
                    !self.settings.api_key_fallback_on_all_accounts_limited;
            }
            Some(3) => {
                self.settings.auto_start_five_hour_quota =
                    !self.settings.auto_start_five_hour_quota;
            }
            Some(4) => {
                self.settings.auto_start_weekly_quota = !self.settings.auto_start_weekly_quota;
            }
            Some(5) => {
                self.settings.show_account_numbers_instead_of_emails =
                    !self.settings.show_account_numbers_instead_of_emails;
            }
            Some(6) => {
                self.settings.show_average_account_limits_in_status_line =
                    !self.settings.show_average_account_limits_in_status_line;
            }
            Some(_) | None => {}
        }
    }

    fn save_and_close(&mut self) {
        if self.complete {
            return;
        }

        self.app_event_tx
            .send(AppEvent::SlopFork(SlopForkEvent::SaveLoginSettings {
                settings: self.settings,
            }));
        self.complete = true;
    }

    fn close_without_saving(&mut self) {
        if self.complete {
            return;
        }

        self.app_event_tx
            .send(AppEvent::SlopFork(SlopForkEvent::OpenLoginPopup {
                kind: LoginPopupKind::Root,
            }));
        self.complete = true;
    }

    fn rows_width(total_width: u16) -> u16 {
        total_width.saturating_sub(2)
    }
}

impl BottomPaneView for LoginSettingsView {
    fn handle_key_event(&mut self, key_event: KeyEvent) {
        match key_event {
            KeyEvent {
                code: KeyCode::Up, ..
            }
            | KeyEvent {
                code: KeyCode::Char('p'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{0010}'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('k'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_up(),
            KeyEvent {
                code: KeyCode::Down,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::CONTROL,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('\u{000e}'),
                modifiers: KeyModifiers::NONE,
                ..
            }
            | KeyEvent {
                code: KeyCode::Char('j'),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.move_down(),
            KeyEvent {
                code: KeyCode::Char(' '),
                modifiers: KeyModifiers::NONE,
                ..
            } => self.toggle_selected(),
            KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                ..
            } => self.save_and_close(),
            _ => {}
        }
    }

    fn is_complete(&self) -> bool {
        self.complete
    }

    fn view_id(&self) -> Option<&'static str> {
        Some(LOGIN_POPUP_VIEW_ID)
    }

    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.close_without_saving();
        CancellationEvent::Handled
    }
}

impl Renderable for LoginSettingsView {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let [content_area, footer_area] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);

        Block::default()
            .style(user_message_style())
            .render(content_area, buf);

        let header_height = self
            .header
            .desired_height(content_area.width.saturating_sub(4));
        let rows = self.build_rows();
        let rows_width = Self::rows_width(content_area.width);
        let rows_height = measure_rows_height(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );
        let [header_area, _, list_area] = Layout::vertical([
            Constraint::Max(header_height),
            Constraint::Max(1),
            Constraint::Length(rows_height),
        ])
        .areas(content_area.inset(Insets::vh(1, 2)));

        self.header.render(header_area, buf);

        if list_area.height > 0 {
            let render_area = Rect {
                x: list_area.x.saturating_sub(2),
                y: list_area.y,
                width: rows_width.max(1),
                height: list_area.height,
            };
            render_rows(
                render_area,
                buf,
                &rows,
                &self.state,
                MAX_POPUP_ROWS,
                "  No account settings available",
            );
        }

        let hint_area = Rect {
            x: footer_area.x + 2,
            y: footer_area.y,
            width: footer_area.width.saturating_sub(2),
            height: footer_area.height,
        };
        self.footer_hint.clone().dim().render(hint_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let rows = self.build_rows();
        let rows_width = Self::rows_width(width);
        let rows_height = measure_rows_height(
            &rows,
            &self.state,
            MAX_POPUP_ROWS,
            rows_width.saturating_add(1),
        );

        let mut height = self.header.desired_height(width.saturating_sub(4));
        height = height.saturating_add(rows_height + 3);
        height.saturating_add(1)
    }
}

fn login_settings_popup_hint_line() -> Line<'static> {
    Line::from(vec![
        "Press ".into(),
        key_hint::plain(KeyCode::Char(' ')).into(),
        " to select or ".into(),
        key_hint::plain(KeyCode::Enter).into(),
        " to save for next conversation".into(),
    ])
}
