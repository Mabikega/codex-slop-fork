use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::AUTORESEARCH_JOURNAL_FILE;
use super::AutoresearchDiscoveryReason;

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
    pub metric: Option<f64>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    pub status: AutoresearchExperimentStatus,
    pub description: String,
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

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchJournal {
    pub path: PathBuf,
    pub entries: Vec<AutoresearchJournalEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AutoresearchJournalEntry {
    Config(AutoresearchConfigEntry),
    Discovery(AutoresearchDiscoveryEntry),
    Experiment(AutoresearchExperimentEntry),
}

#[derive(Debug, Clone, PartialEq)]
pub struct AutoresearchJournalSummary {
    pub current_segment: u32,
    pub config: Option<AutoresearchConfigEntry>,
    pub total_runs: u32,
    pub current_segment_discoveries: Vec<AutoresearchDiscoveryEntry>,
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

    pub fn last_discovery(&self) -> Option<&AutoresearchDiscoveryEntry> {
        self.current_segment_discoveries.last()
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
        let mut current_segment_discoveries = Vec::new();
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
                    current_segment_discoveries.clear();
                    current_segment_runs.clear();
                }
                AutoresearchJournalEntry::Discovery(discovery) => {
                    saw_any_entry = true;
                    current_segment = discovery.segment;
                    current_segment_discoveries.push(discovery.clone());
                }
                AutoresearchJournalEntry::Experiment(experiment) => {
                    saw_any_entry = true;
                    total_runs = total_runs.max(experiment.run);
                    current_segment = experiment.segment;
                    current_segment_runs.push(experiment.clone());
                }
            }
        }

        AutoresearchJournalSummary {
            current_segment,
            config,
            total_runs,
            current_segment_discoveries,
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

    pub fn remove_file(workdir: &Path) -> std::io::Result<()> {
        let path = workdir.join(AUTORESEARCH_JOURNAL_FILE);
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(err),
        }
    }
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
}
