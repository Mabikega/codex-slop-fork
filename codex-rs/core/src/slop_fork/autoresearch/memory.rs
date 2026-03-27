use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use super::AutoresearchApproachStatus;
use super::AutoresearchApproachSummary;
use super::AutoresearchDiscoveryEntry;
use super::AutoresearchExperimentEntry;
use super::AutoresearchExperimentStatus;
use super::AutoresearchJournalSummary;
use super::MetricDirection;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResearchMemorySummary {
    pub lines: Vec<String>,
}

pub(crate) fn build_research_memory_summary(
    summary: &AutoresearchJournalSummary,
    query_tokens: &BTreeSet<String>,
    active_approach_id: Option<&str>,
    recommended_approach_id: Option<&str>,
) -> ResearchMemorySummary {
    let repeated_failure = repeated_failure_memory(
        summary,
        query_tokens,
        active_approach_id,
        recommended_approach_id,
    );
    let dead_end = relevant_dead_end(summary, query_tokens, repeated_failure.as_ref());

    let mut lines = Vec::new();
    if let Some(line) = strongest_family_line(summary, recommended_approach_id) {
        lines.push(line);
    }
    if let Some(line) = repeated_failure.as_ref().map(render_failure_memory) {
        lines.push(line);
    }
    if let Some(line) = dead_end_memory_line(dead_end) {
        lines.push(line);
    }
    if let Some(line) = discovery_memory_line(summary.last_discovery()) {
        lines.push(line);
    }
    ResearchMemorySummary { lines }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FailureMemory {
    approach_id: String,
    family: String,
    discard_count: usize,
    checks_failed_count: usize,
    crash_count: usize,
    latest_description: String,
    repeated_themes: Vec<String>,
}

impl FailureMemory {
    fn non_keep_count(&self) -> usize {
        self.discard_count + self.checks_failed_count + self.crash_count
    }

    fn is_repeated(&self) -> bool {
        self.non_keep_count() >= 2
    }
}

#[derive(Debug, Clone, PartialEq)]
struct FamilyAggregate {
    family: String,
    live_candidate_count: usize,
    keep_count: usize,
    best_metric: Option<f64>,
}

const FAILURE_THEME_STOPWORDS: &[&str] = &[
    "after",
    "again",
    "another",
    "approach",
    "around",
    "baseline",
    "before",
    "branch",
    "candidate",
    "change",
    "changes",
    "feature",
    "first",
    "from",
    "into",
    "local",
    "minor",
    "more",
    "next",
    "second",
    "simple",
    "stack",
    "still",
    "test",
    "tests",
    "third",
    "variant",
    "with",
    "without",
];

fn strongest_family_line(
    summary: &AutoresearchJournalSummary,
    recommended_approach_id: Option<&str>,
) -> Option<String> {
    let direction = summary.config.as_ref().map(|config| config.direction);
    let families = family_aggregates(summary);
    let preferred_family = recommended_approach_id.and_then(|approach_id| {
        summary
            .latest_approach(approach_id)
            .map(|approach| approach.latest.family.as_str())
    });
    let aggregate = preferred_family
        .and_then(|family| families.iter().find(|aggregate| aggregate.family == family))
        .or_else(|| best_family_aggregate(&families, direction))?;

    let best_metric = aggregate
        .best_metric
        .map(|metric| format!(", best={}", super::runtime::format_metric(metric)))
        .unwrap_or_default();
    Some(format!(
        "- Family memory: `{}` is the strongest surviving family so far ({} live candidate{}, {} keep{}{}).",
        aggregate.family,
        aggregate.live_candidate_count,
        plural_suffix(aggregate.live_candidate_count),
        aggregate.keep_count,
        plural_suffix(aggregate.keep_count),
        best_metric
    ))
}

fn family_aggregates(summary: &AutoresearchJournalSummary) -> Vec<FamilyAggregate> {
    let mut aggregates = BTreeMap::<String, FamilyAggregate>::new();
    for approach in &summary.current_segment_approaches {
        let entry = aggregates
            .entry(approach.latest.family.clone())
            .or_insert_with(|| FamilyAggregate {
                family: approach.latest.family.clone(),
                live_candidate_count: 0,
                keep_count: 0,
                best_metric: None,
            });
        if !matches!(
            approach.latest.status,
            AutoresearchApproachStatus::DeadEnd | AutoresearchApproachStatus::Archived
        ) {
            entry.live_candidate_count += 1;
        }
        entry.keep_count += approach.keep_count;
        if let Some(metric) = approach.best_metric {
            entry.best_metric = match (entry.best_metric, summary.config.as_ref()) {
                (Some(existing), Some(config))
                    if compare_metric(metric, existing, config.direction) == Ordering::Greater =>
                {
                    Some(metric)
                }
                (None, _) => Some(metric),
                (existing, _) => existing,
            };
        }
    }
    aggregates
        .into_values()
        .filter(|aggregate| aggregate.live_candidate_count > 0 || aggregate.keep_count > 0)
        .collect()
}

fn best_family_aggregate(
    families: &[FamilyAggregate],
    direction: Option<MetricDirection>,
) -> Option<&FamilyAggregate> {
    families.iter().max_by(|left, right| {
        left.live_candidate_count
            .cmp(&right.live_candidate_count)
            .then_with(|| left.keep_count.cmp(&right.keep_count))
            .then_with(|| compare_optional_metric(left.best_metric, right.best_metric, direction))
            .then_with(|| right.family.cmp(&left.family))
    })
}

fn repeated_failure_memory(
    summary: &AutoresearchJournalSummary,
    query_tokens: &BTreeSet<String>,
    active_approach_id: Option<&str>,
    recommended_approach_id: Option<&str>,
) -> Option<FailureMemory> {
    summary
        .current_segment_approaches
        .iter()
        .filter_map(|approach| {
            let failed_runs = failure_runs_for_approach(summary, &approach.latest.approach_id);
            if failed_runs.is_empty() {
                return None;
            }
            let repeated_themes = extract_failure_themes(approach, &failed_runs, query_tokens);
            Some((
                failure_rank(
                    approach,
                    &failed_runs,
                    query_tokens,
                    active_approach_id,
                    recommended_approach_id,
                ),
                FailureMemory {
                    approach_id: approach.latest.approach_id.clone(),
                    family: approach.latest.family.clone(),
                    discard_count: failed_runs
                        .iter()
                        .filter(|run| run.status == AutoresearchExperimentStatus::Discard)
                        .count(),
                    checks_failed_count: failed_runs
                        .iter()
                        .filter(|run| run.status == AutoresearchExperimentStatus::ChecksFailed)
                        .count(),
                    crash_count: failed_runs
                        .iter()
                        .filter(|run| run.status == AutoresearchExperimentStatus::Crash)
                        .count(),
                    latest_description: failed_runs
                        .first()
                        .map(|run| run.description.clone())
                        .unwrap_or_default(),
                    repeated_themes,
                },
            ))
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.non_keep_count().cmp(&right.1.non_keep_count()))
                .then_with(|| right.1.approach_id.cmp(&left.1.approach_id))
        })
        .map(|(_rank, failure)| failure)
}

