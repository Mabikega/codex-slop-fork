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
use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
use crate::slop_fork::autoresearch::AutoresearchApproachStatus;
use crate::slop_fork::autoresearch::AutoresearchJournal;
use crate::slop_fork::autoresearch::AutoresearchRuntime;
use crate::slop_fork::autoresearch::load_validation_policy_settings;
use crate::slop_fork::autoresearch::refresh_playbook_artifact;
use crate::slop_fork::autoresearch::validation_gate_for_status;

pub(crate) static AUTORESEARCH_LOG_APPROACH_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let string_array = JsonSchema::array(JsonSchema::string(None), None);
    let properties = BTreeMap::from([
            (
                "approach_id".to_string(),
                JsonSchema::string(Some(
                    "Optional existing approach id. Omit to create a new tracked approach."
                        .to_string(),
                )),
            ),
            (
                "title".to_string(),
                JsonSchema::string(Some("Short human-readable approach title.".to_string())),
            ),
            (
                "family".to_string(),
                JsonSchema::string(Some(
                    "Coarse approach family such as ctc, seq2seq, distillation, or retrieval."
                        .to_string(),
                )),
            ),
            (
                "status".to_string(),
                JsonSchema::string(Some(
                    "One of proposed, planned, active, tested, promising, dead_end, winner, or archived."
                        .to_string(),
                )),
            ),
            (
                "summary".to_string(),
                JsonSchema::string(Some(
                    "Concise summary of the current approach state.".to_string(),
                )),
            ),
            (
                "rationale".to_string(),
                JsonSchema::string(Some(
                    "Why this approach is worth keeping in the portfolio.".to_string(),
                )),
            ),
            ("risks".to_string(), string_array.clone()),
            ("sources".to_string(), string_array),
            (
                "parent_approach_id".to_string(),
                JsonSchema::string(Some(
                    "Optional parent approach id when this is a derivative.".to_string(),
                )),
            ),
            (
                "synthesis_parent_approach_ids".to_string(),
                JsonSchema::array(
                    JsonSchema::string(None),
                    Some(
                        "Optional parent pair when this candidate is a synthesized branch."
                            .to_string(),
                    ),
                ),
            ),
        ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "autoresearch_log_approach".to_string(),
        description: "Create or update a tracked research approach in the autoresearch portfolio."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec![
                "title".to_string(),
                "family".to_string(),
                "status".to_string(),
                "summary".to_string(),
            ]),
            Some(false.into()),
        ),
        output_schema: None,
    })
});

pub(crate) fn register_approach_tools(builder: &mut ToolRegistryBuilder, code_mode_enabled: bool) {
    builder.push_spec(augment_tool_spec_for_code_mode(
        AUTORESEARCH_LOG_APPROACH_TOOL.clone(),
        code_mode_enabled,
    ));
    builder.register_handler(
        "autoresearch_log_approach",
        Arc::new(AutoresearchLogApproachHandler),
    );
}

pub(crate) struct AutoresearchLogApproachHandler;

#[derive(Debug, Deserialize)]
struct LogApproachArgs {
    #[serde(default)]
    approach_id: Option<String>,
    title: String,
    family: String,
    status: String,
    summary: String,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    parent_approach_id: Option<String>,
    #[serde(default)]
    synthesis_parent_approach_ids: Option<Vec<String>>,
}

