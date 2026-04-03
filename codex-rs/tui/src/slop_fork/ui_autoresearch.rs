use super::*;
use codex_app_server_protocol::AutoresearchControlAction as AppServerAutoresearchControlAction;
use codex_app_server_protocol::AutoresearchCycleKind as AppServerAutoresearchCycleKind;
use codex_app_server_protocol::AutoresearchMode as AppServerAutoresearchMode;
use codex_app_server_protocol::AutoresearchStatus as AppServerAutoresearchStatus;
use codex_core::slop_fork::autoresearch::AUTORESEARCH_PLAYBOOK_FILE;
use codex_core::slop_fork::autoresearch::AUTORESEARCH_REPORT_FILE;
use codex_core::slop_fork::autoresearch::AutoresearchApproachEntry;
use codex_core::slop_fork::autoresearch::AutoresearchControllerSnapshot;
use codex_core::slop_fork::autoresearch::AutoresearchCycleKind;
use codex_core::slop_fork::autoresearch::AutoresearchCyclePlan;
use codex_core::slop_fork::autoresearch::AutoresearchJournal;
use codex_core::slop_fork::autoresearch::AutoresearchMode;
use codex_core::slop_fork::autoresearch::AutoresearchParallelWorkspaceManager;
use codex_core::slop_fork::autoresearch::AutoresearchResearchWorkspace;
use codex_core::slop_fork::autoresearch::AutoresearchRunState;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::autoresearch::AutoresearchStatus;
use codex_core::slop_fork::autoresearch::AutoresearchWorkspace;
use codex_core::slop_fork::autoresearch::PortfolioRefreshStatusKind;
use codex_core::slop_fork::autoresearch::PortfolioRefreshTriggerKind;
use codex_core::slop_fork::autoresearch::build_init_prompt;
use codex_core::slop_fork::autoresearch::build_open_init_prompt;
use codex_core::slop_fork::autoresearch::clear_thread_state as clear_autoresearch_thread_state;
use codex_core::slop_fork::autoresearch::load_autoresearch_controller_snapshot;
use codex_core::slop_fork::autoresearch::load_evaluation_governance_settings;
use codex_core::slop_fork::autoresearch::load_stage_progress;
use codex_core::slop_fork::autoresearch::load_validation_policy_settings;
use codex_core::slop_fork::autoresearch::render_governance_line;
use codex_core::slop_fork::autoresearch::render_validation_gate_line;
use codex_core::slop_fork::autoresearch::render_validation_policy_line;
use codex_core::slop_fork::autoresearch::validation_gate_for_status;

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
        if ctx.remote_app_server
            && matches!(
                command,
                AutoresearchCommand::Init { .. } | AutoresearchCommand::Portfolio
            )
        {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch init and portfolio views still require local fork state; use $autoresearch status and remote control commands when connected to a remote app-server."
                    .to_string(),
            )];
        }
        match command {
            AutoresearchCommand::Help => self.show_autoresearch_usage(),
            AutoresearchCommand::Init { request, open_mode } => {
                self.autoresearch_init_setup(ctx, request, open_mode)
            }
            AutoresearchCommand::Status => self.autoresearch_status_output(ctx),
            AutoresearchCommand::Portfolio => self.autoresearch_portfolio_output(ctx),
            AutoresearchCommand::Discover { focus } => self.autoresearch_discover(ctx, focus),
            AutoresearchCommand::Start {
                goal,
                max_runs,
                mode,
            } => self.autoresearch_start(ctx, goal, max_runs, mode),
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
        open_mode: bool,
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
                message: if open_mode {
                    "Autoresearch open-ended setup requested.".to_string()
                } else {
                    "Autoresearch setup requested.".to_string()
                },
                hint: Some(if open_mode {
                    "This turn will scaffold an evaluation-first workspace for research or scientist mode. Run $autoresearch start --mode research or --mode scientist once the setup looks right."
                        .to_string()
                } else {
                    "This turn will scaffold autoresearch.md, benchmark scripts, and metric policy. Run $autoresearch start once the setup looks right."
                            .to_string()
                }),
            },
            SlopForkUiEffect::SubmitAutoresearchSetupTurn {
                prompt: if open_mode {
                    build_open_init_prompt(&request)
                } else {
                    build_init_prompt(&request)
                },
            },
        ]
    }

    pub(crate) fn on_autoresearch_turn_submission_started(
        &mut self,
        ctx: &SlopForkUiContext,
        _cycle_kind: AutoresearchCycleKind,
    ) -> Vec<SlopForkUiEffect> {
        let requires_started_event =
            std::mem::take(&mut self.next_autoresearch_turn_requires_started_event);
        self.awaiting_autoresearch_turn_start = !requires_started_event;
        self.recovered_autoresearch_turn_start = requires_started_event;
        if ctx.remote_app_server {
            return Vec::new();
        }
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
        if ctx.remote_app_server {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch submission failed before the turn could start.".to_string(),
            )];
        }
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
        if ctx.remote_app_server {
            let _ = turn_id;
            return Vec::new();
        }
        if ctx.thread_id.is_none() {
            self.clear_autoresearch_turn_start_expectation();
            return Vec::new();
        }
        let should_claim_pending_cycle =
            if self.awaiting_autoresearch_turn_start || self.recovered_autoresearch_turn_start {
                true
            } else {
                self.ensure_autoresearch_runtime(ctx)
                    .ok()
                    .is_some_and(|runtime| {
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
        if ctx.remote_app_server {
            let _ = (turn_id, last_agent_message);
            return Vec::new();
        }
        let completed = match self.complete_autoresearch_turn(ctx, turn_id, last_agent_message) {
            Ok(completed) => completed,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        if !completed {
            return Vec::new();
        }
        self.autoresearch_follow_up_effects(ctx, AutoresearchTurnStartPolicy::AllowCompletionClaim)
    }

    pub(crate) fn autoresearch_has_active_turn(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
    ) -> bool {
        if ctx.remote_app_server {
            return self
                .remote_autoresearch_run
                .as_ref()
                .and_then(|run| run.active_turn_id.as_deref())
                == Some(turn_id);
        }
        let pending_turn_can_claim_completion = self.awaiting_autoresearch_turn_start;
        self.ensure_autoresearch_runtime(ctx)
            .ok()
            .is_some_and(|runtime| {
                runtime.is_active_turn(turn_id)
                    || (pending_turn_can_claim_completion && runtime.has_pending_turn_start())
            })
    }

    pub(crate) fn on_autoresearch_idle(
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
        self.autoresearch_follow_up_effects(ctx, AutoresearchTurnStartPolicy::AllowCompletionClaim)
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
        if ctx.remote_app_server {
            let _ = (turn_id, reason);
            return Vec::new();
        }
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

    pub(crate) fn on_autoresearch_updated(
        &mut self,
        ctx: &SlopForkUiContext,
        update_type: codex_app_server_protocol::AutoresearchUpdateType,
        run: Option<AppServerAutoresearchRun>,
        message: Option<String>,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            self.remote_autoresearch_run = run.clone();
            self.autoresearch_runtime = None;
            if !from_replay && !self.remote_autoresearch_readback_pending {
                self.remote_autoresearch_bootstrap_pending = false;
            }
            self.remote_autoresearch_state_loaded = true;
            self.clear_autoresearch_turn_start_expectation();
            if from_replay {
                return Vec::new();
            }
        } else if from_replay {
            return Vec::new();
        } else if let Some(thread_id) = ctx.thread_id.as_deref() {
            self.autoresearch_runtime = AutoresearchRuntime::load(&ctx.codex_home, thread_id).ok();
        } else {
            self.autoresearch_runtime = None;
            self.remote_autoresearch_state_loaded = false;
        }
        let message =
            message.or_else(|| fallback_autoresearch_status_message(update_type, run.as_ref()));
        message
            .map(|message| SlopForkUiEffect::AddInfoMessage {
                message,
                hint: None,
            })
            .into_iter()
            .collect()
    }

    fn autoresearch_start(
        &mut self,
        ctx: &SlopForkUiContext,
        goal: String,
        max_runs: Option<u32>,
        mode: AutoresearchMode,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: if mode.is_open_ended() {
                        format!("Autoresearch {} mode start requested.", mode.cli_name())
                    } else {
                        "Autoresearch start requested.".to_string()
                    },
                    hint: Some(
                        "The connected app-server owns Autoresearch state and will refresh the cached run after the request completes."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::StartRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    goal,
                    max_runs,
                    mode: autoresearch_mode_to_remote(mode),
                },
            ];
        }
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
        if mode.is_open_ended()
            && let Err(err) =
                AutoresearchResearchWorkspace::prepare(&ctx.codex_home, thread_id, &ctx.cwd)
        {
            return vec![SlopForkUiEffect::AddErrorMessage(err)];
        }
        let Some(runtime) = self.autoresearch_runtime.as_mut() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch runtime should be loaded before start.".to_string(),
            )];
        };
        match runtime.start(
            goal,
            mode,
            ctx.cwd.clone(),
            prepared_workspace.workspace,
            max_runs,
            Local::now(),
        ) {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: if mode.is_open_ended() {
                        format!("Autoresearch {} mode started.", mode.cli_name())
                    } else {
                        "Autoresearch started.".to_string()
                    },
                    hint: Some(format!("Workspace mode: {}", prepared_workspace.summary)),
                }];
                effects.extend(self.autoresearch_follow_up_effects(
                    ctx,
                    AutoresearchTurnStartPolicy::AllowCompletionClaim,
                ));
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
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            self.clear_autoresearch_turn_start_expectation();
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch pause requested.".to_string(),
                    hint: Some(
                        "The connected app-server will refresh the cached Autoresearch state after the control request completes."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    action: AppServerAutoresearchControlAction::Pause,
                    focus: None,
                },
            ];
        }
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

    fn autoresearch_discover(
        &mut self,
        ctx: &SlopForkUiContext,
        focus: Option<String>,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch discovery requested.".to_string(),
                    hint: Some(
                        "The connected app-server will refresh the cached Autoresearch state after the control request completes."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    action: AppServerAutoresearchControlAction::Discover,
                    focus,
                },
            ];
        }
        let runtime = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match runtime.request_discovery(
            codex_core::slop_fork::autoresearch::AutoresearchDiscoveryReason::UserRequested,
            focus,
            Local::now(),
        ) {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch queued a discovery pass.".to_string(),
                    hint: Some(
                        "The next idle checkpoint will schedule a bounded discovery cycle."
                            .to_string(),
                    ),
                }];
                effects.extend(self.autoresearch_follow_up_effects(
                    ctx,
                    AutoresearchTurnStartPolicy::AllowCompletionClaim,
                ));
                effects
            }
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch could not queue discovery in its current state.".to_string(),
            )],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to queue autoresearch discovery: {err}"
            ))],
        }
    }

    fn autoresearch_resume(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch resume requested.".to_string(),
                    hint: Some(
                        "The connected app-server will decide whether the current Autoresearch session can continue."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    action: AppServerAutoresearchControlAction::Resume,
                    focus: None,
                },
            ];
        }
        let recovered = {
            let runtime = match self.ensure_autoresearch_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            match recover_idle_stale_autoresearch_state_for_control(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            }
        };
        if recovered {
            self.clear_autoresearch_turn_start_expectation();
        }
        let Some(runtime) = self.autoresearch_runtime.as_mut() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch runtime should be loaded before resume.".to_string(),
            )];
        };
        match runtime.resume() {
            Ok(true) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch resumed.".to_string(),
                    hint: None,
                }];
                effects.extend(self.autoresearch_follow_up_effects(
                    ctx,
                    if recovered {
                        AutoresearchTurnStartPolicy::StartedEventRequired
                    } else {
                        AutoresearchTurnStartPolicy::AllowCompletionClaim
                    },
                ));
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
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch wrap-up requested.".to_string(),
                    hint: Some(
                        "The connected app-server will refresh the cached Autoresearch state after the control request completes."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    action: AppServerAutoresearchControlAction::WrapUp,
                    focus: None,
                },
            ];
        }
        let recovered = {
            let runtime = match self.ensure_autoresearch_runtime(ctx) {
                Ok(runtime) => runtime,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            };
            match recover_idle_stale_autoresearch_state_for_control(ctx, runtime) {
                Ok(recovered) => recovered,
                Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
            }
        };
        if recovered {
            self.clear_autoresearch_turn_start_expectation();
        }
        let Some(runtime) = self.autoresearch_runtime.as_mut() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch runtime should be loaded before wrap-up.".to_string(),
            )];
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
                effects.extend(self.autoresearch_follow_up_effects(
                    ctx,
                    if recovered {
                        AutoresearchTurnStartPolicy::StartedEventRequired
                    } else {
                        AutoresearchTurnStartPolicy::AllowCompletionClaim
                    },
                ));
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
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            self.clear_autoresearch_turn_start_expectation();
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch stop requested.".to_string(),
                    hint: Some(
                        "If a server-owned controller turn is already running, it may still finish, but no further cycles should be queued."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    action: AppServerAutoresearchControlAction::Stop,
                    focus: None,
                },
            ];
        }
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
        if ctx.remote_app_server {
            let Some(thread_id) = ctx.thread_id.as_ref() else {
                return vec![SlopForkUiEffect::AddErrorMessage(
                    "Autoresearch requires an active session.".to_string(),
                )];
            };
            self.clear_autoresearch_turn_start_expectation();
            return vec![
                SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch clear requested.".to_string(),
                    hint: Some(
                        "The connected app-server will refresh the cached Autoresearch state after the control request completes."
                            .to_string(),
                    ),
                },
                SlopForkUiEffect::ControlRemoteAutoresearch {
                    thread_id: thread_id.clone(),
                    action: AppServerAutoresearchControlAction::Clear,
                    focus: None,
                },
            ];
        }
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
        if let Some(thread_id) = ctx.thread_id.as_deref()
            && let Err(err) = AutoresearchResearchWorkspace::new(&ctx.codex_home, thread_id).clear()
        {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to clear autoresearch research snapshots: {err}"
            ))];
        }
        if let Some(thread_id) = ctx.thread_id.as_deref()
            && let Err(err) = AutoresearchParallelWorkspaceManager::new(&ctx.codex_home, thread_id)
                .clear_all(runtime.state().and_then(|state| state.workspace.as_ref()))
        {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to clear autoresearch parallel workspaces: {err}"
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
        for generated_file in [AUTORESEARCH_PLAYBOOK_FILE, AUTORESEARCH_REPORT_FILE] {
            match std::fs::remove_file(journal_workdir.join(generated_file)) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to delete {generated_file}: {err}"
                    ))];
                }
            }
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
                "The journal and generated playbook/report artifacts were removed, but autoresearch.md and benchmark scripts were left in place."
                    .to_string(),
            ),
        }]
    }

    fn autoresearch_follow_up_effects(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_start_policy: AutoresearchTurnStartPolicy,
    ) -> Vec<SlopForkUiEffect> {
        self.next_autoresearch_turn_requires_started_event = false;
        if ctx.remote_app_server {
            return Vec::new();
        }
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
        self.next_autoresearch_turn_requires_started_event = matches!(
            turn_start_policy,
            AutoresearchTurnStartPolicy::StartedEventRequired
        );
        vec![autoresearch_cycle_effect(next_cycle)]
    }

    fn autoresearch_status_output(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            if !self.remote_autoresearch_state_loaded {
                return vec![SlopForkUiEffect::AddInfoMessage {
                    message: "Autoresearch remote status is unavailable.".to_string(),
                    hint: Some(
                        "Wait for the server to send or refresh Autoresearch state before requesting status."
                            .to_string(),
                    ),
                }];
            }
            let Some(run) = self.remote_autoresearch_run.as_ref() else {
                return vec![SlopForkUiEffect::AddPlainHistoryLines(vec![
                    "Autoresearch".bold().into(),
                    "Status: idle".into(),
                    "Server state: no Autoresearch run is active for this thread.".into(),
                ])];
            };
            let mut lines = vec![
                "Autoresearch".bold().into(),
                format!("Status: {}", remote_autoresearch_status_label(run.status)).into(),
                format!("Mode: {}", remote_autoresearch_mode_label(run.mode)).into(),
                format!("Goal: {}", run.goal).into(),
                format!("Iterations: {}", run.iteration_count).into(),
                format!("Discovery passes: {}", run.discovery_count).into(),
                format!("Started: {}", timestamp_label(run.started_at)).into(),
                format!("Updated: {}", timestamp_label(run.updated_at)).into(),
            ];
            if let Some(max_runs) = run.max_runs {
                lines.push(format!("Max runs: {max_runs}").into());
            }
            if let Some(active_cycle_kind) = run.active_cycle_kind {
                lines.push(
                    format!(
                        "Active cycle: {}",
                        remote_autoresearch_cycle_label(active_cycle_kind)
                    )
                    .into(),
                );
            }
            if let Some(pending_cycle_kind) = run.pending_cycle_kind {
                lines.push(
                    format!(
                        "Pending cycle: {}",
                        remote_autoresearch_cycle_label(pending_cycle_kind)
                    )
                    .into(),
                );
            }
            if let Some(active_turn_id) = run.active_turn_id.as_deref() {
                lines.push(format!("Active turn: {active_turn_id}").into());
            }
            if let Some(last_submitted_turn_id) = run.last_submitted_turn_id.as_deref() {
                lines.push(format!("Last submitted turn: {last_submitted_turn_id}").into());
            }
            if run.wrap_up_requested {
                lines.push("Wrap-up requested: true".into());
            }
            if let Some(stop_requested_at) = run.stop_requested_at {
                lines
                    .push(format!("Stop requested: {}", timestamp_label(stop_requested_at)).into());
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
            if let Some(last_discovery_completed_at) = run.last_discovery_completed_at {
                lines.push(
                    format!(
                        "Last discovery completed: {}",
                        timestamp_label(last_discovery_completed_at)
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
        let validation_policy = load_validation_policy_settings(&journal_workdir);
        let governance = load_evaluation_governance_settings(&journal_workdir);
        let controller_snapshot = runtime_state
            .as_ref()
            .filter(|state| state.mode.is_open_ended())
            .map(|state| {
                load_autoresearch_controller_snapshot(
                    &journal_workdir,
                    state,
                    Some(&summary),
                    Local::now().timestamp(),
                )
            });
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
            lines.push(format!("Portfolio: {}", summary.approach_count()).into());
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
                "Hint: Use $autoresearch start --max-runs 50 <goal> for optimize mode, $autoresearch start --mode research <goal> for lighter portfolio search, or $autoresearch start --mode scientist <goal> for hypothesis-driven exploration."
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
            format!("Mode: {}", state.mode.cli_name()).into(),
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
            format!(
                "Portfolio: {} candidates across {} families",
                summary.approach_count(),
                summary.family_count()
            )
            .into(),
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
        if let Some(controller_snapshot) = controller_snapshot.as_ref() {
            append_controller_lines(&mut lines, controller_snapshot, Some(state));
        }
        if state.mode.is_open_ended() {
            lines.push(render_validation_policy_line(&validation_policy).into());
            if let Some(governance_line) = render_governance_line(&governance) {
                lines.push(governance_line.into());
            }
        }
        if let Some(active_approach) = state
            .active_approach_id
            .as_deref()
            .and_then(|approach_id| summary.latest_approach(approach_id))
            .or_else(|| summary.active_approach())
        {
            lines.push(
                format!(
                    "Active approach: {} [{}]",
                    active_approach.latest.approach_id, active_approach.latest.family
                )
                .into(),
            );
            lines.push(format!("Approach title: {}", active_approach.latest.title).into());
            if let Some(lineage) = format_approach_lineage(&active_approach.latest) {
                lines.push(format!("Approach lineage: {lineage}").dim().into());
            }
            let gate = validation_gate_for_status(
                &summary,
                &active_approach.latest.approach_id,
                codex_core::slop_fork::autoresearch::AutoresearchApproachStatus::Winner,
                &validation_policy,
            );
            lines.push(
                render_validation_gate_line(&active_approach.latest.approach_id, &gate).into(),
            );
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
        if !state.pending_parallel_runs.is_empty() {
            lines.push(
                format!(
                    "Pending parallel runs: {}",
                    state.pending_parallel_runs.len()
                )
                .into(),
            );
        }
        if let Some(last_error) = state.last_error.as_deref() {
            lines.push(format!("Last error: {last_error}").red().into());
        }
        for approach in summary.current_segment_approaches.iter().take(3) {
            let best = approach
                .best_metric
                .map(format_metric)
                .unwrap_or_else(|| "n/a".to_string());
            lines.push(
                format!(
                    "Approach {}: {} [{}] status={} best={}",
                    approach.latest.approach_id,
                    approach.latest.title,
                    approach.latest.family,
                    approach.latest.status.as_str(),
                    best
                )
                .into(),
            );
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn autoresearch_portfolio_output(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        if ctx.thread_id.is_none() {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Autoresearch requires an active session.".to_string(),
            )];
        }
        let journal_workdir = match self.ensure_autoresearch_runtime(ctx) {
            Ok(runtime) => runtime
                .state()
                .map(|state| state.workdir.clone())
                .unwrap_or_else(|| ctx.cwd.clone()),
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        let journal = match AutoresearchJournal::load(&journal_workdir) {
            Ok(journal) => journal,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load autoresearch journal: {err}"
                ))];
            }
        };
        let summary = journal.summary();
        let validation_policy = load_validation_policy_settings(&journal_workdir);
        let governance = load_evaluation_governance_settings(&journal_workdir);
        let runtime_state = self
            .autoresearch_runtime
            .as_ref()
            .and_then(|runtime| runtime.state())
            .cloned();
        let controller_snapshot = runtime_state
            .as_ref()
            .filter(|state| state.mode.is_open_ended())
            .map(|state| {
                load_autoresearch_controller_snapshot(
                    &journal_workdir,
                    state,
                    Some(&summary),
                    Local::now().timestamp(),
                )
            });
        let mut lines = vec![
            "Autoresearch Portfolio".bold().into(),
            format!("Candidates: {}", summary.approach_count()).into(),
            format!("Families: {}", summary.family_count()).into(),
        ];
        if let Some(controller_snapshot) = controller_snapshot.as_ref() {
            append_controller_lines(&mut lines, controller_snapshot, runtime_state.as_ref());
        }
        if runtime_state
            .as_ref()
            .is_some_and(|state| state.mode.is_open_ended())
        {
            lines.push(render_validation_policy_line(&validation_policy).into());
            if let Some(governance_line) = render_governance_line(&governance) {
                lines.push(governance_line.into());
            }
        }
        if summary.current_segment_approaches.is_empty() {
            lines.push("No tracked approaches yet.".into());
        } else {
            for approach in &summary.current_segment_approaches {
                let best = approach
                    .best_metric
                    .map(format_metric)
                    .unwrap_or_else(|| "n/a".to_string());
                lines.push(
                    format!(
                        "{} [{}] status={} best={} runs={} keeps={}",
                        approach.latest.approach_id,
                        approach.latest.family,
                        approach.latest.status.as_str(),
                        best,
                        approach.total_runs,
                        approach.keep_count
                    )
                    .into(),
                );
                lines.push(format!("  {}", approach.latest.title).dim().into());
                if let Some(lineage) = format_approach_lineage(&approach.latest) {
                    lines.push(format!("  {lineage}").dim().into());
                }
                let gate = validation_gate_for_status(
                    &summary,
                    &approach.latest.approach_id,
                    codex_core::slop_fork::autoresearch::AutoresearchApproachStatus::Winner,
                    &validation_policy,
                );
                lines.push(
                    format!(
                        "  {}",
                        render_validation_gate_line(&approach.latest.approach_id, &gate)
                    )
                    .dim()
                    .into(),
                );
            }
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn ensure_autoresearch_runtime(
        &mut self,
        ctx: &SlopForkUiContext,
    ) -> Result<&mut AutoresearchRuntime, String> {
        if ctx.remote_app_server {
            self.autoresearch_runtime = None;
            return Err(
                "Autoresearch runtime is server-owned when connected to a remote app-server."
                    .to_string(),
            );
        }
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
        self.next_autoresearch_turn_requires_started_event = false;
    }

    fn claim_submitted_autoresearch_turn(&mut self, turn_id: &str) -> Result<bool, String> {
        if !self.awaiting_autoresearch_turn_start {
            return Ok(false);
        }
        self.clear_autoresearch_turn_start_expectation();
        let Some(runtime) = self.autoresearch_runtime.as_mut() else {
            return Ok(false);
        };
        if !runtime.has_pending_turn_start() {
            return Ok(false);
        }
        runtime
            .note_turn_submitted(turn_id)
            .map_err(|err| format!("Failed to recover autoresearch turn id: {err}"))?;
        runtime
            .activate_pending_cycle(turn_id.to_string())
            .map_err(|err| format!("Failed to recover autoresearch cycle: {err}"))
    }

    fn complete_autoresearch_turn(
        &mut self,
        ctx: &SlopForkUiContext,
        turn_id: &str,
        last_agent_message: &str,
    ) -> Result<bool, String> {
        let completed = self
            .ensure_autoresearch_runtime(ctx)?
            .complete_turn(turn_id, last_agent_message, Local::now())
            .map_err(|err| format!("Failed to update autoresearch state: {err}"))?;
        if completed {
            return Ok(true);
        }
        if !self.claim_submitted_autoresearch_turn(turn_id)? {
            return Ok(false);
        }
        self.ensure_autoresearch_runtime(ctx)?
            .complete_turn(turn_id, last_agent_message, Local::now())
            .map_err(|err| format!("Failed to update autoresearch state: {err}"))
    }
}

enum AutoresearchTurnStartPolicy {
    AllowCompletionClaim,
    StartedEventRequired,
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

fn autoresearch_mode_to_remote(mode: AutoresearchMode) -> AppServerAutoresearchMode {
    match mode {
        AutoresearchMode::Optimize => AppServerAutoresearchMode::Optimize,
        AutoresearchMode::Research => AppServerAutoresearchMode::Research,
        AutoresearchMode::Scientist => AppServerAutoresearchMode::Scientist,
    }
}

fn remote_autoresearch_status_label(status: AppServerAutoresearchStatus) -> &'static str {
    match status {
        AppServerAutoresearchStatus::Running => "running",
        AppServerAutoresearchStatus::Paused => "paused",
        AppServerAutoresearchStatus::Stopped => "stopped",
        AppServerAutoresearchStatus::Completed => "completed",
    }
}

fn remote_autoresearch_mode_label(mode: AppServerAutoresearchMode) -> &'static str {
    match mode {
        AppServerAutoresearchMode::Optimize => "optimize",
        AppServerAutoresearchMode::Research => "research",
        AppServerAutoresearchMode::Scientist => "scientist",
    }
}

fn remote_autoresearch_cycle_label(kind: AppServerAutoresearchCycleKind) -> &'static str {
    match kind {
        AppServerAutoresearchCycleKind::Continue => "continue",
        AppServerAutoresearchCycleKind::Research => "research",
        AppServerAutoresearchCycleKind::Discovery => "discovery",
        AppServerAutoresearchCycleKind::WrapUp => "wrap-up",
    }
}

fn timestamp_label(timestamp: i64) -> String {
    chrono::TimeZone::timestamp_opt(&Local, timestamp, 0)
        .single()
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| timestamp.to_string())
}