fn failure_runs_for_approach<'a>(
    summary: &'a AutoresearchJournalSummary,
    approach_id: &str,
) -> Vec<&'a AutoresearchExperimentEntry> {
    let mut failed_runs = Vec::new();
    for run in summary
        .current_segment_runs
        .iter()
        .rev()
        .filter(|run| run.approach_id.as_deref() == Some(approach_id))
    {
        if run.status == AutoresearchExperimentStatus::Keep {
            break;
        }
        failed_runs.push(run);
    }
    failed_runs
}

fn failure_rank(
    approach: &AutoresearchApproachSummary,
    failed_runs: &[&AutoresearchExperimentEntry],
    query_tokens: &BTreeSet<String>,
    active_approach_id: Option<&str>,
    recommended_approach_id: Option<&str>,
) -> i64 {
    let mut rank = i64::try_from(failed_runs.len()).unwrap_or(i64::MAX) * 6;
    rank += i64::try_from(
        failed_runs
            .iter()
            .filter(|run| run.status == AutoresearchExperimentStatus::Crash)
            .count(),
    )
    .unwrap_or(i64::MAX)
        * 4;
    rank += i64::try_from(
        failed_runs
            .iter()
            .filter(|run| run.status == AutoresearchExperimentStatus::ChecksFailed)
            .count(),
    )
    .unwrap_or(i64::MAX)
        * 3;
    rank += i64::from(text_overlap_score(
        query_tokens,
        &[
            &approach.latest.title,
            &approach.latest.family,
            &approach.latest.summary,
            &approach.latest.rationale,
        ],
    )) * 2;
    if active_approach_id == Some(approach.latest.approach_id.as_str()) {
        rank += 5;
    }
    if recommended_approach_id == Some(approach.latest.approach_id.as_str()) {
        rank += 4;
    }
    if approach.latest.status == AutoresearchApproachStatus::DeadEnd {
        rank += 2;
    }
    rank
}

