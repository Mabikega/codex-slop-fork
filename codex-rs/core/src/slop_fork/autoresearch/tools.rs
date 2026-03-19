#[path = "discovery_tools.rs"]
mod discovery_tools;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::LazyLock;

use async_trait::async_trait;
use chrono::Local;
use serde::Deserialize;
use uuid::Uuid;

use crate::client_common::tools::ResponsesApiTool;
use crate::client_common::tools::ToolSpec;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec::ExecToolCallOutput;
use crate::exec::process_exec_tool_call;
use crate::exec_env::create_env;
use crate::function_tool::FunctionCallError;
use crate::sandboxing::SandboxPermissions;
use crate::tools::code_mode_description::augment_tool_spec_for_code_mode;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use crate::tools::registry::ToolRegistryBuilder;
use crate::tools::spec::JsonSchema;

use super::AUTORESEARCH_CHECKS_FILE;
use super::AUTORESEARCH_SCRIPT_FILE;
use super::AutoresearchExperimentEntry;
use super::AutoresearchExperimentStatus;
use super::AutoresearchJournal;
use super::AutoresearchRunState;
use super::AutoresearchRuntime;
use super::MetricDirection;
use super::PendingRunResult;
use super::load_stage_progress;

#[cfg(test)]
pub(crate) use discovery_tools::AUTORESEARCH_LOG_DISCOVERY_TOOL;
#[cfg(test)]
pub(crate) use discovery_tools::AUTORESEARCH_REQUEST_DISCOVERY_TOOL;