fn append_controller_lines(
    lines: &mut Vec<Line<'static>>,
    snapshot: &AutoresearchControllerSnapshot,
    state: Option<&AutoresearchRunState>,
) {
    let selection_policy = &snapshot.selection_policy.policy;
    let selection_label = if snapshot.selection_policy.has_custom_values {
        "Selection policy (custom)"
    } else {
        "Selection policy"
    };
    lines.push(
        format!(
            "{selection_label}: weak_gap={} stagnation_window={} synthesis_after={}",
            selection_policy.weak_branch_score_gap,
            selection_policy.stagnation_window,
            selection_policy.synthesis_after_exploit_cycles
        )
        .into(),
    );
    append_policy_issue_lines(
        lines,
        "Selection policy warning",
        &snapshot.selection_policy.issues,
    );

    let refresh_policy = &snapshot.portfolio_refresh_policy.policy;
    let refresh_label = if snapshot.portfolio_refresh_policy.has_custom_values {
        "Portfolio refresh policy (custom)"
    } else {
        "Portfolio refresh policy"
    };
    lines.push(
        format!(
            "{refresh_label}: min_families={} low_diversity_cycles={} exploit_cycles={} cooldown={}s",
            refresh_policy.minimum_family_count,
            refresh_policy.low_diversity_exploit_cycles,
            refresh_policy.exploit_cycles,
            refresh_policy.cooldown_seconds
        )
        .into(),
    );
    lines.push(
        format!(
            "Portfolio refresh status: {}",
            format_portfolio_refresh_status(snapshot, state)
        )
        .into(),
    );
    append_policy_issue_lines(
        lines,
        "Portfolio refresh warning",
        &snapshot.portfolio_refresh_policy.issues,
    );
}