fn render_failure_memory(memory: &FailureMemory) -> String {
    if memory.is_repeated() {
        let themes = if memory.repeated_themes.is_empty() {
            String::new()
        } else {
            format!(
                "; repeated themes: {}",
                memory
                    .repeated_themes
                    .iter()
                    .map(|theme| format!("`{theme}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return format!(
            "- Retrieval memory: `{}` [{}] already has {} non-keeps (discard {}, checks_failed {}, crash {}){}. Do not retry that line without a materially different hypothesis.",
            memory.approach_id,
            memory.family,
            memory.non_keep_count(),
            memory.discard_count,
            memory.checks_failed_count,
            memory.crash_count,
            themes
        );
    }
    format!(
        "- Retrieval memory: latest failure on `{}` [{}] was \"{}\".",
        memory.approach_id,
        memory.family,
        clip_text(&memory.latest_description, /*limit*/ 96)
    )
}

fn relevant_dead_end<'a>(
    summary: &'a AutoresearchJournalSummary,
    query_tokens: &BTreeSet<String>,
    repeated_failure: Option<&FailureMemory>,
) -> Option<&'a AutoresearchApproachSummary> {
    let repeated_failure_id = repeated_failure.map(|memory| memory.approach_id.as_str());
    let mut dead_ends = summary
        .current_segment_approaches
        .iter()
        .filter(|approach| {
            approach.latest.status == AutoresearchApproachStatus::DeadEnd
                && repeated_failure_id != Some(approach.latest.approach_id.as_str())
        })
        .collect::<Vec<_>>();
    dead_ends.sort_by(|left, right| {
        text_overlap_score(
            query_tokens,
            &[
                &right.latest.title,
                &right.latest.family,
                &right.latest.summary,
                &right.latest.rationale,
            ],
        )
        .cmp(&text_overlap_score(
            query_tokens,
            &[
                &left.latest.title,
                &left.latest.family,
                &left.latest.summary,
                &left.latest.rationale,
            ],
        ))
        .then_with(|| right.latest.timestamp.cmp(&left.latest.timestamp))
    });
    dead_ends.into_iter().next()
}

fn dead_end_memory_line(dead_end: Option<&AutoresearchApproachSummary>) -> Option<String> {
    dead_end.map(|dead_end| {
        format!(
            "- Dead-end memory: `{}` [{}] already dead-ended. Last summary: {}.",
            dead_end.latest.approach_id,
            dead_end.latest.family,
            clip_text(&dead_end.latest.summary, /*limit*/ 96)
        )
    })
}

fn discovery_memory_line(discovery: Option<&AutoresearchDiscoveryEntry>) -> Option<String> {
    let discovery = discovery?;
    let recommendation = discovery
        .recommendations
        .first()
        .map(|text| format!("carry forward {}", quoted_clip(text, /*limit*/ 84)));
    let unknown = discovery
        .unknowns
        .first()
        .map(|text| format!("unresolved unknown: {}", quoted_clip(text, /*limit*/ 72)));
    let dead_end = discovery
        .dead_ends
        .first()
        .map(|text| format!("avoid revisiting {}", quoted_clip(text, /*limit*/ 72)));
    let mut parts = [recommendation, unknown, dead_end]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    if parts.is_empty() {
        parts.push(format!(
            "last discovery summary: {}",
            quoted_clip(&discovery.summary, /*limit*/ 84)
        ));
    }
    Some(format!("- Discovery memory: {}.", parts.join("; ")))
}

fn extract_failure_themes(
    approach: &AutoresearchApproachSummary,
    failed_runs: &[&AutoresearchExperimentEntry],
    query_tokens: &BTreeSet<String>,
) -> Vec<String> {
    let ignored_tokens = tokenize(&approach.latest.title)
        .into_iter()
        .chain(tokenize(&approach.latest.family))
        .chain(query_tokens.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut counts = BTreeMap::<String, usize>::new();
    for run in failed_runs {
        for token in tokenize(&run.description) {
            if token.len() < 4
                || ignored_tokens.contains(&token)
                || FAILURE_THEME_STOPWORDS.contains(&token.as_str())
            {
                continue;
            }
            *counts.entry(token).or_default() += 1;
        }
    }
    let mut themes = counts.into_iter().collect::<Vec<_>>();
    themes.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| right.0.len().cmp(&left.0.len()))
            .then_with(|| left.0.cmp(&right.0))
    });
    themes
        .into_iter()
        .take(2)
        .map(|(token, _count)| token)
        .collect()
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

fn tokenize(text: &str) -> BTreeSet<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .map(str::trim)
        .filter(|token| token.len() >= 3)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn compare_optional_metric(
    left: Option<f64>,
    right: Option<f64>,
    direction: Option<MetricDirection>,
) -> Ordering {
    match (left, right, direction) {
        (Some(left), Some(right), Some(direction)) => compare_metric(left, right, direction),
        (Some(_), None, _) => Ordering::Greater,
        (None, Some(_), _) => Ordering::Less,
        _ => Ordering::Equal,
    }
}

