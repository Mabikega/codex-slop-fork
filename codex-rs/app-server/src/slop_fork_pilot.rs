use std::path::Path;

use chrono::Local;
use codex_app_server_protocol::PilotControlAction;
use codex_app_server_protocol::PilotCycleKind;
use codex_app_server_protocol::PilotRun;
use codex_app_server_protocol::PilotStatus;
use codex_app_server_protocol::PilotUpdateType;
use codex_app_server_protocol::PilotUpdatedNotification;
use codex_core::CodexThread;
use codex_core::slop_fork::pilot::PilotCycleKind as CorePilotCycleKind;
use codex_core::slop_fork::pilot::PilotRunState as CorePilotRunState;
use codex_core::slop_fork::pilot::PilotRuntime;
use codex_core::slop_fork::pilot::PilotStatus as CorePilotStatus;
use codex_core::slop_fork::pilot::clear_thread_state;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;

pub(crate) enum SlopForkPilotError {
    InvalidRequest(String),
    Io(std::io::Error),
}

impl From<std::io::Error> for SlopForkPilotError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Default)]
pub(crate) struct SlopForkPilotManager;

impl SlopForkPilotManager {
    pub(crate) async fn read(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
    ) -> std::io::Result<Option<PilotRun>> {
        let runtime = PilotRuntime::load(codex_home, thread_id.to_string())?;
        Ok(runtime.state().map(run_to_api))
    }