impl ToolHandler for AutoresearchLogApproachHandler {
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
                "autoresearch_log_approach received unsupported payload".to_string(),
            ));
        };
        let args: LogApproachArgs = super::parse_arguments(&arguments)?;
        let status = AutoresearchApproachStatus::parse(args.status.trim()).ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "unknown approach status; expected proposed, planned, active, tested, promising, dead_end, winner, or archived"
                    .to_string(),
            )
        })?;
        let thread_id = session.conversation_id.to_string();
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        if !state.mode.is_open_ended() {
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_log_approach is only available in research or scientist mode"
                    .to_string(),
            ));
        }
        let mut runtime = AutoresearchRuntime::load(&turn.config.codex_home, &thread_id)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let approach_id = match args
            .approach_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(approach_id) => approach_id.to_string(),
            None => runtime
                .allocate_approach_id()
                .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?,
        };
        let mut journal = AutoresearchJournal::load(&state.workdir)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let summary = journal.summary();
        let segment = summary.current_segment;
        let existing_approach = summary
            .latest_approach(&approach_id)
            .map(|summary| summary.latest.clone());
        let validation_policy = load_validation_policy_settings(&state.workdir);
        let gate = validation_gate_for_status(&summary, &approach_id, status, &validation_policy);
        if !gate.allows_promotion() {
            return Err(FunctionCallError::RespondToModel(format!(
                "cannot mark approach {} as {} yet: {}",
                approach_id,
                status.as_str(),
                gate.unmet_requirements.join("; ")
            )));
        }
        let (parent_approach_id, synthesis_parent_approach_ids) = resolve_approach_lineage(
            args.parent_approach_id,
            args.synthesis_parent_approach_ids,
            existing_approach.as_ref(),
        )?;
        let entry = journal
            .append_approach(AutoresearchApproachEntry {
                entry_type: "approach".to_string(),
                approach_id: approach_id.clone(),
                title: args.title.trim().to_string(),
                family: args.family.trim().to_string(),
                status,
                summary: args.summary.trim().to_string(),
                rationale: args.rationale.trim().to_string(),
                risks: trim_list(args.risks),
                sources: trim_list(args.sources),
                parent_approach_id,
                synthesis_parent_approach_ids,
                timestamp: Local::now().timestamp(),
                segment,
            })
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        runtime
            .note_approach_status(&approach_id, status)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let updated_summary = journal.summary();
        refresh_playbook_artifact(
            &state.workdir,
            &state.goal,
            state.mode,
            &updated_summary,
            match status {
                AutoresearchApproachStatus::Active | AutoresearchApproachStatus::Winner => {
                    Some(approach_id.as_str())
                }
                _ => state.active_approach_id.as_deref(),
            },
        )
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        Ok(FunctionToolOutput::from_text(
            format!(
                "Logged approach {}.\nFamily: {}\nStatus: {}\nSummary: {}",
                entry.approach_id,
                entry.family,
                entry.status.as_str(),
                entry.summary
            ),
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

fn resolve_approach_lineage(
    parent_approach_id: Option<String>,
    synthesis_parent_approach_ids: Option<Vec<String>>,
    existing_approach: Option<&AutoresearchApproachEntry>,
) -> Result<(Option<String>, Vec<String>), FunctionCallError> {
    let explicit_synthesis_parent_approach_ids = synthesis_parent_approach_ids
        .map(trim_list)
        .filter(|values| !values.is_empty());
    let synthesis_parent_approach_ids = explicit_synthesis_parent_approach_ids
        .clone()
        .or_else(|| {
            existing_approach
                .map(|approach| approach.synthesis_parent_approach_ids.clone())
                .filter(|values| !values.is_empty())
        })
        .unwrap_or_default();
    if !synthesis_parent_approach_ids.is_empty() && synthesis_parent_approach_ids.len() != 2 {
        return Err(FunctionCallError::RespondToModel(
            "synthesis_parent_approach_ids must contain exactly two approach ids".to_string(),
        ));
    }
    let parent_approach_id = parent_approach_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            explicit_synthesis_parent_approach_ids
                .as_ref()
                .and_then(|values| values.first().cloned())
        })
        .or_else(|| existing_approach.and_then(|approach| approach.parent_approach_id.clone()))
        .or_else(|| synthesis_parent_approach_ids.first().cloned());
    Ok((parent_approach_id, synthesis_parent_approach_ids))
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
    fn resolve_approach_lineage_preserves_existing_synthesis_parents() {
        let existing_approach = AutoresearchApproachEntry {
            entry_type: "approach".to_string(),
            approach_id: "approach-4".to_string(),
            title: "synthesis".to_string(),
            family: "retrieval+distillation".to_string(),
            status: AutoresearchApproachStatus::Planned,
            summary: "controller-created".to_string(),
            rationale: String::new(),
            risks: Vec::new(),
            sources: Vec::new(),
            parent_approach_id: Some("approach-2".to_string()),
            synthesis_parent_approach_ids: vec!["approach-2".to_string(), "approach-3".to_string()],
            timestamp: 1,
            segment: 0,
        };

        let (parent_approach_id, synthesis_parent_approach_ids) = resolve_approach_lineage(
            /*parent_approach_id*/ None,
            /*synthesis_parent_approach_ids*/ None,
            Some(&existing_approach),
        )
        .expect("resolve lineage");

        assert_eq!(parent_approach_id.as_deref(), Some("approach-2"));
        assert_eq!(
            synthesis_parent_approach_ids,
            vec!["approach-2".to_string(), "approach-3".to_string()]
        );
    }

    #[test]
    fn resolve_approach_lineage_rejects_non_pair_synthesis_parents() {
        let err = resolve_approach_lineage(
            /*parent_approach_id*/ None,
            Some(vec!["approach-1".to_string()]),
            /*existing_approach*/ None,
        )
        .expect_err("expected invalid synthesis parent count");

        assert_eq!(
            err.to_string(),
            "synthesis_parent_approach_ids must contain exactly two approach ids"
        );
    }

    #[test]
    fn resolve_approach_lineage_replaces_parent_when_synthesis_pair_changes() {
        let existing_approach = AutoresearchApproachEntry {
            entry_type: "approach".to_string(),
            approach_id: "approach-4".to_string(),
            title: "synthesis".to_string(),
            family: "retrieval+distillation".to_string(),
            status: AutoresearchApproachStatus::Planned,
            summary: "controller-created".to_string(),
            rationale: String::new(),
            risks: Vec::new(),
            sources: Vec::new(),
            parent_approach_id: Some("approach-2".to_string()),
            synthesis_parent_approach_ids: vec!["approach-2".to_string(), "approach-3".to_string()],
            timestamp: 1,
            segment: 0,
        };

        let (parent_approach_id, synthesis_parent_approach_ids) = resolve_approach_lineage(
            /*parent_approach_id*/ None,
            Some(vec!["approach-5".to_string(), "approach-6".to_string()]),
            Some(&existing_approach),
        )
        .expect("resolve lineage");

        assert_eq!(parent_approach_id.as_deref(), Some("approach-5"));
        assert_eq!(
            synthesis_parent_approach_ids,
            vec!["approach-5".to_string(), "approach-6".to_string()]
        );
    }
}