fn append_policy_issue_lines(lines: &mut Vec<Line<'static>>, label: &str, issues: &[String]) {
    for issue in issues {
        lines.push(format!("{label}: {issue}").red().into());
    }
}

fn format_portfolio_refresh_status(
    snapshot: &AutoresearchControllerSnapshot,
    state: Option<&AutoresearchRunState>,
) -> String {
    let status = &snapshot.portfolio_refresh_status;
    let trigger = match status.trigger {
        PortfolioRefreshTriggerKind::Bootstrap => "bootstrap",
        PortfolioRefreshTriggerKind::LowDiversity => "low-diversity",
        PortfolioRefreshTriggerKind::Standard => "standard",
    };
    match status.kind {
        PortfolioRefreshStatusKind::Ready => {
            let family_context = status.family_count.map_or_else(
                || " before any candidates exist".to_string(),
                |count| {
                    format!(
                        " after {} exploit cycles across {count} families",
                        status.current_exploit_cycles
                    )
                },
            );
            format!("ready now via {trigger}{family_context}")
        }
        PortfolioRefreshStatusKind::WaitingForExploitCycles => format!(
            "waiting for {} more exploit cycles via {trigger} ({} / {}, families={})",
            status
                .required_exploit_cycles
                .saturating_sub(status.current_exploit_cycles),
            status.current_exploit_cycles,
            status.required_exploit_cycles,
            status.family_count.unwrap_or(0)
        ),
        PortfolioRefreshStatusKind::CoolingDown => format!(
            "cooling down for {}s via {trigger}",
            status
                .cooldown_remaining_seconds
                .unwrap_or(status.cooldown_seconds)
        ),
        PortfolioRefreshStatusKind::BootstrapComplete => {
            "bootstrap already completed; waiting for portfolio growth".to_string()
        }
        PortfolioRefreshStatusKind::Suppressed => {
            if state.is_some_and(|state| state.wrap_up_requested) {
                "suppressed during wrap-up".to_string()
            } else {
                "suppressed outside active open-ended mode".to_string()
            }
        }
    }
}

