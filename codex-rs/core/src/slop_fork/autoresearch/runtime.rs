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
use super::AUTORESEARCH_SCRIPT_FILE;
use super::AutoresearchDiscoveryReason;
use super::AutoresearchDiscoveryRequest;
use super::AutoresearchJournal;
use super::build_discovery_prompt;
use super::load_stage_progress;
use super::workspace::AutoresearchWorkspace;

const AUTORESEARCH_STATE_FILE: &str = ".codex-slop-fork-autoresearch-state.json";
const AUTORESEARCH_STATE_LOCK_FILE: &str = ".codex-slop-fork-autoresearch-state.lock";

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
pub enum AutoresearchCycleKind {
    Continue,
    Discovery,
    WrapUp,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchCyclePlan {
    pub kind: AutoresearchCycleKind,
    pub prompt: String,
    pub notify_on_completion: bool,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoresearchRunState {
    pub goal: String,
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
    pub queued_discovery_request: Option<AutoresearchDiscoveryRequest>,
    pub active_discovery_request: Option<AutoresearchDiscoveryRequest>,
    pub active_discovery_logged: bool,
    pub wrap_up_requested: bool,
    pub stop_requested_at: Option<i64>,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
    pub last_progress_at: Option<i64>,
    pub last_cycle_completed_at: Option<i64>,
    pub last_cycle_summary: Option<String>,
    pub last_agent_message: Option<String>,
    pub pending_run: Option<PendingRunResult>,
}

impl Default for AutoresearchRunState {
    fn default() -> Self {
        Self {
            goal: String::new(),
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
            queued_discovery_request: None,
            active_discovery_request: None,
            active_discovery_logged: false,
            wrap_up_requested: false,
            stop_requested_at: None,
            last_error: None,
            status_message: None,
            last_progress_at: None,
            last_cycle_completed_at: None,
            last_cycle_summary: None,
            last_agent_message: None,
            pending_run: None,
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
        workdir: PathBuf,
        workspace: AutoresearchWorkspace,
        max_runs: Option<u32>,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        self.update_state(|state| {
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
                queued_discovery_request: None,
                active_discovery_request: None,
                active_discovery_logged: false,
                wrap_up_requested: false,
                stop_requested_at: None,
                last_error: None,
                status_message: Some("Autoresearch started.".to_string()),
                last_progress_at: None,
                last_cycle_completed_at: None,
                last_cycle_summary: None,
                last_agent_message: None,
                pending_run: None,
            });
            Ok(true)
        })
    }

    pub fn pause(&mut self) -> std::io::Result<bool> {
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
            let submitted_turn_awaiting_start = state.pending_cycle_kind.is_some()
                && state.active_turn_id.is_none()
                && state.submission_dispatched_at.is_some();
            state.status = AutoresearchStatus::Paused;
            if !submitted_turn_awaiting_start {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(if submitted_turn_awaiting_start {
                "Autoresearch paused. The already-submitted cycle will finish first.".to_string()
            } else {
                "Autoresearch paused.".to_string()
            });
            Ok(true)
        })
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
            if !submitted_turn_awaiting_start {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(if submitted_turn_awaiting_start {
                "Autoresearch stopped. The already-submitted cycle may still finish.".to_string()
            } else {
                "Autoresearch stopped.".to_string()
            });
            Ok(true)
        })
    }

    pub fn clear_orphaned_cycle_if_idle(&mut self, now: DateTime<Local>) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if !matches!(
                state.status,
                AutoresearchStatus::Stopped | AutoresearchStatus::Completed
            ) {
                return Ok(false);
            }
            if state.pending_cycle_kind.is_none()
                && state.submission_dispatched_at.is_none()
                && state.active_cycle_kind.is_none()
                && state.active_turn_id.is_none()
                && state.last_submitted_turn_id.is_none()
            {
                return Ok(false);
            }

            state.pending_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.active_cycle_kind = None;
            state.active_turn_id = None;
            state.last_submitted_turn_id = None;
            state.queued_discovery_request = None;
            state.active_discovery_request = None;
            state.active_discovery_logged = false;
            state.updated_at = now.timestamp();
            state.status_message = Some(
                "Autoresearch cleared stale cycle state after the thread became idle.".to_string(),
            );
            Ok(true)
        })
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
                || (state.active_cycle_kind == Some(AutoresearchCycleKind::Continue)
                    && state.max_runs.is_some_and(|max_runs| {
                        state.iteration_count.saturating_add(1) >= max_runs
                    }))
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

    pub fn prepare_cycle_submission(
        &mut self,
        now: DateTime<Local>,
    ) -> std::io::Result<Option<AutoresearchCyclePlan>> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(None);
            };
            if !matches!(state.status, AutoresearchStatus::Running) {
                return Ok(None);
            }
            if state.pending_cycle_kind.is_some() || state.active_turn_id.is_some() {
                return Ok(None);
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
            } else {
                AutoresearchCycleKind::Continue
            };
            let stage_progress = AutoresearchJournal::load(&state.workdir)
                .ok()
                .map(|journal| journal.summary())
                .and_then(|summary| load_stage_progress(&state.workdir, &summary));
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
                AutoresearchCycleKind::Continue | AutoresearchCycleKind::WrapUp => {
                    build_cycle_prompt(state, kind, now, stage_progress.as_ref())
                }
            };
            Ok(Some(AutoresearchCyclePlan {
                kind,
                prompt,
                notify_on_completion: matches!(kind, AutoresearchCycleKind::WrapUp),
            }))
        })
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
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.status == AutoresearchStatus::Running {
                state.status = AutoresearchStatus::Paused;
            }
            state.pending_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.active_cycle_kind = None;
            state.active_turn_id = None;
            state.last_submitted_turn_id = None;
            state.active_discovery_logged = false;
            state.last_error = Some(reason.to_string());
            state.status_message = Some(reason.to_string());
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
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
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state
                .last_submitted_turn_id
                .as_deref()
                .is_some_and(|submitted_turn_id| submitted_turn_id != turn_id)
            {
                return Ok(false);
            }
            let Some(kind) = state.pending_cycle_kind.take() else {
                return Ok(false);
            };
            if state.status == AutoresearchStatus::Completed {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
                return Ok(false);
            }
            state.submission_dispatched_at = None;
            state.active_cycle_kind = Some(kind);
            state.active_turn_id = Some(turn_id);
            state.last_submitted_turn_id = state.active_turn_id.clone();
            state.last_error = None;
            if kind == AutoresearchCycleKind::Discovery {
                state.active_discovery_logged = false;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(match (state.status, kind) {
                (AutoresearchStatus::Running, AutoresearchCycleKind::Continue) => {
                    "Autoresearch started an experiment cycle.".to_string()
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
            Ok(true)
        })
    }

    pub fn complete_turn(
        &mut self,
        turn_id: &str,
        last_agent_message: &str,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        self.update_state(|state| {
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
            match completed_kind {
                Some(AutoresearchCycleKind::Continue) | Some(AutoresearchCycleKind::WrapUp) => {
                    state.iteration_count = state.iteration_count.saturating_add(1);
                }
                Some(AutoresearchCycleKind::Discovery) => {
                    state.discovery_count = state.discovery_count.saturating_add(1);
                }
                None => {}
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
                state.status_message = Some("Autoresearch completed its wrap-up cycle.".to_string());
            } else if matches!(state.status, AutoresearchStatus::Stopped) {
                state.status = AutoresearchStatus::Completed;
                state.wrap_up_requested = false;
                state.queued_discovery_request = None;
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
                state.pending_run = None;
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
        })
    }

    pub fn abort_turn(&mut self, turn_id: Option<&str>, reason: &str) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if let Some(turn_id) = turn_id
                && state.active_turn_id.as_deref() != Some(turn_id)
            {
                return Ok(false);
            }
            let had_active_cycle =
                state.active_turn_id.is_some() || state.pending_cycle_kind.is_some();
            if !had_active_cycle {
                return Ok(false);
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
            if completed_discovery_checkpoint {
                state.active_discovery_request = None;
                state.active_discovery_logged = false;
            }
            state.last_error = Some(reason.to_string());
            state.status_message =
                Some("Autoresearch paused because the active turn was aborted.".to_string());
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
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
            let Some(pending_run) = state.pending_run.as_ref() else {
                return Ok(None);
            };
            if pending_run.token != token {
                return Ok(None);
            }
            Ok(state.pending_run.take())
        })
    }

    pub fn clear_pending_run(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            let cleared = state.pending_run.take().is_some();
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

pub fn autoresearch_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTORESEARCH_STATE_FILE)
}

