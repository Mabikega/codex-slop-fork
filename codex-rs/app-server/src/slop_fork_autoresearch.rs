use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use chrono::Local;
use codex_app_server_protocol::AutoresearchControlAction;
use codex_app_server_protocol::AutoresearchCycleKind as ApiAutoresearchCycleKind;
use codex_app_server_protocol::AutoresearchMode as ApiAutoresearchMode;
use codex_app_server_protocol::AutoresearchRun;
use codex_app_server_protocol::AutoresearchStatus as ApiAutoresearchStatus;
use codex_app_server_protocol::AutoresearchUpdateType;
use codex_app_server_protocol::AutoresearchUpdatedNotification;
use codex_core::CodexThread;
use codex_core::slop_fork::autoresearch::AUTORESEARCH_PLAYBOOK_FILE;
use codex_core::slop_fork::autoresearch::AUTORESEARCH_REPORT_FILE;
use codex_core::slop_fork::autoresearch::AutoresearchDiscoveryReason;
use codex_core::slop_fork::autoresearch::AutoresearchJournal;
use codex_core::slop_fork::autoresearch::AutoresearchMode as CoreAutoresearchMode;
use codex_core::slop_fork::autoresearch::AutoresearchParallelWorkspaceManager;
use codex_core::slop_fork::autoresearch::AutoresearchResearchWorkspace;
use codex_core::slop_fork::autoresearch::AutoresearchRunState as CoreAutoresearchRunState;
use codex_core::slop_fork::autoresearch::AutoresearchRuntime;
use codex_core::slop_fork::autoresearch::AutoresearchWorkspace;
use codex_core::slop_fork::autoresearch::clear_thread_state;
use codex_protocol::ThreadId;
use codex_protocol::protocol::Op;

#[derive(Debug)]
pub(crate) enum SlopForkAutoresearchError {
    InvalidRequest(String),
    Io(std::io::Error),
}

