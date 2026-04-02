use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::AutoresearchApproachStatus;
use super::AutoresearchApproachSummary;
use super::AutoresearchExperimentEntry;
use super::AutoresearchExperimentStatus;
use super::AutoresearchJournalSummary;
use super::EvaluationGovernanceSettings;
use super::MetricDirection;
use super::ValidationPolicySettings;
use super::memory::build_research_memory_summary;
use super::policy_config::SelectionPolicySettings;
use super::render_policy_issue_lines;
use super::render_validation_prompt_lines;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResearchCycleGuidance {
    pub recommended_approach_id: Option<String>,
    pub controller_selected_approach_id: Option<String>,
    pub materialized_synthesis_approach_id: Option<String>,
    pub should_switch_active: bool,
    pub selection_reasons: Vec<String>,
    pub synthesis_suggestion: Option<ResearchSynthesisSuggestion>,
    pub prompt_block: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResearchSynthesisSuggestion {
    pub left_approach_id: String,
    pub left_family: String,
    pub right_approach_id: String,
    pub right_family: String,
}

pub(crate) fn build_research_cycle_guidance(
    summary: &AutoresearchJournalSummary,
    goal: &str,
    active_approach_id: Option<&str>,
    consecutive_exploit_cycles: u32,
    selection_policy: &SelectionPolicySettings,
    validation_policy: &ValidationPolicySettings,
    governance: &EvaluationGovernanceSettings,
) -> ResearchCycleGuidance {
    let query_tokens = build_query_tokens(summary, goal, active_approach_id);
    let metric_bonus = metric_bonus_map(summary);
    let mut ranked = summary
        .current_segment_approaches
        .iter()
        .map(|approach| {
            let stagnant = approach_is_stagnant(
                summary,
                &approach.latest.approach_id,
                selection_policy.policy.stagnation_window,
            );
            let score = score_approach(
                approach,
                metric_bonus
                    .get(approach.latest.approach_id.as_str())
                    .copied(),
                active_approach_id,
                &query_tokens,
                stagnant,
            );
            RankedApproach {
                summary: approach,
                score,
                stagnant,
            }
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| {
                left.summary
                    .latest
                    .status
                    .sort_rank()
                    .cmp(&right.summary.latest.status.sort_rank())
            })
            .then_with(|| left.summary.latest.family.cmp(&right.summary.latest.family))
            .then_with(|| left.summary.latest.title.cmp(&right.summary.latest.title))
    });

    let recommended = ranked
        .iter()
        .find(|entry| is_preferred_status(entry.summary.latest.status))
        .or_else(|| {
            ranked
                .iter()
                .find(|entry| is_fallback_status(entry.summary.latest.status))
        });
    let recommended_approach_id = recommended.map(|entry| entry.summary.latest.approach_id.clone());
    let active_ranked = active_approach_id.and_then(|approach_id| {
        ranked
            .iter()
            .find(|entry| entry.summary.latest.approach_id == approach_id)
    });
    let active_invalid =
        active_ranked.is_some_and(|entry| !is_preferred_status(entry.summary.latest.status));
    let active_stagnant = active_ranked.is_some_and(|entry| entry.stagnant);
    let active_missing_from_portfolio = active_approach_id.is_some() && active_ranked.is_none();
    let active_assessment = ActiveApproachAssessment {
        invalid: active_invalid,
        stagnant: active_stagnant,
        weak: false,
        missing_from_portfolio: active_missing_from_portfolio,
    };
    let active_weak = match (active_ranked, recommended) {
        (Some(active_entry), Some(recommended_entry)) => {
            recommended_entry.summary.latest.approach_id != active_entry.summary.latest.approach_id
                && recommended_entry.score
                    >= active_entry.score + selection_policy.policy.weak_branch_score_gap
        }
        _ => false,
    };
    let active_assessment = ActiveApproachAssessment {
        weak: active_weak,
        ..active_assessment
    };
    let recommended_differs = match (active_approach_id, recommended) {
        (Some(active_id), Some(recommended_entry)) => {
            recommended_entry.summary.latest.approach_id != active_id
        }
        _ => false,
    };
    let should_switch_active = match (active_approach_id, recommended) {
        (None, Some(_)) => true,
        (Some(active_id), Some(recommended_entry)) => {
            recommended_entry.summary.latest.approach_id != active_id
                && (active_invalid
                    || active_stagnant
                    || active_weak
                    || active_missing_from_portfolio)
        }
        _ => false,
    };
    let selection_reasons = build_selection_reasons(
        active_approach_id,
        recommended,
        &active_assessment,
        recommended_differs,
        should_switch_active,
    );
    let synthesis_suggestion = synthesis_pair(
        &ranked,
        active_stagnant
            || input_exceeded_exploit_threshold(
                consecutive_exploit_cycles,
                selection_policy.policy.synthesis_after_exploit_cycles,
            ),
    )
    .map(|(left, right)| ResearchSynthesisSuggestion {
        left_approach_id: left.latest.approach_id.clone(),
        left_family: left.latest.family.clone(),
        right_approach_id: right.latest.approach_id.clone(),
        right_family: right.latest.family.clone(),
    });

    let prompt_block = render_prompt_block(RenderPromptInput {
        summary,
        recommended,
        active_approach_id,
        active_stagnant,
        active_weak,
        should_switch_active,
        query_tokens: &query_tokens,
        consecutive_exploit_cycles,
        synthesis_suggestion: synthesis_suggestion.as_ref(),
        selection_policy,
        validation_policy,
        governance,
    });

    ResearchCycleGuidance {
        recommended_approach_id: recommended_approach_id.clone(),
        controller_selected_approach_id: recommended_approach_id,
        materialized_synthesis_approach_id: None,
        should_switch_active,
        selection_reasons,
        synthesis_suggestion,
        prompt_block,
    }
}

