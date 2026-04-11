use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::LazyLock;

use futures::future::join_all;
use serde::Deserialize;
use uuid::Uuid;

use super::AutoresearchParallelWorkspaceManager;
use super::JsonSchema;
use super::ResponsesApiTool;
use super::ToolHandler;
use super::ToolInvocation;
use super::ToolKind;
use super::ToolPayload;
use super::ToolRegistryBuilder;
use super::ToolSpec;
use super::augment_tool_spec_for_code_mode;
use super::enforce_autoresearch_script;
use super::ensure_experiment_cycle;
use super::execute_command;
use super::load_active_state;
use super::parse_arguments;
use super::parse_metric_lines;
use super::tail_lines;
use crate::function_tool::FunctionCallError;
use crate::slop_fork::autoresearch::AUTORESEARCH_CHECKS_FILE;
use crate::slop_fork::autoresearch::AutoresearchJournal;
use crate::slop_fork::autoresearch::AutoresearchResearchWorkspace;
use crate::slop_fork::autoresearch::AutoresearchRuntime;
use crate::slop_fork::autoresearch::PendingRunResult;

pub(crate) static AUTORESEARCH_RUN_PARALLEL_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let properties = std::collections::BTreeMap::from([
        (
            "command".to_string(),
            JsonSchema::string(Some(
                "Shell command to benchmark in each isolated candidate workspace.".to_string(),
            )),
        ),
        (
            "approach_ids".to_string(),
            JsonSchema::array(
                JsonSchema::string(None),
                Some(
                    "Tracked approach ids to benchmark in isolated candidate workspaces."
                        .to_string(),
                ),
            ),
        ),
        (
            "timeout_seconds".to_string(),
            JsonSchema::number(Some("Timeout for each benchmark command.".to_string())),
        ),
        (
            "checks_timeout_seconds".to_string(),
            JsonSchema::number(Some(
                "Timeout for autoresearch.checks.sh in each candidate workspace.".to_string(),
            )),
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "autoresearch_run_parallel".to_string(),
        description:
            "Run a bounded set of tracked approach snapshots in isolated candidate workspaces and return authenticated run tokens for later logging."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["command".to_string(), "approach_ids".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
});

pub(crate) fn register_parallel_tools(builder: &mut ToolRegistryBuilder, code_mode_enabled: bool) {
    builder.push_spec(augment_tool_spec_for_code_mode(
        AUTORESEARCH_RUN_PARALLEL_TOOL.clone(),
        code_mode_enabled,
    ));
    builder.register_handler(
        "autoresearch_run_parallel",
        Arc::new(AutoresearchRunParallelHandler),
    );
}

pub(crate) struct AutoresearchRunParallelHandler;

#[derive(Debug, Deserialize)]
struct RunParallelArgs {
    command: String,
    approach_ids: Vec<String>,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    checks_timeout_seconds: Option<u64>,
}

#[derive(Debug)]
struct ParallelRunOutcome {
    approach_id: String,
    pending_run: PendingRunResult,
}

struct ParallelRunConfig<'a> {
    benchmark_command: &'a str,
    primary_metric_name: &'a str,
    timeout_seconds: u64,
    checks_timeout_seconds: u64,
}

impl ToolHandler for AutoresearchRunParallelHandler {
    type Output = super::FunctionToolOutput;

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
                "autoresearch_run_parallel received unsupported payload".to_string(),
            ));
        };
        let args: RunParallelArgs = parse_arguments(&arguments)?;
        let thread_id = session.conversation_id.to_string();
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        if !state.mode.is_open_ended() {
            return Err(FunctionCallError::RespondToModel(
                "autoresearch_run_parallel is only available in research or scientist mode"
                    .to_string(),
            ));
        }
        ensure_experiment_cycle(&state, "autoresearch_run_parallel")?;

        let approach_ids = normalized_approach_ids(args.approach_ids)?;
        let workdir = state.workdir.clone();
        let summary = AutoresearchJournal::load(&workdir)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            .summary();
        let config = summary.config.as_ref().ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "autoresearch_run_parallel requires autoresearch_init to be called first"
                    .to_string(),
            )
        })?;
        for approach_id in &approach_ids {
            if summary.latest_approach(approach_id).is_none() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unknown approach_id {approach_id}; log the approach first"
                )));
            }
        }
        let benchmark_command = enforce_autoresearch_script(&workdir, &args.command)?;
        let research_workspace =
            AutoresearchResearchWorkspace::new(&turn.config.codex_home, &thread_id);
        let parallel_manager =
            AutoresearchParallelWorkspaceManager::new(&turn.config.codex_home, &thread_id);
        let workspace = state.workspace.as_ref().ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "missing autoresearch workspace state for parallel benchmarking".to_string(),
            )
        })?;

        let mut leases = Vec::new();
        for approach_id in &approach_ids {
            let lease = parallel_manager
                .prepare_candidate_workspace(
                    workspace,
                    &research_workspace,
                    approach_id,
                    &Uuid::new_v4().to_string(),
                )
                .map_err(FunctionCallError::RespondToModel)?;
            leases.push((approach_id.clone(), lease));
        }

        let timeout_seconds = args.timeout_seconds.unwrap_or(600);
        let checks_timeout_seconds = args.checks_timeout_seconds.unwrap_or(300);
        let run_config = ParallelRunConfig {
            benchmark_command: &benchmark_command,
            primary_metric_name: &config.metric_name,
            timeout_seconds,
            checks_timeout_seconds,
        };
        let run_futures = leases.iter().map(|(approach_id, lease)| async {
            run_parallel_candidate(
                session.as_ref(),
                turn.as_ref(),
                &run_config,
                approach_id,
                lease,
            )
            .await
        });
        let outcomes = join_all(run_futures).await;
        let mut pending_runs = Vec::new();
        let mut result_lines = Vec::new();
        for outcome in outcomes {
            match outcome {
                Ok(outcome) => {
                    result_lines.push(render_parallel_result_line(&outcome));
                    pending_runs.push(outcome.pending_run);
                }
                Err(err) => {
                    cleanup_parallel_leases(&parallel_manager, workspace, &leases);
                    return Err(err);
                }
            }
        }

        let mut runtime = AutoresearchRuntime::load(&turn.config.codex_home, &thread_id)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        runtime
            .store_pending_parallel_runs(pending_runs)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        Ok(super::FunctionToolOutput::from_text(
            format!(
                "Parallel candidate runs completed.\n{}",
                result_lines.join("\n")
            ),
            Some(true),
        ))
    }
}

