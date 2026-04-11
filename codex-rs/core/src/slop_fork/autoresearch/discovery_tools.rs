use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::LazyLock;

use chrono::Local;
use serde::Deserialize;

use super::FunctionCallError;
use super::FunctionToolOutput;
use super::JsonSchema;
use super::ResponsesApiTool;
use super::ToolHandler;
use super::ToolInvocation;
use super::ToolKind;
use super::ToolPayload;
use super::ToolRegistryBuilder;
use super::ToolSpec;
use super::augment_tool_spec_for_code_mode;
use super::load_active_state;
use crate::slop_fork::autoresearch::AutoresearchDiscoveryEntry;
use crate::slop_fork::autoresearch::AutoresearchDiscoveryReason;
use crate::slop_fork::autoresearch::AutoresearchJournal;
use crate::slop_fork::autoresearch::AutoresearchRunState;
use crate::slop_fork::autoresearch::AutoresearchRuntime;
use crate::slop_fork::autoresearch::refresh_playbook_artifact;

pub(crate) static AUTORESEARCH_REQUEST_DISCOVERY_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let properties = BTreeMap::from([
            (
                "reason".to_string(),
                JsonSchema::string(Some(
                    "Why a bounded discovery pass is needed. One of plateau, stage_complete, weak_assumption, architecture_search, portfolio_refresh, evaluation_gap, follow_up, or user_requested."
                        .to_string(),
                )),
            ),
            (
                "focus".to_string(),
                JsonSchema::string(Some("Optional short focus for the discovery pass.".to_string())),
            ),
        ]);
    ToolSpec::Function(ResponsesApiTool {
            name: "autoresearch_request_discovery".to_string(),
            description:
                "Queue one bounded discovery pass for the next autoresearch cycle so the controller can audit the repo and do targeted external research. Only stop the current cycle when this returns a queued/success result; do not call autoresearch_log_discovery until the dedicated discovery cycle is active."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["reason".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
});

pub(crate) static AUTORESEARCH_LOG_DISCOVERY_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let string_array = JsonSchema::array(JsonSchema::string(None), None);
    let properties = BTreeMap::from([
        (
            "summary".to_string(),
            JsonSchema::string(Some("Concise synthesis of the discovery pass.".to_string())),
        ),
        ("recommendations".to_string(), string_array.clone()),
        ("unknowns".to_string(), string_array.clone()),
        ("sources".to_string(), string_array.clone()),
        ("dead_ends".to_string(), string_array),
    ]);
    ToolSpec::Function(ResponsesApiTool {
            name: "autoresearch_log_discovery".to_string(),
            description:
                "Record the result of an active bounded discovery pass in the autoresearch journal. Do not call this in the cycle that only queued discovery. After it succeeds, stop the turn immediately and do not run more tools or re-sync the repo."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                properties,
                Some(vec!["summary".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
});

pub(crate) fn register_discovery_tools(builder: &mut ToolRegistryBuilder, code_mode_enabled: bool) {
    for spec in [
        AUTORESEARCH_REQUEST_DISCOVERY_TOOL.clone(),
        AUTORESEARCH_LOG_DISCOVERY_TOOL.clone(),
    ] {
        builder.push_spec(augment_tool_spec_for_code_mode(spec, code_mode_enabled));
    }
    builder.register_handler(
        "autoresearch_request_discovery",
        Arc::new(AutoresearchRequestDiscoveryHandler),
    );
    builder.register_handler(
        "autoresearch_log_discovery",
        Arc::new(AutoresearchLogDiscoveryHandler),
    );
}

pub(crate) struct AutoresearchRequestDiscoveryHandler;
pub(crate) struct AutoresearchLogDiscoveryHandler;

#[derive(Debug, Deserialize)]
struct RequestDiscoveryArgs {
    reason: String,
    #[serde(default)]
    focus: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LogDiscoveryArgs {
    summary: String,
    #[serde(default)]
    recommendations: Vec<String>,
    #[serde(default)]
    unknowns: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    dead_ends: Vec<String>,
}

fn logged_discovery_completion_message(reason: &str, summary: &str) -> String {
    format!(
        "Logged discovery.\n\
         Reason: {reason}\n\
         Summary: {summary}\n\
         Discovery is recorded for this cycle. Stop now with a concise final synthesis. Do not call more tools or re-sync the repo in this turn."
    )
}

fn already_logged_discovery_message() -> String {
    "Discovery is already logged for this active cycle. Stop now with a concise final synthesis. Do not call more tools or re-sync the repo in this turn.".to_string()
}

impl ToolHandler for AutoresearchRequestDiscoveryHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_request_discovery received unsupported payload".to_string(),
            ));
        };
        let args: RequestDiscoveryArgs = super::parse_arguments(&arguments)?;
        let reason = AutoresearchDiscoveryReason::parse(args.reason.trim()).ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "unknown discovery reason; expected plateau, stage_complete, weak_assumption, architecture_search, portfolio_refresh, evaluation_gap, follow_up, or user_requested"
                    .to_string(),
            )
        })?;
        let thread_id = session.conversation_id.to_string();
        let mut runtime = AutoresearchRuntime::load(&turn.config.codex_home, &thread_id)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let queued = runtime
            .request_discovery(reason, args.focus, Local::now())
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        if !queued {
            return Ok(FunctionToolOutput::from_text(
                blocked_discovery_request_message(runtime.state()),
                Some(false),
            ));
        }
        let summary = runtime
            .state()
            .and_then(|state| state.status_message.as_deref())
            .unwrap_or("Autoresearch queued a bounded discovery pass.");
        Ok(FunctionToolOutput::from_text(
            summary.to_string(),
            Some(true),
        ))
    }
}

