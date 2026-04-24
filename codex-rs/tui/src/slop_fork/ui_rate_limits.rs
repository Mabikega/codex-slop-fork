use super::*;
use std::future::Future;
use std::sync::LazyLock;

#[path = "ui_rate_limits_touch.rs"]
mod touch;

use touch::QuotaTouchClient;
use touch::SavedAccountBackendSession;
use touch::touch_quota_windows;

static SAVED_ACCOUNT_QUOTA_TOUCH_QUEUE: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));

async fn run_saved_account_quota_touch_serialized<T>(future: impl Future<Output = T>) -> T {
    let _guard = SAVED_ACCOUNT_QUOTA_TOUCH_QUEUE.lock().await;
    future.await
}

const DEACTIVATED_WORKSPACE_ERROR_CODE: &str = "deactivated_workspace";

#[derive(Debug)]
pub(super) struct SavedAccountRateLimitsRefreshState {
    pub(super) started_at: Instant,
    pub(super) target: SavedAccountRateLimitsRefreshTarget,
}

impl SavedAccountRateLimitsRefreshState {
    pub(super) fn description(&self, due_count: usize) -> String {
        match &self.target {
            SavedAccountRateLimitsRefreshTarget::Due => match due_count {
                0 => "Refreshing due saved-account limits in the background.".to_string(),
                1 => "Refreshing 1 due saved account in the background.".to_string(),
                count => format!("Refreshing {count} due saved accounts in the background."),
            },
            SavedAccountRateLimitsRefreshTarget::Accounts(account_ids)
            | SavedAccountRateLimitsRefreshTarget::AllAccounts(account_ids) => {
                match account_ids.len() {
                    0 => "Refreshing saved-account limits in the background.".to_string(),
                    1 => "Refreshing 1 saved account in the background.".to_string(),
                    count => format!("Refreshing {count} saved accounts in the background."),
                }
            }
        }
    }

    pub(super) fn touch_request_after_refresh(
        &self,
        codex_home: &Path,
        updated_account_ids: &[String],
    ) -> Option<(HashSet<String>, TouchQuotaMode)> {
        let (account_ids, touch_mode) = match &self.target {
            SavedAccountRateLimitsRefreshTarget::AllAccounts(account_ids) => {
                (account_ids, automatic_touch_mode(codex_home)?)
            }
            _ => return None,
        };

        let requested_account_ids =
            requested_account_ids_after_refresh(account_ids, updated_account_ids);
        if requested_account_ids.is_empty() {
            return None;
        }

        Some((requested_account_ids, touch_mode))
    }
}

fn automatic_touch_mode(codex_home: &Path) -> Option<TouchQuotaMode> {
    let config = load_slop_fork_config(codex_home).unwrap_or_default();
    let touch_mode = TouchQuotaMode::Automatic {
        start_five_hour: config.auto_start_five_hour_quota,
        start_weekly: config.auto_start_weekly_quota,
    };
    let has_enabled_window = match touch_mode {
        TouchQuotaMode::Automatic {
            start_five_hour,
            start_weekly,
        } => start_five_hour || start_weekly,
    };

    has_enabled_window.then_some(touch_mode)
}

fn requested_account_ids_after_refresh(
    requested_account_ids: &HashSet<String>,
    updated_account_ids: &[String],
) -> HashSet<String> {
    if updated_account_ids.is_empty() {
        return HashSet::new();
    }

    updated_account_ids
        .iter()
        .filter(|account_id| requested_account_ids.contains(account_id.as_str()))
        .cloned()
        .collect()
}

#[derive(Debug)]
pub(super) enum SavedAccountRateLimitsRefreshTarget {
    Due,
    Accounts(HashSet<String>),
    AllAccounts(HashSet<String>),
}

impl SavedAccountRateLimitsRefreshTarget {
    pub(super) fn with_accounts(self, account_ids: HashSet<String>) -> Self {
        match self {
            Self::Due => Self::Due,
            Self::Accounts(_) => Self::Accounts(account_ids),
            Self::AllAccounts(_) => Self::AllAccounts(account_ids),
        }
    }

    pub(super) fn includes_account(&self, account_id: &str, account_is_due: bool) -> bool {
        match self {
            Self::Due => account_is_due,
            Self::Accounts(account_ids) | Self::AllAccounts(account_ids) => {
                account_ids.contains(account_id)
            }
        }
    }
}

