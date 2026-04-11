use std::cmp::Ordering;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Context;
use codex_backend_client::Client as BackendClient;
use codex_backend_client::RawRateLimitSnapshotInput;
use codex_backend_client::RequestError;
use codex_core::CodexAuth;
use codex_core::auth::AuthCredentialsStoreMode;
use codex_core::auth::AuthDotJson;
use codex_core::slop_fork::auth_for_saved_account_file;
use codex_core::slop_fork::refresh_saved_account_auth_from_authority;
use codex_protocol::openai_models::InputModality;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::RateLimitSnapshot;

const FALLBACK_TOUCH_MODEL_CANDIDATES: &[&str] = &[
    "gpt-5.1-codex-mini",
    "gpt-5-codex-mini",
    "gpt-5.1-codex",
    "gpt-5-codex",
    "gpt-5.3-codex",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TouchModelChoice {
    pub(super) model: String,
    pub(super) reasoning_effort: Option<ReasoningEffort>,
}

pub(super) fn minimal_touch_response_request(
    model: &str,
    reasoning_effort: Option<ReasoningEffort>,
) -> serde_json::Value {
    let mut request = serde_json::json!({
        "model": model,
        "instructions": "",
        "input": [
            {
                "type": "message",
                "role": "user",
                "content": [
                    {
                        "type": "input_text",
                        "text": "Reply with exactly: OK"
                    }
                ]
            }
        ],
        "tools": [],
        "tool_choice": "auto",
        "parallel_tool_calls": false,
        "store": false,
        "stream": true,
        "include": []
    });

    if let Some(reasoning_effort) = reasoning_effort
        && let Some(request_object) = request.as_object_mut()
    {
        request_object.insert(
            "reasoning".to_string(),
            serde_json::json!({ "effort": reasoning_effort }),
        );
    }

    request
}

pub(super) fn fallback_touch_model_choices() -> Vec<TouchModelChoice> {
    FALLBACK_TOUCH_MODEL_CANDIDATES
        .iter()
        .map(|model| TouchModelChoice {
            model: (*model).to_string(),
            reasoning_effort: None,
        })
        .collect()
}

pub(super) fn touch_model_choices_from_model_infos(models: &[ModelInfo]) -> Vec<TouchModelChoice> {
    let mut touch_models = models
        .iter()
        .filter(|model| model.supported_in_api)
        .filter(|model| model.input_modalities.contains(&InputModality::Text))
        .collect::<Vec<_>>();
    touch_models.sort_by(|left, right| touch_model_info_order(left, right));
    let mut touch_models = touch_models
        .into_iter()
        .map(|model| TouchModelChoice {
            model: model.slug.clone(),
            reasoning_effort: lowest_supported_reasoning_effort(model),
        })
        .collect::<Vec<_>>();
    touch_models.dedup_by(|left, right| left.model == right.model);
    if touch_models.is_empty() {
        fallback_touch_model_choices()
    } else {
        touch_models
    }
}

pub(super) enum QuotaTouchClient {
    Active(BackendClient),
    Saved(Box<SavedAccountBackendSession>),
}

impl QuotaTouchClient {
    pub(super) fn from_auth(base_url: &str, auth: &CodexAuth) -> anyhow::Result<Self> {
        Ok(Self::Active(BackendClient::from_auth(
            base_url.to_string(),
            auth,
        )?))
    }

    pub(super) fn from_saved_account_session(session: SavedAccountBackendSession) -> Self {
        Self::Saved(Box::new(session))
    }

    async fn list_touch_model_choices(&mut self, account_id: &str) -> Vec<TouchModelChoice> {
        match self {
            Self::Active(client) => client
                .list_models_detailed()
                .await
                .map(|models: Vec<ModelInfo>| touch_model_choices_from_model_infos(&models))
                .unwrap_or_else(|err| {
                    tracing::debug!(
                        account_id,
                        error = ?err,
                        "failed to list models for quota touch"
                    );
                    fallback_touch_model_choices()
                }),
            Self::Saved(session) => {
                session
                    .list_touch_model_choices()
                    .await
                    .unwrap_or_else(|err| {
                        tracing::debug!(
                            account_id,
                            error = ?err,
                            "failed to list models for saved-account quota touch"
                        );
                        fallback_touch_model_choices()
                    })
            }
        }
    }

    async fn create_response(
        &mut self,
        request_body: &serde_json::Value,
    ) -> std::result::Result<(), RequestError> {
        match self {
            Self::Active(client) => client.create_response(request_body).await,
            Self::Saved(session) => session.create_response(request_body).await,
        }
    }

    async fn get_detailed_rate_limits_many(
        &mut self,
    ) -> anyhow::Result<
        Vec<(
            RateLimitSnapshot,
            codex_core::slop_fork::account_rate_limits::RawRateLimitSnapshotInput,
        )>,
    > {
        let snapshots = match self {
            Self::Active(client) => client.get_detailed_rate_limits_many().await?,
            Self::Saved(session) => session.get_detailed_rate_limits_many().await?,
        };
        Ok(snapshots
            .into_iter()
            .map(|(snapshot, raw)| (snapshot, super::backend_raw_rate_limit_snapshot_input(raw)))
            .collect())
    }

    fn is_saved_account(&self) -> bool {
        matches!(self, Self::Saved(_))
    }
}

type RequestFuture<'a, T> =
    Pin<Box<dyn Future<Output = std::result::Result<T, RequestError>> + Send + 'a>>;

pub(super) struct SavedAccountBackendSession {
    base_url: String,
    codex_home: PathBuf,
    account_id: String,
    auth_credentials_store_mode: AuthCredentialsStoreMode,
    auth_dot_json: AuthDotJson,
    auth: CodexAuth,
}

impl SavedAccountBackendSession {
    pub(super) fn new(
        codex_home: PathBuf,
        base_url: String,
        account_id: String,
        auth_dot_json: AuthDotJson,
        auth_credentials_store_mode: AuthCredentialsStoreMode,
    ) -> anyhow::Result<Self> {
        let auth = auth_for_saved_account_file(
            &codex_home,
            &account_id,
            auth_dot_json.clone(),
            auth_credentials_store_mode,
        )?;
        Ok(Self {
            base_url,
            codex_home,
            account_id,
            auth_credentials_store_mode,
            auth_dot_json,
            auth,
        })
    }

    pub(super) async fn list_touch_model_choices(
        &mut self,
    ) -> anyhow::Result<Vec<TouchModelChoice>> {
        self.run_request_with_auth_retry(|client| Box::pin(client.list_models_detailed()))
            .await
            .map(|models: Vec<ModelInfo>| touch_model_choices_from_model_infos(&models))
    }

    pub(super) async fn create_response(
        &mut self,
        request_body: &serde_json::Value,
    ) -> std::result::Result<(), RequestError> {
        match self.client()?.create_response(request_body).await {
            Ok(()) => Ok(()),
            Err(err) if err.is_unauthorized() => {
                self.refresh_auth().await.map_err(RequestError::from)?;
                self.client()?.create_response(request_body).await
            }
            Err(err) => Err(err),
        }
    }

    pub(super) async fn get_detailed_rate_limits_many(
        &mut self,
    ) -> anyhow::Result<Vec<(RateLimitSnapshot, RawRateLimitSnapshotInput)>> {
        self.run_request_with_auth_retry(|client| {
            Box::pin(client.get_detailed_rate_limits_many_detailed())
        })
        .await
    }

    fn client(&self) -> anyhow::Result<BackendClient> {
        BackendClient::from_auth(self.base_url.clone(), &self.auth)
    }

    async fn refresh_auth(&mut self) -> anyhow::Result<()> {
        refresh_saved_account_auth_from_authority(&self.auth)
            .await
            .with_context(|| {
                format!(
                    "failed to refresh saved account {} before retrying quota startup work",
                    self.account_id
                )
            })?;
        self.auth_dot_json = self.auth.auth_dot_json().ok_or_else(|| {
            anyhow::anyhow!(
                "saved account {} auth disappeared after refresh",
                self.account_id
            )
        })?;
        self.auth = auth_for_saved_account_file(
            &self.codex_home,
            &self.account_id,
            self.auth_dot_json.clone(),
            self.auth_credentials_store_mode,
        )?;
        Ok(())
    }

    async fn run_request_with_auth_retry<T>(
        &mut self,
        request: impl for<'a> Fn(&'a BackendClient) -> RequestFuture<'a, T>,
    ) -> anyhow::Result<T> {
        let client = self.client()?;
        match request(&client).await {
            Ok(value) => Ok(value),
            Err(err) if err.is_unauthorized() => {
                self.refresh_auth().await?;
                let client = self.client()?;
                Ok(request(&client).await?)
            }
            Err(err) => Err(err.into()),
        }
    }
}