impl ToolHandler for AutoresearchLogDiscoveryHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let ToolInvocation {
            session,
            turn,
            payload,
            ..
        } = invocation;
        let ToolPayload::Function { arguments } = payload else {
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_log_discovery received unsupported payload".to_string(),
            ));
        };
        let args: LogDiscoveryArgs = super::parse_arguments(&arguments)?;
        let thread_id = session.conversation_id.to_string();
        let mut runtime = AutoresearchRuntime::load(&turn.config.codex_home, &thread_id)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        if state.active_cycle_kind
            != Some(crate::slop_fork::autoresearch::AutoresearchCycleKind::Discovery)
        {
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_log_discovery is only valid during a discovery cycle".to_string(),
            ));
        }
        let Some(request) = state.active_discovery_request.clone() else {
            return Err(FunctionCallError::RespondToModel(
                "missing active discovery request".to_string(),
            ));
        };
        let logged = runtime
            .mark_discovery_logged()
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        if !logged {
            return Ok(FunctionToolOutput::from_text(
                already_logged_discovery_message(),
                Some(true),
            ));
        }
        let mut journal = AutoresearchJournal::load(&state.workdir).map_err(|err| {
            let rollback = runtime.clear_discovery_logged();
            match rollback {
                Ok(_) => FunctionCallError::RespondToModel(err.to_string()),
                Err(clear_err) => FunctionCallError::RespondToModel(format!(
                    "{err} (also failed to restore discovery logging state: {clear_err})"
                )),
            }
        })?;
        let segment = journal.summary().current_segment;
        let entry = journal
            .append_discovery(AutoresearchDiscoveryEntry {
                entry_type: "discovery".to_string(),
                reason: request.reason,
                focus: request.focus,
                summary: args.summary.trim().to_string(),
                recommendations: trim_list(args.recommendations),
                unknowns: trim_list(args.unknowns),
                sources: trim_list(args.sources),
                dead_ends: trim_list(args.dead_ends),
                timestamp: Local::now().timestamp(),
                segment,
            })
            .map_err(|err| {
                let rollback = runtime.clear_discovery_logged();
                match rollback {
                    Ok(_) => FunctionCallError::RespondToModel(err.to_string()),
                    Err(clear_err) => FunctionCallError::RespondToModel(format!(
                        "{err} (also failed to restore discovery logging state: {clear_err})"
                    )),
                }
            })?;
        let updated_summary = journal.summary();
        refresh_playbook_artifact(
            &state.workdir,
            &state.goal,
            state.mode,
            &updated_summary,
            state.active_approach_id.as_deref(),
        )
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(FunctionToolOutput::from_text(
            logged_discovery_completion_message(entry.reason.label(), &entry.summary),
            Some(true),
        ))
    }
}

fn trim_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

