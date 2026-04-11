use std::path::Path;
use std::path::PathBuf;

use chrono::Local;
use codex_app_server_protocol::Automation;
use codex_app_server_protocol::AutomationDefinition;
use codex_app_server_protocol::AutomationMessageSource;
use codex_app_server_protocol::AutomationPolicyCommand;
use codex_app_server_protocol::AutomationScope;
use codex_app_server_protocol::AutomationSetEnabledParams;
use codex_app_server_protocol::AutomationTrigger;
use codex_app_server_protocol::AutomationUpdateType;
use codex_app_server_protocol::AutomationUpdatedNotification;
use codex_core::CodexThread;
use codex_core::slop_fork::automation::AutomationEntry as CoreAutomationEntry;
use codex_core::slop_fork::automation::AutomationEvaluationTrigger;
use codex_core::slop_fork::automation::AutomationLimits as CoreAutomationLimits;
use codex_core::slop_fork::automation::AutomationMessageSource as CoreAutomationMessageSource;
use codex_core::slop_fork::automation::AutomationPolicyCommand as CoreAutomationPolicyCommand;
use codex_core::slop_fork::automation::AutomationPolicyExecutionContext;
use codex_core::slop_fork::automation::AutomationPreparedAction;
use codex_core::slop_fork::automation::AutomationRegistry;
use codex_core::slop_fork::automation::AutomationScope as CoreAutomationScope;
use codex_core::slop_fork::automation::AutomationSpec as CoreAutomationSpec;
use codex_core::slop_fork::automation::AutomationTrigger as CoreAutomationTrigger;
use codex_core::slop_fork::automation::clear_thread_state;
use codex_core::slop_fork::automation::run_policy_command;
use codex_protocol::ThreadId;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;

#[derive(Clone, Default)]
pub(crate) struct SlopForkAutomationManager;

impl SlopForkAutomationManager {
    pub(crate) async fn list(
        &self,
        codex_home: &Path,
        cwd: &Path,
        thread_id: &ThreadId,
    ) -> std::io::Result<Vec<Automation>> {
        let registry = self.load_registry(codex_home, cwd, thread_id).await?;
        Ok(registry
            .list_entries()
            .into_iter()
            .map(entry_to_api)
            .collect())
    }

    pub(crate) async fn upsert(
        &self,
        codex_home: &Path,
        cwd: &Path,
        thread_id: &ThreadId,
        scope: AutomationScope,
        automation: AutomationDefinition,
    ) -> std::io::Result<Automation> {
        let mut registry = self.load_registry(codex_home, cwd, thread_id).await?;
        let entry = registry.upsert(
            api_scope_to_core(scope),
            definition_to_core(automation),
            Local::now(),
        )?;
        Ok(entry_to_api(entry))
    }

    pub(crate) async fn delete(
        &self,
        codex_home: &Path,
        cwd: &Path,
        thread_id: &ThreadId,
        runtime_id: &str,
    ) -> std::io::Result<bool> {
        let mut registry = self.load_registry(codex_home, cwd, thread_id).await?;
        let deleted = registry.remove(runtime_id)?;
        Ok(deleted)
    }