struct RankedApproach<'a> {
    summary: &'a AutoresearchApproachSummary,
    score: i64,
    stagnant: bool,
}

struct ActiveApproachAssessment {
    invalid: bool,
    stagnant: bool,
    weak: bool,
    missing_from_portfolio: bool,
}

fn build_query_tokens(
    summary: &AutoresearchJournalSummary,
    goal: &str,
    active_approach_id: Option<&str>,
) -> BTreeSet<String> {
    let mut tokens = tokenize(goal);
    if let Some(active_approach_id) = active_approach_id
        && let Some(active) = summary.latest_approach(active_approach_id)
    {
        tokens.extend(tokenize(&active.latest.title));
        tokens.extend(tokenize(&active.latest.family));
        tokens.extend(tokenize(&active.latest.summary));
        tokens.extend(tokenize(&active.latest.rationale));
    }
    tokens
}

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn metric_bonus_map(summary: &AutoresearchJournalSummary) -> BTreeMap<&str, i64> {
    let Some(direction) = summary.config.as_ref().map(|config| config.direction) else {
        return BTreeMap::new();
    };
    let mut scored = summary
        .current_segment_approaches
        .iter()
        .filter_map(|approach| {
            approach
                .best_metric
                .map(|metric| (approach.latest.approach_id.as_str(), metric))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| match direction {
        MetricDirection::Lower => left.1.total_cmp(&right.1),
        MetricDirection::Higher => right.1.total_cmp(&left.1),
    });
    scored
        .into_iter()
        .enumerate()
        .map(|(index, (approach_id, _metric))| {
            let bonus = match index {
                0 => 18,
                1 => 12,
                2 => 6,
                _ => 0,
            };
            (approach_id, bonus)
        })
        .collect()
}

fn score_approach(
    approach: &AutoresearchApproachSummary,
    metric_bonus: Option<i64>,
    active_approach_id: Option<&str>,
    query_tokens: &BTreeSet<String>,
    stagnant: bool,
) -> i64 {
    let mut score = match approach.latest.status {
        AutoresearchApproachStatus::Winner => 80,
        AutoresearchApproachStatus::Active => 68,
        AutoresearchApproachStatus::Promising => 58,
        AutoresearchApproachStatus::Tested => 42,
        AutoresearchApproachStatus::Planned => 32,
        AutoresearchApproachStatus::Proposed => 24,
        AutoresearchApproachStatus::Archived => 8,
        AutoresearchApproachStatus::DeadEnd => -12,
    };
    score += metric_bonus.unwrap_or_default();
    score += i64::try_from(approach.keep_count)
        .unwrap_or(i64::MAX)
        .saturating_mul(8);
    score += i64::try_from(approach.total_runs.min(4))
        .unwrap_or(i64::MAX)
        .saturating_mul(2);
    score += i64::from(text_overlap_score(
        query_tokens,
        &[
            &approach.latest.title,
            &approach.latest.family,
            &approach.latest.summary,
            &approach.latest.rationale,
        ],
    )) * 3;
    if active_approach_id == Some(approach.latest.approach_id.as_str()) {
        score += 5;
    }
    if stagnant {
        score -= 14;
    }
    score
}

fn text_overlap_score(query_tokens: &BTreeSet<String>, fields: &[&str]) -> u8 {
    if query_tokens.is_empty() {
        return 0;
    }
    let mut seen = BTreeSet::new();
    for field in fields {
        seen.extend(tokenize(field));
    }
    u8::try_from(seen.intersection(query_tokens).count()).unwrap_or(u8::MAX)
}

fn is_preferred_status(status: AutoresearchApproachStatus) -> bool {
    matches!(
        status,
        AutoresearchApproachStatus::Active
            | AutoresearchApproachStatus::Promising
            | AutoresearchApproachStatus::Winner
    )
}

fn is_fallback_status(status: AutoresearchApproachStatus) -> bool {
    matches!(
        status,
        AutoresearchApproachStatus::Tested
            | AutoresearchApproachStatus::Planned
            | AutoresearchApproachStatus::Proposed
    )
}

fn build_selection_reasons(
    active_approach_id: Option<&str>,
    recommended: Option<&RankedApproach<'_>>,
    active_assessment: &ActiveApproachAssessment,
    recommended_differs: bool,
    should_switch_active: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    if active_approach_id.is_none() && recommended.is_some() {
        reasons.push("no active approach is selected yet".to_string());
    }
    if active_assessment.missing_from_portfolio {
        reasons.push("active approach is missing from the current portfolio summary".to_string());
    }
    if active_assessment.invalid {
        reasons.push("active approach is no longer in a preferred status".to_string());
    }
    if active_assessment.stagnant {
        reasons.push(
            "active approach looks stagnant after recent non-keeps or flat metrics".to_string(),
        );
    }
    if active_assessment.weak {
        reasons.push("active approach is materially weaker than the portfolio leader".to_string());
    }
    if recommended_differs && !should_switch_active {
        reasons.push(
            "current active approach remains within the configured weak-branch switch threshold"
                .to_string(),
        );
    } else if !recommended_differs
        && recommended.is_some()
        && active_approach_id.is_some()
        && reasons.is_empty()
    {
        reasons.push("current active approach remains the strongest ranked candidate".to_string());
    } else if recommended.is_none() && reasons.is_empty() {
        reasons
            .push("the portfolio does not yet contain a stronger recommended branch".to_string());
    }
    reasons
}

fn input_exceeded_exploit_threshold(
    consecutive_exploit_cycles: u32,
    synthesis_after_exploit_cycles: u32,
) -> bool {
    consecutive_exploit_cycles >= synthesis_after_exploit_cycles
}

fn approach_is_stagnant(
    summary: &AutoresearchJournalSummary,
    approach_id: &str,
    stagnation_window: usize,
) -> bool {
    let recent_runs = recent_runs_for_approach(summary, approach_id, stagnation_window);
    if recent_runs.len() < stagnation_window {
        return false;
    }
    if recent_runs
        .iter()
        .take(2)
        .all(|run| run.status != AutoresearchExperimentStatus::Keep)
    {
        return true;
    }
    let Some(direction) = summary.config.as_ref().map(|config| config.direction) else {
        return false;
    };
    let Some(latest_metric) = recent_runs.first().and_then(|run| run.metric) else {
        return false;
    };
    let previous_best = recent_runs
        .iter()
        .skip(1)
        .filter_map(|run| run.metric)
        .reduce(|left, right| match direction {
            MetricDirection::Lower => left.min(right),
            MetricDirection::Higher => left.max(right),
        });
    previous_best.is_some_and(|best| !metric_is_better(latest_metric, best, direction))
}

fn recent_runs_for_approach<'a>(
    summary: &'a AutoresearchJournalSummary,
    approach_id: &str,
    limit: usize,
) -> Vec<&'a AutoresearchExperimentEntry> {
    summary
        .current_segment_runs
        .iter()
        .rev()
        .filter(|run| run.approach_id.as_deref() == Some(approach_id))
        .take(limit)
        .collect()
}

