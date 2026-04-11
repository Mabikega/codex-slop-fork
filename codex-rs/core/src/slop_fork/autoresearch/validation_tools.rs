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
use crate::slop_fork::autoresearch::AutoresearchJournal;
use crate::slop_fork::autoresearch::AutoresearchValidationEntry;
use crate::slop_fork::autoresearch::AutoresearchValidationOutcome;
use crate::slop_fork::autoresearch::AutoresearchValidationType;
use crate::slop_fork::autoresearch::refresh_playbook_artifact;

pub(crate) static AUTORESEARCH_LOG_VALIDATION_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let string_array = JsonSchema::array(JsonSchema::string(None), None);
    let properties = BTreeMap::from([
        (
            "approach_id".to_string(),
            JsonSchema::string(Some(
                "Tracked approach id the validation applies to.".to_string(),
            )),
        ),
        (
            "validation_type".to_string(),
            JsonSchema::string(Some(
                "Validation type: rerun, holdout, adversarial, or evaluator_audit.".to_string(),
            )),
        ),
        (
            "outcome".to_string(),
            JsonSchema::string(Some(
                "Validation outcome: pass, fail, or mixed.".to_string(),
            )),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some(
                "Concise summary of what the validation showed.".to_string(),
            )),
        ),
        ("evidence".to_string(), string_array),
        (
            "metrics".to_string(),
            JsonSchema::object(BTreeMap::new(), None, Some(JsonSchema::number(None).into())),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "autoresearch_log_validation".to_string(),
        description:
            "Record a validation step for a tracked research/scientist approach so promotion and wrap-up can rely on explicit evidence."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec![
                "approach_id".to_string(),
                "validation_type".to_string(),
                "outcome".to_string(),
                "summary".to_string(),
            ]),
            Some(false.into()),
        ),
        output_schema: None,
    })
});

pub(crate) fn register_validation_tools(
    builder: &mut ToolRegistryBuilder,
    code_mode_enabled: bool,
) {
    builder.push_spec(augment_tool_spec_for_code_mode(
        AUTORESEARCH_LOG_VALIDATION_TOOL.clone(),
        code_mode_enabled,
    ));
    builder.register_handler(
        "autoresearch_log_validation",
        Arc::new(AutoresearchLogValidationHandler),
    );
}

pub(crate) struct AutoresearchLogValidationHandler;

#[derive(Debug, Deserialize)]
struct LogValidationArgs {
    approach_id: String,
    validation_type: String,
    outcome: String,
    summary: String,
    #[serde(default)]
    evidence: Vec<String>,
    #[serde(default)]
    metrics: BTreeMap<String, f64>,
}

impl ToolHandler for AutoresearchLogValidationHandler {
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
                "autoresearch_log_validation received unsupported payload".to_string(),
            ));
        };
        let args: LogValidationArgs = super::parse_arguments(&arguments)?;
        let validation_type =
            AutoresearchValidationType::parse(args.validation_type.trim()).ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "unknown validation_type; expected rerun, holdout, adversarial, or evaluator_audit"
                        .to_string(),
                )
            })?;
        let outcome =
            AutoresearchValidationOutcome::parse(args.outcome.trim()).ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "unknown outcome; expected pass, fail, or mixed".to_string(),
                )
            })?;
        let thread_id = session.conversation_id.to_string();
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        if !state.mode.is_open_ended() {
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_log_validation is only available in research or scientist mode"
                    .to_string(),
            ));
        }

        let mut journal = AutoresearchJournal::load(&state.workdir)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        if journal
            .summary()
            .latest_approach(args.approach_id.trim())
            .is_none()
        {
            return Err(FunctionCallError::RespondToModel(format!(
                "unknown approach_id {}; log the approach first",
                args.approach_id.trim()
            )));
        }
        let segment = journal.summary().current_segment;
        let entry = journal
            .append_validation(AutoresearchValidationEntry {
                entry_type: "validation".to_string(),
                approach_id: args.approach_id.trim().to_string(),
                validation_type,
                outcome,
                summary: args.summary.trim().to_string(),
                evidence: trim_list(args.evidence),
                metrics: args.metrics,
                timestamp: Local::now().timestamp(),
                segment,
            })
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
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
            format!(
                "Logged {} validation for {}.\nOutcome: {}\nSummary: {}",
                entry.validation_type.as_str(),
                entry.approach_id,
                entry.outcome.as_str(),
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
