use std::path::Path;

use chrono::Local;
use codex_app_server_protocol::AutoresearchControlAction;
use codex_app_server_protocol::AutoresearchMode as ApiAutoresearchMode;
use codex_core::CodexThread;
use codex_core::slop_fork::autoresearch::AUTORESEARCH_PLAYBOOK_FILE;
use codex_core::slop_fork::autoresearch::AUTORESEARCH_REPORT_FILE;
use codex_core::slop_fork::autoresearch::AutoresearchDiscoveryReason;
use codex_core::slop_fork::autoresearch::AutoresearchJournal;
use codex_core::slop_fork::autoresearch::AutoresearchMode as CoreAutoresearchMode;
use codex_core::slop_fork::autoresearch::AutoresearchParallelWorkspaceManager;
use codex_core::slop_fork::autoresearch::AutoresearchResearchWorkspace;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::autoresearch::AutoresearchWorkspace;
use codex_core::slop_fork::autoresearch::clear_thread_state;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;

#[derive(Debug)]
pub(crate) enum SlopForkAutoresearchError {
    InvalidRequest(String),
    Io(std::io::Error),
}

impl std::fmt::Display for SlopForkAutoresearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) => f.write_str(message),
            Self::Io(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for SlopForkAutoresearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRequest(_) => None,
            Self::Io(err) => Some(err),
        }
    }
}

impl From<std::io::Error> for SlopForkAutoresearchError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Default)]
pub(crate) struct SlopForkAutoresearchManager;

impl SlopForkAutoresearchManager {
    pub(crate) async fn start(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        workdir: &Path,
        goal: String,
        max_runs: Option<u32>,
        mode: ApiAutoresearchMode,
    ) -> Result<bool, SlopForkAutoresearchError> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
        if let Some(state) = runtime.state() {
            if state.active_turn_id.is_some() || state.pending_cycle_kind.is_some() {
                return Err(SlopForkAutoresearchError::InvalidRequest(
                    "Autoresearch already has a queued or running cycle. Stop it first if you want to replace the goal.".to_string(),
                ));
            }
            if !matches!(
                state.status,
                codex_core::slop_fork::autoresearch::AutoresearchStatus::Stopped
                    | codex_core::slop_fork::autoresearch::AutoresearchStatus::Completed
            ) {
                return Err(SlopForkAutoresearchError::InvalidRequest(
                    "Autoresearch already has an active session. Resume it, wrap it up, stop it, or clear it before replacing the goal.".to_string(),
                ));
            }
        }

        let mode = api_mode_to_core(mode);
        let prepared_workspace =
            AutoresearchWorkspace::prepare(codex_home, &thread_id.to_string(), workdir)
                .map_err(SlopForkAutoresearchError::InvalidRequest)?;
        if mode.is_open_ended() {
            AutoresearchResearchWorkspace::prepare(codex_home, &thread_id.to_string(), workdir)
                .map_err(SlopForkAutoresearchError::InvalidRequest)?;
        }

        if !runtime.start(
            goal,
            mode,
            workdir.to_path_buf(),
            prepared_workspace.workspace,
            max_runs,
            Local::now(),
        )? {
            return Err(SlopForkAutoresearchError::InvalidRequest(
                "Autoresearch already has an active session.".to_string(),
            ));
        }

