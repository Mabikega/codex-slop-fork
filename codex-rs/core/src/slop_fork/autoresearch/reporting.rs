use std::collections::BTreeSet;
use std::path::Path;

use super::AUTORESEARCH_PLAYBOOK_FILE;
use super::AUTORESEARCH_REPORT_FILE;
use super::AutoresearchApproachStatus;
use super::AutoresearchApproachSummary;
use super::AutoresearchJournalSummary;
use super::AutoresearchMode;
use super::memory::build_research_memory_summary;
use super::validation::load_evaluation_governance_settings;
use super::validation::load_validation_policy_settings;
use super::validation::render_governance_line;
use super::validation::render_validation_gate_line;
use super::validation::render_validation_policy_line;
use super::validation::validation_gate_for_status;

pub(crate) fn refresh_playbook_artifact(
    workdir: &Path,
    goal: &str,
    mode: AutoresearchMode,
    summary: &AutoresearchJournalSummary,
    active_approach_id: Option<&str>,
) -> std::io::Result<()> {
    let validation = load_validation_policy_settings(workdir);
    let governance = load_evaluation_governance_settings(workdir);
    let best_approach = preferred_approach(summary, active_approach_id);
    let recommended_approach_id =
        best_approach.map(|approach| approach.latest.approach_id.as_str());
    let memory = build_research_memory_summary(
        summary,
        &BTreeSet::new(),
        active_approach_id,
        recommended_approach_id,
    );
    let mut body = vec![
        "# Autoresearch Playbook".to_string(),
        String::new(),
        "## Goal".to_string(),
        goal.to_string(),
        String::new(),
        "## Mode".to_string(),
        format!("- {}", mode.cli_name()),
        String::new(),
        "## Current Leader".to_string(),
    ];
    if let Some(approach) = best_approach {
        body.extend(render_approach_block(approach, summary));
    } else {
        body.push("- No tracked leader yet.".to_string());
    }
    body.push(String::new());
    body.push("## Validation And Governance".to_string());
    body.push(format!("- {}", render_validation_policy_line(&validation)));
    if let Some(governance_line) = render_governance_line(&governance) {
        body.push(format!("- {governance_line}"));
    }
    if let Some(approach) = best_approach {
        let gate = validation_gate_for_status(
            summary,
            &approach.latest.approach_id,
            AutoresearchApproachStatus::Winner,
            &validation,
        );
        body.push(format!(
            "- {}",
            render_validation_gate_line(&approach.latest.approach_id, &gate)
        ));
    }
    for issue in validation.issues.iter().chain(governance.issues.iter()) {
        body.push(format!("- Policy warning: {issue}"));
    }
    body.push(String::new());
    body.push("## Durable Lessons".to_string());
    if memory.lines.is_empty() {
        body.push("- No durable lessons yet.".to_string());
    } else {
        body.extend(memory.lines.into_iter().map(|line| line.trim().to_string()));
    }
    body.push(String::new());
    body.push("## Discovery Carry Forward".to_string());
    if let Some(discovery) = summary.last_discovery() {
        body.push(format!("- {}", discovery.summary));
        body.extend(
            discovery
                .recommendations
                .iter()
                .map(|item| format!("- Recommendation: {item}")),
        );
        body.extend(
            discovery
                .unknowns
                .iter()
                .map(|item| format!("- Unknown: {item}")),
        );
    } else {
        body.push("- No discovery notes yet.".to_string());
    }
    std::fs::write(workdir.join(AUTORESEARCH_PLAYBOOK_FILE), body.join("\n"))
}

pub(crate) fn write_wrap_up_report_artifact(
    workdir: &Path,
    goal: &str,
    mode: AutoresearchMode,
    last_cycle_summary: Option<&str>,
    summary: &AutoresearchJournalSummary,
    active_approach_id: Option<&str>,
) -> std::io::Result<()> {
    let validation = load_validation_policy_settings(workdir);
    let governance = load_evaluation_governance_settings(workdir);
    let best_approach = preferred_approach(summary, active_approach_id);
    let best_metric = summary
        .best_metric()
        .map(|metric| metric.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let mut body = vec![
        "# Autoresearch Report".to_string(),
        String::new(),
        "## Goal".to_string(),
        goal.to_string(),
        String::new(),
        "## Outcome".to_string(),
        format!("- Mode: {}", mode.cli_name()),
        format!("- Best metric: {best_metric}"),
        format!("- Total runs: {}", summary.total_runs),
    ];
    if let Some(summary_line) = last_cycle_summary.filter(|line| !line.trim().is_empty()) {
        body.push(format!("- Final cycle summary: {summary_line}"));
    }
    body.push(String::new());
    body.push("## Best Candidate".to_string());
    if let Some(approach) = best_approach {
        body.extend(render_approach_block(approach, summary));
    } else {
        body.push("- No winning candidate recorded.".to_string());
    }
    body.push(String::new());
    body.push("## Validation".to_string());
    body.push(format!("- {}", render_validation_policy_line(&validation)));
    if let Some(governance_line) = render_governance_line(&governance) {
        body.push(format!("- {governance_line}"));
    }
    if let Some(approach) = best_approach {
        let gate = validation_gate_for_status(
            summary,
            &approach.latest.approach_id,
            AutoresearchApproachStatus::Winner,
            &validation,
        );
        body.push(format!(
            "- {}",
            render_validation_gate_line(&approach.latest.approach_id, &gate)
        ));
    }
    body.push(String::new());
    body.push("## Threats To Validity".to_string());
    if let Some(approach) = best_approach {
        if approach.latest.risks.is_empty() {
            body.push("- No explicit risks logged for the current leader.".to_string());
        } else {
            body.extend(approach.latest.risks.iter().map(|risk| format!("- {risk}")));
        }
    } else {
        body.push("- No explicit risks logged.".to_string());
    }
    if let Some(discovery) = summary.last_discovery() {
        body.extend(discovery.unknowns.iter().map(|item| format!("- {item}")));
    }
    body.push(String::new());
    body.push("## Next Discriminating Experiments".to_string());
    if let Some(discovery) = summary.last_discovery() {
        if discovery.recommendations.is_empty() {
            body.push("- No explicit next experiments logged.".to_string());
        } else {
            body.extend(
                discovery
                    .recommendations
                    .iter()
                    .map(|item| format!("- {item}")),
            );
        }
    } else {
        body.push("- No explicit next experiments logged.".to_string());
    }
    std::fs::write(workdir.join(AUTORESEARCH_REPORT_FILE), body.join("\n"))
}

fn preferred_approach<'a>(
    summary: &'a AutoresearchJournalSummary,
    active_approach_id: Option<&str>,
) -> Option<&'a AutoresearchApproachSummary> {
    summary
        .current_segment_approaches
        .iter()
        .find(|approach| approach.latest.status == AutoresearchApproachStatus::Winner)
        .or_else(|| active_approach_id.and_then(|approach_id| summary.latest_approach(approach_id)))
        .or_else(|| summary.active_approach())
        .or_else(|| summary.current_segment_approaches.first())
}

