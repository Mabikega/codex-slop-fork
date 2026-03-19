use super::*;
use codex_core::slop_fork::autoresearch::AutoresearchCycleKind;
use codex_core::slop_fork::autoresearch::AutoresearchCyclePlan;
use codex_core::slop_fork::autoresearch::AutoresearchJournal;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::autoresearch::AutoresearchStatus;
use codex_core::slop_fork::autoresearch::AutoresearchWorkspace;
use codex_core::slop_fork::autoresearch::build_init_prompt;
use codex_core::slop_fork::autoresearch::clear_thread_state as clear_autoresearch_thread_state;
use codex_core::slop_fork::autoresearch::load_stage_progress;

use crate::slop_fork::autoresearch_command::AutoresearchCommand;
use crate::slop_fork::autoresearch_command::autoresearch_usage;
use crate::slop_fork::autoresearch_command::parse_autoresearch_command;

impl SlopForkUi {
    pub(crate) fn show_autoresearch_usage(&self) -> Vec<SlopForkUiEffect> {
        vec![SlopForkUiEffect::AddPlainHistoryLines(
            autoresearch_usage().lines().map(Line::from).collect(),
        )]
    }

    pub(crate) fn handle_autoresearch_command(
        &mut self,
        ctx: &SlopForkUiContext,
        trimmed: &str,
    ) -> Vec<SlopForkUiEffect> {
        let command = match parse_autoresearch_command(trimmed) {
            Ok(command) => command,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match command {
            AutoresearchCommand::Help => self.show_autoresearch_usage(),
            AutoresearchCommand::Init { request } => self.autoresearch_init_setup(ctx, request),
            AutoresearchCommand::Status => self.autoresearch_status_output(ctx),
            AutoresearchCommand::Start { goal, max_runs } => {
                self.autoresearch_start(ctx, goal, max_runs)
            }
            AutoresearchCommand::Pause => self.autoresearch_pause(ctx),
            AutoresearchCommand::Resume => self.autoresearch_resume(ctx),
            AutoresearchCommand::WrapUp => self.autoresearch_wrap_up(ctx),
            AutoresearchCommand::Stop => self.autoresearch_stop(ctx),
            AutoresearchCommand::Clear => self.autoresearch_clear(ctx),
        }
    }

    fn autoresearch_init_setup(
        &mut self,
        ctx: &SlopForkUiContext,
        request: String,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.thread_id.is_none() {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch requires an active session.".to_string(),
            )];
        }
        let awaiting_autoresearch_turn_start =
            self.awaiting_autoresearch_turn_start || self.recovered_autoresearch_turn_start;
        let (recovered, init_blocked) = {
            let runtime = match self.ensure_autoresearch_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let recovered = match recover_idle_stale_autoresearch_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let init_blocked = awaiting_autoresearch_turn_start
                || runtime.state().is_some_and(|state| {
                    matches!(state.status, AutoresearchStatus::Running)
                        || state.active_turn_id.is_some()
                        || state.pending_cycle_kind.is_some()
                });
            (recovered, init_blocked)
        };
        if recovered {
            self.clear_autoresearch_turn_start_expectation();
        }
        if init_blocked {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Pause, stop, or finish the current autoresearch session before running $autoresearch init."
                    .to_string(),
            )];
        }
        vec![
            SlopForkUiEffect::AddInfoMessage {
                message: "Autoresearch setup requested.".to_string(),
                hint: Some(
                    "This turn will scaffold autoresearch.md, benchmark scripts, and metric policy. Run $autoresearch start once the setup looks right."
                        .to_string(),
                ),
            },
            SlopForkUiEffect::SubmitAutoresearchSetupTurn {
                prompt: build_init_prompt(&request),
            },
        ]
    }

    pub(crate) fn on_autoresearch_turn_submission_started(
        &mut self,
        ctx: &SlopForkUiContext,
        _cycle_kind: AutoresearchCycleKind,
    ) -> Vec<SlopForkUiEffect> {
        self.awaiting_autoresearch_turn_start = true;
        self.recovered_autoresearch_turn_start = false;
        match self.ensure_autoresearch_runtime(ctx).and_then(|runtime| {
            runtime
                .note_submission_dispatched()
                .map_err(|err| format!("Failed to record autoresearch submission: {err}"))
        }) {
            Ok(_) => Vec::new(),
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(err)],
        }
    }

    pub(crate) fn on_autoresearch_turn_submission_failed(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        self.clear_autoresearch_turn_start_expectation();
        match self.ensure_autoresearch_runtime(ctx).and_then(|runtime| {
            runtime
                .note_submission_failure(
                    "Autoresearch submission failed before the turn could start.",
                )
                .map_err(|err| format!("Failed to update autoresearch state: {err}"))
        }) {
            Ok(_) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch submission failed before the turn could start.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(err)],
        }
    }

    pub(crate) fn on_autoresearch_turn_started(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        if ctx.thread_id.is_none() {
            self.clear_autoresearch_turn_start_expectation();
            return Vec::new();
        }
        if !self.awaiting_autoresearch_turn_start && !self.recovered_autoresearch_turn_start {
            return Vec::new();
        }
        self.clear_autoresearch_turn_start_expectation();
        match self.ensure_autoresearch_runtime(ctx).and_then(|runtime| {
            runtime
                .note_turn_submitted(turn_id)
                .map_err(|err| format!("Failed to record autoresearch turn id: {err}"))?;
            runtime
                .activate_pending_cycle(turn_id.to_string())
                .map_err(|err| format!("Failed to activate autoresearch cycle: {err}"))
        }) {
            Ok(_) => Vec::new(),
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(err)],
        }
    }

    pub(crate) fn on_autoresearch_turn_completed(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
        last_agent_message: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        let runtime = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        let completed = match runtime.complete_turn(turn_id, last_agent_message, Local::now()) {
            Ok(completed) => completed,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to update autoresearch state: {err}"
                ))];
            }
        };
        if !completed {
            return Vec::new();
        }
        self.autoresearch_follow_up_effects(ctx)
    }

    pub(crate) fn autoresearch_has_active_turn(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
    ) -> bool {
        self.ensure_autoresearch_runtime(ctx)
            .ok()
            .is_some_and(|runtime| runtime.is_active_turn(turn_id))
    }

    pub(crate) fn on_autoresearch_idle(
        &mut self,
        ctx: &SlopForkUiContext,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        self.autoresearch_follow_up_effects(ctx)
    }

    pub(crate) fn on_autoresearch_turn_aborted(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: Option<&str>,
        reason: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        self.clear_autoresearch_turn_start_expectation();
        match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => match runtime.abort_turn(turn_id, reason) {
                Ok(true) => vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch paused because the active turn was aborted.".to_string(),
                    hint: Some(
                        "Use $autoresearch resume or $autoresearch wrap-up when you want to continue."
                            .to_string(),
                    ),
                }],
                Ok(false) => Vec::new(),
                Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to update autoresearch state: {err}"
                ))],
            },
            Err(_) => Vec::new(),
        }
    }

    fn autoresearch_start(
        &mut self,
        ctx: &SlopForkUiContext,
        goal: String,
        max_runs: Option<u32>,
    ) -> Vec<SlopForkUiEffect> {
        let Some(thread_id) = ctx.thread_id.as_deref() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch requires an active session.".to_string(),
            )];
        };
        let (recovered, start_blocked_message) = {
            let runtime = match self.ensure_autoresearch_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let recovered = match recover_idle_stale_autoresearch_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let start_blocked_message = runtime.state().and_then(|state| {
                if state.active_turn_id.is_some() || state.pending_cycle_kind.is_some() {
                    Some(
                        "Autoresearch already has a queued or running cycle. Stop it first if you want to replace the goal."
                            .to_string(),
                    )
                } else if !matches!(
                    state.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                ) {
                    Some(
                        "Autoresearch already has an active session. Resume it, wrap it up, stop it, or clear it before replacing the goal."
                            .to_string(),
                    )
                } else {
                    None
                }
            });
            (recovered, start_blocked_message)
        };
        if recovered {
            self.clear_autoresearch_turn_start_expectation();
        }
        if let Some(start_blocked_message) = start_blocked_message {
            return vec![SlopForkUiEffect::AddErrorMessage(start_blocked_message)];
        }
        let prepared_workspace =
            match AutoresearchWorkspace::prepare(&ctx.codex_home, thread_id, &ctx.cwd) {
                Ok(workspace) => workspace,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
        let Some(runtime) = self.autoresearch_runtime.as_mut() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch runtime should be loaded before start.".to_string(),
            )];
        };
        match runtime.start(
            goal,
            ctx.cwd.clone(),
            prepared_workspace.workspace,
            max_runs,
            Local::now(),
        ) {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch started.".to_string(),
                    hint: Some(format!("Workspace mode: {}", prepared_workspace.summary)),
                }];
                effects.extend(self.autoresearch_follow_up_effects(ctx));
                effects
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch already has an active session.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to start autoresearch: {err}"
            ))],
        }
    }

    fn autoresearch_pause(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let runtime = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.pause() {
            Ok(true) => {
                let pending_turn_start_preserved = runtime.has_pending_turn_start();
                if pending_turn_start_preserved {
                    self.awaiting_autoresearch_turn_start = true;
                    self.recovered_autoresearch_turn_start = false;
                } else {
                    self.clear_autoresearch_turn_start_expectation();
                }
                vec![SlopForkUiEffect::AddInfoMessage {
                    message: if pending_turn_start_preserved {
                        "Autoresearch paused. The already-submitted cycle will finish first."
                            .to_string()
                    } else {
                        "Autoresearch paused.".to_string()
                    },
                    hint: Some(
                        "The current turn will finish if one is already running, but no new experiment cycle will be queued."
                            .to_string(),
                    ),
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch is not active.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to pause autoresearch: {err}"
            ))],
        }
    }

    fn autoresearch_resume(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let runtime = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.resume() {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch resumed.".to_string(),
                    hint: None,
                }];
                effects.extend(self.autoresearch_follow_up_effects(ctx));
                effects
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch cannot be resumed in its current state.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to resume autoresearch: {err}"
            ))],
        }
    }

    fn autoresearch_wrap_up(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let runtime = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.request_wrap_up() {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch wrap-up requested.".to_string(),
                    hint: Some(
                        "The next autonomous cycle will stop broad exploration and finish with a report."
                            .to_string(),
                    ),
                }];
                effects.extend(self.autoresearch_follow_up_effects(ctx));
                effects
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch cannot wrap up in its current state.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to request autoresearch wrap-up: {err}"
            ))],
        }
    }

    fn autoresearch_stop(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let recovered = {
            let runtime = match self.ensure_autoresearch_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            match recover_idle_stale_autoresearch_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            }
        };
        if recovered {
            self.clear_autoresearch_turn_start_expectation();
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "Autoresearch cleared stale cycle state.".to_string(),
                hint: Some("You can start a new autoresearch goal now.".to_string()),
            }];
        }
        let Some(runtime) = self.autoresearch_runtime.as_mut() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch runtime should be loaded before stop.".to_string(),
            )];
        };
        match runtime.stop() {
            Ok(true) => {
                let pending_turn_start_preserved = runtime.has_pending_turn_start();
                let message = if !ctx.task_running && !pending_turn_start_preserved {
                    match runtime.clear_orphaned_cycle_if_idle(Local::now()) {
                        Ok(true) => {
                            "Autoresearch stopped and cleared stale cycle state.".to_string()
                        }
                        Ok(false) => "Autoresearch stopped.".to_string(),
                        Err(err) => {
                            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                                "Failed to recover stale autoresearch state: {err}"
                            ))];
                        }
                    }
                } else if pending_turn_start_preserved {
                    "Autoresearch stopped. The already-submitted cycle may still finish."
                        .to_string()
                } else {
                    "Autoresearch stopped.".to_string()
                };
                if pending_turn_start_preserved {
                    self.awaiting_autoresearch_turn_start = true;
                    self.recovered_autoresearch_turn_start = false;
                } else {
                    self.clear_autoresearch_turn_start_expectation();
                }
                vec![SlopForkUiEffect::AddInfoMessage {
                    message,
                    hint: Some(
                        "If a controller-owned turn is already running, it may still finish, but no further cycles will be queued."
                            .to_string(),
                    ),
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch is not active.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to stop autoresearch: {err}"
            ))],
        }
    }

    fn autoresearch_clear(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let Some(thread_id) = ctx.thread_id.as_deref() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch requires an active session.".to_string(),
            )];
        };
        let runtime = match AutoresearchRuntime::load(&ctx.codex_home, thread_id) {
            Ok(runtime) => runtime,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load autoresearch state: {err}"
                ))];
            }
        };
        if let Some(state) = runtime.state()
            && let Some(workspace) = state.workspace.as_ref()
            && let Err(err) = workspace.clear_snapshot()
        {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to clear autoresearch snapshot: {err}"
            ))];
        }
        let journal_workdir = runtime
            .state()
            .map(|state| state.workdir.clone())
            .unwrap_or_else(|| ctx.cwd.clone());
        if let Err(err) = AutoresearchJournal::remove_file(&journal_workdir) {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to delete autoresearch.jsonl: {err}"
            ))];
        }
        if let Err(err) = clear_autoresearch_thread_state(&ctx.codex_home, thread_id) {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to clear autoresearch state: {err}"
            ))];
        }
        self.clear_autoresearch_turn_start_expectation();
        self.autoresearch_runtime = None;
        vec![SlopForkUiEffect::AddInfoMessage {
            message: "Autoresearch state cleared.".to_string(),
            hint: Some(
                "The journal was removed, but autoresearch.md and benchmark scripts were left in place."
                    .to_string(),
            ),
        }]
    }

    fn autoresearch_follow_up_effects(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.task_running || ctx.thread_id.is_none() {
            return Vec::new();
        }
        let runtime = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        if runtime.state().is_none() {
            return Vec::new();
        }
        let next_cycle = match runtime.prepare_cycle_submission(Local::now()) {
            Ok(plan) => plan,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to prepare autoresearch follow-up: {err}"
                ))];
            }
        };
        let Some(next_cycle) = next_cycle else {
            return Vec::new();
        };
        vec![autoresearch_cycle_effect(next_cycle)]
    }

    fn autoresearch_status_output(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.thread_id.is_none() {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch requires an active session.".to_string(),
            )];
        }
        let (recovered, runtime_state, journal_workdir) = {
            let runtime = match self.ensure_autoresearch_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let recovered = match recover_idle_stale_autoresearch_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let journal_workdir = runtime
                .state()
                .map(|state| state.workdir.clone())
                .unwrap_or_else(|| ctx.cwd.clone());
            (recovered, runtime.state().cloned(), journal_workdir)
        };
        if recovered {
            self.clear_autoresearch_turn_start_expectation();
        }
        let journal = match AutoresearchJournal::load(&journal_workdir) {
            Ok(journal) => journal,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load autoresearch journal: {err}"
                ))];
            }
        };
        let summary = journal.summary();
        let stage_progress = load_stage_progress(&journal_workdir, &summary);
        let config_name = summary
            .config
            .as_ref()
            .map(|config| config.name.clone())
            .unwrap_or_else(|| "(not initialized)".to_string());
        let config_metric = summary
            .config
            .as_ref()
            .map(|config| config.metric_name.clone())
            .unwrap_or_else(|| "(unknown)".to_string());
        let Some(state) = runtime_state.as_ref() else {
            let mut lines = vec![
                "Autoresearch".bold().into(),
                "Status: idle".into(),
                format!("Session: {config_name}").into(),
                format!("Metric: {config_metric}").into(),
                format!("Workdir: {}", journal_workdir.display()).into(),
            ];
            lines.push(format!("Discovery passes: {}", summary.discovery_count()).into());
            if let Some(last_discovery) = summary.last_discovery() {
                lines.push(format!("Last discovery: {}", last_discovery.reason.label()).into());
            }
            if let Some(stage_progress) = stage_progress.as_ref() {
                if stage_progress.has_issues() {
                    lines.push("Staged targets: invalid".into());
                    lines.push(
                        format!("Stage warning: {}", stage_progress.issue_summary())
                            .red()
                            .into(),
                    );
                } else {
                    lines.push(
                        format!(
                            "Staged targets: {}/{} reached",
                            stage_progress.achieved_count,
                            stage_progress.total_stages()
                        )
                        .into(),
                    );
                }
            }
            lines.push(
                "Hint: Use $autoresearch start --max-runs 50 <goal> to begin an autonomous benchmark loop."
                    .dim()
                    .into(),
            );
            return vec![SlopForkUiEffect::AddPlainHistoryLines(lines)];
        };

        let best_metric = summary
            .best_metric()
            .map(format_metric)
            .unwrap_or_else(|| "n/a".to_string());
        let baseline = summary
            .baseline_metric()
            .map(format_metric)
            .unwrap_or_else(|| "n/a".to_string());
        let mut lines = vec![
            "Autoresearch".bold().into(),
            format!("Status: {}", autoresearch_status_label(state.status)).into(),
            format!("Goal: {}", state.goal).into(),
            format!("Session: {config_name}").into(),
            format!("Metric: {config_metric}").into(),
            format!("Iterations: {}", state.iteration_count).into(),
            format!(
                "Runs in current segment: {}",
                summary.current_segment_runs.len()
            )
            .into(),
            format!("Discovery passes: {}", state.discovery_count).into(),
            format!("Baseline: {baseline}").into(),
            format!("Best: {best_metric}").into(),
            format!("Kept: {}", summary.keep_count()).into(),
            format!("Discarded: {}", summary.discard_count()).into(),
            format!("Crashes: {}", summary.crash_count()).into(),
            format!("Checks failed: {}", summary.checks_failed_count()).into(),
            format!("Workdir: {}", state.workdir.display()).into(),
        ];
        if let Some(stage_progress) = stage_progress.as_ref() {
            if stage_progress.has_issues() {
                lines.push("Staged targets: invalid".into());
                lines.push(
                    format!("Stage warning: {}", stage_progress.issue_summary())
                        .red()
                        .into(),
                );
            } else {
                lines.push(
                    format!(
                        "Staged targets: {}/{} reached",
                        stage_progress.achieved_count,
                        stage_progress.total_stages()
                    )
                    .into(),
                );
                lines.push(
                    if let Some(active_stage) = stage_progress.active_stage() {
                        format!(
                            "Active stage: {}/{} {}",
                            stage_progress.active_stage_number().unwrap_or(1),
                            stage_progress.total_stages(),
                            active_stage.display
                        )
                    } else {
                        "Active stage: all staged targets reached".to_string()
                    }
                    .into(),
                );
            }
        }
        if let Some(max_runs) = state.max_runs {
            lines.push(format!("Max runs: {max_runs}").into());
        }
        if let Some(request) = state.active_discovery_request.as_ref() {
            lines.push(format!("Active discovery: {}", request.display_reason()).into());
        } else if let Some(request) = state.queued_discovery_request.as_ref() {
            lines.push(format!("Queued discovery: {}", request.display_reason()).into());
        } else if let Some(last_discovery) = summary.last_discovery() {
            lines.push(format!("Last discovery: {}", last_discovery.reason.label()).into());
            if let Some(focus) = last_discovery.focus.as_deref() {
                lines.push(format!("Discovery focus: {focus}").into());
            }
        }
        if let Some(status_message) = state.status_message.as_deref() {
            lines.push(format!("Status message: {status_message}").into());
        }
        if let Some(last_cycle_summary) = state.last_cycle_summary.as_deref() {
            lines.push(format!("Last cycle summary: {last_cycle_summary}").into());
        }
        if let Some(pending_run) = state.pending_run.as_ref() {
            lines.push(
                format!(
                    "Pending run: {} ({:.1}s)",
                    pending_run.command, pending_run.duration_seconds
                )
                .into(),
            );
            lines.push(format!("Pending token: {}", pending_run.token).dim().into());
        }
        if let Some(last_error) = state.last_error.as_deref() {
            lines.push(format!("Last error: {last_error}").red().into());
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn ensure_autoresearch_runtime(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Result<&mut AutoresearchRuntime, String> {
        let Some(thread_id) = ctx.thread_id.as_deref() else {
            self.autoresearch_runtime = None;
            return Err("Autoresearch requires an active session.".to_string());
        };
        self.autoresearch_runtime = Some(
            AutoresearchRuntime::load(&ctx.codex_home, thread_id)
                .map_err(|err| format!("Failed to load autoresearch state: {err}"))?,
        );
        self.autoresearch_runtime
            .as_mut()
            .ok_or_else(|| "Autoresearch is unavailable.".to_string())
    }

    fn clear_autoresearch_turn_start_expectation(&mut self) {
        self.awaiting_autoresearch_turn_start = false;
        self.recovered_autoresearch_turn_start = false;
    }
}

fn autoresearch_cycle_effect(next_cycle: AutoresearchCyclePlan) -> SlopForkUiEffect {
    SlopForkUiEffect::SubmitAutoresearchTurn {
        prompt: next_cycle.prompt,
        cycle_kind: next_cycle.kind,
        notify_on_completion: next_cycle.notify_on_completion,
    }
}

fn autoresearch_status_label(status: AutoresearchStatus) -> &'static str {
    match status {
        AutoresearchStatus::Running => "running",
        AutoresearchStatus::Paused => "paused",
        AutoresearchStatus::Stopped => "stopped",
        AutoresearchStatus::Completed => "completed",
    }
}

fn format_metric(metric: f64) -> String {
    if metric.fract() == 0.0 {
        format!("{}", metric as i64)
    } else {
        format!("{metric:.4}")
    }
}

fn recover_idle_stale_autoresearch_state(
    ctx: &SlopForkUiContext,
    runtime: &mut AutoresearchRuntime,
) -> Result<bool, String> {
    if ctx.task_running {
        return Ok(false);
    }
    if runtime.has_pending_turn_start()
        && runtime.state().is_some_and(|state| {
            !matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            )
        })
    {
        return Ok(false);
    }
    runtime
        .clear_orphaned_cycle_if_idle(Local::now())
        .map_err(|err| format!("Failed to recover stale autoresearch state: {err}"))
}
