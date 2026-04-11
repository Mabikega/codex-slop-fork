use super::*;
use codex_app_server_protocol::PilotCycleKind as AppServerPilotCycleKind;
use codex_app_server_protocol::PilotStatus as AppServerPilotStatus;
use codex_core::slop_fork::pilot::PilotCyclePlan;
use codex_core::slop_fork::pilot::PilotStatus;

use crate::slop_fork::pilot_command::PilotCommand;
use crate::slop_fork::pilot_command::parse_pilot_command;
use crate::slop_fork::pilot_command::pilot_usage;

const PILOT_PENDING_START_RECOVERY_GRACE_SECS: i64 = 2;

impl SlopForkUi {
    pub(crate) fn show_pilot_usage(&self) -> Vec<SlopForkUiEffect> {
        vec![SlopForkUiEffect::AddPlainHistoryLines(
            pilot_usage().lines().map(Line::from).collect(),
        )]
    }

    pub(crate) fn handle_pilot_command(
        &mut self,
        ctx: &SlopForkUiContext,
        trimmed: &str,
    ) -> Vec<SlopForkUiEffect> {
        let command = match parse_pilot_command(trimmed, Local::now()) {
            Ok(command) => command,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match command {
            PilotCommand::Help => self.show_pilot_usage(),
            PilotCommand::Status => self.pilot_status_output(ctx),
            PilotCommand::Start { goal, deadline_at } => self.pilot_start(ctx, goal, deadline_at),
            PilotCommand::Pause => self.pilot_pause(ctx),
            PilotCommand::Resume => self.pilot_resume(ctx),
            PilotCommand::WrapUp => self.pilot_wrap_up(ctx),
            PilotCommand::Stop => self.pilot_stop(ctx),
        }
    }

    pub(crate) fn on_pilot_turn_submission_started(
        &mut self,
        ctx: &SlopForkUiContext,
        cycle_kind: PilotCycleKind,
    ) -> Vec<SlopForkUiEffect> {
        let _ = cycle_kind;
        self.awaiting_pilot_turn_start = true;
        if ctx.remote_app_server {
            return Vec::new();
        }
        match self.ensure_pilot_runtime(ctx).and_then(|runtime| {
            runtime
                .note_submission_dispatched()
                .map_err(|err| format!("Failed to record pilot submission dispatch: {err}"))
        }) {
            Ok(_) => Vec::new(),
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(err)],
        }
    }

