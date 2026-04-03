use super::*;
use codex_app_server_protocol::AutomationDefinition as AppServerAutomationDefinition;
use codex_app_server_protocol::AutomationLimits as AppServerAutomationLimits;
use codex_app_server_protocol::AutomationMessageSource as AppServerAutomationMessageSource;
use codex_app_server_protocol::AutomationPolicyCommand as AppServerAutomationPolicyCommand;
use codex_app_server_protocol::AutomationScope as AppServerAutomationScope;
use codex_app_server_protocol::AutomationTrigger as AppServerAutomationTrigger;

#[derive(Clone, Copy)]
enum AutomationCreateInitialRun {
    Immediate,
    Deferred,
    SuppressedByDisabled,
}

impl SlopForkUi {
    pub(crate) fn show_auto_usage(&self) -> Vec<SlopForkUiEffect> {
        vec![SlopForkUiEffect::AddPlainHistoryLines(
            auto_usage().lines().map(Line::from).collect(),
        )]
    }

    pub(crate) fn handle_auto_command(
        &mut self,
        ctx: &SlopForkUiContext,
        trimmed: &str,
        last_user_message: Option<&str>,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            let command = match parse_auto_command(
                trimmed,
                AutomationScope::Session,
                Local::now(),
                last_user_message,
            ) {
                Ok(command) => command,
                Err(message) => return vec![SlopForkUiEffect::AddErrorMessage(message)],
            };
            return match command {
                AutoCommand::Help => self.show_auto_usage(),
                AutoCommand::List => self.remote_auto_status_output(),
                AutoCommand::Show { runtime_id } => self.remote_auto_show_output(&runtime_id),
                AutoCommand::Pause { runtime_id } => {
                    self.remote_auto_set_enabled(ctx, &runtime_id, /*enabled*/ false)
                }
                AutoCommand::Resume { runtime_id } => {
                    self.remote_auto_set_enabled(ctx, &runtime_id, /*enabled*/ true)
                }
                AutoCommand::Remove { runtime_id } => self.remote_auto_remove(ctx, &runtime_id),
                AutoCommand::Create {
                    scope,
                    spec,
                    note,
                    send_now,
                } => self.remote_auto_create(ctx, scope, spec, note, send_now),
            };
        }
        let fork_config = match load_slop_fork_config(&ctx.codex_home) {
            Ok(config) => config,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load fork config: {err}"
                ))];
            }
        };
        let command = match parse_auto_command(
            trimmed,
            fork_config.automation_default_scope,
            Local::now(),
            last_user_message,
        ) {
            Ok(command) => command,
            Err(message) => return vec![SlopForkUiEffect::AddErrorMessage(message)],
        };
        match command {
            AutoCommand::Help => self.show_auto_usage(),
            AutoCommand::List => self.auto_status_output(ctx),
            AutoCommand::Show { runtime_id } => self.auto_show_output(ctx, &runtime_id),
            AutoCommand::Pause { runtime_id } => {
                self.auto_set_paused(ctx, &runtime_id, /*paused*/ true)
            }
            AutoCommand::Resume { runtime_id } => {
                self.auto_set_paused(ctx, &runtime_id, /*paused*/ false)
            }
            AutoCommand::Remove { runtime_id } => self.auto_remove(ctx, &runtime_id),
            AutoCommand::Create {
                scope,
                spec,
                note,
                send_now,
            } => self.auto_create(
                &fork_config,
                ctx,
                scope,
                spec,
                note,
                if !fork_config.automation_enabled {
                    AutomationCreateInitialRun::SuppressedByDisabled
                } else if send_now {
                    AutomationCreateInitialRun::Immediate
                } else {
                    AutomationCreateInitialRun::Deferred
                },
            ),
        }
    }

    pub(crate) fn poll_timer_automations(
        &mut self,
        ctx: &SlopForkUiContext,
        now: DateTime<Local>,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            return Vec::new();
        }
        let Ok(fork_config) = load_slop_fork_config(&ctx.codex_home) else {
            return Vec::new();
        };
        if !fork_config.automation_enabled {
            return Vec::new();
        }

        let Ok(registry) = self.ensure_automation_registry(ctx) else {
            return Vec::new();
        };
        let actions = registry.prepare_actions(AutomationEvaluationTrigger::Timer, now);
        let Ok(actions) = actions else {
            return Vec::new();
        };

        self.dispatch_automation_actions(&fork_config, ctx, actions)
    }

    pub(crate) fn automation_frame_effects(
        &mut self,
        ctx: &SlopForkUiContext,
        now: DateTime<Local>,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            return Vec::new();
        }
        let Ok(fork_config) = load_slop_fork_config(&ctx.codex_home) else {
            return Vec::new();
        };
        if !fork_config.automation_enabled {
            return Vec::new();
        }

        let Ok(registry) = self.ensure_automation_registry(ctx) else {
            return Vec::new();
        };
        registry
            .next_wake_in(now)
            .ok()
            .flatten()
            .map(SlopForkUiEffect::ScheduleFrameIn)
            .into_iter()
            .collect()
    }

    pub(crate) fn on_turn_completed(
        &mut self,
        ctx: &SlopForkUiContext,
        last_agent_message: &str,
        from_replay: bool,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            return Vec::new();
        }
        if from_replay || ctx.thread_id.is_none() {
            return Vec::new();
        }

        let fork_config = match load_slop_fork_config(&ctx.codex_home) {
            Ok(config) => config,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to load fork config: {err}"
                ))];
            }
        };
        if !fork_config.automation_enabled {
            return Vec::new();
        }

        let actions = match self.ensure_automation_registry(ctx).and_then(|registry| {
            registry
                .prepare_actions(
                    AutomationEvaluationTrigger::TurnCompleted {
                        turn_id: None,
                        last_agent_message: last_agent_message.to_string(),
                    },
                    Local::now(),
                )
                .map_err(|err| format!("Failed to evaluate automations: {err}"))
        }) {
            Ok(actions) => actions,
            Err(err) => {
                return vec![SlopForkUiEffect::AddErrorMessage(err)];
            }
        };

        self.dispatch_automation_actions(&fork_config, ctx, actions)
    }

    fn dispatch_automation_actions(
        &mut self,
        fork_config: &SlopForkConfig,
        ctx: &SlopForkUiContext,
        actions: Vec<AutomationPreparedAction>,
    ) -> Vec<SlopForkUiEffect> {
        let mut effects = Vec::new();
        for action in actions {
            match action {
                AutomationPreparedAction::Send {
                    runtime_id,
                    message,
                } => {
                    if let Ok(registry) = self.ensure_automation_registry(ctx) {
                        let _ = registry.record_delivery(&runtime_id, Local::now());
                    }
                    effects.push(SlopForkUiEffect::QueueAutomationPrompt {
                        prompt: message,
                        suppress_legacy_notify: fork_config.automation_disable_notify_script,
                        suppress_terminal_notification: fork_config
                            .automation_disable_terminal_notifications,
                    });
                }
                AutomationPreparedAction::RunPolicy(policy) => {
                    let Some(thread_id) = ctx.thread_id.as_ref() else {
                        continue;
                    };
                    if self
                        .pending_automation_policies
                        .contains(&(thread_id.clone(), policy.runtime_id.clone()))
                    {
                        continue;
                    }
                    self.spawn_automation_policy_task(
                        ctx,
                        *policy,
                        fork_config.automation_shell_timeout_ms,
                    );
                }
            }
        }
        effects
    }

    fn auto_status_output(&mut self, ctx: &SlopForkUiContext) -> Vec<SlopForkUiEffect> {
        let registry = match self.ensure_automation_registry(ctx) {
            Ok(registry) => registry,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        let entries = registry.list_entries();
        if entries.is_empty() {
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "No automations configured.".to_string(),
                hint: Some(
                    "Use $auto on-complete \"continue working on this\" to create one.".to_string(),
                ),
            }];
        }

        let mut lines = vec![
            "Automations".bold().into(),
            "Configured follow-up rules for this session.".dim().into(),
            "".into(),
        ];
        for entry in entries {
            let status = if entry.state.stopped {
                "stopped"
            } else if entry.state.paused {
                "paused"
            } else if entry.spec.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let trigger = match &entry.spec.trigger {
                codex_core::slop_fork::automation::AutomationTrigger::TurnCompleted => {
                    "on-complete".to_string()
                }
                codex_core::slop_fork::automation::AutomationTrigger::Interval {
                    every_seconds,
                } => {
                    format!(
                        "every {}",
                        compact_duration_label(Duration::from_secs(*every_seconds))
                    )
                }
                codex_core::slop_fork::automation::AutomationTrigger::Cron { expression } => {
                    format!("cron {expression}")
                }
            };
            let source = match &entry.spec.message_source {
                codex_core::slop_fork::automation::AutomationMessageSource::Static { message } => {
                    message.clone()
                }
                codex_core::slop_fork::automation::AutomationMessageSource::RoundRobin {
                    messages,
                } => {
                    format!("round-robin {} messages", messages.len())
                }
            };
            lines.push(Line::from(vec![
                entry.runtime_id.cyan().bold(),
                " ".into(),
                format!("[{status}]").dim(),
                " ".into(),
                trigger.into(),
            ]));
            lines.push(Line::from(format!(
                "  source: {source} | runs: {}",
                entry.state.run_count
            )));
            if let Some(last_error) = entry.state.last_error.as_deref() {
                lines.push(format!("  last error: {last_error}").red().into());
            }
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn auto_show_output(
        &mut self,
        ctx: &SlopForkUiContext,
        runtime_id_to_show: &str,
    ) -> Vec<SlopForkUiEffect> {
        let registry = match self.ensure_automation_registry(ctx) {
            Ok(registry) => registry,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        let Some(entry) = registry
            .list_entries()
            .into_iter()
            .find(|entry| entry.runtime_id == runtime_id_to_show)
        else {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "No automation with id {runtime_id_to_show}."
            ))];
        };
        let mut lines = vec![
            entry.runtime_id.cyan().bold().into(),
            format!("Scope: {:?}", entry.scope).into(),
            format!("Enabled: {}", entry.spec.enabled).into(),
            format!("Paused: {}", entry.state.paused).into(),
            format!("Stopped: {}", entry.state.stopped).into(),
            format!("Runs: {}", entry.state.run_count).into(),
        ];
        match &entry.spec.trigger {
            codex_core::slop_fork::automation::AutomationTrigger::TurnCompleted => {
                lines.push("Trigger: on-complete".into());
            }
            codex_core::slop_fork::automation::AutomationTrigger::Interval { every_seconds } => {
                lines.push(
                    format!(
                        "Trigger: every {}",
                        compact_duration_label(Duration::from_secs(*every_seconds))
                    )
                    .into(),
                );
            }
            codex_core::slop_fork::automation::AutomationTrigger::Cron { expression } => {
                lines.push(format!("Trigger: cron {expression}").into());
            }
        }
        match &entry.spec.message_source {
            codex_core::slop_fork::automation::AutomationMessageSource::Static { message } => {
                lines.push(format!("Message: {message}").into());
            }
            codex_core::slop_fork::automation::AutomationMessageSource::RoundRobin { messages } => {
                lines.push("Messages:".into());
                lines.extend(
                    messages
                        .iter()
                        .map(|message| Line::from(format!("  - {message}"))),
                );
            }
        }
        if let Some(max_runs) = entry.spec.limits.max_runs {
            lines.push(format!("Max runs: {max_runs}").into());
        }
        if let Some(until_at) = entry.spec.limits.until_at {
            lines.push(format!("Until: {until_at}").into());
        }
        if let Some(policy_command) = entry.spec.policy_command.as_ref() {
            lines.push(format!("Policy: {}", policy_command.command.join(" ")).into());
        }
        if let Some(last_error) = entry.state.last_error.as_deref() {
            lines.push(format!("Last error: {last_error}").red().into());
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn auto_set_paused(
        &mut self,
        ctx: &SlopForkUiContext,
        runtime_id_to_update: &str,
        paused: bool,
    ) -> Vec<SlopForkUiEffect> {
        let registry = match self.ensure_automation_registry(ctx) {
            Ok(registry) => registry,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match registry.set_paused(runtime_id_to_update, paused) {
            Ok(true) => vec![SlopForkUiEffect::AddInfoMessage {
                message: if paused {
                    format!("Paused automation {runtime_id_to_update}.")
                } else {
                    format!("Resumed automation {runtime_id_to_update}.")
                },
                hint: None,
            }],
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "No automation with id {runtime_id_to_update}."
            ))],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to update automation {runtime_id_to_update}: {err}"
            ))],
        }
    }

    fn auto_remove(
        &mut self,
        ctx: &SlopForkUiContext,
        runtime_id_to_remove: &str,
    ) -> Vec<SlopForkUiEffect> {
        let registry = match self.ensure_automation_registry(ctx) {
            Ok(registry) => registry,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match registry.remove(runtime_id_to_remove) {
            Ok(true) => vec![SlopForkUiEffect::AddInfoMessage {
                message: format!("Removed automation {runtime_id_to_remove}."),
                hint: None,
            }],
            Ok(false) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "No automation with id {runtime_id_to_remove}."
            ))],
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to remove automation {runtime_id_to_remove}: {err}"
            ))],
        }
    }

    fn auto_create(
        &mut self,
        fork_config: &SlopForkConfig,
        ctx: &SlopForkUiContext,
        scope: AutomationScope,
        spec: AutomationSpec,
        note: Option<String>,
        initial_run: AutomationCreateInitialRun,
    ) -> Vec<SlopForkUiEffect> {
        let registry = match self.ensure_automation_registry(ctx) {
            Ok(registry) => registry,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        match registry.upsert(scope, spec, Local::now()) {
            Ok(entry) => {
                let mut effects = vec![SlopForkUiEffect::AddInfoMessage {
                    message: format!("Created automation {}.", entry.runtime_id),
                    hint: Some(match (note, entry.scope, initial_run) {
                        (
                            Some(note),
                            scope,
                            AutomationCreateInitialRun::SuppressedByDisabled,
                        ) => match scope {
                            AutomationScope::Session => format!(
                                "{note} Automation execution is currently disabled, so it was saved without running now. Session scope only. It will disappear when this conversation ends."
                            ),
                            AutomationScope::Repo => format!(
                                "{note} Automation execution is currently disabled, so it was saved without running now. Repo scope is saved for future conversations in this repository."
                            ),
                            AutomationScope::Global => format!(
                                "{note} Automation execution is currently disabled, so it was saved without running now. Global scope is saved for future conversations everywhere."
                            ),
                        },
                        (Some(note), AutomationScope::Session, AutomationCreateInitialRun::Immediate) => format!(
                            "{note} Queued the first run immediately. Session scope only. It will disappear when this conversation ends."
                        ),
                        (Some(note), AutomationScope::Repo, AutomationCreateInitialRun::Immediate) => format!(
                            "{note} Queued the first run immediately. Repo scope is saved for future conversations in this repository."
                        ),
                        (Some(note), AutomationScope::Global, AutomationCreateInitialRun::Immediate) => format!(
                            "{note} Queued the first run immediately. Global scope is saved for future conversations everywhere."
                        ),
                        (Some(note), AutomationScope::Session, AutomationCreateInitialRun::Deferred) => format!(
                            "{note} Session scope only. It will disappear when this conversation ends."
                        ),
                        (Some(note), AutomationScope::Repo, AutomationCreateInitialRun::Deferred) => format!(
                            "{note} Repo scope is saved for future conversations in this repository."
                        ),
                        (Some(note), AutomationScope::Global, AutomationCreateInitialRun::Deferred) => {
                            format!(
                                "{note} Global scope is saved for future conversations everywhere."
                            )
                        }
                        (
                            None,
                            AutomationScope::Session,
                            AutomationCreateInitialRun::SuppressedByDisabled,
                        ) => {
                            "Automation execution is currently disabled, so it was saved without running now. Session scope only. It will disappear when this conversation ends."
                                .to_string()
                        }
                        (
                            None,
                            AutomationScope::Repo,
                            AutomationCreateInitialRun::SuppressedByDisabled,
                        ) => {
                            "Automation execution is currently disabled, so it was saved without running now. Repo scope is saved for future conversations in this repository."
                                .to_string()
                        }
                        (
                            None,
                            AutomationScope::Global,
                            AutomationCreateInitialRun::SuppressedByDisabled,
                        ) => {
                            "Automation execution is currently disabled, so it was saved without running now. Global scope is saved for future conversations everywhere."
                                .to_string()
                        }
                        (None, AutomationScope::Session, AutomationCreateInitialRun::Immediate) => {
                            "Queued the first run immediately. Session scope only. It will disappear when this conversation ends."
                                .to_string()
                        }
                        (None, AutomationScope::Repo, AutomationCreateInitialRun::Immediate) => {
                            "Queued the first run immediately. Repo scope is saved for future conversations in this repository."
                                .to_string()
                        }
                        (None, AutomationScope::Global, AutomationCreateInitialRun::Immediate) => {
                            "Queued the first run immediately. Global scope is saved for future conversations everywhere."
                                .to_string()
                        }
                        (None, AutomationScope::Session, AutomationCreateInitialRun::Deferred) => {
                            "Session scope only. It will disappear when this conversation ends."
                                .to_string()
                        }
                        (None, AutomationScope::Repo, AutomationCreateInitialRun::Deferred) => {
                            "Repo scope is saved for future conversations in this repository."
                                .to_string()
                        }
                        (None, AutomationScope::Global, AutomationCreateInitialRun::Deferred) => {
                            "Global scope is saved for future conversations everywhere.".to_string()
                        }
                    }),
                }];
                if matches!(initial_run, AutomationCreateInitialRun::Immediate) {
                    let message = match &entry.spec.message_source {
                        codex_core::slop_fork::automation::AutomationMessageSource::Static {
                            message,
                        } => message.clone(),
                        codex_core::slop_fork::automation::AutomationMessageSource::RoundRobin {
                            messages,
                        } => messages
                            .get(entry.state.round_robin_index % messages.len().max(1))
                            .cloned()
                            .unwrap_or_default(),
                    };
                    if let Err(err) = registry.record_delivery(&entry.runtime_id, Local::now()) {
                        return vec![SlopForkUiEffect::AddErrorMessage(format!(
                            "Failed to queue first automation run: {err}"
                        ))];
                    }
                    effects.push(self.automation_prompt_effect(fork_config, message));
                }
                effects
            }
            Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                "Failed to create automation: {err}"
            ))],
        }
    }

    fn remote_auto_create(
        &self,
        ctx: &SlopForkUiContext,
        scope: AutomationScope,
        spec: AutomationSpec,
        note: Option<String>,
        send_now: bool,
    ) -> Vec<SlopForkUiEffect> {
        let Some(thread_id) = ctx.thread_id.as_ref() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Automations require an active session.".to_string(),
            )];
        };
        let mut hint = note.unwrap_or_else(|| {
            "The connected app-server owns automation state and will broadcast the updated entry."
                .to_string()
        });
        if send_now {
            hint.push_str(
                " The first run will still be decided by the server-owned automation policy.",
            );
        }
        vec![
            SlopForkUiEffect::AddInfoMessage {
                message: "Remote automation create requested.".to_string(),
                hint: Some(hint),
            },
            SlopForkUiEffect::UpsertRemoteAutomation {
                thread_id: thread_id.clone(),
                scope: automation_scope_to_remote(scope),
                automation: automation_definition_to_remote(spec),
            },
        ]
    }

    fn remote_auto_set_enabled(
        &self,
        ctx: &SlopForkUiContext,
        runtime_id: &str,
        enabled: bool,
    ) -> Vec<SlopForkUiEffect> {
        let Some(thread_id) = ctx.thread_id.as_ref() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Automations require an active session.".to_string(),
            )];
        };
        vec![
            SlopForkUiEffect::AddInfoMessage {
                message: if enabled {
                    format!("Requested remote automation resume for {runtime_id}.")
                } else {
                    format!("Requested remote automation pause for {runtime_id}.")
                },
                hint: Some(
                    "The remote app-server currently exposes automation enable/disable controls for this path."
                        .to_string(),
                ),
            },
            SlopForkUiEffect::SetRemoteAutomationEnabled {
                thread_id: thread_id.clone(),
                runtime_id: runtime_id.to_string(),
                enabled,
            },
        ]
    }

    fn remote_auto_remove(
        &self,
        ctx: &SlopForkUiContext,
        runtime_id: &str,
    ) -> Vec<SlopForkUiEffect> {
        let Some(thread_id) = ctx.thread_id.as_ref() else {
            return vec![SlopForkUiEffect::AddErrorMessage(
                "Automations require an active session.".to_string(),
            )];
        };
        vec![
            SlopForkUiEffect::AddInfoMessage {
                message: format!("Requested remote automation removal for {runtime_id}."),
                hint: Some(
                    "The connected app-server will broadcast the deletion if it succeeds."
                        .to_string(),
                ),
            },
            SlopForkUiEffect::DeleteRemoteAutomation {
                thread_id: thread_id.clone(),
                runtime_id: runtime_id.to_string(),
            },
        ]
    }

    fn spawn_automation_policy_task(
        &mut self,
        ctx: &SlopForkUiContext,
        prepared: AutomationPreparedPolicy,
        default_timeout_ms: u64,
    ) {
        let Some(thread_id) = ctx.thread_id.clone() else {
            return;
        };
        self.pending_automation_policies
            .insert((thread_id.clone(), prepared.runtime_id.clone()));
        let runtime_id = prepared.runtime_id.clone();
        let tx = ctx.app_event_tx.clone();
        let cwd = ctx.cwd.clone();
        let sandbox_policy = ctx.sandbox_policy.clone();
        let file_system_sandbox_policy = ctx.file_system_sandbox_policy.clone();
        let network_sandbox_policy = ctx.network_sandbox_policy;
        let codex_linux_sandbox_exe = ctx.codex_linux_sandbox_exe.clone();
        let windows_sandbox_level = ctx.windows_sandbox_level;
        let windows_sandbox_private_desktop = ctx.windows_sandbox_private_desktop;
        tokio::spawn(async move {
            let execution = AutomationPolicyExecutionContext {
                session_cwd: cwd,
                sandbox_policy,
                file_system_sandbox_policy,
                network_sandbox_policy,
                codex_linux_sandbox_exe,
                windows_sandbox_level,
                windows_sandbox_private_desktop,
            };
            match run_policy_command(
                &prepared.command,
                &prepared.payload,
                default_timeout_ms,
                &execution,
            )
            .await
            {
                Ok(decision) => tx.send(AppEvent::SlopFork(
                    SlopForkEvent::AutomationPolicyEvaluated {
                        thread_id,
                        runtime_id,
                        decision,
                    },
                )),
                Err(error) => tx.send(AppEvent::SlopFork(SlopForkEvent::AutomationPolicyFailed {
                    thread_id,
                    runtime_id,
                    error,
                })),
            }
        });
    }

    pub(crate) fn on_automation_policy_evaluated(
        &mut self,
        ctx: &SlopForkUiContext,
        thread_id: &str,
        runtime_id_to_update: &str,
        decision: AutomationPolicyDecision,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            self.pending_automation_policies
                .remove(&(thread_id.to_string(), runtime_id_to_update.to_string()));
            return Vec::new();
        }
        self.pending_automation_policies
            .remove(&(thread_id.to_string(), runtime_id_to_update.to_string()));
        if ctx.thread_id.as_deref() != Some(thread_id) {
            return Vec::new();
        }
        let registry = match self.ensure_automation_registry(ctx) {
            Ok(registry) => registry,
            Err(err) => return vec![SlopForkUiEffect::AddErrorMessage(err)],
        };
        let emitted_message = registry.preview_policy_message(runtime_id_to_update, &decision);
        if let Some(message) = emitted_message {
            if let Err(err) =
                registry.apply_policy_decision(runtime_id_to_update, decision, Local::now())
            {
                return vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to apply automation policy result for {runtime_id_to_update}: {err}"
                ))];
            }
            let fork_config = match load_slop_fork_config(&ctx.codex_home) {
                Ok(config) => config,
                Err(err) => {
                    return vec![SlopForkUiEffect::AddErrorMessage(format!(
                        "Failed to load fork config: {err}"
                    ))];
                }
            };
            vec![self.automation_prompt_effect(&fork_config, message)]
        } else {
            match registry.apply_policy_decision(runtime_id_to_update, decision, Local::now()) {
                Ok(Some(_)) | Ok(None) => Vec::new(),
                Err(err) => vec![SlopForkUiEffect::AddErrorMessage(format!(
                    "Failed to apply automation policy result for {runtime_id_to_update}: {err}"
                ))],
            }
        }
    }

    pub(crate) fn on_automation_policy_failed(
        &mut self,
        ctx: &SlopForkUiContext,
        thread_id: &str,
        runtime_id_to_update: &str,
        error: String,
    ) -> Vec<SlopForkUiEffect> {
        if ctx.remote_app_server {
            self.pending_automation_policies
                .remove(&(thread_id.to_string(), runtime_id_to_update.to_string()));
            return Vec::new();
        }
        self.pending_automation_policies
            .remove(&(thread_id.to_string(), runtime_id_to_update.to_string()));
        if ctx.thread_id.as_deref() != Some(thread_id) {
            return Vec::new();
        }
        if let Ok(registry) = self.ensure_automation_registry(ctx) {
            let _ = registry.record_error(runtime_id_to_update, error.clone());
        }
        vec![SlopForkUiEffect::AddErrorMessage(format!(
            "Automation {runtime_id_to_update} paused after policy failure: {error}"
        ))]
    }

    fn automation_prompt_effect(
        &self,
        fork_config: &SlopForkConfig,
        prompt: String,
    ) -> SlopForkUiEffect {
        SlopForkUiEffect::QueueAutomationPrompt {
            prompt,
            suppress_legacy_notify: fork_config.automation_disable_notify_script,
            suppress_terminal_notification: fork_config.automation_disable_terminal_notifications,
        }
    }

    fn remote_auto_status_output(&self) -> Vec<SlopForkUiEffect> {
        if !self.remote_automation_state_loaded {
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "Automation remote status is unavailable.".to_string(),
                hint: Some(
                    "Wait for the server to send automation state before requesting status."
                        .to_string(),
                ),
            }];
        }
        if self.remote_automations.is_empty() {
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "No automations configured.".to_string(),
                hint: Some(
                    "Remote automation state is loaded from the connected app-server.".to_string(),
                ),
            }];
        }

        let mut lines = vec![
            "Automations".bold().into(),
            "Server-owned follow-up rules for this session."
                .dim()
                .into(),
            "".into(),
        ];
        for entry in &self.remote_automations {
            let status = if entry.stopped {
                "stopped"
            } else if entry.paused {
                "paused"
            } else if entry.enabled {
                "enabled"
            } else {
                "disabled"
            };
            let trigger = remote_automation_trigger_label(&entry.trigger);
            let source = remote_automation_source_label(&entry.message_source);
            lines.push(Line::from(vec![
                entry.runtime_id.clone().cyan().bold(),
                " ".into(),
                format!("[{status}]").dim(),
                " ".into(),
                trigger.into(),
            ]));
            lines.push(Line::from(format!(
                "  source: {source} | runs: {}",
                entry.run_count
            )));
            if let Some(last_error) = entry.last_error.as_deref() {
                lines.push(format!("  last error: {last_error}").red().into());
            }
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }

    fn remote_auto_show_output(&self, runtime_id_to_show: &str) -> Vec<SlopForkUiEffect> {
        if !self.remote_automation_state_loaded {
            return vec![SlopForkUiEffect::AddInfoMessage {
                message: "Automation remote status is unavailable.".to_string(),
                hint: Some(
                    "Wait for the server to send automation state before requesting details."
                        .to_string(),
                ),
            }];
        }
        let Some(entry) = self
            .remote_automations
            .iter()
            .find(|entry| entry.runtime_id == runtime_id_to_show)
        else {
            return vec![SlopForkUiEffect::AddErrorMessage(format!(
                "No automation with id {runtime_id_to_show}."
            ))];
        };

        let mut lines = vec![
            entry.runtime_id.clone().cyan().bold().into(),
            format!("Scope: {}", remote_automation_scope_label(entry.scope)).into(),
            format!("Enabled: {}", entry.enabled).into(),
            format!("Paused: {}", entry.paused).into(),
            format!("Stopped: {}", entry.stopped).into(),
            format!("Runs: {}", entry.run_count).into(),
        ];
        lines.push(
            format!(
                "Trigger: {}",
                remote_automation_trigger_label(&entry.trigger)
            )
            .into(),
        );
        match &entry.message_source {
            AppServerAutomationMessageSource::Static { message } => {
                lines.push(format!("Message: {message}").into());
            }
            AppServerAutomationMessageSource::RoundRobin { messages } => {
                lines.push("Messages:".into());
                lines.extend(
                    messages
                        .iter()
                        .map(|message| Line::from(format!("  - {message}"))),
                );
            }
        }
        if let Some(next_fire_at) = entry.next_fire_at {
            lines.push(format!("Next fire: {next_fire_at}").into());
        }
        if let Some(max_runs) = entry.limits.max_runs {
            lines.push(format!("Max runs: {max_runs}").into());
        }
        if let Some(until_at) = entry.limits.until_at {
            lines.push(format!("Until: {until_at}").into());
        }
        if let Some(policy_command) = entry.policy_command.as_ref() {
            lines.push(format!("Policy: {}", policy_command.command.join(" ")).into());
        }
        if let Some(last_error) = entry.last_error.as_deref() {
            lines.push(format!("Last error: {last_error}").red().into());
        }
        vec![SlopForkUiEffect::AddPlainHistoryLines(lines)]
    }
}

