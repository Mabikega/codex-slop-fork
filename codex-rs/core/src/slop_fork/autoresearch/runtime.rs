#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::Local;
use fd_lock::RwLock as FileRwLock;
use serde::Deserialize;
use serde::Serialize;

use super::AUTORESEARCH_CHECKS_FILE;
use super::AUTORESEARCH_DOC_FILE;
use super::AUTORESEARCH_IDEAS_FILE;
use super::AUTORESEARCH_JOURNAL_FILE;
use super::AUTORESEARCH_PLAYBOOK_FILE;
use super::AUTORESEARCH_REPORT_FILE;
use super::AUTORESEARCH_SCRIPT_FILE;
use super::AutoresearchApproachEntry;
use super::AutoresearchApproachStatus;
use super::AutoresearchDiscoveryReason;
use super::AutoresearchDiscoveryRequest;
use super::AutoresearchJournal;
use super::AutoresearchParallelWorkspaceLease;
use super::AutoresearchResearchWorkspace;
use super::build_discovery_prompt;
use super::controller::load_autoresearch_controller_snapshot;
use super::journal::AutoresearchControllerDecisionEntry;
use super::journal::AutoresearchJournalEntry;
use super::journal::AutoresearchPortfolioRefreshDecision;
use super::journal::AutoresearchSelectionDecision;
use super::journal::AutoresearchSynthesisSuggestion;
use super::load_evaluation_governance_settings;
use super::load_stage_progress;
use super::load_validation_policy_settings;
use super::policy::ResearchCycleGuidance;
use super::policy::build_research_cycle_guidance;
use super::policy_config::load_selection_policy_settings;
use super::refresh_playbook_artifact;
use super::workspace::AutoresearchWorkspace;
use super::write_wrap_up_report_artifact;

const AUTORESEARCH_STATE_FILE: &str = ".codex-slop-fork-autoresearch-state.json";
const AUTORESEARCH_STATE_LOCK_FILE: &str = ".codex-slop-fork-autoresearch-state.lock";

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedCycleJournalFailureMode {
    None,
    BeforeWrite,
    AfterFirstLine,
}

