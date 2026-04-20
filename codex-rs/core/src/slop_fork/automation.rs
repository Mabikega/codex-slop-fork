use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use chrono::DateTime;
use chrono::Datelike;
use chrono::Local;
use chrono::LocalResult;
use chrono::NaiveTime;
use chrono::TimeDelta;
use chrono::TimeZone;
use chrono::Timelike;
use chrono::Weekday;
use fd_lock::RwLock as FileRwLock;
use once_cell::sync::Lazy;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;

use crate::exec::ExecExpiration;
use crate::exec::ExecParams;
use crate::exec::process_exec_tool_call;
use crate::sandboxing::SandboxPermissions;
use crate::slop_fork::resolve_root_git_project_for_trust_local;
use codex_protocol::config_types::WindowsSandboxLevel;
use codex_protocol::permissions::FileSystemSandboxPolicy;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::SandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;

const GLOBAL_AUTOMATIONS_FILE: &str = "codex-slop-fork-automations.toml";
const AUTOMATION_STATE_FILE: &str = ".codex-slop-fork-automation-state.json";
const AUTOMATION_STATE_LOCK_FILE: &str = ".codex-slop-fork-automation-state.lock";
static PENDING_TURN_SUPPRESSIONS: Lazy<
    Mutex<HashMap<String, VecDeque<AutomationTurnSuppression>>>,
> = Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutomationTurnSuppression {
    pub suppress_legacy_notify: bool,
}

pub fn enqueue_automation_turn_suppression(
    thread_id: &str,
    suppression: AutomationTurnSuppression,
) {
    if suppression == AutomationTurnSuppression::default() {
        return;
    }
    let mut pending = match PENDING_TURN_SUPPRESSIONS.lock() {
        Ok(pending) => pending,
        Err(_) => panic!("automation turn suppression queue poisoned"),
    };
    pending
        .entry(thread_id.to_string())
        .or_default()
        .push_back(suppression);
}

pub fn discard_queued_automation_turn_suppression(thread_id: &str) {
    let mut pending = match PENDING_TURN_SUPPRESSIONS.lock() {
        Ok(pending) => pending,
        Err(_) => panic!("automation turn suppression queue poisoned"),
    };
    let should_remove = pending
        .get_mut(thread_id)
        .map(|queue| {
            queue.pop_back();
            queue.is_empty()
        })
        .unwrap_or(false);
    if should_remove {
        pending.remove(thread_id);
    }
}

pub fn take_automation_turn_suppression(thread_id: &str) -> AutomationTurnSuppression {
    let mut pending = match PENDING_TURN_SUPPRESSIONS.lock() {
        Ok(pending) => pending,
        Err(_) => panic!("automation turn suppression queue poisoned"),
    };
    let Some(queue) = pending.get_mut(thread_id) else {
        return AutomationTurnSuppression::default();
    };
    let suppression = queue.pop_front().unwrap_or_default();
    if queue.is_empty() {
        pending.remove(thread_id);
    }
    suppression
}

fn previous_fork_filename(parts: &[&str]) -> String {
    parts.join("-")
}

fn previous_path_for(path: &Path) -> Option<PathBuf> {
    let previous_name = match path.file_name()?.to_str()? {
        GLOBAL_AUTOMATIONS_FILE => {
            previous_fork_filename(&["codex", "alt", "fork", "automations.toml"])
        }
        AUTOMATION_STATE_FILE => {
            previous_fork_filename(&[".codex", "alt", "fork", "automation", "state.json"])
        }
        AUTOMATION_STATE_LOCK_FILE => {
            previous_fork_filename(&[".codex", "alt", "fork", "automation", "state.lock"])
        }
        _ => return None,
    };
    Some(path.with_file_name(previous_name))
}

fn read_with_previous_path_fallback(path: &Path) -> std::io::Result<Option<(PathBuf, String)>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => Ok(Some((path.to_path_buf(), contents))),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let Some(previous_path) = previous_path_for(path) else {
                return Ok(None);
            };
            match std::fs::read_to_string(&previous_path) {
                Ok(contents) => Ok(Some((previous_path, contents))),
                Err(previous_err) if previous_err.kind() == std::io::ErrorKind::NotFound => {
                    Ok(None)
                }
                Err(previous_err) => Err(previous_err),
            }
        }
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutomationScope {
    Session,
    Repo,
    Global,
}