        Ok(true)
    }

    pub(crate) async fn control(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        fallback_workdir: &Path,
        has_active_turn: bool,
        action: AutoresearchControlAction,
        focus: Option<String>,
    ) -> Result<bool, SlopForkAutoresearchError> {
        if matches!(action, AutoresearchControlAction::Clear) {
            self.clear(codex_home, thread_id, fallback_workdir)?;
            return Ok(true);
        }
        if !matches!(action, AutoresearchControlAction::Discover) && focus.is_some() {
            return Err(SlopForkAutoresearchError::InvalidRequest(
                "Discovery focus is only allowed for the discover action.".to_string(),
            ));
        }

        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
        let updated = match action {
            AutoresearchControlAction::Pause => runtime.pause()?,
            AutoresearchControlAction::Resume => {
                if !has_active_turn {
                    runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.resume()?
            }
            AutoresearchControlAction::WrapUp => {
                if !has_active_turn {
                    runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.request_wrap_up()?
            }
            AutoresearchControlAction::Stop => {
                if !has_active_turn && runtime.clear_orphaned_cycle_if_idle(Local::now())? {
                    true
                } else {
                    runtime.stop()?
                }
            }
            AutoresearchControlAction::Discover => runtime.request_discovery(
                AutoresearchDiscoveryReason::UserRequested,
                focus,
                Local::now(),
            )?,
            AutoresearchControlAction::Clear => unreachable!("clear is handled above"),
        };
        Ok(updated)
    }

    pub(crate) async fn evaluate_idle(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
    ) -> Result<(), String> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
        let Some(plan) = runtime
            .prepare_cycle_submission(Local::now())
            .map_err(|err| format!("Failed to prepare autoresearch follow-up: {err}"))?
        else {
            return Ok(());
        };
        runtime
            .note_submission_dispatched()
            .map_err(|err| format!("Failed to record autoresearch submission dispatch: {err}"))?;

        let turn_id = match thread
            .submit(Op::SlopForkAutoresearchTurn {
                prompt: plan.prompt,
            })
            .await
        {
            Ok(turn_id) => turn_id,
            Err(err) => {
                let failure_message =
                    format!("Autoresearch submission failed before the turn could start: {err}");
                runtime
                    .note_submission_failure(&failure_message)
                    .map_err(|save_err| {
                        format!(
                            "Failed to record autoresearch submission failure after submit error {err}: {save_err}"
                        )
                    })?;
                return Err(failure_message);
            }
        };
        let _ = runtime
            .note_turn_submitted(&turn_id)
            .map_err(|err| format!("Failed to record autoresearch submission: {err}"))?;
        Ok(())
    }

    pub(crate) async fn maybe_evaluate_idle(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
        has_active_turn: bool,
    ) -> Result<(), String> {
        if has_active_turn {
            return Ok(());
        }
        self.evaluate_idle(codex_home, thread, thread_id).await
    }

    pub(crate) async fn handle_event(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        event: &EventMsg,
    ) -> Result<(), String> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
        match event {
            EventMsg::TurnStarted(turn_started) => {
                let _ = runtime
                    .activate_pending_cycle(turn_started.turn_id.clone())
                    .map_err(|err| format!("Failed to activate autoresearch cycle: {err}"))?;
            }
            EventMsg::TurnComplete(turn_complete) => {
                let _ = runtime
                    .complete_turn(
                        &turn_complete.turn_id,
                        turn_complete
                            .last_agent_message
                            .as_deref()
                            .unwrap_or_default(),
                        Local::now(),
                    )
                    .map_err(|err| format!("Failed to update autoresearch state: {err}"))?;
            }
            EventMsg::TurnAborted(turn_aborted) => {
                let reason = format!(
                    "Autoresearch paused because the active turn was aborted: {:?}.",
                    turn_aborted.reason
                );
                let _ = runtime
                    .abort_turn(turn_aborted.turn_id.as_deref(), &reason)
                    .map_err(|err| format!("Failed to record autoresearch abort: {err}"))?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) async fn clear_thread(&self, codex_home: &Path, thread_id: &ThreadId) {
        if let Err(err) = clear_thread_state(codex_home, &thread_id.to_string()) {
            tracing::warn!("failed to clear autoresearch state for {thread_id}: {err}");
        }
    }

    fn clear(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        fallback_workdir: &Path,
    ) -> Result<(), SlopForkAutoresearchError> {
        let runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
        if let Some(state) = runtime.state()
            && let Some(workspace) = state.workspace.as_ref()
        {
            workspace.clear_snapshot()?;
        }

        AutoresearchResearchWorkspace::new(codex_home, &thread_id.to_string()).clear()?;
        AutoresearchParallelWorkspaceManager::new(codex_home, &thread_id.to_string())
            .clear_all(runtime.state().and_then(|state| state.workspace.as_ref()))
            .map_err(SlopForkAutoresearchError::InvalidRequest)?;

        let journal_workdir = runtime
            .state()
            .map(|state| state.workdir.clone())
            .unwrap_or_else(|| fallback_workdir.to_path_buf());
        AutoresearchJournal::remove_file(&journal_workdir)?;
        for generated_file in [AUTORESEARCH_PLAYBOOK_FILE, AUTORESEARCH_REPORT_FILE] {
            let path = journal_workdir.join(generated_file);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(SlopForkAutoresearchError::Io(err)),
            }
        }
        clear_thread_state(codex_home, &thread_id.to_string())?;
        Ok(())
    }
}