pub(crate) static AUTORESEARCH_INIT_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let properties = BTreeMap::from([
        (
            "name".to_string(),
            JsonSchema::String {
                description: Some("Human-readable session name.".to_string()),
            },
        ),
        (
            "metric_name".to_string(),
            JsonSchema::String {
                description: Some("Primary metric name emitted by the benchmark.".to_string()),
            },
        ),
        (
            "metric_unit".to_string(),
            JsonSchema::String {
                description: Some("Primary metric unit, such as s, ms, or kb.".to_string()),
            },
        ),
        (
            "direction".to_string(),
            JsonSchema::String {
                description: Some("Whether lower or higher values are better.".to_string()),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "autoresearch_init".to_string(),
        description: "Initialize or reinitialize the active autoresearch session.".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["name".to_string(), "metric_name".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
});

pub(crate) static AUTORESEARCH_RUN_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let properties = BTreeMap::from([
        (
            "command".to_string(),
            JsonSchema::String {
                description: Some("Shell command to benchmark.".to_string()),
            },
        ),
        (
            "timeout_seconds".to_string(),
            JsonSchema::Number {
                description: Some("Timeout for the benchmark command.".to_string()),
            },
        ),
        (
            "checks_timeout_seconds".to_string(),
            JsonSchema::Number {
                description: Some("Timeout for autoresearch.checks.sh when it exists.".to_string()),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "autoresearch_run".to_string(),
        description: "Run the active autoresearch benchmark and capture a pending result."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
});

pub(crate) static AUTORESEARCH_LOG_TOOL: LazyLock<ToolSpec> = LazyLock::new(|| {
    let properties = BTreeMap::from([
        (
            "run_token".to_string(),
            JsonSchema::String {
                description: Some("Token returned by autoresearch_run.".to_string()),
            },
        ),
        (
            "metric".to_string(),
            JsonSchema::Number {
                description: Some(
                    "Optional fallback primary metric for discard/crash logging when autoresearch_run could not parse one."
                        .to_string(),
                ),
            },
        ),
        (
            "status".to_string(),
            JsonSchema::String {
                description: Some("One of keep, discard, crash, checks_failed.".to_string()),
            },
        ),
        (
            "description".to_string(),
            JsonSchema::String {
                description: Some("Short description of the experiment.".to_string()),
            },
        ),
        (
            "metrics".to_string(),
            JsonSchema::Object {
                properties: BTreeMap::new(),
                required: None,
                additional_properties: Some(JsonSchema::Number { description: None }.into()),
            },
        ),
    ]);
    ToolSpec::Function(ResponsesApiTool {
        name: "autoresearch_log".to_string(),
        description: "Record the pending autoresearch run and keep or discard workspace changes."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![
                "run_token".to_string(),
                "status".to_string(),
                "description".to_string(),
            ]),
            additional_properties: Some(false.into()),
        },
        output_schema: None,
    })
});

pub(crate) fn register_autoresearch_tools(
    builder: &mut ToolRegistryBuilder,
    code_mode_enabled: bool,
) {
    for spec in [
        AUTORESEARCH_INIT_TOOL.clone(),
        AUTORESEARCH_RUN_TOOL.clone(),
        AUTORESEARCH_LOG_TOOL.clone(),
    ] {
        builder.push_spec(augment_tool_spec_for_code_mode(spec, code_mode_enabled));
    }
    discovery_tools::register_discovery_tools(builder, code_mode_enabled);
    builder.register_handler("autoresearch_init", Arc::new(AutoresearchInitHandler));
    builder.register_handler("autoresearch_run", Arc::new(AutoresearchRunHandler));
    builder.register_handler("autoresearch_log", Arc::new(AutoresearchLogHandler));
}

pub struct AutoresearchInitHandler;
pub struct AutoresearchRunHandler;
pub struct AutoresearchLogHandler;

#[derive(Debug, Deserialize)]
struct InitArgs {
    name: String,
    metric_name: String,
    #[serde(default)]
    metric_unit: String,
    #[serde(default)]
    direction: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunArgs {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
    #[serde(default)]
    checks_timeout_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct LogArgs {
    run_token: String,
    #[serde(default)]
    metric: Option<f64>,
    status: String,
    description: String,
    #[serde(default)]
    metrics: BTreeMap<String, f64>,
}

#[derive(Debug, PartialEq)]
struct ResolvedLoggedRun {
    metric: Option<f64>,
    metrics: BTreeMap<String, f64>,
    result_json: String,
}

#[async_trait]
impl ToolHandler for AutoresearchInitHandler {
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
                "autoresearch_init received unsupported payload".to_string(),
            ));
        };
        let args: InitArgs = parse_arguments(&arguments)?;
        let direction = match args.direction.as_deref() {
            Some("higher") => MetricDirection::Higher,
            Some("lower") | None => MetricDirection::Lower,
            Some(other) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "unknown direction {other}; expected lower or higher"
                )));
            }
        };
        let thread_id = session.conversation_id.to_string();
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        ensure_experiment_cycle(&state, "autoresearch_init")?;
        let workdir = state.workdir;
        let mut journal = AutoresearchJournal::load(&workdir)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let entry = journal
            .append_config(args.name, args.metric_name, args.metric_unit, direction)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let content = format!(
            "Autoresearch initialized.\nName: {}\nMetric: {} ({}, {:?} is better)",
            entry.name,
            entry.metric_name,
            if entry.metric_unit.is_empty() {
                "unitless"
            } else {
                entry.metric_unit.as_str()
            },
            entry.direction
        );
        let _ = turn;
        Ok(FunctionToolOutput::from_text(content, Some(true)))
    }
}