pub(super) struct SavedAccountRateLimitsRefreshRenderable {
    started_at: Instant,
    details: String,
    frame_requester: FrameRequester,
    animations_enabled: bool,
}

impl SavedAccountRateLimitsRefreshRenderable {
    pub(super) fn new(
        started_at: Instant,
        details: String,
        frame_requester: FrameRequester,
        animations_enabled: bool,
    ) -> Self {
        Self {
            started_at,
            details,
            frame_requester,
            animations_enabled,
        }
    }

    fn wrapped_details_lines(&self, width: u16) -> Vec<Line<'static>> {
        if self.details.is_empty() || width == 0 {
            return Vec::new();
        }

        word_wrap_lines(
            std::iter::once(vec![self.details.clone().dim()]),
            RtOptions::new(usize::from(width))
                .initial_indent(Line::from("  └ ".dim()))
                .subsequent_indent(Line::from("    ".dim()))
                .break_words(/*break_words*/ true),
        )
    }
}

impl Renderable for SavedAccountRateLimitsRefreshRenderable {
    fn desired_height(&self, width: u16) -> u16 {
        1 + u16::try_from(self.wrapped_details_lines(width).len()).unwrap_or(0)
    }

    fn render(&self, area: ratatui::layout::Rect, buf: &mut ratatui::buffer::Buffer) {
        if area.is_empty() {
            return;
        }

        if self.animations_enabled {
            self.frame_requester
                .schedule_frame_in(Duration::from_millis(32));
        }

        let elapsed = fmt_elapsed_compact(self.started_at.elapsed().as_secs());
        let mut spans = Vec::new();
        spans.push(spinner(Some(self.started_at), self.animations_enabled));
        spans.push(" ".into());
        if self.animations_enabled {
            spans.extend(shimmer_spans("Refreshing saved account limits"));
        } else {
            spans.push("Refreshing saved account limits".into());
        }
        spans.push(" ".into());
        spans.push(format!("({elapsed})").dim());

        let mut lines = vec![truncate_line_with_ellipsis_if_overflow(
            Line::from(spans),
            usize::from(area.width),
        )];
        if area.height > 1 {
            let max_details = usize::from(area.height.saturating_sub(1));
            lines.extend(
                self.wrapped_details_lines(area.width)
                    .into_iter()
                    .take(max_details),
            );
        }

        Paragraph::new(Text::from(lines)).render_ref(area, buf);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TouchQuotaMode {
    Automatic {
        start_five_hour: bool,
        start_weekly: bool,
    },
}

impl TouchQuotaMode {
    fn enabled_window_kinds(self) -> Vec<account_rate_limits::QuotaWindowKind> {
        match self {
            Self::Automatic {
                start_five_hour,
                start_weekly,
            } => {
                let mut kinds = Vec::new();
                if start_five_hour {
                    kinds.push(account_rate_limits::QuotaWindowKind::FiveHour);
                }
                if start_weekly {
                    kinds.push(account_rate_limits::QuotaWindowKind::Weekly);
                }
                kinds
            }
        }
    }

    fn summary_prefix(self) -> &'static str {
        match self {
            Self::Automatic { .. } => "Auto-started cached quotas",
        }
    }
}

#[derive(Default)]
pub(crate) struct CachedQuotaTouchResult {
    pub(crate) checked_accounts: usize,
    pub(crate) message: String,
    pub(crate) updated_account_ids: Vec<String>,
}

impl SlopForkUi {
    pub(crate) fn refresh_saved_account_rate_limits(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        self.refresh_saved_account_rate_limits_inner(
            ctx,
            /*include_active*/ true,
            SavedAccountRateLimitsRefreshTarget::Due,
            /*requested_account_ids*/ None,
        )
    }

    pub(crate) fn refresh_all_saved_account_rate_limits(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        self.refresh_all_saved_account_rate_limits_with_target(
            ctx,
            SavedAccountRateLimitsRefreshTarget::AllAccounts(HashSet::new()),
        )
    }