#[cfg(test)]
thread_local! {
    static PREPARED_CYCLE_JOURNAL_FAILURE_MODE: Cell<PreparedCycleJournalFailureMode> =
        const { Cell::new(PreparedCycleJournalFailureMode::None) };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchStatus {
    Running,
    Paused,
    Stopped,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchMode {
    Optimize,
    Research,
    Scientist,
}

impl AutoresearchMode {
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::Optimize => "optimize",
            Self::Research => "research",
            Self::Scientist => "scientist",
        }
    }

    pub fn is_open_ended(self) -> bool {
        matches!(self, Self::Research | Self::Scientist)
    }

    pub fn cycle_label(self) -> &'static str {
        match self {
            Self::Optimize => "experiment",
            Self::Research => "research",
            Self::Scientist => "scientist",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchCycleKind {
    Continue,
    Research,
    Discovery,
    WrapUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchCyclePlan {
    pub kind: AutoresearchCycleKind,
    pub prompt: String,
    pub notify_on_completion: bool,
}

struct PreparedCycleSubmission {
    plan: AutoresearchCyclePlan,
    journal_entries: PreparedCycleJournalEntries,
    rollback: PreparedCycleRollback,
    workdir: PathBuf,
}

struct PreparedCycleJournalEntries {
    synthesized_approach: Option<AutoresearchApproachEntry>,
    controller_decision: Option<AutoresearchControllerDecisionEntry>,
}

impl PreparedCycleJournalEntries {
    fn is_empty(&self) -> bool {
        self.synthesized_approach.is_none() && self.controller_decision.is_none()
    }
}

struct PreparedCycleRollback {
    expected_kind: AutoresearchCycleKind,
    previous_status_message: Option<String>,
    previous_queued_discovery_request: Option<AutoresearchDiscoveryRequest>,
    previous_active_discovery_request: Option<AutoresearchDiscoveryRequest>,
    previous_active_discovery_logged: bool,
    previous_pending_active_approach_id: Option<String>,
    previous_pending_prepared_journal_original_len: Option<u64>,
    previous_pending_prepared_candidate_counter: Option<u32>,
    previous_candidate_counter: u32,
}

struct ControllerSynthesisTarget {
    approach_id: String,
    family: String,
    left_approach_id: String,
    left_family: String,
    right_approach_id: String,
    right_family: String,
    journal_entry: Option<AutoresearchApproachEntry>,
}

enum StaleCycleRecoveryScope {
    TerminalOnly,
    ExplicitControl,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PendingRunResult {
    pub token: String,
    pub command: String,
    pub duration_seconds: f64,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub passed: bool,
    pub checks_pass: Option<bool>,
    pub checks_output: String,
    pub parsed_primary: Option<f64>,
    pub parsed_metrics: BTreeMap<String, f64>,
    pub output_tail: String,
    pub parallel_workspace: Option<AutoresearchParallelWorkspaceLease>,
}

impl Default for PendingRunResult {
    fn default() -> Self {
        Self {
            token: String::new(),
            command: String::new(),
            duration_seconds: 0.0,
            exit_code: None,
            timed_out: false,
            passed: false,
            checks_pass: None,
            checks_output: String::new(),
            parsed_primary: None,
            parsed_metrics: BTreeMap::new(),
            output_tail: String::new(),
            parallel_workspace: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoresearchRunState {
    pub goal: String,
    pub mode: AutoresearchMode,
    pub status: AutoresearchStatus,
    pub workdir: PathBuf,
    pub workspace: Option<AutoresearchWorkspace>,
    pub started_at: i64,
    pub updated_at: i64,
    pub max_runs: Option<u32>,
    pub iteration_count: u32,
    pub discovery_count: u32,
    pub pending_cycle_kind: Option<AutoresearchCycleKind>,
    pub submission_dispatched_at: Option<i64>,
    pub active_cycle_kind: Option<AutoresearchCycleKind>,
    pub active_turn_id: Option<String>,
    pub last_submitted_turn_id: Option<String>,
    pub active_approach_id: Option<String>,
    pub pending_active_approach_id: Option<String>,
    pub pending_prepared_journal_original_len: Option<u64>,
    pub pending_prepared_candidate_counter: Option<u32>,
    pub cycle_origin_approach_id: Option<String>,
    pub candidate_counter: u32,
    pub consecutive_exploit_cycles: u32,
    pub queued_discovery_request: Option<AutoresearchDiscoveryRequest>,
    pub active_discovery_request: Option<AutoresearchDiscoveryRequest>,
    pub active_discovery_logged: bool,
    pub wrap_up_requested: bool,
    pub stop_requested_at: Option<i64>,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
    pub last_progress_at: Option<i64>,
    pub last_cycle_completed_at: Option<i64>,
    pub last_discovery_completed_at: Option<i64>,
    pub last_cycle_summary: Option<String>,
    pub last_agent_message: Option<String>,
    pub pending_run: Option<PendingRunResult>,
    pub pending_parallel_runs: Vec<PendingRunResult>,
}

impl Default for AutoresearchRunState {
    fn default() -> Self {
        Self {
            goal: String::new(),
            mode: AutoresearchMode::Optimize,
            status: AutoresearchStatus::Stopped,
            workdir: PathBuf::new(),
            workspace: None,
            started_at: 0,
            updated_at: 0,
            max_runs: None,
            iteration_count: 0,
            discovery_count: 0,
            pending_cycle_kind: None,
            submission_dispatched_at: None,
            active_cycle_kind: None,
            active_turn_id: None,
            last_submitted_turn_id: None,
            active_approach_id: None,
            pending_active_approach_id: None,
            pending_prepared_journal_original_len: None,
            pending_prepared_candidate_counter: None,
            cycle_origin_approach_id: None,
            candidate_counter: 0,
            consecutive_exploit_cycles: 0,
            queued_discovery_request: None,
            active_discovery_request: None,
            active_discovery_logged: false,
            wrap_up_requested: false,
            stop_requested_at: None,
            last_error: None,
            status_message: None,
            last_progress_at: None,
            last_cycle_completed_at: None,
            last_discovery_completed_at: None,
            last_cycle_summary: None,
            last_agent_message: None,
            pending_run: None,
            pending_parallel_runs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct AutoresearchStateFile {
    threads: BTreeMap<String, AutoresearchRunState>,
}

#[derive(Debug, Clone)]
pub struct AutoresearchRuntime {
    codex_home: PathBuf,
    thread_id: String,
    state: Option<AutoresearchRunState>,
}

impl AutoresearchRuntime {
    pub fn load(codex_home: &Path, thread_id: impl Into<String>) -> std::io::Result<Self> {
        let thread_id = thread_id.into();
        let state = load_thread_state(codex_home, &thread_id)?;
        Ok(Self {
            codex_home: codex_home.to_path_buf(),
            thread_id,
            state,
        })
    }

    pub fn state(&self) -> Option<&AutoresearchRunState> {
        self.state.as_ref()
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn is_active_turn(&self, turn_id: &str) -> bool {
        self.state
            .as_ref()
            .and_then(|state| state.active_turn_id.as_deref())
            == Some(turn_id)
    }

    pub fn has_pending_turn_start(&self) -> bool {
        self.state.as_ref().is_some_and(|state| {
            state.pending_cycle_kind.is_some()
                && state.active_turn_id.is_none()
                && state.submission_dispatched_at.is_some()
        })
    }

    pub fn start(
        &mut self,
        goal: String,
        mode: AutoresearchMode,
        workdir: PathBuf,
        workspace: AutoresearchWorkspace,
        max_runs: Option<u32>,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        let candidate_counter = if mode.is_open_ended() {
            next_candidate_counter(&workdir)?
        } else {
            0
        };
        let playbook_goal = goal.clone();
        let playbook_workdir = workdir.clone();
        let started = self.update_state(|state| {
            if state.as_ref().is_some_and(|state| {
                !matches!(
                    state.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                ) || state.active_turn_id.is_some()
                    || state.pending_cycle_kind.is_some()
            }) {
                return Ok(false);
            }
            *state = Some(AutoresearchRunState {
                goal,
                mode,
                status: AutoresearchStatus::Running,
                workdir,
                workspace: Some(workspace),
                started_at: now.timestamp(),
                updated_at: now.timestamp(),
                max_runs,
                iteration_count: 0,
                discovery_count: 0,
                pending_cycle_kind: None,
                submission_dispatched_at: None,
                active_cycle_kind: None,
                active_turn_id: None,
                last_submitted_turn_id: None,
                active_approach_id: None,
                pending_active_approach_id: None,
                pending_prepared_journal_original_len: None,
                pending_prepared_candidate_counter: None,
                cycle_origin_approach_id: None,
                candidate_counter,
                consecutive_exploit_cycles: 0,
                queued_discovery_request: None,
                active_discovery_request: None,
                active_discovery_logged: false,
                wrap_up_requested: false,
                stop_requested_at: None,
                last_error: None,
                status_message: Some("Autoresearch started.".to_string()),
                last_progress_at: None,
                last_cycle_completed_at: None,
                last_discovery_completed_at: None,
                last_cycle_summary: None,
                last_agent_message: None,
                pending_run: None,
                pending_parallel_runs: Vec::new(),
            });
            Ok(true)
        })?;
        if started && mode.is_open_ended() {
            let summary = AutoresearchJournal::load(&playbook_workdir)?.summary();
            refresh_playbook_artifact(
                &playbook_workdir,
                &playbook_goal,
                mode,
                &summary,
                /*active_approach_id*/ None,
            )?;
        }
        Ok(started)
    }

    pub fn pause(&mut self) -> std::io::Result<bool> {
        let (paused, journal_rollback) = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok((false, None));
            };
            if matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            ) {
                return Ok((false, None));
            }
            let submitted_turn_awaiting_start = state.pending_cycle_kind.is_some()
                && state.active_turn_id.is_none()
                && state.submission_dispatched_at.is_some();
            state.status = AutoresearchStatus::Paused;
            let journal_rollback = if submitted_turn_awaiting_start {
                None
            } else {
                take_pre_activation_prepared_journal_rollback(state)
            };
            if !submitted_turn_awaiting_start {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
                state.pending_active_approach_id = None;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(if submitted_turn_awaiting_start {
                "Autoresearch paused. The already-submitted cycle will finish first.".to_string()
            } else {
                "Autoresearch paused.".to_string()
            });
            Ok((true, journal_rollback))
        })?;
        rollback_pre_activation_prepared_journal_if_needed(journal_rollback)?;
        Ok(paused)
    }

    pub fn resume(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            ) {
                return Ok(false);
            }
            state.status = AutoresearchStatus::Running;
            state.updated_at = Local::now().timestamp();
            state.status_message = Some("Autoresearch resumed.".to_string());
            Ok(true)
        })
    }

    pub fn request_wrap_up(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            ) {
                return Ok(false);
            }
            state.wrap_up_requested = true;
            state.status = AutoresearchStatus::Running;
            state.updated_at = Local::now().timestamp();
            state.status_message = Some("Autoresearch wrap-up requested.".to_string());
            Ok(true)
        })
    }

    pub fn stop(&mut self) -> std::io::Result<bool> {
        let (stopped, journal_rollback) = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok((false, None));
            };
            if matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            ) {
                return Ok((false, None));
            }
            let submitted_turn_awaiting_start = state.pending_cycle_kind.is_some()
                && state.active_turn_id.is_none()
                && state.submission_dispatched_at.is_some();
            state.status = AutoresearchStatus::Stopped;
            state.wrap_up_requested = false;
            state.queued_discovery_request = None;
            if !submitted_turn_awaiting_start && state.active_turn_id.is_none() {
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
            }
            state.stop_requested_at = Some(Local::now().timestamp());
            let journal_rollback = if submitted_turn_awaiting_start {
                None
            } else {
                take_pre_activation_prepared_journal_rollback(state)
            };
            if !submitted_turn_awaiting_start {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
                state.pending_active_approach_id = None;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(if submitted_turn_awaiting_start {
                "Autoresearch stopped. The already-submitted cycle may still finish.".to_string()
            } else {
                "Autoresearch stopped.".to_string()
            });
            Ok((true, journal_rollback))
        })?;
        rollback_pre_activation_prepared_journal_if_needed(journal_rollback)?;
        Ok(stopped)
    }

    pub fn clear_orphaned_cycle_if_idle(&mut self, now: DateTime<Local>) -> std::io::Result<bool> {
        self.clear_orphaned_cycle_if_idle_with_scope(now, StaleCycleRecoveryScope::TerminalOnly)
    }

    pub fn clear_orphaned_cycle_if_idle_for_control(
        &mut self,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        self.clear_orphaned_cycle_if_idle_with_scope(now, StaleCycleRecoveryScope::ExplicitControl)
    }

    fn clear_orphaned_cycle_if_idle_with_scope(
        &mut self,
        now: DateTime<Local>,
        scope: StaleCycleRecoveryScope,
    ) -> std::io::Result<bool> {
        let (cleared, journal_rollback) = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok((false, None));
            };
            if matches!(scope, StaleCycleRecoveryScope::TerminalOnly)
                && !matches!(
                    state.status,
                    AutoresearchStatus::Stopped | AutoresearchStatus::Completed
                )
            {
                return Ok((false, None));
            }
            if state.pending_cycle_kind.is_none()
                && state.submission_dispatched_at.is_none()
                && state.active_cycle_kind.is_none()
                && state.active_turn_id.is_none()
                && state.last_submitted_turn_id.is_none()
            {
                return Ok((false, None));
            }

            let journal_rollback = take_pre_activation_prepared_journal_rollback(state);
            let previous_active_approach_id = state.cycle_origin_approach_id.take();
            if state.active_cycle_kind == Some(AutoresearchCycleKind::Research)
                && previous_active_approach_id.as_deref() != state.active_approach_id.as_deref()
            {
                state.active_approach_id = previous_active_approach_id;
            }
            state.pending_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.active_cycle_kind = None;
            state.active_turn_id = None;
            state.last_submitted_turn_id = None;
            state.pending_active_approach_id = None;
            state.updated_at = now.timestamp();
            state.status_message = Some(
                "Autoresearch cleared stale cycle state after the thread became idle.".to_string(),
            );
            Ok((true, journal_rollback))
        })?;
        rollback_pre_activation_prepared_journal_if_needed(journal_rollback)?;
        Ok(cleared)
    }

    pub fn request_discovery(
        &mut self,
        reason: AutoresearchDiscoveryReason,
        focus: Option<String>,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            ) {
                return Ok(false);
            }
            if state.wrap_up_requested
                || state.pending_cycle_kind == Some(AutoresearchCycleKind::WrapUp)
                || state.active_cycle_kind == Some(AutoresearchCycleKind::WrapUp)
                || discovery_request_would_be_dropped(state)
            {
                return Ok(false);
            }
            if state.queued_discovery_request.is_some()
                || state.active_discovery_request.is_some()
                || state.pending_cycle_kind == Some(AutoresearchCycleKind::Discovery)
                || state.active_cycle_kind == Some(AutoresearchCycleKind::Discovery)
            {
                return Ok(false);
            }
            state.queued_discovery_request = Some(AutoresearchDiscoveryRequest {
                reason,
                focus: focus
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty()),
                requested_at: now.timestamp(),
            });
            state.updated_at = now.timestamp();
            state.status_message = Some(match state.status {
                AutoresearchStatus::Paused => {
                    "Autoresearch queued a bounded discovery pass. Resume to run it.".to_string()
                }
                AutoresearchStatus::Running => {
                    "Autoresearch queued a bounded discovery pass.".to_string()
                }
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed => {
                    unreachable!("stopped/completed autoresearch cannot queue discovery")
                }
            });
            Ok(true)
        })
    }

    pub fn allocate_approach_id(&mut self) -> std::io::Result<String> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok("approach-1".to_string());
            };
            state.candidate_counter = state.candidate_counter.saturating_add(1);
            state.updated_at = Local::now().timestamp();
            Ok(format!("approach-{}", state.candidate_counter))
        })
    }

    pub fn note_approach_status(
        &mut self,
        approach_id: &str,
        status: AutoresearchApproachStatus,
    ) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            match status {
                AutoresearchApproachStatus::Active | AutoresearchApproachStatus::Winner => {
                    state.active_approach_id = Some(approach_id.to_string());
                }
                AutoresearchApproachStatus::Proposed
                | AutoresearchApproachStatus::Planned
                | AutoresearchApproachStatus::Tested
                | AutoresearchApproachStatus::Promising
                | AutoresearchApproachStatus::Archived
                | AutoresearchApproachStatus::DeadEnd => {
                    if state.active_approach_id.as_deref() == Some(approach_id) {
                        state.active_approach_id = None;
                    }
                }
            }
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn prepare_cycle_submission(
        &mut self,
        now: DateTime<Local>,
    ) -> std::io::Result<Option<AutoresearchCyclePlan>> {
        if let Some((workdir, cycle_origin_approach_id)) = self.state.as_ref().and_then(|state| {
            (state.mode.is_open_ended()
                && matches!(state.status, AutoresearchStatus::Running)
                && state.pending_cycle_kind.is_none()
                && state.active_turn_id.is_none())
            .then(|| (state.workdir.clone(), state.active_approach_id.clone()))
        }) {
            let research_workspace =
                AutoresearchResearchWorkspace::new(&self.codex_home, &self.thread_id);
            research_workspace
                .restore_for_approach(&workdir, cycle_origin_approach_id.as_deref())
                .map_err(std::io::Error::other)?;
            self.update_state(|state| {
                let Some(state) = state.as_mut() else {
                    return Ok(());
                };
                state.cycle_origin_approach_id = cycle_origin_approach_id;
                state.updated_at = now.timestamp();
                Ok(())
            })?;
        }
        let prepared = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(None);
            };
            if !matches!(state.status, AutoresearchStatus::Running) {
                return Ok(None);
            }
            if state.pending_cycle_kind.is_some() || state.active_turn_id.is_some() {
                return Ok(None);
            }
            let rollback = PreparedCycleRollback {
                expected_kind: AutoresearchCycleKind::Continue,
                previous_status_message: state.status_message.clone(),
                previous_queued_discovery_request: state.queued_discovery_request.clone(),
                previous_active_discovery_request: state.active_discovery_request.clone(),
                previous_active_discovery_logged: state.active_discovery_logged,
                previous_pending_active_approach_id: state.pending_active_approach_id.clone(),
                previous_pending_prepared_journal_original_len: state
                    .pending_prepared_journal_original_len,
                previous_pending_prepared_candidate_counter: state
                    .pending_prepared_candidate_counter,
                previous_candidate_counter: state.candidate_counter,
            };
            let journal = AutoresearchJournal::load(&state.workdir)?;
            let journal_summary = journal.summary();
            let validation_policy = state
                .mode
                .is_open_ended()
                .then(|| load_validation_policy_settings(&state.workdir));
            let governance = state
                .mode
                .is_open_ended()
                .then(|| load_evaluation_governance_settings(&state.workdir));
            let controller_snapshot = state.mode.is_open_ended().then(|| {
                load_autoresearch_controller_snapshot(
                    &state.workdir,
                    state,
                    Some(&journal_summary),
                    now.timestamp(),
                )
            });
            if state.mode.is_open_ended()
                && state.queued_discovery_request.is_none()
                && state.active_discovery_request.is_none()
                && controller_snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.portfolio_refresh_status.ready_now()
                })
            {
                state.queued_discovery_request = Some(AutoresearchDiscoveryRequest {
                    reason: AutoresearchDiscoveryReason::PortfolioRefresh,
                    focus: Some("refresh the active approach portfolio".to_string()),
                    requested_at: now.timestamp(),
                });
            }
            let kind = if state.wrap_up_requested
                || state
                    .max_runs
                    .is_some_and(|max_runs| state.iteration_count >= max_runs)
            {
                AutoresearchCycleKind::WrapUp
            } else if state.active_discovery_request.is_some()
                || state.queued_discovery_request.is_some()
            {
                if state.active_discovery_request.is_none() {
                    state.active_discovery_request = state.queued_discovery_request.take();
                }
                state.active_discovery_logged = false;
                AutoresearchCycleKind::Discovery
            } else if state.mode.is_open_ended() {
                AutoresearchCycleKind::Research
            } else {
                AutoresearchCycleKind::Continue
            };
            let rollback = PreparedCycleRollback {
                expected_kind: kind,
                ..rollback
            };
            let mut research_guidance = if kind == AutoresearchCycleKind::Research {
                controller_snapshot
                    .as_ref()
                    .zip(validation_policy.as_ref())
                    .zip(governance.as_ref())
                    .map(|((snapshot, validation_policy), governance)| {
                        build_research_cycle_guidance(
                            &journal_summary,
                            &state.goal,
                            state.active_approach_id.as_deref(),
                            state.consecutive_exploit_cycles,
                            &snapshot.selection_policy,
                            validation_policy,
                            governance,
                        )
                    })
            } else {
                None
            };
            let synthesized_target = if kind == AutoresearchCycleKind::Research {
                research_guidance.as_mut().and_then(|guidance| {
                    materialize_synthesis_target(state, &journal_summary, guidance, now)
                })
            } else {
                None
            };
            state.pending_active_approach_id = if let Some(target) = synthesized_target.as_ref() {
                Some(target.approach_id.clone())
            } else if state.active_approach_id.is_none() {
                research_guidance
                    .as_ref()
                    .and_then(|guidance| guidance.controller_selected_approach_id.clone())
            } else {
                research_guidance.as_ref().and_then(|guidance| {
                    guidance
                        .should_switch_active
                        .then(|| guidance.controller_selected_approach_id.clone())
                        .flatten()
                })
            };
            let stage_progress = load_stage_progress(&state.workdir, &journal_summary);
            state.pending_cycle_kind = Some(kind);
            state.updated_at = now.timestamp();
            state.status_message = Some(match kind {
                AutoresearchCycleKind::Continue => {
                    if let Some(progress) = stage_progress.as_ref()
                        && progress.has_issues()
                    {
                        format!(
                            "Autoresearch queued the next experiment cycle. Staged target config needs repair: {}",
                            progress.issue_summary()
                        )
                    } else {
                        stage_progress
                            .as_ref()
                            .and_then(|progress| {
                                progress.active_stage_number().and_then(|stage_number| {
                                    progress.active_stage().map(|stage| {
                                        format!(
                                            "Autoresearch queued the next experiment cycle. Active stage: {stage_number}/{} ({})",
                                            progress.total_stages(),
                                            stage.display
                                        )
                                    })
                                })
                            })
                            .unwrap_or_else(|| {
                                "Autoresearch queued the next experiment cycle.".to_string()
                            })
                    }
                }
                AutoresearchCycleKind::Research => {
                    let cycle_label = state.mode.cycle_label();
                    if let Some(approach_id) = state
                        .pending_active_approach_id
                        .as_deref()
                        .or(state.active_approach_id.as_deref())
                    {
                        format!(
                            "Autoresearch queued the next {cycle_label} cycle on approach `{approach_id}`."
                        )
                    } else {
                        format!("Autoresearch queued the next {cycle_label} cycle.")
                    }
                }
                AutoresearchCycleKind::Discovery => state
                    .active_discovery_request
                    .as_ref()
                    .map(|request| {
                        format!(
                            "Autoresearch queued a bounded discovery pass: {}.",
                            request.display_reason()
                        )
                    })
                    .unwrap_or_else(|| {
                        "Autoresearch queued a bounded discovery pass.".to_string()
                    }),
                AutoresearchCycleKind::WrapUp => "Autoresearch queued a wrap-up cycle.".to_string(),
            });
            let prompt = match kind {
                AutoresearchCycleKind::Discovery => {
                    let Some(request) = state.active_discovery_request.as_ref() else {
                        return Ok(None);
                    };
                    build_discovery_prompt(state, request, now, stage_progress.as_ref())
                }
                AutoresearchCycleKind::Continue
                | AutoresearchCycleKind::Research
                | AutoresearchCycleKind::WrapUp => {
                    build_cycle_prompt(
                        state,
                        kind,
                        now,
                        stage_progress.as_ref(),
                        research_guidance.as_ref(),
                    )
                }
            };
            let synthesized_approach = synthesized_target
                .as_ref()
                .and_then(|target| target.journal_entry.clone());
            let journal_entries = PreparedCycleJournalEntries {
                synthesized_approach,
                controller_decision: build_controller_decision_entry(
                    state,
                    kind,
                    controller_snapshot.as_ref(),
                    research_guidance.as_ref(),
                    journal_summary.current_segment,
                    now,
                ),
            };
            let prepared_journal_original_len = if journal_entries.is_empty() {
                None
            } else {
                Some(match std::fs::metadata(state.workdir.join(AUTORESEARCH_JOURNAL_FILE)) {
                    Ok(metadata) => metadata.len(),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
                    Err(err) => return Err(err),
                })
            };
            state.pending_prepared_journal_original_len = prepared_journal_original_len;
            state.pending_prepared_candidate_counter = synthesized_target
                .as_ref()
                .and_then(|target| {
                    target
                        .journal_entry
                        .as_ref()
                        .map(|_| rollback.previous_candidate_counter)
                });
            Ok(Some(PreparedCycleSubmission {
                plan: AutoresearchCyclePlan {
                    kind,
                    prompt,
                    notify_on_completion: matches!(kind, AutoresearchCycleKind::WrapUp),
                },
                journal_entries,
                rollback,
                workdir: state.workdir.clone(),
            }))
        })?;
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        let PreparedCycleSubmission {
            plan,
            journal_entries,
            rollback,
            workdir,
        } = prepared;
        if let Err(err) = append_prepared_cycle_journal_entries(&workdir, &journal_entries) {
            self.rollback_prepared_cycle_after_journal_failure(&rollback, now)?;
            return Err(err);
        }
        Ok(Some(plan))
    }

    fn rollback_prepared_cycle_after_journal_failure(
        &mut self,
        rollback: &PreparedCycleRollback,
        now: DateTime<Local>,
    ) -> std::io::Result<()> {
        let rolled_back = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.pending_cycle_kind != Some(rollback.expected_kind)
                || state.active_turn_id.is_some()
            {
                return Ok(false);
            }
            state.pending_cycle_kind = None;
            state.pending_active_approach_id = rollback.previous_pending_active_approach_id.clone();
            state.pending_prepared_journal_original_len =
                rollback.previous_pending_prepared_journal_original_len;
            state.pending_prepared_candidate_counter =
                rollback.previous_pending_prepared_candidate_counter;
            state.candidate_counter = rollback.previous_candidate_counter;
            state.queued_discovery_request = rollback.previous_queued_discovery_request.clone();
            state.active_discovery_request = rollback.previous_active_discovery_request.clone();
            state.active_discovery_logged = rollback.previous_active_discovery_logged;
            state.status_message = rollback.previous_status_message.clone();
            state.updated_at = now.timestamp();
            Ok(true)
        })?;
        if rolled_back {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "failed to roll back pending cycle after controller journal error",
            ))
        }
    }

    pub fn note_submission_dispatched(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.pending_cycle_kind.is_none() {
                return Ok(false);
            }
            state.submission_dispatched_at = Some(Local::now().timestamp());
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn note_submission_failure(&mut self, reason: &str) -> std::io::Result<bool> {
        let (failed, journal_rollback) = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok((false, None));
            };
            let journal_rollback = take_pre_activation_prepared_journal_rollback(state);
            if state.status == AutoresearchStatus::Running {
                state.status = AutoresearchStatus::Paused;
            }
            state.pending_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.active_cycle_kind = None;
            state.active_turn_id = None;
            state.last_submitted_turn_id = None;
            state.pending_active_approach_id = None;
            state.active_discovery_logged = false;
            state.last_error = Some(reason.to_string());
            state.status_message = Some(reason.to_string());
            state.updated_at = Local::now().timestamp();
            Ok((true, journal_rollback))
        })?;
        rollback_pre_activation_prepared_journal_if_needed(journal_rollback)?;
        Ok(failed)
    }

    pub fn note_turn_submitted(&mut self, turn_id: &str) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.pending_cycle_kind.is_none() {
                return Ok(false);
            }
            state.last_submitted_turn_id = Some(turn_id.to_string());
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn activate_pending_cycle(&mut self, turn_id: String) -> std::io::Result<bool> {
        let pending_approach_restore = self.state.as_ref().and_then(|state| {
            (state.pending_cycle_kind == Some(AutoresearchCycleKind::Research)
                && state.status != AutoresearchStatus::Completed
                && state
                    .last_submitted_turn_id
                    .as_deref()
                    .is_none_or(|submitted_turn_id| submitted_turn_id == turn_id))
            .then(|| {
                state
                    .pending_active_approach_id
                    .as_ref()
                    .map(|approach_id| (state.workdir.clone(), approach_id.clone()))
            })
            .flatten()
        });
        if let Some((workdir, approach_id)) = pending_approach_restore.as_ref() {
            let research_workspace =
                AutoresearchResearchWorkspace::new(&self.codex_home, &self.thread_id);
            research_workspace
                .restore_for_approach(workdir, Some(approach_id.as_str()))
                .map_err(std::io::Error::other)?;
        }
        let (activated, journal_rollback) = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok((false, None));
            };
            if state
                .last_submitted_turn_id
                .as_deref()
                .is_some_and(|submitted_turn_id| submitted_turn_id != turn_id)
            {
                return Ok((false, None));
            }
            let Some(kind) = state.pending_cycle_kind.take() else {
                return Ok((false, None));
            };
            if state.status == AutoresearchStatus::Completed {
                let journal_rollback = take_pre_activation_prepared_journal_rollback(state);
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
                return Ok((false, journal_rollback));
            }
            state.submission_dispatched_at = None;
            state.active_cycle_kind = Some(kind);
            state.active_turn_id = Some(turn_id);
            state.last_submitted_turn_id = state.active_turn_id.clone();
            state.pending_prepared_journal_original_len = None;
            state.pending_prepared_candidate_counter = None;
            if kind == AutoresearchCycleKind::Research
                && let Some(approach_id) = state.pending_active_approach_id.take()
            {
                state.active_approach_id = Some(approach_id);
            }
            state.last_error = None;
            if kind == AutoresearchCycleKind::Discovery {
                state.active_discovery_logged = false;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(match (state.status, kind) {
                (AutoresearchStatus::Running, AutoresearchCycleKind::Continue) => {
                    "Autoresearch started an experiment cycle.".to_string()
                }
                (AutoresearchStatus::Running, AutoresearchCycleKind::Research) => {
                    format!(
                        "Autoresearch started a {} cycle.",
                        state.mode.cycle_label()
                    )
                }
                (AutoresearchStatus::Running, AutoresearchCycleKind::Discovery) => {
                    "Autoresearch started a bounded discovery pass.".to_string()
                }
                (AutoresearchStatus::Running, AutoresearchCycleKind::WrapUp) => {
                    "Autoresearch started its wrap-up cycle.".to_string()
                }
                (AutoresearchStatus::Paused, _) => {
                    "Autoresearch is waiting for the already-submitted cycle to finish before pausing."
                        .to_string()
                }
                (AutoresearchStatus::Stopped, _) => {
                    "Autoresearch is waiting for the already-submitted cycle to finish before stopping."
                        .to_string()
                }
                (AutoresearchStatus::Completed, _) => {
                    unreachable!("completed autoresearch cannot activate")
                }
            });
            Ok((true, None))
        })?;
        rollback_pre_activation_prepared_journal_if_needed(journal_rollback)?;
        Ok(activated)
    }

    pub fn complete_turn(
        &mut self,
        turn_id: &str,
        last_agent_message: &str,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        let completed = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.active_turn_id.as_deref() != Some(turn_id) {
                return Ok(false);
            }
            let completed_kind = state.active_cycle_kind;
            state.active_turn_id = None;
            state.active_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.last_submitted_turn_id = None;
            state.pending_active_approach_id = None;
            state.cycle_origin_approach_id = None;
            match completed_kind {
                Some(AutoresearchCycleKind::Continue)
                | Some(AutoresearchCycleKind::Research)
                | Some(AutoresearchCycleKind::WrapUp) => {
                    state.iteration_count = state.iteration_count.saturating_add(1);
                }
                Some(AutoresearchCycleKind::Discovery) => {
                    state.discovery_count = state.discovery_count.saturating_add(1);
                    state.consecutive_exploit_cycles = 0;
                    state.last_discovery_completed_at = Some(now.timestamp());
                }
                None => {}
            }
            if matches!(
                completed_kind,
                Some(AutoresearchCycleKind::Continue) | Some(AutoresearchCycleKind::Research)
            ) {
                state.consecutive_exploit_cycles =
                    state.consecutive_exploit_cycles.saturating_add(1);
            }
            state.last_progress_at = Some(now.timestamp());
            state.last_cycle_completed_at = Some(now.timestamp());
            state.last_error = None;
            if !last_agent_message.trim().is_empty() {
                state.last_cycle_summary = summarize_cycle_message(last_agent_message);
                state.last_agent_message = Some(last_agent_message.trim().to_string());
            }
            state.updated_at = now.timestamp();

            let reached_max_runs = state
                .max_runs
                .is_some_and(|max_runs| state.iteration_count >= max_runs);
            if matches!(completed_kind, Some(AutoresearchCycleKind::WrapUp)) {
                state.status = AutoresearchStatus::Completed;
                state.wrap_up_requested = false;
                state.queued_discovery_request = None;
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
                state.pending_run = None;
                state.pending_parallel_runs.clear();
                state.status_message = Some("Autoresearch completed its wrap-up cycle.".to_string());
            } else if matches!(state.status, AutoresearchStatus::Stopped) {
                state.status = AutoresearchStatus::Completed;
                state.wrap_up_requested = false;
                state.queued_discovery_request = None;
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
                state.pending_run = None;
                state.pending_parallel_runs.clear();
                state.status_message =
                    Some("Autoresearch stopped after the active cycle finished.".to_string());
            } else if matches!(state.status, AutoresearchStatus::Paused) {
                if matches!(completed_kind, Some(AutoresearchCycleKind::Discovery)) {
                    state.active_discovery_request = None;
                    state.active_discovery_logged = false;
                    if state.wrap_up_requested {
                        state.status_message = Some(
                            "Autoresearch completed a bounded discovery pass. Resume to run the wrap-up cycle."
                                .to_string(),
                        );
                    } else {
                        state.status_message = Some(
                            "Autoresearch paused after a bounded discovery pass.".to_string(),
                        );
                    }
                } else if reached_max_runs {
                    state.wrap_up_requested = true;
                    state.status_message = Some(
                        "Autoresearch reached max runs while paused. Resume to run the wrap-up cycle."
                            .to_string(),
                    );
                } else if state.wrap_up_requested {
                    state.status_message = Some(
                        "Autoresearch is paused. Resume to run the wrap-up cycle.".to_string(),
                    );
                } else {
                    state.status_message = Some("Autoresearch paused.".to_string());
                }
            } else if matches!(completed_kind, Some(AutoresearchCycleKind::Discovery)) {
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
                state.status = AutoresearchStatus::Running;
                state.status_message = Some(
                    "Autoresearch completed a bounded discovery pass.".to_string(),
                );
            } else if reached_max_runs {
                state.wrap_up_requested = true;
                state.status_message = Some(
                    "Autoresearch reached max runs and will wrap up on the next cycle."
                        .to_string(),
                );
            } else if state.wrap_up_requested {
                state.status_message = Some(
                    "Autoresearch will run the wrap-up cycle next.".to_string(),
                );
            } else {
                state.status = AutoresearchStatus::Running;
                state.status_message = Some("Autoresearch cycle completed.".to_string());
            }
            Ok(true)
        })?;
        let Some(state) = self.state.as_ref() else {
            return Ok(completed);
        };
        if completed && state.mode.is_open_ended() {
            let summary = AutoresearchJournal::load(&state.workdir)?.summary();
            refresh_playbook_artifact(
                &state.workdir,
                &state.goal,
                state.mode,
                &summary,
                state.active_approach_id.as_deref(),
            )?;
            if state.status == AutoresearchStatus::Completed {
                write_wrap_up_report_artifact(
                    &state.workdir,
                    &state.goal,
                    state.mode,
                    state.last_cycle_summary.as_deref(),
                    &summary,
                    state.active_approach_id.as_deref(),
                )?;
            }
        }
        Ok(completed)
    }

    pub fn abort_turn(&mut self, turn_id: Option<&str>, reason: &str) -> std::io::Result<bool> {
        let restore_target = self.state.as_ref().and_then(|state| {
            let turn_matches =
                turn_id.is_none_or(|turn_id| state.active_turn_id.as_deref() == Some(turn_id));
            let had_active_cycle =
                state.active_turn_id.is_some() || state.pending_cycle_kind.is_some();
            let previous_active_approach_id = state.cycle_origin_approach_id.clone();
            (turn_matches
                && had_active_cycle
                && state.active_cycle_kind == Some(AutoresearchCycleKind::Research)
                && previous_active_approach_id.as_deref() != state.active_approach_id.as_deref())
            .then(|| (state.workdir.clone(), previous_active_approach_id))
        });
        if let Some((workdir, previous_active_approach_id)) = restore_target.as_ref() {
            let research_workspace =
                AutoresearchResearchWorkspace::new(&self.codex_home, &self.thread_id);
            research_workspace
                .restore_for_approach(workdir, previous_active_approach_id.as_deref())
                .map_err(std::io::Error::other)?;
        }
        let (aborted, journal_rollback) = self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok((false, None));
            };
            if let Some(turn_id) = turn_id
                && state.active_turn_id.as_deref() != Some(turn_id)
            {
                return Ok((false, None));
            }
            let had_active_cycle =
                state.active_turn_id.is_some() || state.pending_cycle_kind.is_some();
            if !had_active_cycle {
                return Ok((false, None));
            }
            let journal_rollback = take_pre_activation_prepared_journal_rollback(state);
            let previous_active_approach_id = state.cycle_origin_approach_id.take();
            if state.active_cycle_kind == Some(AutoresearchCycleKind::Research)
                && previous_active_approach_id.as_deref() != state.active_approach_id.as_deref()
            {
                state.active_approach_id = previous_active_approach_id;
            }
            let completed_discovery_checkpoint = state.active_cycle_kind
                == Some(AutoresearchCycleKind::Discovery)
                && state.active_discovery_logged;
            state.status = AutoresearchStatus::Paused;
            state.pending_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.active_cycle_kind = None;
            state.active_turn_id = None;
            state.last_submitted_turn_id = None;
            state.pending_active_approach_id = None;
            if completed_discovery_checkpoint {
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
            }
            state.last_error = Some(reason.to_string());
            state.status_message =
                Some("Autoresearch paused because the active turn was aborted.".to_string());
            state.updated_at = Local::now().timestamp();
            Ok((true, journal_rollback))
        })?;
        rollback_pre_activation_prepared_journal_if_needed(journal_rollback)?;
        Ok(aborted)
    }

    pub fn store_pending_run(&mut self, pending_run: PendingRunResult) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            state.pending_run = Some(pending_run);
            state.updated_at = Local::now().timestamp();
            state.last_progress_at = Some(Local::now().timestamp());
            Ok(true)
        })
    }

    pub fn store_pending_parallel_runs(
        &mut self,
        pending_runs: Vec<PendingRunResult>,
    ) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if pending_runs.is_empty() {
                return Ok(false);
            }
            state.pending_parallel_runs.extend(pending_runs);
            state.updated_at = Local::now().timestamp();
            state.last_progress_at = Some(Local::now().timestamp());
            Ok(true)
        })
    }

    pub fn mark_discovery_logged(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.active_cycle_kind != Some(AutoresearchCycleKind::Discovery)
                || state.active_discovery_logged
            {
                return Ok(false);
            }
            state.active_discovery_logged = true;
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn clear_discovery_logged(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.active_cycle_kind != Some(AutoresearchCycleKind::Discovery)
                || !state.active_discovery_logged
            {
                return Ok(false);
            }
            state.active_discovery_logged = false;
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn take_pending_run(&mut self, token: &str) -> std::io::Result<Option<PendingRunResult>> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(None);
            };
            if state
                .pending_run
                .as_ref()
                .is_some_and(|pending_run| pending_run.token == token)
            {
                return Ok(state.pending_run.take());
            }
            let Some(index) = state
                .pending_parallel_runs
                .iter()
                .position(|pending_run| pending_run.token == token)
            else {
                return Ok(None);
            };
            Ok(Some(state.pending_parallel_runs.remove(index)))
        })
    }

    pub fn clear_pending_run(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            let cleared =
                state.pending_run.take().is_some() || !state.pending_parallel_runs.is_empty();
            state.pending_parallel_runs.clear();
            state.updated_at = Local::now().timestamp();
            Ok(cleared)
        })
    }

    pub fn replace_workspace(&mut self, workspace: AutoresearchWorkspace) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            state.workspace = Some(workspace);
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn clear_completed_if_idle(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state_ref) = state.as_ref() else {
                return Ok(false);
            };
            if state_ref.active_turn_id.is_some() || state_ref.pending_cycle_kind.is_some() {
                return Ok(false);
            }
            if !matches!(
                state_ref.status,
                AutoresearchStatus::Completed | AutoresearchStatus::Stopped
            ) {
                return Ok(false);
            }
            *state = None;
            Ok(true)
        })
    }

    fn update_state<T>(
        &mut self,
        mutator: impl FnOnce(&mut Option<AutoresearchRunState>) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let (state, output) = mutate_thread_state(&self.codex_home, &self.thread_id, mutator)?;
        self.state = state;
        Ok(output)
    }
}