fn metric_is_better(candidate: f64, baseline: f64, direction: MetricDirection) -> bool {
    match direction {
        MetricDirection::Lower => candidate < baseline,
        MetricDirection::Higher => candidate > baseline,
    }
}

struct RenderPromptInput<'a> {
    summary: &'a AutoresearchJournalSummary,
    recommended: Option<&'a RankedApproach<'a>>,
    active_approach_id: Option<&'a str>,
    active_stagnant: bool,
    active_weak: bool,
    should_switch_active: bool,
    query_tokens: &'a BTreeSet<String>,
    consecutive_exploit_cycles: u32,
    synthesis_suggestion: Option<&'a ResearchSynthesisSuggestion>,
    selection_policy: &'a SelectionPolicySettings,
    validation_policy: &'a ValidationPolicySettings,
    governance: &'a EvaluationGovernanceSettings,
}

fn render_prompt_block(input: RenderPromptInput<'_>) -> String {
    let mut lines = Vec::new();
    let policy = input.selection_policy.policy;

    if !input.selection_policy.issues.is_empty() {
        lines.extend(input.selection_policy.issues.iter().map(|issue| {
            format!(
                "- Selection policy warning: {issue}. Falling back to parsed/default values for this cycle."
            )
        }));
    }

    if input.selection_policy.has_custom_values {
        lines.push(format!(
            "- Configured selection policy: weak-branch score gap >= {}, stagnation window = {} runs, synthesis after {} exploit cycle(s).",
            policy.weak_branch_score_gap,
            policy.stagnation_window,
            policy.synthesis_after_exploit_cycles
        ));
    }

    if let Some(recommended) = input.recommended {
        let best_metric = recommended
            .summary
            .best_metric
            .map(|metric| format!(" best={}", super::runtime::format_metric(metric)))
            .unwrap_or_default();
        let rationale = format!(
            "status={} keeps={} runs={}{}",
            recommended.summary.latest.status.as_str(),
            recommended.summary.keep_count,
            recommended.summary.total_runs,
            best_metric
        );
        if input.should_switch_active {
            lines.push(format!(
                "- Recommended active approach: switch to `{}` [{}] because {rationale}.",
                recommended.summary.latest.approach_id, recommended.summary.latest.family
            ));
        } else {
            lines.push(format!(
                "- Recommended active approach: `{}` [{}] because {rationale}.",
                recommended.summary.latest.approach_id, recommended.summary.latest.family
            ));
        }
    } else {
        lines.push(
            "- Recommended active approach: none yet. Widen the portfolio before overfitting local tweaks."
                .to_string(),
        );
    }

    if let Some(active_approach_id) = input.active_approach_id
        && input.active_stagnant
    {
        lines.push(format!(
            "- Active branch health: `{active_approach_id}` looks stagnant after recent non-keeps or flat metrics."
        ));
    } else if let Some(active_approach_id) = input.active_approach_id
        && input.active_weak
    {
        lines.push(format!(
            "- Active branch health: `{active_approach_id}` is materially weaker than the current portfolio leader."
        ));
    }

    let memory_summary = build_research_memory_summary(
        input.summary,
        input.query_tokens,
        input.active_approach_id,
        input
            .recommended
            .map(|entry| entry.summary.latest.approach_id.as_str()),
    );
    lines.extend(memory_summary.lines);
    lines.extend(render_validation_prompt_lines(
        input.summary,
        input.active_approach_id,
        input
            .recommended
            .map(|entry| entry.summary.latest.approach_id.as_str()),
        input.validation_policy,
        input.governance,
    ));
    lines.extend(
        render_policy_issue_lines(input.validation_policy, input.governance)
            .into_iter()
            .map(|line| format!("- {line}")),
    );

    let enable_synthesis = input.active_stagnant
        || input_exceeded_exploit_threshold(
            input.consecutive_exploit_cycles,
            policy.synthesis_after_exploit_cycles,
        );
    if enable_synthesis && let Some(synthesis) = input.synthesis_suggestion {
        lines.push(format!(
            "- Synthesis opportunity: if the next checkpoint stalls, combine `{}` [{}] with `{}` [{}] into a fresh candidate instead of another minor variant.",
            synthesis.left_approach_id,
            synthesis.left_family,
            synthesis.right_approach_id,
            synthesis.right_family
        ));
    }

    if lines.is_empty() {
        String::new()
    } else {
        format!("Selection policy context:\n{}\n", lines.join("\n"))
    }
}