    pub(crate) fn on_pilot_turn_submission_failed(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Vec<SlopForkUiEffect> {
        self.awaiting_pilot_turn_start = false;
        if ctx.remote_app_server {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot submission failed before the turn could start.".to_string(),
            )];
        }
        match self.ensure_pilot_runtime(ctx).and_then(|runtime| {
            runtime
                .note_submission_failure("Pilot submission failed before the turn could start.")
                .map_err(|err| format!("Failed to update pilot state: {err}"))
        }) {
            Ok(_) => vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot submission failed before the turn could start.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(err)],
        }
    }

    pub(crate) fn on_pilot_turn_started(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        if ctx.remote_app_server {
            return Vec::new();
        }
        let should_claim_pending_cycle = if self.awaiting_pilot_turn_start {
            true
        } else {
            self.ensure_pilot_runtime(ctx).ok().is_some_and(|runtime| {
                runtime.state().is_some_and(|state| {
                    state.pending_cycle_kind.is_some()
                        && state.active_turn_id.is_none()
                        && state.submission_dispatched_at.is_none()
                })
            })
        };
        if !should_claim_pending_cycle {
            return Vec::new();
        }
        self.awaiting_pilot_turn_start = false;
        match self.ensure_pilot_runtime(ctx).and_then(|runtime| {
            runtime
                .note_turn_submitted(turn_id)
                .map_err(|err| format!("Failed to record pilot turn id: {err}"))?;
            runtime
                .activate_pending_cycle(turn_id.to_string())
                .map_err(|err| format!("Failed to activate pilot turn: {err}"))
        }) {
            Ok(true) => Vec::new(),
            Ok(false) => Vec::new(),
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(err)],
        }
    }

    pub(crate) fn on_pilot_turn_completed(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
        last_agent_message: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        if ctx.remote_app_server {
            let _ = (turn_id, last_agent_message);
            return Vec::new();
        }
        let runtime = match self.ensure_pilot_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        let completed = match runtime.complete_turn(turn_id, last_agent_message, Local::now()) {
            Ok(completed) => completed,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to update pilot state: {err}"
                ))];
            }
        };
        if !completed {
            return Vec::new();
        }
        self.pilot_follow_up_effects(ctx)
    }

    pub(crate) fn on_pilot_idle(
        &mut self,
        ctx: &SlopForkUiContext,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        if ctx.remote_app_server {
            return Vec::new();
        }
        self.pilot_follow_up_effects(ctx)
    }

    pub(crate) fn on_pilot_turn_aborted(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: Option<&str>,
        reason: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if from_replay {
            return Vec::new();
        }
        self.awaiting_pilot_turn_start = false;
        if ctx.remote_app_server {
            let _ = (turn_id, reason);
            return Vec::new();
        }
        match self.ensure_pilot_runtime(ctx) {
            Ok(runtime) => match runtime.abort_turn(turn_id, reason) {
                Ok(true) => vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot paused because the active turn was aborted.".to_string(),
                    hint: Some(
                        "Use $pilot resume or $pilot wrap-up when you want to continue."
                            .to_string(),
                    ),
                }],
                Ok(false) => Vec::new(),
                Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to update pilot state: {err}"
                ))],
            },
            Err(_) => Vec::new(),
        }
    }

    pub(crate) fn pilot_has_active_turn(&mut self, ctx: &SlopForkUiContext, turn_id: &str) -> bool {
        if ctx.remote_app_server {
            return self
                .remote_pilot_run
                .as_ref()
                .and_then(|run| run.active_turn_id.as_deref())
                == Some(turn_id);
        }
        self.ensure_pilot_runtime(ctx)
            .ok()
            .is_some_and(|runtime| runtime.is_active_turn(turn_id))
    }

    pub(crate) fn pilot_owns_turn_abort(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: Option<&str>,
    ) -> bool {
        if ctx.remote_app_server {
            return self
                .remote_pilot_run
                .as_ref()
                .is_some_and(|run| pilot_run_owns_turn_abort(run, turn_id));
        }
        let awaiting_pilot_turn_start = self.awaiting_pilot_turn_start;
        self.ensure_pilot_runtime(ctx).ok().is_some_and(|runtime| {
            runtime.state().is_some_and(|state| {
                state.active_turn_id.is_some()
                    && turn_id
                        .is_none_or(|turn_id| state.active_turn_id.as_deref() == Some(turn_id))
            }) || (awaiting_pilot_turn_start
                && runtime.state().is_some_and(|state| {
                    state.pending_cycle_kind.is_some()
                        && state.active_turn_id.is_none()
                        && turn_id.is_none_or(|turn_id| {
                            state
                                .last_submitted_turn_id
                                .as_deref()
                                .is_none_or(|submitted_turn_id| submitted_turn_id == turn_id)
                        })
                }))
        }) || self
            .remote_pilot_run
            .as_ref()
            .is_some_and(|run| pilot_run_owns_turn_abort(run, turn_id))
    }

    fn pilot_start(
        &mut self,
        ctx: &SlopForkUiContext,
        goal: String,
        deadline_at: Option<i64>,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Pilot requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot start requested.".to_string(),
                    hint: Some(
                        "The connected app-server owns Pilot state and will queue the first cycle when the thread is idle."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::StartRemotePilot {
                    thread_id: thread_id.clone(),
                    goal,
                    deadline_at,
                },
            ];
        }
        let (recovered, has_queued_or_running_cycle) = {
            let runtime = match self.ensure_pilot_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let recovered = match recover_idle_stale_pilot_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let has_queued_or_running_cycle = runtime.state().is_some_and(|state| {
                state.active_turn_id.is_some() || state.pending_cycle_kind.is_some()
            });
            (recovered, has_queued_or_running_cycle)
        };
        if recovered {
            self.awaiting_pilot_turn_start = false;
        }
        if has_queued_or_running_cycle {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot already has a queued or running cycle. Stop it first if you want to replace the goal."
                    .to_string(),
            )];
        }
        let start_result = {
            let runtime = match self.ensure_pilot_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            runtime.start(goal, deadline_at, Local::now())
        };
        match start_result {
            Ok(false) => {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Pilot already has a queued or running cycle. Stop it first if you want to replace the goal."
                        .to_string(),
                )];
            }
            Ok(true) => {}
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to start Pilot: {err}"
                ))];
            }
        }

        let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
            message: "Pilot started.".to_string(),
            hint: Some(if ctx.task_running {
                "Pilot will queue its first autonomous cycle when the current turn becomes idle."
                    .to_string()
            } else {
                "Pilot uses assistant-controlled continuation turns, so it can continue without synthetic user messages."
                        .to_string()
            }),
        }];
        effects.extend(self.pilot_follow_up_effects(ctx));
        effects
    }

    fn pilot_pause(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Pilot requires an active session.".to_string(),
                )];
            };
            self.awaiting_pilot_turn_start = false;
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot pause requested.".to_string(),
                    hint: Some(
                        "Use $pilot status to inspect the server-owned Pilot state after the control request completes."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemotePilot {
                    thread_id: thread_id.clone(),
                    action: codex_app_server_protocol::PilotControlAction::Pause,
                },
            ];
        }
        let runtime = match self.ensure_pilot_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.pause() {
            Ok(true) => {
                self.awaiting_pilot_turn_start = false;
                vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot paused.".to_string(),
                    hint: Some(
                        "The current turn will finish if one is already running, but Pilot will not schedule another cycle."
                            .to_string(),
                    ),
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot is not active.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to pause Pilot: {err}"
            ))],
        }
    }

    fn pilot_resume(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Pilot requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot resume requested.".to_string(),
                    hint: Some(
                        "The connected app-server will decide whether Pilot can continue from its current state."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemotePilot {
                    thread_id: thread_id.clone(),
                    action: codex_app_server_protocol::PilotControlAction::Resume,
                },
            ];
        }
        let runtime = match self.ensure_pilot_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.resume() {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot resumed.".to_string(),
                    hint: None,
                }];
                effects.extend(self.pilot_follow_up_effects(ctx));
                effects
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot cannot be resumed in its current state.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to resume Pilot: {err}"
            ))],
        }
    }

    fn pilot_wrap_up(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Pilot requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot wrap-up requested.".to_string(),
                    hint: Some(
                        "The connected app-server will stop broad new work and finish with a final report when Pilot accepts the request."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemotePilot {
                    thread_id: thread_id.clone(),
                    action: codex_app_server_protocol::PilotControlAction::WrapUp,
                },
            ];
        }
        let runtime = match self.ensure_pilot_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.request_wrap_up() {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot wrap-up requested.".to_string(),
                    hint: Some(
                        "Pilot will stop starting broad new work and finish with a final report."
                            .to_string(),
                    ),
                }];
                effects.extend(self.pilot_follow_up_effects(ctx));
                effects
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot cannot wrap up in its current state.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to request Pilot wrap-up: {err}"
            ))],
        }
    }

    fn pilot_stop(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Pilot requires an active session.".to_string(),
                )];
            };
            self.awaiting_pilot_turn_start = false;
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot stop requested.".to_string(),
                    hint: Some(
                        "If a server-owned Pilot turn is already running, it may still finish, but no further cycles should be queued."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemotePilot {
                    thread_id: thread_id.clone(),
                    action: codex_app_server_protocol::PilotControlAction::Stop,
                },
            ];
        }
        let recovered = {
            let runtime = match self.ensure_pilot_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            match recover_idle_stale_pilot_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            }
        };
        if recovered {
            self.awaiting_pilot_turn_start = false;
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "Pilot cleared stale cycle state.".to_string(),
                hint: Some("You can start a new Pilot goal now.".to_string()),
            }];
        }
        let stop_result = {
            let runtime = match self.ensure_pilot_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            runtime.stop()
        };
        match stop_result {
            Ok(true) => {
                self.awaiting_pilot_turn_start = false;
                let message = if !ctx.task_running {
                    let clear_result = {
                        let runtime = match self.ensure_pilot_runtime(ctx) {
                            Ok(runtime) => runtime,
                            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
                        };
                        runtime.clear_orphaned_cycle_if_idle(Local::now())
                    };
                    match clear_result {
                        Ok(true) => "Pilot stopped and cleared stale cycle state.".to_string(),
                        Ok(false) => "Pilot stopped.".to_string(),
                        Err(err) => {
                            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                                "Failed to recover stale Pilot state: {err}"
                            ))];
                        }
                    }
                } else {
                    "Pilot stopped.".to_string()
                };
                vec![SlopForkUiEffect::AddInfoMessage {
                    message,
                    hint: Some(
                        "If a Pilot-controlled turn is already running, it may finish, but no further Pilot cycles will be scheduled."
                            .to_string(),
                    ),
                }]
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Pilot is not active.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to stop Pilot: {err}"
            ))],
        }
    }

    fn pilot_follow_up_effects(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            return Vec::new();
        }
        if ctx.task_running {
            return Vec::new();
        }
        if ctx.thread_id.is_none() {
            self.pilot_runtime = None;
            return Vec::new();
        }
        let runtime = match self.ensure_pilot_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        if let Err(err) = recover_idle_stale_pilot_state(ctx, runtime) {
            return vec![SlopForkUiEffect::AddErrorMessage(err)];
        }
        let next_cycle = match runtime.prepare_cycle_submission(Local::now()) {
            Ok(plan) => plan,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to prepare Pilot follow-up: {err}"
                ))];
            }
        };
        let Some(next_cycle) = next_cycle else {
            return Vec::new();
        };
        vec![pilot_cycle_effect(next_cycle)]
    }

    pub(crate) fn pilot_frame_effects(
        &mut self,
        ctx: &SlopForkUiContext,
        _now: DateTime<Local>,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server || ctx.task_running || ctx.thread_id.is_none() {
            return Vec::new();
        }
        let Ok(runtime) = self.ensure_pilot_runtime(ctx) else {
            return Vec::new();
        };
        let should_retry_while_idle = runtime.state().is_some_and(|state| {
            state.status == PilotStatus::Running
                && state.active_turn_id.is_none()
                && (state.pending_cycle_kind.is_some() || state.submission_dispatched_at.is_some())
        });
        should_retry_while_idle
            .then_some(SlopForkUiEffect::ScheduleFrameIn(
                std::time::Duration::from_secs(
                    u64::try_from(PILOT_PENDING_START_RECOVERY_GRACE_SECS).unwrap_or(2),
                ),
            ))
            .into_iter()
            .collect()
    }

    fn pilot_status_output(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            if !self.remote_pilot_state_loaded {
                return vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Pilot remote status is unavailable.".to_string(),
                    hint: Some(
                        "Wait for the server to send a Pilot update before requesting status."
                            .to_string(),
                    ),
                }];
            }
            let Some(run) = self.remote_pilot_run.as_ref() else {
                return vec![SlopForkUiEffect::AddPlainHistoryLines(vec![
                    "Pilot".bold().into(),
                    "Status: idle".into(),
                    "Server state: no Pilot run is active for this thread.".into(),
                ])];
            };

            let mut lines = vec![
                "Pilot".bold().into(),
                format!("Status: {}", remote_pilot_status_label(run)).into(),
                format!("Goal: {}", run.goal).into(),
                format!("Iterations: {}", run.iteration_count).into(),
                format!("Started: {}", timestamp_label(run.started_at)).into(),
                format!("Updated: {}", timestamp_label(run.updated_at)).into(),
            ];
            if let Some(deadline_at) = run.deadline_at {
                lines.push(format!("Deadline: {}", timestamp_label(deadline_at)).into());
            } else {
                lines.push("Deadline: none".into());
            }
            if let Some(active_turn_id) = run.active_turn_id.as_deref() {
                lines.push(format!("Active turn: {active_turn_id}").into());
            }
            if let Some(last_submitted_turn_id) = run.last_submitted_turn_id.as_deref() {
                lines.push(format!("Last submitted turn: {last_submitted_turn_id}").into());
            }
            if let Some(pending_cycle_kind) = run.pending_cycle_kind {
                lines.push(
                    format!(
                        "Pending cycle: {}",
                        remote_pilot_cycle_label(pending_cycle_kind)
                    )
                    .into(),
                );
            }
            if run.wrap_up_requested {
                lines.push("Wrap-up requested: true".into());
            }
            if let Some(last_cycle_completed_at) = run.last_cycle_completed_at {
                lines.push(
                    format!(
                        "Last cycle completed: {}",
                        timestamp_label(last_cycle_completed_at)
                    )
                    .into(),
                );
            }
            if let Some(last_progress_at) = run.last_progress_at {
                lines.push(format!("Last progress: {}", timestamp_label(last_progress_at)).into());
            }
            if let Some(status_message) = run.status_message.as_deref() {
                lines.push(format!("Status message: {status_message}").into());
            }
            if let Some(last_cycle_summary) = run.last_cycle_summary.as_deref() {
                lines.push(format!("Last cycle summary: {last_cycle_summary}").into());
            }
            if let Some(last_error) = run.last_error.as_deref() {
                lines.push(format!("Last error: {last_error}").red().into());
            }
            return vec![SlopForkUiEffect::AddPlainHistoryLines(lines)];
        }
        let (recovered, state) = {
            let runtime = match self.ensure_pilot_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            let recovered = match recover_idle_stale_pilot_state(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            (recovered, runtime.state().cloned())
        };
        if recovered {
            self.awaiting_pilot_turn_start = false;
        }
        let Some(state) = state else {
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "Pilot is idle.".to_string(),
                hint: Some(
                    "Use $pilot start --for 4h <goal> to begin an autonomous run.".to_string(),
                ),
            }];
        };

        let mut lines = vec![
            "Pilot".bold().into(),
            format!("Status: {}", pilot_status_label(&state)).into(),
            format!("Goal: {}", state.goal).into(),
            format!("Iterations: {}", state.iteration_count).into(),
            format!("Started: {}", timestamp_label(state.started_at)).into(),
            format!("Updated: {}", timestamp_label(state.updated_at)).into(),
        ];
        if let Some(deadline_at) = state.deadline_at {
            lines.push(format!("Deadline: {}", timestamp_label(deadline_at)).into());
        } else {
            lines.push("Deadline: none".into());
        }
        if let Some(active_turn_id) = state.active_turn_id.as_deref() {
            lines.push(format!("Active turn: {active_turn_id}").into());
        }
        if let Some(last_submitted_turn_id) = state.last_submitted_turn_id.as_deref() {
            lines.push(format!("Last submitted turn: {last_submitted_turn_id}").into());
        }
        if let Some(pending_cycle_kind) = state.pending_cycle_kind {
            lines.push(format!("Pending cycle: {}", pilot_cycle_label(pending_cycle_kind)).into());
        }
        if state.wrap_up_requested {
            lines.push("Wrap-up requested: true".into());
        }
        if let Some(last_cycle_completed_at) = state.last_cycle_completed_at {
            lines.push(
                format!(
                    "Last cycle completed: {}",
                    timestamp_label(last_cycle_completed_at)
                )
                .into(),
            );
        }
        if let Some(last_progress_at) = state.last_progress_at {
            lines.push(format!("Last progress: {}", timestamp_label(last_progress_at)).into());
        }
        if let Some(status_message) = state.status_message.as_deref() {
            lines.push(format!("Status message: {status_message}").into());
        }
        if let Some(last_cycle_summary) = state.last_cycle_summary.as_deref() {
            lines.push(format!("Last cycle summary: {last_cycle_summary}").into());
        }
        if let Some(last_error) = state.last_error.as_deref() {
            lines.push(format!("Last error: {last_error}").red().into());
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn ensure_pilot_runtime(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Result<&mut PilotRuntime, String> {
        if ctx.remote_app_server {
            self.pilot_runtime = None;
            return Err(
                "Pilot runtime is server-owned when connected to a remote app-server.".to_string(),
            );
        }
        let Some(thread_id) = ctx.thread_id.as_deref() else {
            self.pilot_runtime = None;
            return Err("Pilot requires an active thread.".to_string());
        };
        self.pilot_runtime = Some(
            PilotRuntime::load(&ctx.codex_home, thread_id)
                .map_err(|err| format!("Failed to load pilot state: {err}"))?,
        );
        self.pilot_runtime
            .as_mut()
            .ok_or_else(|| "Pilot is unavailable.".to_string())
    }
}

fn pilot_run_owns_turn_abort(run: &AppServerPilotRun, turn_id: Option<&str>) -> bool {
    (run.pending_cycle_kind.is_some()
        && run.active_turn_id.is_none()
        && turn_id.is_none_or(|turn_id| {
            run.last_submitted_turn_id
                .as_deref()
                .is_none_or(|submitted_turn_id| submitted_turn_id == turn_id)
        }))
        || (run.active_turn_id.is_some()
            && turn_id.is_none_or(|turn_id| run.active_turn_id.as_deref() == Some(turn_id)))
}

fn pilot_cycle_effect(next_cycle: PilotCyclePlan) -> SlopForkUiEffect {
    SlopForkUiEffect::SubmitPilotTurn {
        prompt: next_cycle.prompt,
        cycle_kind: next_cycle.kind,
        notify_on_completion: next_cycle.notify_on_completion,
    }
}

fn pilot_status_label(state: &codex_core::slop_fork::pilot::PilotRunState) -> &'static str {
    match state.status {
        PilotStatus::Running => "running",
        PilotStatus::Paused => "paused",
        PilotStatus::Stopped => "stopped",
        PilotStatus::Completed => "completed",
    }
}