    pub(crate) async fn start(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        goal: String,
        deadline_at: Option<i64>,
    ) -> Result<PilotRun, SlopForkPilotError> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())?;
        if !runtime.start(goal, deadline_at, Local::now())? {
            return Err(SlopForkPilotError::InvalidRequest(
                "Pilot already has a queued or running cycle. Stop it first if you want to replace the goal."
                    .to_string(),
            ));
        }
        runtime.state().map(run_to_api).ok_or_else(|| {
            SlopForkPilotError::InvalidRequest(
                "Pilot did not persist state after start.".to_string(),
            )
        })
    }

    pub(crate) async fn control(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        action: PilotControlAction,
    ) -> Result<(bool, Option<PilotRun>, PilotUpdateType, Option<String>), SlopForkPilotError> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())?;
        let updated = match action {
            PilotControlAction::Pause => runtime.pause()?,
            PilotControlAction::Resume => runtime.resume()?,
            PilotControlAction::WrapUp => runtime.request_wrap_up()?,
            PilotControlAction::Stop => runtime.stop()?,
        };
        let update_type = match action {
            PilotControlAction::Pause => PilotUpdateType::Paused,
            PilotControlAction::Resume => PilotUpdateType::Resumed,
            PilotControlAction::WrapUp => PilotUpdateType::WrapUpRequested,
            PilotControlAction::Stop => PilotUpdateType::Stopped,
        };
        let run = runtime.state().map(run_to_api);
        let message = run.as_ref().and_then(|run| run.status_message.clone());
        Ok((updated, run, update_type, message))
    }

    pub(crate) async fn evaluate_idle(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        let Some(plan) = runtime
            .prepare_cycle_submission(Local::now())
            .map_err(|err| format!("Failed to prepare Pilot follow-up: {err}"))?
        else {
            return Ok(None);
        };
        runtime
            .note_submission_dispatched()
            .map_err(|err| format!("Failed to record Pilot submission dispatch: {err}"))?;

        let turn_id = match thread
            .submit(Op::SlopForkPilotTurn {
                prompt: plan.prompt,
            })
            .await
        {
            Ok(turn_id) => turn_id,
            Err(err) => {
                let failure_message =
                    format!("Pilot submission failed before the turn could start: {err}");
                runtime
                    .note_submission_failure(&failure_message)
                    .map_err(|save_err| {
                        format!(
                            "Failed to record Pilot submission failure after submit error {err}: {save_err}"
                        )
                    })?;
                let state = runtime.state().map(run_to_api);
                return Ok(Some(PilotUpdatedNotification {
                    thread_id: thread_id.to_string(),
                    update_type: PilotUpdateType::Failed,
                    run: state.clone(),
                    message: state.and_then(|run| run.status_message),
                }));
            }
        };

        let recorded_submission = runtime
            .note_turn_submitted(&turn_id)
            .map_err(|err| format!("Failed to record Pilot submission: {err}"))?;
        if !recorded_submission {
            let refreshed_runtime =
                PilotRuntime::load(codex_home, thread_id.to_string()).map_err(|err| {
                    format!("Failed to reload Pilot state after submission race: {err}")
                })?;
            if refreshed_runtime
                .state()
                .and_then(|state| state.active_turn_id.as_deref())
                == Some(turn_id.as_str())
            {
                return Ok(None);
            }
            let state = refreshed_runtime.state().map(run_to_api);
            return Ok(Some(PilotUpdatedNotification {
                thread_id: thread_id.to_string(),
                update_type: PilotUpdateType::Updated,
                run: state.clone(),
                message: state.and_then(|run| run.status_message),
            }));
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(PilotUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: PilotUpdateType::Queued,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
    }

    pub(crate) async fn maybe_evaluate_idle(
        &self,
        codex_home: &Path,
        thread: &CodexThread,
        thread_id: &ThreadId,
        has_active_turn: bool,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        if has_active_turn {
            return Ok(None);
        }
        self.evaluate_idle(codex_home, thread, thread_id).await
    }

    pub(crate) async fn handle_turn_started(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        let updated = runtime
            .activate_pending_cycle(turn_id.to_string())
            .map_err(|err| format!("Failed to activate Pilot cycle: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(PilotUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: PilotUpdateType::CycleStarted,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
    }

    pub(crate) async fn handle_turn_completed(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        turn_id: &str,
        last_agent_message: &str,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        let updated = runtime
            .complete_turn(turn_id, last_agent_message, Local::now())
            .map_err(|err| format!("Failed to update Pilot state: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        let update_type = match state.as_ref().map(|run| run.status) {
            Some(PilotStatus::Completed) => PilotUpdateType::Completed,
            _ => PilotUpdateType::CycleCompleted,
        };
        Ok(Some(PilotUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
    }

    pub(crate) async fn handle_turn_aborted(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        turn_id: Option<&str>,
        reason: &str,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        let updated = runtime
            .abort_turn(turn_id, reason)
            .map_err(|err| format!("Failed to record Pilot abort: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(PilotUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: PilotUpdateType::Paused,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
    }

    pub(crate) async fn clear_thread(&self, codex_home: &Path, thread_id: &ThreadId) {
        if let Err(err) = clear_thread_state(codex_home, &thread_id.to_string()) {
            tracing::warn!("failed to clear pilot state for {thread_id}: {err}");
        }
    }

    pub(crate) async fn handle_event(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        event: &EventMsg,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        match event {
            EventMsg::TurnStarted(turn_started) => {
                self.handle_turn_started(codex_home, thread_id, &turn_started.turn_id)
                    .await
            }
            EventMsg::TurnComplete(turn_complete) => {
                self.handle_turn_completed(
                    codex_home,
                    thread_id,
                    &turn_complete.turn_id,
                    turn_complete
                        .last_agent_message
                        .as_deref()
                        .unwrap_or_default(),
                )
                .await
            }
            EventMsg::TurnAborted(turn_aborted) => {
                let reason = format!(
                    "Pilot paused because the active turn was aborted: {:?}.",
                    turn_aborted.reason
                );
                self.handle_turn_aborted(
                    codex_home,
                    thread_id,
                    turn_aborted.turn_id.as_deref(),
                    &reason,
                )
                .await
            }
            _ => Ok(None),
        }
    }
}

fn run_to_api(state: &CorePilotRunState) -> PilotRun {
    PilotRun {
        goal: state.goal.clone(),
        status: pilot_status_to_api(state.status),
        started_at: state.started_at,
        deadline_at: state.deadline_at,
        updated_at: state.updated_at,
        iteration_count: state.iteration_count,
        pending_cycle_kind: state.pending_cycle_kind.map(pilot_cycle_kind_to_api),
        active_cycle_kind: state.active_cycle_kind.map(pilot_cycle_kind_to_api),
        active_turn_id: state.active_turn_id.clone(),
        last_submitted_turn_id: state.last_submitted_turn_id.clone(),
        wrap_up_requested: state.wrap_up_requested,
        wrap_up_requested_at: state.wrap_up_requested_at,
        stop_requested_at: state.stop_requested_at,
        last_error: state.last_error.clone(),
        status_message: state.status_message.clone(),
        last_progress_at: state.last_progress_at,
        last_cycle_completed_at: state.last_cycle_completed_at,
        last_cycle_summary: state.last_cycle_summary.clone(),
        last_cycle_kind: state.last_cycle_kind.map(pilot_cycle_kind_to_api),
        last_agent_message: state.last_agent_message.clone(),
    }
}

fn pilot_status_to_api(value: CorePilotStatus) -> PilotStatus {
    match value {
        CorePilotStatus::Running => PilotStatus::Running,
        CorePilotStatus::Paused => PilotStatus::Paused,
        CorePilotStatus::Stopped => PilotStatus::Stopped,
        CorePilotStatus::Completed => PilotStatus::Completed,
    }
}

fn pilot_cycle_kind_to_api(value: CorePilotCycleKind) -> PilotCycleKind {
    match value {
        CorePilotCycleKind::Continue => PilotCycleKind::Continue,
        CorePilotCycleKind::WrapUp => PilotCycleKind::WrapUp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[tokio::test]
    async fn aborted_turn_reports_paused_update() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370")?;
        let manager = SlopForkPilotManager;
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_turn_submitted("turn-pilot")?);
        assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);

        let notification = manager
            .handle_turn_aborted(dir.path(), &thread_id, Some("turn-pilot"), "aborted")
            .await
            .map_err(anyhow::Error::msg)?
            .expect("pilot abort should emit notification");

        assert_eq!(notification.update_type, PilotUpdateType::Paused);
        assert_eq!(
            notification.run.as_ref().map(|run| run.status),
            Some(PilotStatus::Paused)
        );
        Ok(())
    }
}