fn compare_metric(left: f64, right: f64, direction: MetricDirection) -> Ordering {
    match direction {
        MetricDirection::Lower => right.total_cmp(&left),
        MetricDirection::Higher => left.total_cmp(&right),
    }
}

fn quoted_clip(text: &str, limit: usize) -> String {
    format!("\"{}\"", clip_text(text, limit))
}

fn clip_text(text: &str, limit: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= limit {
        compact
    } else {
        let clipped = compact
            .chars()
            .take(limit.saturating_sub(3))
            .collect::<String>();
        format!("{clipped}...")
    }
}

fn plural_suffix(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
    use crate::slop_fork::autoresearch::AutoresearchConfigEntry;
    use crate::slop_fork::autoresearch::AutoresearchDiscoveryReason;
    use pretty_assertions::assert_eq;

    fn sample_summary() -> AutoresearchJournalSummary {
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
                reason: AutoresearchDiscoveryReason::FollowUp,
                focus: Some("retrieval".to_string()),
                summary: "Compared retrieval families".to_string(),
                recommendations: vec!["compare the reranker with distilled retrieval".to_string()],
                unknowns: vec!["whether evaluator variance is masking small wins".to_string()],
                sources: Vec::new(),
                dead_ends: vec!["parser rewrite".to_string()],
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
    fn summarized_memory_condenses_family_failure_and_discovery_context() {
        let summary = sample_summary();
        let memory = build_research_memory_summary(
            &summary,
            &tokenize("improve retrieval quality"),
            Some("approach-1"),
            Some("approach-2"),
        );

        assert_eq!(
            memory.lines,
            vec![
                "- Family memory: `retrieval` is the strongest surviving family so far (1 live candidate, 2 keeps, best=0.71).".to_string(),
                "- Retrieval memory: `approach-1` [baseline] already has 3 non-keeps (discard 1, checks_failed 1, crash 1); repeated themes: `recall`, `tweak`. Do not retry that line without a materially different hypothesis.".to_string(),
                "- Dead-end memory: `approach-3` [parsing] already dead-ended. Last summary: The parser rewrite kept breaking the evaluator.".to_string(),
                "- Discovery memory: carry forward \"compare the reranker with distilled retrieval\"; unresolved unknown: \"whether evaluator variance is masking small wins\"; avoid revisiting \"parser rewrite\".".to_string(),
            ]
        );
    }

    #[test]
    fn summarized_memory_falls_back_to_latest_failure_without_repeat_pattern() {
        let mut summary = sample_summary();
        summary.current_segment_runs = vec![AutoresearchExperimentEntry {
            run: 1,
            commit: "aaa".to_string(),
            approach_id: Some("approach-1".to_string()),
            metric: Some(0.40),
            metrics: BTreeMap::new(),
            status: AutoresearchExperimentStatus::Discard,
            description: "baseline tweak regressed retrieval recall".to_string(),
            timestamp: 10,
            segment: 0,
        }];

        let memory = build_research_memory_summary(
            &summary,
            &tokenize("improve retrieval quality"),
            Some("approach-1"),
            Some("approach-2"),
        );

        assert_eq!(
            memory.lines[1],
            "- Retrieval memory: latest failure on `approach-1` [baseline] was \"baseline tweak regressed retrieval recall\"."
        );
    }

    #[test]
    fn recovered_branch_does_not_emit_retry_warning_from_old_failures() {
        let mut summary = sample_summary();
        summary.current_segment_approaches[0].keep_count = 1;
        summary.current_segment_approaches[0].best_metric = Some(0.73);
        summary.current_segment_approaches[0].last_metric = Some(0.73);
        summary.current_segment_approaches[0].latest.status = AutoresearchApproachStatus::Winner;
        summary.current_segment_approaches[1].keep_count = 1;
        summary.current_segment_approaches[1].best_metric = Some(0.71);
        summary.current_segment_approaches[1].last_metric = Some(0.71);
        summary.current_segment_runs = vec![
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
                description: "baseline stack broke checks after another recall tweak".to_string(),
                timestamp: 11,
                segment: 0,
            },
            AutoresearchExperimentEntry {
                run: 3,
                commit: "ccc".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(0.73),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "baseline recovered with stronger retrieval quality".to_string(),
                timestamp: 12,
                segment: 0,
            },
        ];

        let memory = build_research_memory_summary(
            &summary,
            &tokenize("improve retrieval quality"),
            Some("approach-1"),
            Some("approach-1"),
        );

        assert!(
            !memory
                .lines
                .iter()
                .any(|line| line.contains("Do not retry that line"))
        );
        assert!(
            !memory
                .lines
                .iter()
                .any(|line| line.contains("approach-1") && line.contains("Retrieval memory"))
        );
    }
}