fn blocked_discovery_request_message(state: Option<&AutoresearchRunState>) -> String {
    let Some(state) = state else {
        return "Autoresearch discovery is unavailable because there is no active session."
            .to_string();
    };
    if state.queued_discovery_request.is_some()
        || state.pending_cycle_kind
            == Some(crate::slop_fork::autoresearch::AutoresearchCycleKind::Discovery)
    {
        return "Autoresearch already has a bounded discovery pass queued for the next cycle. Stop this cycle and let that queued discovery run; do not call autoresearch_log_discovery until the discovery cycle is active.".to_string();
    }
    if state.active_discovery_request.is_some()
        || state.active_cycle_kind
            == Some(crate::slop_fork::autoresearch::AutoresearchCycleKind::Discovery)
    {
        return "Autoresearch is already in a bounded discovery cycle. Finish that cycle and call autoresearch_log_discovery there instead of requesting another discovery pass.".to_string();
    }
    if state.wrap_up_requested
        || state.pending_cycle_kind
            == Some(crate::slop_fork::autoresearch::AutoresearchCycleKind::WrapUp)
        || state.active_cycle_kind
            == Some(crate::slop_fork::autoresearch::AutoresearchCycleKind::WrapUp)
    {
        return "Autoresearch is already wrapping up, so it cannot queue another discovery pass."
            .to_string();
    }
    if state.max_runs.is_some_and(|max_runs| {
        (matches!(
            state.active_cycle_kind,
            Some(
                crate::slop_fork::autoresearch::AutoresearchCycleKind::Continue
                    | crate::slop_fork::autoresearch::AutoresearchCycleKind::Research
            )
        ) || (state.active_cycle_kind.is_none()
            && matches!(
                state.pending_cycle_kind,
                Some(
                    crate::slop_fork::autoresearch::AutoresearchCycleKind::Continue
                        | crate::slop_fork::autoresearch::AutoresearchCycleKind::Research
                )
            )))
            && state.iteration_count.saturating_add(1) >= max_runs
    }) {
        return "Autoresearch cannot queue discovery because the current exploit cycle is already the last allowed run and the session will wrap up next.".to_string();
    }
    if state
        .max_runs
        .is_some_and(|max_runs| state.iteration_count >= max_runs)
    {
        return "Autoresearch cannot queue discovery because the session has already reached its max-run limit and will wrap up instead.".to_string();
    }
    state
        .status_message
        .clone()
        .unwrap_or_else(|| {
            "Autoresearch discovery requires an active session that is not already wrapping up and has no other discovery pass queued or running".to_string()
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn trim_list_drops_blank_entries() {
        assert_eq!(
            trim_list(vec![
                " one ".to_string(),
                String::new(),
                "   ".to_string(),
                "two".to_string(),
            ]),
            vec!["one".to_string(), "two".to_string()]
        );
    }

    #[test]
    fn reason_parser_accepts_known_values() {
        assert_eq!(
            AutoresearchDiscoveryReason::parse("plateau"),
            Some(AutoresearchDiscoveryReason::Plateau)
        );
        assert_eq!(AutoresearchDiscoveryReason::parse("unknown"), None);
    }

    #[test]
    fn blocked_discovery_message_explains_queued_pass() {
        let state = AutoresearchRunState {
            queued_discovery_request: Some(
                crate::slop_fork::autoresearch::AutoresearchDiscoveryRequest {
                    reason: AutoresearchDiscoveryReason::Plateau,
                    focus: Some("frontier".to_string()),
                    requested_at: 1,
                },
            ),
            ..AutoresearchRunState::default()
        };
        assert_eq!(
            blocked_discovery_request_message(Some(&state)),
            "Autoresearch already has a bounded discovery pass queued for the next cycle. Stop this cycle and let that queued discovery run; do not call autoresearch_log_discovery until the discovery cycle is active."
        );
    }

    #[test]
    fn blocked_discovery_message_explains_last_allowed_research_cycle() {
        let state = AutoresearchRunState {
            active_cycle_kind: Some(
                crate::slop_fork::autoresearch::AutoresearchCycleKind::Research,
            ),
            iteration_count: 0,
            max_runs: Some(1),
            ..AutoresearchRunState::default()
        };

        assert_eq!(
            blocked_discovery_request_message(Some(&state)),
            "Autoresearch cannot queue discovery because the current exploit cycle is already the last allowed run and the session will wrap up next."
        );
    }

    #[test]
    fn logged_discovery_message_tells_model_to_stop() {
        let message = logged_discovery_completion_message("portfolio refresh", "refresh ranking");
        assert!(message.contains("Logged discovery."));
        assert!(message.contains("Stop now with a concise final synthesis."));
        assert!(message.contains("Do not call more tools or re-sync the repo"));
    }

    #[test]
    fn already_logged_discovery_message_tells_model_to_stop() {
        let message = already_logged_discovery_message();
        assert!(message.contains("already logged"));
        assert!(message.contains("Stop now with a concise final synthesis."));
        assert!(message.contains("Do not call more tools or re-sync the repo"));
    }
}