    pub(crate) async fn set_enabled(
        &self,
        codex_home: &Path,
        cwd: &Path,
        thread_id: &ThreadId,
        params: AutomationSetEnabledParams,
    ) -> std::io::Result<Option<Automation>> {
        let mut registry = self.load_registry(codex_home, cwd, thread_id).await?;
        if !registry.set_enabled(&params.runtime_id, params.enabled)? {
            return Ok(None);
        }
        let automation = registry
            .list_entries()
            .into_iter()
            .find(|entry| entry.runtime_id == params.runtime_id)
            .map(entry_to_api);
        Ok(automation)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn evaluate_turn_completed(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
        turn_id: &str,
        last_agent_message: &str,
        default_timeout_ms: u64,
        codex_linux_sandbox_exe: Option<PathBuf>,
        windows_sandbox_level: WindowsSandboxLevel,
        windows_sandbox_private_desktop: bool,
    ) -> Result<Vec<AutomationUpdatedNotification>, String> {
        let snapshot = thread.config_snapshot().await;
        let mut registry = self
            .load_registry(codex_home, &snapshot.cwd, thread_id)
            .await
            .map_err(|err| format!("Failed to load automation state: {err}"))?;
        let actions = registry
            .prepare_actions(
                AutomationEvaluationTrigger::TurnCompleted {
                    turn_id: Some(turn_id.to_string()),
                    last_agent_message: last_agent_message.to_string(),
                },
                Local::now(),
            )
            .map_err(|err| format!("Failed to evaluate automations: {err}"))?;
        if actions.is_empty() {
            return Ok(Vec::new());
        }

        let sandbox_policy = snapshot.sandbox_policy.clone();
        let execution = AutomationPolicyExecutionContext {
            session_cwd: snapshot.cwd.clone(),
            sandbox_policy: sandbox_policy.clone(),
            file_system_sandbox_policy: FileSystemSandboxPolicy::from(&sandbox_policy),
            network_sandbox_policy: NetworkSandboxPolicy::from(&sandbox_policy),
            codex_linux_sandbox_exe,
            windows_sandbox_level,
            windows_sandbox_private_desktop,
        };
        let notifications = self
            .run_prepared_actions(
                &mut registry,
                thread,
                thread_id,
                actions,
                default_timeout_ms,
                &execution,
            )
            .await?;
        Ok(notifications)
    }

    pub(crate) async fn next_timer_wake(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
    ) -> std::io::Result<Option<std::time::Duration>> {
        let snapshot = thread.config_snapshot().await;
        let mut registry = self
            .load_registry(codex_home, &snapshot.cwd, thread_id)
            .await?;
        let next_wake = registry.next_wake_in(Local::now())?;
        Ok(next_wake)
    }

    pub(crate) async fn evaluate_timers(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
        default_timeout_ms: u64,
        codex_linux_sandbox_exe: Option<PathBuf>,
        windows_sandbox_level: WindowsSandboxLevel,
        windows_sandbox_private_desktop: bool,
    ) -> Result<Vec<AutomationUpdatedNotification>, String> {
        let snapshot = thread.config_snapshot().await;
        let mut registry = self
            .load_registry(codex_home, &snapshot.cwd, thread_id)
            .await
            .map_err(|err| format!("Failed to load automation state: {err}"))?;
        let actions = registry
            .prepare_actions(AutomationEvaluationTrigger::Timer, Local::now())
            .map_err(|err| format!("Failed to evaluate automations: {err}"))?;
        if actions.is_empty() {
            return Ok(Vec::new());
        }

        let sandbox_policy = snapshot.sandbox_policy.clone();
        let execution = AutomationPolicyExecutionContext {
            session_cwd: snapshot.cwd.clone(),
            sandbox_policy: sandbox_policy.clone(),
            file_system_sandbox_policy: FileSystemSandboxPolicy::from(&sandbox_policy),
            network_sandbox_policy: NetworkSandboxPolicy::from(&sandbox_policy),
            codex_linux_sandbox_exe,
            windows_sandbox_level,
            windows_sandbox_private_desktop,
        };
        let notifications = self
            .run_prepared_actions(
                &mut registry,
                thread,
                thread_id,
                actions,
                default_timeout_ms,
                &execution,
            )
            .await?;
        Ok(notifications)
    }

    pub(crate) async fn clear_thread(&self, codex_home: &Path, thread_id: &ThreadId) {
        if let Err(err) = clear_thread_state(codex_home, &thread_id.to_string()) {
            tracing::warn!("failed to clear automation state for {thread_id}: {err}");
        }
    }

    async fn load_registry(
        &self,
        codex_home: &Path,
        cwd: &Path,
        thread_id: &ThreadId,
    ) -> std::io::Result<AutomationRegistry> {
        AutomationRegistry::load(codex_home, cwd, thread_id.to_string())
    }

    async fn run_prepared_actions(
        &self,
        registry: &mut AutomationRegistry,
        thread: &CodexThread,
        thread_id: &ThreadId,
        actions: Vec<AutomationPreparedAction>,
        default_timeout_ms: u64,
        execution: &AutomationPolicyExecutionContext,
    ) -> Result<Vec<AutomationUpdatedNotification>, String> {
        let mut notifications = Vec::new();
        for action in actions {
            match action {
                AutomationPreparedAction::Send {
                    runtime_id,
                    message,
                } => {
                    submit_follow_up_prompt(thread, &message)
                        .await
                        .map_err(|err| format!("Failed to submit automation follow-up: {err}"))?;
                    registry
                        .record_delivery(&runtime_id, Local::now())
                        .map_err(|err| {
                            format!("Failed to record automation delivery for {runtime_id}: {err}")
                        })?;
                    notifications.push(AutomationUpdatedNotification {
                        thread_id: thread_id.to_string(),
                        runtime_id: runtime_id.clone(),
                        update_type: AutomationUpdateType::Fired,
                        automation: registry
                            .list_entries()
                            .into_iter()
                            .find(|entry| entry.runtime_id == runtime_id)
                            .map(entry_to_api),
                        message: Some(message),
                    });
                }
                AutomationPreparedAction::RunPolicy(prepared) => {
                    match run_policy_command(
                        &prepared.command,
                        &prepared.payload,
                        default_timeout_ms,
                        execution,
                    )
                    .await
                    {
                        Ok(decision) => {
                            let emitted_message =
                                registry.preview_policy_message(&prepared.runtime_id, &decision);
                            if let Some(message) = emitted_message {
                                submit_follow_up_prompt(thread, &message)
                                    .await
                                    .map_err(|err| {
                                        format!("Failed to submit automation follow-up: {err}")
                                    })?;
                                let _ = registry
                                    .apply_policy_decision(
                                        &prepared.runtime_id,
                                        decision,
                                        Local::now(),
                                    )
                                    .map_err(|err| {
                                        format!(
                                            "Failed to apply automation policy result for {}: {err}",
                                            prepared.runtime_id
                                        )
                                    })?;
                                notifications.push(AutomationUpdatedNotification {
                                    thread_id: thread_id.to_string(),
                                    runtime_id: prepared.runtime_id.clone(),
                                    update_type: AutomationUpdateType::Fired,
                                    automation: registry
                                        .list_entries()
                                        .into_iter()
                                        .find(|entry| entry.runtime_id == prepared.runtime_id)
                                        .map(entry_to_api),
                                    message: Some(message),
                                });
                            } else {
                                let _ = registry
                                    .apply_policy_decision(
                                        &prepared.runtime_id,
                                        decision,
                                        Local::now(),
                                    )
                                    .map_err(|err| {
                                        format!(
                                            "Failed to apply automation policy result for {}: {err}",
                                            prepared.runtime_id
                                        )
                                    })?;
                            }
                            if registry
                                .list_entries()
                                .into_iter()
                                .find(|entry| entry.runtime_id == prepared.runtime_id)
                                .map(entry_to_api)
                                .as_ref()
                                .is_some_and(|automation| automation.stopped)
                            {
                                let automation = registry
                                    .list_entries()
                                    .into_iter()
                                    .find(|entry| entry.runtime_id == prepared.runtime_id)
                                    .map(entry_to_api);
                                notifications.push(AutomationUpdatedNotification {
                                    thread_id: thread_id.to_string(),
                                    runtime_id: prepared.runtime_id.clone(),
                                    update_type: AutomationUpdateType::Stopped,
                                    automation,
                                    message: None,
                                });
                            }
                        }
                        Err(error) => {
                            registry
                                .record_error(&prepared.runtime_id, error.clone())
                                .map_err(|err| {
                                    format!(
                                        "Failed to record automation policy error for {}: {err}",
                                        prepared.runtime_id
                                    )
                                })?;
                            notifications.push(AutomationUpdatedNotification {
                                thread_id: thread_id.to_string(),
                                runtime_id: prepared.runtime_id.clone(),
                                update_type: AutomationUpdateType::Failed,
                                automation: registry
                                    .list_entries()
                                    .into_iter()
                                    .find(|entry| entry.runtime_id == prepared.runtime_id)
                                    .map(entry_to_api),
                                message: Some(error),
                            });
                        }
                    }
                }
            }
        }
        Ok(notifications)
    }
}

async fn submit_follow_up_prompt(
    thread: &CodexThread,
    message: &str,
) -> codex_protocol::error::Result<String> {
    thread
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: message.to_string(),
                text_elements: Vec::new(),
            }],
            final_output_json_schema: None,
            responsesapi_client_metadata: None,
        })
        .await
}

