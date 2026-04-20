use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::DateTime;
use chrono::Local;
use fd_lock::RwLock as FileRwLock;
use serde::Deserialize;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::session::session::Session;
use crate::session::turn::TurnRunOptions;
use crate::session::turn::run_turn_with_options;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskContext;
use codex_protocol::user_input::UserInput;

const PILOT_STATE_FILE: &str = ".codex-slop-fork-pilot-state.json";
const PILOT_STATE_LOCK_FILE: &str = ".codex-slop-fork-pilot-state.lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotStatus {
    Running,
    Paused,
    Stopped,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PilotCycleKind {
    Continue,
    WrapUp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PilotCyclePlan {
    pub kind: PilotCycleKind,
    pub prompt: String,
    pub notify_on_completion: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PilotRunState {
    pub goal: String,
    pub status: PilotStatus,
    pub started_at: i64,
    pub deadline_at: Option<i64>,
    pub updated_at: i64,
    pub iteration_count: u32,
    pub pending_cycle_kind: Option<PilotCycleKind>,
    pub submission_dispatched_at: Option<i64>,
    pub active_cycle_kind: Option<PilotCycleKind>,
    pub active_turn_id: Option<String>,
    pub last_submitted_turn_id: Option<String>,
    pub wrap_up_requested: bool,
    pub wrap_up_requested_at: Option<i64>,
    pub stop_requested_at: Option<i64>,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
    pub last_progress_at: Option<i64>,
    pub last_cycle_completed_at: Option<i64>,
    pub last_cycle_summary: Option<String>,
    pub last_cycle_kind: Option<PilotCycleKind>,
    pub last_agent_message: Option<String>,
}

impl Default for PilotRunState {
    fn default() -> Self {
        Self {
            goal: String::new(),
            status: PilotStatus::Stopped,
            started_at: 0,
            deadline_at: None,
            updated_at: 0,
            iteration_count: 0,
            pending_cycle_kind: None,
            submission_dispatched_at: None,
            active_cycle_kind: None,
            active_turn_id: None,
            last_submitted_turn_id: None,
            wrap_up_requested: false,
            wrap_up_requested_at: None,
            stop_requested_at: None,
            last_error: None,
            status_message: None,
            last_progress_at: None,
            last_cycle_completed_at: None,
            last_cycle_summary: None,
            last_cycle_kind: None,
            last_agent_message: None,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PilotStateFile {
    threads: BTreeMap<String, PilotRunState>,
}

#[derive(Debug, Clone)]
pub struct PilotRuntime {
    codex_home: PathBuf,
    thread_id: String,
    state: Option<PilotRunState>,
}

impl PilotRuntime {
    pub fn load(codex_home: &Path, thread_id: impl Into<String>) -> std::io::Result<Self> {
        let thread_id = thread_id.into();
        let state = load_thread_state(codex_home, &thread_id)?;
        Ok(Self {
            codex_home: codex_home.to_path_buf(),
            thread_id,
            state,
        })
    }

    pub fn state(&self) -> Option<&PilotRunState> {
        self.state.as_ref()
    }

    pub fn start(
        &mut self,
        goal: String,
        deadline_at: Option<i64>,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        self.update_state(|state| {
            if state.as_ref().is_some_and(|state| {
                !matches!(state.status, PilotStatus::Stopped | PilotStatus::Completed)
                    || state.active_turn_id.is_some()
                    || state.pending_cycle_kind.is_some()
            }) {
                return Ok(false);
            }
            *state = Some(PilotRunState {
                goal,
                status: PilotStatus::Running,
                started_at: now.timestamp(),
                deadline_at,
                updated_at: now.timestamp(),
                iteration_count: 0,
                pending_cycle_kind: None,
                submission_dispatched_at: None,
                active_cycle_kind: None,
                active_turn_id: None,
                last_submitted_turn_id: None,
                wrap_up_requested: false,
                wrap_up_requested_at: None,
                stop_requested_at: None,
                last_error: None,
                status_message: Some("Pilot started.".to_string()),
                last_progress_at: None,
                last_cycle_completed_at: None,
                last_cycle_summary: None,
                last_cycle_kind: None,
                last_agent_message: None,
            });
            Ok(true)
        })
    }

    pub fn pause(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(state.status, PilotStatus::Stopped | PilotStatus::Completed) {
                return Ok(false);
            }
            let submitted_turn_awaiting_start = state.pending_cycle_kind.is_some()
                && state.active_turn_id.is_none()
                && state.submission_dispatched_at.is_some();
            state.status = PilotStatus::Paused;
            if !submitted_turn_awaiting_start {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(if submitted_turn_awaiting_start {
                "Pilot paused. The already-submitted cycle will finish before pausing.".to_string()
            } else {
                "Pilot paused.".to_string()
            });
            Ok(true)
        })
    }

    pub fn resume(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(state.status, PilotStatus::Stopped | PilotStatus::Completed) {
                return Ok(false);
            }
            state.status = PilotStatus::Running;
            state.updated_at = Local::now().timestamp();
            state.status_message = Some("Pilot resumed.".to_string());
            Ok(true)
        })
    }

    pub fn request_wrap_up(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(state.status, PilotStatus::Stopped | PilotStatus::Completed) {
                return Ok(false);
            }
            state.wrap_up_requested = true;
            state.status = PilotStatus::Running;
            if state.wrap_up_requested_at.is_none() {
                state.wrap_up_requested_at = Some(Local::now().timestamp());
            }
            state.updated_at = Local::now().timestamp();
            state.status_message = Some("Pilot wrap-up requested.".to_string());
            Ok(true)
        })
    }

    pub fn stop(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(state.status, PilotStatus::Stopped | PilotStatus::Completed) {
                return Ok(false);
            }
            let submitted_turn_awaiting_start = state.pending_cycle_kind.is_some()
                && state.active_turn_id.is_none()
                && state.submission_dispatched_at.is_some();
            state.status = PilotStatus::Stopped;
            state.wrap_up_requested = false;
            if !submitted_turn_awaiting_start {
                state.pending_cycle_kind = None;
                state.submission_dispatched_at = None;
                state.last_submitted_turn_id = None;
            }
            state.stop_requested_at = Some(Local::now().timestamp());
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(if submitted_turn_awaiting_start {
                "Pilot stopped. The already-submitted cycle will finish before stopping."
                    .to_string()
            } else {
                "Pilot stopped.".to_string()
            });
            Ok(true)
        })
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
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if matches!(scope, StaleCycleRecoveryScope::TerminalOnly)
                && !matches!(state.status, PilotStatus::Stopped | PilotStatus::Completed)
            {
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
            state.updated_at = now.timestamp();
            state.status_message =
                Some("Pilot cleared stale cycle state after the thread became idle.".to_string());
            Ok(true)
        })
    }

    pub fn prepare_cycle_submission(
        &mut self,
        now: DateTime<Local>,
    ) -> std::io::Result<Option<PilotCyclePlan>> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(None);
            };
            if state.status != PilotStatus::Running
                || state.pending_cycle_kind.is_some()
                || state.active_turn_id.is_some()
            {
                return Ok(None);
            }
            let kind = if state.wrap_up_requested
                || state
                    .deadline_at
                    .is_some_and(|deadline_at| now.timestamp() >= deadline_at)
            {
                PilotCycleKind::WrapUp
            } else {
                PilotCycleKind::Continue
            };
            let prompt = build_cycle_prompt(state, kind, now);
            state.pending_cycle_kind = Some(kind);
            state.submission_dispatched_at = None;
            state.last_submitted_turn_id = None;
            state.last_cycle_kind = Some(kind);
            state.last_error = None;
            state.updated_at = now.timestamp();
            state.status_message = Some(match kind {
                PilotCycleKind::Continue => "Pilot queued the next autonomous cycle.".to_string(),
                PilotCycleKind::WrapUp => "Pilot queued the wrap-up cycle.".to_string(),
            });
            Ok(Some(PilotCyclePlan {
                kind,
                prompt,
                notify_on_completion: kind == PilotCycleKind::WrapUp,
            }))
        })
    }

    pub fn note_submission_dispatched(&mut self) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.pending_cycle_kind.is_none() || state.active_turn_id.is_some() {
                return Ok(false);
            }
            state.submission_dispatched_at = Some(Local::now().timestamp());
            state.updated_at = Local::now().timestamp();
            Ok(true)
        })
    }

    pub fn note_submission_failure(&mut self, message: &str) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if state.pending_cycle_kind.is_none() {
                return Ok(false);
            }
            state.last_submitted_turn_id = None;
            state.pending_cycle_kind = None;
            state.submission_dispatched_at = None;
            if state.status == PilotStatus::Running {
                state.status = PilotStatus::Paused;
            }
            state.last_error = Some(message.to_string());
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(message.to_string());
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
            state.submission_dispatched_at = Some(Local::now().timestamp());
            state.last_submitted_turn_id = Some(turn_id.to_string());
            state.updated_at = Local::now().timestamp();
            state.status_message =
                Some("Pilot submitted a cycle and is waiting for it to start.".to_string());
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
            if state.status == PilotStatus::Completed {
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
            state.updated_at = Local::now().timestamp();
            state.status_message = Some(match (state.status, kind) {
                (PilotStatus::Running, PilotCycleKind::Continue) => {
                    "Pilot is running the next autonomous cycle.".to_string()
                }
                (PilotStatus::Running, PilotCycleKind::WrapUp) => {
                    "Pilot is running the wrap-up cycle.".to_string()
                }
                (PilotStatus::Paused, _) => {
                    "Pilot is waiting for the already-submitted cycle to finish before pausing."
                        .to_string()
                }
                (PilotStatus::Stopped, _) => {
                    "Pilot is waiting for the already-submitted cycle to finish before stopping."
                        .to_string()
                }
                (PilotStatus::Completed, _) => unreachable!("completed pilot cannot activate"),
            });
            Ok(true)
        })
    }

    pub fn is_active_turn(&self, turn_id: &str) -> bool {
        self.state
            .as_ref()
            .and_then(|state| state.active_turn_id.as_deref())
            == Some(turn_id)
    }

    pub fn abort_turn(&mut self, turn_id: Option<&str>, reason: &str) -> std::io::Result<bool> {
        self.update_state(|state| {
            let Some(state) = state.as_mut() else {
                return Ok(false);
            };
            if let Some(turn_id) = turn_id {
                let matches_active_turn = state.active_turn_id.as_deref() == Some(turn_id);
                let matches_pending_turn = state.pending_cycle_kind.is_some()
                    && state
                        .last_submitted_turn_id
                        .as_deref()
                        .is_none_or(|id| id == turn_id);
                if !matches_active_turn && !matches_pending_turn {
                    return Ok(false);
                }
            }
            let had_active_cycle =
                state.active_turn_id.is_some() || state.pending_cycle_kind.is_some();
            if !had_active_cycle {
                return Ok(false);
            }
            state.pending_cycle_kind = None;
            state.active_turn_id = None;
            state.active_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.last_submitted_turn_id = None;
            if state.status == PilotStatus::Running {
                state.status = PilotStatus::Paused;
            }
            state.last_error = Some(reason.to_string());
            state.updated_at = Local::now().timestamp();
            state.status_message =
                Some("Pilot paused because the active turn was aborted.".to_string());
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

            let completed_cycle = state.active_cycle_kind;
            state.active_turn_id = None;
            state.active_cycle_kind = None;
            state.submission_dispatched_at = None;
            state.last_submitted_turn_id = None;
            state.iteration_count = state.iteration_count.saturating_add(1);
            state.updated_at = now.timestamp();
            state.last_progress_at = Some(now.timestamp());
            state.last_cycle_completed_at = Some(now.timestamp());
            state.last_error = None;
            if !last_agent_message.trim().is_empty() {
                state.last_agent_message = Some(last_agent_message.to_string());
                state.last_cycle_summary = summarize_cycle_message(last_agent_message);
            }
            state.last_cycle_kind = completed_cycle;
            if completed_cycle == Some(PilotCycleKind::WrapUp) {
                state.wrap_up_requested = false;
                state.status = PilotStatus::Completed;
                state.status_message = Some("Pilot completed its wrap-up cycle.".to_string());
            } else if state
                .deadline_at
                .is_some_and(|deadline_at| now.timestamp() >= deadline_at)
            {
                state.wrap_up_requested = true;
                state.wrap_up_requested_at.get_or_insert(now.timestamp());
                state.status_message = Some(
                    "Pilot reached its deadline and will wrap up on the next cycle.".to_string(),
                );
            } else {
                state.status_message = Some("Pilot completed a cycle.".to_string());
            }
            Ok(true)
        })
    }

    fn update_state<T>(
        &mut self,
        mutator: impl FnOnce(&mut Option<PilotRunState>) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        let (state, output) = mutate_thread_state(&self.codex_home, &self.thread_id, mutator)?;
        self.state = state;
        Ok(output)
    }
}

