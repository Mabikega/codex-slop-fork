use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
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
use super::augment_tool_spec_for_code_mode;
use super::load_active_state;
use crate::slop_fork::autoresearch::AutoresearchDiscoveryEntry;
use crate::slop_fork::autoresearch::AutoresearchDiscoveryReason;
use crate::slop_fork::autoresearch::AutoresearchJournal;
use crate::slop_fork::autoresearch::AutoresearchRuntime;

pub(crate) static AUTORESEARCH_REQUEST_DISCOVERY_TOOL: LazyLock<
    crate::client_common::tools::ToolSpec,
> = LazyLock::new(|| {
    let properties = BTreeMap::from([
            (
                "reason".to_string(),
                JsonSchema::String {
                    description: Some(
                        "Why a bounded discovery pass is needed. One of plateau, stage_complete, weak_assumption, architecture_search, evaluation_gap, follow_up, or user_requested."
                            .to_string(),
                    ),
                },
            ),
            (
                "focus".to_string(),
                JsonSchema::String {
                    description: Some(
                        "Optional short focus for the discovery pass.".to_string(),
                    ),
                },
            ),
        ]);
    crate::client_common::tools::ToolSpec::Function(ResponsesApiTool {
            name: "autoresearch_request_discovery".to_string(),
            description:
                "Queue one bounded discovery pass so autoresearch can audit the repo and do targeted external research before the next experiment cycle."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::Object {
                properties,
                required: Some(vec!["reason".to_string()]),
                additional_properties: Some(false.into()),
            },
            output_schema: None,
        })
});

pub(crate) static AUTORESEARCH_LOG_DISCOVERY_TOOL: LazyLock<crate::client_common::tools::ToolSpec> =
    LazyLock::new(|| {
        let string_array = JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: None,
        };
        let properties = BTreeMap::from([
            (
                "summary".to_string(),
                JsonSchema::String {
                    description: Some("Concise synthesis of the discovery pass.".to_string()),
                },
            ),
            ("recommendations".to_string(), string_array.clone()),
            ("unknowns".to_string(), string_array.clone()),
            ("sources".to_string(), string_array.clone()),
            ("dead_ends".to_string(), string_array),
        ]);
        crate::client_common::tools::ToolSpec::Function(ResponsesApiTool {
            name: "autoresearch_log_discovery".to_string(),
            description:
                "Record the result of a bounded discovery pass in the autoresearch journal."
                    .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::Object {
                properties,
                required: Some(vec!["summary".to_string()]),
                additional_properties: Some(false.into()),
            },
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

#[async_trait]
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
                "unknown discovery reason; expected plateau, stage_complete, weak_assumption, architecture_search, evaluation_gap, follow_up, or user_requested"
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
            return Err(FunctionCallError::RespondToModel(
                "autoresearch discovery requires an active session that is not already wrapping up and has no other discovery pass queued or running".to_string(),
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

#[async_trait]
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
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_log_discovery can only be called once during an active discovery cycle".to_string(),
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
        let text = format!(
            "Logged discovery.\nReason: {}\nSummary: {}",
            entry.reason.label(),
            entry.summary
        );
        Ok(FunctionToolOutput::from_text(text, Some(true)))
    }
}

fn trim_list(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
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
}