fn entry_to_api(entry: CoreAutomationEntry) -> Automation {
    Automation {
        runtime_id: entry.runtime_id,
        id: entry.spec.id,
        scope: core_scope_to_api(entry.scope),
        enabled: entry.spec.enabled,
        paused: entry.state.paused,
        stopped: entry.state.stopped,
        run_count: entry.state.run_count,
        next_fire_at: entry.state.next_fire_at,
        last_error: entry.state.last_error,
        trigger: core_trigger_to_api(entry.spec.trigger),
        message_source: core_message_source_to_api(entry.spec.message_source),
        limits: core_limits_to_api(entry.spec.limits),
        policy_command: entry.spec.policy_command.map(core_policy_command_to_api),
    }
}

fn definition_to_core(definition: AutomationDefinition) -> CoreAutomationSpec {
    CoreAutomationSpec {
        id: definition.id.unwrap_or_default(),
        enabled: definition.enabled,
        trigger: api_trigger_to_core(definition.trigger),
        message_source: api_message_source_to_core(definition.message_source),
        limits: api_limits_to_core(definition.limits),
        policy_command: definition.policy_command.map(api_policy_command_to_core),
    }
}

fn api_scope_to_core(scope: AutomationScope) -> CoreAutomationScope {
    match scope {
        AutomationScope::Session => CoreAutomationScope::Session,
        AutomationScope::Repo => CoreAutomationScope::Repo,
        AutomationScope::Global => CoreAutomationScope::Global,
    }
}