#[async_trait]
impl ToolHandler for AutoresearchRunHandler {
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
                "autoresearch_run received unsupported payload".to_string(),
            ));
        };
        let args: RunArgs = parse_arguments(&arguments)?;
        let thread_id = session.conversation_id.to_string();
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        ensure_experiment_cycle(&state, "autoresearch_run")?;
        let workdir = state.workdir.clone();
        let summary = AutoresearchJournal::load(&workdir)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?
            .summary();
        let config = summary.config.ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "autoresearch_run requires autoresearch_init to be called first".to_string(),
            )
        })?;
        let benchmark_command = enforce_autoresearch_script(&workdir, &args.command)?;
        let benchmark_output = execute_command(
            session.as_ref(),
            turn.as_ref(),
            &workdir,
            &benchmark_command,
            args.timeout_seconds.unwrap_or(600),
        )
        .await?;

        let benchmark_text = aggregate_output_text(&benchmark_output);
        let parsed_metrics = parse_metric_lines(&benchmark_text);
        let parsed_primary = parsed_metrics.get(&config.metric_name).copied();
        let checks_path = workdir.join(AUTORESEARCH_CHECKS_FILE);
        let mut checks_pass = None;
        let mut checks_output = String::new();
        if benchmark_output.exit_code == 0 && !benchmark_output.timed_out && checks_path.is_file() {
            let checks_command = format!("./{AUTORESEARCH_CHECKS_FILE}");
            let output = execute_command(
                session.as_ref(),
                turn.as_ref(),
                &workdir,
                &checks_command,
                args.checks_timeout_seconds.unwrap_or(300),
            )
            .await?;
            checks_pass = Some(output.exit_code == 0 && !output.timed_out);
            checks_output = tail_lines(&aggregate_output_text(&output), 40);
        }

        let pending_run = PendingRunResult {
            token: Uuid::new_v4().to_string(),
            command: benchmark_command.clone(),
            duration_seconds: benchmark_output.duration.as_secs_f64(),
            exit_code: Some(benchmark_output.exit_code),
            timed_out: benchmark_output.timed_out,
            passed: benchmark_output.exit_code == 0
                && !benchmark_output.timed_out
                && checks_pass.unwrap_or(true),
            checks_pass,
            checks_output: checks_output.clone(),
            parsed_primary,
            parsed_metrics: parsed_metrics.clone(),
            output_tail: tail_lines(&benchmark_text, 20),
        };
        let mut runtime = AutoresearchRuntime::load(&turn.config.codex_home, thread_id)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        runtime
            .store_pending_run(pending_run.clone())
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        let mut text = String::new();
        if pending_run.timed_out {
            text.push_str(&format!(
                "Benchmark timed out after {:.1}s.\n",
                pending_run.duration_seconds
            ));
        } else if benchmark_output.exit_code != 0 {
            text.push_str(&format!(
                "Benchmark failed with exit code {} after {:.1}s.\n",
                benchmark_output.exit_code, pending_run.duration_seconds
            ));
        } else {
            text.push_str(&format!(
                "Benchmark passed in {:.1}s.\n",
                pending_run.duration_seconds
            ));
        }
        if let Some(checks_pass) = pending_run.checks_pass {
            text.push_str(if checks_pass {
                "Checks passed.\n"
            } else {
                "Checks failed.\n"
            });
        }
        text.push_str(&format!("Run token: {}\n", pending_run.token));
        if let Some(parsed_primary) = pending_run.parsed_primary {
            text.push_str(&format!(
                "Primary metric {}={}\n",
                config.metric_name,
                format_metric(parsed_primary)
            ));
        }
        if !pending_run.parsed_metrics.is_empty() {
            let metrics = pending_run
                .parsed_metrics
                .iter()
                .map(|(name, value)| format!("{name}={}", format_metric(*value)))
                .collect::<Vec<_>>()
                .join(", ");
            text.push_str(&format!("Parsed metrics: {metrics}\n"));
        }
        text.push('\n');
        text.push_str(&pending_run.output_tail);
        if !checks_output.is_empty() {
            text.push_str("\n\nChecks output:\n");
            text.push_str(&checks_output);
        }
        Ok(FunctionToolOutput::from_text(text, Some(true)))
    }
}