    fn refresh_all_saved_account_rate_limits_with_target(
        &mut self,
        ctx: &SlopForkUiContext,
        target: SavedAccountRateLimitsRefreshTarget,
    ) -> Vec<SlopForkUiEffect> {
        let (_, _, _, accounts, _, _, _) = match self.login_popup_state(ctx) {
            Ok(state) => state,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load saved accounts: {err}"
                ))];
            }
        };
        let account_ids = accounts
            .into_iter()
            .filter(|account| auth_dot_json_is_chatgpt(&account.auth))
            .map(|account| account.id)
            .collect::<Vec<_>>();
        self.refresh_saved_account_rate_limits_inner(
            ctx,
            /*include_active*/ true,
            target.with_accounts(account_ids.iter().cloned().collect::<HashSet<_>>()),
            Some(account_ids),
        )
    }

    pub(crate) fn refresh_saved_account_rate_limit(
        &mut self,
        ctx: &SlopForkUiContext,
        account_id: &str,
    ) -> Vec<SlopForkUiEffect> {
        self.refresh_saved_account_rate_limits_inner(
            ctx,
            /*include_active*/ true,
            SavedAccountRateLimitsRefreshTarget::Accounts(
                [account_id.to_string()].into_iter().collect::<HashSet<_>>(),
            ),
            Some(vec![account_id.to_string()]),
        )
    }

    pub(crate) fn on_saved_account_rate_limits_refresh_completed(
        &mut self,
        ctx: &SlopForkUiContext,
        updated_account_ids: Vec<String>,
        is_login_view_active: bool,
    ) -> Vec<SlopForkUiEffect> {
        let completed_refresh = self.saved_account_rate_limits_refresh.take();
        let had_manual_refresh = completed_refresh.is_some();
        let touch_request = completed_refresh.as_ref().and_then(|refresh_state| {
            refresh_state.touch_request_after_refresh(&ctx.codex_home, &updated_account_ids)
        });
        let mut effects = self.account_limits_popup_refresh_effects(ctx, is_login_view_active);

        if had_manual_refresh {
            let refreshed_count = updated_account_ids.len();
            effects.push(SlopForkUiEffect::AddInfoMessage {
                message: match refreshed_count {
                    0 => "No saved account limit snapshots were refreshed.".to_string(),
                    1 => "Refreshed 1 saved account limit snapshot.".to_string(),
                    count => format!("Refreshed {count} saved account limit snapshots."),
                },
                hint: if refreshed_count == 0 {
                    Some(
                        "The selected accounts may still be waiting for fresh usage data."
                            .to_string(),
                    )
                } else {
                    None
                },
            });
        }

        if let Some((requested_account_ids, touch_mode)) = touch_request {
            let codex_home = ctx.codex_home.clone();
            let base_url = ctx.chatgpt_base_url.clone();
            let auth_credentials_store_mode = ctx.auth_credentials_store_mode;
            let app_event_tx = ctx.app_event_tx.clone();
            tokio::spawn(async move {
                let result = touch_cached_quotas_for_requested_saved_accounts(
                    codex_home,
                    base_url,
                    auth_credentials_store_mode,
                    requested_account_ids,
                    touch_mode,
                )
                .await;
                app_event_tx.send(AppEvent::SlopFork(
                    SlopForkEvent::SavedAccountQuotaTouchCompleted {
                        updated_account_ids: result.updated_account_ids,
                        message: result.message,
                    },
                ));
            });
        }

        effects
    }

    pub(crate) fn on_saved_account_quota_touch_completed(
        &mut self,
        ctx: &SlopForkUiContext,
        updated_account_ids: Vec<String>,
        message: String,
        is_login_view_active: bool,
    ) -> Vec<SlopForkUiEffect> {
        let mut effects = if updated_account_ids.is_empty() {
            Vec::new()
        } else {
            self.account_limits_popup_refresh_effects(ctx, is_login_view_active)
        };
        effects.push(SlopForkUiEffect::AddInfoMessage {
            message,
            hint: None,
        });
        effects
    }

    fn account_limits_popup_refresh_effects(
        &mut self,
        ctx: &SlopForkUiContext,
        is_login_view_active: bool,
    ) -> Vec<SlopForkUiEffect> {
        if is_login_view_active
            && matches!(
                self.active_login_popup_kind,
                Some(LoginPopupKind::AccountLimits)
            )
        {
            self.open_login_popup(ctx, LoginPopupKind::AccountLimits)
        } else {
            Vec::new()
        }
    }

    fn refresh_saved_account_rate_limits_inner(
        &mut self,
        ctx: &SlopForkUiContext,
        include_active: bool,
        target: SavedAccountRateLimitsRefreshTarget,
        requested_account_ids: Option<Vec<String>>,
    ) -> Vec<SlopForkUiEffect> {
        if self.saved_account_rate_limits_refresh.is_some() {
            return Vec::new();
        }

        self.saved_account_rate_limits_refresh = Some(SavedAccountRateLimitsRefreshState {
            started_at: Instant::now(),
            target,
        });
        let codex_home = ctx.codex_home.clone();
        let base_url = ctx.chatgpt_base_url.clone();
        let auth_credentials_store_mode = ctx.auth_credentials_store_mode;
        let app_event_tx = ctx.app_event_tx.clone();
        tokio::spawn(async move {
            let updated_account_ids = refresh_saved_account_rate_limits_once(
                codex_home,
                base_url,
                auth_credentials_store_mode,
                include_active,
                requested_account_ids,
            )
            .await;
            app_event_tx.send(AppEvent::SlopFork(
                SlopForkEvent::SavedAccountRateLimitsRefreshCompleted {
                    updated_account_ids,
                },
            ));
        });
        if matches!(
            self.active_login_popup_kind,
            Some(LoginPopupKind::AccountLimits)
        ) {
            self.open_login_popup(ctx, LoginPopupKind::AccountLimits)
        } else {
            Vec::new()
        }
    }
}

