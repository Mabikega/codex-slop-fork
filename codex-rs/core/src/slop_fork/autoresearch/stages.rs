use std::path::Path;
use std::sync::LazyLock;

use regex_lite::Regex;

use super::AUTORESEARCH_DOC_FILE;
use super::AutoresearchConfigEntry;
use super::AutoresearchJournalSummary;
use super::MetricDirection;

const STAGED_TARGETS_HEADING: &str = "Staged Targets";
const PRIMARY_METRIC_HEADING: &str = "Primary Metric";

static STAGE_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    let Ok(regex) = Regex::new(
        r"(?x)
        ^\s*
        (?:[-*]|\d+[.)])?
        \s*
        (?:[^:]+:\s*)?
        (?:(?P<metric>[A-Za-z0-9_.-]+)\s*)?
        (?P<op><=|>=|<|>)
        \s*
        (?P<value>[-+]?(?:\d+(?:\.\d+)?|\.\d+)(?:[eE][-+]?\d+)?)
        (?:\s*(?P<unit>[^\s,;]+))?
        (?:\s+.*)?$
    ",
    ) else {
        panic!("stage regex should compile");
    };
    regex
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageComparator {
    LessThan,
    LessThanOrEqual,
    GreaterThan,
    GreaterThanOrEqual,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchStageTarget {
    pub display: String,
    pub threshold: f64,
    pub comparator: StageComparator,
}

impl AutoresearchStageTarget {
    pub fn is_reached(&self, metric: f64) -> bool {
        match self.comparator {
            StageComparator::LessThan => metric < self.threshold,
            StageComparator::LessThanOrEqual => {
                metric < self.threshold || same_metric(metric, self.threshold)
            }
            StageComparator::GreaterThan => metric > self.threshold,
            StageComparator::GreaterThanOrEqual => {
                metric > self.threshold || same_metric(metric, self.threshold)
            }
        }
    }

    fn is_same_or_harder_than(&self, other: &Self, direction: MetricDirection) -> bool {
        match direction {
            MetricDirection::Lower => {
                if self.threshold < other.threshold {
                    true
                } else if self.threshold > other.threshold {
                    false
                } else {
                    strictness_rank(self.comparator) >= strictness_rank(other.comparator)
                }
            }
            MetricDirection::Higher => {
                if self.threshold > other.threshold {
                    true
                } else if self.threshold < other.threshold {
                    false
                } else {
                    strictness_rank(self.comparator) >= strictness_rank(other.comparator)
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchStageProgress {
    pub stages: Vec<AutoresearchStageTarget>,
    pub achieved_count: usize,
    pub best_metric: Option<f64>,
    pub issues: Vec<String>,
}

impl AutoresearchStageProgress {
    pub fn has_issues(&self) -> bool {
        !self.issues.is_empty()
    }

    pub fn issue_summary(&self) -> String {
        self.issues.join("; ")
    }

    pub fn total_stages(&self) -> usize {
        self.stages.len()
    }

    pub fn all_reached(&self) -> bool {
        !self.has_issues() && self.total_stages() > 0 && self.achieved_count >= self.total_stages()
    }

    pub fn active_stage_number(&self) -> Option<usize> {
        (!self.has_issues() && !self.all_reached()).then_some(self.achieved_count.saturating_add(1))
    }

    pub fn active_stage(&self) -> Option<&AutoresearchStageTarget> {
        (!self.has_issues() && !self.all_reached())
            .then(|| self.stages.get(self.achieved_count))
            .flatten()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
struct ParsedStagedTargets {
    stages: Vec<AutoresearchStageTarget>,
    issues: Vec<String>,
}

pub fn load_stage_progress(
    workdir: &Path,
    summary: &AutoresearchJournalSummary,
) -> Option<AutoresearchStageProgress> {
    let doc_path = workdir.join(AUTORESEARCH_DOC_FILE);
    let doc = match std::fs::read_to_string(&doc_path) {
        Ok(doc) => doc,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(err) => {
            tracing::debug!(
                error = %err,
                path = %doc_path.display(),
                "failed to read autoresearch staged targets"
            );
            return None;
        }
    };
    let staged_targets_section = extract_markdown_section(&doc, STAGED_TARGETS_HEADING)?;
    let config = match stage_validation_config(&doc, summary) {
        Ok(config) => config,
        Err(issue) => {
            return Some(AutoresearchStageProgress {
                stages: Vec::new(),
                achieved_count: 0,
                best_metric: summary.best_metric(),
                issues: vec![issue],
            });
        }
    };
    let parsed = parse_staged_targets_section(&staged_targets_section, &config);
    if parsed.stages.is_empty() && parsed.issues.is_empty() {
        return None;
    }

    let achieved_count = if parsed.issues.is_empty() {
        summary.best_metric().map_or(0, |best_metric| {
            parsed
                .stages
                .iter()
                .take_while(|stage| stage.is_reached(best_metric))
                .count()
        })
    } else {
        0
    };

    Some(AutoresearchStageProgress {
        stages: parsed.stages,
        achieved_count,
        best_metric: summary.best_metric(),
        issues: parsed.issues,
    })
}

fn stage_validation_config(
    doc: &str,
    summary: &AutoresearchJournalSummary,
) -> Result<AutoresearchConfigEntry, String> {
    if let Some(config) = summary.config.clone() {
        return Ok(config);
    }
    parse_primary_metric_config(doc)
}

#[cfg(test)]
fn parse_staged_targets(doc: &str, config: &AutoresearchConfigEntry) -> ParsedStagedTargets {
    let Some(section) = extract_markdown_section(doc, STAGED_TARGETS_HEADING) else {
        return ParsedStagedTargets::default();
    };
    parse_staged_targets_section(&section, config)
}

fn parse_staged_targets_section(
    section: &str,
    config: &AutoresearchConfigEntry,
) -> ParsedStagedTargets {
    let mut parsed = ParsedStagedTargets::default();
    let mut stage_number = 0usize;
    for line in section.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        stage_number = stage_number.saturating_add(1);
        match parse_stage_line(trimmed, config) {
            Ok(stage) => parsed.stages.push(stage),
            Err(issue) => parsed.issues.push(format!(
                "staged target {stage_number} `{trimmed}` is invalid: {issue}"
            )),
        }
    }
    parsed
        .issues
        .extend(validate_stage_sequence(&parsed.stages, config));
    parsed
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

fn parse_primary_metric_config(doc: &str) -> Result<AutoresearchConfigEntry, String> {
    let Some(section) = extract_markdown_section(doc, PRIMARY_METRIC_HEADING) else {
        return Err(
            "Primary Metric section is required before staged targets can be validated".to_string(),
        );
    };

    let mut metric_name = None::<String>;
    let mut metric_unit = None::<String>;
    let mut direction = None::<MetricDirection>;

    for raw_line in section.lines() {
        let trimmed = strip_list_marker(raw_line.trim());
        if trimmed.is_empty() {
            continue;
        }

        if let Some((key, value)) = parse_primary_metric_field(trimmed) {
            match key.as_str() {
                "name" | "metric" | "metricname" => {
                    metric_name = Some(value.to_string());
                }
                "unit" | "metricunit" => {
                    metric_unit = Some(value.to_string());
                }
                "direction" => {
                    direction = parse_direction_value(value);
                }
                _ => {}
            }
            continue;
        }

        if direction.is_none() {
            direction = parse_direction_value(trimmed);
        }
    }

    let metric_name = metric_name.ok_or_else(|| {
        "Primary Metric section must define a metric name before staged targets can be validated"
            .to_string()
    })?;
    let direction = direction.ok_or_else(|| {
        "Primary Metric section must define `Direction: lower` or `Direction: higher` before staged targets can be validated"
            .to_string()
    })?;

    Ok(AutoresearchConfigEntry {
        entry_type: "config".to_string(),
        name: metric_name.clone(),
        metric_name,
        metric_unit: metric_unit.unwrap_or_default(),
        direction,
    })
}

fn parse_primary_metric_field(line: &str) -> Option<(String, &str)> {
    let (raw_key, raw_value) = line.split_once(':').or_else(|| line.split_once('='))?;
    let key = raw_key
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '_' && *ch != '-')
        .collect::<String>()
        .to_ascii_lowercase();
    let value = raw_value.trim().trim_matches('`');
    (!value.is_empty()).then_some((key, value))
}

fn parse_direction_value(value: &str) -> Option<MetricDirection> {
    let normalized = value.trim().trim_matches('`').to_ascii_lowercase();
    if normalized.starts_with("lower") || normalized.contains("lower is better") {
        Some(MetricDirection::Lower)
    } else if normalized.starts_with("higher") || normalized.contains("higher is better") {
        Some(MetricDirection::Higher)
    } else {
        None
    }
}

fn parse_markdown_heading(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with('#') {
        return None;
    }
    let heading = trimmed.trim_start_matches('#').trim();
    (!heading.is_empty()).then_some(heading)
}

fn parse_stage_line(
    line: &str,
    config: &AutoresearchConfigEntry,
) -> Result<AutoresearchStageTarget, String> {
    let captures = STAGE_LINE_RE.captures(line.trim()).ok_or_else(|| {
        "expected a threshold expression like `- latency_ms <= 500 ms`".to_string()
    })?;
    let metric_name = captures.name("metric").map(|metric| metric.as_str());
    if let Some(metric_name) = metric_name
        && metric_name != config.metric_name
    {
        return Err(format!(
            "it targets `{metric_name}`, but staged targets must use the primary metric `{}`",
            config.metric_name
        ));
    }

    let comparator = match captures
        .name("op")
        .ok_or_else(|| "missing comparator".to_string())?
        .as_str()
    {
        "<" => StageComparator::LessThan,
        "<=" => StageComparator::LessThanOrEqual,
        ">" => StageComparator::GreaterThan,
        ">=" => StageComparator::GreaterThanOrEqual,
        _ => return Err("unsupported comparator".to_string()),
    };
    if !comparator_matches_direction(comparator, config.direction) {
        return Err(format!(
            "comparator `{}` does not match the primary metric direction `{}`",
            comparator_symbol(comparator),
            metric_direction_label(config.direction)
        ));
    }
    let threshold = captures
        .name("value")
        .ok_or_else(|| "missing numeric threshold".to_string())?
        .as_str()
        .parse::<f64>()
        .map_err(|_| "threshold is not a valid number".to_string())?;
    let unit = captures.name("unit").map(|unit| unit.as_str());
    let threshold = convert_threshold(threshold, unit, &config.metric_unit)
        .map_err(|issue| format!("{issue} (expected `{}` units)", config.metric_unit))?;
    Ok(AutoresearchStageTarget {
        display: stage_display(line, &config.metric_name, comparator, threshold, unit),
        threshold,
        comparator,
    })
}

fn validate_stage_sequence(
    stages: &[AutoresearchStageTarget],
    config: &AutoresearchConfigEntry,
) -> Vec<String> {
    let mut issues = Vec::new();
    for pair in stages.windows(2) {
        let [previous, current] = pair else {
            continue;
        };
        if !current.is_same_or_harder_than(previous, config.direction) {
            issues.push(format!(
                "staged targets must be ordered from easier to harder on `{}`; `{}` cannot follow `{}`",
                config.metric_name, current.display, previous.display
            ));
        }
    }
    issues
}

fn comparator_matches_direction(comparator: StageComparator, direction: MetricDirection) -> bool {
    matches!(
        (comparator, direction),
        (StageComparator::LessThan, MetricDirection::Lower)
            | (StageComparator::LessThanOrEqual, MetricDirection::Lower)
            | (StageComparator::GreaterThan, MetricDirection::Higher)
            | (StageComparator::GreaterThanOrEqual, MetricDirection::Higher)
    )
}

fn comparator_symbol(comparator: StageComparator) -> &'static str {
    match comparator {
        StageComparator::LessThan => "<",
        StageComparator::LessThanOrEqual => "<=",
        StageComparator::GreaterThan => ">",
        StageComparator::GreaterThanOrEqual => ">=",
    }
}

fn metric_direction_label(direction: MetricDirection) -> &'static str {
    match direction {
        MetricDirection::Lower => "lower",
        MetricDirection::Higher => "higher",
    }
}

fn stage_display(
    line: &str,
    metric_name: &str,
    comparator: StageComparator,
    threshold: f64,
    unit: Option<&str>,
) -> String {
    let stripped = strip_stage_prefix(line);
    if !stripped.is_empty() {
        return stripped.to_string();
    }

    let comparator = match comparator {
        StageComparator::LessThan => "<",
        StageComparator::LessThanOrEqual => "<=",
        StageComparator::GreaterThan => ">",
        StageComparator::GreaterThanOrEqual => ">=",
    };
    if let Some(unit) = unit {
        format!(
            "{metric_name} {comparator} {} {unit}",
            format_metric(threshold)
        )
    } else {
        format!("{metric_name} {comparator} {}", format_metric(threshold))
    }
}

fn strip_stage_prefix(line: &str) -> &str {
    strip_list_marker(line)
}

fn strip_list_marker(line: &str) -> &str {
    let trimmed = line.trim();
    if let Some(stripped) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
    {
        return stripped.trim();
    }

    let mut digits = 0usize;
    for ch in trimmed.chars() {
        if ch.is_ascii_digit() {
            digits = digits.saturating_add(1);
        } else {
            break;
        }
    }
    if digits == 0 {
        return trimmed;
    }

    let remainder = &trimmed[digits..];
    if let Some(stripped) = remainder
        .strip_prefix(". ")
        .or_else(|| remainder.strip_prefix(") "))
    {
        return stripped.trim();
    }
    trimmed
}

fn same_metric(left: f64, right: f64) -> bool {
    let scale = left.abs().max(right.abs()).max(1.0);
    (left - right).abs() <= scale * 1e-9
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum UnitKind {
    Time,
    DataSize,
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct UnitSpec {
    kind: UnitKind,
    factor: f64,
}

fn strictness_rank(comparator: StageComparator) -> u8 {
    match comparator {
        StageComparator::LessThan | StageComparator::GreaterThan => 1,
        StageComparator::LessThanOrEqual | StageComparator::GreaterThanOrEqual => 0,
    }
}

fn convert_threshold(value: f64, from_unit: Option<&str>, to_unit: &str) -> Result<f64, String> {
    let from_unit = from_unit.map(str::trim).filter(|unit| !unit.is_empty());
    let to_unit = to_unit.trim();
    if from_unit.is_none() {
        return Ok(value);
    }
    let from_unit = from_unit.ok_or_else(|| "missing source unit".to_string())?;
    if to_unit.is_empty() {
        return Err(format!(
            "unit `{from_unit}` cannot be used because the primary metric is unitless"
        ));
    }
    if from_unit.eq_ignore_ascii_case(to_unit) {
        return Ok(value);
    }
    let from_spec = parse_unit_spec(from_unit)
        .ok_or_else(|| format!("unsupported staged-target unit `{from_unit}`"))?;
    let to_spec =
        parse_unit_spec(to_unit).ok_or_else(|| format!("unsupported metric unit `{to_unit}`"))?;
    if from_spec.kind != to_spec.kind {
        return Err(format!(
            "unit `{from_unit}` cannot be converted into `{to_unit}`"
        ));
    }
    Ok(value * from_spec.factor / to_spec.factor)
}

fn parse_unit_spec(unit: &str) -> Option<UnitSpec> {
    let normalized = unit.trim().to_ascii_lowercase();
    let spec = match normalized.as_str() {
        "ns" | "nanosecond" | "nanoseconds" => UnitSpec {
            kind: UnitKind::Time,
            factor: 1e-9,
        },
        "us" | "µs" | "μs" | "microsecond" | "microseconds" => UnitSpec {
            kind: UnitKind::Time,
            factor: 1e-6,
        },
        "ms" | "millisecond" | "milliseconds" => UnitSpec {
            kind: UnitKind::Time,
            factor: 1e-3,
        },
        "s" | "sec" | "secs" | "second" | "seconds" => UnitSpec {
            kind: UnitKind::Time,
            factor: 1.0,
        },
        "m" | "min" | "mins" | "minute" | "minutes" => UnitSpec {
            kind: UnitKind::Time,
            factor: 60.0,
        },
        "h" | "hr" | "hrs" | "hour" | "hours" => UnitSpec {
            kind: UnitKind::Time,
            factor: 3600.0,
        },
        "b" | "byte" | "bytes" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1.0,
        },
        "kb" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1_000.0,
        },
        "mb" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1_000_000.0,
        },
        "gb" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1_000_000_000.0,
        },
        "tb" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1_000_000_000_000.0,
        },
        "kib" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1024.0,
        },
        "mib" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1024.0 * 1024.0,
        },
        "gib" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1024.0 * 1024.0 * 1024.0,
        },
        "tib" => UnitSpec {
            kind: UnitKind::DataSize,
            factor: 1024.0 * 1024.0 * 1024.0 * 1024.0,
        },
        "%" | "pct" | "percent" | "percentage" => UnitSpec {
            kind: UnitKind::Percent,
            factor: 1.0,
        },
        _ => return None,
    };
    Some(spec)
}