fn pilot_cycle_label(kind: PilotCycleKind) -> &'static str {
    match kind {
        PilotCycleKind::Continue => "continue",
        PilotCycleKind::WrapUp => "wrap_up",
    }
}

fn remote_pilot_status_label(run: &codex_app_server_protocol::PilotRun) -> &'static str {
    match run.status {
        AppServerPilotStatus::Running => "running",
        AppServerPilotStatus::Paused => "paused",
        AppServerPilotStatus::Stopped => "stopped",
        AppServerPilotStatus::Completed => "completed",
    }
}

fn remote_pilot_cycle_label(kind: AppServerPilotCycleKind) -> &'static str {
    match kind {
        AppServerPilotCycleKind::Continue => "continue",
        AppServerPilotCycleKind::WrapUp => "wrap_up",
    }
}

fn timestamp_label(timestamp: i64) -> String {
    chrono::TimeZone::timestamp_opt(&Local, timestamp, 0)
        .single()
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| timestamp.to_string())
}

fn recover_idle_stale_pilot_state(
    ctx: &SlopForkUiContext,
    runtime: &mut PilotRuntime,
) -> Result<bool, String> {
    if ctx.task_running {
        return Ok(false);
    }
    if runtime.state().is_some_and(|state| {
        state.pending_cycle_kind.is_some()
            && state.active_turn_id.is_none()
            && state.submission_dispatched_at.is_some_and(|ts| {
                Local::now().timestamp() - ts < PILOT_PENDING_START_RECOVERY_GRACE_SECS
            })
    }) {
        return Ok(false);
    }
    runtime
        .clear_orphaned_cycle_if_idle_for_control(Local::now())
        .map_err(|err| format!("Failed to recover stale Pilot state: {err}"))
}