pub(crate) async fn fetch_rate_limits(
    base_url: String,
    auth: CodexAuth,
) -> Vec<(
    RateLimitSnapshot,
    account_rate_limits::RawRateLimitSnapshotInput,
)> {
    match BackendClient::from_auth(base_url, &auth) {
        Ok(client) => match client.get_detailed_rate_limits_many().await {
            Ok(snapshots) => snapshots
                .into_iter()
                .map(|(snapshot, raw)| (snapshot, backend_raw_rate_limit_snapshot_input(raw)))
                .collect(),
            Err(err) => {
                tracing::debug!(error = ?err, "failed to fetch rate limits from /usage");
                Vec::new()
            }
        },
        Err(err) => {
            tracing::debug!(error = ?err, "failed to construct backend client for rate limits");
            Vec::new()
        }
    }
}

#[derive(serde::Deserialize)]
struct UsageErrorEnvelope {
    #[serde(default)]
    detail: Option<UsageErrorDetail>,
}

#[derive(serde::Deserialize)]
struct UsageErrorDetail {
    #[serde(default)]
    code: Option<String>,
}

fn usage_error_code(err: &codex_backend_client::RequestError) -> Option<String> {
    let codex_backend_client::RequestError::UnexpectedStatus { body, .. } = err else {
        return None;
    };
    serde_json::from_str::<UsageErrorEnvelope>(body)
        .ok()
        .and_then(|envelope| envelope.detail)
        .and_then(|detail| detail.code)
}

fn saved_account_usage_error_code(err: &anyhow::Error) -> Option<String> {
    err.downcast_ref::<codex_backend_client::RequestError>()
        .and_then(usage_error_code)
}

fn backend_raw_rate_limit_snapshot_input(
    raw: codex_backend_client::RawRateLimitSnapshotInput,
) -> account_rate_limits::RawRateLimitSnapshotInput {
    account_rate_limits::RawRateLimitSnapshotInput {
        primary: raw.primary.map(backend_raw_rate_limit_window),
        secondary: raw.secondary.map(backend_raw_rate_limit_window),
    }
}

fn backend_raw_rate_limit_window(
    raw: codex_backend_client::RawRateLimitWindowSnapshot,
) -> account_rate_limits::RawRateLimitWindowSnapshot {
    account_rate_limits::RawRateLimitWindowSnapshot {
        used_percent: raw.used_percent,
        limit_window_seconds: raw.limit_window_seconds,
        reset_after_seconds: raw.reset_after_seconds,
        reset_at: raw.reset_at,
    }
}

pub(super) fn auth_dot_json_is_chatgpt(auth: &AuthDotJson) -> bool {
    auth.openai_api_key.is_none() && auth.tokens.is_some()
}

pub(crate) fn has_saved_chatgpt_accounts(codex_home: &Path) -> bool {
    auth_accounts::list_accounts(codex_home)
        .map(|accounts| {
            accounts
                .iter()
                .any(|account| auth_dot_json_is_chatgpt(&account.auth))
        })
        .unwrap_or(false)
}

