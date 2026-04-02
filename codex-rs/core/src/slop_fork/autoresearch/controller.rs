use std::path::Path;

use serde::Deserialize;
use serde::Serialize;

use super::AutoresearchJournalSummary;
use super::AutoresearchRunState;
use super::policy_config::PortfolioRefreshPolicy;
use super::policy_config::PortfolioRefreshPolicySettings;
use super::policy_config::SelectionPolicySettings;
use super::policy_config::load_portfolio_refresh_policy_settings;
use super::policy_config::load_selection_policy_settings;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoresearchControllerSnapshot {
    pub selection_policy: SelectionPolicySettings,
    pub portfolio_refresh_policy: PortfolioRefreshPolicySettings,
    pub portfolio_refresh_status: PortfolioRefreshStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortfolioRefreshStatusKind {
    Ready,
    WaitingForExploitCycles,
    CoolingDown,
    BootstrapComplete,
    Suppressed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortfolioRefreshTriggerKind {
    Bootstrap,
    LowDiversity,
    Standard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioRefreshStatus {
    pub kind: PortfolioRefreshStatusKind,
    pub trigger: PortfolioRefreshTriggerKind,
    pub family_count: Option<usize>,
    pub required_family_count: usize,
    pub current_exploit_cycles: u32,
    pub required_exploit_cycles: u32,
    pub cooldown_seconds: u32,
    pub cooldown_remaining_seconds: Option<u32>,
}

impl PortfolioRefreshStatus {
    pub fn ready_now(&self) -> bool {
        self.kind == PortfolioRefreshStatusKind::Ready
    }
}

pub fn load_autoresearch_controller_snapshot(
    workdir: &Path,
    state: &AutoresearchRunState,
    summary: Option<&AutoresearchJournalSummary>,
    now_timestamp: i64,
) -> AutoresearchControllerSnapshot {
    let selection_policy = load_selection_policy_settings(workdir);
    let portfolio_refresh_policy = load_portfolio_refresh_policy_settings(workdir);
    let portfolio_refresh_status = evaluate_portfolio_refresh_status(
        state,
        summary,
        now_timestamp,
        portfolio_refresh_policy.policy,
    );
    AutoresearchControllerSnapshot {
        selection_policy,
        portfolio_refresh_policy,
        portfolio_refresh_status,
    }
}

pub fn evaluate_portfolio_refresh_status(
    state: &AutoresearchRunState,
    summary: Option<&AutoresearchJournalSummary>,
    now_timestamp: i64,
    policy: PortfolioRefreshPolicy,
) -> PortfolioRefreshStatus {
    if !state.mode.is_open_ended() || state.wrap_up_requested {
        return PortfolioRefreshStatus {
            kind: PortfolioRefreshStatusKind::Suppressed,
            trigger: PortfolioRefreshTriggerKind::Standard,
            family_count: summary.map(AutoresearchJournalSummary::family_count),
            required_family_count: policy.minimum_family_count,
            current_exploit_cycles: state.consecutive_exploit_cycles,
            required_exploit_cycles: policy.exploit_cycles,
            cooldown_seconds: policy.cooldown_seconds,
            cooldown_remaining_seconds: None,
        };
    }

    let Some(summary) = summary else {
        return bootstrap_status(state, policy, /*family_count*/ None);
    };
    if summary.current_segment_approaches.is_empty() {
        return bootstrap_status(state, policy, Some(0));
    }

    let family_count = summary.family_count();
    let (trigger, required_exploit_cycles) = if family_count < policy.minimum_family_count {
        (
            PortfolioRefreshTriggerKind::LowDiversity,
            policy.low_diversity_exploit_cycles,
        )
    } else {
        (PortfolioRefreshTriggerKind::Standard, policy.exploit_cycles)
    };

    let cooldown_remaining_seconds = state.last_discovery_completed_at.and_then(|completed_at| {
        let elapsed = now_timestamp.saturating_sub(completed_at);
        let remaining = i64::from(policy.cooldown_seconds).saturating_sub(elapsed);
        u32::try_from(remaining)
            .ok()
            .filter(|remaining| *remaining > 0)
    });
    if let Some(cooldown_remaining_seconds) = cooldown_remaining_seconds {
        return PortfolioRefreshStatus {
            kind: PortfolioRefreshStatusKind::CoolingDown,
            trigger,
            family_count: Some(family_count),
            required_family_count: policy.minimum_family_count,
            current_exploit_cycles: state.consecutive_exploit_cycles,
            required_exploit_cycles,
            cooldown_seconds: policy.cooldown_seconds,
            cooldown_remaining_seconds: Some(cooldown_remaining_seconds),
        };
    }

    exploit_cycle_status(
        trigger,
        family_count,
        state.consecutive_exploit_cycles,
        policy.minimum_family_count,
        required_exploit_cycles,
        policy.cooldown_seconds,
    )
}

fn bootstrap_status(
    state: &AutoresearchRunState,
    policy: PortfolioRefreshPolicy,
    family_count: Option<usize>,
) -> PortfolioRefreshStatus {
    PortfolioRefreshStatus {
        kind: if state.discovery_count == 0 {
            PortfolioRefreshStatusKind::Ready
        } else {
            PortfolioRefreshStatusKind::BootstrapComplete
        },
        trigger: PortfolioRefreshTriggerKind::Bootstrap,
        family_count,
        required_family_count: policy.minimum_family_count,
        current_exploit_cycles: state.consecutive_exploit_cycles,
        required_exploit_cycles: 0,
        cooldown_seconds: policy.cooldown_seconds,
        cooldown_remaining_seconds: None,
    }
}

fn exploit_cycle_status(
    trigger: PortfolioRefreshTriggerKind,
    family_count: usize,
    current_exploit_cycles: u32,
    required_family_count: usize,
    required_exploit_cycles: u32,
    cooldown_seconds: u32,
) -> PortfolioRefreshStatus {
    PortfolioRefreshStatus {
        kind: if current_exploit_cycles >= required_exploit_cycles {
            PortfolioRefreshStatusKind::Ready
        } else {
            PortfolioRefreshStatusKind::WaitingForExploitCycles
        },
        trigger,
        family_count: Some(family_count),
        required_family_count,
        current_exploit_cycles,
        required_exploit_cycles,
        cooldown_seconds,
        cooldown_remaining_seconds: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
    use crate::slop_fork::autoresearch::AutoresearchApproachStatus;
    use crate::slop_fork::autoresearch::AutoresearchJournal;
    use crate::slop_fork::autoresearch::AutoresearchMode;
    use crate::slop_fork::autoresearch::MetricDirection;
    use tempfile::tempdir;

    fn sample_state() -> AutoresearchRunState {
        AutoresearchRunState {
            mode: AutoresearchMode::Research,
            status: super::super::AutoresearchStatus::Running,
            consecutive_exploit_cycles: 3,
            discovery_count: 1,
            ..AutoresearchRunState::default()
        }
    }

    #[test]
    fn portfolio_refresh_waits_for_low_diversity_threshold() {
        let mut state = sample_state();
        state.consecutive_exploit_cycles = 2;
        let status = evaluate_portfolio_refresh_status(
            &state,
            Some(&summary_with_families(/*family_count*/ 2)),
            /*now_timestamp*/ 1_000,
            PortfolioRefreshPolicy {
                minimum_family_count: 3,
                low_diversity_exploit_cycles: 4,
                exploit_cycles: 5,
                cooldown_seconds: 300,
            },
        );

        assert_eq!(
            status.kind,
            PortfolioRefreshStatusKind::WaitingForExploitCycles
        );
        assert_eq!(status.trigger, PortfolioRefreshTriggerKind::LowDiversity);
        assert_eq!(status.required_exploit_cycles, 4);
    }

    #[test]
    fn portfolio_refresh_honors_standard_cooldown() {
        let mut state = sample_state();
        state.consecutive_exploit_cycles = 6;
        state.last_discovery_completed_at = Some(900);
        let status = evaluate_portfolio_refresh_status(
            &state,
            Some(&summary_with_families(/*family_count*/ 3)),
            /*now_timestamp*/ 1_000,
            PortfolioRefreshPolicy {
                minimum_family_count: 3,
                low_diversity_exploit_cycles: 2,
                exploit_cycles: 5,
                cooldown_seconds: 300,
            },
        );

        assert_eq!(status.kind, PortfolioRefreshStatusKind::CoolingDown);
        assert_eq!(status.trigger, PortfolioRefreshTriggerKind::Standard);
        assert_eq!(status.cooldown_remaining_seconds, Some(200));
    }

    #[test]
    fn portfolio_refresh_honors_low_diversity_cooldown() {
        let mut state = sample_state();
        state.consecutive_exploit_cycles = 6;
        state.last_discovery_completed_at = Some(900);
        let status = evaluate_portfolio_refresh_status(
            &state,
            Some(&summary_with_families(/*family_count*/ 2)),
            /*now_timestamp*/ 1_000,
            PortfolioRefreshPolicy {
                minimum_family_count: 3,
                low_diversity_exploit_cycles: 2,
                exploit_cycles: 5,
                cooldown_seconds: 300,
            },
        );

        assert_eq!(status.kind, PortfolioRefreshStatusKind::CoolingDown);
        assert_eq!(status.trigger, PortfolioRefreshTriggerKind::LowDiversity);
        assert_eq!(status.required_exploit_cycles, 2);
        assert_eq!(status.cooldown_remaining_seconds, Some(200));
    }

    fn summary_with_families(family_count: usize) -> AutoresearchJournalSummary {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
        journal
            .append_config(
                "quality".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        for index in 0..family_count {
            journal
                .append_approach(AutoresearchApproachEntry {
                    entry_type: "approach".to_string(),
                    approach_id: format!("approach-{}", index + 1),
                    title: format!("approach-{}", index + 1),
                    family: format!("family-{index}"),
                    status: AutoresearchApproachStatus::Promising,
                    summary: "candidate".to_string(),
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
        journal.summary()
    }
}