pub fn clear_thread_state(codex_home: &Path, thread_id: &str) -> std::io::Result<()> {
    save_thread_state(codex_home, thread_id, None)
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

fn build_cycle_prompt(
    state: &AutoresearchRunState,
    kind: AutoresearchCycleKind,
    now: DateTime<Local>,
    stage_progress: Option<&super::AutoresearchStageProgress>,
) -> String {
    let mode_line = match kind {
        AutoresearchCycleKind::Continue => {
            "Autoresearch directive: continue the autonomous benchmark loop."
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
         - Call `autoresearch_init` using the primary metric documented in {doc_file} before the first benchmark run of a segment or whenever you intentionally reinitialize the session.\n\
         - `autoresearch_run` returns a `run_token`; you must pass that exact token to `autoresearch_log`.\n\
         - If local progress plateaus, assumptions look weak, current framing looks suspect, or broader architecture/evaluation discovery is needed, call `autoresearch_request_discovery` instead of browsing widely inside this cycle.\n\
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

fn summarize_cycle_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_line = trimmed.lines().find(|line| !line.trim().is_empty())?.trim();
    Some(first_line.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn wrap_up_cycle_when_max_runs_reached() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize tests".to_string(),
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
                workdir.path().to_path_buf(),
                sample_workspace(workdir.path()),
                None,
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
    }

    #[test]
    fn complete_discovery_does_not_advance_iteration_count() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
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
    fn discovery_logging_is_allowed_once_per_active_pass() {
        let codex_home = tempdir().expect("tempdir");
        let workdir = tempdir().expect("workdir");
        let mut runtime = AutoresearchRuntime::load(codex_home.path(), "thread-1").expect("load");
        runtime
            .start(
                "optimize OCR latency".to_string(),
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
}