impl AutomationScope {
    pub fn as_prefix(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Repo => "repo",
            Self::Global => "global",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationTrigger {
    TurnCompleted,
    Interval { every_seconds: u64 },
    Cron { expression: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AutomationMessageSource {
    Static { message: String },
    RoundRobin { messages: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationLimits {
    pub max_runs: Option<u32>,
    pub until_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationPolicyCommand {
    pub command: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutomationSpec {
    pub id: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub message_source: AutomationMessageSource,
    pub limits: AutomationLimits,
    pub policy_command: Option<AutomationPolicyCommand>,
}

impl Default for AutomationSpec {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: true,
            trigger: AutomationTrigger::TurnCompleted,
            message_source: AutomationMessageSource::Static {
                message: String::new(),
            },
            limits: AutomationLimits::default(),
            policy_command: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AutomationRuntimeState {
    pub run_count: u32,
    pub round_robin_index: usize,
    pub next_fire_at: Option<i64>,
    pub paused: bool,
    pub stopped: bool,
    pub last_error: Option<String>,
    pub state: Option<JsonValue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationEntry {
    pub runtime_id: String,
    pub scope: AutomationScope,
    pub spec: AutomationSpec,
    pub state: AutomationRuntimeState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationEvaluationTrigger {
    TurnCompleted {
        turn_id: Option<String>,
        last_agent_message: String,
    },
    Timer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutomationPreparedPolicy {
    pub runtime_id: String,
    pub scope: AutomationScope,
    pub command: AutomationPolicyCommand,
    pub payload: AutomationPolicyPayload,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutomationPolicyPayload {
    pub automation_id: String,
    pub scope: AutomationScope,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub run_count: u32,
    pub last_agent_message: String,
    pub default_message: String,
    pub state: Option<JsonValue>,
    pub now: DateTime<Local>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationPolicyExecutionContext {
    pub session_cwd: PathBuf,
    pub sandbox_policy: SandboxPolicy,
    pub file_system_sandbox_policy: FileSystemSandboxPolicy,
    pub network_sandbox_policy: NetworkSandboxPolicy,
    pub codex_linux_sandbox_exe: Option<PathBuf>,
    pub windows_sandbox_level: WindowsSandboxLevel,
    pub windows_sandbox_private_desktop: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AutomationPreparedAction {
    Send { runtime_id: String, message: String },
    RunPolicy(Box<AutomationPreparedPolicy>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum AutomationPolicyDecision {
    UseDefault {
        #[serde(default)]
        state: Option<JsonValue>,
    },
    Send {
        message: String,
        #[serde(default)]
        state: Option<JsonValue>,
    },
    Skip {
        #[serde(default)]
        state: Option<JsonValue>,
    },
    Stop {
        #[serde(default)]
        state: Option<JsonValue>,
    },
}

pub async fn run_policy_command(
    command: &AutomationPolicyCommand,
    payload: &AutomationPolicyPayload,
    default_timeout_ms: u64,
    execution: &AutomationPolicyExecutionContext,
) -> Result<AutomationPolicyDecision, String> {
    let timeout_ms = command.timeout_ms.unwrap_or(default_timeout_ms);
    let payload_json = serde_json::json!({
        "automation_id": payload.automation_id,
        "scope": payload.scope.as_prefix(),
        "thread_id": payload.thread_id,
        "turn_id": payload.turn_id,
        "run_count": payload.run_count,
        "last_agent_message": payload.last_agent_message,
        "default_message": payload.default_message,
        "state": payload.state,
        "now": payload.now.to_rfc3339(),
    })
    .to_string();
    let cwd = match command.cwd.as_ref() {
        Some(cwd) => AbsolutePathBuf::resolve_path_against_base(cwd, &execution.session_cwd),
        None => AbsolutePathBuf::try_from(execution.session_cwd.clone())
            .map_err(|err| format!("automation session cwd should be absolute: {err}"))?,
    };
    let session_cwd = AbsolutePathBuf::try_from(execution.session_cwd.clone())
        .map_err(|err| format!("automation session cwd should be absolute: {err}"))?;
    let joined_command = shlex::try_join(command.command.iter().map(String::as_str))
        .map_err(|err| format!("Failed to serialize automation policy command: {err}"))?;
    let wrapped_command = if cfg!(windows) {
        vec![
            "powershell".to_string(),
            "-NoLogo".to_string(),
            "-NoProfile".to_string(),
            "-Command".to_string(),
            format!("@'\n{payload_json}\n'@ | & {joined_command}"),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-lc".to_string(),
            format!(
                "cat <<'__CODEX_SLOP_FORK_AUTOMATION__' | {joined_command}\n{payload_json}\n__CODEX_SLOP_FORK_AUTOMATION__"
            ),
        ]
    };
    let output = process_exec_tool_call(
        ExecParams {
            command: wrapped_command,
            cwd,
            expiration: ExecExpiration::Timeout(Duration::from_millis(timeout_ms)),
            capture_policy: crate::exec::ExecCapturePolicy::ShellTool,
            env: HashMap::new(),
            network: None,
            sandbox_permissions: SandboxPermissions::UseDefault,
            windows_sandbox_level: execution.windows_sandbox_level,
            windows_sandbox_private_desktop: execution.windows_sandbox_private_desktop,
            justification: Some("Run /auto policy command".to_string()),
            arg0: None,
        },
        &execution.sandbox_policy,
        &execution.file_system_sandbox_policy,
        execution.network_sandbox_policy,
        &session_cwd,
        &execution.codex_linux_sandbox_exe,
        /*use_legacy_landlock*/ false,
        /*stdout_stream*/ None,
    )
    .await
    .map_err(|err| format!("failed to run automation policy command: {err}"))?;
    if output.timed_out {
        return Err(format!("policy command timed out after {timeout_ms}ms"));
    }
    if output.exit_code != 0 {
        let stderr = output.stderr.text.trim();
        let stdout = output.stdout.text.trim();
        let suffix = if !stderr.is_empty() {
            stderr.to_string()
        } else if !stdout.is_empty() {
            stdout.to_string()
        } else {
            format!("exit code {}", output.exit_code)
        };
        return Err(format!("policy command failed: {suffix}"));
    }
    parse_policy_decision(&output.stdout.text)
}

#[derive(Debug, Clone)]
pub struct AutomationRegistry {
    codex_home: PathBuf,
    cwd: PathBuf,
    thread_id: String,
    session_automations: Vec<AutomationSpec>,
    repo_automations: Vec<AutomationSpec>,
    global_automations: Vec<AutomationSpec>,
    runtime_states: HashMap<String, AutomationRuntimeState>,
    next_generated_id: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct AutomationFile {
    automations: Vec<AutomationSpec>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct AutomationStateFile {
    threads: BTreeMap<String, StoredThreadAutomationState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct StoredThreadAutomationState {
    session_automations: Vec<AutomationSpec>,
    runtime_states: BTreeMap<String, AutomationRuntimeState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredThreadAutomationStateCompat {
    Current(StoredThreadAutomationState),
    Legacy(BTreeMap<String, AutomationRuntimeState>),
}

impl Default for StoredThreadAutomationStateCompat {
    fn default() -> Self {
        Self::Current(StoredThreadAutomationState::default())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct AutomationStateFileCompat {
    threads: BTreeMap<String, StoredThreadAutomationStateCompat>,
}

impl AutomationRegistry {
    pub fn load(
        codex_home: &Path,
        cwd: &Path,
        thread_id: impl Into<String>,
    ) -> std::io::Result<Self> {
        let thread_id = thread_id.into();
        let repo_automations = match repo_automations_path(cwd) {
            Some(path) => load_automation_file(&path)?,
            None => Vec::new(),
        };
        let global_automations = load_automation_file(&global_automations_path(codex_home))?;
        let (session_automations, runtime_states) = load_thread_state(codex_home, &thread_id)?;
        let mut next_generated_id = 0_u64;
        for spec in session_automations
            .iter()
            .chain(repo_automations.iter())
            .chain(global_automations.iter())
        {
            if let Some(rest) = spec.id.strip_prefix("auto-")
                && let Ok(value) = rest.parse::<u64>()
            {
                next_generated_id = next_generated_id.max(value);
            }
        }

        Ok(Self {
            codex_home: codex_home.to_path_buf(),
            cwd: cwd.to_path_buf(),
            thread_id,
            session_automations,
            repo_automations,
            global_automations,
            runtime_states,
            next_generated_id,
        })
    }

    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    pub fn session_automations(&self) -> &[AutomationSpec] {
        &self.session_automations
    }

    pub fn set_session_automations(&mut self, automations: Vec<AutomationSpec>) {
        self.session_automations = automations;
        for spec in &self.session_automations {
            if let Some(rest) = spec.id.strip_prefix("auto-")
                && let Ok(value) = rest.parse::<u64>()
            {
                self.next_generated_id = self.next_generated_id.max(value);
            }
        }
        let session_runtime_ids = self
            .session_automations
            .iter()
            .map(|spec| runtime_id(AutomationScope::Session, &spec.id))
            .collect::<HashSet<_>>();
        self.runtime_states.retain(|runtime_id, _| {
            !matches!(
                split_runtime_id(runtime_id),
                Some((AutomationScope::Session, _))
            ) || session_runtime_ids.contains(runtime_id)
        });
    }

    pub fn list_entries(&self) -> Vec<AutomationEntry> {
        let mut entries = Vec::new();
        self.extend_entries_for_scope(
            &mut entries,
            AutomationScope::Session,
            &self.session_automations,
        );
        self.extend_entries_for_scope(&mut entries, AutomationScope::Repo, &self.repo_automations);
        self.extend_entries_for_scope(
            &mut entries,
            AutomationScope::Global,
            &self.global_automations,
        );
        entries
    }

    fn extend_entries_for_scope(
        &self,
        entries: &mut Vec<AutomationEntry>,
        scope: AutomationScope,
        specs: &[AutomationSpec],
    ) {
        entries.extend(specs.iter().cloned().map(|spec| {
            AutomationEntry {
                runtime_id: runtime_id(scope, &spec.id),
                state: self
                    .runtime_states
                    .get(&runtime_id(scope, &spec.id))
                    .cloned()
                    .unwrap_or_default(),
                scope,
                spec,
            }
        }));
    }

    pub fn upsert(
        &mut self,
        scope: AutomationScope,
        mut spec: AutomationSpec,
        now: DateTime<Local>,
    ) -> std::io::Result<AutomationEntry> {
        let previous_trigger = (!spec.id.trim().is_empty())
            .then(|| {
                self.find_spec(scope, &spec.id)
                    .map(|existing| existing.trigger.clone())
            })
            .flatten();
        if spec.id.trim().is_empty() {
            self.next_generated_id = self.next_generated_id.saturating_add(1);
            spec.id = format!("auto-{}", self.next_generated_id);
        }
        self.initialize_runtime_for_spec(scope, &spec, now);
        if previous_trigger
            .as_ref()
            .is_some_and(|trigger| trigger != &spec.trigger)
            && let Some(state) = self.runtime_states.get_mut(&runtime_id(scope, &spec.id))
        {
            state.next_fire_at = next_fire_at_for_trigger(&spec.trigger, now);
            state.stopped = false;
        }
        match scope {
            AutomationScope::Session => upsert_spec_in(&mut self.session_automations, spec.clone()),
            AutomationScope::Repo => {
                upsert_spec_in(&mut self.repo_automations, spec.clone());
                save_automation_file(
                    &repo_automations_path(&self.cwd).ok_or_else(|| {
                        std::io::Error::other("Repo-scoped automations require a git repository.")
                    })?,
                    &self.repo_automations,
                )?;
            }
            AutomationScope::Global => {
                upsert_spec_in(&mut self.global_automations, spec.clone());
                save_automation_file(
                    &global_automations_path(&self.codex_home),
                    &self.global_automations,
                )?;
            }
        }
        self.save_runtime_states()?;
        Ok(AutomationEntry {
            runtime_id: runtime_id(scope, &spec.id),
            scope,
            state: self
                .runtime_states
                .get(&runtime_id(scope, &spec.id))
                .cloned()
                .unwrap_or_default(),
            spec,
        })
    }

    pub fn remove(&mut self, runtime_id_to_remove: &str) -> std::io::Result<bool> {
        let Some((scope, spec_id)) = split_runtime_id(runtime_id_to_remove) else {
            return Ok(false);
        };
        let removed = match scope {
            AutomationScope::Session => remove_spec_from(&mut self.session_automations, spec_id),
            AutomationScope::Repo => {
                let removed = remove_spec_from(&mut self.repo_automations, spec_id);
                if removed {
                    save_automation_file(
                        &repo_automations_path(&self.cwd).ok_or_else(|| {
                            std::io::Error::other(
                                "Repo-scoped automations require a git repository.",
                            )
                        })?,
                        &self.repo_automations,
                    )?;
                }
                removed
            }
            AutomationScope::Global => {
                let removed = remove_spec_from(&mut self.global_automations, spec_id);
                if removed {
                    save_automation_file(
                        &global_automations_path(&self.codex_home),
                        &self.global_automations,
                    )?;
                }
                removed
            }
        };
        if removed {
            self.runtime_states.remove(runtime_id_to_remove);
            self.save_runtime_states()?;
        }
        Ok(removed)
    }

    pub fn set_enabled(
        &mut self,
        runtime_id_to_update: &str,
        enabled: bool,
    ) -> std::io::Result<bool> {
        let Some((scope, spec_id)) = split_runtime_id(runtime_id_to_update) else {
            return Ok(false);
        };
        let updated = match scope {
            AutomationScope::Session => {
                set_spec_enabled(&mut self.session_automations, spec_id, enabled)
            }
            AutomationScope::Repo => {
                let updated = set_spec_enabled(&mut self.repo_automations, spec_id, enabled);
                if updated {
                    save_automation_file(
                        &repo_automations_path(&self.cwd).ok_or_else(|| {
                            std::io::Error::other(
                                "Repo-scoped automations require a git repository.",
                            )
                        })?,
                        &self.repo_automations,
                    )?;
                }
                updated
            }
            AutomationScope::Global => {
                let updated = set_spec_enabled(&mut self.global_automations, spec_id, enabled);
                if updated {
                    save_automation_file(
                        &global_automations_path(&self.codex_home),
                        &self.global_automations,
                    )?;
                }
                updated
            }
        };
        if updated && enabled {
            let next_fire_at = split_runtime_id(runtime_id_to_update)
                .and_then(|(scope, spec_id)| self.find_spec(scope, spec_id))
                .and_then(|spec| next_fire_at_for_trigger(&spec.trigger, Local::now()));
            if let Some(state) = self.runtime_states.get_mut(runtime_id_to_update) {
                state.paused = false;
                state.stopped = false;
                if state.next_fire_at.is_none() {
                    state.next_fire_at = next_fire_at;
                }
            }
        }
        self.save_runtime_states()?;
        Ok(updated)
    }

    pub fn set_paused(
        &mut self,
        runtime_id_to_update: &str,
        paused: bool,
    ) -> std::io::Result<bool> {
        let Some(state) = self.runtime_states.get_mut(runtime_id_to_update) else {
            return Ok(false);
        };
        state.paused = paused;
        self.save_runtime_states()?;
        Ok(true)
    }

    pub fn prepare_actions(
        &mut self,
        trigger: AutomationEvaluationTrigger,
        now: DateTime<Local>,
    ) -> std::io::Result<Vec<AutomationPreparedAction>> {
        self.prepare_actions_internal(trigger, now, RuntimeActionSelection::All)
    }

    pub fn prepare_actions_excluding_runtime_ids(
        &mut self,
        trigger: AutomationEvaluationTrigger,
        now: DateTime<Local>,
        excluded_runtime_ids: &HashSet<String>,
    ) -> std::io::Result<Vec<AutomationPreparedAction>> {
        self.prepare_actions_internal(
            trigger,
            now,
            RuntimeActionSelection::AllExcept(excluded_runtime_ids),
        )
    }

    pub fn prepare_actions_for_runtime_ids(
        &mut self,
        runtime_ids: &HashSet<String>,
        now: DateTime<Local>,
    ) -> std::io::Result<Vec<AutomationPreparedAction>> {
        self.prepare_actions_internal(
            AutomationEvaluationTrigger::Timer,
            now,
            RuntimeActionSelection::Only(runtime_ids),
        )
    }

    fn prepare_actions_internal(
        &mut self,
        trigger: AutomationEvaluationTrigger,
        now: DateTime<Local>,
        selection: RuntimeActionSelection<'_>,
    ) -> std::io::Result<Vec<AutomationPreparedAction>> {
        let mut actions = Vec::new();
        for scope in [
            AutomationScope::Session,
            AutomationScope::Repo,
            AutomationScope::Global,
        ] {
            let specs = match scope {
                AutomationScope::Session => self.session_automations.clone(),
                AutomationScope::Repo => self.repo_automations.clone(),
                AutomationScope::Global => self.global_automations.clone(),
            };
            for spec in specs {
                if !spec.enabled {
                    continue;
                }
                let runtime_id = runtime_id(scope, &spec.id);
                let selected = match selection {
                    RuntimeActionSelection::All => true,
                    RuntimeActionSelection::Only(runtime_ids) => runtime_ids.contains(&runtime_id),
                    RuntimeActionSelection::AllExcept(runtime_ids) => {
                        !runtime_ids.contains(&runtime_id)
                    }
                };
                if !selected {
                    continue;
                }
                self.initialize_runtime_for_spec(scope, &spec, now);
                let Some(state) = self.runtime_states.get_mut(&runtime_id) else {
                    continue;
                };
                if state.paused || state.stopped {
                    continue;
                }
                if spec
                    .limits
                    .until_at
                    .is_some_and(|until_at| now.timestamp() > until_at)
                {
                    state.stopped = true;
                    continue;
                }
                if spec
                    .limits
                    .max_runs
                    .is_some_and(|max_runs| state.run_count >= max_runs)
                {
                    state.stopped = true;
                    continue;
                }
                let should_fire = match (&spec.trigger, &trigger) {
                    (
                        AutomationTrigger::Interval { .. } | AutomationTrigger::Cron { .. },
                        AutomationEvaluationTrigger::Timer,
                    ) if matches!(selection, RuntimeActionSelection::Only(_)) => true,
                    (
                        AutomationTrigger::TurnCompleted,
                        AutomationEvaluationTrigger::TurnCompleted { .. },
                    ) => true,
                    (AutomationTrigger::Interval { .. }, AutomationEvaluationTrigger::Timer)
                    | (AutomationTrigger::Cron { .. }, AutomationEvaluationTrigger::Timer) => state
                        .next_fire_at
                        .is_some_and(|next_fire_at| now.timestamp() >= next_fire_at),
                    _ => false,
                };
                if !should_fire {
                    continue;
                }

                let default_message = default_message_for_source(&spec.message_source, state);
                let (turn_id, last_agent_message) = match &trigger {
                    AutomationEvaluationTrigger::TurnCompleted {
                        turn_id,
                        last_agent_message,
                    } => (turn_id.clone(), last_agent_message.clone()),
                    AutomationEvaluationTrigger::Timer => (None, String::new()),
                };
                if let Some(policy_command) = spec.policy_command.clone() {
                    actions.push(AutomationPreparedAction::RunPolicy(Box::new(
                        AutomationPreparedPolicy {
                            runtime_id: runtime_id.clone(),
                            scope,
                            command: policy_command,
                            payload: AutomationPolicyPayload {
                                automation_id: spec.id.clone(),
                                scope,
                                thread_id: self.thread_id.clone(),
                                turn_id,
                                run_count: state.run_count,
                                last_agent_message,
                                default_message,
                                state: state.state.clone(),
                                now,
                            },
                        },
                    )));
                    continue;
                }

                actions.push(AutomationPreparedAction::Send {
                    runtime_id,
                    message: default_message,
                });
            }
        }
        self.save_runtime_states()?;
        Ok(actions)
    }

    pub fn apply_policy_decision(
        &mut self,
        runtime_id_to_update: &str,
        decision: AutomationPolicyDecision,
        now: DateTime<Local>,
    ) -> std::io::Result<Option<String>> {
        let Some((scope, spec_id)) = split_runtime_id(runtime_id_to_update) else {
            return Ok(None);
        };
        let Some(spec) = self.find_spec(scope, spec_id).cloned() else {
            return Ok(None);
        };
        let Some(state) = self.runtime_states.get(runtime_id_to_update) else {
            return Ok(None);
        };
        let default_message = default_message_for_source(&spec.message_source, state);
        match decision {
            AutomationPolicyDecision::UseDefault { state } => {
                self.apply_successful_action(runtime_id_to_update, &spec, now, state)?;
                self.save_runtime_states()?;
                Ok(Some(default_message))
            }
            AutomationPolicyDecision::Send { message, state } => {
                self.apply_successful_action(runtime_id_to_update, &spec, now, state)?;
                self.save_runtime_states()?;
                Ok(Some(message))
            }
            AutomationPolicyDecision::Skip { state } => {
                self.apply_skipped_action(runtime_id_to_update, &spec, now, state)?;
                self.save_runtime_states()?;
                Ok(None)
            }
            AutomationPolicyDecision::Stop { state } => {
                if let Some(runtime_state) = self.runtime_states.get_mut(runtime_id_to_update) {
                    runtime_state.state = state;
                    runtime_state.stopped = true;
                }
                self.save_runtime_states()?;
                Ok(None)
            }
        }
    }

    pub fn record_error(
        &mut self,
        runtime_id_to_update: &str,
        error: String,
    ) -> std::io::Result<()> {
        if let Some(state) = self.runtime_states.get_mut(runtime_id_to_update) {
            state.last_error = Some(error);
            state.paused = true;
        }
        self.save_runtime_states()
    }

    pub fn preview_policy_message(
        &self,
        runtime_id_to_update: &str,
        decision: &AutomationPolicyDecision,
    ) -> Option<String> {
        let (scope, spec_id) = split_runtime_id(runtime_id_to_update)?;
        let spec = self.find_spec(scope, spec_id)?;
        let state = self.runtime_states.get(runtime_id_to_update)?;
        match decision {
            AutomationPolicyDecision::UseDefault { .. } => {
                Some(default_message_for_source(&spec.message_source, state))
            }
            AutomationPolicyDecision::Send { message, .. } => Some(message.clone()),
            AutomationPolicyDecision::Skip { .. } | AutomationPolicyDecision::Stop { .. } => None,
        }
    }

    pub fn record_delivery(
        &mut self,
        runtime_id_to_update: &str,
        now: DateTime<Local>,
    ) -> std::io::Result<bool> {
        let Some((scope, spec_id)) = split_runtime_id(runtime_id_to_update) else {
            return Ok(false);
        };
        let Some(spec) = self.find_spec(scope, spec_id).cloned() else {
            return Ok(false);
        };
        self.initialize_runtime_for_spec(scope, &spec, now);
        self.apply_successful_action(runtime_id_to_update, &spec, now, /*state_update*/ None)?;
        self.save_runtime_states()?;
        Ok(true)
    }

    pub fn set_next_fire_at(
        &mut self,
        runtime_id_to_update: &str,
        next_fire_at: Option<i64>,
    ) -> std::io::Result<bool> {
        let Some(state) = self.runtime_states.get_mut(runtime_id_to_update) else {
            return Ok(false);
        };
        state.next_fire_at = next_fire_at;
        self.save_runtime_states()?;
        Ok(true)
    }

    pub fn next_wake_in(&mut self, now: DateTime<Local>) -> std::io::Result<Option<Duration>> {
        let mut next_duration: Option<Duration> = None;
        for scope in [
            AutomationScope::Session,
            AutomationScope::Repo,
            AutomationScope::Global,
        ] {
            let specs = match scope {
                AutomationScope::Session => self.session_automations.clone(),
                AutomationScope::Repo => self.repo_automations.clone(),
                AutomationScope::Global => self.global_automations.clone(),
            };
            for spec in &specs {
                self.initialize_runtime_for_spec(scope, spec, now);
                if !spec.enabled {
                    continue;
                }
                let runtime_id = runtime_id(scope, &spec.id);
                let Some(state) = self.runtime_states.get_mut(&runtime_id) else {
                    continue;
                };
                if state.paused || state.stopped {
                    continue;
                }
                if spec
                    .limits
                    .until_at
                    .is_some_and(|until_at| now.timestamp() > until_at)
                {
                    state.stopped = true;
                    continue;
                }
                if spec
                    .limits
                    .max_runs
                    .is_some_and(|max_runs| state.run_count >= max_runs)
                {
                    state.stopped = true;
                    continue;
                }
                if matches!(spec.trigger, AutomationTrigger::TurnCompleted) {
                    continue;
                }
                let Some(next_fire_at) = state.next_fire_at else {
                    continue;
                };
                let delta = next_fire_at.saturating_sub(now.timestamp());
                let duration = if delta <= 0 {
                    Duration::ZERO
                } else {
                    Duration::from_secs(delta as u64)
                };
                next_duration = Some(match next_duration {
                    Some(current) => current.min(duration),
                    None => duration,
                });
            }
        }
        self.save_runtime_states()?;
        Ok(next_duration)
    }

    fn initialize_runtime_for_spec(
        &mut self,
        scope: AutomationScope,
        spec: &AutomationSpec,
        now: DateTime<Local>,
    ) {
        let runtime_id = runtime_id(scope, &spec.id);
        let state = self.runtime_states.entry(runtime_id).or_default();
        if state.next_fire_at.is_none() {
            state.next_fire_at = next_fire_at_for_trigger(&spec.trigger, now);
        }
    }

    fn apply_successful_action(
        &mut self,
        runtime_id_to_update: &str,
        spec: &AutomationSpec,
        now: DateTime<Local>,
        state_update: Option<JsonValue>,
    ) -> std::io::Result<()> {
        if let Some(runtime_state) = self.runtime_states.get_mut(runtime_id_to_update) {
            runtime_state.run_count = runtime_state.run_count.saturating_add(1);
            runtime_state.round_robin_index = runtime_state.round_robin_index.saturating_add(1);
            runtime_state.last_error = None;
            runtime_state.state = state_update;
            runtime_state.next_fire_at = next_fire_at_for_trigger(&spec.trigger, now);
            if spec
                .limits
                .max_runs
                .is_some_and(|max_runs| runtime_state.run_count >= max_runs)
            {
                runtime_state.stopped = true;
            }
        }
        Ok(())
    }

    fn apply_skipped_action(
        &mut self,
        runtime_id_to_update: &str,
        spec: &AutomationSpec,
        now: DateTime<Local>,
        state_update: Option<JsonValue>,
    ) -> std::io::Result<()> {
        if let Some(runtime_state) = self.runtime_states.get_mut(runtime_id_to_update) {
            runtime_state.last_error = None;
            runtime_state.state = state_update;
            runtime_state.next_fire_at = next_fire_at_for_trigger(&spec.trigger, now);
        }
        Ok(())
    }

    fn find_spec(&self, scope: AutomationScope, spec_id: &str) -> Option<&AutomationSpec> {
        let specs = match scope {
            AutomationScope::Session => &self.session_automations,
            AutomationScope::Repo => &self.repo_automations,
            AutomationScope::Global => &self.global_automations,
        };
        specs.iter().find(|spec| spec.id == spec_id)
    }

    fn save_runtime_states(&self) -> std::io::Result<()> {
        save_runtime_states(
            &self.codex_home,
            &self.thread_id,
            &self.session_automations,
            &self.runtime_states,
        )
    }
}

pub fn parse_policy_decision(input: &str) -> Result<AutomationPolicyDecision, String> {
    serde_json::from_str(input.trim())
        .map_err(|err| format!("Invalid automation policy JSON: {err}"))
}

pub fn parse_until_for_today(now: DateTime<Local>, raw: &str) -> Result<DateTime<Local>, String> {
    let time = NaiveTime::parse_from_str(raw, "%H:%M")
        .map_err(|_| format!("Invalid --until time {raw:?}; expected HH:MM in local time."))?;
    let date = now.date_naive();
    let naive = date.and_time(time);
    let resolved = match Local.from_local_datetime(&naive) {
        LocalResult::Single(datetime) => datetime,
        LocalResult::Ambiguous(first, _) => first,
        LocalResult::None => {
            return Err(format!(
                "Local time {raw:?} does not exist today in your timezone."
            ));
        }
    };
    if resolved <= now {
        return Err(format!("Local time {raw:?} has already passed today."));
    }
    Ok(resolved)
}

pub fn global_automations_path(codex_home: &Path) -> PathBuf {
    codex_home.join(GLOBAL_AUTOMATIONS_FILE)
}

pub fn repo_automations_path(cwd: &Path) -> Option<PathBuf> {
    let repo_root = resolve_root_git_project_for_trust_local(cwd)?;
    Some(repo_root.join(".codex").join(GLOBAL_AUTOMATIONS_FILE))
}

pub fn automation_state_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTOMATION_STATE_FILE)
}

pub fn clear_thread_state(codex_home: &Path, thread_id: &str) -> std::io::Result<()> {
    let path = automation_state_path(codex_home);
    let lock_path = automation_state_lock_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _state_guard = state_lock.write().map_err(|err| {
        std::io::Error::other(format!("failed to lock {}: {err}", lock_path.display()))
    })?;
    let Some((loaded_path, contents)) = read_with_previous_path_fallback(&path)? else {
        return Ok(());
    };
    let mut file_contents = parse_automation_state_file(&loaded_path, &contents)?;
    file_contents.threads.remove(thread_id);
    write_automation_state_file(&path, &file_contents)
}

fn automation_state_lock_path(codex_home: &Path) -> PathBuf {
    codex_home.join(AUTOMATION_STATE_LOCK_FILE)
}

pub fn runtime_id(scope: AutomationScope, spec_id: &str) -> String {
    format!("{}:{spec_id}", scope.as_prefix())
}

pub fn split_runtime_id(runtime_id: &str) -> Option<(AutomationScope, &str)> {
    let (scope, spec_id) = runtime_id.split_once(':')?;
    let scope = match scope {
        "session" => AutomationScope::Session,
        "repo" => AutomationScope::Repo,
        "global" => AutomationScope::Global,
        _ => return None,
    };
    Some((scope, spec_id))
}

fn load_automation_file(path: &Path) -> std::io::Result<Vec<AutomationSpec>> {
    let Some((loaded_path, contents)) = read_with_previous_path_fallback(path)? else {
        return Ok(Vec::new());
    };
    toml::from_str::<AutomationFile>(&contents)
        .map(|file| file.automations)
        .map_err(|err| {
            std::io::Error::other(format!("failed to parse {}: {err}", loaded_path.display()))
        })
}

fn save_automation_file(path: &Path, automations: &[AutomationSpec]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let serialized = toml::to_string_pretty(&AutomationFile {
        automations: automations.to_vec(),
    })
    .map_err(|err| std::io::Error::other(format!("failed to serialize automation file: {err}")))?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(serialized.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn load_thread_state(
    codex_home: &Path,
    thread_id: &str,
) -> std::io::Result<(Vec<AutomationSpec>, HashMap<String, AutomationRuntimeState>)> {
    let path = automation_state_path(codex_home);
    let Some((loaded_path, contents)) = read_with_previous_path_fallback(&path)? else {
        return Ok((Vec::new(), HashMap::new()));
    };
    let file = parse_automation_state_file(&loaded_path, &contents)?;
    let stored_thread_state = file.threads.get(thread_id).cloned().unwrap_or_default();
    Ok((
        stored_thread_state.session_automations,
        stored_thread_state.runtime_states.into_iter().collect(),
    ))
}

fn save_runtime_states(
    codex_home: &Path,
    thread_id: &str,
    session_automations: &[AutomationSpec],
    runtime_states: &HashMap<String, AutomationRuntimeState>,
) -> std::io::Result<()> {
    let path = automation_state_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let lock_path = automation_state_lock_path(codex_home);
    let lock_file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)?;
    let mut state_lock = FileRwLock::new(lock_file);
    let _state_guard = state_lock.write().map_err(|err| {
        std::io::Error::other(format!("failed to lock {}: {err}", lock_path.display()))
    })?;
    let mut file_contents = match read_with_previous_path_fallback(&path)? {
        Some((loaded_path, contents)) => parse_automation_state_file(&loaded_path, &contents)?,
        None => AutomationStateFile::default(),
    };
    if session_automations.is_empty() && runtime_states.is_empty() {
        file_contents.threads.remove(thread_id);
    } else {
        file_contents.threads.insert(
            thread_id.to_string(),
            StoredThreadAutomationState {
                session_automations: session_automations.to_vec(),
                runtime_states: runtime_states.clone().into_iter().collect(),
            },
        );
    }
    write_automation_state_file(&path, &file_contents)
}

fn parse_automation_state_file(
    path: &Path,
    contents: &str,
) -> std::io::Result<AutomationStateFile> {
    let file = serde_json::from_str::<AutomationStateFileCompat>(contents).map_err(|err| {
        std::io::Error::other(format!("failed to parse {}: {err}", path.display()))
    })?;
    Ok(AutomationStateFile {
        threads: file
            .threads
            .into_iter()
            .map(|(thread_id, state)| {
                let state = match state {
                    StoredThreadAutomationStateCompat::Current(state) => state,
                    StoredThreadAutomationStateCompat::Legacy(runtime_states) => {
                        StoredThreadAutomationState {
                            session_automations: Vec::new(),
                            runtime_states,
                        }
                    }
                };
                (thread_id, state)
            })
            .collect(),
    })
}

fn write_automation_state_file(
    path: &Path,
    file_contents: &AutomationStateFile,
) -> std::io::Result<()> {
    let serialized = serde_json::to_string_pretty(file_contents).map_err(|err| {
        std::io::Error::other(format!("failed to serialize automation state: {err}"))
    })?;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::other(format!("failed to resolve parent for {}", path.display()))
    })?;
    let mut temp_file = tempfile::NamedTempFile::new_in(parent)?;
    temp_file.write_all(serialized.as_bytes())?;
    temp_file.flush()?;
    temp_file.persist(path).map_err(|err| {
        std::io::Error::other(format!("failed to replace {}: {err}", path.display()))
    })?;
    Ok(())
}

#[derive(Clone, Copy)]
enum RuntimeActionSelection<'a> {
    All,
    Only(&'a HashSet<String>),
    AllExcept(&'a HashSet<String>),
}

fn upsert_spec_in(specs: &mut Vec<AutomationSpec>, spec: AutomationSpec) {
    if let Some(existing) = specs.iter_mut().find(|existing| existing.id == spec.id) {
        *existing = spec;
    } else {
        specs.push(spec);
    }
}

fn remove_spec_from(specs: &mut Vec<AutomationSpec>, spec_id: &str) -> bool {
    let before = specs.len();
    specs.retain(|spec| spec.id != spec_id);
    specs.len() != before
}

fn set_spec_enabled(specs: &mut [AutomationSpec], spec_id: &str, enabled: bool) -> bool {
    let Some(spec) = specs.iter_mut().find(|spec| spec.id == spec_id) else {
        return false;
    };
    spec.enabled = enabled;
    true
}

fn default_message_for_source(
    source: &AutomationMessageSource,
    state: &AutomationRuntimeState,
) -> String {
    match source {
        AutomationMessageSource::Static { message } => message.clone(),
        AutomationMessageSource::RoundRobin { messages } => {
            if messages.is_empty() {
                String::new()
            } else {
                messages[state.round_robin_index % messages.len()].clone()
            }
        }
    }
}

fn next_fire_at_for_trigger(trigger: &AutomationTrigger, now: DateTime<Local>) -> Option<i64> {
    match trigger {
        AutomationTrigger::TurnCompleted => None,
        AutomationTrigger::Interval { every_seconds } => {
            let delta = TimeDelta::seconds((*every_seconds).min(i64::MAX as u64) as i64);
            Some((now + delta).timestamp())
        }
        AutomationTrigger::Cron { expression } => {
            let cron = CronExpression::parse(expression).ok()?;
            let expires_at = now + TimeDelta::days(3650);
            cron.next_after(now, expires_at)
                .map(|next_fire_at| next_fire_at.timestamp())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronExpression {
    expression: String,
    minutes: CronField,
    hours: CronField,
    days_of_month: CronField,
    months: CronField,
    days_of_week: CronField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronField {
    any: bool,
    min: u32,
    allowed: Vec<bool>,
}

impl CronExpression {
    fn parse(input: &str) -> Result<Self, String> {
        let trimmed = input.trim();
        let fields = trimmed.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(format!(
                "Cron schedule {trimmed:?} must have five fields: minute hour day-of-month month day-of-week."
            ));
        }

        Ok(Self {
            expression: trimmed.to_string(),
            minutes: CronField::parse(
                fields[0], /*min*/ 0, /*max*/ 59, "minute", /*day_of_week*/ false,
            )?,
            hours: CronField::parse(
                fields[1], /*min*/ 0, /*max*/ 23, "hour", /*day_of_week*/ false,
            )?,
            days_of_month: CronField::parse(
                fields[2],
                /*min*/ 1,
                /*max*/ 31,
                "day-of-month",
                /*day_of_week*/ false,
            )?,
            months: CronField::parse(
                fields[3], /*min*/ 1, /*max*/ 12, "month", /*day_of_week*/ false,
            )?,
            days_of_week: CronField::parse(
                fields[4],
                /*min*/ 0,
                /*max*/ 7,
                "day-of-week",
                /*day_of_week*/ true,
            )?,
        })
    }

    fn next_after(
        &self,
        after: DateTime<Local>,
        expires_at: DateTime<Local>,
    ) -> Option<DateTime<Local>> {
        let mut candidate = next_minute_boundary(after)?;
        while candidate <= expires_at {
            if self.matches(candidate) {
                return Some(candidate);
            }
            candidate += TimeDelta::minutes(1);
        }
        None
    }

    fn matches(&self, candidate: DateTime<Local>) -> bool {
        if !self.months.contains(candidate.month()) {
            return false;
        }
        if !self.hours.contains(candidate.hour()) {
            return false;
        }
        if !self.minutes.contains(candidate.minute()) {
            return false;
        }

        let day_of_month_matches = self.days_of_month.contains(candidate.day());
        let day_of_week_matches = self
            .days_of_week
            .contains(day_of_week_value(candidate.weekday()));
        match (self.days_of_month.any, self.days_of_week.any) {
            (true, true) => true,
            (true, false) => day_of_week_matches,
            (false, true) => day_of_month_matches,
            (false, false) => day_of_month_matches || day_of_week_matches,
        }
    }
}

impl CronField {
    fn parse(
        input: &str,
        min: u32,
        max: u32,
        field_name: &str,
        day_of_week: bool,
    ) -> Result<Self, String> {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Err(format!("Cron {field_name} field cannot be empty."));
        }

        let mut allowed = vec![false; (max.saturating_sub(min) + 1) as usize];
        if trimmed == "*" {
            allowed.fill(true);
            return Ok(Self {
                any: true,
                min,
                allowed,
            });
        }

        let mut any = false;
        for part in trimmed.split(',') {
            any |= Self::apply_part(part.trim(), min, max, field_name, day_of_week, &mut allowed)?;
        }

        if !allowed.iter().any(|value| *value) {
            return Err(format!(
                "Cron {field_name} field must select at least one value."
            ));
        }

        Ok(Self { any, min, allowed })
    }

    fn apply_part(
        part: &str,
        min: u32,
        max: u32,
        field_name: &str,
        day_of_week: bool,
        allowed: &mut [bool],
    ) -> Result<bool, String> {
        if part.is_empty() {
            return Err(format!(
                "Cron {field_name} field contains an empty list entry."
            ));
        }

        let (base, step) = match part.split_once('/') {
            Some((base, step)) => {
                let step = step.parse::<usize>().map_err(|_| {
                    format!(
                        "Cron {field_name} field has invalid step {step:?}; expected a positive integer."
                    )
                })?;
                if step == 0 {
                    return Err(format!(
                        "Cron {field_name} field step must be greater than zero."
                    ));
                }
                (base, step)
            }
            None => (part, 1),
        };
        let has_step = part.split_once('/').is_some();
        let is_unrestricted_wildcard = base == "*" && step == 1;

        let values = if base == "*" {
            if day_of_week {
                (0..=6).collect::<Vec<_>>()
            } else {
                (min..=max).collect::<Vec<_>>()
            }
        } else if let Some((start, end)) = base.split_once('-') {
            expand_range(
                parse_cron_value(start, min, max, field_name, day_of_week)?,
                parse_cron_value(end, min, max, field_name, day_of_week)?,
                min,
                max,
                field_name,
                day_of_week,
            )?
        } else {
            let start = parse_cron_value(base, min, max, field_name, day_of_week)?;
            if has_step {
                expand_range(start, max, min, max, field_name, day_of_week)?
            } else {
                vec![start]
            }
        };

        for value in values.into_iter().step_by(step) {
            let normalized = normalize_cron_value(value, day_of_week);
            let index = normalized.saturating_sub(min) as usize;
            if let Some(slot) = allowed.get_mut(index) {
                *slot = true;
            }
        }
        Ok(is_unrestricted_wildcard)
    }

    fn contains(&self, value: u32) -> bool {
        let index = value.saturating_sub(self.min) as usize;
        self.allowed.get(index).copied().unwrap_or(false)
    }
}

fn day_of_week_value(day: Weekday) -> u32 {
    day.num_days_from_sunday()
}

fn parse_cron_value(
    raw: &str,
    min: u32,
    max: u32,
    field_name: &str,
    day_of_week: bool,
) -> Result<u32, String> {
    let value = raw.parse::<u32>().map_err(|_| {
        format!("Cron {field_name} field has invalid value {raw:?}; expected a number.")
    })?;
    if day_of_week {
        if value > 7 {
            return Err(format!(
                "Cron {field_name} field value {value} is out of range {min}-{max}."
            ));
        }
        return Ok(value);
    }
    if value < min || value > max {
        return Err(format!(
            "Cron {field_name} field value {value} is out of range {min}-{max}."
        ));
    }
    Ok(value)
}

fn normalize_cron_value(value: u32, day_of_week: bool) -> u32 {
    if day_of_week && value == 7 { 0 } else { value }
}

fn expand_range(
    start: u32,
    end: u32,
    min: u32,
    max: u32,
    field_name: &str,
    day_of_week: bool,
) -> Result<Vec<u32>, String> {
    if day_of_week {
        if start == 7 && end == 7 {
            return Ok(vec![0]);
        }
        if start > end {
            return Err(format!(
                "Cron {field_name} field range {start}-{end} must be ascending."
            ));
        }
        if end == 7 {
            let mut values = (start..=6).collect::<Vec<_>>();
            values.push(0);
            return Ok(values);
        }
    }

    if start < min || end > max || start > end {
        return Err(format!(
            "Cron {field_name} field range {start}-{end} is invalid for {min}-{max}."
        ));
    }
    Ok((start..=end).collect())
}

fn next_minute_boundary(after: DateTime<Local>) -> Option<DateTime<Local>> {
    let candidate = after + TimeDelta::minutes(1);
    Some(
        candidate
            - TimeDelta::seconds(i64::from(candidate.second()))
            - TimeDelta::nanoseconds(i64::from(candidate.nanosecond())),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn parses_until_for_today_and_rejects_past_times() {
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let until = parse_until_for_today(now, "14:00").expect("until");
        assert_eq!(until.hour(), 14);
        assert_eq!(
            parse_until_for_today(now, "09:59").expect_err("past time"),
            "Local time \"09:59\" has already passed today."
        );
    }

    #[test]
    fn round_robin_and_max_runs_stop_after_limit() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.set_session_automations(vec![AutomationSpec {
            id: "auto-1".to_string(),
            enabled: true,
            trigger: AutomationTrigger::TurnCompleted,
            message_source: AutomationMessageSource::RoundRobin {
                messages: vec!["one".to_string(), "two".to_string()],
            },
            limits: AutomationLimits {
                max_runs: Some(2),
                until_at: None,
            },
            policy_command: None,
        }]);

        let first = registry.prepare_actions(
            AutomationEvaluationTrigger::TurnCompleted {
                turn_id: None,
                last_agent_message: "next step".to_string(),
            },
            now,
        )?;
        assert_eq!(
            first,
            vec![AutomationPreparedAction::Send {
                runtime_id: "session:auto-1".to_string(),
                message: "one".to_string(),
            }]
        );
        assert!(registry.record_delivery("session:auto-1", now)?);

        let second = registry.prepare_actions(
            AutomationEvaluationTrigger::TurnCompleted {
                turn_id: None,
                last_agent_message: "next step".to_string(),
            },
            now + TimeDelta::minutes(1),
        )?;
        assert_eq!(
            second,
            vec![AutomationPreparedAction::Send {
                runtime_id: "session:auto-1".to_string(),
                message: "two".to_string(),
            }]
        );
        assert!(registry.record_delivery("session:auto-1", now + TimeDelta::minutes(1))?);

        assert_eq!(
            registry.prepare_actions(
                AutomationEvaluationTrigger::TurnCompleted {
                    turn_id: None,
                    last_agent_message: "next step".to_string(),
                },
                now + TimeDelta::minutes(2),
            )?,
            Vec::new()
        );
        Ok(())
    }

    #[test]
    fn persists_global_automations_and_runtime_state() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.upsert(
            AutomationScope::Global,
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: true,
                trigger: AutomationTrigger::TurnCompleted,
                message_source: AutomationMessageSource::Static {
                    message: "continue working on this".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            now,
        )?;
        registry.prepare_actions(
            AutomationEvaluationTrigger::TurnCompleted {
                turn_id: None,
                last_agent_message: "next step".to_string(),
            },
            now,
        )?;
        assert!(registry.record_delivery("global:auto-1", now)?);

        let reloaded = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        let entries = reloaded.list_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].state.run_count, 1);
        Ok(())
    }

    #[test]
    fn prepare_actions_does_not_advance_runtime_state_before_delivery() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.set_session_automations(vec![AutomationSpec {
            id: "auto-1".to_string(),
            enabled: true,
            trigger: AutomationTrigger::TurnCompleted,
            message_source: AutomationMessageSource::RoundRobin {
                messages: vec!["one".to_string(), "two".to_string()],
            },
            limits: AutomationLimits::default(),
            policy_command: None,
        }]);

        let first = registry.prepare_actions(
            AutomationEvaluationTrigger::TurnCompleted {
                turn_id: None,
                last_agent_message: "next step".to_string(),
            },
            now,
        )?;
        let second = registry.prepare_actions(
            AutomationEvaluationTrigger::TurnCompleted {
                turn_id: None,
                last_agent_message: "next step".to_string(),
            },
            now,
        )?;

        assert_eq!(first, second);
        assert_eq!(
            registry
                .list_entries()
                .into_iter()
                .find(|entry| entry.runtime_id == "session:auto-1")
                .expect("entry")
                .state
                .run_count,
            0
        );
        Ok(())
    }

    #[test]
    fn record_delivery_advances_round_robin_and_run_count() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.set_session_automations(vec![AutomationSpec {
            id: "auto-1".to_string(),
            enabled: true,
            trigger: AutomationTrigger::TurnCompleted,
            message_source: AutomationMessageSource::RoundRobin {
                messages: vec!["one".to_string(), "two".to_string()],
            },
            limits: AutomationLimits::default(),
            policy_command: None,
        }]);

        assert!(registry.record_delivery("session:auto-1", now)?);
        let next = registry.prepare_actions(
            AutomationEvaluationTrigger::TurnCompleted {
                turn_id: None,
                last_agent_message: "next step".to_string(),
            },
            now,
        )?;

        assert_eq!(
            next,
            vec![AutomationPreparedAction::Send {
                runtime_id: "session:auto-1".to_string(),
                message: "two".to_string(),
            }]
        );
        assert_eq!(
            registry
                .list_entries()
                .into_iter()
                .find(|entry| entry.runtime_id == "session:auto-1")
                .expect("entry")
                .state
                .run_count,
            1
        );
        Ok(())
    }

    #[test]
    fn session_generated_ids_advance_past_existing_session_automations() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.upsert(
            AutomationScope::Session,
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: true,
                trigger: AutomationTrigger::TurnCompleted,
                message_source: AutomationMessageSource::Static {
                    message: "continue".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            now,
        )?;
        let mut reloaded = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;

        let entry = reloaded.upsert(
            AutomationScope::Session,
            AutomationSpec {
                id: String::new(),
                enabled: true,
                trigger: AutomationTrigger::TurnCompleted,
                message_source: AutomationMessageSource::Static {
                    message: "next".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            now,
        )?;

        assert_eq!(entry.runtime_id, "session:auto-2");
        Ok(())
    }

    #[test]
    fn clear_thread_state_removes_persisted_session_automations() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.upsert(
            AutomationScope::Session,
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: true,
                trigger: AutomationTrigger::Interval { every_seconds: 60 },
                message_source: AutomationMessageSource::Static {
                    message: "continue".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            now,
        )?;

        clear_thread_state(dir.path(), "thread-1")?;

        let reloaded = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        assert_eq!(reloaded.session_automations(), &[]);
        assert_eq!(reloaded.list_entries(), Vec::<AutomationEntry>::new());
        Ok(())
    }

    #[test]
    fn next_wake_ignores_disabled_and_expired_automations() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.set_session_automations(vec![
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: false,
                trigger: AutomationTrigger::Interval { every_seconds: 60 },
                message_source: AutomationMessageSource::Static {
                    message: "disabled".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            AutomationSpec {
                id: "auto-2".to_string(),
                enabled: true,
                trigger: AutomationTrigger::Interval { every_seconds: 120 },
                message_source: AutomationMessageSource::Static {
                    message: "expired".to_string(),
                },
                limits: AutomationLimits {
                    max_runs: None,
                    until_at: Some(now.timestamp() - 1),
                },
                policy_command: None,
            },
            AutomationSpec {
                id: "auto-3".to_string(),
                enabled: true,
                trigger: AutomationTrigger::Interval { every_seconds: 300 },
                message_source: AutomationMessageSource::Static {
                    message: "active".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
        ]);

        assert_eq!(registry.next_wake_in(now)?, Some(Duration::from_secs(300)));
        assert!(
            registry
                .list_entries()
                .into_iter()
                .find(|entry| entry.runtime_id == "session:auto-2")
                .is_some_and(|entry| entry.state.stopped)
        );
        Ok(())
    }

    #[test]
    fn upsert_recomputes_next_fire_when_trigger_changes() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        registry.upsert(
            AutomationScope::Global,
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: true,
                trigger: AutomationTrigger::Interval { every_seconds: 600 },
                message_source: AutomationMessageSource::Static {
                    message: "long".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            now,
        )?;

        let later = now + TimeDelta::seconds(30);
        let entry = registry.upsert(
            AutomationScope::Global,
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: true,
                trigger: AutomationTrigger::Interval { every_seconds: 60 },
                message_source: AutomationMessageSource::Static {
                    message: "short".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            later,
        )?;

        assert_eq!(
            entry.state.next_fire_at,
            Some((later + TimeDelta::seconds(60)).timestamp())
        );
        Ok(())
    }

    #[test]
    fn enabling_clears_paused_runtime_state() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 0, 0)
            .single()
            .unwrap();
        let mut registry = AutomationRegistry::load(dir.path(), dir.path(), "thread-1")?;
        let entry = registry.upsert(
            AutomationScope::Session,
            AutomationSpec {
                id: "auto-1".to_string(),
                enabled: true,
                trigger: AutomationTrigger::TurnCompleted,
                message_source: AutomationMessageSource::Static {
                    message: "continue".to_string(),
                },
                limits: AutomationLimits::default(),
                policy_command: None,
            },
            now,
        )?;
        registry.record_error(&entry.runtime_id, "boom".to_string())?;

        assert!(registry.set_enabled(&entry.runtime_id, /*enabled*/ true)?);
        assert!(
            registry
                .list_entries()
                .into_iter()
                .find(|listed| listed.runtime_id == entry.runtime_id)
                .is_some_and(|listed| !listed.state.paused)
        );
        Ok(())
    }

    #[test]
    fn cron_trigger_computes_next_fire() {
        let now = Local
            .with_ymd_and_hms(2026, 3, 11, 10, 1, 1)
            .single()
            .unwrap();
        let next_fire = next_fire_at_for_trigger(
            &AutomationTrigger::Cron {
                expression: "*/15 * * * *".to_string(),
            },
            now,
        )
        .expect("next fire");
        let next_fire = Local.timestamp_opt(next_fire, 0).single().unwrap();
        assert_eq!(next_fire.minute(), 15);
    }

    #[test]
    fn parses_policy_decision_json() {
        assert_eq!(
            parse_policy_decision("{\"action\":\"send\",\"message\":\"continue\"}")
                .expect("decision"),
            AutomationPolicyDecision::Send {
                message: "continue".to_string(),
                state: None,
            }
        );
    }
}
