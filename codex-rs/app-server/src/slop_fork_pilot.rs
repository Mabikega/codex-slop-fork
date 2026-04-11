use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

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
use codex_protocol::protocol::Op;

#[derive(Debug)]
pub(crate) enum SlopForkPilotError {
    InvalidRequest(String),
    Io(std::io::Error),
}

pub(crate) struct PilotReadResult {
    pub(crate) run: Option<PilotRun>,
    pub(crate) updated: bool,
}

impl From<std::io::Error> for SlopForkPilotError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Clone, Default)]
pub(crate) struct SlopForkPilotManager {
    pre_submission_started_turns: Arc<Mutex<BTreeMap<String, String>>>,
    pre_submission_completed_turns: Arc<Mutex<BTreeMap<String, (String, String)>>>,
    pre_submission_aborted_turns: Arc<Mutex<PreSubmissionAbortedTurns>>,
}

type PreSubmissionAbortedTurns = BTreeMap<String, (Option<String>, String)>;

impl SlopForkPilotManager {
    fn pre_submission_started_turns_guard(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<String, String>> {
        match self.pre_submission_started_turns.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn pre_submission_completed_turns_guard(
        &self,
    ) -> std::sync::MutexGuard<'_, BTreeMap<String, (String, String)>> {
        match self.pre_submission_completed_turns.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn pre_submission_aborted_turns_guard(
        &self,
    ) -> std::sync::MutexGuard<'_, PreSubmissionAbortedTurns> {
        match self.pre_submission_aborted_turns.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn is_pre_submission_pending(state: Option<&CorePilotRunState>) -> bool {
        state.is_some_and(|state| {
            state.pending_cycle_kind.is_some()
                && state.submission_dispatched_at.is_some()
                && state.active_turn_id.is_none()
                && state.last_submitted_turn_id.is_none()
        })
    }

    fn has_in_flight_running_cycle_state(state: Option<&CorePilotRunState>) -> bool {
        state.is_some_and(|state| {
            state.status == CorePilotStatus::Running
                && (state.active_turn_id.is_some()
                    || state.pending_cycle_kind.is_some()
                    || state.submission_dispatched_at.is_some()
                    || state.last_submitted_turn_id.is_some())
        })
    }

    fn observed_pre_submission_turn_matches(&self, thread_id: &ThreadId, turn_id: &str) -> bool {
        let thread_id = thread_id.to_string();
        self.pre_submission_started_turns_guard()
            .get(&thread_id)
            .is_some_and(|observed| observed == turn_id)
            || self
                .pre_submission_completed_turns_guard()
                .get(&thread_id)
                .is_some_and(|(observed, _)| observed == turn_id)
            || self
                .pre_submission_aborted_turns_guard()
                .get(&thread_id)
                .and_then(|(observed, _)| observed.as_deref())
                == Some(turn_id)
    }

    fn has_pre_submission_tracking(&self, thread_id: &ThreadId) -> bool {
        let thread_id = thread_id.to_string();
        self.pre_submission_started_turns_guard()
            .contains_key(&thread_id)
            || self
                .pre_submission_completed_turns_guard()
                .contains_key(&thread_id)
            || self
                .pre_submission_aborted_turns_guard()
                .contains_key(&thread_id)
    }

    fn remember_pre_submission_turn_start_if_needed(
        &self,
        thread_id: &ThreadId,
        state: Option<&CorePilotRunState>,
        turn_id: &str,
    ) {
        if Self::is_pre_submission_pending(state) {
            self.pre_submission_started_turns_guard()
                .insert(thread_id.to_string(), turn_id.to_string());
        }
    }

    fn remember_pre_submission_turn_completed_if_needed(
        &self,
        thread_id: &ThreadId,
        state: Option<&CorePilotRunState>,
        turn_id: &str,
        last_agent_message: &str,
    ) {
        if Self::is_pre_submission_pending(state) {
            self.pre_submission_completed_turns_guard().insert(
                thread_id.to_string(),
                (turn_id.to_string(), last_agent_message.to_string()),
            );
        }
    }

    fn remember_pre_submission_turn_aborted_if_needed(
        &self,
        thread_id: &ThreadId,
        state: Option<&CorePilotRunState>,
        turn_id: Option<&str>,
        reason: &str,
    ) {
        if Self::is_pre_submission_pending(state) {
            self.pre_submission_aborted_turns_guard().insert(
                thread_id.to_string(),
                (turn_id.map(ToString::to_string), reason.to_string()),
            );
        }
    }

    fn clear_pre_submission_tracking(&self, thread_id: &ThreadId) {
        let _ = self
            .pre_submission_started_turns_guard()
            .remove(&thread_id.to_string());
        let _ = self
            .pre_submission_completed_turns_guard()
            .remove(&thread_id.to_string());
        let _ = self
            .pre_submission_aborted_turns_guard()
            .remove(&thread_id.to_string());
    }

    fn recover_pre_submission_turn_start(
        &self,
        runtime: &mut PilotRuntime,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<bool, String> {
        let observed_turn_id = self
            .pre_submission_started_turns_guard()
            .remove(&thread_id.to_string());
        if observed_turn_id.as_deref() != Some(turn_id) {
            return Ok(false);
        }
        let _ = self
            .pre_submission_completed_turns_guard()
            .remove(&thread_id.to_string());
        let _ = self
            .pre_submission_aborted_turns_guard()
            .remove(&thread_id.to_string());
        runtime
            .activate_pending_cycle(turn_id.to_string())
            .map_err(|err| format!("Failed to recover early Pilot turn start: {err}"))
    }

    fn recover_pre_submission_turn_completed(
        &self,
        runtime: &mut PilotRuntime,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let observed = self
            .pre_submission_completed_turns_guard()
            .remove(&thread_id.to_string());
        let Some((observed_turn_id, last_agent_message)) = observed else {
            return Ok(None);
        };
        if observed_turn_id != turn_id {
            return Ok(None);
        }
        let _ = self
            .pre_submission_started_turns_guard()
            .remove(&thread_id.to_string());
        let _ = self
            .pre_submission_aborted_turns_guard()
            .remove(&thread_id.to_string());
        let _ = runtime
            .activate_pending_cycle(turn_id.to_string())
            .map_err(|err| format!("Failed to recover early Pilot turn completion: {err}"))?;
        let updated = runtime
            .complete_turn(turn_id, &last_agent_message, Local::now())
            .map_err(|err| format!("Failed to apply early Pilot completion: {err}"))?;
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

    fn recover_pre_submission_turn_aborted(
        &self,
        runtime: &mut PilotRuntime,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let observed = self
            .pre_submission_aborted_turns_guard()
            .remove(&thread_id.to_string());
        let Some((observed_turn_id, reason)) = observed else {
            return Ok(None);
        };
        if observed_turn_id
            .as_deref()
            .is_some_and(|observed| observed != turn_id)
        {
            return Ok(None);
        }
        let _ = self
            .pre_submission_started_turns_guard()
            .remove(&thread_id.to_string());
        let _ = self
            .pre_submission_completed_turns_guard()
            .remove(&thread_id.to_string());
        let _ = runtime
            .activate_pending_cycle(turn_id.to_string())
            .map_err(|err| format!("Failed to recover early Pilot abort: {err}"))?;
        let updated = runtime
            .abort_turn(Some(turn_id), &reason)
            .map_err(|err| format!("Failed to apply early Pilot abort: {err}"))?;
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

    fn recover_submitted_turn_without_start(
        runtime: &mut PilotRuntime,
        turn_id: &str,
    ) -> Result<bool, String> {
        let Some(state) = runtime.state() else {
            return Ok(false);
        };
        if state.active_turn_id.is_some()
            || state.pending_cycle_kind.is_none()
            || state.last_submitted_turn_id.as_deref() != Some(turn_id)
        {
            return Ok(false);
        }
        runtime
            .activate_pending_cycle(turn_id.to_string())
            .map_err(|err| format!("Failed to recover submitted Pilot turn without start: {err}"))
    }

    fn recover_lost_pre_submission_dispatch_if_needed(
        &self,
        runtime: &mut PilotRuntime,
        thread_id: &ThreadId,
    ) -> Result<bool, String> {
        if !Self::is_pre_submission_pending(runtime.state())
            || self.has_pre_submission_tracking(thread_id)
        {
            return Ok(false);
        }
        runtime
            .clear_orphaned_cycle_if_idle_for_control(Local::now())
            .map_err(|err| {
                format!("Failed to clear stale pre-submission Pilot dispatch state: {err}")
            })
    }

    pub(crate) async fn owns_turn(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<bool, String> {
        let runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        let state = runtime.state();
        Ok(state.is_some_and(|state| {
            state.active_turn_id.as_deref() == Some(turn_id)
                || state.last_submitted_turn_id.as_deref() == Some(turn_id)
        }) || self.observed_pre_submission_turn_matches(thread_id, turn_id))
    }

    pub(crate) async fn read(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        has_active_turn: bool,
    ) -> std::io::Result<PilotReadResult> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())?;
        let updated = if has_active_turn {
            runtime.clear_orphaned_cycle_if_idle(Local::now())?
        } else {
            runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?
        };
        Ok(PilotReadResult {
            run: runtime.state().map(run_to_api),
            updated,
        })
    }

    pub(crate) async fn start(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        has_active_turn: bool,
        goal: String,
        deadline_at: Option<i64>,
    ) -> Result<PilotRun, SlopForkPilotError> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())?;
        if has_active_turn {
            let _ = runtime.clear_orphaned_cycle_if_idle(Local::now())?;
        } else {
            let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
            let _ = runtime.stop()?;
        }
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
        has_active_turn: bool,
        action: PilotControlAction,
    ) -> Result<(bool, Option<PilotRun>, PilotUpdateType, Option<String>), SlopForkPilotError> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())?;
        let updated = match action {
            PilotControlAction::Pause => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.pause()?
            }
            PilotControlAction::Resume => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.resume()?
            }
            PilotControlAction::WrapUp => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.request_wrap_up()?
            }
            PilotControlAction::Stop => {
                if !has_active_turn && runtime.clear_orphaned_cycle_if_idle(Local::now())? {
                    true
                } else {
                    if !has_active_turn {
                        let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                    }
                    runtime.stop()?
                }
            }
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
        let _ = self.recover_lost_pre_submission_dispatch_if_needed(&mut runtime, thread_id)?;
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
                self.clear_pre_submission_tracking(thread_id);
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
        let recovered_notification = if recorded_submission {
            if let Some(notification) =
                self.recover_pre_submission_turn_completed(&mut runtime, thread_id, &turn_id)?
            {
                Some(notification)
            } else if let Some(notification) =
                self.recover_pre_submission_turn_aborted(&mut runtime, thread_id, &turn_id)?
            {
                Some(notification)
            } else if self.recover_pre_submission_turn_start(&mut runtime, thread_id, &turn_id)? {
                let state = runtime.state().map(run_to_api);
                Some(PilotUpdatedNotification {
                    thread_id: thread_id.to_string(),
                    update_type: PilotUpdateType::CycleStarted,
                    run: state.clone(),
                    message: state.and_then(|run| run.status_message),
                })
            } else {
                None
            }
        } else {
            self.clear_pre_submission_tracking(thread_id);
            None
        };
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
        if let Some(notification) = recovered_notification {
            return Ok(Some(notification));
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

    pub(crate) async fn has_in_flight_running_cycle(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
    ) -> Result<bool, String> {
        let runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        Ok(Self::has_in_flight_running_cycle_state(runtime.state()))
    }

    pub(crate) async fn handle_turn_started(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<Option<PilotUpdatedNotification>, String> {
        let mut runtime = PilotRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load pilot state: {err}"))?;
        if runtime
            .state()
            .and_then(|state| state.last_submitted_turn_id.as_deref())
            != Some(turn_id)
        {
            self.remember_pre_submission_turn_start_if_needed(thread_id, runtime.state(), turn_id);
            return Ok(None);
        }
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
        if Self::is_pre_submission_pending(runtime.state()) {
            self.remember_pre_submission_turn_completed_if_needed(
                thread_id,
                runtime.state(),
                turn_id,
                last_agent_message,
            );
            return Ok(None);
        }
        let _ = Self::recover_submitted_turn_without_start(&mut runtime, turn_id)?;
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
        if Self::is_pre_submission_pending(runtime.state()) {
            self.remember_pre_submission_turn_aborted_if_needed(
                thread_id,
                runtime.state(),
                turn_id,
                reason,
            );
            return Ok(None);
        }
        if let Some(turn_id) = turn_id {
            let _ = Self::recover_submitted_turn_without_start(&mut runtime, turn_id)?;
        }
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
    async fn read_clears_stopped_orphaned_cycle_state() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433369")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);
        assert!(runtime.stop()?);

        let read = manager
            .read(dir.path(), &thread_id, /*has_active_turn*/ false)
            .await?;
        assert!(read.updated);
        let run = read.run.expect("pilot run after stale recovery");
        assert_eq!(run.status, PilotStatus::Stopped);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        assert_eq!(
            run.status_message.as_deref(),
            Some("Pilot cleared stale cycle state after the thread became idle.")
        );

        let state = PilotRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("pilot state");
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn aborted_turn_reports_paused_update() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370")?;
        let manager = SlopForkPilotManager::default();
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

    #[tokio::test]
    async fn turn_started_ignores_pending_cycle_without_matching_submitted_turn() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");

        let notification = manager
            .handle_turn_started(dir.path(), &thread_id, "turn-other")
            .await
            .map_err(anyhow::Error::msg)?;

        assert!(notification.is_none());
        let state = PilotRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("pilot state");
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn turn_started_before_submission_id_is_recovered_after_submit_returns() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433371")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);

        let notification = manager
            .handle_turn_started(dir.path(), &thread_id, "turn-pilot")
            .await
            .map_err(anyhow::Error::msg)?;
        assert!(notification.is_none());

        assert!(runtime.note_turn_submitted("turn-pilot")?);
        assert!(
            manager
                .recover_pre_submission_turn_start(&mut runtime, &thread_id, "turn-pilot")
                .map_err(anyhow::Error::msg)?
        );

        let state = runtime.state().cloned().expect("pilot state");
        assert_eq!(state.active_turn_id.as_deref(), Some("turn-pilot"));
        assert_eq!(state.active_cycle_kind, Some(CorePilotCycleKind::Continue));
        Ok(())
    }

    #[tokio::test]
    async fn turn_completed_before_submission_id_is_recovered_after_submit_returns() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433372")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);

        let notification = manager
            .handle_turn_completed(dir.path(), &thread_id, "turn-pilot", "done")
            .await
            .map_err(anyhow::Error::msg)?;
        assert!(notification.is_none());
        assert!(
            manager
                .owns_turn(dir.path(), &thread_id, "turn-pilot")
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.note_turn_submitted("turn-pilot")?);
        let notification = manager
            .recover_pre_submission_turn_completed(&mut runtime, &thread_id, "turn-pilot")
            .map_err(anyhow::Error::msg)?
            .expect("pilot completion should recover");

        assert_eq!(notification.update_type, PilotUpdateType::CycleCompleted);
        let state = runtime.state().cloned().expect("pilot state");
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        assert_eq!(state.last_agent_message.as_deref(), Some("done"));
        Ok(())
    }

    #[tokio::test]
    async fn turn_aborted_before_submission_id_is_recovered_after_submit_returns() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433373")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);

        let notification = manager
            .handle_turn_aborted(dir.path(), &thread_id, Some("turn-pilot"), "aborted")
            .await
            .map_err(anyhow::Error::msg)?;
        assert!(notification.is_none());
        assert!(
            manager
                .owns_turn(dir.path(), &thread_id, "turn-pilot")
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.note_turn_submitted("turn-pilot")?);
        let notification = manager
            .recover_pre_submission_turn_aborted(&mut runtime, &thread_id, "turn-pilot")
            .map_err(anyhow::Error::msg)?
            .expect("pilot abort should recover");

        assert_eq!(notification.update_type, PilotUpdateType::Paused);
        let state = runtime.state().cloned().expect("pilot state");
        assert_eq!(state.status, CorePilotStatus::Paused);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn turn_completed_recovers_submitted_turn_when_start_event_is_missing() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433383")?;
        let manager = SlopForkPilotManager::default();
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

        let notification = manager
            .handle_turn_completed(dir.path(), &thread_id, "turn-pilot", "done")
            .await
            .map_err(anyhow::Error::msg)?
            .expect("pilot completion should emit notification");

        assert_eq!(notification.update_type, PilotUpdateType::CycleCompleted);
        let state = PilotRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("pilot state");
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        assert_eq!(state.last_agent_message.as_deref(), Some("done"));
        Ok(())
    }

    #[tokio::test]
    async fn turn_aborted_recovers_submitted_turn_when_start_event_is_missing() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433384")?;
        let manager = SlopForkPilotManager::default();
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

        let notification = manager
            .handle_turn_aborted(dir.path(), &thread_id, Some("turn-pilot"), "aborted")
            .await
            .map_err(anyhow::Error::msg)?
            .expect("pilot abort should emit notification");

        assert_eq!(notification.update_type, PilotUpdateType::Paused);
        let state = PilotRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("pilot state");
        assert_eq!(state.status, CorePilotStatus::Paused);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn lost_pre_submission_dispatch_is_cleared_when_no_turn_was_observed() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433385")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);

        assert!(
            manager
                .recover_lost_pre_submission_dispatch_if_needed(&mut runtime, &thread_id)
                .map_err(anyhow::Error::msg)?
        );

        let state = runtime.state().cloned().expect("pilot state");
        assert_eq!(state.status, CorePilotStatus::Running);
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.submission_dispatched_at, None);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        assert_eq!(
            state.status_message.as_deref(),
            Some("Pilot cleared stale cycle state after the thread became idle.")
        );
        Ok(())
    }

    #[tokio::test]
    async fn lost_pre_submission_dispatch_is_not_cleared_when_turn_was_observed() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433386")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);

        let notification = manager
            .handle_turn_started(dir.path(), &thread_id, "turn-pilot")
            .await
            .map_err(anyhow::Error::msg)?;
        assert!(notification.is_none());

        assert!(
            !manager
                .recover_lost_pre_submission_dispatch_if_needed(&mut runtime, &thread_id)
                .map_err(anyhow::Error::msg)?
        );

        let state = runtime.state().cloned().expect("pilot state");
        assert_eq!(state.pending_cycle_kind, Some(CorePilotCycleKind::Continue));
        assert!(state.submission_dispatched_at.is_some());
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn start_recovers_stale_stopped_cycle_state() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433374")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start("stale".to_string(), /*deadline_at*/ None, Local::now(),)?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);
        assert!(runtime.stop()?);

        let run = manager
            .start(
                dir.path(),
                &thread_id,
                /*has_active_turn*/ false,
                "fresh goal".to_string(),
                /*deadline_at*/ None,
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;

        assert_eq!(run.goal, "fresh goal");
        assert_eq!(run.status, PilotStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        Ok(())
    }

    #[tokio::test]
    async fn stop_clears_stale_stopped_cycle_state() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433375")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start("stale".to_string(), /*deadline_at*/ None, Local::now(),)?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);
        assert!(runtime.stop()?);

        let (updated, run, update_type, message) = manager
            .control(
                dir.path(),
                &thread_id,
                /*has_active_turn*/ false,
                PilotControlAction::Stop,
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;

        assert!(updated);
        assert_eq!(update_type, PilotUpdateType::Stopped);
        assert_eq!(run.as_ref().and_then(|run| run.pending_cycle_kind), None);
        assert_eq!(
            run.as_ref()
                .and_then(|run| run.last_submitted_turn_id.clone()),
            None
        );
        assert!(
            message
                .as_deref()
                .is_some_and(|message| message.contains("cleared stale cycle state"))
        );
        Ok(())
    }

    #[tokio::test]
    async fn pause_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433377")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start("stale".to_string(), /*deadline_at*/ None, Local::now(),)?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_turn_submitted("turn-pilot")?);
        assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);

        let (updated, run, update_type, _message) = manager
            .control(
                dir.path(),
                &thread_id,
                /*has_active_turn*/ false,
                PilotControlAction::Pause,
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;

        assert!(updated);
        assert_eq!(update_type, PilotUpdateType::Paused);
        let run = run.expect("pilot run after pause");
        assert_eq!(run.status, PilotStatus::Paused);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn start_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433382")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start("stale".to_string(), /*deadline_at*/ None, Local::now(),)?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_turn_submitted("turn-pilot")?);
        assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);

        let run = manager
            .start(
                dir.path(),
                &thread_id,
                /*has_active_turn*/ false,
                "fresh goal".to_string(),
                /*deadline_at*/ None,
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err:?}"))?;

        assert_eq!(run.goal, "fresh goal");
        assert_eq!(run.status, PilotStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn read_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433379")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(runtime.start(
            "keep shipping".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_turn_submitted("turn-pilot")?);
        assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);

        let read = manager
            .read(dir.path(), &thread_id, /*has_active_turn*/ false)
            .await?;
        assert!(read.updated);
        let run = read.run.expect("pilot run after stale recovery");

        assert_eq!(run.status, PilotStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        assert_eq!(
            run.status_message.as_deref(),
            Some("Pilot cleared stale cycle state after the thread became idle.")
        );
        Ok(())
    }

    #[tokio::test]
    async fn in_flight_running_cycle_detects_active_or_pending_submission_state() -> Result<()> {
        let dir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433376")?;
        let manager = SlopForkPilotManager::default();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id.to_string())?;

        assert!(
            !manager
                .has_in_flight_running_cycle(dir.path(), &thread_id)
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.start(
            "ship it".to_string(),
            /*deadline_at*/ None,
            Local::now(),
        )?);
        assert!(
            !manager
                .has_in_flight_running_cycle(dir.path(), &thread_id)
                .await
                .map_err(anyhow::Error::msg)?
        );

        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("pilot cycle should be prepared");
        assert!(runtime.note_submission_dispatched()?);
        assert!(
            manager
                .has_in_flight_running_cycle(dir.path(), &thread_id)
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.note_turn_submitted("turn-pilot")?);
        assert!(runtime.activate_pending_cycle("turn-pilot".to_string())?);
        assert!(
            manager
                .has_in_flight_running_cycle(dir.path(), &thread_id)
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.abort_turn(Some("turn-pilot"), "paused")?);
        assert!(
            !manager
                .has_in_flight_running_cycle(dir.path(), &thread_id)
                .await
                .map_err(anyhow::Error::msg)?
        );
        Ok(())
    }
}
