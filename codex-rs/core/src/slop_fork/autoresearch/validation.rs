use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use serde::Deserialize;
use serde::Serialize;
use tracing::debug;

use super::AUTORESEARCH_DOC_FILE;
use super::AutoresearchApproachStatus;
use super::AutoresearchExperimentStatus;
use super::AutoresearchJournalSummary;

const VALIDATION_POLICY_HEADING: &str = "Validation Policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchValidationType {
    Rerun,
    Holdout,
    Adversarial,
    EvaluatorAudit,
}

impl AutoresearchValidationType {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_key(value).as_str() {
            "rerun" | "reruns" | "replication" => Some(Self::Rerun),
            "holdout" | "outofsample" => Some(Self::Holdout),
            "adversarial" => Some(Self::Adversarial),
            "evaluatoraudit" | "audit" => Some(Self::EvaluatorAudit),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rerun => "rerun",
            Self::Holdout => "holdout",
            Self::Adversarial => "adversarial",
            Self::EvaluatorAudit => "evaluator_audit",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Rerun => "rerun",
            Self::Holdout => "holdout",
            Self::Adversarial => "adversarial",
            Self::EvaluatorAudit => "evaluator audit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchValidationOutcome {
    Pass,
    Fail,
    Mixed,
}

impl AutoresearchValidationOutcome {
    pub fn parse(value: &str) -> Option<Self> {
        match normalize_key(value).as_str() {
            "pass" | "passed" => Some(Self::Pass),
            "fail" | "failed" => Some(Self::Fail),
            "mixed" => Some(Self::Mixed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchValidationEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub approach_id: String,
    pub validation_type: AutoresearchValidationType,
    pub outcome: AutoresearchValidationOutcome,
    pub summary: String,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub timestamp: i64,
    #[serde(default)]
    pub segment: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidationPolicy {
    pub promising_min_keeps: usize,
    pub winner_min_keeps: usize,
    pub require_holdout_for_winner: bool,
    pub require_adversarial_for_winner: bool,
    pub require_evaluator_audit_for_winner: bool,
}

impl Default for ValidationPolicy {
    fn default() -> Self {
        Self {
            promising_min_keeps: 1,
            winner_min_keeps: 2,
            require_holdout_for_winner: false,
            require_adversarial_for_winner: false,
            require_evaluator_audit_for_winner: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationPolicySettings {
    pub policy: ValidationPolicy,
    pub issues: Vec<String>,
    pub has_custom_values: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvaluationGovernance {
    pub locked_metrics: BTreeSet<String>,
    pub exploratory_metrics: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvaluationGovernanceSettings {
    pub governance: EvaluationGovernance,
    pub issues: Vec<String>,
    pub has_custom_values: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationGateDecision {
    pub keep_count: usize,
    pub latest_results: BTreeMap<AutoresearchValidationType, AutoresearchValidationOutcome>,
    pub unmet_requirements: Vec<String>,
}

impl ValidationGateDecision {
    pub fn allows_promotion(&self) -> bool {
        self.unmet_requirements.is_empty()
    }

    pub fn passed_types(&self) -> Vec<AutoresearchValidationType> {
        self.latest_results
            .iter()
            .filter_map(|(validation_type, outcome)| {
                (*outcome == AutoresearchValidationOutcome::Pass).then_some(*validation_type)
            })
            .collect()
    }
}

pub fn load_validation_policy_settings(workdir: &Path) -> ValidationPolicySettings {
    let Some(doc) = load_autoresearch_doc(workdir) else {
        return ValidationPolicySettings::default();
    };
    parse_validation_policy_settings(&doc)
}

pub fn load_evaluation_governance_settings(workdir: &Path) -> EvaluationGovernanceSettings {
    let Some(doc) = load_autoresearch_doc(workdir) else {
        return EvaluationGovernanceSettings::default();
    };
    parse_evaluation_governance_settings(&doc)
}

pub fn validation_gate_for_status(
    summary: &AutoresearchJournalSummary,
    approach_id: &str,
    target_status: AutoresearchApproachStatus,
    policy: &ValidationPolicySettings,
) -> ValidationGateDecision {
    let keep_count = summary
        .current_segment_runs
        .iter()
        .filter(|run| run.approach_id.as_deref() == Some(approach_id))
        .filter(|run| run.status == AutoresearchExperimentStatus::Keep)
        .count();
    let latest_results = latest_validation_results(summary, approach_id);
    let mut unmet_requirements = Vec::new();

    if matches!(
        target_status,
        AutoresearchApproachStatus::Promising | AutoresearchApproachStatus::Winner
    ) && keep_count < policy.policy.promising_min_keeps
    {
        unmet_requirements.push(format!(
            "needs at least {} keep run(s) before it can be marked promising",
            policy.policy.promising_min_keeps
        ));
    }

    if target_status == AutoresearchApproachStatus::Winner {
        if keep_count < policy.policy.winner_min_keeps {
            unmet_requirements.push(format!(
                "needs at least {} keep run(s) before it can be marked winner",
                policy.policy.winner_min_keeps
            ));
        }
        extend_required_validation(
            &mut unmet_requirements,
            &latest_results,
            policy.policy.require_holdout_for_winner,
            AutoresearchValidationType::Holdout,
        );
        extend_required_validation(
            &mut unmet_requirements,
            &latest_results,
            policy.policy.require_adversarial_for_winner,
            AutoresearchValidationType::Adversarial,
        );
        extend_required_validation(
            &mut unmet_requirements,
            &latest_results,
            policy.policy.require_evaluator_audit_for_winner,
            AutoresearchValidationType::EvaluatorAudit,
        );
    }

    ValidationGateDecision {
        keep_count,
        latest_results,
        unmet_requirements,
    }
}

pub(crate) fn enforce_locked_metrics(
    metrics: &BTreeMap<String, f64>,
    governance: &EvaluationGovernanceSettings,
) -> Result<(), String> {
    if governance.governance.locked_metrics.is_empty() {
        return Ok(());
    }
    let present = metrics
        .keys()
        .map(|name| normalize_metric_name(name))
        .collect::<BTreeSet<_>>();
    let missing = governance
        .governance
        .locked_metrics
        .iter()
        .filter(|name| !present.contains(&normalize_metric_name(name)))
        .cloned()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        return Ok(());
    }
    Err(format!(
        "locked metrics missing from benchmark output: {}",
        missing.join(", ")
    ))
}

pub fn render_validation_policy_line(settings: &ValidationPolicySettings) -> String {
    format!(
        "Validation policy: promising_keeps={} winner_keeps={} holdout_before_winner={} adversarial_before_winner={} evaluator_audit_before_winner={}",
        settings.policy.promising_min_keeps,
        settings.policy.winner_min_keeps,
        yes_no(settings.policy.require_holdout_for_winner),
        yes_no(settings.policy.require_adversarial_for_winner),
        yes_no(settings.policy.require_evaluator_audit_for_winner)
    )
}

pub fn render_governance_line(settings: &EvaluationGovernanceSettings) -> Option<String> {
    if settings.governance.locked_metrics.is_empty()
        && settings.governance.exploratory_metrics.is_empty()
    {
        return None;
    }
    let locked = if settings.governance.locked_metrics.is_empty() {
        "none".to_string()
    } else {
        settings
            .governance
            .locked_metrics
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let exploratory = if settings.governance.exploratory_metrics.is_empty() {
        "none".to_string()
    } else {
        settings
            .governance
            .exploratory_metrics
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(format!(
        "Evaluation governance: locked={locked} exploratory={exploratory}"
    ))
}

pub fn render_validation_gate_line(approach_id: &str, gate: &ValidationGateDecision) -> String {
    let passed = gate
        .passed_types()
        .into_iter()
        .map(AutoresearchValidationType::display_name)
        .collect::<Vec<_>>();
    let passed_text = if passed.is_empty() {
        "none".to_string()
    } else {
        passed.join(", ")
    };
    if gate.unmet_requirements.is_empty() {
        format!(
            "Validation state: `{approach_id}` is promotion-ready with {} keep run(s); passed validations: {passed_text}",
            gate.keep_count
        )
    } else {
        format!(
            "Validation state: `{approach_id}` has {} keep run(s); pending: {}",
            gate.keep_count,
            gate.unmet_requirements.join("; ")
        )
    }
}

pub(crate) fn render_validation_prompt_lines(
    summary: &AutoresearchJournalSummary,
    active_approach_id: Option<&str>,
    recommended_approach_id: Option<&str>,
    policy: &ValidationPolicySettings,
    governance: &EvaluationGovernanceSettings,
) -> Vec<String> {
    let mut lines = vec![format!("- {}", render_validation_policy_line(policy))];
    if let Some(governance_line) = render_governance_line(governance) {
        lines.push(format!("- {governance_line}."));
    }
    for approach_id in [recommended_approach_id, active_approach_id]
        .into_iter()
        .flatten()
    {
        let gate = validation_gate_for_status(
            summary,
            approach_id,
            AutoresearchApproachStatus::Winner,
            policy,
        );
        lines.push(format!(
            "- {}.",
            render_validation_gate_line(approach_id, &gate)
        ));
    }
    lines
}

pub(crate) fn render_policy_issue_lines(
    validation: &ValidationPolicySettings,
    governance: &EvaluationGovernanceSettings,
) -> Vec<String> {
    validation
        .issues
        .iter()
        .chain(governance.issues.iter())
        .map(|issue| format!("Policy warning: {issue}"))
        .collect()
}

fn extend_required_validation(
    unmet_requirements: &mut Vec<String>,
    latest_results: &BTreeMap<AutoresearchValidationType, AutoresearchValidationOutcome>,
    required: bool,
    validation_type: AutoresearchValidationType,
) {
    if !required {
        return;
    }
    match latest_results.get(&validation_type).copied() {
        Some(AutoresearchValidationOutcome::Pass) => {}
        Some(AutoresearchValidationOutcome::Fail | AutoresearchValidationOutcome::Mixed) => {
            unmet_requirements.push(format!(
                "latest {} validation is not a pass",
                validation_type.display_name()
            ));
        }
        None => {
            unmet_requirements.push(format!(
                "needs a passing {} validation before it can be marked winner",
                validation_type.display_name()
            ));
        }
    }
}

fn latest_validation_results(
    summary: &AutoresearchJournalSummary,
    approach_id: &str,
) -> BTreeMap<AutoresearchValidationType, AutoresearchValidationOutcome> {
    let mut latest_results = BTreeMap::new();
    for validation in &summary.current_segment_validations {
        if validation.approach_id == approach_id {
            latest_results.insert(validation.validation_type, validation.outcome);
        }
    }
    latest_results
}

fn parse_validation_policy_settings(doc: &str) -> ValidationPolicySettings {
    let Some(section) = extract_markdown_section(doc, VALIDATION_POLICY_HEADING) else {
        return ValidationPolicySettings::default();
    };
    let mut settings = ValidationPolicySettings::default();
    for (index, raw_line) in section.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match apply_validation_policy_line(&mut settings.policy, trimmed) {
            Ok(LineOutcome::Applied) => settings.has_custom_values = true,
            Ok(LineOutcome::Ignored) => {}
            Err(issue) => settings.issues.push(format!(
                "validation policy line {} `{trimmed}` is invalid: {issue}",
                index.saturating_add(1)
            )),
        }
    }
    settings
}

fn parse_evaluation_governance_settings(doc: &str) -> EvaluationGovernanceSettings {
    let Some(section) = extract_markdown_section(doc, VALIDATION_POLICY_HEADING) else {
        return EvaluationGovernanceSettings::default();
    };
    let mut settings = EvaluationGovernanceSettings::default();
    for (index, raw_line) in section.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match apply_governance_line(&mut settings.governance, trimmed) {
            Ok(LineOutcome::Applied) => settings.has_custom_values = true,
            Ok(LineOutcome::Ignored) => {}
            Err(issue) => settings.issues.push(format!(
                "validation policy line {} `{trimmed}` is invalid: {issue}",
                index.saturating_add(1)
            )),
        }
    }
    let overlapping = settings
        .governance
        .locked_metrics
        .intersection(&settings.governance.exploratory_metrics)
        .cloned()
        .collect::<Vec<_>>();
    if !overlapping.is_empty() {
        settings.issues.push(format!(
            "metrics cannot be both locked and exploratory: {}",
            overlapping.join(", ")
        ));
    }
    settings
}

enum LineOutcome {
    Applied,
    Ignored,
}

fn apply_validation_policy_line(
    policy: &mut ValidationPolicy,
    line: &str,
) -> Result<LineOutcome, String> {
    let Some((key, value)) = parse_bullet_key_value(line) else {
        return Ok(LineOutcome::Ignored);
    };
    match normalize_key(&key).as_str() {
        "promisingminimumkeeps" => {
            policy.promising_min_keeps =
                parse_positive_usize(&value, "Promising Minimum Keeps", /*minimum*/ 1)?;
            Ok(LineOutcome::Applied)
        }
        "winnerminimumkeeps" => {
            policy.winner_min_keeps =
                parse_positive_usize(&value, "Winner Minimum Keeps", /*minimum*/ 1)?;
            Ok(LineOutcome::Applied)
        }
        "requireholdoutbeforewinner" => {
            policy.require_holdout_for_winner =
                parse_bool(&value, "Require Holdout Before Winner")?;
            Ok(LineOutcome::Applied)
        }
        "requireadversarialbeforewinner" => {
            policy.require_adversarial_for_winner =
                parse_bool(&value, "Require Adversarial Before Winner")?;
            Ok(LineOutcome::Applied)
        }
        "requireevaluatorauditbeforewinner" => {
            policy.require_evaluator_audit_for_winner =
                parse_bool(&value, "Require Evaluator Audit Before Winner")?;
            Ok(LineOutcome::Applied)
        }
        _ => Ok(LineOutcome::Ignored),
    }
}

fn apply_governance_line(
    governance: &mut EvaluationGovernance,
    line: &str,
) -> Result<LineOutcome, String> {
    let Some((key, value)) = parse_bullet_key_value(line) else {
        return Ok(LineOutcome::Ignored);
    };
    match normalize_key(&key).as_str() {
        "lockedmetrics" => {
            governance.locked_metrics = parse_metric_list(&value, "Locked Metrics")?;
            Ok(LineOutcome::Applied)
        }
        "exploratorymetrics" => {
            governance.exploratory_metrics = parse_metric_list(&value, "Exploratory Metrics")?;
            Ok(LineOutcome::Applied)
        }
        _ => Ok(LineOutcome::Ignored),
    }
}

fn load_autoresearch_doc(workdir: &Path) -> Option<String> {
    let doc_path = workdir.join(AUTORESEARCH_DOC_FILE);
    match std::fs::read_to_string(&doc_path) {
        Ok(doc) => Some(doc),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            debug!(
                error = %err,
                path = %doc_path.display(),
                "failed to read autoresearch validation configuration"
            );
            None
        }
    }
}

fn extract_markdown_section(doc: &str, heading: &str) -> Option<String> {
    let mut in_section = false;
    let mut lines = Vec::new();
    for line in doc.lines() {
        if let Some(found_heading) = parse_markdown_heading(line) {
            if in_section {
                break;
            }
            if found_heading == heading {
                in_section = true;
            }
            continue;
        }
        if in_section {
            lines.push(line);
        }
    }
    let section = lines.join("\n");
    let trimmed = section.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn parse_markdown_heading(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
    if hashes == 0 {
        return None;
    }
    let heading = trimmed[hashes..].trim();
    (!heading.is_empty()).then_some(heading)
}

fn parse_bullet_key_value(line: &str) -> Option<(String, String)> {
    let stripped = strip_list_prefix(line.trim());
    let (key, value) = stripped.split_once(':')?;
    Some((key.trim().to_string(), value.trim().to_string()))
}

fn strip_list_prefix(line: &str) -> &str {
    if let Some(rest) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
        return rest.trim_start();
    }
    let digit_prefix_len = line
        .chars()
        .take_while(char::is_ascii_digit)
        .map(char::len_utf8)
        .sum::<usize>();
    if digit_prefix_len == 0 {
        return line;
    }
    let suffix = &line[digit_prefix_len..];
    if let Some(rest) = suffix
        .strip_prefix(". ")
        .or_else(|| suffix.strip_prefix(") "))
    {
        return rest.trim_start();
    }
    line
}

fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn normalize_metric_name(metric: &str) -> String {
    metric.trim().to_ascii_lowercase()
}

fn parse_positive_usize(value: &str, field_name: &str, minimum: usize) -> Result<usize, String> {
    let parsed = value
        .trim()
        .parse::<usize>()
        .map_err(|_| format!("{field_name} must be an integer"))?;
    if parsed < minimum {
        return Err(format!("{field_name} must be >= {minimum}"));
    }
    Ok(parsed)
}

fn parse_bool(value: &str, field_name: &str) -> Result<bool, String> {
    match normalize_key(value).as_str() {
        "true" | "yes" | "required" => Ok(true),
        "false" | "no" | "optional" => Ok(false),
        _ => Err(format!("{field_name} must be true/false")),
    }
}

fn parse_metric_list(value: &str, field_name: &str) -> Result<BTreeSet<String>, String> {
    let metrics = value
        .split(',')
        .map(str::trim)
        .filter(|metric| !metric.is_empty())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    if metrics.is_empty() {
        return Err(format!("{field_name} must list at least one metric"));
    }
    Ok(metrics)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;
    use crate::slop_fork::autoresearch::AutoresearchApproachEntry;
    use crate::slop_fork::autoresearch::AutoresearchJournal;

    #[test]
    fn validation_policy_defaults_when_missing() {
        assert_eq!(
            parse_validation_policy_settings("# Goal\nx\n"),
            ValidationPolicySettings::default()
        );
    }

    #[test]
    fn validation_policy_parses_supported_keys() {
        let settings = parse_validation_policy_settings(
            "# Goal\nx\n\n## Validation Policy\n- Promising Minimum Keeps: 2\n- Winner Minimum Keeps: 4\n- Require Holdout Before Winner: yes\n- Require Adversarial Before Winner: true\n- Require Evaluator Audit Before Winner: no\n",
        );

        assert!(settings.has_custom_values);
        assert_eq!(
            settings.policy,
            ValidationPolicy {
                promising_min_keeps: 2,
                winner_min_keeps: 4,
                require_holdout_for_winner: true,
                require_adversarial_for_winner: true,
                require_evaluator_audit_for_winner: false,
            }
        );
        assert!(settings.issues.is_empty());
    }

    #[test]
    fn governance_parses_locked_and_exploratory_metrics() {
        let settings = parse_evaluation_governance_settings(
            "# Goal\nx\n\n## Validation Policy\n- Locked Metrics: cer, latency_ms\n- Exploratory Metrics: judge_score, novelty_score\n",
        );

        assert!(settings.has_custom_values);
        assert_eq!(
            settings.governance.locked_metrics,
            BTreeSet::from(["cer".to_string(), "latency_ms".to_string()])
        );
        assert_eq!(
            settings.governance.exploratory_metrics,
            BTreeSet::from(["judge_score".to_string(), "novelty_score".to_string()])
        );
        assert!(settings.issues.is_empty());
    }

    #[test]
    fn gate_requires_keep_count_and_validations_for_winner() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("journal");
        journal
            .append_config(
                "ocr".to_string(),
                "cer".to_string(),
                "%".to_string(),
                crate::slop_fork::autoresearch::MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_approach(AutoresearchApproachEntry {
                entry_type: "approach".to_string(),
                approach_id: "approach-1".to_string(),
                title: "teacher".to_string(),
                family: "distill".to_string(),
                status: AutoresearchApproachStatus::Active,
                summary: "x".to_string(),
                rationale: String::new(),
                risks: Vec::new(),
                sources: Vec::new(),
                parent_approach_id: None,
                synthesis_parent_approach_ids: Vec::new(),
                timestamp: 1,
                segment: 0,
            })
            .expect("approach");
        journal
            .append_experiment(
                crate::slop_fork::autoresearch::AutoresearchExperimentEntry {
                    run: 1,
                    commit: "approach:approach-1".to_string(),
                    approach_id: Some("approach-1".to_string()),
                    metric: Some(4.0),
                    metrics: BTreeMap::new(),
                    status: AutoresearchExperimentStatus::Keep,
                    description: "first".to_string(),
                    timestamp: 2,
                    segment: 0,
                },
            )
            .expect("experiment");

        let summary = journal.summary();
        let gate = validation_gate_for_status(
            &summary,
            "approach-1",
            AutoresearchApproachStatus::Winner,
            &ValidationPolicySettings {
                policy: ValidationPolicy {
                    promising_min_keeps: 1,
                    winner_min_keeps: 2,
                    require_holdout_for_winner: true,
                    require_adversarial_for_winner: false,
                    require_evaluator_audit_for_winner: false,
                },
                issues: Vec::new(),
                has_custom_values: true,
            },
        );

        assert_eq!(gate.keep_count, 1);
        assert_eq!(
            gate.unmet_requirements,
            vec![
                "needs at least 2 keep run(s) before it can be marked winner".to_string(),
                "needs a passing holdout validation before it can be marked winner".to_string()
            ]
        );
    }

    #[test]
    fn enforce_locked_metrics_reports_missing_values() {
        let governance = EvaluationGovernanceSettings {
            governance: EvaluationGovernance {
                locked_metrics: BTreeSet::from(["cer".to_string(), "latency_ms".to_string()]),
                exploratory_metrics: BTreeSet::new(),
            },
            issues: Vec::new(),
            has_custom_values: true,
        };

        let error =
            enforce_locked_metrics(&BTreeMap::from([("cer".to_string(), 4.0)]), &governance)
                .expect_err("missing locked metric");

        assert!(error.contains("latency_ms"));
    }
}
