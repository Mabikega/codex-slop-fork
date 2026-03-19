use chrono::DateTime;
use chrono::Local;
use serde::Deserialize;
use serde::Serialize;

use super::AUTORESEARCH_CHECKS_FILE;
use super::AUTORESEARCH_DOC_FILE;
use super::AUTORESEARCH_IDEAS_FILE;
use super::AUTORESEARCH_JOURNAL_FILE;
use super::AUTORESEARCH_SCRIPT_FILE;
use super::AutoresearchStageProgress;
use super::runtime::AutoresearchRunState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchDiscoveryReason {
    Plateau,
    StageComplete,
    WeakAssumption,
    ArchitectureSearch,
    EvaluationGap,
    FollowUp,
    UserRequested,
}

impl AutoresearchDiscoveryReason {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "plateau" => Some(Self::Plateau),
            "stage_complete" => Some(Self::StageComplete),
            "weak_assumption" => Some(Self::WeakAssumption),
            "architecture_search" => Some(Self::ArchitectureSearch),
            "evaluation_gap" => Some(Self::EvaluationGap),
            "follow_up" => Some(Self::FollowUp),
            "user_requested" => Some(Self::UserRequested),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plateau => "plateau",
            Self::StageComplete => "stage_complete",
            Self::WeakAssumption => "weak_assumption",
            Self::ArchitectureSearch => "architecture_search",
            Self::EvaluationGap => "evaluation_gap",
            Self::FollowUp => "follow_up",
            Self::UserRequested => "user_requested",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Plateau => "plateau escape",
            Self::StageComplete => "post-milestone discovery",
            Self::WeakAssumption => "assumption audit",
            Self::ArchitectureSearch => "architecture search",
            Self::EvaluationGap => "evaluation gap audit",
            Self::FollowUp => "follow-up research",
            Self::UserRequested => "user-requested discovery",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoresearchDiscoveryRequest {
    pub reason: AutoresearchDiscoveryReason,
    #[serde(default)]
    pub focus: Option<String>,
    pub requested_at: i64,
}

impl AutoresearchDiscoveryRequest {
    pub fn display_reason(&self) -> String {
        match self
            .focus
            .as_deref()
            .map(str::trim)
            .filter(|focus| !focus.is_empty())
        {
            Some(focus) => format!("{} ({focus})", self.reason.label()),
            None => self.reason.label().to_string(),
        }
    }
}

pub fn build_discovery_prompt(
    state: &AutoresearchRunState,
    request: &AutoresearchDiscoveryRequest,
    now: DateTime<Local>,
    stage_progress: Option<&AutoresearchStageProgress>,
) -> String {
    let focus_line = request
        .focus
        .as_deref()
        .map(str::trim)
        .filter(|focus| !focus.is_empty())
        .map(|focus| format!("- Focus: {focus}\n"))
        .unwrap_or_default();
    let stage_context = stage_progress
        .map(|progress| {
            if progress.has_issues() {
                format!(
                    "- Staged targets currently need repair: {}.\n",
                    progress.issue_summary()
                )
            } else if let Some(active_stage) = progress.active_stage() {
                format!(
                    "- Current staged target: {}/{} {}.\n",
                    progress.active_stage_number().unwrap_or(1),
                    progress.total_stages(),
                    active_stage.display
                )
            } else if progress.all_reached() {
                format!(
                    "- All {} staged targets are currently satisfied.\n",
                    progress.total_stages()
                )
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
    let max_runs_line = state
        .max_runs
        .map(|max_runs| format!("- max_runs: {max_runs}\n"))
        .unwrap_or_else(|| "- max_runs: none\n".to_string());
    format!(
        "Autoresearch directive: run one bounded discovery pass separate from the benchmark loop.\n\
         This instruction comes from the autoresearch controller, not from a user message.\n\n\
         Goal:\n\
         {goal}\n\n\
         Discovery trigger:\n\
         - Reason: {reason}\n\
         {focus_line}\
         Working files:\n\
         - {doc_file}\n\
         - {script_file}\n\
         - {checks_file} (optional)\n\
         - {ideas_file} (optional)\n\
         - {journal_file}\n\n\
         Bounded discovery workflow:\n\
         - Audit the current local repo state first: code, scripts, tests, docs, journal history, and artifacts.\n\
         - Use targeted online research only where it could materially change the next experiments or the current problem framing.\n\
         - Prefer primary sources for technical claims. Codebases, configs, artifacts, and benchmarks are also valid evidence when relevant.\n\
         - Use parallel sub-agents only for distinct, bounded questions; do not duplicate the same research thread.\n\
         - Challenge your first-pass conclusions before settling on recommendations.\n\
         - Update {ideas_file} with prioritized next experiments, hidden constraints, dead ends, and promising radical alternatives when useful.\n\
         - Call `autoresearch_log_discovery` exactly once near the end with the discovery summary, recommendations, open unknowns, sources, and dead ends.\n\
         - Do not call `autoresearch_run` or `autoresearch_log` in this discovery cycle. The benchmark loop remains separate.\n\
         - Stop after a bounded synthesis checkpoint. Do not start a new autonomous experiment cycle from inside this turn.\n\n\
         Current run context:\n\
         - iteration: {iteration}\n\
         - discovery_passes_completed: {discovery_count}\n\
         - started_at: {started_at}\n\
         - now: {now}\n\
         {max_runs_line}\
         {stage_context}\
         Finish with a concise synthesis of what changed in the project understanding and which experiments should run next.",
        goal = state.goal,
        reason = request.display_reason(),
        focus_line = focus_line,
        doc_file = AUTORESEARCH_DOC_FILE,
        script_file = AUTORESEARCH_SCRIPT_FILE,
        checks_file = AUTORESEARCH_CHECKS_FILE,
        ideas_file = AUTORESEARCH_IDEAS_FILE,
        journal_file = AUTORESEARCH_JOURNAL_FILE,
        iteration = state.iteration_count.saturating_add(1),
        discovery_count = state.discovery_count,
        started_at = chrono::TimeZone::timestamp_opt(&Local, state.started_at, 0)
            .single()
            .map(|ts| ts.to_rfc3339())
            .unwrap_or_else(|| state.started_at.to_string()),
        now = now.to_rfc3339(),
        max_runs_line = max_runs_line,
        stage_context = stage_context,
    )
}