#[async_trait]
impl ToolHandler for AutoresearchLogHandler {
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
                "autoresearch_log received unsupported payload".to_string(),
            ));
        };
        let args: LogArgs = parse_arguments(&arguments)?;
        let status = parse_status(&args.status)?;
        let thread_id = session.conversation_id.to_string();
        let mut runtime = AutoresearchRuntime::load(&turn.config.codex_home, &thread_id)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let state = load_active_state(turn.as_ref(), &thread_id)?;
        ensure_experiment_cycle(&state, "autoresearch_log")?;
        let pending_run = state.pending_run.clone().ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "autoresearch_log requires a pending result from autoresearch_run".to_string(),
            )
        })?;
        if pending_run.token != args.run_token {
            return Err(FunctionCallError::RespondToModel(
                "run_token does not match the most recent autoresearch_run result".to_string(),
            ));
        }

        let resolved_run = resolve_logged_run(&args, status, &pending_run)
            .map_err(FunctionCallError::RespondToModel)?;

        let workdir = state.workdir.clone();
        let mut journal = AutoresearchJournal::load(&workdir)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        let summary = journal.summary();
        let mut workspace = state.workspace.ok_or_else(|| {
            FunctionCallError::RespondToModel("missing autoresearch workspace state".to_string())
        })?;
        let commit = match status {
            AutoresearchExperimentStatus::Keep => workspace
                .commit_keep(&args.description, &resolved_run.result_json)
                .map_err(FunctionCallError::RespondToModel)?
                .unwrap_or_else(|| short_revision(workspace.accepted_revision.as_deref())),
            AutoresearchExperimentStatus::Discard
            | AutoresearchExperimentStatus::Crash
            | AutoresearchExperimentStatus::ChecksFailed => {
                workspace
                    .restore_discard()
                    .map_err(FunctionCallError::RespondToModel)?;
                short_revision(workspace.accepted_revision.as_deref())
            }
        };

        let experiment = journal
            .append_experiment(AutoresearchExperimentEntry {
                run: summary.total_runs.saturating_add(1),
                commit,
                metric: resolved_run.metric,
                metrics: resolved_run.metrics,
                status,
                description: args.description.clone(),
                timestamp: Local::now().timestamp(),
                segment: summary.current_segment,
            })
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        runtime
            .replace_workspace(workspace)
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
        runtime
            .clear_pending_run()
            .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;

        let updated_summary = journal.summary();
        let stage_progress = load_stage_progress(&workdir, &updated_summary);
        let content = format!(
            "Logged run #{} as {}.\nBaseline: {}\nBest: {}\nDescription: {}",
            experiment.run,
            status_label(experiment.status),
            updated_summary
                .baseline_metric()
                .map(format_metric)
                .unwrap_or_else(|| "n/a".to_string()),
            updated_summary
                .best_metric()
                .map(format_metric)
                .unwrap_or_else(|| "n/a".to_string()),
            experiment.description
        );
        let content = if let Some(stage_progress) = stage_progress {
            if stage_progress.has_issues() {
                format!(
                    "{content}\nStaged targets: invalid\nStage warning: {}",
                    stage_progress.issue_summary()
                )
            } else if stage_progress.all_reached() {
                format!(
                    "{content}\nStaged targets: {}/{} reached\nActive stage: all staged targets reached",
                    stage_progress.achieved_count,
                    stage_progress.total_stages()
                )
            } else if let Some(active_stage) = stage_progress.active_stage() {
                format!(
                    "{content}\nStaged targets: {}/{} reached\nActive stage: {}/{} {}",
                    stage_progress.achieved_count,
                    stage_progress.total_stages(),
                    stage_progress.active_stage_number().unwrap_or(1),
                    stage_progress.total_stages(),
                    active_stage.display
                )
            } else {
                content
            }
        } else {
            content
        };
        Ok(FunctionToolOutput::from_text(content, Some(true)))
    }
}

fn load_active_state(
    turn: &TurnContext,
    thread_id: &str,
) -> Result<AutoresearchRunState, FunctionCallError> {
    let runtime = AutoresearchRuntime::load(&turn.config.codex_home, thread_id)
        .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))?;
    runtime
        .state()
        .filter(|state| allows_autoresearch_tool_calls(state))
        .cloned()
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "no active autoresearch session for this thread".to_string(),
            )
        })
}

fn allows_autoresearch_tool_calls(state: &AutoresearchRunState) -> bool {
    !matches!(
        state.status,
        crate::slop_fork::autoresearch::AutoresearchStatus::Stopped
            | crate::slop_fork::autoresearch::AutoresearchStatus::Completed
    ) || (state.active_cycle_kind.is_some() && state.active_turn_id.is_some())
}