fn codex_rate_limit_snapshot(
    snapshots: &[(
        RateLimitSnapshot,
        account_rate_limits::RawRateLimitSnapshotInput,
    )],
) -> Option<&(
    RateLimitSnapshot,
    account_rate_limits::RawRateLimitSnapshotInput,
)> {
    snapshots
        .iter()
        .find(|(snapshot, _raw)| snapshot.limit_id.as_deref() == Some("codex"))
        .or_else(|| snapshots.first())
}

fn cached_window_kinds_to_start(
    snapshot: &account_rate_limits::StoredRateLimitSnapshot,
    mode: TouchQuotaMode,
    now: DateTime<Utc>,
) -> Vec<account_rate_limits::QuotaWindowKind> {
    mode.enabled_window_kinds()
        .into_iter()
        .filter(|kind| account_rate_limits::quota_window_should_start(snapshot, *kind, now))
        .collect()
}

pub(crate) async fn touch_cached_quotas_for_saved_accounts(
    codex_home: PathBuf,
    base_url: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    mode: TouchQuotaMode,
) -> CachedQuotaTouchResult {
    touch_cached_quotas_for_requested_saved_accounts(
        codex_home,
        base_url,
        auth_credentials_store_mode,
        HashSet::new(),
        mode,
    )
    .await
}

pub(super) async fn touch_cached_quotas_for_requested_saved_accounts(
    codex_home: PathBuf,
    base_url: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    requested_account_ids: HashSet<String>,
    mode: TouchQuotaMode,
) -> CachedQuotaTouchResult {
    run_saved_account_quota_touch_serialized(async move {
        let now = Utc::now();
        let accounts = auth_accounts::list_accounts(&codex_home).unwrap_or_default();
        let snapshots = account_rate_limits::snapshot_map_for_accounts(&codex_home, &accounts)
            .unwrap_or_default();
        let restrict_to_requested_accounts = !requested_account_ids.is_empty();

        let mut checked_accounts = 0usize;
        let mut started_accounts = 0usize;
        let mut started_windows = 0usize;
        let mut updated_account_ids = Vec::new();

        for account in accounts {
            if restrict_to_requested_accounts && !requested_account_ids.contains(&account.id) {
                continue;
            }
            if !auth_dot_json_is_chatgpt(&account.auth) {
                continue;
            }
            let Some(snapshot) = snapshots.get(&account.id) else {
                continue;
            };
            let window_kinds = cached_window_kinds_to_start(snapshot, mode, now);
            if window_kinds.is_empty() {
                continue;
            }
            checked_accounts += 1;

            let plan = snapshot.plan.clone().or_else(|| {
                account
                    .auth
                    .tokens
                    .as_ref()
                    .and_then(|tokens| tokens.id_token.get_chatgpt_plan_type())
            });
            let session = match SavedAccountBackendSession::new(
                codex_home.clone(),
                base_url.clone(),
                account.id.clone(),
                account.auth.clone(),
                auth_credentials_store_mode,
            ) {
                Ok(session) => session,
                Err(err) => {
                    tracing::warn!(
                        "failed to build auth for saved account {} while touching quotas: {err}",
                        account.id
                    );
                    continue;
                }
            };
            let mut touch_client = QuotaTouchClient::from_saved_account_session(session);
            match touch_quota_windows(
                &codex_home,
                &account.id,
                plan.as_deref(),
                &window_kinds,
                &mut touch_client,
            )
            .await
            {
                Ok(true) => {
                    started_accounts += 1;
                    started_windows += window_kinds.len();
                    updated_account_ids.push(account.id.clone());
                }
                Ok(false) => {}
                Err(err) => {
                    tracing::warn!(
                        "failed to touch cached quotas for saved account {}: {err}",
                        account.id
                    );
                }
            }
        }

        let message = match (checked_accounts, started_accounts, started_windows) {
            (0, _, _) => format!(
                "{}: no cached untouched windows needed a start.",
                mode.summary_prefix()
            ),
            (_, 0, _) => format!(
                "{}: checked {checked_accounts} account(s), but no touch request completed.",
                mode.summary_prefix()
            ),
            _ => format!(
                "{}: started {started_windows} window(s) across {started_accounts} account(s).",
                mode.summary_prefix()
            ),
        };

        CachedQuotaTouchResult {
            checked_accounts,
            message,
            updated_account_ids,
        }
    })
    .await
}