fn core_scope_to_api(scope: CoreAutomationScope) -> AutomationScope {
    match scope {
        CoreAutomationScope::Session => AutomationScope::Session,
        CoreAutomationScope::Repo => AutomationScope::Repo,
        CoreAutomationScope::Global => AutomationScope::Global,
    }
}

fn api_trigger_to_core(trigger: AutomationTrigger) -> CoreAutomationTrigger {
    match trigger {
        AutomationTrigger::TurnCompleted => CoreAutomationTrigger::TurnCompleted,
        AutomationTrigger::Interval { every_seconds } => {
            CoreAutomationTrigger::Interval { every_seconds }
        }
        AutomationTrigger::Cron { expression } => CoreAutomationTrigger::Cron { expression },
    }
}

fn core_trigger_to_api(trigger: CoreAutomationTrigger) -> AutomationTrigger {
    match trigger {
        CoreAutomationTrigger::TurnCompleted => AutomationTrigger::TurnCompleted,
        CoreAutomationTrigger::Interval { every_seconds } => {
            AutomationTrigger::Interval { every_seconds }
        }
        CoreAutomationTrigger::Cron { expression } => AutomationTrigger::Cron { expression },
    }
}

fn api_message_source_to_core(
    message_source: AutomationMessageSource,
) -> CoreAutomationMessageSource {
    match message_source {
        AutomationMessageSource::Static { message } => {
            CoreAutomationMessageSource::Static { message }
        }
        AutomationMessageSource::RoundRobin { messages } => {
            CoreAutomationMessageSource::RoundRobin { messages }
        }
    }
}

fn core_message_source_to_api(
    message_source: CoreAutomationMessageSource,
) -> AutomationMessageSource {
    match message_source {
        CoreAutomationMessageSource::Static { message } => {
            AutomationMessageSource::Static { message }
        }
        CoreAutomationMessageSource::RoundRobin { messages } => {
            AutomationMessageSource::RoundRobin { messages }
        }
    }
}

fn api_limits_to_core(limits: codex_app_server_protocol::AutomationLimits) -> CoreAutomationLimits {
    CoreAutomationLimits {
        max_runs: limits.max_runs,
        until_at: limits.until_at,
    }
}

fn core_limits_to_api(limits: CoreAutomationLimits) -> codex_app_server_protocol::AutomationLimits {
    codex_app_server_protocol::AutomationLimits {
        max_runs: limits.max_runs,
        until_at: limits.until_at,
    }
}