fn render_approach_block(
    approach: &AutoresearchApproachSummary,
    summary: &AutoresearchJournalSummary,
) -> Vec<String> {
    let best_metric = approach
        .best_metric
        .map(|metric| metric.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let last_metric = approach
        .last_metric
        .map(|metric| metric.to_string())
        .unwrap_or_else(|| "n/a".to_string());
    let validation_count = summary
        .current_segment_validations
        .iter()
        .filter(|entry| entry.approach_id == approach.latest.approach_id)
        .count();
    vec![
        format!("- Approach: {}", approach.latest.approach_id),
        format!("- Title: {}", approach.latest.title),
        format!("- Family: {}", approach.latest.family),
        format!("- Status: {}", approach.latest.status.as_str()),
        format!("- Best metric: {best_metric}"),
        format!("- Last metric: {last_metric}"),
        format!("- Keep runs: {}", approach.keep_count),
        format!("- Validation entries: {validation_count}"),
        format!("- Summary: {}", approach.latest.summary),
    ]
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use pretty_assertions::assert_eq;

    use super::*;
    use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
    use crate::slop_fork::autoresearch::AutoresearchConfigEntry;
    use crate::slop_fork::autoresearch::AutoresearchDiscoveryEntry;
    use crate::slop_fork::autoresearch::AutoresearchDiscoveryReason;
    use crate::slop_fork::autoresearch::AutoresearchExperimentEntry;
    use crate::slop_fork::autoresearch::AutoresearchExperimentStatus;
    use crate::slop_fork::autoresearch::MetricDirection;

    fn sample_summary() -> AutoresearchJournalSummary {
        AutoresearchJournalSummary {
            current_segment: 0,
            config: Some(AutoresearchConfigEntry {
                entry_type: "config".to_string(),
                name: "ocr".to_string(),
                metric_name: "cer".to_string(),
                metric_unit: "%".to_string(),
                direction: MetricDirection::Lower,
            }),
            total_runs: 2,
            current_segment_approaches: vec![
                crate::slop_fork::autoresearch::AutoresearchApproachSummary {
                    latest: AutoresearchApproachEntry {
                        entry_type: "approach".to_string(),
                        approach_id: "approach-1".to_string(),
                        title: "teacher-student".to_string(),
                        family: "distillation".to_string(),
                        status: AutoresearchApproachStatus::Winner,
                        summary: "best".to_string(),
                        rationale: String::new(),
                        risks: vec!["variance still unclear".to_string()],
                        sources: Vec::new(),
                        parent_approach_id: None,
                        synthesis_parent_approach_ids: Vec::new(),
                        timestamp: 1,
                        segment: 0,
                    },
                    total_runs: 2,
                    keep_count: 2,
                    best_metric: Some(4.2),
                    last_metric: Some(4.4),
                },
            ],
            current_segment_discoveries: vec![AutoresearchDiscoveryEntry {
                entry_type: "discovery".to_string(),
                reason: AutoresearchDiscoveryReason::FollowUp,
                focus: None,
                summary: "compare on holdout split".to_string(),
                recommendations: vec!["run adversarial OCR set".to_string()],
                unknowns: vec!["noise floor still unclear".to_string()],
                sources: Vec::new(),
                dead_ends: Vec::new(),
                timestamp: 2,
                segment: 0,
            }],
            current_segment_controller_decisions: Vec::new(),
            current_segment_validations: Vec::new(),
            current_segment_runs: vec![AutoresearchExperimentEntry {
                run: 1,
                commit: "approach:approach-1".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(4.2),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "best".to_string(),
                timestamp: 3,
                segment: 0,
            }],
        }
    }

    #[test]
    fn playbook_selects_winner_first() {
        let summary = sample_summary();
        let selected = preferred_approach(&summary, Some("missing")).expect("preferred approach");

        assert_eq!(selected.latest.approach_id, "approach-1");
    }

    #[test]
    fn approach_block_mentions_validation_entry_count() {
        let summary = sample_summary();
        let lines = render_approach_block(
            summary
                .current_segment_approaches
                .first()
                .expect("approach"),
            &summary,
        );

        assert!(
            lines
                .iter()
                .any(|line| line.contains("Validation entries: 0"))
        );
    }
}