fn materialize_synthesis_target(
    state: &mut AutoresearchRunState,
    summary: &super::AutoresearchJournalSummary,
    guidance: &mut ResearchCycleGuidance,
    now: DateTime<Local>,
) -> Option<ControllerSynthesisTarget> {
    if guidance.should_switch_active || state.active_approach_id.is_none() {
        return None;
    }
    let synthesis = guidance.synthesis_suggestion.as_ref()?.clone();
    let target = if let Some(existing) = summary
        .latest_live_synthesis_approach(&synthesis.left_approach_id, &synthesis.right_approach_id)
    {
        ControllerSynthesisTarget {
            approach_id: existing.latest.approach_id.clone(),
            family: existing.latest.family.clone(),
            left_approach_id: synthesis.left_approach_id,
            left_family: synthesis.left_family,
            right_approach_id: synthesis.right_approach_id,
            right_family: synthesis.right_family,
            journal_entry: None,
        }
    } else {
        state.candidate_counter = state.candidate_counter.saturating_add(1);
        let approach_id = format!("approach-{}", state.candidate_counter);
        let title = format!(
            "Synthesis: {} + {}",
            synthesis.left_approach_id, synthesis.right_approach_id
        );
        let family = format!("{}+{}", synthesis.left_family, synthesis.right_family);
        let journal_entry = AutoresearchApproachEntry {
            entry_type: "approach".to_string(),
            approach_id: approach_id.clone(),
            title,
            family: family.clone(),
            status: AutoresearchApproachStatus::Planned,
            summary: format!(
                "Controller-created synthesis candidate combining `{}` [{}] with `{}` [{}].",
                synthesis.left_approach_id,
                synthesis.left_family,
                synthesis.right_approach_id,
                synthesis.right_family
            ),
            rationale:
                "Created automatically by the native controller to branch the search into a hybrid candidate."
                    .to_string(),
            risks: vec![
                "Validate that the combined design still respects the evaluator contract."
                    .to_string(),
            ],
            sources: Vec::new(),
            parent_approach_id: Some(synthesis.left_approach_id.clone()),
            synthesis_parent_approach_ids: vec![
                synthesis.left_approach_id.clone(),
                synthesis.right_approach_id.clone(),
            ],
            timestamp: now.timestamp(),
            segment: summary.current_segment,
        };
        ControllerSynthesisTarget {
            approach_id,
            family,
            left_approach_id: synthesis.left_approach_id,
            left_family: synthesis.left_family,
            right_approach_id: synthesis.right_approach_id,
            right_family: synthesis.right_family,
            journal_entry: Some(journal_entry),
        }
    };
    apply_synthesis_target_to_guidance(guidance, state.active_approach_id.as_deref(), &target);
    Some(target)
}

fn apply_synthesis_target_to_guidance(
    guidance: &mut ResearchCycleGuidance,
    active_approach_id: Option<&str>,
    target: &ControllerSynthesisTarget,
) {
    guidance.controller_selected_approach_id = Some(target.approach_id.clone());
    guidance.materialized_synthesis_approach_id = Some(target.approach_id.clone());
    guidance.should_switch_active = active_approach_id != Some(target.approach_id.as_str());
    guidance.selection_reasons.retain(|reason| {
        !matches!(
            reason.as_str(),
            "current active approach remains within the configured weak-branch switch threshold"
                | "current active approach remains the strongest ranked candidate"
                | "the portfolio does not yet contain a stronger recommended branch"
        )
    });
    guidance
        .selection_reasons
        .push(if target.journal_entry.is_some() {
            format!(
                "controller created synthesized branch `{}` from `{}` + `{}`",
                target.approach_id, target.left_approach_id, target.right_approach_id
            )
        } else {
            format!(
                "controller selected synthesized branch `{}` from `{}` + `{}`",
                target.approach_id, target.left_approach_id, target.right_approach_id
            )
        });
    if !guidance.prompt_block.is_empty() {
        guidance.prompt_block.push('\n');
    }
    guidance.prompt_block.push_str(&format!(
        "Controller synthesis action:\n\
         - Work on synthesized candidate `{}` [{}].\n\
         - It combines `{}` [{}] with `{}` [{}].\n\
         - Keep this lineage when you update the branch with `autoresearch_log_approach`.\n",
        target.approach_id,
        target.family,
        target.left_approach_id,
        target.left_family,
        target.right_approach_id,
        target.right_family
    ));
}

fn append_prepared_cycle_journal_entries(
    workdir: &Path,
    journal_entries: &PreparedCycleJournalEntries,
) -> std::io::Result<()> {
    let mut serialized_lines = Vec::new();
    if let Some(synthesized_approach) = journal_entries.synthesized_approach.as_ref() {
        serialized_lines.push(serialize_prepared_cycle_entry(synthesized_approach)?);
    }
    if let Some(controller_decision) = journal_entries.controller_decision.as_ref() {
        serialized_lines.push(serialize_prepared_cycle_entry(controller_decision)?);
    }
    if serialized_lines.is_empty() {
        return Ok(());
    }
    let path = workdir.join(AUTORESEARCH_JOURNAL_FILE);
    let original_len = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => return Err(err),
    };
    #[cfg(test)]
    let failure_mode = PREPARED_CYCLE_JOURNAL_FAILURE_MODE
        .with(|mode| mode.replace(PreparedCycleJournalFailureMode::None));
    #[cfg(not(test))]
    let _failure_mode = ();
    let append_result = (|| -> std::io::Result<()> {
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        #[cfg(test)]
        if failure_mode == PreparedCycleJournalFailureMode::BeforeWrite {
            return Err(std::io::Error::other(
                "injected prepared cycle journal failure before write",
            ));
        }
        for (line_index, line) in serialized_lines.iter().enumerate() {
            writeln!(file, "{line}")?;
            #[cfg(not(test))]
            let _ = line_index;
            #[cfg(test)]
            if failure_mode == PreparedCycleJournalFailureMode::AfterFirstLine && line_index == 0 {
                return Err(std::io::Error::other(
                    "injected prepared cycle journal failure after first line",
                ));
            }
        }
        file.flush()?;
        Ok(())
    })();
    if let Err(err) = append_result {
        rollback_prepared_cycle_journal_file(&path, original_len)?;
        return Err(err);
    }
    Ok(())
}