pub(crate) async fn spawn_turn_task(
    sess: &Arc<Session>,
    turn_context: Arc<TurnContext>,
    prompt: String,
) {
    sess.spawn_task(
        Arc::clone(&turn_context),
        Vec::new(),
        PilotTask::new(prompt),
    )
    .await;
}

struct PilotTask {
    prompt: String,
}

impl PilotTask {
    fn new(prompt: String) -> Self {
        Self { prompt }
    }
}

impl SessionTask for PilotTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.pilot"
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        cancellation_token: CancellationToken,
    ) -> Option<String> {
        let sess = session.clone_session();
        sess.set_server_reasoning_included(/*included*/ false).await;
        if !input.is_empty() {
            tracing::warn!("pilot turn ignored unexpected user input items");
        }
        run_turn_with_options(
            sess,
            ctx,
            Vec::new(),
            TurnRunOptions {
                additional_instructions: Some(self.prompt.clone()),
                prewarmed_client_session: None,
            },
            cancellation_token,
        )
        .await
    }
}

pub fn pilot_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join(PILOT_STATE_FILE)
}

pub fn clear_thread_state(codex_home: &Path, thread_id: &str) -> std::io::Result<()> {
    save_thread_state(codex_home, thread_id, /*state*/ None)
}

fn pilot_state_lock_path(codex_home: &Path) -> PathBuf {
    codex_home.join(PILOT_STATE_LOCK_FILE)
}