fn remote_automation_scope_label(scope: AppServerAutomationScope) -> &'static str {
    match scope {
        AppServerAutomationScope::Session => "session",
        AppServerAutomationScope::Repo => "repo",
        AppServerAutomationScope::Global => "global",
    }
}

fn remote_automation_trigger_label(trigger: &AppServerAutomationTrigger) -> String {
    match trigger {
        AppServerAutomationTrigger::TurnCompleted => "on-complete".to_string(),
        AppServerAutomationTrigger::Interval { every_seconds } => {
            format!(
                "every {}",
                compact_duration_label(Duration::from_secs(*every_seconds))
            )
        }
        AppServerAutomationTrigger::Cron { expression } => format!("cron {expression}"),
    }
}

fn remote_automation_source_label(message_source: &AppServerAutomationMessageSource) -> String {
    match message_source {
        AppServerAutomationMessageSource::Static { message } => message.clone(),
        AppServerAutomationMessageSource::RoundRobin { messages } => {
            format!("round-robin {} messages", messages.len())
        }
    }
}

fn automation_scope_to_remote(scope: AutomationScope) -> AppServerAutomationScope {
    match scope {
        AutomationScope::Session => AppServerAutomationScope::Session,
        AutomationScope::Repo => AppServerAutomationScope::Repo,
        AutomationScope::Global => AppServerAutomationScope::Global,
    }
}