fn synthesis_pair<'a>(
    ranked: &'a [RankedApproach<'_>],
    enable: bool,
) -> Option<(
    &'a AutoresearchApproachSummary,
    &'a AutoresearchApproachSummary,
)> {
    if !enable {
        return None;
    }
    let left = ranked
        .iter()
        .find(|entry| is_preferred_status(entry.summary.latest.status))?;
    let right = ranked.iter().find(|entry| {
        is_preferred_status(entry.summary.latest.status)
            && entry.summary.latest.family != left.summary.latest.family
            && entry.summary.latest.approach_id != left.summary.latest.approach_id
    })?;
    Some((left.summary, right.summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
    use crate::slop_fork::autoresearch::AutoresearchConfigEntry;
    use crate::slop_fork::autoresearch::AutoresearchDiscoveryEntry;
    use crate::slop_fork::autoresearch::AutoresearchExperimentEntry;
    use crate::slop_fork::autoresearch::AutoresearchExperimentStatus;
    use crate::slop_fork::autoresearch::policy_config::SelectionPolicy;
    use crate::slop_fork::autoresearch::policy_config::SelectionPolicySettings;

    fn default_policy_settings() -> SelectionPolicySettings {
        SelectionPolicySettings::default()
    }

    fn default_validation_policy_settings() -> ValidationPolicySettings {
        ValidationPolicySettings::default()
    }

    fn default_governance_settings() -> EvaluationGovernanceSettings {
        EvaluationGovernanceSettings::default()
    }

    fn summary_with_research_memory() -> AutoresearchJournalSummary {
        AutoresearchJournalSummary {
            current_segment: 0,
            config: Some(AutoresearchConfigEntry {
                entry_type: "config".to_string(),
                name: "quality".to_string(),
                metric_name: "score".to_string(),
                metric_unit: String::new(),
                direction: MetricDirection::Higher,
            }),
            total_runs: 5,
            current_segment_approaches: vec![
                AutoresearchApproachSummary {
                    latest: AutoresearchApproachEntry {
                        entry_type: "approach".to_string(),
                        approach_id: "approach-1".to_string(),
                        title: "baseline stack".to_string(),
                        family: "baseline".to_string(),
                        status: AutoresearchApproachStatus::Active,
                        summary: "Simple stack with local tweaks".to_string(),
                        rationale: String::new(),
                        risks: Vec::new(),
                        sources: Vec::new(),
                        parent_approach_id: None,
                        synthesis_parent_approach_ids: Vec::new(),
                        timestamp: 1,
                        segment: 0,
                    },
                    total_runs: 3,
                    keep_count: 0,
                    best_metric: Some(0.42),
                    last_metric: Some(0.42),
                },
                AutoresearchApproachSummary {
                    latest: AutoresearchApproachEntry {
                        entry_type: "approach".to_string(),
                        approach_id: "approach-2".to_string(),
                        title: "retrieval reranker".to_string(),
                        family: "retrieval".to_string(),
                        status: AutoresearchApproachStatus::Promising,
                        summary: "Better candidate with stronger eval results".to_string(),
                        rationale: "Focus on retrieval failures".to_string(),
                        risks: Vec::new(),
                        sources: Vec::new(),
                        parent_approach_id: None,
                        synthesis_parent_approach_ids: Vec::new(),
                        timestamp: 2,
                        segment: 0,
                    },
                    total_runs: 2,
                    keep_count: 2,
                    best_metric: Some(0.71),
                    last_metric: Some(0.71),
                },
                AutoresearchApproachSummary {
                    latest: AutoresearchApproachEntry {
                        entry_type: "approach".to_string(),
                        approach_id: "approach-3".to_string(),
                        title: "parser rewrite".to_string(),
                        family: "parsing".to_string(),
                        status: AutoresearchApproachStatus::DeadEnd,
                        summary: "The parser rewrite kept breaking the evaluator".to_string(),
                        rationale: String::new(),
                        risks: Vec::new(),
                        sources: Vec::new(),
                        parent_approach_id: None,
                        synthesis_parent_approach_ids: Vec::new(),
                        timestamp: 3,
                        segment: 0,
                    },
                    total_runs: 1,
                    keep_count: 0,
                    best_metric: None,
                    last_metric: None,
                },
            ],
            current_segment_discoveries: vec![AutoresearchDiscoveryEntry {
                entry_type: "discovery".to_string(),
                reason: crate::slop_fork::autoresearch::AutoresearchDiscoveryReason::FollowUp,
                focus: Some("retrieval".to_string()),
                summary: "Compared retrieval families".to_string(),
                recommendations: vec!["compare the reranker with distilled retrieval".to_string()],
                unknowns: Vec::new(),
                sources: Vec::new(),
                dead_ends: Vec::new(),
                timestamp: 4,
                segment: 0,
            }],
            current_segment_controller_decisions: Vec::new(),
            current_segment_validations: Vec::new(),
            current_segment_runs: vec![
                AutoresearchExperimentEntry {
                    run: 1,
                    commit: "aaa".to_string(),
                    approach_id: Some("approach-1".to_string()),
                    metric: Some(0.40),
                    metrics: BTreeMap::new(),
                    status: AutoresearchExperimentStatus::Discard,
                    description: "baseline tweak regressed retrieval recall".to_string(),
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
                    description: "baseline stack broke checks after another recall tweak"
                        .to_string(),
                    timestamp: 11,
                    segment: 0,
                },
                AutoresearchExperimentEntry {
                    run: 3,
                    commit: "bb2".to_string(),
                    approach_id: Some("approach-1".to_string()),
                    metric: Some(0.42),
                    metrics: BTreeMap::new(),
                    status: AutoresearchExperimentStatus::Crash,
                    description: "baseline stack crashed after a third recall tweak".to_string(),
                    timestamp: 12,
                    segment: 0,
                },
                AutoresearchExperimentEntry {
                    run: 4,
                    commit: "ccc".to_string(),
                    approach_id: Some("approach-2".to_string()),
                    metric: Some(0.69),
                    metrics: BTreeMap::new(),
                    status: AutoresearchExperimentStatus::Keep,
                    description: "reranker baseline".to_string(),
                    timestamp: 13,
                    segment: 0,
                },
                AutoresearchExperimentEntry {
                    run: 5,
                    commit: "ddd".to_string(),
                    approach_id: Some("approach-2".to_string()),
                    metric: Some(0.71),
                    metrics: BTreeMap::new(),
                    status: AutoresearchExperimentStatus::Keep,
                    description: "reranker improvement".to_string(),
                    timestamp: 14,
                    segment: 0,
                },
            ],
        }
    }

    #[test]
    fn guidance_switches_away_from_stagnant_active_branch() {
        let summary = summary_with_research_memory();

        let guidance = build_research_cycle_guidance(
            &summary,
            "improve retrieval quality",
            Some("approach-1"),
            /*consecutive_exploit_cycles*/ 3,
            &default_policy_settings(),
            &default_validation_policy_settings(),
            &default_governance_settings(),
        );

        assert_eq!(
            guidance.recommended_approach_id.as_deref(),
            Some("approach-2")
        );
        assert!(guidance.should_switch_active);
        assert!(
            guidance
                .prompt_block
                .contains("switch to `approach-2` [retrieval]")
        );
        assert!(
            guidance
                .prompt_block
                .contains("`approach-1` looks stagnant")
        );
    }

    #[test]
    fn guidance_surfaces_memory_and_synthesis_signals() {
        let summary = summary_with_research_memory();

        let guidance = build_research_cycle_guidance(
            &summary,
            "improve retrieval quality",
            Some("approach-1"),
            /*consecutive_exploit_cycles*/ 3,
            &default_policy_settings(),
            &default_validation_policy_settings(),
            &default_governance_settings(),
        );

        assert!(
            guidance
                .prompt_block
                .contains("Family memory: `retrieval` is the strongest surviving family so far")
        );
        assert!(guidance.prompt_block.contains("already has 3 non-keeps"));
        assert!(guidance.prompt_block.contains("`recall`, `tweak`"));
        assert!(
            guidance
                .prompt_block
                .contains("Dead-end memory: `approach-3` [parsing] already dead-ended")
        );
        assert!(
            guidance
                .prompt_block
                .contains("Discovery memory: carry forward \"compare the reranker")
        );
        assert!(guidance.prompt_block.contains("Synthesis opportunity"));
    }

    #[test]
    fn guidance_switches_when_active_branch_is_clearly_weaker() {
        let mut summary = summary_with_research_memory();
        summary.current_segment_runs = vec![
            AutoresearchExperimentEntry {
                run: 1,
                commit: "aaa".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.58),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "baseline held steady".to_string(),
                timestamp: 10,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 2,
                commit: "bbb".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.69),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "reranker baseline".to_string(),
                timestamp: 11,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 3,
                commit: "ccc".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(0.71),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "reranker improvement".to_string(),
                timestamp: 12,
                segment: 0,
            },
        ];
        summary.current_segment_approaches[1].latest.status = AutoresearchApproachStatus::Winner;
        summary.current_segment_approaches[0].total_runs = 1;
        summary.current_segment_approaches[0].keep_count = 1;
        summary.current_segment_approaches[0].best_metric = Some(0.58);
        summary.current_segment_approaches[0].last_metric = Some(0.58);

        let guidance = build_research_cycle_guidance(
            &summary,
            "improve retrieval quality",
            Some("approach-1"),
            /*consecutive_exploit_cycles*/ 1,
            &default_policy_settings(),
            &default_validation_policy_settings(),
            &default_governance_settings(),
        );

        assert!(guidance.should_switch_active);
        assert!(guidance.prompt_block.contains("materially weaker"));
    }

    #[test]
    fn guidance_uses_selection_policy_thresholds_from_doc_settings() {
        let summary = summary_with_research_memory();
        let policy_settings = SelectionPolicySettings {
            policy: SelectionPolicy {
                weak_branch_score_gap: 50,
                stagnation_window: 4,
                synthesis_after_exploit_cycles: 5,
            },
            issues: Vec::new(),
            has_custom_values: true,
        };

        let guidance = build_research_cycle_guidance(
            &summary,
            "improve retrieval quality",
            Some("approach-1"),
            /*consecutive_exploit_cycles*/ 1,
            &policy_settings,
            &default_validation_policy_settings(),
            &default_governance_settings(),
        );

        assert_eq!(
            guidance.recommended_approach_id.as_deref(),
            Some("approach-1")
        );
        assert!(!guidance.should_switch_active);
        assert!(
            guidance
                .prompt_block
                .contains("Configured selection policy: weak-branch score gap >= 50")
        );
        assert!(
            !guidance
                .prompt_block
                .contains("`approach-1` looks stagnant")
        );
        assert!(!guidance.prompt_block.contains("Synthesis opportunity"));
    }
}