pub(crate) async fn maybe_touch_active_account_cached_quotas(
    codex_home: PathBuf,
    base_url: String,
    auth: CodexAuth,
    mode: TouchQuotaMode,
) -> CachedQuotaTouchResult {
    let Some(account_id) = auth
        .auth_dot_json()
        .and_then(|auth_dot_json| auth_accounts::stored_account_id(&auth_dot_json))
        .or_else(|| auth.get_account_id())
    else {
        return CachedQuotaTouchResult {
            checked_accounts: 0,
            message: format!("{}: no active account id available.", mode.summary_prefix()),
            updated_account_ids: Vec::new(),
        };
    };
    let Some(snapshot) = account_rate_limits::load_rate_limit_snapshot(&codex_home, &account_id)
        .ok()
        .flatten()
    else {
        return CachedQuotaTouchResult {
            checked_accounts: 0,
            message: format!(
                "{}: no cached active-account snapshot found.",
                mode.summary_prefix()
            ),
            updated_account_ids: Vec::new(),
        };
    };

    let window_kinds = cached_window_kinds_to_start(&snapshot, mode, Utc::now());
    if window_kinds.is_empty() {
        return CachedQuotaTouchResult {
            checked_accounts: 0,
            message: format!(
                "{}: no cached active-account windows needed a start.",
                mode.summary_prefix()
            ),
            updated_account_ids: Vec::new(),
        };
    }

    let mut touch_client = match QuotaTouchClient::from_auth(&base_url, &auth) {
        Ok(client) => client,
        Err(err) => {
            return CachedQuotaTouchResult {
                checked_accounts: 1,
                message: format!(
                    "{}: failed to build active-account backend client: {err}",
                    mode.summary_prefix()
                ),
                updated_account_ids: Vec::new(),
            };
        }
    };
    match touch_quota_windows(
        &codex_home,
        &account_id,
        snapshot.plan.as_deref(),
        &window_kinds,
        &mut touch_client,
    )
    .await
    {
        Ok(true) => CachedQuotaTouchResult {
            checked_accounts: 1,
            message: format!(
                "{}: started {} window(s) for the active account.",
                mode.summary_prefix(),
                window_kinds.len()
            ),
            updated_account_ids: vec![account_id],
        },
        Ok(false) => CachedQuotaTouchResult {
            checked_accounts: 1,
            message: format!(
                "{}: no active-account touch was needed.",
                mode.summary_prefix()
            ),
            updated_account_ids: Vec::new(),
        },
        Err(err) => CachedQuotaTouchResult {
            checked_accounts: 1,
            message: format!(
                "{}: failed for the active account: {err}",
                mode.summary_prefix()
            ),
            updated_account_ids: Vec::new(),
        },
    }
}

