use std::path::PathBuf;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SavedAccountActivateParams {
    pub account_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SavedAccountActivateResponse {
    pub activated: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SavedAccountRemoveParams {
    pub account_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SavedAccountRemoveResponse {
    pub removed: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
#[ts(rename_all = "snake_case", export_to = "v2/")]
pub enum AutomationScope {
    Session,
    Repo,
    Global,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum AutomationTrigger {
    TurnCompleted,
    Interval { every_seconds: u64 },
    Cron { expression: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum AutomationMessageSource {
    Static { message: String },
    RoundRobin { messages: Vec<String> },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationLimits {
    pub max_runs: Option<u32>,
    pub until_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationPolicyCommand {
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub timeout_ms: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationDefinition {
    pub id: Option<String>,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub message_source: AutomationMessageSource,
    pub limits: AutomationLimits,
    pub policy_command: Option<AutomationPolicyCommand>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct Automation {
    pub runtime_id: String,
    pub id: String,
    pub scope: AutomationScope,
    pub enabled: bool,
    pub paused: bool,
    pub stopped: bool,
    pub run_count: u32,
    pub next_fire_at: Option<i64>,
    pub last_error: Option<String>,
    pub trigger: AutomationTrigger,
    pub message_source: AutomationMessageSource,
    pub limits: AutomationLimits,
    pub policy_command: Option<AutomationPolicyCommand>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationListParams {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationListResponse {
    pub data: Vec<Automation>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationUpsertParams {
    pub thread_id: String,
    pub scope: AutomationScope,
    pub automation: AutomationDefinition,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationUpsertResponse {
    pub automation: Automation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationDeleteParams {
    pub thread_id: String,
    pub runtime_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationDeleteResponse {
    pub deleted: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationSetEnabledParams {
    pub thread_id: String,
    pub runtime_id: String,
    pub enabled: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationSetEnabledResponse {
    pub updated: bool,
    pub automation: Option<Automation>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AutomationUpdateType {
    Upserted,
    Deleted,
    Fired,
    Paused,
    Failed,
    Stopped,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutomationUpdatedNotification {
    pub thread_id: String,
    pub runtime_id: String,
    pub update_type: AutomationUpdateType,
    pub automation: Option<Automation>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum PilotStatus {
    Running,
    Paused,
    Stopped,
    Completed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum PilotCycleKind {
    Continue,
    WrapUp,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum PilotControlAction {
    Pause,
    Resume,
    WrapUp,
    Stop,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotRun {
    pub goal: String,
    pub status: PilotStatus,
    pub started_at: i64,
    pub deadline_at: Option<i64>,
    pub updated_at: i64,
    pub iteration_count: u32,
    pub pending_cycle_kind: Option<PilotCycleKind>,
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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotReadParams {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotReadResponse {
    pub run: Option<PilotRun>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotStartParams {
    pub thread_id: String,
    pub goal: String,
    #[ts(optional = nullable)]
    pub deadline_at: Option<i64>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotStartResponse {
    pub run: PilotRun,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotControlParams {
    pub thread_id: String,
    pub action: PilotControlAction,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotControlResponse {
    pub updated: bool,
    pub run: Option<PilotRun>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum PilotUpdateType {
    Started,
    Queued,
    CycleStarted,
    CycleCompleted,
    Paused,
    Resumed,
    WrapUpRequested,
    Stopped,
    Completed,
    Failed,
    Updated,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotUpdatedNotification {
    pub thread_id: String,
    pub update_type: PilotUpdateType,
    pub run: Option<PilotRun>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AutoresearchStatus {
    Running,
    Paused,
    Stopped,
    Completed,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AutoresearchCycleKind {
    Continue,
    Research,
    Discovery,
    WrapUp,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AutoresearchMode {
    Optimize,
    Research,
    Scientist,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchRun {
    pub goal: String,
    pub mode: AutoresearchMode,
    pub status: AutoresearchStatus,
    pub started_at: i64,
    pub updated_at: i64,
    pub max_runs: Option<u32>,
    pub iteration_count: u32,
    pub discovery_count: u32,
    pub pending_cycle_kind: Option<AutoresearchCycleKind>,
    pub active_cycle_kind: Option<AutoresearchCycleKind>,
    pub active_turn_id: Option<String>,
    pub last_submitted_turn_id: Option<String>,
    pub wrap_up_requested: bool,
    pub stop_requested_at: Option<i64>,
    pub last_error: Option<String>,
    pub status_message: Option<String>,
    pub last_progress_at: Option<i64>,
    pub last_cycle_completed_at: Option<i64>,
    pub last_discovery_completed_at: Option<i64>,
    pub last_cycle_summary: Option<String>,
    pub last_agent_message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchReadParams {
    pub thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchReadResponse {
    pub run: Option<AutoresearchRun>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchStartParams {
    pub thread_id: String,
    pub goal: String,
    pub mode: AutoresearchMode,
    #[ts(optional = nullable)]
    pub max_runs: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchStartResponse {
    pub updated: bool,
    pub run: Option<AutoresearchRun>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum AutoresearchControlAction {
    Pause,
    Resume,
    WrapUp,
    Stop,
    Clear,
    Discover,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchControlParams {
    pub thread_id: String,
    pub action: AutoresearchControlAction,
    #[ts(optional = nullable)]
    pub focus: Option<String>,
}

impl<'de> Deserialize<'de> for AutoresearchControlParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "camelCase")]
        enum AutoresearchControlActionCompat {
            Pause,
            Resume,
            WrapUp,
            Stop,
            Clear,
            Discover {
                #[serde(default)]
                focus: Option<String>,
            },
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct AutoresearchControlParamsWire {
            thread_id: String,
            action: AutoresearchControlActionCompat,
            #[serde(default)]
            focus: Option<String>,
        }

        let wire = AutoresearchControlParamsWire::deserialize(deserializer)?;
        let (action, legacy_focus) = match wire.action {
            AutoresearchControlActionCompat::Pause => (AutoresearchControlAction::Pause, None),
            AutoresearchControlActionCompat::Resume => (AutoresearchControlAction::Resume, None),
            AutoresearchControlActionCompat::WrapUp => (AutoresearchControlAction::WrapUp, None),
            AutoresearchControlActionCompat::Stop => (AutoresearchControlAction::Stop, None),
            AutoresearchControlActionCompat::Clear => (AutoresearchControlAction::Clear, None),
            AutoresearchControlActionCompat::Discover { focus } => {
                (AutoresearchControlAction::Discover, focus)
            }
        };

        if let (Some(top_level_focus), Some(action_focus)) = (&wire.focus, &legacy_focus)
            && top_level_focus != action_focus
        {
            return Err(serde::de::Error::custom(
                "discover focus was provided in both params.focus and action.focus with different values",
            ));
        }

        Ok(Self {
            thread_id: wire.thread_id,
            action,
            focus: wire.focus.or(legacy_focus),
        })
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchControlResponse {
    pub updated: bool,
    pub run: Option<AutoresearchRun>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum AutoresearchUpdateType {
    Started,
    Queued,
    CycleStarted,
    CycleCompleted,
    Paused,
    Resumed,
    WrapUpRequested,
    Stopped,
    Completed,
    Failed,
    Cleared,
    DiscoveryQueued,
    Updated,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct AutoresearchUpdatedNotification {
    pub thread_id: String,
    pub update_type: AutoresearchUpdateType,
    pub run: Option<AutoresearchRun>,
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum SlopForkAssistantTurnKind {
    Pilot,
    Autoresearch,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SlopForkAssistantTurnStartParams {
    pub thread_id: String,
    pub prompt: String,
    pub kind: SlopForkAssistantTurnKind,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct SlopForkAssistantTurnStartResponse {
    pub turn_id: String,
}