fn api_policy_command_to_core(command: AutomationPolicyCommand) -> CoreAutomationPolicyCommand {
    CoreAutomationPolicyCommand {
        command: command.command,
        cwd: command.cwd,
        timeout_ms: command.timeout_ms,
    }
}

fn core_policy_command_to_api(command: CoreAutomationPolicyCommand) -> AutomationPolicyCommand {
    AutomationPolicyCommand {
        command: command.command,
        cwd: command.cwd,
        timeout_ms: command.timeout_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    #[tokio::test]
    async fn session_automation_lifecycle_round_trips() -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let manager = SlopForkAutomationManager;
        let thread_id =
            ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370").expect("valid thread id");

        let created = manager
            .upsert(
                temp_dir.path(),
                temp_dir.path(),
                &thread_id,
                AutomationScope::Session,
                AutomationDefinition {
                    id: Some("auto-1".to_string()),
                    enabled: true,
                    trigger: AutomationTrigger::TurnCompleted,
                    message_source: AutomationMessageSource::Static {
                        message: "continue working on this".to_string(),
                    },
                    limits: codex_app_server_protocol::AutomationLimits::default(),
                    policy_command: None,
                },
            )
            .await?;
        assert_eq!(
            created,
            Automation {
                runtime_id: "session:auto-1".to_string(),
                id: "auto-1".to_string(),
                scope: AutomationScope::Session,
                enabled: true,
                paused: false,
                stopped: false,
                run_count: 0,
                next_fire_at: None,
                last_error: None,
                trigger: AutomationTrigger::TurnCompleted,
                message_source: AutomationMessageSource::Static {
                    message: "continue working on this".to_string(),
                },
                limits: codex_app_server_protocol::AutomationLimits::default(),
                policy_command: None,
            }
        );

        let listed = manager
            .list(temp_dir.path(), temp_dir.path(), &thread_id)
            .await?;
        assert_eq!(listed, vec![created.clone()]);

        let updated = manager
            .set_enabled(
                temp_dir.path(),
                temp_dir.path(),
                &thread_id,
                AutomationSetEnabledParams {
                    thread_id: thread_id.to_string(),
                    runtime_id: created.runtime_id.clone(),
                    enabled: false,
                },
            )
            .await?;
        assert_eq!(
            updated,
            Some(Automation {
                enabled: false,
                ..created.clone()
            })
        );

        let deleted = manager
            .delete(
                temp_dir.path(),
                temp_dir.path(),
                &thread_id,
                &created.runtime_id,
            )
            .await?;
        assert!(deleted);
        assert_eq!(
            manager
                .list(temp_dir.path(), temp_dir.path(), &thread_id)
                .await?,
            Vec::<Automation>::new()
        );
        Ok(())
    }

    #[tokio::test]
    async fn session_automations_persist_across_manager_instances_until_thread_clear()
    -> std::io::Result<()> {
        let temp_dir = TempDir::new()?;
        let thread_id =
            ThreadId::from_string("7e070b33-4250-4ecf-9224-163f9386216a").expect("valid thread id");
        SlopForkAutomationManager
            .upsert(
                temp_dir.path(),
                temp_dir.path(),
                &thread_id,
                AutomationScope::Session,
                AutomationDefinition {
                    id: Some("auto-1".to_string()),
                    enabled: true,
                    trigger: AutomationTrigger::TurnCompleted,
                    message_source: AutomationMessageSource::Static {
                        message: "continue".to_string(),
                    },
                    limits: codex_app_server_protocol::AutomationLimits::default(),
                    policy_command: None,
                },
            )
            .await?;

        let listed = SlopForkAutomationManager
            .list(temp_dir.path(), temp_dir.path(), &thread_id)
            .await?;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].runtime_id, "session:auto-1");

        SlopForkAutomationManager
            .clear_thread(temp_dir.path(), &thread_id)
            .await;

        assert_eq!(
            SlopForkAutomationManager
                .list(temp_dir.path(), temp_dir.path(), &thread_id)
                .await?,
            Vec::<Automation>::new()
        );
        Ok(())
    }
}
