use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::AUTORESEARCH_JOURNAL_FILE;
use super::AutoresearchDiscoveryReason;
use super::controller::PortfolioRefreshTriggerKind;
use super::runtime::AutoresearchCycleKind;
use super::validation::AutoresearchValidationEntry;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MetricDirection {
    #[default]
    Lower,
    Higher,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchConfigEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub name: String,
    pub metric_name: String,
    pub metric_unit: String,
    pub direction: MetricDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchExperimentStatus {
    Keep,
    Discard,
    Crash,
    ChecksFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchExperimentEntry {
    pub run: u32,
    pub commit: String,
    #[serde(default)]
    pub approach_id: Option<String>,
    #[serde(default)]
    pub metric: Option<f64>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub status: AutoresearchExperimentStatus,
    pub description: String,
    pub timestamp: i64,
    #[serde(default)]
    pub segment: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchApproachStatus {
    Proposed,
    Planned,
    Active,
    Tested,
    Promising,
    DeadEnd,
    Winner,
    Archived,
}

impl AutoresearchApproachStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(Self::Proposed),
            "planned" => Some(Self::Planned),
            "active" => Some(Self::Active),
            "tested" => Some(Self::Tested),
            "promising" => Some(Self::Promising),
            "dead_end" => Some(Self::DeadEnd),
            "winner" => Some(Self::Winner),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Planned => "planned",
            Self::Active => "active",
            Self::Tested => "tested",
            Self::Promising => "promising",
            Self::DeadEnd => "dead_end",
            Self::Winner => "winner",
            Self::Archived => "archived",
        }
    }

    pub fn sort_rank(self) -> u8 {
        match self {
            Self::Winner => 0,
            Self::Active => 1,
            Self::Promising => 2,
            Self::Tested => 3,
            Self::Planned => 4,
            Self::Proposed => 5,
            Self::Archived => 6,
            Self::DeadEnd => 7,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchApproachEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub approach_id: String,
    pub title: String,
    pub family: String,
    pub status: AutoresearchApproachStatus,
    pub summary: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub risks: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub parent_approach_id: Option<String>,
    #[serde(default)]
    pub synthesis_parent_approach_ids: Vec<String>,
    pub timestamp: i64,
    #[serde(default)]
    pub segment: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchDiscoveryEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub reason: AutoresearchDiscoveryReason,
    #[serde(default)]
    pub focus: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub recommendations: Vec<String>,
    #[serde(default)]
    pub unknowns: Vec<String>,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub dead_ends: Vec<String>,
    pub timestamp: i64,
    #[serde(default)]
    pub segment: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchSelectionDecision {
    None,
    SwitchActiveApproach,
    KeepActiveApproach,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutoresearchPortfolioRefreshDecision {
    NotApplicable,
    Queued,
    Waiting,
    CoolingDown,
    BootstrapComplete,
    Suppressed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchSynthesisSuggestion {
    pub left_approach_id: String,
    pub right_approach_id: String,
    #[serde(default)]
    pub synthesized_approach_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoresearchControllerDecisionEntry {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub cycle_kind: AutoresearchCycleKind,
    pub selection_decision: AutoresearchSelectionDecision,
    #[serde(default)]
    pub selection_reasons: Vec<String>,
    #[serde(default)]
    pub active_approach_id: Option<String>,
    #[serde(default)]
    pub recommended_approach_id: Option<String>,
    pub portfolio_refresh_decision: AutoresearchPortfolioRefreshDecision,
    #[serde(default)]
    pub portfolio_refresh_trigger: Option<PortfolioRefreshTriggerKind>,
    #[serde(default)]
    pub portfolio_refresh_reasons: Vec<String>,
    #[serde(default)]
    pub synthesis_suggestion: Option<AutoresearchSynthesisSuggestion>,
    pub summary: String,
    pub timestamp: i64,
    #[serde(default)]
    pub segment: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchJournal {
    pub path: PathBuf,
    pub entries: Vec<AutoresearchJournalEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AutoresearchJournalEntry {
    Config(AutoresearchConfigEntry),
    Approach(AutoresearchApproachEntry),
    Discovery(AutoresearchDiscoveryEntry),
    Controller(AutoresearchControllerDecisionEntry),
    Validation(AutoresearchValidationEntry),
    Experiment(AutoresearchExperimentEntry),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchApproachSummary {
    pub latest: AutoresearchApproachEntry,
    pub total_runs: usize,
    pub keep_count: usize,
    pub best_metric: Option<f64>,
    pub last_metric: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchJournalSummary {
    pub current_segment: u32,
    pub config: Option<AutoresearchConfigEntry>,
    pub total_runs: u32,
    pub current_segment_approaches: Vec<AutoresearchApproachSummary>,
    pub current_segment_discoveries: Vec<AutoresearchDiscoveryEntry>,
    pub current_segment_controller_decisions: Vec<AutoresearchControllerDecisionEntry>,
    pub current_segment_validations: Vec<AutoresearchValidationEntry>,
    pub current_segment_runs: Vec<AutoresearchExperimentEntry>,
}

impl AutoresearchJournalSummary {
    pub fn best_metric(&self) -> Option<f64> {
        let direction = self.config.as_ref().map(|config| config.direction)?;
        self.current_segment_runs
            .iter()
            .filter(|entry| entry.status == AutoresearchExperimentStatus::Keep)
            .filter_map(|entry| entry.metric)
            .reduce(|left, right| match direction {
                MetricDirection::Lower => left.min(right),
                MetricDirection::Higher => left.max(right),
            })
    }

    pub fn baseline_metric(&self) -> Option<f64> {
        self.current_segment_runs
            .iter()
            .find_map(|entry| entry.metric)
    }

    pub fn keep_count(&self) -> usize {
        self.current_segment_runs
            .iter()
            .filter(|entry| entry.status == AutoresearchExperimentStatus::Keep)
            .count()
    }

    pub fn discard_count(&self) -> usize {
        self.current_segment_runs
            .iter()
            .filter(|entry| entry.status == AutoresearchExperimentStatus::Discard)
            .count()
    }

    pub fn crash_count(&self) -> usize {
        self.current_segment_runs
            .iter()
            .filter(|entry| entry.status == AutoresearchExperimentStatus::Crash)
            .count()
    }

    pub fn checks_failed_count(&self) -> usize {
        self.current_segment_runs
            .iter()
            .filter(|entry| entry.status == AutoresearchExperimentStatus::ChecksFailed)
            .count()
    }

    pub fn discovery_count(&self) -> usize {
        self.current_segment_discoveries.len()
    }

    pub fn approach_count(&self) -> usize {
        self.current_segment_approaches.len()
    }

    pub fn family_count(&self) -> usize {
        let mut families = self
            .current_segment_approaches
            .iter()
            .map(|summary| summary.latest.family.clone())
            .collect::<Vec<_>>();
        families.sort();
        families.dedup();
        families.len()
    }

    pub fn active_approach(&self) -> Option<&AutoresearchApproachSummary> {
        self.current_segment_approaches
            .iter()
            .find(|summary| summary.latest.status == AutoresearchApproachStatus::Active)
            .or_else(|| {
                self.current_segment_approaches
                    .iter()
                    .find(|summary| summary.latest.status == AutoresearchApproachStatus::Winner)
            })
    }

    pub fn viable_approach_count(&self) -> usize {
        self.current_segment_approaches
            .iter()
            .filter(|summary| {
                matches!(
                    summary.latest.status,
                    AutoresearchApproachStatus::Active
                        | AutoresearchApproachStatus::Promising
                        | AutoresearchApproachStatus::Winner
                )
            })
            .count()
    }

    pub fn latest_approach(&self, approach_id: &str) -> Option<&AutoresearchApproachSummary> {
        self.current_segment_approaches
            .iter()
            .find(|summary| summary.latest.approach_id == approach_id)
    }

    pub fn latest_live_synthesis_approach(
        &self,
        left_approach_id: &str,
        right_approach_id: &str,
    ) -> Option<&AutoresearchApproachSummary> {
        self.current_segment_approaches
            .iter()
            .filter(|summary| {
                !matches!(
                    summary.latest.status,
                    AutoresearchApproachStatus::DeadEnd | AutoresearchApproachStatus::Archived
                ) && approach_matches_synthesis_lineage(
                    &summary.latest,
                    left_approach_id,
                    right_approach_id,
                )
            })
            .max_by_key(|summary| summary.latest.timestamp)
    }

    pub fn last_discovery(&self) -> Option<&AutoresearchDiscoveryEntry> {
        self.current_segment_discoveries.last()
    }

    pub fn last_controller_decision(&self) -> Option<&AutoresearchControllerDecisionEntry> {
        self.current_segment_controller_decisions.last()
    }
}

impl AutoresearchJournal {
    pub fn load(workdir: &Path) -> std::io::Result<Self> {
        let path = workdir.join(AUTORESEARCH_JOURNAL_FILE);
        let mut entries = Vec::new();
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self { path, entries });
            }
            Err(err) => return Err(err),
        };

        let mut segment = 0_u32;
        let mut saw_any_entry = false;
        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let raw = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(raw) => raw,
                Err(_) => continue,
            };
            if raw.get("type").and_then(serde_json::Value::as_str) == Some("config") {
                let mut config: AutoresearchConfigEntry = match serde_json::from_value(raw) {
                    Ok(config) => config,
                    Err(_) => continue,
                };
                config.entry_type = "config".to_string();
                if saw_any_entry {
                    segment = segment.saturating_add(1);
                }
                saw_any_entry = true;
                entries.push(AutoresearchJournalEntry::Config(config));
                continue;
            }
            if raw.get("type").and_then(serde_json::Value::as_str) == Some("discovery") {
                let mut discovery: AutoresearchDiscoveryEntry = match serde_json::from_value(raw) {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                saw_any_entry = true;
                discovery.segment = segment;
                entries.push(AutoresearchJournalEntry::Discovery(discovery));
                continue;
            }
            if raw.get("type").and_then(serde_json::Value::as_str) == Some("approach") {
                let mut approach: AutoresearchApproachEntry = match serde_json::from_value(raw) {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                saw_any_entry = true;
                approach.segment = segment;
                entries.push(AutoresearchJournalEntry::Approach(approach));
                continue;
            }
            if raw.get("type").and_then(serde_json::Value::as_str) == Some("controller") {
                let mut controller: AutoresearchControllerDecisionEntry =
                    match serde_json::from_value(raw) {
                        Ok(entry) => entry,
                        Err(_) => continue,
                    };
                saw_any_entry = true;
                controller.segment = segment;
                entries.push(AutoresearchJournalEntry::Controller(controller));
                continue;
            }
            if raw.get("type").and_then(serde_json::Value::as_str) == Some("validation") {
                let mut validation: AutoresearchValidationEntry = match serde_json::from_value(raw)
                {
                    Ok(entry) => entry,
                    Err(_) => continue,
                };
                saw_any_entry = true;
                validation.segment = segment;
                entries.push(AutoresearchJournalEntry::Validation(validation));
                continue;
            }

            let mut experiment: AutoresearchExperimentEntry = match serde_json::from_value(raw) {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            saw_any_entry = true;
            experiment.segment = segment;
            entries.push(AutoresearchJournalEntry::Experiment(experiment));
        }

        Ok(Self { path, entries })
    }

    pub fn summary(&self) -> AutoresearchJournalSummary {
        let mut current_segment = 0_u32;
        let mut config = None;
        let mut total_runs = 0_u32;
        let mut current_segment_approaches = Vec::new();
        let mut current_segment_discoveries = Vec::new();
        let mut current_segment_controller_decisions = Vec::new();
        let mut current_segment_validations = Vec::new();
        let mut current_segment_runs = Vec::new();
        let mut saw_any_entry = false;

        for entry in &self.entries {
            match entry {
                AutoresearchJournalEntry::Config(next_config) => {
                    if saw_any_entry {
                        current_segment = current_segment.saturating_add(1);
                    }
                    saw_any_entry = true;
                    config = Some(next_config.clone());
                    current_segment_approaches.clear();
                    current_segment_discoveries.clear();
                    current_segment_controller_decisions.clear();
                    current_segment_validations.clear();
                    current_segment_runs.clear();
                }
                AutoresearchJournalEntry::Approach(approach) => {
                    saw_any_entry = true;
                    current_segment = approach.segment;
                    upsert_approach_summary(&mut current_segment_approaches, approach.clone());
                }
                AutoresearchJournalEntry::Discovery(discovery) => {
                    saw_any_entry = true;
                    current_segment = discovery.segment;
                    current_segment_discoveries.push(discovery.clone());
                }
                AutoresearchJournalEntry::Controller(controller) => {
                    saw_any_entry = true;
                    current_segment = controller.segment;
                    current_segment_controller_decisions.push(controller.clone());
                }
                AutoresearchJournalEntry::Validation(validation) => {
                    saw_any_entry = true;
                    current_segment = validation.segment;
                    current_segment_validations.push(validation.clone());
                }
                AutoresearchJournalEntry::Experiment(experiment) => {
                    saw_any_entry = true;
                    total_runs = total_runs.max(experiment.run);
                    current_segment = experiment.segment;
                    apply_experiment_to_approaches(
                        &mut current_segment_approaches,
                        experiment,
                        config.as_ref().map(|entry| entry.direction),
                    );
                    current_segment_runs.push(experiment.clone());
                }
            }
        }

        current_segment_approaches.sort_by(|left, right| {
            let status_cmp = left
                .latest
                .status
                .sort_rank()
                .cmp(&right.latest.status.sort_rank());
            if status_cmp.is_ne() {
                return status_cmp;
            }
            let family_cmp = left.latest.family.cmp(&right.latest.family);
            if family_cmp.is_ne() {
                return family_cmp;
            }
            left.latest.title.cmp(&right.latest.title)
        });

        AutoresearchJournalSummary {
            current_segment,
            config,
            total_runs,
            current_segment_approaches,
            current_segment_discoveries,
            current_segment_controller_decisions,
            current_segment_validations,
            current_segment_runs,
        }
    }

    pub fn append_config(
        &mut self,
        name: String,
        metric_name: String,
        metric_unit: String,
        direction: MetricDirection,
    ) -> std::io::Result<AutoresearchConfigEntry> {
        let entry = AutoresearchConfigEntry {
            entry_type: "config".to_string(),
            name,
            metric_name,
            metric_unit,
            direction,
        };
        append_json_line(&self.path, &entry)?;
        self.entries
            .push(AutoresearchJournalEntry::Config(entry.clone()));
        Ok(entry)
    }

    pub fn append_experiment(
        &mut self,
        entry: AutoresearchExperimentEntry,
    ) -> std::io::Result<AutoresearchExperimentEntry> {
        append_json_line(&self.path, &entry)?;
        self.entries
            .push(AutoresearchJournalEntry::Experiment(entry.clone()));
        Ok(entry)
    }

    pub fn append_discovery(
        &mut self,
        entry: AutoresearchDiscoveryEntry,
    ) -> std::io::Result<AutoresearchDiscoveryEntry> {
        append_json_line(&self.path, &entry)?;
        self.entries
            .push(AutoresearchJournalEntry::Discovery(entry.clone()));
        Ok(entry)
    }

    pub fn append_approach(
        &mut self,
        entry: AutoresearchApproachEntry,
    ) -> std::io::Result<AutoresearchApproachEntry> {
        append_json_line(&self.path, &entry)?;
        self.entries
            .push(AutoresearchJournalEntry::Approach(entry.clone()));
        Ok(entry)
    }

    pub fn append_controller_decision(
        &mut self,
        entry: AutoresearchControllerDecisionEntry,
    ) -> std::io::Result<AutoresearchControllerDecisionEntry> {
        append_json_line(&self.path, &entry)?;
        self.entries
            .push(AutoresearchJournalEntry::Controller(entry.clone()));
        Ok(entry)
    }

    pub fn append_validation(
        &mut self,
        entry: AutoresearchValidationEntry,
    ) -> std::io::Result<AutoresearchValidationEntry> {
        append_json_line(&self.path, &entry)?;
        self.entries
            .push(AutoresearchJournalEntry::Validation(entry.clone()));
        Ok(entry)
    }

    pub fn remove_file(workdir: &Path) -> std::io::Result<()> {
        let path = workdir.join(AUTORESEARCH_JOURNAL_FILE);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
}

fn upsert_approach_summary(
    approaches: &mut Vec<AutoresearchApproachSummary>,
    approach: AutoresearchApproachEntry,
) {
    if let Some(existing) = approaches
        .iter_mut()
        .find(|summary| summary.latest.approach_id == approach.approach_id)
    {
        existing.latest = approach;
    } else {
        approaches.push(AutoresearchApproachSummary {
            latest: approach,
            total_runs: 0,
            keep_count: 0,
            best_metric: None,
            last_metric: None,
        });
    }
}

fn apply_experiment_to_approaches(
    approaches: &mut [AutoresearchApproachSummary],
    experiment: &AutoresearchExperimentEntry,
    direction: Option<MetricDirection>,
) {
    let Some(approach_id) = experiment.approach_id.as_deref() else {
        return;
    };
    let Some(summary) = approaches
        .iter_mut()
        .find(|summary| summary.latest.approach_id == approach_id)
    else {
        return;
    };
    summary.total_runs = summary.total_runs.saturating_add(1);
    summary.last_metric = experiment.metric;
    if experiment.status == AutoresearchExperimentStatus::Keep {
        summary.keep_count = summary.keep_count.saturating_add(1);
        if let (Some(metric), Some(direction)) = (experiment.metric, direction) {
            summary.best_metric = Some(match summary.best_metric {
                Some(current_best) => match direction {
                    MetricDirection::Lower => current_best.min(metric),
                    MetricDirection::Higher => current_best.max(metric),
                },
                None => metric,
            });
        }
    }
}

fn approach_matches_synthesis_lineage(
    approach: &AutoresearchApproachEntry,
    left_approach_id: &str,
    right_approach_id: &str,
) -> bool {
    if approach.synthesis_parent_approach_ids.len() != 2 {
        return false;
    }
    let mut actual = approach
        .synthesis_parent_approach_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = [left_approach_id, right_approach_id];
    expected.sort_unstable();
    actual[0] == expected[0] && actual[1] == expected[1]
}

fn append_json_line<T>(path: &Path, value: &T) -> std::io::Result<()>
where
    T: Serialize,
{
    let serialized = serde_json::to_string(value).map_err(|err| {
        std::io::Error::other(format!("failed to serialize autoresearch entry: {err}"))
    })?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    writeln!(file, "{serialized}")?;
    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn summary_tracks_latest_config_segment() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "first".to_string(),
                "seconds".to_string(),
                "s".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 1,
                commit: "abc1234".to_string(),
                approach_id: None,
                metric: Some(12.0),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "baseline".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");
        journal
            .append_config(
                "second".to_string(),
                "loss".to_string(),
                "".to_string(),
                MetricDirection::Higher,
            )
            .expect("config");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 2,
                commit: "def5678".to_string(),
                approach_id: None,
                metric: Some(0.9),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "baseline".to_string(),
                timestamp: 2,
                segment: 1,
            })
            .expect("experiment");

        let reloaded = AutoresearchJournal::load(dir.path()).expect("reload");
        let summary = reloaded.summary();
        assert_eq!(summary.current_segment, 1);
        assert_eq!(summary.total_runs, 2);
        assert_eq!(summary.config.as_ref().expect("config").metric_name, "loss");
        assert!(summary.current_segment_discoveries.is_empty());
        assert!(summary.current_segment_controller_decisions.is_empty());
        assert_eq!(summary.current_segment_runs.len(), 1);
        assert_eq!(summary.best_metric(), Some(0.9));
    }

    #[test]
    fn reload_keeps_segment_numbers_when_configs_repeat_without_runs() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "first".to_string(),
                "seconds".to_string(),
                "s".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_config(
                "second".to_string(),
                "loss".to_string(),
                "".to_string(),
                MetricDirection::Higher,
            )
            .expect("config");
        let summary = journal.summary();
        assert_eq!(summary.current_segment, 1);
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 1,
                commit: "def5678".to_string(),
                approach_id: None,
                metric: Some(0.9),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "baseline".to_string(),
                timestamp: 2,
                segment: summary.current_segment,
            })
            .expect("experiment");

        let reloaded = AutoresearchJournal::load(dir.path()).expect("reload");
        let summary = reloaded.summary();
        assert_eq!(summary.current_segment, 1);
        assert_eq!(summary.current_segment_runs.len(), 1);
        assert_eq!(summary.current_segment_runs[0].segment, 1);
    }

    #[test]
    fn baseline_skips_runs_without_metric() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "first".to_string(),
                "seconds".to_string(),
                "s".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 1,
                commit: "abc1234".to_string(),
                approach_id: None,
                metric: None,
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Crash,
                description: "no metric".to_string(),
                timestamp: 1,
                segment: 0,
            })
            .expect("experiment");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 2,
                commit: "def5678".to_string(),
                approach_id: None,
                metric: Some(12.0),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "baseline".to_string(),
                timestamp: 2,
                segment: 0,
            })
            .expect("experiment");

        let summary = journal.summary();
        assert_eq!(summary.baseline_metric(), Some(12.0));
        assert_eq!(summary.best_metric(), Some(12.0));
    }

    #[test]
    fn summary_tracks_discovery_entries_in_current_segment() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "session".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_discovery(AutoresearchDiscoveryEntry {
                entry_type: "discovery".to_string(),
                reason: AutoresearchDiscoveryReason::ArchitectureSearch,
                focus: Some("teacher-student OCR".to_string()),
                summary: "Found two plausible teacher-model directions.".to_string(),
                recommendations: vec!["Try a higher-capacity teacher.".to_string()],
                unknowns: vec!["Dataset label noise still unclear.".to_string()],
                sources: vec!["https://example.com/paper".to_string()],
                dead_ends: vec!["Heavy decoder beam search looks too slow.".to_string()],
                timestamp: 42,
                segment: 0,
            })
            .expect("discovery");

        let summary = journal.summary();
        assert_eq!(summary.discovery_count(), 1);
        assert_eq!(
            summary.last_discovery().expect("last discovery").reason,
            AutoresearchDiscoveryReason::ArchitectureSearch
        );
        assert_eq!(summary.current_segment_runs.len(), 0);
    }

    #[test]
    fn summary_tracks_controller_decisions_in_current_segment() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "session".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_controller_decision(AutoresearchControllerDecisionEntry {
                entry_type: "controller".to_string(),
                cycle_kind: AutoresearchCycleKind::Research,
                selection_decision: AutoresearchSelectionDecision::SwitchActiveApproach,
                selection_reasons: vec!["active approach looks stagnant".to_string()],
                active_approach_id: Some("approach-1".to_string()),
                recommended_approach_id: Some("approach-2".to_string()),
                portfolio_refresh_decision: AutoresearchPortfolioRefreshDecision::Waiting,
                portfolio_refresh_trigger: Some(PortfolioRefreshTriggerKind::LowDiversity),
                portfolio_refresh_reasons: vec![
                    "family count is below the configured minimum".to_string(),
                ],
                synthesis_suggestion: Some(AutoresearchSynthesisSuggestion {
                    left_approach_id: "approach-2".to_string(),
                    right_approach_id: "approach-3".to_string(),
                    synthesized_approach_id: Some("approach-4".to_string()),
                }),
                summary: "Switch to approach-2 and keep portfolio refresh waiting.".to_string(),
                timestamp: 42,
                segment: 0,
            })
            .expect("controller");

        let summary = journal.summary();
        assert_eq!(summary.current_segment_controller_decisions.len(), 1);
        assert_eq!(
            summary
                .last_controller_decision()
                .expect("last controller decision")
                .selection_decision,
            AutoresearchSelectionDecision::SwitchActiveApproach
        );
        assert_eq!(
            summary
                .last_controller_decision()
                .expect("last controller decision")
                .portfolio_refresh_trigger,
            Some(PortfolioRefreshTriggerKind::LowDiversity)
        );
    }

    #[test]
    fn summary_tracks_approach_portfolio_metrics() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "session".to_string(),
                "latency_ms".to_string(),
                "ms".to_string(),
                MetricDirection::Lower,
            )
            .expect("config");
        journal
            .append_approach(AutoresearchApproachEntry {
                entry_type: "approach".to_string(),
                approach_id: "approach-1".to_string(),
                title: "Teacher model distillation".to_string(),
                family: "distillation".to_string(),
                status: AutoresearchApproachStatus::Active,
                summary: "Use a larger teacher to guide a lightweight OCR student.".to_string(),
                rationale: "Expected to improve accuracy without expanding inference cost."
                    .to_string(),
                risks: vec!["Teacher quality may saturate.".to_string()],
                sources: vec!["https://example.com/distill".to_string()],
                parent_approach_id: None,
                synthesis_parent_approach_ids: Vec::new(),
                timestamp: 10,
                segment: 0,
            })
            .expect("approach");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 1,
                commit: "abc1234".to_string(),
                approach_id: Some("approach-1".to_string()),
                metric: Some(480.0),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Keep,
                description: "teacher distillation baseline".to_string(),
                timestamp: 11,
                segment: 0,
            })
            .expect("experiment");
        journal
            .append_approach(AutoresearchApproachEntry {
                entry_type: "approach".to_string(),
                approach_id: "approach-2".to_string(),
                title: "Retrieval-heavy decoder".to_string(),
                family: "retrieval".to_string(),
                status: AutoresearchApproachStatus::DeadEnd,
                summary: "Attach a heavy retrieval stage before decoding.".to_string(),
                rationale: "Might recover rare glyphs.".to_string(),
                risks: vec!["Likely too slow.".to_string()],
                sources: Vec::new(),
                parent_approach_id: None,
                synthesis_parent_approach_ids: Vec::new(),
                timestamp: 12,
                segment: 0,
            })
            .expect("approach");
        journal
            .append_experiment(AutoresearchExperimentEntry {
                run: 2,
                commit: "def5678".to_string(),
                approach_id: Some("approach-2".to_string()),
                metric: Some(620.0),
                metrics: BTreeMap::new(),
                status: AutoresearchExperimentStatus::Discard,
                description: "retrieval prototype".to_string(),
                timestamp: 13,
                segment: 0,
            })
            .expect("experiment");

        let summary = journal.summary();
        assert_eq!(summary.approach_count(), 2);
        assert_eq!(summary.family_count(), 2);
        assert_eq!(summary.viable_approach_count(), 1);
        assert_eq!(
            summary
                .active_approach()
                .map(|entry| entry.latest.approach_id.as_str()),
            Some("approach-1")
        );
        assert_eq!(
            summary
                .latest_approach("approach-1")
                .expect("approach-1 summary")
                .best_metric,
            Some(480.0)
        );
        assert_eq!(
            summary.current_segment_approaches[0].latest.approach_id,
            "approach-1"
        );
    }

    #[test]
    fn latest_live_synthesis_approach_uses_parent_pair() {
        let dir = tempdir().expect("tempdir");
        let mut journal = AutoresearchJournal::load(dir.path()).expect("load");
        journal
            .append_config(
                "session".to_string(),
                "score".to_string(),
                String::new(),
                MetricDirection::Higher,
            )
            .expect("config");
        journal
            .append_approach(AutoresearchApproachEntry {
                entry_type: "approach".to_string(),
                approach_id: "approach-4".to_string(),
                title: "Synthesis: approach-2 + approach-3".to_string(),
                family: "retrieval+distillation".to_string(),
                status: AutoresearchApproachStatus::Promising,
                summary: "Reusable synthesis branch".to_string(),
                rationale: String::new(),
                risks: Vec::new(),
                sources: Vec::new(),
                parent_approach_id: Some("approach-2".to_string()),
                synthesis_parent_approach_ids: vec![
                    "approach-2".to_string(),
                    "approach-3".to_string(),
                ],
                timestamp: 20,
                segment: 0,
            })
            .expect("approach");

        let summary = journal.summary();
        assert_eq!(
            summary
                .latest_live_synthesis_approach("approach-3", "approach-2")
                .expect("synthesis")
                .latest
                .approach_id,
            "approach-4"
        );
    }
}