pub(crate) struct AutoresearchReadResult {
    pub(crate) run: Option<AutoresearchRun>,
    pub(crate) updated: bool,
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
pub(crate) struct SlopForkAutoresearchManager {
    pre_submission_started_turns: Arc<Mutex<BTreeMap<String, String>>>,
    pre_submission_completed_turns: Arc<Mutex<BTreeMap<String, (String, String)>>>,
    pre_submission_aborted_turns: Arc<Mutex<BTreeMap<String, (Option<String>, String)>>>,
}

impl SlopForkAutoresearchManager {
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
    ) -> std::sync::MutexGuard<'_, BTreeMap<String, (Option<String>, String)>> {
        match self.pre_submission_aborted_turns.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn is_pre_submission_pending(state: Option<&CoreAutoresearchRunState>) -> bool {
        state.is_some_and(|state| {
            state.pending_cycle_kind.is_some()
                && state.submission_dispatched_at.is_some()
                && state.active_turn_id.is_none()
                && state.last_submitted_turn_id.is_none()
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

    fn remember_pre_submission_turn_start_if_needed(
        &self,
        thread_id: &ThreadId,
        state: Option<&CoreAutoresearchRunState>,
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
        state: Option<&CoreAutoresearchRunState>,
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
        state: Option<&CoreAutoresearchRunState>,
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
        runtime: &mut AutoresearchRuntime,
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
            .map_err(|err| format!("Failed to recover early autoresearch turn start: {err}"))
    }

    fn recover_pre_submission_turn_completed(
        &self,
        runtime: &mut AutoresearchRuntime,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
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
            .map_err(|err| {
                format!("Failed to recover early autoresearch turn completion: {err}")
            })?;
        let updated = runtime
            .complete_turn(turn_id, &last_agent_message, Local::now())
            .map_err(|err| format!("Failed to apply early autoresearch completion: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        let update_type = match state.as_ref().map(|run| run.status) {
            Some(ApiAutoresearchStatus::Completed) => AutoresearchUpdateType::Completed,
            _ => AutoresearchUpdateType::CycleCompleted,
        };
        Ok(Some(AutoresearchUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
    }

    fn recover_pre_submission_turn_aborted(
        &self,
        runtime: &mut AutoresearchRuntime,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
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
            .map_err(|err| format!("Failed to recover early autoresearch abort: {err}"))?;
        let updated = runtime
            .abort_turn(Some(turn_id), &reason)
            .map_err(|err| format!("Failed to apply early autoresearch abort: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(AutoresearchUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: AutoresearchUpdateType::Paused,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
    }

    pub(crate) async fn owns_turn(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        turn_id: &str,
    ) -> Result<bool, String> {
        let runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
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
    ) -> std::io::Result<AutoresearchReadResult> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
        let updated = if has_active_turn {
            runtime.clear_orphaned_cycle_if_idle(Local::now())?
        } else {
            runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?
        };
        Ok(AutoresearchReadResult {
            run: runtime.state().map(run_to_api),
            updated,
        })
    }

    pub(crate) async fn start(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        has_active_turn: bool,
        workdir: &Path,
        goal: String,
        max_runs: Option<u32>,
        mode: ApiAutoresearchMode,
    ) -> Result<AutoresearchRun, SlopForkAutoresearchError> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
        if has_active_turn {
            let _ = runtime.clear_orphaned_cycle_if_idle(Local::now())?;
        } else {
            let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
            let _ = runtime.stop()?;
        }
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

        let Some(run) = runtime.state().map(run_to_api) else {
            return Err(SlopForkAutoresearchError::InvalidRequest(
                "Autoresearch started without persisted runtime state.".to_string(),
            ));
        };
        Ok(run)
    }

    pub(crate) async fn control(
        &self,
        codex_home: &Path,
        thread_id: &ThreadId,
        fallback_workdir: Option<&Path>,
        has_active_turn: bool,
        action: AutoresearchControlAction,
        focus: Option<String>,
    ) -> Result<
        (
            bool,
            Option<AutoresearchRun>,
            AutoresearchUpdateType,
            Option<String>,
        ),
        SlopForkAutoresearchError,
    > {
        if matches!(action, AutoresearchControlAction::Clear) {
            let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
            let clear_blocked = runtime.state().is_some_and(|state| {
                state.active_turn_id.is_some()
                    || (state.pending_cycle_kind.is_some()
                        && state.submission_dispatched_at.is_some()
                        && !matches!(
                            state.status,
                            codex_core::slop_fork::autoresearch::AutoresearchStatus::Stopped
                                | codex_core::slop_fork::autoresearch::AutoresearchStatus::Completed
                        ))
            });
            if has_active_turn || clear_blocked {
                return Err(SlopForkAutoresearchError::InvalidRequest(
                    "Autoresearch cannot be cleared while a controller turn is still active. Stop it first and wait for the turn to finish.".to_string(),
                ));
            }
            let _ = runtime.clear_orphaned_cycle_if_idle(Local::now())?;
            drop(runtime);
            self.clear(codex_home, thread_id, fallback_workdir)?;
            return Ok((
                true,
                None,
                AutoresearchUpdateType::Cleared,
                Some("Autoresearch cleared.".to_string()),
            ));
        }
        if !matches!(action, AutoresearchControlAction::Discover) && focus.is_some() {
            return Err(SlopForkAutoresearchError::InvalidRequest(
                "Discovery focus is only allowed for the discover action.".to_string(),
            ));
        }

        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())?;
        let updated = match action {
            AutoresearchControlAction::Pause => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.pause()?
            }
            AutoresearchControlAction::Resume => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.resume()?
            }
            AutoresearchControlAction::WrapUp => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.request_wrap_up()?
            }
            AutoresearchControlAction::Stop => {
                if !has_active_turn && runtime.clear_orphaned_cycle_if_idle(Local::now())? {
                    true
                } else {
                    if !has_active_turn {
                        let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                    }
                    runtime.stop()?
                }
            }
            AutoresearchControlAction::Discover => {
                if !has_active_turn {
                    let _ = runtime.clear_orphaned_cycle_if_idle_for_control(Local::now())?;
                }
                runtime.request_discovery(
                    AutoresearchDiscoveryReason::UserRequested,
                    focus,
                    Local::now(),
                )?
            }
            AutoresearchControlAction::Clear => unreachable!("clear is handled above"),
        };
        let update_type = match action {
            AutoresearchControlAction::Pause => AutoresearchUpdateType::Paused,
            AutoresearchControlAction::Resume => AutoresearchUpdateType::Resumed,
            AutoresearchControlAction::WrapUp => AutoresearchUpdateType::WrapUpRequested,
            AutoresearchControlAction::Stop => AutoresearchUpdateType::Stopped,
            AutoresearchControlAction::Discover => AutoresearchUpdateType::DiscoveryQueued,
            AutoresearchControlAction::Clear => unreachable!("clear is handled above"),
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
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
        let Some(plan) = runtime
            .prepare_cycle_submission(Local::now())
            .map_err(|err| format!("Failed to prepare autoresearch follow-up: {err}"))?
        else {
            return Ok(None);
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
                self.clear_pre_submission_tracking(thread_id);
                let state = runtime.state().map(run_to_api);
                return Ok(Some(AutoresearchUpdatedNotification {
                    thread_id: thread_id.to_string(),
                    update_type: AutoresearchUpdateType::Failed,
                    run: state.clone(),
                    message: state.and_then(|run| run.status_message),
                }));
            }
        };
        let recorded_submission = runtime
            .note_turn_submitted(&turn_id)
            .map_err(|err| format!("Failed to record autoresearch submission: {err}"))?;
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
                Some(AutoresearchUpdatedNotification {
                    thread_id: thread_id.to_string(),
                    update_type: AutoresearchUpdateType::CycleStarted,
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
            let refreshed_runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
                .map_err(|err| {
                    format!("Failed to reload autoresearch state after submission race: {err}")
                })?;
            if refreshed_runtime
                .state()
                .and_then(|state| state.active_turn_id.as_deref())
                == Some(turn_id.as_str())
            {
                return Ok(None);
            }
            let state = refreshed_runtime.state().map(run_to_api);
            return Ok(Some(AutoresearchUpdatedNotification {
                thread_id: thread_id.to_string(),
                update_type: AutoresearchUpdateType::Updated,
                run: state.clone(),
                message: state.and_then(|run| run.status_message),
            }));
        }
        if let Some(notification) = recovered_notification {
            return Ok(Some(notification));
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(AutoresearchUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: AutoresearchUpdateType::Queued,
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
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
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
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
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
            .map_err(|err| format!("Failed to activate autoresearch cycle: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(AutoresearchUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: AutoresearchUpdateType::CycleStarted,
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
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
        if Self::is_pre_submission_pending(runtime.state()) {
            self.remember_pre_submission_turn_completed_if_needed(
                thread_id,
                runtime.state(),
                turn_id,
                last_agent_message,
            );
            return Ok(None);
        }
        let updated = runtime
            .complete_turn(turn_id, last_agent_message, Local::now())
            .map_err(|err| format!("Failed to update autoresearch state: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        let update_type = match state.as_ref().map(|run| run.status) {
            Some(ApiAutoresearchStatus::Completed) => AutoresearchUpdateType::Completed,
            _ => AutoresearchUpdateType::CycleCompleted,
        };
        Ok(Some(AutoresearchUpdatedNotification {
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
    ) -> Result<Option<AutoresearchUpdatedNotification>, String> {
        let mut runtime = AutoresearchRuntime::load(codex_home, thread_id.to_string())
            .map_err(|err| format!("Failed to load autoresearch state: {err}"))?;
        if Self::is_pre_submission_pending(runtime.state()) {
            self.remember_pre_submission_turn_aborted_if_needed(
                thread_id,
                runtime.state(),
                turn_id,
                reason,
            );
            return Ok(None);
        }
        let updated = runtime
            .abort_turn(turn_id, reason)
            .map_err(|err| format!("Failed to record autoresearch abort: {err}"))?;
        if !updated {
            return Ok(None);
        }
        let state = runtime.state().map(run_to_api);
        Ok(Some(AutoresearchUpdatedNotification {
            thread_id: thread_id.to_string(),
            update_type: AutoresearchUpdateType::Paused,
            run: state.clone(),
            message: state.and_then(|run| run.status_message),
        }))
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
        fallback_workdir: Option<&Path>,
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

        if let Some(journal_workdir) = runtime
            .state()
            .map(|state| state.workdir.clone())
            .or_else(|| fallback_workdir.map(Path::to_path_buf))
        {
            AutoresearchJournal::remove_file(&journal_workdir)?;
            for generated_file in [AUTORESEARCH_PLAYBOOK_FILE, AUTORESEARCH_REPORT_FILE] {
                let path = journal_workdir.join(generated_file);
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => return Err(SlopForkAutoresearchError::Io(err)),
                }
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

fn run_to_api(
    state: &codex_core::slop_fork::autoresearch::AutoresearchRunState,
) -> AutoresearchRun {
    AutoresearchRun {
        goal: state.goal.clone(),
        mode: autoresearch_mode_to_api(state.mode),
        status: autoresearch_status_to_api(state.status),
        started_at: state.started_at,
        updated_at: state.updated_at,
        max_runs: state.max_runs,
        iteration_count: state.iteration_count,
        discovery_count: state.discovery_count,
        pending_cycle_kind: state.pending_cycle_kind.map(autoresearch_cycle_kind_to_api),
        active_cycle_kind: state.active_cycle_kind.map(autoresearch_cycle_kind_to_api),
        active_turn_id: state.active_turn_id.clone(),
        last_submitted_turn_id: state.last_submitted_turn_id.clone(),
        wrap_up_requested: state.wrap_up_requested,
        stop_requested_at: state.stop_requested_at,
        last_error: state.last_error.clone(),
        status_message: state.status_message.clone(),
        last_progress_at: state.last_progress_at,
        last_cycle_completed_at: state.last_cycle_completed_at,
        last_discovery_completed_at: state.last_discovery_completed_at,
        last_cycle_summary: state.last_cycle_summary.clone(),
        last_agent_message: state.last_agent_message.clone(),
    }
}

fn autoresearch_mode_to_api(value: CoreAutoresearchMode) -> ApiAutoresearchMode {
    match value {
        CoreAutoresearchMode::Optimize => ApiAutoresearchMode::Optimize,
        CoreAutoresearchMode::Research => ApiAutoresearchMode::Research,
        CoreAutoresearchMode::Scientist => ApiAutoresearchMode::Scientist,
    }
}

fn autoresearch_status_to_api(
    value: codex_core::slop_fork::autoresearch::AutoresearchStatus,
) -> ApiAutoresearchStatus {
    match value {
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Running => {
            ApiAutoresearchStatus::Running
        }
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Paused => {
            ApiAutoresearchStatus::Paused
        }
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Stopped => {
            ApiAutoresearchStatus::Stopped
        }
        codex_core::slop_fork::autoresearch::AutoresearchStatus::Completed => {
            ApiAutoresearchStatus::Completed
        }
    }
}

fn autoresearch_cycle_kind_to_api(
    value: codex_core::slop_fork::autoresearch::AutoresearchCycleKind,
) -> ApiAutoresearchCycleKind {
    match value {
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Continue => {
            ApiAutoresearchCycleKind::Continue
        }
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Research => {
            ApiAutoresearchCycleKind::Research
        }
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Discovery => {
            ApiAutoresearchCycleKind::Discovery
        }
        codex_core::slop_fork::autoresearch::AutoresearchCycleKind::WrapUp => {
            ApiAutoresearchCycleKind::WrapUp
        }
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
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[tokio::test]
    async fn read_clears_stopped_orphaned_cycle_state() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433369")?;
        let manager = SlopForkAutoresearchManager::default();
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
        assert!(runtime.note_submission_dispatched()?);
        assert!(runtime.stop()?);

        let read = manager
            .read(dir.path(), &thread_id, /*has_active_turn*/ false)
            .await?;
        assert!(read.updated);
        let run = read.run.expect("autoresearch run after stale recovery");
        assert_eq!(run.status, ApiAutoresearchStatus::Stopped);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        assert_eq!(
            run.status_message.as_deref(),
            Some("Autoresearch cleared stale cycle state after the thread became idle.")
        );

        let state = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("state");
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.active_cycle_kind, None);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn handle_turn_aborted_pauses_runtime() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370")?;
        let manager = SlopForkAutoresearchManager::default();
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
            .handle_turn_aborted(
                dir.path(),
                &thread_id,
                Some("turn-autoresearch"),
                "Autoresearch paused because the active turn was aborted: Interrupted.",
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
    async fn turn_started_ignores_pending_cycle_without_matching_submitted_turn() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433370")?;
        let manager = SlopForkAutoresearchManager::default();
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

        manager
            .handle_turn_started(dir.path(), &thread_id, "turn-other")
            .await
            .map_err(anyhow::Error::msg)?;

        let state = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?
            .state()
            .cloned()
            .expect("state");
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn turn_started_before_submission_id_is_recovered_after_submit_returns() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433372")?;
        let manager = SlopForkAutoresearchManager::default();
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
        assert!(runtime.note_submission_dispatched()?);

        manager
            .handle_turn_started(dir.path(), &thread_id, "turn-autoresearch")
            .await
            .map_err(anyhow::Error::msg)?;

        assert!(runtime.note_turn_submitted("turn-autoresearch")?);
        assert!(
            manager
                .recover_pre_submission_turn_start(&mut runtime, &thread_id, "turn-autoresearch",)
                .map_err(anyhow::Error::msg)?
        );

        let state = runtime.state().cloned().expect("state");
        assert_eq!(state.active_turn_id.as_deref(), Some("turn-autoresearch"));
        assert_eq!(
            state.active_cycle_kind,
            Some(codex_core::slop_fork::autoresearch::AutoresearchCycleKind::Discovery)
        );
        Ok(())
    }

    #[tokio::test]
    async fn turn_completed_before_submission_id_is_recovered_after_submit_returns() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433373")?;
        let manager = SlopForkAutoresearchManager::default();
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
        assert!(runtime.note_submission_dispatched()?);

        let notification = manager
            .handle_turn_completed(dir.path(), &thread_id, "turn-autoresearch", "done")
            .await
            .map_err(anyhow::Error::msg)?;
        assert!(notification.is_none());
        assert!(
            manager
                .owns_turn(dir.path(), &thread_id, "turn-autoresearch")
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.note_turn_submitted("turn-autoresearch")?);
        let notification = manager
            .recover_pre_submission_turn_completed(&mut runtime, &thread_id, "turn-autoresearch")
            .map_err(anyhow::Error::msg)?
            .expect("autoresearch completion should recover");

        assert_eq!(
            notification.update_type,
            AutoresearchUpdateType::CycleCompleted
        );
        let state = runtime.state().cloned().expect("state");
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        assert_eq!(state.last_agent_message.as_deref(), Some("done"));
        Ok(())
    }

    #[tokio::test]
    async fn turn_aborted_before_submission_id_is_recovered_after_submit_returns() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433374")?;
        let manager = SlopForkAutoresearchManager::default();
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
        assert!(runtime.note_submission_dispatched()?);

        let notification = manager
            .handle_turn_aborted(
                dir.path(),
                &thread_id,
                Some("turn-autoresearch"),
                "Autoresearch paused because the active turn was aborted: Interrupted.",
            )
            .await
            .map_err(anyhow::Error::msg)?;
        assert!(notification.is_none());
        assert!(
            manager
                .owns_turn(dir.path(), &thread_id, "turn-autoresearch")
                .await
                .map_err(anyhow::Error::msg)?
        );

        assert!(runtime.note_turn_submitted("turn-autoresearch")?);
        let notification = manager
            .recover_pre_submission_turn_aborted(&mut runtime, &thread_id, "turn-autoresearch")
            .map_err(anyhow::Error::msg)?
            .expect("autoresearch abort should recover");

        assert_eq!(notification.update_type, AutoresearchUpdateType::Paused);
        let state = runtime.state().cloned().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Paused);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn start_recovers_stale_stopped_cycle_state() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433375")?;
        let manager = SlopForkAutoresearchManager::default();
        let mut runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        AutoresearchResearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
            .map_err(anyhow::Error::msg)?;

        assert!(runtime.start(
            "stale".to_string(),
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
        assert!(runtime.note_submission_dispatched()?);
        assert!(runtime.stop()?);

        let run = manager
            .start(
                dir.path(),
                &thread_id,
                /*has_active_turn*/ false,
                workdir.path(),
                "fresh goal".to_string(),
                /*max_runs*/ None,
                ApiAutoresearchMode::Scientist,
            )
            .await?;

        assert_eq!(run.goal, "fresh goal");
        assert_eq!(run.status, ApiAutoresearchStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        Ok(())
    }

    #[tokio::test]
    async fn clear_rejects_while_controller_cycle_is_still_starting() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433376")?;
        let manager = SlopForkAutoresearchManager::default();
        let mut runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        AutoresearchResearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
            .map_err(anyhow::Error::msg)?;

        assert!(runtime.start(
            "stale".to_string(),
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
        assert!(runtime.note_submission_dispatched()?);

        let result = manager
            .control(
                dir.path(),
                &thread_id,
                Some(workdir.path()),
                /*has_active_turn*/ false,
                AutoresearchControlAction::Clear,
                /*focus*/ None,
            )
            .await;

        assert!(matches!(
            result,
            Err(SlopForkAutoresearchError::InvalidRequest(message))
                if message.contains("cannot be cleared while a controller turn is still active")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn pause_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433377")?;
        let manager = SlopForkAutoresearchManager::default();
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

        let (updated, run, update_type, _message) = manager
            .control(
                dir.path(),
                &thread_id,
                Some(workdir.path()),
                /*has_active_turn*/ false,
                AutoresearchControlAction::Pause,
                /*focus*/ None,
            )
            .await?;

        assert!(updated);
        assert_eq!(update_type, AutoresearchUpdateType::Paused);
        let run = run.expect("autoresearch run after pause");
        assert_eq!(run.status, ApiAutoresearchStatus::Paused);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn start_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433383")?;
        let manager = SlopForkAutoresearchManager::default();
        let mut runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        AutoresearchResearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
            .map_err(anyhow::Error::msg)?;

        assert!(runtime.start(
            "stale".to_string(),
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

        let run = manager
            .start(
                dir.path(),
                &thread_id,
                /*has_active_turn*/ false,
                workdir.path(),
                "fresh goal".to_string(),
                /*max_runs*/ None,
                ApiAutoresearchMode::Scientist,
            )
            .await?;

        assert_eq!(run.goal, "fresh goal");
        assert_eq!(run.status, ApiAutoresearchStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        Ok(())
    }

    #[tokio::test]
    async fn stop_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433380")?;
        let manager = SlopForkAutoresearchManager::default();
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

        let (updated, run, update_type, message) = manager
            .control(
                dir.path(),
                &thread_id,
                Some(workdir.path()),
                /*has_active_turn*/ false,
                AutoresearchControlAction::Stop,
                /*focus*/ None,
            )
            .await?;

        assert!(updated);
        assert_eq!(update_type, AutoresearchUpdateType::Stopped);
        let run = run.expect("autoresearch run after stop");
        assert_eq!(run.status, ApiAutoresearchStatus::Stopped);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        assert_eq!(message.as_deref(), Some("Autoresearch stopped."));
        Ok(())
    }

    #[tokio::test]
    async fn read_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433381")?;
        let manager = SlopForkAutoresearchManager::default();
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

        let read = manager
            .read(dir.path(), &thread_id, /*has_active_turn*/ false)
            .await?;
        assert!(read.updated);
        let run = read.run.expect("autoresearch run after stale recovery");

        assert_eq!(run.status, ApiAutoresearchStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        assert_eq!(
            run.status_message.as_deref(),
            Some("Autoresearch cleared stale cycle state after the thread became idle.")
        );
        Ok(())
    }

    #[tokio::test]
    async fn discover_clears_stale_running_cycle_state_when_no_turn_is_active() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433378")?;
        let manager = SlopForkAutoresearchManager::default();
        let mut runtime = AutoresearchRuntime::load(dir.path(), thread_id.to_string())?;
        let prepared_workspace =
            AutoresearchWorkspace::prepare(dir.path(), &thread_id.to_string(), workdir.path())
                .map_err(anyhow::Error::msg)?;

        assert!(runtime.start(
            "map hypotheses".to_string(),
            AutoresearchMode::Optimize,
            workdir.path().to_path_buf(),
            prepared_workspace.workspace,
            /*max_runs*/ None,
            Local::now(),
        )?);
        let _plan = runtime
            .prepare_cycle_submission(Local::now())?
            .expect("autoresearch cycle should be prepared");
        assert!(runtime.note_turn_submitted("turn-autoresearch")?);
        assert!(runtime.activate_pending_cycle("turn-autoresearch".to_string())?);

        let (updated, run, update_type, _message) = manager
            .control(
                dir.path(),
                &thread_id,
                Some(workdir.path()),
                /*has_active_turn*/ false,
                AutoresearchControlAction::Discover,
                Some("OCR transfer failures".to_string()),
            )
            .await?;

        assert!(updated);
        assert_eq!(update_type, AutoresearchUpdateType::DiscoveryQueued);
        let run = run.expect("autoresearch run after discovery request");
        assert_eq!(run.status, ApiAutoresearchStatus::Running);
        assert_eq!(run.pending_cycle_kind, None);
        assert_eq!(run.active_cycle_kind, None);
        assert_eq!(run.active_turn_id, None);
        assert_eq!(run.last_submitted_turn_id, None);
        assert_eq!(
            run.status_message.as_deref(),
            Some("Autoresearch queued a bounded discovery pass.")
        );
        Ok(())
    }

    #[tokio::test]
    async fn clear_removes_state_and_artifacts() -> Result<()> {
        let dir = tempdir()?;
        let workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433371")?;
        let manager = SlopForkAutoresearchManager::default();
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

        let (updated, ..) = manager
            .control(
                dir.path(),
                &thread_id,
                Some(workdir.path()),
                /*has_active_turn*/ false,
                AutoresearchControlAction::Clear,
                /*focus*/ None,
            )
            .await?;
        assert!(updated);

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

    #[tokio::test]
    async fn clear_without_state_and_without_fallback_workdir_keeps_unrelated_files() -> Result<()>
    {
        let dir = tempdir()?;
        let unrelated_workdir = tempdir()?;
        let thread_id = ThreadId::from_string("ad7f0408-99b8-4f6e-a46f-bd0eec433384")?;
        let manager = SlopForkAutoresearchManager::default();
        let journal_path = unrelated_workdir.path().join(AUTORESEARCH_JOURNAL_FILE);
        let playbook_path = unrelated_workdir.path().join(AUTORESEARCH_PLAYBOOK_FILE);
        let report_path = unrelated_workdir.path().join(AUTORESEARCH_REPORT_FILE);
        std::fs::write(&journal_path, "[]")?;
        std::fs::write(&playbook_path, "playbook")?;
        std::fs::write(&report_path, "report")?;

        let (updated, run, update_type, message) = manager
            .control(
                dir.path(),
                &thread_id,
                /*fallback_workdir*/ None,
                /*has_active_turn*/ false,
                AutoresearchControlAction::Clear,
                /*focus*/ None,
            )
            .await?;

        assert!(updated);
        assert_eq!(run, None);
        assert_eq!(update_type, AutoresearchUpdateType::Cleared);
        assert_eq!(message.as_deref(), Some("Autoresearch cleared."));
        assert!(journal_path.exists());
        assert!(playbook_path.exists());
        assert!(report_path.exists());
        Ok(())
    }
}
