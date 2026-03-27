use std::path::Path;

use tracing::debug;

use super::AUTORESEARCH_DOC_FILE;

const DISCOVERY_POLICY_HEADING: &str = "Discovery Policy";
const SELECTION_POLICY_HEADING: &str = "Selection Policy";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionPolicy {
    pub weak_branch_score_gap: i64,
    pub stagnation_window: usize,
    pub synthesis_after_exploit_cycles: u32,
}

impl Default for SelectionPolicy {
    fn default() -> Self {
        Self {
            weak_branch_score_gap: 8,
            stagnation_window: 3,
            synthesis_after_exploit_cycles: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SelectionPolicySettings {
    pub policy: SelectionPolicy,
    pub issues: Vec<String>,
    pub has_custom_values: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortfolioRefreshPolicy {
    pub minimum_family_count: usize,
    pub low_diversity_exploit_cycles: u32,
    pub exploit_cycles: u32,
    pub cooldown_seconds: u32,
}

impl Default for PortfolioRefreshPolicy {
    fn default() -> Self {
        Self {
            minimum_family_count: 3,
            low_diversity_exploit_cycles: 2,
            exploit_cycles: 5,
            cooldown_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PortfolioRefreshPolicySettings {
    pub policy: PortfolioRefreshPolicy,
    pub issues: Vec<String>,
    pub has_custom_values: bool,
}

pub(crate) fn load_selection_policy_settings(workdir: &Path) -> SelectionPolicySettings {
    let Some(doc) = load_autoresearch_doc(workdir) else {
        return SelectionPolicySettings::default();
    };
    parse_selection_policy_settings(&doc)
}

pub(crate) fn load_portfolio_refresh_policy_settings(
    workdir: &Path,
) -> PortfolioRefreshPolicySettings {
    let Some(doc) = load_autoresearch_doc(workdir) else {
        return PortfolioRefreshPolicySettings::default();
    };
    parse_portfolio_refresh_policy_settings(&doc)
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
                "failed to read autoresearch policy configuration"
            );
            None
        }
    }
}

fn parse_selection_policy_settings(doc: &str) -> SelectionPolicySettings {
    let Some(section) = extract_markdown_section(doc, SELECTION_POLICY_HEADING) else {
        return SelectionPolicySettings::default();
    };

    let mut settings = SelectionPolicySettings::default();
    for (index, raw_line) in section.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match apply_policy_line(&mut settings.policy, trimmed) {
            Ok(SelectionPolicyLineOutcome::Applied) => {
                settings.has_custom_values = true;
            }
            Ok(SelectionPolicyLineOutcome::Ignored) => {}
            Err(issue) => {
                settings.issues.push(format!(
                    "selection policy line {} `{trimmed}` is invalid: {issue}",
                    index.saturating_add(1)
                ));
            }
        }
    }
    settings
}

fn parse_portfolio_refresh_policy_settings(doc: &str) -> PortfolioRefreshPolicySettings {
    let Some(section) = extract_markdown_section(doc, DISCOVERY_POLICY_HEADING) else {
        return PortfolioRefreshPolicySettings::default();
    };

    let mut settings = PortfolioRefreshPolicySettings::default();
    for (index, raw_line) in section.lines().enumerate() {
        let trimmed = raw_line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match apply_portfolio_refresh_policy_line(&mut settings.policy, trimmed) {
            Ok(SelectionPolicyLineOutcome::Applied) => {
                settings.has_custom_values = true;
            }
            Ok(SelectionPolicyLineOutcome::Ignored) => {}
            Err(issue) => {
                settings.issues.push(format!(
                    "discovery policy line {} `{trimmed}` is invalid: {issue}",
                    index.saturating_add(1)
                ));
            }
        }
    }
    settings
}

enum SelectionPolicyLineOutcome {
    Applied,
    Ignored,
}

fn apply_policy_line(
    policy: &mut SelectionPolicy,
    line: &str,
) -> Result<SelectionPolicyLineOutcome, String> {
    let Some((key, value)) = parse_bullet_key_value(line) else {
        return Ok(SelectionPolicyLineOutcome::Ignored);
    };
    match normalize_key(&key).as_str() {
        "weakbranchscoregap" => {
            policy.weak_branch_score_gap =
                parse_positive_i64(&value, "Weak Branch Score Gap", /*minimum*/ 1)?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        "stagnationwindow" => {
            policy.stagnation_window =
                parse_positive_usize(&value, "Stagnation Window", /*minimum*/ 2)?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        "synthesisafterexploitcycles" => {
            policy.synthesis_after_exploit_cycles =
                parse_non_negative_u32(&value, "Synthesis After Exploit Cycles")?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        _ => Ok(SelectionPolicyLineOutcome::Ignored),
    }
}

fn apply_portfolio_refresh_policy_line(
    policy: &mut PortfolioRefreshPolicy,
    line: &str,
) -> Result<SelectionPolicyLineOutcome, String> {
    let Some((key, value)) = parse_bullet_key_value(line) else {
        return Ok(SelectionPolicyLineOutcome::Ignored);
    };
    match normalize_key(&key).as_str() {
        "portfoliorefreshminimumfamilies" => {
            policy.minimum_family_count = parse_positive_usize(
                &value,
                "Portfolio Refresh Minimum Families",
                /*minimum*/ 1,
            )?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        "portfoliorefreshexploitcycleslowdiversity" => {
            policy.low_diversity_exploit_cycles =
                parse_non_negative_u32(&value, "Portfolio Refresh Exploit Cycles (Low Diversity)")?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        "portfoliorefreshexploitcycles" => {
            policy.exploit_cycles =
                parse_non_negative_u32(&value, "Portfolio Refresh Exploit Cycles")?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        "portfoliorefreshcooldownseconds" => {
            policy.cooldown_seconds =
                parse_non_negative_u32(&value, "Portfolio Refresh Cooldown Seconds")?;
            Ok(SelectionPolicyLineOutcome::Applied)
        }
        _ => Ok(SelectionPolicyLineOutcome::Ignored),
    }
}

fn parse_positive_i64(value: &str, field_name: &str, minimum: i64) -> Result<i64, String> {
    let parsed = value
        .trim()
        .parse::<i64>()
        .map_err(|_| format!("{field_name} must be an integer"))?;
    if parsed < minimum {
        return Err(format!("{field_name} must be >= {minimum}"));
    }
    Ok(parsed)
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

fn parse_non_negative_u32(value: &str, field_name: &str) -> Result<u32, String> {
    value
        .trim()
        .parse::<u32>()
        .map_err(|_| format!("{field_name} must be a non-negative integer"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_policy_defaults_when_section_missing() {
        let settings = parse_selection_policy_settings("# Goal\nx\n");

        assert_eq!(settings, SelectionPolicySettings::default());
    }

    #[test]
    fn selection_policy_parses_custom_values() {
        let settings = parse_selection_policy_settings(
            "# Goal\nx\n\n## Selection Policy\n- Weak Branch Score Gap: 11\n- Stagnation Window: 4\n- Synthesis After Exploit Cycles: 3\n",
        );

        assert!(settings.has_custom_values);
        assert_eq!(
            settings.policy,
            SelectionPolicy {
                weak_branch_score_gap: 11,
                stagnation_window: 4,
                synthesis_after_exploit_cycles: 3,
            }
        );
        assert!(settings.issues.is_empty());
    }

    #[test]
    fn selection_policy_ignores_freeform_strategy_text() {
        let settings = parse_selection_policy_settings(
            "# Goal\nx\n\n## Selection Policy\n- Promote approaches after repeated keeps.\n- Retire dead ends quickly when checks fail.\n- Winner criteria: stable keeps plus constraint headroom.\n",
        );

        assert_eq!(settings.policy, SelectionPolicy::default());
        assert!(!settings.has_custom_values);
        assert!(settings.issues.is_empty());
    }

    #[test]
    fn selection_policy_reports_invalid_supported_keys_and_keeps_defaults() {
        let settings = parse_selection_policy_settings(
            "# Goal\nx\n\n## Selection Policy\n- Weak Branch Score Gap: nope\n- Stagnation Window: 1\n- Winner Criteria: stable keeps\n",
        );

        assert_eq!(settings.policy, SelectionPolicy::default());
        assert!(!settings.has_custom_values);
        assert_eq!(settings.issues.len(), 2);
        assert!(
            settings
                .issues
                .iter()
                .any(|issue| issue.contains("Weak Branch Score Gap must be an integer"))
        );
        assert!(
            settings
                .issues
                .iter()
                .any(|issue| issue.contains("Stagnation Window must be >= 2"))
        );
    }

    #[test]
    fn portfolio_refresh_policy_parses_custom_values() {
        let settings = parse_portfolio_refresh_policy_settings(
            "# Goal\nx\n\n## Discovery Policy\n- Portfolio Refresh Minimum Families: 4\n- Portfolio Refresh Exploit Cycles (Low Diversity): 3\n- Portfolio Refresh Exploit Cycles: 6\n- Portfolio Refresh Cooldown Seconds: 90\n",
        );

        assert!(settings.has_custom_values);
        assert_eq!(
            settings.policy,
            PortfolioRefreshPolicy {
                minimum_family_count: 4,
                low_diversity_exploit_cycles: 3,
                exploit_cycles: 6,
                cooldown_seconds: 90,
            }
        );
        assert!(settings.issues.is_empty());
    }

    #[test]
    fn portfolio_refresh_policy_ignores_freeform_strategy_text() {
        let settings = parse_portfolio_refresh_policy_settings(
            "# Goal\nx\n\n## Discovery Policy\n- Use repo-wide audits when progress stalls.\n- Bring in online research when the benchmark keeps rejecting local tweaks.\n",
        );

        assert_eq!(settings.policy, PortfolioRefreshPolicy::default());
        assert!(!settings.has_custom_values);
        assert!(settings.issues.is_empty());
    }

    #[test]
    fn portfolio_refresh_policy_reports_invalid_supported_keys_and_keeps_defaults() {
        let settings = parse_portfolio_refresh_policy_settings(
            "# Goal\nx\n\n## Discovery Policy\n- Portfolio Refresh Minimum Families: 0\n- Portfolio Refresh Cooldown Seconds: nope\n- Discovery Cadence: weekly\n",
        );

        assert_eq!(settings.policy, PortfolioRefreshPolicy::default());
        assert!(!settings.has_custom_values);
        assert_eq!(settings.issues.len(), 2);
        assert!(
            settings
                .issues
                .iter()
                .any(|issue| issue.contains("Portfolio Refresh Minimum Families must be >= 1"))
        );
        assert!(settings.issues.iter().any(|issue| {
            issue.contains("Portfolio Refresh Cooldown Seconds must be a non-negative integer")
        }));
    }
}