fn serialize_prepared_cycle_entry<T: Serialize>(entry: &T) -> std::io::Result<String> {
    serde_json::to_string(entry).map_err(|err| {
        std::io::Error::other(format!(
            "failed to serialize prepared autoresearch journal entry: {err}"
        ))
    })
}

fn rollback_prepared_cycle_journal_file(path: &Path, original_len: u64) -> std::io::Result<()> {
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_len(original_len)
}

fn take_pre_activation_prepared_journal_rollback(
    state: &mut AutoresearchRunState,
) -> Option<(PathBuf, u64)> {
    if state.active_turn_id.is_some() || state.active_cycle_kind.is_some() {
        return None;
    }
    let original_len = state.pending_prepared_journal_original_len.take()?;
    if let Some(previous_candidate_counter) = state.pending_prepared_candidate_counter.take() {
        state.candidate_counter = previous_candidate_counter;
    }
    Some((state.workdir.join(AUTORESEARCH_JOURNAL_FILE), original_len))
}

fn rollback_pre_activation_prepared_journal_if_needed(
    rollback: Option<(PathBuf, u64)>,
) -> std::io::Result<()> {
    let Some((path, original_len)) = rollback else {
        return Ok(());
    };
    rollback_prepared_cycle_journal_file(&path, original_len)
}

#[cfg(test)]
fn fail_next_post_prepare_controller_journal_append_for_test() {
    PREPARED_CYCLE_JOURNAL_FAILURE_MODE
        .with(|mode| mode.set(PreparedCycleJournalFailureMode::BeforeWrite));
}

#[cfg(test)]
fn fail_after_first_prepared_cycle_journal_line_for_test() {
    PREPARED_CYCLE_JOURNAL_FAILURE_MODE
        .with(|mode| mode.set(PreparedCycleJournalFailureMode::AfterFirstLine));
}

fn build_controller_decision_entry(
    state: &AutoresearchRunState,
    kind: AutoresearchCycleKind,
    controller_snapshot: Option<&super::AutoresearchControllerSnapshot>,
    research_guidance: Option<&ResearchCycleGuidance>,
    segment: u32,
    now: DateTime<Local>,
) -> Option<AutoresearchControllerDecisionEntry> {
    if !state.mode.is_open_ended() {
        return None;
    }
    let snapshot = controller_snapshot?;
    let (selection_decision, selection_reasons, active_approach_id, recommended_approach_id) =
        if let Some(guidance) = research_guidance {
            let selected_approach_id = guidance.controller_selected_approach_id.clone();
            (
                if selected_approach_id.is_some()
                    && selected_approach_id != state.active_approach_id
                {
                    AutoresearchSelectionDecision::SwitchActiveApproach
                } else if state.active_approach_id.is_some() {
                    AutoresearchSelectionDecision::KeepActiveApproach
                } else {
                    AutoresearchSelectionDecision::None
                },
                guidance.selection_reasons.clone(),
                state.active_approach_id.clone(),
                selected_approach_id,
            )
        } else {
            (
                AutoresearchSelectionDecision::None,
                Vec::new(),
                state.active_approach_id.clone(),
                None,
            )
        };
    let synthesis_suggestion = research_guidance.and_then(|guidance| {
        guidance
            .synthesis_suggestion
            .as_ref()
            .map(|suggestion| AutoresearchSynthesisSuggestion {
                left_approach_id: suggestion.left_approach_id.clone(),
                right_approach_id: suggestion.right_approach_id.clone(),
                synthesized_approach_id: guidance.materialized_synthesis_approach_id.clone(),
            })
    });
    let (portfolio_refresh_decision, portfolio_refresh_reasons) =
        portfolio_refresh_decision(state, kind, &snapshot.portfolio_refresh_status);
    let summary = controller_decision_summary(
        selection_decision,
        selection_reasons.as_slice(),
        active_approach_id.as_deref(),
        recommended_approach_id.as_deref(),
        portfolio_refresh_decision,
        portfolio_refresh_reasons.as_slice(),
        synthesis_suggestion.as_ref(),
    );
    Some(AutoresearchControllerDecisionEntry {
        entry_type: "controller".to_string(),
        cycle_kind: kind,
        selection_decision,
        selection_reasons,
        active_approach_id,
        recommended_approach_id,
        portfolio_refresh_decision,
        portfolio_refresh_trigger: (portfolio_refresh_decision
            != AutoresearchPortfolioRefreshDecision::NotApplicable)
            .then_some(snapshot.portfolio_refresh_status.trigger),
        portfolio_refresh_reasons,
        synthesis_suggestion,
        summary,
        timestamp: now.timestamp(),
        segment,
    })
}

fn portfolio_refresh_decision(
    state: &AutoresearchRunState,
    kind: AutoresearchCycleKind,
    status: &super::PortfolioRefreshStatus,
) -> (AutoresearchPortfolioRefreshDecision, Vec<String>) {
    if kind == AutoresearchCycleKind::Discovery
        && state
            .active_discovery_request
            .as_ref()
            .is_some_and(|request| request.reason == AutoresearchDiscoveryReason::PortfolioRefresh)
    {
        return (
            AutoresearchPortfolioRefreshDecision::Queued,
            portfolio_refresh_reasons(
                state,
                status,
                Some("controller queued a portfolio-refresh discovery pass"),
            ),
        );
    }
    if state.active_discovery_request.is_some() || state.queued_discovery_request.is_some() {
        return (
            AutoresearchPortfolioRefreshDecision::NotApplicable,
            vec!["another discovery request already owns the next cycle".to_string()],
        );
    }
    let decision = match status.kind {
        super::PortfolioRefreshStatusKind::Ready => AutoresearchPortfolioRefreshDecision::Queued,
        super::PortfolioRefreshStatusKind::WaitingForExploitCycles => {
            AutoresearchPortfolioRefreshDecision::Waiting
        }
        super::PortfolioRefreshStatusKind::CoolingDown => {
            AutoresearchPortfolioRefreshDecision::CoolingDown
        }
        super::PortfolioRefreshStatusKind::BootstrapComplete => {
            AutoresearchPortfolioRefreshDecision::BootstrapComplete
        }
        super::PortfolioRefreshStatusKind::Suppressed => {
            AutoresearchPortfolioRefreshDecision::Suppressed
        }
    };
    (
        decision,
        portfolio_refresh_reasons(state, status, /*leading_reason*/ None),
    )
}

fn portfolio_refresh_reasons(
    state: &AutoresearchRunState,
    status: &super::PortfolioRefreshStatus,
    leading_reason: Option<&str>,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if let Some(leading_reason) = leading_reason {
        reasons.push(leading_reason.to_string());
    }
    match status.trigger {
        super::PortfolioRefreshTriggerKind::Bootstrap => {
            reasons.push(
                "portfolio bootstrap logic is still responsible for widening the search"
                    .to_string(),
            );
        }
        super::PortfolioRefreshTriggerKind::LowDiversity => {
            let family_count = status.family_count.unwrap_or(0);
            reasons.push(format!(
                "portfolio diversity is low at {family_count}/{} configured families",
                status.required_family_count
            ));
        }
        super::PortfolioRefreshTriggerKind::Standard => {
            reasons.push(format!(
                "portfolio diversity already meets the configured minimum of {} families",
                status.required_family_count
            ));
        }
    }
    match status.kind {
        super::PortfolioRefreshStatusKind::Ready => reasons.push(format!(
            "controller threshold is satisfied at {}/{} exploit cycles",
            status.current_exploit_cycles, status.required_exploit_cycles
        )),
        super::PortfolioRefreshStatusKind::WaitingForExploitCycles => reasons.push(format!(
            "waiting for {}/{} exploit cycles before refreshing the portfolio",
            status.current_exploit_cycles, status.required_exploit_cycles
        )),
        super::PortfolioRefreshStatusKind::CoolingDown => reasons.push(format!(
            "cooldown still has {} second(s) remaining",
            status.cooldown_remaining_seconds.unwrap_or_default()
        )),
        super::PortfolioRefreshStatusKind::BootstrapComplete => {
            reasons.push("bootstrap discovery already completed for this segment".to_string());
        }
        super::PortfolioRefreshStatusKind::Suppressed => {
            reasons.push(if state.wrap_up_requested {
                "portfolio refresh is suppressed during wrap-up".to_string()
            } else {
                "portfolio refresh is suppressed outside active research scheduling".to_string()
            });
        }
    }
    reasons
}

fn controller_decision_summary(
    selection_decision: AutoresearchSelectionDecision,
    selection_reasons: &[String],
    active_approach_id: Option<&str>,
    recommended_approach_id: Option<&str>,
    portfolio_refresh_decision: AutoresearchPortfolioRefreshDecision,
    portfolio_refresh_reasons: &[String],
    synthesis_suggestion: Option<&AutoresearchSynthesisSuggestion>,
) -> String {
    let mut clauses = Vec::new();
    if selection_decision != AutoresearchSelectionDecision::None {
        let selection_clause = match selection_decision {
            AutoresearchSelectionDecision::None => None,
            AutoresearchSelectionDecision::SwitchActiveApproach => Some(format!(
                "selection switched from `{}` to `{}`",
                active_approach_id.unwrap_or("(none)"),
                recommended_approach_id.unwrap_or("(none)")
            )),
            AutoresearchSelectionDecision::KeepActiveApproach => Some(format!(
                "selection kept `{}` while ranking `{}` as the current recommendation",
                active_approach_id.unwrap_or("(none)"),
                recommended_approach_id.unwrap_or(active_approach_id.unwrap_or("(none)"))
            )),
        };
        if let Some(selection_clause) = selection_clause {
            clauses.push(append_reasons(selection_clause, selection_reasons));
        }
    }
    if portfolio_refresh_decision != AutoresearchPortfolioRefreshDecision::NotApplicable {
        clauses.push(append_reasons(
            format!(
                "portfolio refresh is {}",
                portfolio_refresh_decision_label(portfolio_refresh_decision)
            ),
            portfolio_refresh_reasons,
        ));
    }
    if let Some(synthesis_suggestion) = synthesis_suggestion {
        clauses.push(
            synthesis_suggestion
                .synthesized_approach_id
                .as_deref()
                .map(|approach_id| {
                    format!(
                        "synthesis branch `{approach_id}`: `{}` + `{}`",
                        synthesis_suggestion.left_approach_id,
                        synthesis_suggestion.right_approach_id
                    )
                })
                .unwrap_or_else(|| {
                    format!(
                        "synthesis suggestion: `{}` + `{}`",
                        synthesis_suggestion.left_approach_id,
                        synthesis_suggestion.right_approach_id
                    )
                }),
        );
    }
    if clauses.is_empty() {
        "controller planned the next cycle without additional decisions".to_string()
    } else {
        clauses.join(" | ")
    }
}

fn append_reasons(prefix: String, reasons: &[String]) -> String {
    if reasons.is_empty() {
        prefix
    } else {
        format!("{prefix}: {}", reasons.join("; "))
    }
}

fn portfolio_refresh_decision_label(
    decision: AutoresearchPortfolioRefreshDecision,
) -> &'static str {
    match decision {
        AutoresearchPortfolioRefreshDecision::NotApplicable => "not_applicable",
        AutoresearchPortfolioRefreshDecision::Queued => "queued",
        AutoresearchPortfolioRefreshDecision::Waiting => "waiting",
        AutoresearchPortfolioRefreshDecision::CoolingDown => "cooling_down",
        AutoresearchPortfolioRefreshDecision::BootstrapComplete => "bootstrap_complete",
        AutoresearchPortfolioRefreshDecision::Suppressed => "suppressed",
    }
}

pub fn autoresearch_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTORESEARCH_STATE_FILE)
}

pub fn clear_thread_state(codex_home: &Path, thread_id: &str) -> std::io::Result<()> {
    save_thread_state(codex_home, thread_id, /*state*/ None)
}

fn autoresearch_state_lock_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTORESEARCH_STATE_LOCK_FILE)
}

fn load_thread_state(
    codex_home: &Path,
    thread_id: &str,
) -> std::io::Result<Option<AutoresearchRunState>> {
    let path = autoresearch_state_path(codex_home);
    let lock_path = autoresearch_state_lock_path(codex_home);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let lock_file = options.open(lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _guard = state_lock.write()?;

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let file: AutoresearchStateFile = serde_json::from_str(&contents).map_err(|err| {
        std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
    })?;
    Ok(file.threads.get(thread_id).cloned())
}

fn mutate_thread_state<T>(
    codex_home: &Path,
    thread_id: &str,
    mutator: impl FnOnce(&mut Option<AutoresearchRunState>) -> std::io::Result<T>,
) -> std::io::Result<(Option<AutoresearchRunState>, T)> {
    let path = autoresearch_state_path(codex_home);
    let lock_path = autoresearch_state_lock_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let lock_file = options.open(lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _guard = state_lock.write()?;

    let mut file = match std::fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str::<AutoresearchStateFile>(&contents).map_err(|err| {
                std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
            })?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => AutoresearchStateFile::default(),
        Err(err) => return Err(err),
    };

    let mut state = file.threads.get(thread_id).cloned();
    let output = mutator(&mut state)?;
    if let Some(state) = &state {
        file.threads.insert(thread_id.to_string(), state.clone());
    } else {
        file.threads.remove(thread_id);
    }

    let serialized = serde_json::to_string_pretty(&file).map_err(|err| {
        std::io::Error::other(format!("failed to serialize autoresearch state: {err}"))
    })?;
    let mut file_options = OpenOptions::new();
    file_options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        file_options.mode(0o600);
    }
    let mut output_file = file_options.open(path)?;
    output_file.write_all(serialized.as_bytes())?;
    output_file.flush()?;

    Ok((state, output))
}

fn save_thread_state(
    codex_home: &Path,
    thread_id: &str,
    state: Option<&AutoresearchRunState>,
) -> std::io::Result<()> {
    let path = autoresearch_state_path(codex_home);
    let lock_path = autoresearch_state_lock_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let lock_file = options.open(lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _guard = state_lock.write()?;

    let mut file = match std::fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str::<AutoresearchStateFile>(&contents).map_err(|err| {
                std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
            })?
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => AutoresearchStateFile::default(),
        Err(err) => return Err(err),
    };

    if let Some(state) = state {
        file.threads.insert(thread_id.to_string(), state.clone());
    } else {
        file.threads.remove(thread_id);
    }

    let serialized = serde_json::to_string_pretty(&file).map_err(|err| {
        std::io::Error::other(format!("failed to serialize autoresearch state: {err}"))
    })?;
    let mut file_options = OpenOptions::new();
    file_options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        file_options.mode(0o600);
    }
    let mut output_file = file_options.open(path)?;
    output_file.write_all(serialized.as_bytes())?;
    output_file.flush()?;
    Ok(())
}

pub fn build_init_prompt(request: &str) -> String {
    format!(
        "Autoresearch init directive: scaffold this workspace for the native autoresearch loop.\n\
         This instruction comes from the autoresearch controller, not from a user message.\n\n\
         User request:\n\
         {request}\n\n\
         Create or update the minimal files needed to make this workspace autoresearch-ready:\n\
         - {AUTORESEARCH_DOC_FILE}\n\
         - {AUTORESEARCH_SCRIPT_FILE}\n\
         - {AUTORESEARCH_CHECKS_FILE} (optional but preferred when you can define correctness gates)\n\
         - {AUTORESEARCH_IDEAS_FILE} (optional)\n\n\
         Requirements:\n\
         - Do not start the autonomous benchmark loop in this turn.\n\
         - Do not call `autoresearch_init`, `autoresearch_run`, or `autoresearch_log` in this setup turn.\n\
         - If the repo is empty or missing a benchmarkable target, scaffold the smallest practical project layout needed for the request.\n\
         - Make {AUTORESEARCH_SCRIPT_FILE} the canonical entrypoint for evaluation.\n\
         - {AUTORESEARCH_SCRIPT_FILE} must emit one primary `METRIC name=value` line and may emit additional `METRIC` lines for supporting metrics.\n\
         - If you choose a composite score as the primary metric, still emit the raw component metrics separately.\n\
         - Use {AUTORESEARCH_CHECKS_FILE} for hard constraints whenever possible.\n\
         - Add a bounded discovery policy when broader repo audit or online research should be used to challenge the current direction.\n\
         - Keep the setup pragmatic and runnable, not aspirational.\n\n\
         In {AUTORESEARCH_DOC_FILE}, write clear sections with these exact headings:\n\
         - Goal\n\
         - Primary Metric\n\
         - Hard Constraints\n\
         - Staged Targets\n\
         - Additional Metrics\n\
         - Composite Score Mode\n\
         - Exploration Policy\n\
         - Discovery Policy\n\
         - Hidden Constraints And Unknowns\n\
         - Benchmark Contract\n\
         - Checks Contract\n\
         - Notes And Assumptions\n\n\
         Guidance for those sections:\n\
         - In Primary Metric, write explicit bullets in this form: `- Name: <metric_name>`, `- Unit: <unit or blank>`, `- Direction: lower|higher`.\n\
         - Choose one primary metric, its unit, and whether lower or higher is better.\n\
         - If the task has multiple goals, decide which one is primary and which ones are hard constraints.\n\
         - If staged milestones are useful on the primary metric, list them in order under Staged Targets using explicit threshold expressions such as `- latency_ms <= 500 ms`.\n\
         - If weighting tradeoffs is useful, choose a composite score formula and explain the weights.\n\
         - In Exploration Policy, define how aggressively to explore versus exploit, including early diversity and periodic escape attempts.\n\
         - In Discovery Policy, explain when broader repo audit, online research, or parallel sub-agents should be used instead of immediately running another local benchmark attempt.\n\
         - In Hidden Constraints And Unknowns, list the biggest unverified assumptions, likely measurement artifacts, and missing evidence.\n\
         - Make it easy for a later autoresearch cycle to read {AUTORESEARCH_DOC_FILE} and call `autoresearch_init` with the documented primary metric.\n\n\
         Finish by summarizing what you scaffolded, what you chose as the primary metric, which constraints you encoded, and any blockers.",
    )
}