fn ensure_experiment_cycle(
    state: &AutoresearchRunState,
    tool_name: &str,
) -> Result<(), FunctionCallError> {
    if state.active_cycle_kind
        == Some(crate::slop_fork::autoresearch::AutoresearchCycleKind::Discovery)
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "{tool_name} is not available during a bounded discovery pass"
        )));
    }
    Ok(())
}

fn enforce_autoresearch_script(workdir: &Path, command: &str) -> Result<String, FunctionCallError> {
    let script_path = workdir.join(AUTORESEARCH_SCRIPT_FILE);
    if !script_path.is_file() {
        return Ok(command.to_string());
    }
    if command.contains(AUTORESEARCH_SCRIPT_FILE) {
        return Ok(command.to_string());
    }
    Err(FunctionCallError::RespondToModel(format!(
        "{AUTORESEARCH_SCRIPT_FILE} exists, so autoresearch_run must invoke it"
    )))
}

async fn execute_command(
    session: &Session,
    turn: &TurnContext,
    workdir: &Path,
    command: &str,
    timeout_seconds: u64,
) -> Result<ExecToolCallOutput, FunctionCallError> {
    let mut env = create_env(
        &turn.shell_environment_policy,
        Some(session.conversation_id),
    );
    env.extend(session.dependency_env().await);
    let exec_params = ExecParams {
        command: session.user_shell().derive_exec_args(command, false),
        cwd: workdir.to_path_buf(),
        expiration: ExecExpiration::Timeout(std::time::Duration::from_secs(timeout_seconds)),
        env,
        network: None,
        sandbox_permissions: SandboxPermissions::UseDefault,
        windows_sandbox_level: turn.windows_sandbox_level,
        windows_sandbox_private_desktop: turn.config.permissions.windows_sandbox_private_desktop,
        justification: Some("Run autoresearch command".to_string()),
        arg0: None,
    };
    process_exec_tool_call(
        exec_params,
        turn.sandbox_policy.get(),
        &turn.file_system_sandbox_policy,
        turn.network_sandbox_policy,
        &turn.cwd,
        &turn.codex_linux_sandbox_exe,
        false,
        None,
    )
    .await
    .map_err(|err| FunctionCallError::RespondToModel(err.to_string()))
}

fn aggregate_output_text(output: &ExecToolCallOutput) -> String {
    let mut text = String::new();
    text.push_str(&output.stdout.text);
    if !output.stderr.text.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&output.stderr.text);
    }
    text
}

fn parse_metric_lines(output: &str) -> BTreeMap<String, f64> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let payload = line.strip_prefix("METRIC ")?;
            let (name, value) = payload.split_once('=')?;
            let parsed = value.trim().parse::<f64>().ok()?;
            Some((name.trim().to_string(), parsed))
        })
        .collect()
}

fn tail_lines(text: &str, max_lines: usize) -> String {
    let mut lines = text.lines().rev().take(max_lines).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

fn parse_status(status: &str) -> Result<AutoresearchExperimentStatus, FunctionCallError> {
    match status {
        "keep" => Ok(AutoresearchExperimentStatus::Keep),
        "discard" => Ok(AutoresearchExperimentStatus::Discard),
        "crash" => Ok(AutoresearchExperimentStatus::Crash),
        "checks_failed" => Ok(AutoresearchExperimentStatus::ChecksFailed),
        other => Err(FunctionCallError::RespondToModel(format!(
            "unknown status {other}; expected keep, discard, crash, or checks_failed"
        ))),
    }
}

fn parse_arguments<T>(arguments: &str) -> Result<T, FunctionCallError>
where
    T: for<'de> Deserialize<'de>,
{
    serde_json::from_str(arguments).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse function arguments: {err}"))
    })
}

