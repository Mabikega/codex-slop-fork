use std::path::Path;
use std::path::PathBuf;

const SLOP_FORK_CONFIG_FILE: &str = "config-slop-fork.toml";

fn legacy_config_filename() -> String {
    ["codex", "alt", "fork.toml"].join("-")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlopForkConfig {
    pub follow_external_account_switches: bool,
    pub show_account_numbers_instead_of_emails: bool,
}

pub fn maybe_load_slop_fork_config(codex_home: &Path) -> std::io::Result<SlopForkConfig> {
    read_slop_fork_config(codex_home)
}

fn read_slop_fork_config(codex_home: &Path) -> std::io::Result<SlopForkConfig> {
    let path = slop_fork_config_path(codex_home);
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(parse_slop_fork_config(&contents)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let legacy_path = legacy_slop_fork_config_path(codex_home);
            match std::fs::read_to_string(&legacy_path) {
                Ok(contents) => Ok(parse_slop_fork_config(&contents)),
                Err(legacy_err) if legacy_err.kind() == std::io::ErrorKind::NotFound => {
                    Ok(SlopForkConfig::default())
                }
                Err(legacy_err) => Err(legacy_err),
            }
        }
        Err(err) => Err(err),
    }
}

fn slop_fork_config_path(codex_home: &Path) -> PathBuf {
    codex_home.join(SLOP_FORK_CONFIG_FILE)
}

fn legacy_slop_fork_config_path(codex_home: &Path) -> PathBuf {
    codex_home.join(legacy_config_filename())
}

fn parse_slop_fork_config(contents: &str) -> SlopForkConfig {
    let mut config = SlopForkConfig::default();
    let mut in_root_table = true;

    for raw_line in contents.lines() {
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_root_table = false;
            continue;
        }
        if !in_root_table {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let parsed = match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        };
        match (key, parsed) {
            ("follow_external_account_switches", Some(value)) => {
                config.follow_external_account_switches = value;
            }
            ("show_account_numbers_instead_of_emails", Some(value)) => {
                config.show_account_numbers_instead_of_emails = value;
            }
            _ => {}
        }
    }

    config
}

fn strip_comment(line: &str) -> &str {
    line.split('#').next().unwrap_or(line)
}