fn automation_definition_to_remote(spec: AutomationSpec) -> AppServerAutomationDefinition {
    AppServerAutomationDefinition {
        id: (!spec.id.is_empty()).then_some(spec.id),
        enabled: spec.enabled,
        trigger: automation_trigger_to_remote(spec.trigger),
        message_source: automation_message_source_to_remote(spec.message_source),
        limits: AppServerAutomationLimits {
            max_runs: spec.limits.max_runs,
            until_at: spec.limits.until_at,
        },
        policy_command: spec.policy_command.map(|policy_command| {
            AppServerAutomationPolicyCommand {
                command: policy_command.command,
                cwd: policy_command.cwd,
                timeout_ms: policy_command.timeout_ms,
            }
        }),
    }
}

fn automation_trigger_to_remote(
    trigger: codex_core::slop_fork::automation::AutomationTrigger,
) -> AppServerAutomationTrigger {
    match trigger {
        codex_core::slop_fork::automation::AutomationTrigger::TurnCompleted => {
            AppServerAutomationTrigger::TurnCompleted
        }
        codex_core::slop_fork::automation::AutomationTrigger::Interval { every_seconds } => {
            AppServerAutomationTrigger::Interval { every_seconds }
        }
        codex_core::slop_fork::automation::AutomationTrigger::Cron { expression } => {
            AppServerAutomationTrigger::Cron { expression }
        }
    }
}

fn automation_message_source_to_remote(
    message_source: codex_core::slop_fork::automation::AutomationMessageSource,
) -> AppServerAutomationMessageSource {
    match message_source {
        codex_core::slop_fork::automation::AutomationMessageSource::Static { message } => {
            AppServerAutomationMessageSource::Static { message }
        }
        codex_core::slop_fork::automation::AutomationMessageSource::RoundRobin { messages } => {
            AppServerAutomationMessageSource::RoundRobin { messages }
        }
    }
}