fn should_confirm_quota_window_touch(
    state: codex_core::slop_fork::account_rate_limits::QuotaWindowState,
) -> bool {
    matches!(
        state,
        codex_core::slop_fork::account_rate_limits::QuotaWindowState::Started
            | codex_core::slop_fork::account_rate_limits::QuotaWindowState::Untouched
    )
}

fn mark_quota_window_touch_attempts(
    codex_home: &std::path::Path,
    account_id: &str,
    plan: Option<&str>,
    window_kinds: &[codex_core::slop_fork::account_rate_limits::QuotaWindowKind],
    attempted_at: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<()> {
    for kind in window_kinds {
        codex_core::slop_fork::account_rate_limits::mark_quota_window_touch_attempt(
            codex_home,
            account_id,
            plan,
            *kind,
            attempted_at,
        )?;
    }
    Ok(())
}

fn persist_touched_quota_snapshot(
    codex_home: &std::path::Path,
    account_id: &str,
    plan: Option<&str>,
    window_kinds: &[codex_core::slop_fork::account_rate_limits::QuotaWindowKind],
    snapshots: Vec<(
        RateLimitSnapshot,
        codex_core::slop_fork::account_rate_limits::RawRateLimitSnapshotInput,
    )>,
) -> anyhow::Result<()> {
    let Some((snapshot, raw)) = super::codex_rate_limit_snapshot(&snapshots) else {
        anyhow::bail!("quota touch succeeded but /usage returned no snapshots");
    };
    codex_core::slop_fork::account_rate_limits::record_rate_limit_snapshot_with_raw(
        codex_home,
        account_id,
        plan,
        snapshot,
        Some(raw),
        chrono::Utc::now(),
    )?;
    let stored = codex_core::slop_fork::account_rate_limits::load_rate_limit_snapshot(
        codex_home, account_id,
    )?
    .ok_or_else(|| anyhow::anyhow!("updated quota snapshot missing from cache"))?;
    let confirmed_at = chrono::Utc::now();
    for kind in window_kinds {
        let state = codex_core::slop_fork::account_rate_limits::quota_window_state(
            &stored,
            *kind,
            confirmed_at,
        );
        if should_confirm_quota_window_touch(state) {
            let reset_at =
                codex_core::slop_fork::account_rate_limits::quota_window(&stored, *kind).reset_at;
            codex_core::slop_fork::account_rate_limits::mark_quota_window_touch_confirmed(
                codex_home,
                account_id,
                plan,
                *kind,
                reset_at,
                confirmed_at,
            )?;
        }
    }
    Ok(())
}

pub(super) async fn touch_quota_windows(
    codex_home: &std::path::Path,
    account_id: &str,
    plan: Option<&str>,
    window_kinds: &[codex_core::slop_fork::account_rate_limits::QuotaWindowKind],
    client: &mut QuotaTouchClient,
) -> anyhow::Result<bool> {
    if window_kinds.is_empty() {
        return Ok(false);
    }

    let attempted_at = chrono::Utc::now();
    mark_quota_window_touch_attempts(codex_home, account_id, plan, window_kinds, attempted_at)?;

    let touch_models = client.list_touch_model_choices(account_id).await;
    let mut last_error = None;
    for touch_model in touch_models {
        let request =
            minimal_touch_response_request(&touch_model.model, touch_model.reasoning_effort);
        match client.create_response(&request).await {
            Ok(()) => {
                persist_touched_quota_snapshot(
                    codex_home,
                    account_id,
                    plan,
                    window_kinds,
                    client.get_detailed_rate_limits_many().await?,
                )?;
                return Ok(true);
            }
            Err(err) => {
                if client.is_saved_account() {
                    tracing::debug!(
                        account_id,
                        model = touch_model.model,
                        reasoning_effort = ?touch_model.reasoning_effort,
                        error = ?err,
                        "failed to touch cached quota window for saved account"
                    );
                } else {
                    tracing::debug!(
                        account_id,
                        model = touch_model.model,
                        reasoning_effort = ?touch_model.reasoning_effort,
                        error = ?err,
                        "failed to touch cached quota window"
                    );
                }
                last_error = Some(err);
            }
        }
    }

    let last_error = last_error
        .map(|err| err.to_string())
        .unwrap_or_else(|| "all touch models failed".to_string());
    anyhow::bail!("{last_error}")
}

fn touch_model_info_order(left: &ModelInfo, right: &ModelInfo) -> Ordering {
    touch_model_cost_rank(left)
        .cmp(&touch_model_cost_rank(right))
        .then_with(|| left.priority.cmp(&right.priority))
        .then_with(|| left.slug.cmp(&right.slug))
}

fn lowest_supported_reasoning_effort(model: &ModelInfo) -> Option<ReasoningEffort> {
    model
        .supported_reasoning_levels
        .iter()
        .map(|preset| preset.effort)
        .min_by_key(|effort| reasoning_effort_rank(*effort))
}

fn touch_model_cost_rank(model: &ModelInfo) -> u8 {
    let normalized = format!("{} {}", model.slug, model.display_name).to_ascii_lowercase();
    if normalized.contains("nano") {
        return 0;
    }
    if normalized.contains("mini") {
        return 1;
    }
    if normalized.contains("small") {
        return 2;
    }
    3
}

const fn reasoning_effort_rank(effort: ReasoningEffort) -> u8 {
    match effort {
        ReasoningEffort::None => 0,
        ReasoningEffort::Minimal => 1,
        ReasoningEffort::Low => 2,
        ReasoningEffort::Medium => 3,
        ReasoningEffort::High => 4,
        ReasoningEffort::XHigh => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::TouchModelChoice;
    use super::minimal_touch_response_request;
    use super::should_confirm_quota_window_touch;
    use super::touch_model_choices_from_model_infos;
    use codex_core::slop_fork::account_rate_limits::QuotaWindowState;
    use codex_protocol::openai_models::ConfigShellToolType;
    use codex_protocol::openai_models::ModelInfo;
    use codex_protocol::openai_models::ModelVisibility;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_protocol::openai_models::ReasoningEffortPreset;
    use codex_protocol::openai_models::TruncationPolicyConfig;
    use pretty_assertions::assert_eq;

    #[test]
    fn minimal_touch_request_omits_reasoning_when_no_supported_effort_is_known() {
        let request =
            minimal_touch_response_request("gpt-5.1-codex-mini", /*reasoning_effort*/ None);
        assert_eq!(request["stream"], serde_json::Value::Bool(true));
        assert_eq!(request["store"], serde_json::Value::Bool(false));
        assert!(request.get("reasoning").is_none());
    }

    #[test]
    fn minimal_touch_request_uses_selected_reasoning_effort() {
        let request =
            minimal_touch_response_request("gpt-5.1-codex-mini", Some(ReasoningEffort::Minimal));
        assert_eq!(
            request["reasoning"]["effort"],
            serde_json::Value::String("minimal".into())
        );
    }

    #[test]
    fn quota_touch_confirmation_accepts_lagging_untouched_state() {
        assert!(should_confirm_quota_window_touch(QuotaWindowState::Started));
        assert!(should_confirm_quota_window_touch(
            QuotaWindowState::Untouched
        ));
        assert!(!should_confirm_quota_window_touch(
            QuotaWindowState::Unknown
        ));
        assert!(!should_confirm_quota_window_touch(
            QuotaWindowState::ResetPassed
        ));
    }

    #[test]
    fn touch_model_choices_prefer_cheaper_models_and_lowest_supported_effort() {
        let models = vec![
            model_info(
                "gpt-5-mini",
                vec![ReasoningEffortPreset {
                    effort: ReasoningEffort::Low,
                    description: "low".to_string(),
                }],
            ),
            model_info(
                "gpt-5-codex",
                vec![
                    ReasoningEffortPreset {
                        effort: ReasoningEffort::Medium,
                        description: "medium".to_string(),
                    },
                    ReasoningEffortPreset {
                        effort: ReasoningEffort::Low,
                        description: "low".to_string(),
                    },
                ],
            ),
            model_info(
                "gpt-5.1-codex-mini",
                vec![
                    ReasoningEffortPreset {
                        effort: ReasoningEffort::Minimal,
                        description: "minimal".to_string(),
                    },
                    ReasoningEffortPreset {
                        effort: ReasoningEffort::Low,
                        description: "low".to_string(),
                    },
                ],
            ),
            model_info("gpt-5.1-codex-nano", vec![]),
        ];

        assert_eq!(
            touch_model_choices_from_model_infos(&models),
            vec![
                TouchModelChoice {
                    model: "gpt-5.1-codex-nano".to_string(),
                    reasoning_effort: None,
                },
                TouchModelChoice {
                    model: "gpt-5-mini".to_string(),
                    reasoning_effort: Some(ReasoningEffort::Low),
                },
                TouchModelChoice {
                    model: "gpt-5.1-codex-mini".to_string(),
                    reasoning_effort: Some(ReasoningEffort::Minimal),
                },
                TouchModelChoice {
                    model: "gpt-5-codex".to_string(),
                    reasoning_effort: Some(ReasoningEffort::Low),
                },
            ]
        );
    }

    fn model_info(
        model: &str,
        supported_reasoning_levels: Vec<ReasoningEffortPreset>,
    ) -> ModelInfo {
        ModelInfo {
            slug: model.to_string(),
            display_name: model.to_string(),
            description: None,
            default_reasoning_level: Some(ReasoningEffort::Medium),
            supported_reasoning_levels,
            shell_type: ConfigShellToolType::Default,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: 0,
            additional_speed_tiers: Vec::new(),
            availability_nux: None,
            upgrade: None,
            base_instructions: String::new(),
            model_messages: None,
            supports_reasoning_summaries: false,
            default_reasoning_summary: Default::default(),
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            web_search_tool_type: Default::default(),
            truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
            supports_parallel_tool_calls: false,
            supports_image_detail_original: false,
            context_window: None,
            auto_compact_token_limit: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: codex_protocol::openai_models::default_input_modalities(),
            used_fallback_model_metadata: false,
            supports_search_tool: false,
        }
    }
}