async fn run_parallel_candidate(
    session: &crate::codex::Session,
    turn: &crate::codex::TurnContext,
    run_config: &ParallelRunConfig<'_>,
    approach_id: &str,
    lease: &crate::slop_fork::autoresearch::AutoresearchParallelWorkspaceLease,
) -> Result<ParallelRunOutcome, FunctionCallError> {
    let benchmark_output = execute_command(
        session,
        turn,
        &lease.workdir,
        run_config.benchmark_command,
        run_config.timeout_seconds,
    )
    .await?;
    let benchmark_text = super::aggregate_output_text(&benchmark_output);
    let parsed_metrics = parse_metric_lines(&benchmark_text);
    let parsed_primary = parsed_metrics.get(run_config.primary_metric_name).copied();
    let checks_path = lease.workdir.join(AUTORESEARCH_CHECKS_FILE);
    let mut checks_pass = None;
    let mut checks_output = String::new();
    if benchmark_output.exit_code == 0 && !benchmark_output.timed_out && checks_path.is_file() {
        let checks_command = format!("./{AUTORESEARCH_CHECKS_FILE}");
        let output = execute_command(
            session,
            turn,
            &lease.workdir,
            &checks_command,
            run_config.checks_timeout_seconds,
        )
        .await?;
        checks_pass = Some(output.exit_code == 0 && !output.timed_out);
        checks_output = tail_lines(
            &super::aggregate_output_text(&output),
            /*max_lines*/ 40,
        );
    }

    Ok(ParallelRunOutcome {
        approach_id: approach_id.to_string(),
        pending_run: PendingRunResult {
            token: Uuid::new_v4().to_string(),
            command: run_config.benchmark_command.to_string(),
            duration_seconds: benchmark_output.duration.as_secs_f64(),
            exit_code: Some(benchmark_output.exit_code),
            timed_out: benchmark_output.timed_out,
            passed: benchmark_output.exit_code == 0
                && !benchmark_output.timed_out
                && checks_pass.unwrap_or(true),
            checks_pass,
            checks_output,
            parsed_primary,
            parsed_metrics,
            output_tail: tail_lines(&benchmark_text, /*max_lines*/ 20),
            parallel_workspace: Some(lease.clone()),
        },
    })
}

fn cleanup_parallel_leases(
    manager: &AutoresearchParallelWorkspaceManager,
    workspace: &crate::slop_fork::autoresearch::AutoresearchWorkspace,
    leases: &[(
        String,
        crate::slop_fork::autoresearch::AutoresearchParallelWorkspaceLease,
    )],
) {
    for (_approach_id, lease) in leases {
        let _ = manager.clear_candidate(workspace, lease);
    }
}

fn normalized_approach_ids(values: Vec<String>) -> Result<Vec<String>, FunctionCallError> {
    let approach_ids = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if approach_ids.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "autoresearch_run_parallel requires at least one approach_id".to_string(),
        ));
    }
    let unique = approach_ids.iter().cloned().collect::<BTreeSet<_>>();
    if unique.len() != approach_ids.len() {
        return Err(FunctionCallError::RespondToModel(
            "autoresearch_run_parallel approach_ids must be unique".to_string(),
        ));
    }
    Ok(approach_ids)
}

fn render_parallel_result_line(outcome: &ParallelRunOutcome) -> String {
    let pending_run = &outcome.pending_run;
    let primary = pending_run
        .parsed_primary
        .map(super::format_metric)
        .unwrap_or_else(|| "n/a".to_string());
    let checks = match pending_run.checks_pass {
        Some(true) => "checks=pass",
        Some(false) => "checks=fail",
        None => "checks=n/a",
    };
    format!(
        "- {} token={} metric={} exit_code={} {}",
        outcome.approach_id,
        pending_run.token,
        primary,
        pending_run.exit_code.unwrap_or_default(),
        checks
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn normalized_approach_ids_rejects_duplicates() {
        let err = normalized_approach_ids(vec!["a".to_string(), "a".to_string()])
            .expect_err("duplicate approach ids should fail");

        assert_eq!(
            err.to_string(),
            "autoresearch_run_parallel approach_ids must be unique"
        );
    }
}