pub fn build_open_init_prompt(request: &str) -> String {
    format!(
        "Autoresearch init directive: scaffold this workspace for open-ended research or scientist mode.\n\
         This instruction comes from the autoresearch controller, not from a user message.\n\n\
         User request:\n\
         {request}\n\n\
         Create or update the minimal files needed to make this workspace research-ready:\n\
         - {AUTORESEARCH_DOC_FILE}\n\
         - {AUTORESEARCH_SCRIPT_FILE}\n\
         - {AUTORESEARCH_CHECKS_FILE} (optional but preferred)\n\
         - {AUTORESEARCH_IDEAS_FILE} (optional)\n\n\
         Requirements:\n\
         - Do not start the autonomous loop in this turn.\n\
         - Do not call `autoresearch_init`, `autoresearch_run`, or `autoresearch_log` in this setup turn.\n\
         - Set up evaluation and discovery first. Do not lock the project into one concrete solution unless a tiny sanity baseline is needed to validate the harness.\n\
         - Make {AUTORESEARCH_SCRIPT_FILE} the canonical evaluator for later candidate runs.\n\
         - If you create a sanity baseline, keep it clearly labeled as a control, not as the favored direction.\n\
         - Encode the primary metric, constraints, candidate contract, exploration policy, discovery policy, validation policy, and report contract so later research or scientist cycles can discover, compare, and validate multiple distinct approaches.\n\n\
         In {AUTORESEARCH_DOC_FILE}, write clear sections with these exact headings:\n\
         - Goal\n\
         - Problem Framing\n\
         - Primary Metric\n\
         - Hard Constraints\n\
         - Staged Targets\n\
         - Additional Metrics\n\
         - Composite Score Mode\n\
         - Exploration Policy\n\
         - Discovery Policy\n\
         - Validation Policy\n\
         - Candidate Contract\n\
         - Selection Policy\n\
         - Report Contract\n\
         - Hidden Constraints And Unknowns\n\
         - Benchmark Contract\n\
         - Checks Contract\n\
         - Notes And Assumptions\n\n\
         Guidance:\n\
         - In Problem Framing, describe the problem at the capability level, not as a chosen architecture.\n\
         - In Primary Metric, use explicit bullets: `- Name: <metric_name>`, `- Unit: <unit or blank>`, `- Direction: lower|higher`.\n\
         - In Candidate Contract, describe what a candidate must provide so later cycles can scaffold different approaches against the same evaluator.\n\
         - In Selection Policy, explain how promising approaches are promoted, when dead ends are retired, and when a winner is declared.\n\
         - If you need to override the native controller thresholds, add explicit bullets there such as `- Weak Branch Score Gap: 8`, `- Stagnation Window: 3`, or `- Synthesis After Exploit Cycles: 2`; prose bullets are still allowed.\n\
         - In Exploration Policy, require early diversity across approach families and periodic portfolio refreshes.\n\
         - In Discovery Policy, explain when repo audit, online research, or sub-agents should be used to widen the search space.\n\
         - In Validation Policy, explain when candidate claims need reruns, adversarial checks, out-of-sample checks, or evaluator audits before they should meaningfully change the ranking.\n\
         - If you need to override the native portfolio-refresh discovery thresholds, add explicit bullets there such as `- Portfolio Refresh Minimum Families: 3`, `- Portfolio Refresh Exploit Cycles (Low Diversity): 2`, `- Portfolio Refresh Exploit Cycles: 5`, or `- Portfolio Refresh Cooldown Seconds: 300`; prose bullets are still allowed.\n\
         - In Report Contract, specify what each research wrap-up must report about hypotheses, strongest evidence, threats to validity, and next discriminating experiments.\n\
         - In Hidden Constraints And Unknowns, list the largest open questions that could change which approach family wins.\n\n\
         Finish by summarizing the evaluation harness, the primary metric, the validation/report contract, the candidate contract, and the most important unknowns.",
    )
}