fn format_metric(metric: f64) -> String {
    if metric.fract() == 0.0 {
        format!("{}", metric as i64)
    } else {
        format!("{metric:.4}")
    }
}

fn format_approach_lineage(approach: &AutoresearchApproachEntry) -> Option<String> {
    if approach.synthesis_parent_approach_ids.len() == 2 {
        return Some(format!(
            "synthesized from {} + {}",
            approach.synthesis_parent_approach_ids[0], approach.synthesis_parent_approach_ids[1]
        ));
    }
    approach
        .parent_approach_id
        .as_deref()
        .map(|parent_approach_id| format!("derived from {parent_approach_id}"))
}

fn recover_idle_stale_autoresearch_state(
    ctx: &SlopForkUiContext,
    runtime: &mut AutoresearchRuntime,
) -> Result<bool, String> {
    recover_idle_stale_autoresearch_state_impl(ctx, runtime, AutoresearchStaleRecoveryMode::Passive)
}

fn recover_idle_stale_autoresearch_state_for_control(
    ctx: &SlopForkUiContext,
    runtime: &mut AutoresearchRuntime,
) -> Result<bool, String> {
    recover_idle_stale_autoresearch_state_impl(
        ctx,
        runtime,
        AutoresearchStaleRecoveryMode::ExplicitControl,
    )
}