fn format_metric(metric: f64) -> String {
    if metric.fract() == 0.0 {
        format!("{metric:.0}")
    } else {
        format!("{metric:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slop_fork::autoresearch::AutoresearchExperimentEntry;
    use crate::slop_fork::autoresearch::AutoresearchExperimentStatus;
    use crate::slop_fork::autoresearch::AutoresearchJournal;
    use crate::slop_fork::autoresearch::MetricDirection;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    fn config(metric_name: &str, metric_unit: &str) -> AutoresearchConfigEntry {
        config_with_direction(metric_name, metric_unit, MetricDirection::Lower)
    }

    fn config_with_direction(
        metric_name: &str,
        metric_unit: &str,
        direction: MetricDirection,
    ) -> AutoresearchConfigEntry {
        AutoresearchConfigEntry {
            entry_type: "config".to_string(),
            name: "latency".to_string(),
            metric_name: metric_name.to_string(),
            metric_unit: metric_unit.to_string(),
            direction,
        }
    }

    #[test]
    fn load_stage_progress_tracks_current_active_stage() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 500 ms\n- latency_ms <= 400 ms\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
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
                description: "first target".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");

        let progress = load_stage_progress(dir.path(), &journal.summary()).expect("progress");
        assert_eq!(progress.total_stages(), 2);
        assert_eq!(progress.achieved_count, 1);
        assert_eq!(progress.active_stage_number(), Some(2));
        assert_eq!(
            progress.active_stage().map(|stage| stage.display.as_str()),
            Some("latency_ms <= 400 ms")
        );
    }

    #[test]
    fn load_stage_progress_marks_all_targets_reached() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 500 ms\n- latency_ms <= 400 ms\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
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
                metric: Some(390.0),
                metrics: Default::default(),
                status: AutoresearchExperimentStatus::Keep,
                description: "second target".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");

        let progress = load_stage_progress(dir.path(), &journal.summary()).expect("progress");
        assert_eq!(progress.achieved_count, 2);
        assert!(progress.all_reached());
        assert_eq!(progress.active_stage(), None);
    }

    #[test]
    fn parse_stage_line_ignores_other_metrics() {
        let err =
            parse_stage_line("- accuracy >= 85%", &config("latency_ms", "ms")).expect_err("error");
        assert!(err.contains("must use the primary metric `latency_ms`"));
    }

    #[test]
    fn parse_stage_line_handles_labeled_entries() {
        let parsed = parse_stage_line(
            "1. stage two: latency_ms <= 400 ms",
            &config("latency_ms", "ms"),
        )
        .expect("parsed");
        assert_eq!(parsed.comparator, StageComparator::LessThanOrEqual);
        assert_eq!(parsed.threshold, 400.0);
        assert_eq!(parsed.display, "stage two: latency_ms <= 400 ms");
    }

    #[test]
    fn load_stage_progress_converts_stage_units_into_metric_unit() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 0.5 s\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
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
                description: "converted threshold".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");

        let progress = load_stage_progress(dir.path(), &journal.summary()).expect("progress");
        assert_eq!(progress.achieved_count, 1);
        assert_eq!(
            progress.active_stage().map(|stage| stage.display.as_str()),
            None
        );
    }

    #[test]
    fn strict_stage_comparator_requires_strict_improvement() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms < 400 ms\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
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
                metric: Some(400.0),
                metrics: Default::default(),
                status: AutoresearchExperimentStatus::Keep,
                description: "equal threshold".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");

        let progress = load_stage_progress(dir.path(), &journal.summary()).expect("progress");
        assert_eq!(progress.achieved_count, 0);
        assert_eq!(
            progress.active_stage().map(|stage| stage.display.as_str()),
            Some("latency_ms < 400 ms")
        );
    }

    #[test]
    fn load_stage_progress_reports_out_of_order_targets() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 400 ms\n- latency_ms <= 500 ms\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
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
                description: "bad order".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");

        let progress = load_stage_progress(dir.path(), &journal.summary()).expect("progress");
        assert!(progress.has_issues());
        assert_eq!(progress.achieved_count, 0);
        assert_eq!(progress.active_stage(), None);
        assert!(
            progress
                .issue_summary()
                .contains("ordered from easier to harder")
        );
    }

    #[test]
    fn load_stage_progress_reports_unsupported_units() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 500 widgets\n",
        )
        .expect("write doc");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load journal");
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
                description: "bad unit".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");

        let progress = load_stage_progress(dir.path(), &journal.summary()).expect("progress");
        assert!(progress.has_issues());
        assert_eq!(progress.achieved_count, 0);
        assert_eq!(progress.total_stages(), 0);
        assert!(
            progress
                .issue_summary()
                .contains("unsupported staged-target unit `widgets`")
        );
    }

    #[test]
    fn parse_stage_line_rejects_direction_mismatches() {
        let err = parse_stage_line(
            "- latency_ms >= 500 ms",
            &config_with_direction("latency_ms", "ms", MetricDirection::Lower),
        )
        .expect_err("error");
        assert!(err.contains("does not match the primary metric direction"));
    }

    #[test]
    fn parse_stage_line_rejects_unitful_target_for_unitless_metric() {
        let err = parse_stage_line(
            "- accuracy >= 95 %",
            &config_with_direction("accuracy", "", MetricDirection::Higher),
        )
        .expect_err("error");
        assert!(err.contains("primary metric is unitless"));
    }

    #[test]
    fn staged_target_issue_numbering_ignores_blank_lines() {
        let parsed = parse_staged_targets(
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 500 ms\n\n- nope\n",
            &config("latency_ms", "ms"),
        );
        assert_eq!(
            parsed.issues,
            vec![
                "staged target 2 `- nope` is invalid: expected a threshold expression like `- latency_ms <= 500 ms`"
                    .to_string()
            ]
        );
    }

    #[test]
    fn load_stage_progress_uses_primary_metric_section_before_journal_init() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Primary Metric\n- Name: latency_ms\n- Unit: ms\n- Direction: lower\n\n## Staged Targets\n- latency_ms <= 500 ms\n- latency_ms <= 400 ms\n",
        )
        .expect("write doc");

        let progress = load_stage_progress(
            dir.path(),
            &AutoresearchJournal::load(dir.path())
                .expect("load journal")
                .summary(),
        )
        .expect("progress");

        assert_eq!(progress.total_stages(), 2);
        assert_eq!(progress.achieved_count, 0);
        assert_eq!(
            progress.active_stage().map(|stage| stage.display.as_str()),
            Some("latency_ms <= 500 ms")
        );
    }

    #[test]
    fn load_stage_progress_reports_missing_primary_metric_before_journal_init() {
        let dir = tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(AUTORESEARCH_DOC_FILE),
            "# Goal\nx\n\n## Staged Targets\n- latency_ms <= 500 ms\n",
        )
        .expect("write doc");

        let progress = load_stage_progress(
            dir.path(),
            &AutoresearchJournal::load(dir.path())
                .expect("load journal")
                .summary(),
        )
        .expect("progress");

        assert!(progress.has_issues());
        assert_eq!(progress.achieved_count, 0);
        assert_eq!(progress.active_stage(), None);
        assert!(
            progress
                .issue_summary()
                .contains("Primary Metric section is required")
        );
    }
}