fn api_mode_to_core(mode: ApiAutoresearchMode) -> CoreAutoresearchMode {
    match mode {
        ApiAutoresearchMode::Optimize => CoreAutoresearchMode::Optimize,
        ApiAutoresearchMode::Research => CoreAutoresearchMode::Research,
        ApiAutoresearchMode::Scientist => CoreAutoresearchMode::Scientist,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use codex_core::slop_fork::autoresearch::AUTORESEARCH_JOURNAL_FILE;
    use codex_core::slop_fork::autoresearch::AutoresearchMode;
    use codex_core::slop_fork::autoresearch::AutoresearchResearchWorkspace;
    use codex_core::slop_fork::autoresearch::AutoresearchStatus;
    use codex_core::slop_fork::autoresearch::AutoresearchWorkspaceMode;
    use codex_protocol::protocol::TurnAbortReason;
    use codex_protocol::protocol::TurnAbortedEvent;
    use tempfile::tempdir;

    #[tokio::test]
    async fn handle_abort_event_pauses_runtime() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370")?;
        let manager = SlopForkAutoresearchManager;
        let mut runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        AutoresearchResearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
            .map_err(anyhow::Error::msg)?;

        assert!(runtime.start(
            "map hypotheses".to_string(),
            AutoresearchMode::Scientist,
            workdir.path().to_path_buf(),
            AutoresearchWorkspace {
                mode: AutoresearchWorkspaceMode::Filesystem,
                workdir: workdir.path().to_path_buf(),
                git_root: None,
                git_branch: None,
                accepted_revision: None,
                snapshot_root: Some(workdir.path().join("snapshot")),
            },
            /*max_runs*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("autoresearch cycle should be prepared");
        assert!(runtime.note_turn_submitted("turn-autoresearch")?);
        assert!(runtime.activate_pending_cycle("turn-autoresearch".to_string())?);

        manager
            .handle_event(
                dir.path(),
                &thread_id,
                &EventMsg::TurnAborted(TurnAbortedEvent {
                    turn_id: Some("turn-autoresearch".to_string()),
                    reason: TurnAbortReason::Interrupted,
                }),
            )
            .await
            .map_err(anyhow::Error::msg)?;

        let state = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("state");
        assert_eq!(state.status, AutoresearchStatus::Paused);
        Ok(())
    }

    #[tokio::test]
    async fn clear_removes_state_and_artifacts() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433371")?;
        let manager = SlopForkAutoresearchManager;
        let prepared_workspace =
            AutoresearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
                .map_err(anyhow::Error::msg)?;
        AutoresearchResearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
            .map_err(anyhow::Error::msg)?;
        let mut runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        assert!(runtime.start(
            "map hypotheses".to_string(),
            AutoresearchMode::Scientist,
            workdir.path().to_path_buf(),
            prepared_workspace.workspace.clone(),
            /*max_runs*/ None,
            Local::now(),
        )?);
        std::fs::write(workdir.path().join(AUTORESEARCH_JOURNAL_FILE), "[]")?;
        std::fs::write(workdir.path().join(AUTORESEARCH_PLAYBOOK_FILE), "playbook")?;
        std::fs::write(workdir.path().join(AUTORESEARCH_REPORT_FILE), "report")?;

        assert!(
            manager
                .control(
                    dir.path(),
                    &thread_id,
                    workdir.path(),
                    /*has_active_turn*/ false,
                    AutoresearchControlAction::Clear,
                    /*focus*/ None,
                )
                .await?
        );

        let runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        assert!(runtime.state().is_none());
        assert!(!workdir.path().join(AUTORESEARCH_JOURNAL_FILE).exists());
        assert!(!workdir.path().join(AUTORESEARCH_PLAYBOOK_FILE).exists());
        assert!(!workdir.path().join(AUTORESEARCH_REPORT_FILE).exists());
        if let Some(snapshot_root) = prepared_workspace.workspace.snapshot_root.as_ref() {
            assert!(!snapshot_root.exists());
        }
        let research_workspace =
            AutoresearchResearchWorkspace::new(dir.path(), &thread_id.to_string());
        research_workspace.clear()?;
        Ok(())
    }
}