fn resolve_logged_run(
    args: &LogArgs,
    status: AutoresearchExperimentStatus,
    pending_run: &PendingRunResult,
) -> Result<ResolvedLoggedRun, String> {
    if let Some(metric) = args.metric
        && let Some(parsed_primary) = pending_run.parsed_primary
        && !same_metric(metric, parsed_primary)
    {
        return Err(
            "metric does not match the authenticated value from autoresearch_run".to_string(),
        );
    }
    if !args.metrics.is_empty()
        && !pending_run.parsed_metrics.is_empty()
        && args.metrics != pending_run.parsed_metrics
    {
        return Err(
            "metrics do not match the authenticated values from autoresearch_run".to_string(),
        );
    }

    let (metric, metrics) = match status {
        AutoresearchExperimentStatus::Keep => {
            if pending_run.timed_out {
                return Err("timed out benchmark runs cannot be kept".to_string());
            }
            if pending_run.exit_code != Some(0) || !pending_run.passed {
                return Err("failed benchmark runs cannot be kept".to_string());
            }
            if pending_run.checks_pass == Some(false) {
                return Err("checks failed, so this run cannot be kept".to_string());
            }
            let metric = pending_run.parsed_primary.ok_or_else(|| {
                "kept runs require a parsed primary metric from autoresearch_run".to_string()
            })?;
            if !args.metrics.is_empty() && pending_run.parsed_metrics.is_empty() {
                return Err(
                    "metrics were not authenticated by autoresearch_run, so they cannot be supplied for a kept run"
                        .to_string(),
                );
            }
            (Some(metric), pending_run.parsed_metrics.clone())
        }
        AutoresearchExperimentStatus::Discard
        | AutoresearchExperimentStatus::Crash
        | AutoresearchExperimentStatus::ChecksFailed => (
            pending_run.parsed_primary.or(args.metric),
            if pending_run.parsed_metrics.is_empty() {
                args.metrics.clone()
            } else {
                pending_run.parsed_metrics.clone()
            },
        ),
    };

    let result_json = serde_json::to_string(&serde_json::json!({
        "status": status_label(status),
        "metric": metric,
        "metrics": metrics,
        "benchmarkPassed": pending_run.passed,
        "exitCode": pending_run.exit_code,
        "timedOut": pending_run.timed_out,
        "checksPass": pending_run.checks_pass,
    }))
    .map_err(|err| format!("failed to serialize autoresearch result: {err}"))?;

    Ok(ResolvedLoggedRun {
        metric,
        metrics,
        result_json,
    })
}

fn same_metric(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= scale * 1e-9
}

fn status_label(status: AutoresearchExperimentStatus) -> &'static str {
    match status {
        AutoresearchExperimentStatus::Keep => "keep",
        AutoresearchExperimentStatus::Discard => "discard",
        AutoresearchExperimentStatus::Crash => "crash",
        AutoresearchExperimentStatus::ChecksFailed => "checks_failed",
    }
}

fn short_revision(revision: Option<&str>) -> String {
    revision
        .map(|revision| revision.chars().take(7).collect())
        .unwrap_or_else(|| "workspace".to_string())
}