fn load_thread_state(codex_home: &Path, thread_id: &str) -> std::io::Result<Option<PilotRunState>> {
    let path = pilot_state_path(codex_home);
    let lock_path = pilot_state_lock_path(codex_home);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let lock_file = options.open(&lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _guard = state_lock.write()?;

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let file: PilotStateFile = serde_json::from_str(&contents).map_err(|err| {
        std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
    })?;
    Ok(file.threads.get(thread_id).cloned())
}

fn mutate_thread_state<T>(
    codex_home: &Path,
    thread_id: &str,
    mutator: impl FnOnce(&mut Option<PilotRunState>) -> std::io::Result<T>,
) -> std::io::Result<(Option<PilotRunState>, T)> {
    let path = pilot_state_path(codex_home);
    let lock_path = pilot_state_lock_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let lock_file = options.open(&lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _guard = state_lock.write()?;

    let mut file = match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<PilotStateFile>(&contents).map_err(|err| {
            std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => PilotStateFile::default(),
        Err(err) => return Err(err),
    };

    let mut state = file.threads.get(thread_id).cloned();
    let output = mutator(&mut state)?;
    if let Some(state) = &state {
        file.threads.insert(thread_id.to_string(), state.clone());
    } else {
        file.threads.remove(thread_id);
    }

    let serialized = serde_json::to_string_pretty(&file)
        .map_err(|err| std::io::Error::other(format!("failed to serialize pilot state: {err}")))?;
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
    state: Option<&PilotRunState>,
) -> std::io::Result<()> {
    let path = pilot_state_path(codex_home);
    let lock_path = pilot_state_lock_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let lock_file = options.open(&lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _guard = state_lock.write()?;

    let mut file = match std::fs::read_to_string(&path) {
        Ok(contents) => serde_json::from_str::<PilotStateFile>(&contents).map_err(|err| {
            std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => PilotStateFile::default(),
        Err(err) => return Err(err),
    };

    if let Some(state) = state {
        file.threads.insert(thread_id.to_string(), state.clone());
    } else {
        file.threads.remove(thread_id);
    }

    let serialized = serde_json::to_string_pretty(&file)
        .map_err(|err| std::io::Error::other(format!("failed to serialize pilot state: {err}")))?;
    let mut file_options = OpenOptions::new();
    file_options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        file_options.mode(0o600);
    }
    let mut output = file_options.open(path)?;
    output.write_all(serialized.as_bytes())?;
    output.flush()?;
    Ok(())
}

enum StaleCycleRecoveryScope {
    TerminalOnly,
    ExplicitControl,
}

fn build_cycle_prompt(state: &PilotRunState, kind: PilotCycleKind, now: DateTime<Local>) -> String {
    let iteration = state.iteration_count.saturating_add(1);
    let started_at = chrono::TimeZone::timestamp_opt(&Local, state.started_at, 0)
        .single()
        .map(|ts| ts.to_rfc3339())
        .unwrap_or_else(|| state.started_at.to_string());
    let deadline_line = state
        .deadline_at
        .and_then(|deadline_at| chrono::TimeZone::timestamp_opt(&Local, deadline_at, 0).single())
        .map(|ts| format!("- deadline_at: {}", ts.to_rfc3339()))
        .unwrap_or_else(|| "- deadline_at: none".to_string());
    let mode_line = match kind {
        PilotCycleKind::Continue => {
            "Pilot directive: continue autonomous work toward the goal below."
        }
        PilotCycleKind::WrapUp => {
            "Pilot directive: wrap up now. Do not start broad new work unless it is required to finish cleanly."
        }
    };
    let finish_line = match kind {
        PilotCycleKind::Continue => {
            "- Stop this cycle after a coherent checkpoint. Do not ask whether you should continue; the pilot controller will decide."
        }
        PilotCycleKind::WrapUp => {
            "- Finish with a concise final report of what changed, what was verified, and any remaining blockers."
        }
    };
    format!(
        "{mode_line}\n\
         This instruction comes from the pilot controller, not from a user message.\n\
         The controller, not you, enforces the deadline. Do not spend effort checking the time.\n\n\
         Goal:\n\
         {goal}\n\n\
         Run context:\n\
         - iteration: {iteration}\n\
         - now: {now}\n\
         - started_at: {started_at}\n\
         {deadline_line}\n\n\
         Constraints:\n\
         - Prefer concrete progress over discussion.\n\
         - Use tools, tests, and verification when they materially help.\n\
         - Do not ask the user whether to continue.\n\
         {finish_line}",
        goal = state.goal,
        now = now.to_rfc3339(),
    )
}

fn summarize_cycle_message(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first_line = trimmed.lines().find(|line| !line.trim().is_empty())?.trim();
    let summary = first_line.chars().take(200).collect::<String>();
    Some(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn next_cycle_wraps_up_after_deadline() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let now = Local::now();
        let state = PilotRunState {
            goal: "ship it".to_string(),
            status: PilotStatus::Running,
            started_at: now.timestamp(),
            deadline_at: Some(now.timestamp() - 1),
            iteration_count: 2,
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();

        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();
        let cycle = runtime
            .prepare_cycle_submission(now)
            .unwrap()
            .expect("expected wrap-up cycle");
        assert_eq!(cycle.kind, PilotCycleKind::WrapUp);
        assert!(cycle.notify_on_completion);
        assert_eq!(
            runtime.state().and_then(|state| state.pending_cycle_kind),
            Some(PilotCycleKind::WrapUp)
        );
    }

    #[test]
    fn load_keeps_in_flight_cycle_state_intact() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            goal: "ship it".to_string(),
            status: PilotStatus::Running,
            started_at: 1,
            iteration_count: 1,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(PilotCycleKind::Continue),
            active_turn_id: Some("turn-1".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();

        let runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();
        assert_eq!(runtime.state(), Some(&state));
    }

    #[test]
    fn start_uses_latest_persisted_state() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let persisted = PilotRunState {
            goal: "already running".to_string(),
            status: PilotStatus::Running,
            started_at: 1,
            updated_at: 1,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&persisted)).unwrap();

        let mut runtime = PilotRuntime {
            codex_home: dir.path().to_path_buf(),
            thread_id: thread_id.to_string(),
            state: None,
        };

        let started = runtime
            .start(
                "replace goal".to_string(),
                /*deadline_at*/ None,
                Local::now(),
            )
            .unwrap();
        assert!(!started);
        assert_eq!(runtime.state(), Some(&persisted));
    }

    #[test]
    fn start_rejects_paused_and_running_runs_without_active_cycle() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";

        for status in [PilotStatus::Paused, PilotStatus::Running] {
            let persisted = PilotRunState {
                goal: "already active".to_string(),
                status,
                started_at: 1,
                updated_at: 1,
                ..PilotRunState::default()
            };
            save_thread_state(dir.path(), thread_id, Some(&persisted)).unwrap();

            let mut runtime = PilotRuntime {
                codex_home: dir.path().to_path_buf(),
                thread_id: thread_id.to_string(),
                state: None,
            };

            let started = runtime
                .start(
                    "replace goal".to_string(),
                    /*deadline_at*/ None,
                    Local::now(),
                )
                .unwrap();
            assert!(!started);
            assert_eq!(runtime.state(), Some(&persisted));
        }
    }

    #[test]
    fn pause_does_not_revive_stopped_run() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Stopped,
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        let paused = runtime.pause().unwrap();
        assert!(!paused);
        assert_eq!(
            runtime.state().map(|state| state.status),
            Some(PilotStatus::Stopped)
        );
    }

    #[test]
    fn stop_preserves_completed_run() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Completed,
            status_message: Some("Pilot completed its wrap-up cycle.".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        let stopped = runtime.stop().unwrap();
        assert!(!stopped);
        assert_eq!(runtime.state(), Some(&state));
    }

    #[test]
    fn pause_preserves_submitted_turn_until_start_event() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(runtime.pause().unwrap());
        assert!(
            runtime
                .activate_pending_cycle("turn-pilot".to_string())
                .unwrap()
        );
        assert!(
            runtime
                .complete_turn("turn-pilot", "done", Local::now())
                .unwrap()
        );
        assert_eq!(
            runtime.state().map(|state| state.status),
            Some(PilotStatus::Paused)
        );
        assert_eq!(
            runtime
                .state()
                .and_then(|state| state.active_turn_id.clone()),
            None
        );
    }

    #[test]
    fn stop_preserves_submitted_turn_until_start_event() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(runtime.stop().unwrap());
        assert!(
            runtime
                .activate_pending_cycle("turn-pilot".to_string())
                .unwrap()
        );
        assert!(
            runtime
                .complete_turn("turn-pilot", "done", Local::now())
                .unwrap()
        );
        assert_eq!(
            runtime.state().map(|state| state.status),
            Some(PilotStatus::Stopped)
        );
        assert_eq!(
            runtime
                .state()
                .and_then(|state| state.active_turn_id.clone()),
            None
        );
    }

    #[test]
    fn pause_clears_pre_start_pending_cycle_without_submitted_turn_id() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(runtime.pause().unwrap());
        let paused_state = runtime.state().expect("pilot state");
        assert_eq!(paused_state.status, PilotStatus::Paused);
        assert_eq!(paused_state.pending_cycle_kind, None);
        assert_eq!(paused_state.submission_dispatched_at, None);
        assert_eq!(paused_state.last_submitted_turn_id, None);
        assert_eq!(
            paused_state.status_message.as_deref(),
            Some("Pilot paused.")
        );
    }

    #[test]
    fn abort_turn_requires_matching_turn_id() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            active_cycle_kind: Some(PilotCycleKind::Continue),
            active_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        let aborted = runtime.abort_turn(Some("turn-other"), "mismatch").unwrap();
        assert!(!aborted);
        assert_eq!(
            runtime
                .state()
                .and_then(|state| state.active_turn_id.as_deref()),
            Some("turn-pilot")
        );
    }

    #[test]
    fn abort_turn_clears_pending_cycle_before_turn_started() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(Local::now().timestamp()),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        let aborted = runtime
            .abort_turn(Some("turn-pilot"), "interrupted before start")
            .unwrap();
        assert!(aborted);
        let paused_state = runtime
            .state()
            .cloned()
            .expect("pilot state after pre-start abort");
        assert_eq!(
            paused_state,
            PilotRunState {
                status: PilotStatus::Paused,
                last_error: Some("interrupted before start".to_string()),
                status_message: Some(
                    "Pilot paused because the active turn was aborted.".to_string()
                ),
                updated_at: paused_state.updated_at,
                ..PilotRunState::default()
            }
        );
    }

    #[test]
    fn activate_pending_cycle_requires_matching_submitted_turn_id() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        let activated = runtime
            .activate_pending_cycle("turn-other".to_string())
            .unwrap();
        assert!(!activated);
        assert_eq!(runtime.state(), Some(&state));
    }

    #[test]
    fn note_submission_dispatched_marks_pending_cycle_in_flight() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(runtime.note_submission_dispatched().unwrap());
        assert!(
            runtime
                .state()
                .and_then(|state| state.submission_dispatched_at)
                .is_some()
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_clears_stopped_in_flight_markers() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Stopped,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(PilotCycleKind::Continue),
            active_turn_id: Some("turn-pilot".to_string()),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(runtime.clear_orphaned_cycle_if_idle(Local::now()).unwrap());

        let cleared_state = runtime.state().expect("pilot state");
        assert_eq!(cleared_state.status, PilotStatus::Stopped);
        assert_eq!(cleared_state.pending_cycle_kind, None);
        assert_eq!(cleared_state.submission_dispatched_at, None);
        assert_eq!(cleared_state.active_cycle_kind, None);
        assert_eq!(cleared_state.active_turn_id, None);
        assert_eq!(cleared_state.last_submitted_turn_id, None);
        assert_eq!(
            cleared_state.status_message.as_deref(),
            Some("Pilot cleared stale cycle state after the thread became idle.")
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_preserves_running_state() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            active_cycle_kind: Some(PilotCycleKind::Continue),
            active_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(!runtime.clear_orphaned_cycle_if_idle(Local::now()).unwrap());
        assert_eq!(runtime.state(), Some(&state));
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_for_control_clears_paused_in_flight_markers() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Paused,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(PilotCycleKind::Continue),
            active_turn_id: Some("turn-pilot".to_string()),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(
            runtime
                .clear_orphaned_cycle_if_idle_for_control(Local::now())
                .unwrap()
        );

        let cleared_state = runtime.state().expect("pilot state");
        assert_eq!(cleared_state.status, PilotStatus::Paused);
        assert_eq!(cleared_state.pending_cycle_kind, None);
        assert_eq!(cleared_state.submission_dispatched_at, None);
        assert_eq!(cleared_state.active_cycle_kind, None);
        assert_eq!(cleared_state.active_turn_id, None);
        assert_eq!(cleared_state.last_submitted_turn_id, None);
        assert_eq!(
            cleared_state.status_message.as_deref(),
            Some("Pilot cleared stale cycle state after the thread became idle.")
        );
    }

    #[test]
    fn clear_orphaned_cycle_if_idle_for_control_clears_running_in_flight_markers() {
        let dir = tempdir().unwrap();
        let thread_id = "thread-1";
        let state = PilotRunState {
            status: PilotStatus::Running,
            pending_cycle_kind: Some(PilotCycleKind::Continue),
            submission_dispatched_at: Some(1),
            active_cycle_kind: Some(PilotCycleKind::Continue),
            active_turn_id: Some("turn-pilot".to_string()),
            last_submitted_turn_id: Some("turn-pilot".to_string()),
            ..PilotRunState::default()
        };
        save_thread_state(dir.path(), thread_id, Some(&state)).unwrap();
        let mut runtime = PilotRuntime::load(dir.path(), thread_id).unwrap();

        assert!(
            runtime
                .clear_orphaned_cycle_if_idle_for_control(Local::now())
                .unwrap()
        );

        let cleared_state = runtime.state().expect("pilot state");
        assert_eq!(cleared_state.status, PilotStatus::Running);
        assert_eq!(cleared_state.pending_cycle_kind, None);
        assert_eq!(cleared_state.submission_dispatched_at, None);
        assert_eq!(cleared_state.active_cycle_kind, None);
        assert_eq!(cleared_state.active_turn_id, None);
        assert_eq!(cleared_state.last_submitted_turn_id, None);
        assert_eq!(
            cleared_state.status_message.as_deref(),
            Some("Pilot cleared stale cycle state after the thread became idle.")
        );
    }
}