pub(crate) async fn refresh_saved_account_rate_limits_once(
    codex_home: PathBuf,
    base_url: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    include_active: bool,
    requested_account_ids: Option<Vec<String>>,
) -> Vec<String> {
    let now = Utc::now();
    let active_account_id =
        auth_accounts::current_active_account_id(&codex_home, auth_credentials_store_mode)
            .ok()
            .flatten();
    let accounts = auth_accounts::list_accounts(&codex_home).unwrap_or_default();
    let mut rate_limit_snapshots =
        account_rate_limits::snapshot_map_for_accounts(&codex_home, &accounts).unwrap_or_default();
    let requested_account_ids =
        requested_account_ids.map(|ids| ids.into_iter().collect::<HashSet<_>>());

    let mut due_accounts = Vec::new();
    for account in accounts {
        let is_requested = requested_account_ids
            .as_ref()
            .is_some_and(|ids| ids.contains(&account.id));
        if !auth_dot_json_is_chatgpt(&account.auth)
            || (!include_active
                && !is_requested
                && active_account_id.as_deref() == Some(account.id.as_str()))
        {
            continue;
        }
        let stored_snapshot = rate_limit_snapshots.remove(&account.id);
        let reset_at = stored_snapshot
            .as_ref()
            .and_then(account_rate_limits::snapshot_reset_at);
        let plan = stored_snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.plan.clone())
            .or_else(|| {
                account
                    .auth
                    .tokens
                    .as_ref()
                    .and_then(|tokens| tokens.id_token.get_chatgpt_plan_type())
            });
        let should_refresh = if is_requested {
            true
        } else {
            account_rate_limits::mark_rate_limit_refresh_attempt_if_due(
                &codex_home,
                &account.id,
                plan.as_deref(),
                reset_at,
                now,
                account_rate_limits::rate_limit_refresh_stale_interval(),
            )
            .unwrap_or(false)
        };
        if should_refresh {
            due_accounts.push((account, plan));
        }
    }

    let mut updated_account_ids = Vec::new();
    for (account, plan) in due_accounts {
        let mut session = match SavedAccountBackendSession::new(
            codex_home.clone(),
            base_url.clone(),
            account.id.clone(),
            account.auth.clone(),
            auth_credentials_store_mode,
        ) {
            Ok(session) => session,
            Err(err) => {
                tracing::warn!(
                    "failed to build background auth for saved account {}: {err}",
                    account.id
                );
                continue;
            }
        };
        let snapshots = match session.get_detailed_rate_limits_many().await {
            Ok(snapshots) => snapshots
                .into_iter()
                .map(|(snapshot, raw)| (snapshot, backend_raw_rate_limit_snapshot_input(raw)))
                .collect::<Vec<_>>(),
            Err(err) => {
                if saved_account_usage_error_code(&err).as_deref()
                    == Some(DEACTIVATED_WORKSPACE_ERROR_CODE)
                    && let Err(persist_err) = account_rate_limits::record_workspace_deactivated(
                        &codex_home,
                        &account.id,
                        plan.as_deref(),
                        Utc::now(),
                    )
                {
                    tracing::warn!(
                        "failed to persist deactivated workspace marker for saved account {}: {persist_err}",
                        account.id
                    );
                }
                tracing::warn!(
                    "failed to refresh rate limits for saved account {}: {err}",
                    account.id
                );
                continue;
            }
        };
        let Some((snapshot, raw)) = codex_rate_limit_snapshot(&snapshots) else {
            continue;
        };
        if let Err(err) = account_rate_limits::record_rate_limit_snapshot_with_raw(
            &codex_home,
            &account.id,
            plan.as_deref(),
            snapshot,
            Some(raw),
            Utc::now(),
        ) {
            tracing::warn!(
                "failed to persist rate-limit snapshot for saved account {}: {err}",
                account.id
            );
            continue;
        }
        updated_account_ids.push(account.id);
    }

    updated_account_ids
}

pub(crate) fn saved_account_rate_limit_refresh_is_due(
    account: &auth_accounts::StoredAccount,
    snapshot: Option<&account_rate_limits::StoredRateLimitSnapshot>,
    now: DateTime<Utc>,
) -> bool {
    auth_dot_json_is_chatgpt(&account.auth)
        && account_rate_limits::rate_limit_refresh_is_due(
            snapshot,
            now,
            account_rate_limits::rate_limit_refresh_stale_interval(),
        )
}

#[cfg(test)]
mod tests {
    use super::run_saved_account_quota_touch_serialized;
    use pretty_assertions::assert_eq;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::sync::oneshot;

    #[tokio::test(flavor = "multi_thread")]
    async fn saved_account_quota_touch_requests_are_serialized() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let second_started = Arc::new(AtomicBool::new(false));
        let (first_started_tx, first_started_rx) = oneshot::channel();
        let (release_first_tx, release_first_rx) = oneshot::channel();

        let first_events = Arc::clone(&events);
        let first = tokio::spawn(async move {
            run_saved_account_quota_touch_serialized(async move {
                first_events
                    .lock()
                    .expect("lock first events")
                    .push("first-start");
                first_started_tx.send(()).expect("signal first start");
                release_first_rx.await.expect("release first request");
                first_events
                    .lock()
                    .expect("lock first events")
                    .push("first-end");
            })
            .await;
        });

        first_started_rx.await.expect("wait for first request");

        let second_events = Arc::clone(&events);
        let second_started_flag = Arc::clone(&second_started);
        let second = tokio::spawn(async move {
            run_saved_account_quota_touch_serialized(async move {
                second_events
                    .lock()
                    .expect("lock second events")
                    .push("second-start");
                second_started_flag.store(true, Ordering::SeqCst);
                second_events
                    .lock()
                    .expect("lock second events")
                    .push("second-end");
            })
            .await;
        });

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(second_started.load(Ordering::SeqCst), false);
        release_first_tx.send(()).expect("release first request");

        first.await.expect("join first request");
        second.await.expect("join second request");

        assert_eq!(
            *events.lock().expect("lock serialized events"),
            vec!["first-start", "first-end", "second-start", "second-end"]
        );
    }
}
