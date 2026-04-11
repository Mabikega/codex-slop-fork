use crate::config::Config;
use codex_exec_server::ExecutorFileSystem;
use codex_utils_absolute_path::AbsolutePathBuf;
use dunce::canonicalize as normalize_path;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::path::PathBuf;

use crate::slop_fork::automation::AutomationScope;
use codex_git_utils::resolve_root_git_project_for_trust;

const SLOP_FORK_CONFIG_FILE: &str = "config-slop-fork.toml";

fn legacy_config_filename() -> String {
    ["codex", "alt", "fork.toml"].join("-")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlopForkConfig {
    pub auto_switch_accounts_on_rate_limit: bool,
    pub follow_external_account_switches: bool,
    pub api_key_fallback_on_all_accounts_limited: bool,
    pub auto_start_five_hour_quota: bool,
    pub auto_start_weekly_quota: bool,
    pub show_account_numbers_instead_of_emails: bool,
    pub show_average_account_limits_in_status_line: bool,
    pub automation_enabled: bool,
    pub automation_default_scope: AutomationScope,
    pub automation_shell_timeout_ms: u64,
    pub automation_disable_notify_script: bool,
    pub automation_disable_terminal_notifications: bool,
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruction_files: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub projects: HashMap<String, SlopForkProjectConfig>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SlopForkProjectConfig {
    pub instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instruction_files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SlopForkInstructionOverlay {
    pub global_instructions: Option<String>,
    pub project_instructions: Option<String>,
    pub instruction_files: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SlopForkProjectDocOverlay {
    pub(crate) prefix_sections: Vec<String>,
    pub(crate) suffix_sections: Vec<String>,
    pub(crate) additional_filenames: Vec<String>,
}

impl Default for SlopForkConfig {
    fn default() -> Self {
        Self {
            auto_switch_accounts_on_rate_limit: true,
            follow_external_account_switches: false,
            api_key_fallback_on_all_accounts_limited: false,
            auto_start_five_hour_quota: false,
            auto_start_weekly_quota: false,
            show_account_numbers_instead_of_emails: false,
            show_average_account_limits_in_status_line: false,
            automation_enabled: true,
            automation_default_scope: AutomationScope::Session,
            automation_shell_timeout_ms: 30_000,
            automation_disable_notify_script: false,
            automation_disable_terminal_notifications: false,
            instructions: None,
            instruction_files: Vec::new(),
            projects: HashMap::new(),
        }
    }
}

pub fn slop_fork_config_path(codex_home: &Path) -> PathBuf {
    codex_home.join(SLOP_FORK_CONFIG_FILE)
}

fn legacy_slop_fork_config_path(codex_home: &Path) -> PathBuf {
    codex_home.join(legacy_config_filename())
}

fn parse_slop_fork_config(path: &Path, contents: &str) -> std::io::Result<SlopForkConfig> {
    toml::from_str(contents)
        .map_err(|err| std::io::Error::other(format!("failed to parse {}: {err}", path.display())))
}

fn read_slop_fork_config(
    codex_home: &Path,
    persist_missing: bool,
) -> std::io::Result<SlopForkConfig> {
    let path = slop_fork_config_path(codex_home);
    match std::fs::read_to_string(&path) {
        Ok(contents) => parse_slop_fork_config(&path, &contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let legacy_path = legacy_slop_fork_config_path(codex_home);
            if legacy_path.exists() {
                let contents = std::fs::read_to_string(&legacy_path)?;
                let config = parse_slop_fork_config(&legacy_path, &contents)?;
                if persist_missing {
                    save_slop_fork_config(codex_home, &config)?;
                }
                return Ok(config);
            }
            if persist_missing {
                let config = SlopForkConfig::default();
                save_slop_fork_config(codex_home, &config)?;
                return Ok(config);
            }
            Ok(SlopForkConfig::default())
        }
        Err(err) => Err(err),
    }
}

pub fn load_slop_fork_config(codex_home: &Path) -> std::io::Result<SlopForkConfig> {
    read_slop_fork_config(codex_home, /*persist_missing*/ true)
}

pub fn maybe_load_slop_fork_config(codex_home: &Path) -> std::io::Result<SlopForkConfig> {
    read_slop_fork_config(codex_home, /*persist_missing*/ false)
}

pub fn save_slop_fork_config(codex_home: &Path, config: &SlopForkConfig) -> std::io::Result<()> {
    let path = slop_fork_config_path(codex_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let serialized = toml::to_string_pretty(config)
        .map_err(|err| std::io::Error::other(format!("failed to serialize fork config: {err}")))?;
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(serialized.as_bytes())?;
    file.flush()?;
    Ok(())
}

pub fn update_slop_fork_config(
    codex_home: &Path,
    update: impl FnOnce(&mut SlopForkConfig),
) -> std::io::Result<SlopForkConfig> {
    let mut config = load_slop_fork_config(codex_home)?;
    update(&mut config);
    save_slop_fork_config(codex_home, &config)?;
    Ok(config)
}

fn trimmed_non_empty_string(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn extend_unique_instruction_files(target: &mut Vec<String>, files: &[String]) {
    for file in files {
        let trimmed = file.trim();
        if trimmed.is_empty() || target.iter().any(|existing| existing == trimmed) {
            continue;
        }
        target.push(trimmed.to_string());
    }
}

impl SlopForkConfig {
    pub fn get_active_project(&self, resolved_cwd: &Path) -> Option<SlopForkProjectConfig> {
        if let Some(project_config) = self
            .projects
            .get(&resolved_cwd.to_string_lossy().to_string())
        {
            return Some(project_config.clone());
        }

        if let Some(repo_root) = resolve_root_git_project_for_trust(resolved_cwd)
            && let Some(project_config) =
                self.projects.get(&repo_root.to_string_lossy().to_string())
        {
            return Some(project_config.clone());
        }

        None
    }

    pub(crate) fn instruction_overlay(&self, resolved_cwd: &Path) -> SlopForkInstructionOverlay {
        let project_config = self.get_active_project(resolved_cwd);
        let mut instruction_files = Vec::new();
        extend_unique_instruction_files(&mut instruction_files, &self.instruction_files);
        if let Some(project_config) = project_config.as_ref() {
            extend_unique_instruction_files(
                &mut instruction_files,
                &project_config.instruction_files,
            );
        }

        SlopForkInstructionOverlay {
            global_instructions: trimmed_non_empty_string(self.instructions.as_deref()),
            project_instructions: trimmed_non_empty_string(
                project_config
                    .as_ref()
                    .and_then(|project| project.instructions.as_deref()),
            ),
            instruction_files,
        }
    }
}

pub(crate) fn load_slop_fork_instruction_overlay(
    codex_home: &Path,
    resolved_cwd: &Path,
) -> std::io::Result<SlopForkInstructionOverlay> {
    Ok(maybe_load_slop_fork_config(codex_home)?.instruction_overlay(resolved_cwd))
}

pub(crate) fn load_project_doc_overlay(
    config: &Config,
) -> std::io::Result<SlopForkProjectDocOverlay> {
    let overlay = load_slop_fork_instruction_overlay(config.codex_home.as_path(), &config.cwd)?;
    let mut prefix_sections = Vec::new();
    if let Some(instructions) = overlay.global_instructions {
        prefix_sections.push(instructions);
    }
    let mut suffix_sections = Vec::new();
    if let Some(instructions) = overlay.project_instructions {
        suffix_sections.push(instructions);
    }
    Ok(SlopForkProjectDocOverlay {
        prefix_sections,
        suffix_sections,
        additional_filenames: overlay.instruction_files,
    })
}

pub(crate) async fn extend_project_doc_paths(
    config: &Config,
    fs: &dyn ExecutorFileSystem,
    sandbox_cwd: &AbsolutePathBuf,
    search_dirs: &[AbsolutePathBuf],
    primary_filenames: &[&str],
    additional_filenames: &[String],
    found: &mut Vec<AbsolutePathBuf>,
) -> std::io::Result<()> {
    let additional_candidate_filenames =
        additional_candidate_filenames(additional_filenames, primary_filenames);
    let mut seen_additional_paths: Vec<AbsolutePathBuf> = Vec::new();
    for directory in search_dirs {
        for name in &additional_candidate_filenames {
            let candidate_path = directory.join(name.as_str());
            if seen_additional_paths
                .iter()
                .any(|existing| existing == &candidate_path)
            {
                continue;
            }
            seen_additional_paths.push(candidate_path.clone());

            if let Some(additional_doc) =
                existing_additional_doc_path(config, fs, sandbox_cwd, &candidate_path).await?
            {
                push_unique_project_doc_path(found, additional_doc);
            }
        }
    }

    Ok(())
}

pub(crate) fn push_unique_project_doc_path(
    found: &mut Vec<AbsolutePathBuf>,
    path: AbsolutePathBuf,
) {
    if !found.iter().any(|existing| existing == &path) {
        found.push(path);
    }
}

fn project_doc_path_matches_readable_roots(
    config: &Config,
    sandbox_cwd: &Path,
    path: &Path,
) -> bool {
    let file_system_policy = &config.permissions.file_system_sandbox_policy;

    if file_system_policy.has_full_disk_read_access() {
        return true;
    }

    if file_system_policy
        .get_unreadable_roots_with_cwd(sandbox_cwd)
        .iter()
        .any(|root| path.starts_with(root.as_path()))
    {
        return false;
    }

    file_system_policy
        .get_readable_roots_with_cwd(sandbox_cwd)
        .iter()
        .any(|root| path.starts_with(root.as_path()))
}

fn additional_candidate_filenames(
    configured_filenames: &[String],
    primary_filenames: &[&str],
) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(configured_filenames.len());
    for candidate in configured_filenames {
        let candidate = candidate.trim();
        if candidate.is_empty()
            || primary_filenames.contains(&candidate)
            || names.iter().any(|existing| existing == candidate)
        {
            continue;
        }
        names.push(candidate.to_string());
    }
    names
}

async fn existing_additional_doc_path(
    config: &Config,
    fs: &dyn ExecutorFileSystem,
    sandbox_cwd: &AbsolutePathBuf,
    path: &AbsolutePathBuf,
) -> std::io::Result<Option<AbsolutePathBuf>> {
    if !project_doc_path_matches_readable_roots(config, sandbox_cwd, path) {
        tracing::warn!(
            "Skipping project doc `{}` because it is outside the filesystem sandbox policy.",
            path.display()
        );
        return Ok(None);
    }

    match fs.get_metadata(path).await {
        Ok(metadata) if !metadata.is_file => Ok(None),
        Ok(_) => {
            let resolved_path = normalize_path(path)
                .ok()
                .and_then(|resolved| AbsolutePathBuf::try_from(resolved).ok());
            if let Some(resolved_path) = resolved_path.as_ref()
                && !project_doc_path_matches_readable_roots(config, sandbox_cwd, resolved_path)
            {
                tracing::warn!(
                    "Skipping project doc `{}` because it resolves outside the filesystem sandbox policy.",
                    path.display()
                );
                return Ok(None);
            }

            Ok(Some(path.clone()))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;

    #[test]
    fn load_defaults_when_config_is_missing() -> anyhow::Result<()> {
        let dir = tempdir()?;
        assert_eq!(
            load_slop_fork_config(dir.path())?,
            SlopForkConfig::default()
        );
        assert!(slop_fork_config_path(dir.path()).exists());
        Ok(())
    }

    #[test]
    fn round_trips_config() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let config = SlopForkConfig {
            auto_switch_accounts_on_rate_limit: false,
            follow_external_account_switches: true,
            api_key_fallback_on_all_accounts_limited: true,
            auto_start_five_hour_quota: true,
            auto_start_weekly_quota: true,
            show_account_numbers_instead_of_emails: true,
            show_average_account_limits_in_status_line: true,
            automation_enabled: false,
            automation_default_scope: AutomationScope::Global,
            automation_shell_timeout_ms: 5_000,
            automation_disable_notify_script: true,
            automation_disable_terminal_notifications: true,
            instructions: Some("Follow CLAUDE.md too".to_string()),
            instruction_files: vec!["CLAUDE.md".to_string()],
            projects: HashMap::from([(
                "/tmp/project".to_string(),
                SlopForkProjectConfig {
                    instructions: Some("Project local instruction".to_string()),
                    instruction_files: vec!["GEMINI.md".to_string()],
                },
            )]),
        };
        save_slop_fork_config(dir.path(), &config)?;
        assert_eq!(load_slop_fork_config(dir.path())?, config);
        Ok(())
    }

    #[test]
    fn migrates_previous_config_filename() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let config = SlopForkConfig {
            auto_switch_accounts_on_rate_limit: false,
            follow_external_account_switches: true,
            api_key_fallback_on_all_accounts_limited: true,
            auto_start_five_hour_quota: true,
            auto_start_weekly_quota: false,
            show_account_numbers_instead_of_emails: true,
            show_average_account_limits_in_status_line: true,
            automation_enabled: false,
            automation_default_scope: AutomationScope::Repo,
            automation_shell_timeout_ms: 15_000,
            automation_disable_notify_script: true,
            automation_disable_terminal_notifications: false,
            instructions: Some("Use extra docs".to_string()),
            instruction_files: vec!["CLAUDE.md".to_string()],
            projects: HashMap::new(),
        };
        let legacy_path = legacy_slop_fork_config_path(dir.path());
        std::fs::write(&legacy_path, toml::to_string_pretty(&config)?)?;

        assert_eq!(load_slop_fork_config(dir.path())?, config);
        assert_eq!(
            std::fs::read_to_string(slop_fork_config_path(dir.path()))?,
            toml::to_string_pretty(&config)?
        );
        Ok(())
    }

    #[test]
    fn maybe_load_missing_config_does_not_create_file() -> anyhow::Result<()> {
        let dir = tempdir()?;
        assert_eq!(
            maybe_load_slop_fork_config(dir.path())?,
            SlopForkConfig::default()
        );
        assert!(!slop_fork_config_path(dir.path()).exists());
        Ok(())
    }

    #[test]
    fn instruction_overlay_combines_global_and_project_settings() -> anyhow::Result<()> {
        let dir = tempdir()?;
        let repo = dir.path().join("repo");
        let nested = repo.join("nested");
        std::fs::create_dir_all(repo.join(".git"))?;
        std::fs::create_dir_all(&nested)?;

        let config = SlopForkConfig {
            auto_switch_accounts_on_rate_limit: true,
            follow_external_account_switches: false,
            api_key_fallback_on_all_accounts_limited: false,
            auto_start_five_hour_quota: false,
            auto_start_weekly_quota: true,
            show_account_numbers_instead_of_emails: false,
            show_average_account_limits_in_status_line: false,
            automation_enabled: true,
            automation_default_scope: AutomationScope::Session,
            automation_shell_timeout_ms: 30_000,
            automation_disable_notify_script: false,
            automation_disable_terminal_notifications: true,
            instructions: Some("  global extra  ".to_string()),
            instruction_files: vec!["CLAUDE.md".to_string(), "  ".to_string()],
            projects: HashMap::from([(
                repo.to_string_lossy().to_string(),
                SlopForkProjectConfig {
                    instructions: Some("  project extra  ".to_string()),
                    instruction_files: vec![
                        "CLAUDE.md".to_string(),
                        "GEMINI.md".to_string(),
                        "".to_string(),
                    ],
                },
            )]),
        };

        let overlay = config.instruction_overlay(&nested);
        assert_eq!(
            overlay,
            SlopForkInstructionOverlay {
                global_instructions: Some("global extra".to_string()),
                project_instructions: Some("project extra".to_string()),
                instruction_files: vec!["CLAUDE.md".to_string(), "GEMINI.md".to_string()],
            }
        );
        Ok(())
    }
}