enum AutoresearchStaleRecoveryMode {
    Passive,
    ExplicitControl,
}

fn recover_idle_stale_autoresearch_state_impl(
    ctx: &SlopForkUiContext,
    runtime: &mut AutoresearchRuntime,
    mode: AutoresearchStaleRecoveryMode,
) -> Result<bool, String> {
    if ctx.task_running {
        return Ok(false);
    }
    if matches!(mode, AutoresearchStaleRecoveryMode::Passive)
        && runtime.has_pending_turn_start()
        && runtime.state().is_some_and(|state| {
            !matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            )
        })
    {
        return Ok(false);
    }
    if matches!(mode, AutoresearchStaleRecoveryMode::ExplicitControl) {
        runtime
            .clear_orphaned_cycle_if_idle_for_control(Local::now())
            .map_err(|err| format!("Failed to recover stale autoresearch state: {err}"))
    } else {
        runtime
            .clear_orphaned_cycle_if_idle(Local::now())
            .map_err(|err| format!("Failed to recover stale autoresearch state: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::format_approach_lineage;
    use codex_core::slop_fork::autoresearch::AutoresearchApproachEntry;
    use codex_core::slop_fork::autoresearch::AutoresearchApproachStatus;
    use pretty_assertions::assert_eq;

    fn sample_approach() -> AutoresearchApproachEntry {
        AutoresearchApproachEntry {
            entry_type: "approach".to_string(),
            approach_id: "approach-4".to_string(),
            title: "synthesis".to_string(),
            family: "retrieval+distillation".to_string(),
            status: AutoresearchApproachStatus::Planned,
            summary: "controller-created".to_string(),
            rationale: String::new(),
            risks: Vec::new(),
            sources: Vec::new(),
            parent_approach_id: None,
            synthesis_parent_approach_ids: Vec::new(),
            timestamp: 1,
            segment: 0,
        }
    }

    #[test]
    fn format_approach_lineage_prefers_synthesis_pair() {
        let mut approach = sample_approach();
        approach.parent_approach_id = Some("approach-2".to_string());
        approach.synthesis_parent_approach_ids =
            vec!["approach-2".to_string(), "approach-3".to_string()];

        assert_eq!(
            format_approach_lineage(&approach).as_deref(),
            Some("synthesized from approach-2 + approach-3")
        );
    }

    #[test]
    fn format_approach_lineage_falls_back_to_single_parent() {
        let mut approach = sample_approach();
        approach.parent_approach_id = Some("approach-2".to_string());

        assert_eq!(
            format_approach_lineage(&approach).as_deref(),
            Some("derived from approach-2")
        );
    }
}