fn format_metric(metric: f64) -> String {
    if metric.fract() == 0.0 {
        format!("{metric:.0}")
    } else {
        format!("{metric:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn pending_run() -> PendingRunResult {
        PendingRunResult {
            token: "token".to_string(),
            command: "./autoresearch.sh".to_string(),
            duration_seconds: 1.0,
            exit_code: Some(0),
            timed_out: false,
            passed: true,
            checks_pass: Some(true),
            checks_output: String::new(),
            parsed_primary: Some(1.25),
            parsed_metrics: BTreeMap::from([("score".to_string(), 1.25)]),
            output_tail: String::new(),
        }
    }

    #[test]
    fn keep_requires_authenticated_metric() {
        let pending_run = PendingRunResult {
            parsed_primary: None,
            ..pending_run()
        };
        let args = LogArgs {
            run_token: "token".to_string(),
            metric: Some(9.0),
            status: "keep".to_string(),
            description: "keep it".to_string(),
            metrics: BTreeMap::new(),
        };

        let err = resolve_logged_run(&args, AutoresearchExperimentStatus::Keep, &pending_run)
            .expect_err("keep should fail without parsed metric");
        assert_eq!(
            err,
            "kept runs require a parsed primary metric from autoresearch_run"
        );
    }

    #[test]
    fn keep_rejects_failed_runs() {
        let pending_run = PendingRunResult {
            passed: false,
            exit_code: Some(1),
            ..pending_run()
        };
        let args = LogArgs {
            run_token: "token".to_string(),
            metric: None,
            status: "keep".to_string(),
            description: "keep it".to_string(),
            metrics: BTreeMap::new(),
        };

        let err = resolve_logged_run(&args, AutoresearchExperimentStatus::Keep, &pending_run)
            .expect_err("failed runs cannot be kept");
        assert_eq!(err, "failed benchmark runs cannot be kept");
    }

    #[test]
    fn discard_reuses_authenticated_metrics() {
        let args = LogArgs {
            run_token: "token".to_string(),
            metric: None,
            status: "discard".to_string(),
            description: "discard it".to_string(),
            metrics: BTreeMap::new(),
        };

        let resolved =
            resolve_logged_run(&args, AutoresearchExperimentStatus::Discard, &pending_run())
                .expect("discard should resolve");
        assert_eq!(resolved.metric, Some(1.25));
        assert_eq!(
            resolved.metrics,
            BTreeMap::from([("score".to_string(), 1.25)])
        );
    }

    #[test]
    fn mismatched_metric_is_rejected() {
        let args = LogArgs {
            run_token: "token".to_string(),
            metric: Some(9.0),
            status: "discard".to_string(),
            description: "discard it".to_string(),
            metrics: BTreeMap::new(),
        };

        let err = resolve_logged_run(&args, AutoresearchExperimentStatus::Discard, &pending_run())
            .expect_err("mismatched metric should fail");
        assert_eq!(
            err,
            "metric does not match the authenticated value from autoresearch_run"
        );
    }

    #[test]
    fn discard_without_any_metric_stays_metricless() {
        let args = LogArgs {
            run_token: "token".to_string(),
            metric: None,
            status: "discard".to_string(),
            description: "discard it".to_string(),
            metrics: BTreeMap::new(),
        };
        let mut pending_run = pending_run();
        pending_run.parsed_primary = None;
        pending_run.parsed_metrics.clear();

        let resolved =
            resolve_logged_run(&args, AutoresearchExperimentStatus::Discard, &pending_run)
                .expect("discard should resolve");
        assert_eq!(resolved.metric, None);
        assert_eq!(resolved.metrics, BTreeMap::new());
    }

    #[test]
    fn stopped_in_flight_cycle_still_allows_autoresearch_tools() {
        let state = AutoresearchRunState {
            status: crate::slop_fork::autoresearch::AutoresearchStatus::Stopped,
            active_cycle_kind: Some(
                crate::slop_fork::autoresearch::AutoresearchCycleKind::Continue,
            ),
            active_turn_id: Some("turn-1".to_string()),
            ..AutoresearchRunState::default()
        };

        assert!(allows_autoresearch_tool_calls(&state));
    }

    #[test]
    fn stopped_idle_state_blocks_autoresearch_tools() {
        let state = AutoresearchRunState {
            status: crate::slop_fork::autoresearch::AutoresearchStatus::Stopped,
            ..AutoresearchRunState::default()
        };

        assert!(!allows_autoresearch_tool_calls(&state));
    }

    #[test]
    fn discovery_cycle_rejects_experiment_tools() {
        let state = AutoresearchRunState {
            active_cycle_kind: Some(
                crate::slop_fork::autoresearch::AutoresearchCycleKind::Discovery,
            ),
            ..AutoresearchRunState::default()
        };

        let err = ensure_experiment_cycle(&state, "autoresearch_init").expect_err("must reject");
        assert_eq!(
            err,
            FunctionCallError::RespondToModel(
                "autoresearch_init is not available during a bounded discovery pass".to_string(),
            )
        );
    }
}