fn build_cycle_prompt(
    state: &AutoresearchRunState,
    kind: AutoresearchCycleKind,
    now: DateTime<Local>,
    stage_progress: Option<&super::AutoresearchStageProgress>,
    research_guidance: Option<&ResearchCycleGuidance>,
) -> String {
    if kind == AutoresearchCycleKind::Research {
        return build_research_cycle_prompt(state, now, stage_progress, research_guidance);
    }
    let mode_line = match kind {
        AutoresearchCycleKind::Continue => {
            "Autoresearch directive: continue the autonomous benchmark loop."
        }
        AutoresearchCycleKind::Research => {
            unreachable!("research cycles use build_research_cycle_prompt")
        }
        AutoresearchCycleKind::Discovery => {
            unreachable!("discovery cycles use build_discovery_prompt")
        }
        AutoresearchCycleKind::WrapUp => {
            "Autoresearch directive: wrap up now. Do not start broad new experiments unless required to finish cleanly."
        }
    };
    let finish_line = match kind {
        AutoresearchCycleKind::Continue => {
            "- Stop this cycle after a coherent checkpoint. Do not ask whether to continue."
        }
        AutoresearchCycleKind::Research => {
            unreachable!("research cycles use build_research_cycle_prompt")
        }
        AutoresearchCycleKind::Discovery => {
            unreachable!("discovery cycles use build_discovery_prompt")
        }
        AutoresearchCycleKind::WrapUp => {
            "- Finish with a concise final report of the best result, what changed, and any remaining blockers."
        }
    };
    let max_runs_line = state
        .max_runs
        .map(|max_runs| format!("- max_runs: {max_runs}"))
        .unwrap_or_else(|| "- max_runs: none".to_string());
    let staged_targets_block = stage_progress
        .map(|progress| {
            if progress.has_issues() {
                format!(
                    "- Staged Targets: the staged-target configuration in {AUTORESEARCH_DOC_FILE} is invalid and must be repaired before you rely on it.\n\
                     - Current staged-target issues: {}.\n\
                     - Fix the Staged Targets section so each milestone uses the primary metric, compatible units, and an easier-to-harder order.\n",
                    progress.issue_summary()
                )
            } else if progress.all_reached() {
                format!(
                    "- Staged Targets: all {} staged targets are currently satisfied by the best kept run.\n\
                     - Keep pushing beyond the final milestone unless wrap-up or max-runs ends the loop.\n",
                    progress.total_stages()
                )
            } else if let Some(active_stage) = progress.active_stage() {
                format!(
                    "- Staged Targets: {} of {} staged targets are already satisfied by the best kept run.\n\
                     - Active staged target: {}/{} {}.\n\
                     - Once that target is satisfied by a kept run that still passes constraints, immediately advance to the next staged target instead of stopping.\n",
                    progress.achieved_count,
                    progress.total_stages(),
                    progress.active_stage_number().unwrap_or(1),
                    progress.total_stages(),
                    active_stage.display
                )
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
    format!(
        "{mode_line}\n\
         This instruction comes from the autoresearch controller, not from a user message.\n\n\
         Goal:\n\
         {goal}\n\n\
         Working files:\n\
         - {doc_file}\n\
         - {script_file}\n\
         - {checks_file} (optional)\n\
         - {ideas_file} (optional)\n\n\
         Loop rules:\n\
         - Read {doc_file} at the start of every cycle.\n\
         - Respect the Primary Metric, Hard Constraints, Staged Targets, Additional Metrics, Composite Score Mode, Exploration Policy, Discovery Policy, and Hidden Constraints And Unknowns sections in {doc_file} when they exist.\n\
         - Use the native tools `autoresearch_init`, `autoresearch_run`, `autoresearch_log`, and `autoresearch_request_discovery`.\n\
         - Call `autoresearch_init` using the primary metric documented in {doc_file} before the first benchmark run only when the current journal segment is not already initialized with the same metric config.\n\
         - Do not repeat `autoresearch_init` as cycle housekeeping. Repeating the same config is a no-op and should not be used to fake a fresh segment.\n\
         - `autoresearch_run` returns a `run_token`; you must pass that exact token to `autoresearch_log`.\n\
         - If local progress plateaus, assumptions look weak, current framing looks suspect, or broader architecture/evaluation discovery is needed, call `autoresearch_request_discovery` instead of browsing widely inside this cycle.\n\
         - `autoresearch_request_discovery` only queues the next discovery cycle when it returns a queued/success result. After that, stop the current cycle and wait for the dedicated discovery cycle before calling `autoresearch_log_discovery`.\n\
         - If {script_file} exists, run it instead of inventing a different benchmark command.\n\
         - If a composite score is the chosen primary metric, keep the raw component metrics visible in logs and benchmark output.\n\
         - If checks fail, log `checks_failed`, not `keep`.\n\
         - Keep promising deferred ideas in {ideas_file}.\n\
         - Do not ask the user whether to continue.\n\
         {staged_targets_block}\
         {finish_line}\n\n\
         Run context:\n\
         - now: {now}\n\
         - started_at: {started_at}\n\
         - iteration: {iteration}\n\
         {max_runs_line}",
        goal = state.goal,
        doc_file = AUTORESEARCH_DOC_FILE,
        script_file = AUTORESEARCH_SCRIPT_FILE,
        checks_file = AUTORESEARCH_CHECKS_FILE,
        ideas_file = AUTORESEARCH_IDEAS_FILE,
        now = now.to_rfc3339(),
        started_at = chrono::TimeZone::timestamp_opt(&Local, state.started_at, 0)
            .single()
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| state.started_at.to_string()),
        iteration = state.iteration_count.saturating_add(1),
        staged_targets_block = staged_targets_block,
    )
}

fn build_research_cycle_prompt(
    state: &AutoresearchRunState,
    now: DateTime<Local>,
    stage_progress: Option<&super::AutoresearchStageProgress>,
    research_guidance: Option<&ResearchCycleGuidance>,
) -> String {
    let journal_summary = AutoresearchJournal::load(&state.workdir)
        .ok()
        .map(|journal| journal.summary());
    let validation_policy = load_validation_policy_settings(&state.workdir);
    let governance = load_evaluation_governance_settings(&state.workdir);
    let mut portfolio_lines = journal_summary
        .as_ref()
        .map(|summary| {
            if summary.current_segment_approaches.is_empty() {
                "- Portfolio: no approaches registered yet.\n".to_string()
            } else {
                let mut lines = vec![format!(
                    "- Portfolio: {} approaches across {} families.\n",
                    summary.approach_count(),
                    summary.family_count()
                )];
                for approach in summary.current_segment_approaches.iter().take(5) {
                    let metric = approach
                        .best_metric
                        .map(format_metric)
                        .unwrap_or_else(|| "n/a".to_string());
                    lines.push(format!(
                        "  - {} [{}] status={} best={} runs={}\n",
                        approach.latest.approach_id,
                        approach.latest.family,
                        approach.latest.status.as_str(),
                        metric,
                        approach.total_runs
                    ));
                }
                lines.concat()
            }
        })
        .unwrap_or_else(|| "- Portfolio: unavailable.\n".to_string());
    if let Some(active_approach_id) = state.active_approach_id.as_deref() {
        portfolio_lines.push_str(&format!("- Active approach id: {active_approach_id}\n"));
    } else {
        portfolio_lines.push_str("- Active approach id: none\n");
    }
    let stage_lines = stage_progress
        .map(|progress| {
            if progress.has_issues() {
                format!(
                    "- Staged Targets: invalid\n- Stage warning: {}\n",
                    progress.issue_summary()
                )
            } else if let Some(active_stage) = progress.active_stage() {
                format!(
                    "- Active staged target: {}/{} {}\n",
                    progress.active_stage_number().unwrap_or(1),
                    progress.total_stages(),
                    active_stage.display
                )
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
    let selection_policy_block = research_guidance
        .filter(|guidance| !guidance.prompt_block.is_empty())
        .map(|guidance| format!("{}\n", guidance.prompt_block))
        .unwrap_or_else(|| {
            journal_summary
                .as_ref()
                .map(|summary| {
                    let selection_policy = load_selection_policy_settings(&state.workdir);
                    build_research_cycle_guidance(
                        summary,
                        &state.goal,
                        state.active_approach_id.as_deref(),
                        state.consecutive_exploit_cycles,
                        &selection_policy,
                        &validation_policy,
                        &governance,
                    )
                })
                .filter(|guidance| !guidance.prompt_block.is_empty())
                .map(|guidance| format!("{}\n", guidance.prompt_block))
                .unwrap_or_default()
        });
    let (directive_line, loop_heading, loop_rules, stop_line) = match state.mode {
        AutoresearchMode::Research => (
            "Autoresearch directive: continue the open-ended research loop.",
            "Research loop rules:",
            format!(
                "- Read {AUTORESEARCH_DOC_FILE}, {AUTORESEARCH_PLAYBOOK_FILE}, and the current journal state before acting.\n\
         - Keep the evaluator contract stable while exploring multiple distinct approach families.\n\
         - Use the native tools `autoresearch_log_approach`, `autoresearch_request_discovery`, `autoresearch_init`, `autoresearch_run`, `autoresearch_run_parallel`, `autoresearch_log`, and `autoresearch_log_validation`.\n\
         - Only call `autoresearch_init` when the current journal segment is not already initialized with the same metric config or when that config actually changes.\n\
         - Register or update approaches with `autoresearch_log_approach` before or after meaningful candidate work.\n\
         - If no strong active candidate exists, widen the search instead of overfitting the current tree.\n\
         - If you benchmark a candidate, attach the run to its `approach_id` when you call `autoresearch_log`.\n\
         - Use `autoresearch_run_parallel` for bounded head-to-head evaluation only after the compared approaches already have meaningful snapshots; log each returned token back into the journal with `autoresearch_log`.\n\
         - Use `autoresearch_request_discovery` when the portfolio lacks diversity, evidence is weak, or a wider literature/repo audit could change the ranking of approach families.\n\
         - After `autoresearch_request_discovery` returns a queued/success result, stop the current cycle and wait for the dedicated discovery cycle before calling `autoresearch_log_discovery`.\n\
         - Prefer distinct families over minor variants when adding new candidates.\n\
         - Keep dead ends and promising next steps in {AUTORESEARCH_IDEAS_FILE} when useful.\n"
            ),
            "- Stop after a coherent checkpoint. Do not ask whether to continue.",
        ),
        AutoresearchMode::Scientist => (
            "Autoresearch directive: continue the scientist loop.",
            "Scientist loop rules:",
            format!(
                "- Read {AUTORESEARCH_DOC_FILE}, {AUTORESEARCH_PLAYBOOK_FILE}, the current journal state, and any validation/report contract before acting.\n\
         - Treat each cycle as hypothesis-driven research: frame the question, choose the most discriminating experiment, and keep claims separate from evidence.\n\
         - Keep the evaluator contract stable while exploring multiple distinct approach families.\n\
         - Use the native tools `autoresearch_log_approach`, `autoresearch_request_discovery`, `autoresearch_init`, `autoresearch_run`, `autoresearch_run_parallel`, `autoresearch_log`, and `autoresearch_log_validation`.\n\
         - Only call `autoresearch_init` when the current journal segment is not already initialized with the same metric config or when that config actually changes.\n\
         - Register or update approaches with `autoresearch_log_approach` before or after meaningful candidate work, and preserve lineage when a candidate is synthesized or reframed.\n\
         - If you benchmark a candidate, attach the run to its `approach_id` when you call `autoresearch_log`.\n\
         - Use `autoresearch_run_parallel` when a bounded comparison across several already-materialized candidate snapshots is cheaper than serial reruns, then log each token explicitly.\n\
         - Before changing the portfolio ranking on a strong-looking result, prefer the cheapest validation step that meaningfully reduces noise, evaluator gaming risk, or overfitting risk.\n\
         - Record reruns, holdouts, adversarial checks, and evaluator audits with `autoresearch_log_validation` so promotion gates and the final report rely on explicit evidence.\n\
         - Use `autoresearch_request_discovery` when the portfolio lacks diversity, evidence is weak, evaluator assumptions look shaky, or a wider literature/repo audit could change the ranking of approach families.\n\
         - After `autoresearch_request_discovery` returns a queued/success result, stop the current cycle and wait for the dedicated discovery cycle before calling `autoresearch_log_discovery`.\n\
         - Prefer experiments that falsify weak assumptions, compare distinct families, or improve confidence in the current leader over another small tweak.\n\
         - Keep dead ends, threats to validity, and next discriminating experiments in {AUTORESEARCH_IDEAS_FILE} when useful.\n\
         - Treat {AUTORESEARCH_REPORT_FILE} as the wrap-up artifact contract and keep it consistent with the strongest logged evidence.\n"
            ),
            "- Stop after a coherent scientist checkpoint. End with a concise note on hypothesis, strongest evidence, confidence/risk, and the next discriminating experiment.",
        ),
        AutoresearchMode::Optimize => {
            unreachable!("optimize mode does not use the research cycle prompt")
        }
    };
    format!(
        "{directive_line}\n\
         This instruction comes from the autoresearch controller, not from a user message.\n\n\
         Goal:\n\
         {goal}\n\n\
         Working files:\n\
         - {doc_file}\n\
         - {script_file}\n\
         - {checks_file} (optional)\n\
         - {ideas_file} (optional)\n\
         - {playbook_file} (generated context)\n\
         - {report_file} (generated on wrap-up)\n\n\
         {loop_heading}\n\
         {loop_rules}\
         {stop_line}\n\n\
         Portfolio summary:\n\
         {portfolio_lines}\
         {selection_policy_block}\
         {stage_lines}\
         Run context:\n\
         - now: {now}\n\
         - started_at: {started_at}\n\
         - iteration: {iteration}\n\
         - discovery_passes_completed: {discovery_count}\n\
         - consecutive_exploit_cycles: {consecutive_exploit_cycles}\n",
        directive_line = directive_line,
        goal = state.goal,
        doc_file = AUTORESEARCH_DOC_FILE,
        script_file = AUTORESEARCH_SCRIPT_FILE,
        checks_file = AUTORESEARCH_CHECKS_FILE,
        ideas_file = AUTORESEARCH_IDEAS_FILE,
        playbook_file = AUTORESEARCH_PLAYBOOK_FILE,
        report_file = AUTORESEARCH_REPORT_FILE,
        loop_heading = loop_heading,
        loop_rules = loop_rules,
        stop_line = stop_line,
        portfolio_lines = portfolio_lines,
        selection_policy_block = selection_policy_block,
        stage_lines = stage_lines,
        now = now.to_rfc3339(),
        started_at = chrono::TimeZone::timestamp_opt(&Local, state.started_at, 0)
            .single()
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| state.started_at.to_string()),
        iteration = state.iteration_count.saturating_add(1),
        discovery_count = state.discovery_count,
        consecutive_exploit_cycles = state.consecutive_exploit_cycles,
    )
}

fn summarize_cycle_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_line = trimmed.lines().find(|line| !line.trim().is_empty())?.trim();
    Some(first_line.chars().take(200).collect())
}

pub(crate) fn format_metric(metric: f64) -> String {
    if metric.fract() == 0.0 {
        format!("{metric:.0}")
    } else {
        format!("{metric:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

fn discovery_request_would_be_dropped(state: &AutoresearchRunState) -> bool {
    let Some(max_runs) = state.max_runs else {
        return false;
    };
    let cycle_will_consume_last_run = matches!(
        state.active_cycle_kind,
        Some(AutoresearchCycleKind::Continue | AutoresearchCycleKind::Research)
    ) || (state.active_cycle_kind.is_none()
        && matches!(
            state.pending_cycle_kind,
            Some(AutoresearchCycleKind::Continue | AutoresearchCycleKind::Research)
        ));
    if cycle_will_consume_last_run {
        return state.iteration_count.saturating_add(1) >= max_runs;
    }
    state.iteration_count >= max_runs
}

fn next_candidate_counter(workdir: &Path) -> std::io::Result<u32> {
    let journal = AutoresearchJournal::load(workdir)?;
    let max_counter = journal
        .entries
        .iter()
        .filter_map(|entry| match entry {
            AutoresearchJournalEntry::Approach(approach) => approach
                .approach_id
                .strip_prefix("approach-")
                .and_then(|suffix| suffix.parse::<u32>().ok()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    Ok(max_counter)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AUTORESEARCH_JOURNAL_FILE;
    use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
    use crate::slop_fork::autoresearch::AutoresearchDiscoveryEntry;
    use crate::slop_fork::autoresearch::AutoresearchExperimentEntry;
    use crate::slop_fork::autoresearch::AutoresearchExperimentStatus;
    use crate::slop_fork::autoresearch::AutoresearchJournal;
    use crate::slop_fork::autoresearch::MetricDirection;
    use crate::slop_fork::autoresearch::workspace::AutoresearchWorkspaceMode;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn sample_workspace(workdir: &Path) -> AutoresearchWorkspace {
        AutoresearchWorkspace {
            mode: AutoresearchWorkspaceMode::Filesystem,
            workdir: workdir.to_path_buf(),
            git_root: None,
            git_branch: None,
            accepted_revision: None,
            snapshot_root: Some(workdir.join("snapshot")),
        }
    }

    fn seed_synthesis_ready_portfolio(
        journal: &mut AutoresearchJournal,
        include_existing_synthesis: bool,
    ) {
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for (
            approach_id,
            title,
            family,
            status,
            parent_approach_id,
            synthesis_parent_approach_ids,
            timestamp,
        ) in [
            (
                "approach-1",
                "baseline",
                "baseline",
                AutoresearchApproachStatus::DeadEnd,
                None,
                Vec::new(),
                1,
            ),
            (
                "approach-2",
                "retrieval",
                "retrieval",
                AutoresearchApproachStatus::Promising,
                None,
                Vec::new(),
                2,
            ),
            (
                "approach-3",
                "distillation",
                "distillation",
                AutoresearchApproachStatus::Active,
                None,
                Vec::new(),
                3,
            ),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: title.to_string(),
                    family: family.to_string(),
                    status,
                    summary: family.to_string(),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: parent_approach_id.map(str::to_string),
                    synthesis_parent_approach_ids,
                    timestamp,
                    segment: 0,
                })
                .expect("approach");
        }
        for experiment in [
            AutoresearchExperimentEntry {
                run: 1,
                commit: "bbb2222".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.70),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "retrieval improved".to_string(),
                timestamp: 10,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 2,
                commit: "ccc3333".to_string(),
                approach_id: Some("approach-3".to_string()),
                metric: Some(0.68),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "distillation improved".to_string(),
                timestamp: 11,
                segment: 0,
            },
        ] {
            journal.append_experiment(experiment).expect("experiment");
        }
        if include_existing_synthesis {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: "approach-4".to_string(),
                    title: "Synthesis: approach-2 + approach-3".to_string(),
                    family: "retrieval+distillation".to_string(),
                    status: AutoresearchApproachStatus::Promising,
                    summary: "Existing synthesis candidate".to_string(),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: Some("approach-2".to_string()),
                    synthesis_parent_approach_ids: vec![
                        "approach-2".to_string(),
                        "approach-3".to_string(),
                    ],
                    timestamp: 12,
                    segment: 0,
                })
                .expect("approach");
        }
    }

    #[test]
    fn wrap_up_cycle_when_max_runs_reached() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize tests".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(2),
                Local::now(),
            )
            .expect("start");
        runtime
            .update_state(|state| {
                state.as_mut().expect("state").iteration_count = 2;
                Ok(())
            })
            .expect("update");
        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::WrapUp);
    }

    #[test]
    fn complete_turn_preserves_paused_state_after_in_flight_pause() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize tests".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .unwrap()
        );
        assert!(runtime.pause().unwrap());

        assert!(
            runtime
                .complete_turn("turn-1", "done", Local::now())
                .unwrap()
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Paused);
        assert_eq!(state.wrap_up_requested, false);
        assert_eq!(
            state.status_message.as_deref(),
            Some("Autoresearch paused.")
        );
        assert!(
            runtime
                .prepare_cycle_submission(Local::now())
                .expect("prepare paused")
                .is_none()
        );
    }

    #[test]
    fn complete_turn_queues_wrap_up_after_in_flight_wrap_up_request() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize tests".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .unwrap()
        );
        assert!(runtime.request_wrap_up().unwrap());

        assert!(
            runtime
                .complete_turn("turn-1", "done", Local::now())
                .unwrap()
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Running);
        assert!(state.wrap_up_requested);
        assert_eq!(
            state.status_message.as_deref(),
            Some("Autoresearch will run the wrap-up cycle next.")
        );
        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare wrap-up")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::WrapUp);
    }

    #[test]
    fn complete_turn_queues_wrap_up_after_reaching_max_runs() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize tests".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(1),
                Local::now(),
            )
            .expect("start");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .unwrap()
        );

        assert!(
            runtime
                .complete_turn("turn-1", "done", Local::now())
                .unwrap()
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Running);
        assert!(state.wrap_up_requested);
        assert_eq!(
            state.status_message.as_deref(),
            Some("Autoresearch reached max runs and will wrap up on the next cycle.")
        );
        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare wrap-up")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::WrapUp);
    }

    #[test]
    fn pending_run_requires_matching_token() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize tests".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                /*max_runs*/ None,
                Local::now(),
            )
            .expect("start");
        runtime
            .store_pending_run(PendingRunResult {
                token: "token-1".to_string(),
                ..PendingRunResult::default()
            })
            .expect("store");
        assert!(runtime.take_pending_run("wrong").expect("take").is_none());
        assert_eq!(
            runtime
                .take_pending_run("token-1")
                .expect("take")
                .expect("run")
                .token,
            "token-1"
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_clears_stopped_in_flight_markers() {
        let codex_home = tempdir().expect("tempdir");
        let thread_id = "thread-1";
        let state = AutoresearchRunState {
            status: AutoresearchStatus::Stopped,
            pending_cycle_kind: Some(AutoresearchCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(AutoresearchCycleKind::Continue),
            active_turn_id: Some("turn-autoresearch".to_string()),
            last_submitted_turn_id: Some("turn-autoresearch".to_string()),
            ..AutoresearchRunState::default()
        };
        save_thread_state(codex_home.path(), thread_id, Some(&state)).expect("save");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id).expect("load");

        assert!(runtime.clear_orphaned_cycle_if_idle(Local::now()).unwrap());

        let cleared_state = runtime.state().expect("autoresearch state");
        assert_eq!(cleared_state.status, AutoresearchStatus::Stopped);
        assert_eq!(cleared_state.pending_cycle_kind, None);
        assert_eq!(cleared_state.submission_dispatched_at, None);
        assert_eq!(cleared_state.active_cycle_kind, None);
        assert_eq!(cleared_state.active_turn_id, None);
        assert_eq!(cleared_state.last_submitted_turn_id, None);
        assert_eq!(
            cleared_state.status_message.as_deref(),
            Some("Autoresearch cleared stale cycle state after the thread became idle.")
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_preserves_running_state() {
        let codex_home = tempdir().expect("tempdir");
        let thread_id = "thread-1";
        let state = AutoresearchRunState {
            status: AutoresearchStatus::Running,
            active_cycle_kind: Some(AutoresearchCycleKind::Continue),
            active_turn_id: Some("turn-autoresearch".to_string()),
            ..AutoresearchRunState::default()
        };
        save_thread_state(codex_home.path(), thread_id, Some(&state)).expect("save");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id).expect("load");

        assert!(!runtime.clear_orphaned_cycle_if_idle(Local::now()).unwrap());
        assert_eq!(runtime.state(), Some(&state));
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_for_control_clears_paused_in_flight_markers() {
        let codex_home = tempdir().expect("tempdir");
        let thread_id = "thread-1";
        let state = AutoresearchRunState {
            status: AutoresearchStatus::Paused,
            pending_cycle_kind: Some(AutoresearchCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(AutoresearchCycleKind::Continue),
            active_turn_id: Some("turn-autoresearch".to_string()),
            last_submitted_turn_id: Some("turn-autoresearch".to_string()),
            ..AutoresearchRunState::default()
        };
        save_thread_state(codex_home.path(), thread_id, Some(&state)).expect("save");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id).expect("load");

        assert!(
            runtime
                .clear_orphaned_cycle_if_idle_for_control(Local::now())
                .unwrap()
        );

        let cleared_state = runtime.state().expect("autoresearch state");
        assert_eq!(cleared_state.status, AutoresearchStatus::Paused);
        assert_eq!(cleared_state.pending_cycle_kind, None);
        assert_eq!(cleared_state.submission_dispatched_at, None);
        assert_eq!(cleared_state.active_cycle_kind, None);
        assert_eq!(cleared_state.active_turn_id, None);
        assert_eq!(cleared_state.last_submitted_turn_id, None);
        assert_eq!(
            cleared_state.status_message.as_deref(),
            Some("Autoresearch cleared stale cycle state after the thread became idle.")
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_for_control_clears_running_in_flight_markers() {
        let codex_home = tempdir().expect("tempdir");
        let thread_id = "thread-1";
        let state = AutoresearchRunState {
            status: AutoresearchStatus::Running,
            pending_cycle_kind: Some(AutoresearchCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(AutoresearchCycleKind::Continue),
            active_turn_id: Some("turn-autoresearch".to_string()),
            last_submitted_turn_id: Some("turn-autoresearch".to_string()),
            ..AutoresearchRunState::default()
        };
        save_thread_state(codex_home.path(), thread_id, Some(&state)).expect("save");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id).expect("load");

        assert!(
            runtime
                .clear_orphaned_cycle_if_idle_for_control(Local::now())
                .unwrap()
        );

        let cleared_state = runtime.state().expect("autoresearch state");
        assert_eq!(cleared_state.status, AutoresearchStatus::Running);
        assert_eq!(cleared_state.pending_cycle_kind, None);
        assert_eq!(cleared_state.submission_dispatched_at, None);
        assert_eq!(cleared_state.active_cycle_kind, None);
        assert_eq!(cleared_state.active_turn_id, None);
        assert_eq!(cleared_state.last_submitted_turn_id, None);
        assert_eq!(
            cleared_state.status_message.as_deref(),
            Some("Autoresearch cleared stale cycle state after the thread became idle.")
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_for_control_preserves_discovery_requests() {
        let codex_home = tempdir().expect("tempdir");
        let thread_id = "thread-1";
        let discovery_request = AutoresearchDiscoveryRequest {
            reason: AutoresearchDiscoveryReason::UserRequested,
            focus: Some("audit candidate families".to_string()),
            requested_at: 7,
        };
        let state = AutoresearchRunState {
            status: AutoresearchStatus::Running,
            pending_cycle_kind: Some(AutoresearchCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(AutoresearchCycleKind::Continue),
            active_turn_id: Some("turn-autoresearch".to_string()),
            last_submitted_turn_id: Some("turn-autoresearch".to_string()),
            queued_discovery_request: Some(discovery_request.clone()),
            active_discovery_request: Some(discovery_request.clone()),
            active_discovery_logged: true,
            ..AutoresearchRunState::default()
        };
        save_thread_state(codex_home.path(), thread_id, Some(&state)).expect("save");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), thread_id).expect("load");

        assert!(
            runtime
                .clear_orphaned_cycle_if_idle_for_control(Local::now())
                .unwrap()
        );

        let cleared_state = runtime.state().expect("autoresearch state");
        assert_eq!(
            cleared_state.queued_discovery_request,
            Some(discovery_request.clone())
        );
        assert_eq!(
            cleared_state.active_discovery_request,
            Some(discovery_request)
        );
        assert!(cleared_state.active_discovery_logged);
    }

    #[test]
    fn init_prompt_requires_structured_multimetric_setup() {
        let prompt = build_init_prompt("Create an OCR project with CER < 5%");

        assert!(prompt.contains("Autoresearch init directive"));
        assert!(prompt.contains("Primary Metric"));
        assert!(prompt.contains("`- Name: <metric_name>`"));
        assert!(prompt.contains("Hard Constraints"));
        assert!(prompt.contains("Staged Targets"));
        assert!(prompt.contains("Composite Score Mode"));
        assert!(prompt.contains("Exploration Policy"));
        assert!(prompt.contains("Discovery Policy"));
        assert!(prompt.contains("Hidden Constraints And Unknowns"));
        assert!(prompt.contains("Do not start the autonomous benchmark loop"));
        assert!(prompt.contains(
            "Do not call `autoresearch_init`, `autoresearch_run`, or `autoresearch_log`"
        ));
    }

    #[test]
    fn request_discovery_queues_discovery_cycle_before_next_experiment() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        assert!(
            runtime
                .request_discovery(
                    AutoresearchDiscoveryReason::Plateau,
                    Some("look for non-CTC architectures".to_string()),
                    Local::now(),
                )
                .expect("queue discovery")
        );

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert_eq!(plan.kind, AutoresearchCycleKind::Discovery);
        assert!(plan.prompt.contains("bounded discovery pass"));
        assert!(plan.prompt.contains("look for non-CTC architectures"));
        assert!(plan.prompt.contains("autoresearch_log_discovery"));
        assert!(
            plan.prompt
                .contains("After `autoresearch_log_discovery` succeeds")
        );
        assert!(!plan.prompt.contains("autoresearch_log_approach"));
    }

    #[test]
    fn complete_discovery_does_not_advance_iteration_count() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        runtime
            .request_discovery(
                AutoresearchDiscoveryReason::WeakAssumption,
                Some("check label noise".to_string()),
                Local::now(),
            )
            .expect("queue discovery");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .expect("activate")
        );

        assert!(
            runtime
                .complete_turn("turn-1", "Discovery complete", Local::now())
                .expect("complete")
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.iteration_count, 0);
        assert_eq!(state.discovery_count, 1);
        assert_eq!(state.active_discovery_request, None);
        assert_eq!(state.status, AutoresearchStatus::Running);
        assert_eq!(
            state.status_message.as_deref(),
            Some("Autoresearch completed a bounded discovery pass.")
        );
    }

    #[test]
    fn request_discovery_is_rejected_while_wrapping_up() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        assert!(runtime.request_wrap_up().expect("wrap up"));

        assert!(
            !runtime
                .request_discovery(
                    AutoresearchDiscoveryReason::StageComplete,
                    Some("look for next-stage ideas".to_string()),
                    Local::now(),
                )
                .expect("discovery request")
        );
    }

    #[test]
    fn request_discovery_is_rejected_when_current_cycle_would_hit_max_runs() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(1),
                Local::now(),
            )
            .expect("start");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .expect("activate")
        );

        assert!(
            !runtime
                .request_discovery(
                    AutoresearchDiscoveryReason::StageComplete,
                    Some("look for next-stage ideas".to_string()),
                    Local::now(),
                )
                .expect("discovery request")
        );
    }

    #[test]
    fn request_discovery_is_rejected_when_active_research_cycle_would_hit_max_runs() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(1),
                Local::now(),
            )
            .expect("start");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare discovery")
            .expect("discovery plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-discovery".to_string())
                .expect("activate discovery")
        );
        assert!(
            runtime
                .complete_turn("turn-discovery", "Discovery complete", Local::now())
                .expect("complete discovery")
        );

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare research")
            .expect("research plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Research);
        assert!(
            runtime
                .activate_pending_cycle("turn-research".to_string())
                .expect("activate research")
        );

        assert!(
            !runtime
                .request_discovery(
                    AutoresearchDiscoveryReason::PortfolioRefresh,
                    Some("expand candidate families".to_string()),
                    Local::now(),
                )
                .expect("discovery request")
        );
    }

    #[test]
    fn discovery_logging_is_allowed_once_per_active_pass() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        runtime
            .request_discovery(
                AutoresearchDiscoveryReason::Plateau,
                Some("look for alternate decoders".to_string()),
                Local::now(),
            )
            .expect("queue discovery");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .expect("activate")
        );

        assert!(runtime.mark_discovery_logged().expect("mark discovery"));
        assert!(
            !runtime
                .mark_discovery_logged()
                .expect("mark discovery once")
        );
        assert!(runtime.clear_discovery_logged().expect("clear discovery"));
        assert!(
            runtime
                .mark_discovery_logged()
                .expect("mark discovery again")
        );

        runtime
            .complete_turn("turn-1", "Discovery complete", Local::now())
            .expect("complete");
        assert!(!runtime.mark_discovery_logged().expect("inactive discovery"));
        assert!(!runtime.clear_discovery_logged().expect("inactive clear"));
    }

    #[test]
    fn submission_failure_pauses_running_session() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert!(
            runtime
                .note_submission_failure("submission failed")
                .expect("submission failure")
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Paused);
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.active_cycle_kind, None);
        assert_eq!(state.active_turn_id, None);
        assert_eq!(state.last_submitted_turn_id, None);
        assert_eq!(state.status_message.as_deref(), Some("submission failed"));
    }

    #[test]
    fn aborting_logged_discovery_retires_the_request() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");
        runtime
            .request_discovery(
                AutoresearchDiscoveryReason::FollowUp,
                Some("double-check decoder ideas".to_string()),
                Local::now(),
            )
            .expect("queue discovery");
        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .expect("activate")
        );
        assert!(runtime.mark_discovery_logged().expect("mark discovery"));

        assert!(
            runtime
                .abort_turn(Some("turn-1"), "aborted")
                .expect("abort")
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Paused);
        assert_eq!(state.active_discovery_request, None);
        assert_eq!(state.active_discovery_logged, false);
    }

    #[test]
    fn cycle_prompt_surfaces_current_staged_target() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        std::fs::write(
            workdir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nOptimize latency.\n\n## Staged Targets\n- latency_ms <= 500 ms\n- latency_ms <= 400 ms\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "latency".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 1,
                commit: "abc1234".to_string(),
                approach_id: None,
                metric: Some(480.0),
                metrics: Default::default(),
                status: AutoresearchExperimentStatus::Keep,
                description: "first milestone".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert!(
            plan.prompt
                .contains("Staged Targets: 1 of 2 staged targets are already satisfied")
        );
        assert!(
            plan.prompt
                .contains("Active staged target: 2/2 latency_ms <= 400 ms.")
        );
        assert!(
            plan.prompt
                .contains("immediately advance to the next staged target instead of stopping")
        );
        assert!(
            plan.prompt
                .contains("Do not repeat `autoresearch_init` as cycle housekeeping")
        );
        assert!(
            plan.prompt
                .contains("`autoresearch_request_discovery` only queues the next discovery cycle")
        );
    }

    #[test]
    fn cycle_prompt_flags_invalid_staged_targets() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        std::fs::write(
            workdir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nOptimize latency.\n\n## Staged Targets\n- latency_ms <= 400 ms\n- latency_ms <= 500 ms\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "latency".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 1,
                commit: "abc1234".to_string(),
                approach_id: None,
                metric: Some(450.0),
                metrics: Default::default(),
                status: AutoresearchExperimentStatus::Keep,
                description: "bad milestone order".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert!(
            plan.prompt
                .contains("the staged-target configuration in autoresearch.md is invalid")
        );
        assert!(
            plan.prompt
                .contains("ordered from easier to harder on `latency_ms`")
        );
    }

    #[test]
    fn cycle_prompt_flags_invalid_staged_targets_before_journal_config_exists() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        std::fs::write(
            workdir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nOptimize latency.\n\n## Primary Metric\n- Name: latency_ms\n- Unit: ms\n- Direction: lower\n\n## Staged Targets\n- latency_ms <= 400 ms\n- latency_ms <= 500 ms\n",
        )
        .expect("write doc");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize latency".to_string(),
                AutoresearchMode::Optimize,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(4),
                Local::now(),
            )
            .expect("start");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert!(
            plan.prompt
                .contains("the staged-target configuration in autoresearch.md is invalid")
        );
        assert!(
            plan.prompt
                .contains("ordered from easier to harder on `latency_ms`")
        );
    }

    #[test]
    fn research_mode_starts_with_discovery_cycle() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");

        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Discovery);
        assert!(plan.prompt.contains("bounded discovery pass"));
        assert!(plan.prompt.contains("autoresearch_log_approach"));
        assert!(
            plan.prompt
                .contains("After `autoresearch_log_discovery` succeeds")
        );
        assert_eq!(
            runtime.state().expect("state").status_message.as_deref(),
            Some(
                "Autoresearch queued a bounded discovery pass: portfolio refresh (refresh the active approach portfolio)."
            )
        );
    }

    #[test]
    fn research_mode_does_not_livelock_after_empty_discovery_pass() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");

        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");

        let first_plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare discovery")
            .expect("discovery plan");
        assert_eq!(first_plan.kind, AutoresearchCycleKind::Discovery);
        assert!(
            runtime
                .activate_pending_cycle("turn-discovery".to_string())
                .expect("activate discovery")
        );
        assert!(
            runtime
                .complete_turn("turn-discovery", "Discovery complete", Local::now())
                .expect("complete discovery")
        );

        let second_plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare research")
            .expect("research plan");
        assert_eq!(second_plan.kind, AutoresearchCycleKind::Research);
    }

    #[test]
    fn research_mode_switches_to_stronger_candidate_when_active_branch_stagnates() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for (approach_id, family, status, summary) in [
            (
                "approach-1",
                "baseline",
                AutoresearchApproachStatus::Active,
                "Local baseline tweaks.",
            ),
            (
                "approach-2",
                "retrieval",
                AutoresearchApproachStatus::Promising,
                "Stronger retrieval reranker.",
            ),
            (
                "approach-3",
                "parser",
                AutoresearchApproachStatus::DeadEnd,
                "Parser rewrite kept breaking evaluation.",
            ),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status,
                    summary: summary.to_string(),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp: 1,
                    segment: 0,
                })
                .expect("approach");
        }
        for experiment in [
            AutoresearchExperimentEntry {
                run: 1,
                commit: "aaa1111".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.40),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "baseline tweak regressed retrieval quality".to_string(),
                timestamp: 10,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 2,
                commit: "bbb2222".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.42),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::ChecksFailed,
                description: "baseline tweak broke checks".to_string(),
                timestamp: 11,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 3,
                commit: "bbc3333".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.42),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Crash,
                description: "baseline tweak crashed again".to_string(),
                timestamp: 12,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 4,
                commit: "ccc3333".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.69),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "retrieval reranker baseline".to_string(),
                timestamp: 13,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 5,
                commit: "ddd4444".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.71),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "retrieval reranker improved ranking".to_string(),
                timestamp: 14,
                segment: 0,
            },
        ] {
            journal.append_experiment(experiment).expect("experiment");
        }
        journal
            .append_discovery(AutoresearchDiscoveryEntry {
                entry_type: "discovery".to_string(),
                reason: AutoresearchDiscoveryReason::FollowUp,
                focus: Some("retrieval".to_string()),
                summary: "Compared retrieval families.".to_string(),
                recommendations: vec![
                    "compare the reranker with a distilled retrieval stack".to_string(),
                ],
                unknowns: Vec::new(),
                sources: Vec::new(),
                dead_ends: Vec::new(),
                timestamp: 14,
                segment: 0,
            })
            .expect("discovery");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.last_discovery_completed_at = Some(Local::now().timestamp());
                state.active_approach_id = Some("approach-1".to_string());
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert_eq!(plan.kind, AutoresearchCycleKind::Research);
        assert_eq!(
            runtime
                .state()
                .expect("state")
                .active_approach_id
                .as_deref(),
            Some("approach-1")
        );
        assert_eq!(
            runtime
                .state()
                .expect("state")
                .pending_active_approach_id
                .as_deref(),
            Some("approach-2")
        );
        assert_eq!(
            runtime.state().expect("state").status_message.as_deref(),
            Some("Autoresearch queued the next research cycle on approach `approach-2`.")
        );
        assert!(plan.prompt.contains("Selection policy context:"));
        assert!(plan.prompt.contains("switch to `approach-2` [retrieval]"));
        assert!(plan.prompt.contains("`approach-1` looks stagnant"));
        assert!(
            plan.prompt
                .contains("Family memory: `retrieval` is the strongest surviving family so far")
        );
        assert!(plan.prompt.contains("already has 3 non-keeps"));
        assert!(plan.prompt.contains("materially different hypothesis"));
        assert!(
            plan.prompt
                .contains("Dead-end memory: `approach-3` [parser] already dead-ended")
        );
        assert!(
            plan.prompt
                .contains("Discovery memory: carry forward \"compare the reranker")
        );
        assert!(plan.prompt.contains("Synthesis opportunity"));
    }

    #[test]
    fn research_cycle_respects_selection_policy_from_autoresearch_doc() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        std::fs::write(
            workdir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nImprove retrieval quality.\n\n## Selection Policy\n- Weak Branch Score Gap: 50\n- Stagnation Window: 4\n- Synthesis After Exploit Cycles: 5\n",
        )
        .expect("write doc");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for (approach_id, family, status) in [
            ("approach-1", "baseline", AutoresearchApproachStatus::Active),
            (
                "approach-2",
                "retrieval",
                AutoresearchApproachStatus::Promising,
            ),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status,
                    summary: family.to_string(),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp: 1,
                    segment: 0,
                })
                .expect("approach");
        }
        for experiment in [
            AutoresearchExperimentEntry {
                run: 1,
                commit: "aaa".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.40),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "baseline regressed".to_string(),
                timestamp: 10,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 2,
                commit: "bbb".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.42),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::ChecksFailed,
                description: "baseline broke checks".to_string(),
                timestamp: 11,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 3,
                commit: "ccc".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.69),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "reranker baseline".to_string(),
                timestamp: 12,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 4,
                commit: "ddd".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.71),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "reranker improved".to_string(),
                timestamp: 13,
                segment: 0,
            },
        ] {
            journal.append_experiment(experiment).expect("experiment");
        }

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.active_approach_id = Some("approach-1".to_string());
                state.consecutive_exploit_cycles = 1;
                state.discovery_count = 1;
                state.last_discovery_completed_at = Some(Local::now().timestamp());
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert_eq!(plan.kind, AutoresearchCycleKind::Research);
        assert_eq!(
            runtime.state().expect("state").pending_active_approach_id,
            None
        );
        assert!(
            plan.prompt
                .contains("Configured selection policy: weak-branch score gap >= 50")
        );
        assert!(!plan.prompt.contains("`approach-1` looks stagnant"));
        assert!(!plan.prompt.contains("Synthesis opportunity"));
    }

    #[test]
    fn aborting_rotated_research_cycle_restores_previous_active_approach() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        std::fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let research_workspace = AutoresearchResearchWorkspace::new(codex_home.path(), "thread-1");
        std::fs::write(workdir.path().join("code.txt"), "approach-1 accepted").expect("write");
        research_workspace
            .keep_approach_snapshot(workdir.path(), "approach-1")
            .expect("snapshot");
        std::fs::write(workdir.path().join("code.txt"), "approach-2 accepted").expect("write");
        research_workspace
            .keep_approach_snapshot(workdir.path(), "approach-2")
            .expect("snapshot");
        std::fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for (approach_id, family, status) in [
            ("approach-1", "baseline", AutoresearchApproachStatus::Active),
            (
                "approach-2",
                "retrieval",
                AutoresearchApproachStatus::Promising,
            ),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status,
                    summary: family.to_string(),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp: 1,
                    segment: 0,
                })
                .expect("approach");
        }
        for experiment in [
            AutoresearchExperimentEntry {
                run: 1,
                commit: "aaa1111".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.40),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "baseline regressed".to_string(),
                timestamp: 10,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 2,
                commit: "bbb2222".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.41),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "baseline regressed again".to_string(),
                timestamp: 11,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 3,
                commit: "ccc3333".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.70),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "retrieval improved".to_string(),
                timestamp: 12,
                segment: 0,
            },
        ] {
            journal.append_experiment(experiment).expect("experiment");
        }

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-1".to_string());
                Ok(())
            })
            .expect("update state");

        let _ = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert!(
            runtime
                .activate_pending_cycle("turn-1".to_string())
                .expect("activate")
        );
        assert_eq!(
            std::fs::read_to_string(workdir.path().join("code.txt")).expect("read"),
            "approach-2 accepted"
        );
        assert_eq!(
            runtime
                .state()
                .expect("state")
                .active_approach_id
                .as_deref(),
            Some("approach-2")
        );

        assert!(
            runtime
                .abort_turn(Some("turn-1"), "aborted")
                .expect("abort")
        );
        assert_eq!(
            std::fs::read_to_string(workdir.path().join("code.txt")).expect("read"),
            "approach-1 accepted"
        );

        assert_eq!(
            runtime
                .state()
                .expect("state")
                .active_approach_id
                .as_deref(),
            Some("approach-1")
        );
        assert_eq!(
            runtime
                .state()
                .expect("state")
                .pending_active_approach_id
                .as_deref(),
            None
        );
    }

    #[test]
    fn portfolio_refresh_uses_last_discovery_timestamp() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "latency".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        for (approach_id, family) in [
            ("approach-1", "ctc"),
            ("approach-2", "attention"),
            ("approach-3", "transducer"),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status: AutoresearchApproachStatus::Active,
                    summary: format!("{family} candidate"),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp: 1,
                    segment: 0,
                })
                .expect("approach");
        }
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.consecutive_exploit_cycles = 5;
                state.last_cycle_completed_at = Some(now.timestamp());
                state.last_discovery_completed_at = Some(now.timestamp() - 301);
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert_eq!(plan.kind, AutoresearchCycleKind::Discovery);
    }

    #[test]
    fn prepare_cycle_submission_logs_controller_switch_decision() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for (approach_id, family, status, timestamp) in [
            (
                "approach-1",
                "baseline",
                AutoresearchApproachStatus::Active,
                1,
            ),
            (
                "approach-2",
                "retrieval",
                AutoresearchApproachStatus::Promising,
                2,
            ),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status,
                    summary: family.to_string(),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp,
                    segment: 0,
                })
                .expect("approach");
        }
        for experiment in [
            AutoresearchExperimentEntry {
                run: 1,
                commit: "aaa1111".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.40),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "baseline regressed".to_string(),
                timestamp: 10,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 2,
                commit: "bbb2222".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.41),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "baseline regressed again".to_string(),
                timestamp: 11,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 3,
                commit: "ccc3333".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.70),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "retrieval improved".to_string(),
                timestamp: 12,
                segment: 0,
            },
        ] {
            journal.append_experiment(experiment).expect("experiment");
        }

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-1".to_string());
                state.consecutive_exploit_cycles = 1;
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Research);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let decision = journal
            .summary()
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.selection_decision,
            AutoresearchSelectionDecision::SwitchActiveApproach
        );
        assert_eq!(decision.active_approach_id.as_deref(), Some("approach-1"));
        assert_eq!(
            decision.recommended_approach_id.as_deref(),
            Some("approach-2")
        );
        assert_eq!(
            decision.portfolio_refresh_decision,
            AutoresearchPortfolioRefreshDecision::Waiting
        );
        assert!(
            decision
                .summary
                .contains("selection switched from `approach-1` to `approach-2`")
        );
    }

    #[test]
    fn prepare_cycle_submission_materializes_synthesized_candidate() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        seed_synthesis_ready_portfolio(&mut journal, /*include_existing_synthesis*/ false);

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-3".to_string());
                state.consecutive_exploit_cycles = 2;
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(now)
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Research);
        assert!(
            plan.prompt.contains("Controller synthesis action:"),
            "{}",
            plan.prompt
        );
        assert!(plan.prompt.contains("approach-4"), "{}", plan.prompt);
        assert_eq!(
            runtime
                .state()
                .expect("state")
                .pending_active_approach_id
                .as_deref(),
            Some("approach-4")
        );

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let summary = journal.summary();
        let synthesized = summary
            .latest_approach("approach-4")
            .expect("synthesized approach");
        let mut lineage = synthesized.latest.synthesis_parent_approach_ids.clone();
        lineage.sort();
        assert_eq!(
            lineage,
            vec!["approach-2".to_string(), "approach-3".to_string()]
        );
        assert_eq!(
            synthesized.latest.status,
            AutoresearchApproachStatus::Planned
        );

        let decision = summary
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.recommended_approach_id.as_deref(),
            Some("approach-4")
        );
        assert_eq!(
            decision
                .synthesis_suggestion
                .as_ref()
                .and_then(|suggestion| suggestion.synthesized_approach_id.as_deref()),
            Some("approach-4")
        );
        assert!(decision.summary.contains("synthesis branch `approach-4`"));
    }

    #[test]
    fn prepare_cycle_submission_reuses_existing_synthesized_candidate() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        seed_synthesis_ready_portfolio(&mut journal, /*include_existing_synthesis*/ true);

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-3".to_string());
                state.consecutive_exploit_cycles = 2;
                Ok(())
            })
            .expect("update state");

        let _ = runtime
            .prepare_cycle_submission(now)
            .expect("prepare")
            .expect("plan");
        assert_eq!(
            runtime
                .state()
                .expect("state")
                .pending_active_approach_id
                .as_deref(),
            Some("approach-4")
        );

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let summary = journal.summary();
        assert!(summary.latest_approach("approach-5").is_none());
        assert_eq!(summary.approach_count(), 4);
        let decision = summary
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.recommended_approach_id.as_deref(),
            Some("approach-4")
        );
        assert_eq!(
            decision
                .synthesis_suggestion
                .as_ref()
                .and_then(|suggestion| suggestion.synthesized_approach_id.as_deref()),
            Some("approach-4")
        );
    }

    #[test]
    fn prepare_cycle_submission_does_not_mark_direct_switch_as_synthesized_branch() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        seed_synthesis_ready_portfolio(&mut journal, /*include_existing_synthesis*/ false);

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-1".to_string());
                state.consecutive_exploit_cycles = 2;
                Ok(())
            })
            .expect("update state");

        let _ = runtime
            .prepare_cycle_submission(now)
            .expect("prepare")
            .expect("plan");

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let decision = journal
            .summary()
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_ne!(
            decision.recommended_approach_id.as_deref(),
            Some("approach-1")
        );
        assert_eq!(
            decision
                .synthesis_suggestion
                .as_ref()
                .and_then(|suggestion| suggestion.synthesized_approach_id.as_deref()),
            None
        );
        let suggestion = decision
            .synthesis_suggestion
            .as_ref()
            .expect("synthesis suggestion");
        let mut lineage = [
            suggestion.left_approach_id.clone(),
            suggestion.right_approach_id.clone(),
        ];
        lineage.sort();
        assert_eq!(
            lineage,
            ["approach-2".to_string(), "approach-3".to_string()]
        );
        assert!(decision.summary.contains("synthesis suggestion:"));
        assert!(!decision.summary.contains("synthesis branch `"));
    }

    #[test]
    fn controller_decision_uses_none_selection_without_an_active_branch() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find promising approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .update_state(|state| {
                state.as_mut().expect("state").discovery_count = 1;
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Research);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let decision = journal
            .summary()
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.selection_decision,
            AutoresearchSelectionDecision::None
        );
        assert!(!decision.summary.contains("selection kept"));
    }

    #[test]
    fn non_refresh_discovery_controller_decision_omits_refresh_trigger() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find promising approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        assert!(
            runtime
                .request_discovery(
                    AutoresearchDiscoveryReason::Plateau,
                    Some("look for alternatives".to_string()),
                    now,
                )
                .expect("queue discovery")
        );

        let plan = runtime
            .prepare_cycle_submission(now)
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Discovery);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let decision = journal
            .summary()
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.portfolio_refresh_decision,
            AutoresearchPortfolioRefreshDecision::NotApplicable
        );
        assert_eq!(decision.portfolio_refresh_trigger, None);
    }

    #[test]
    fn portfolio_refresh_respects_configured_discovery_policy() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        std::fs::write(
            workdir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nFind strong retrieval strategies.\n\n## Discovery Policy\n- Portfolio Refresh Minimum Families: 3\n- Portfolio Refresh Exploit Cycles: 3\n- Portfolio Refresh Cooldown Seconds: 60\n",
        )
        .expect("write doc");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for (approach_id, family) in [
            ("approach-1", "baseline"),
            ("approach-2", "retrieval"),
            ("approach-3", "distillation"),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status: AutoresearchApproachStatus::Active,
                    summary: format!("{family} candidate"),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp: 1,
                    segment: 0,
                })
                .expect("approach");
        }

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong retrieval strategies".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.consecutive_exploit_cycles = 3;
                state.last_cycle_completed_at = Some(now.timestamp());
                state.last_discovery_completed_at = Some(now.timestamp() - 61);
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");

        assert_eq!(plan.kind, AutoresearchCycleKind::Discovery);
    }

    #[test]
    fn portfolio_refresh_discovery_logs_controller_queue_decision() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "latency".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        for (approach_id, family) in [
            ("approach-1", "ctc"),
            ("approach-2", "attention"),
            ("approach-3", "transducer"),
        ] {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: approach_id.to_string(),
                    title: approach_id.to_string(),
                    family: family.to_string(),
                    status: AutoresearchApproachStatus::Active,
                    summary: format!("{family} candidate"),
                    rationale: String::new(),
                    risks: Vec::new(),
                    sources: Vec::new(),
                    parent_approach_id: None,
                    synthesis_parent_approach_ids: Vec::new(),
                    timestamp: 1,
                    segment: 0,
                })
                .expect("approach");
        }

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.consecutive_exploit_cycles = 5;
                state.last_cycle_completed_at = Some(now.timestamp());
                state.last_discovery_completed_at = Some(now.timestamp() - 301);
                Ok(())
            })
            .expect("update state");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::Discovery);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let decision = journal
            .summary()
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.portfolio_refresh_decision,
            AutoresearchPortfolioRefreshDecision::Queued
        );
        assert_eq!(
            decision.portfolio_refresh_trigger,
            Some(super::super::PortfolioRefreshTriggerKind::Standard)
        );
        assert!(
            decision
                .portfolio_refresh_reasons
                .iter()
                .any(|reason| reason
                    .contains("controller queued a portfolio-refresh discovery pass"))
        );
        assert!(decision.summary.contains("portfolio refresh is queued"));
    }

    #[test]
    fn prepare_cycle_submission_errors_when_controller_journal_cannot_be_loaded() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find promising approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        std::fs::create_dir(workdir.path().join(AUTORESEARCH_JOURNAL_FILE))
            .expect("create journal dir");

        assert!(runtime.prepare_cycle_submission(Local::now()).is_err());
        assert_eq!(runtime.state().expect("state").pending_cycle_kind, None);
    }

    #[test]
    fn prepare_cycle_submission_rolls_back_when_controller_append_fails_after_persist() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find promising approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        assert!(
            runtime
                .request_discovery(
                    AutoresearchDiscoveryReason::Plateau,
                    Some("look for alternatives".to_string()),
                    now,
                )
                .expect("queue discovery")
        );

        fail_next_post_prepare_controller_journal_append_for_test();

        assert!(runtime.prepare_cycle_submission(now).is_err());

        let state = runtime.state().expect("state");
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.active_discovery_request, None);
        assert_eq!(
            state.queued_discovery_request,
            Some(AutoresearchDiscoveryRequest {
                reason: AutoresearchDiscoveryReason::Plateau,
                focus: Some("look for alternatives".to_string()),
                requested_at: now.timestamp(),
            })
        );
        assert_eq!(
            state.status_message.as_deref(),
            Some("Autoresearch queued a bounded discovery pass.")
        );

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        assert!(journal.summary().last_controller_decision().is_none());
    }

    #[test]
    fn prepare_cycle_submission_rolls_back_synthesized_branch_after_partial_journal_append() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        seed_synthesis_ready_portfolio(&mut journal, /*include_existing_synthesis*/ false);

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-3".to_string());
                state.consecutive_exploit_cycles = 2;
                Ok(())
            })
            .expect("update state");

        fail_after_first_prepared_cycle_journal_line_for_test();

        assert!(runtime.prepare_cycle_submission(now).is_err());

        let state = runtime.state().expect("state");
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.pending_active_approach_id, None);
        assert_eq!(state.candidate_counter, 3);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let summary = journal.summary();
        assert!(summary.latest_approach("approach-4").is_none());
        assert!(summary.last_controller_decision().is_none());
    }

    #[test]
    fn submission_failure_rolls_back_prepared_synthesized_branch_and_controller_entry() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let now = Local::now();
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        seed_synthesis_ready_portfolio(&mut journal, /*include_existing_synthesis*/ false);

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "improve retrieval quality".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                now,
            )
            .expect("start");
        runtime
            .update_state(|state| {
                let state = state.as_mut().expect("state");
                state.discovery_count = 1;
                state.active_approach_id = Some("approach-3".to_string());
                state.consecutive_exploit_cycles = 2;
                Ok(())
            })
            .expect("update state");

        let _ = runtime
            .prepare_cycle_submission(now)
            .expect("prepare")
            .expect("plan");
        let prepared_journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        assert!(
            prepared_journal
                .summary()
                .latest_approach("approach-4")
                .is_some()
        );
        assert!(
            prepared_journal
                .summary()
                .last_controller_decision()
                .is_some()
        );

        assert!(
            runtime
                .note_submission_failure("submission failed")
                .expect("submission failure")
        );

        let state = runtime.state().expect("state");
        assert_eq!(state.status, AutoresearchStatus::Paused);
        assert_eq!(state.pending_cycle_kind, None);
        assert_eq!(state.pending_active_approach_id, None);
        assert_eq!(state.pending_prepared_journal_original_len, None);
        assert_eq!(state.pending_prepared_candidate_counter, None);
        assert_eq!(state.candidate_counter, 3);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let summary = journal.summary();
        assert!(summary.latest_approach("approach-4").is_none());
        assert!(summary.last_controller_decision().is_none());
    }

    #[test]
    fn wrap_up_controller_decision_records_suppressed_refresh_reason() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
            .expect("prepare research workspace");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "wrap up promising approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime.request_wrap_up().expect("wrap up");

        let plan = runtime
            .prepare_cycle_submission(Local::now())
            .expect("prepare")
            .expect("plan");
        assert_eq!(plan.kind, AutoresearchCycleKind::WrapUp);

        let journal = AutoresearchJournal::load(workdir.path()).expect("reload journal");
        let decision = journal
            .summary()
            .last_controller_decision()
            .expect("controller decision")
            .clone();
        assert_eq!(
            decision.portfolio_refresh_decision,
            AutoresearchPortfolioRefreshDecision::Suppressed
        );
        assert!(
            decision
                .portfolio_refresh_reasons
                .iter()
                .any(|reason| reason.contains("suppressed during wrap-up"))
        );
    }

    #[test]
    fn paused_research_session_does_not_restore_worktree_on_idle_check() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        std::fs::write(workdir.path().join("code.txt"), "baseline").expect("write");
        let research_workspace =
            AutoresearchResearchWorkspace::prepare(codex_home.path(), "thread-1", workdir.path())
                .expect("prepare research workspace");
        std::fs::write(workdir.path().join("code.txt"), "accepted").expect("write");
        research_workspace
            .keep_approach_snapshot(workdir.path(), "approach-1")
            .expect("snapshot");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .note_approach_status("approach-1", AutoresearchApproachStatus::Active)
            .expect("activate");
        runtime.pause().expect("pause");
        std::fs::write(workdir.path().join("code.txt"), "manual").expect("write");

        assert!(
            runtime
                .prepare_cycle_submission(Local::now())
                .expect("prepare")
                .is_none()
        );
        assert_eq!(
            std::fs::read_to_string(workdir.path().join("code.txt")).expect("read"),
            "manual"
        );
    }

    #[test]
    fn research_start_continues_candidate_counter_from_existing_journal() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut journal = AutoresearchJournal::load(workdir.path()).expect("load journal");
        journal
            .append_approach(AutoresearchApproachEntry {
                entry_type: "approach".to_string(),
                approach_id: "approach-3".to_string(),
                title: "Existing lineage".to_string(),
                family: "baseline".to_string(),
                status: AutoresearchApproachStatus::Archived,
                summary: "Archived previous run".to_string(),
                rationale: String::new(),
                risks: Vec::new(),
                sources: Vec::new(),
                parent_approach_id: None,
                synthesis_parent_approach_ids: Vec::new(),
                timestamp: 1,
                segment: 0,
            })
            .expect("append approach");

        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");

        assert_eq!(
            runtime.allocate_approach_id().expect("allocate"),
            "approach-4"
        );
    }

    #[test]
    fn note_approach_status_clears_active_id_when_approach_is_downgraded() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "find strong OCR approaches".to_string(),
                AutoresearchMode::Research,
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                Some(6),
                Local::now(),
            )
            .expect("start");
        runtime
            .note_approach_status("approach-1", AutoresearchApproachStatus::Active)
            .expect("activate");
        runtime
            .note_approach_status("approach-1", AutoresearchApproachStatus::Tested)
            .expect("downgrade");

        assert_eq!(
            runtime
                .state()
                .expect("state")
                .active_approach_id
                .as_deref(),
            None
        );
    }

    #[test]
    fn open_init_prompt_requires_evaluation_first_scaffold() {
        let prompt = build_open_init_prompt("discover robust OCR strategies");
        assert!(prompt.contains("open-ended research or scientist mode"));
        assert!(prompt.contains("Set up evaluation and discovery first."));
        assert!(prompt.contains("Validation Policy"));
        assert!(prompt.contains("Candidate Contract"));
        assert!(prompt.contains("Selection Policy"));
        assert!(prompt.contains("Report Contract"));
        assert!(prompt.contains("Portfolio Refresh Minimum Families"));
    }
}
