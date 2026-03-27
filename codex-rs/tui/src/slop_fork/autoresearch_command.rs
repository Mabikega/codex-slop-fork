use std::ops::Range;

use codex_core::slop_fork::autoresearch::AutoresearchMode;

use crate::bottom_pane::MentionItem;

pub(crate) const AUTORESEARCH_COMMAND_NAME: &str = "autoresearch";
pub(crate) const AUTORESEARCH_COMMAND_MENTION_PATH: &str = "slop-fork://command/autoresearch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoresearchCommand {
    Help,
    Init {
        request: String,
        open_mode: bool,
    },
    Status,
    Portfolio,
    Discover {
        focus: Option<String>,
    },
    Start {
        goal: String,
        max_runs: Option<u32>,
        mode: AutoresearchMode,
    },
    Pause,
    Resume,
    WrapUp,
    Stop,
    Clear,
}

pub(crate) fn parse_autoresearch_command(args: &str) -> Result<AutoresearchCommand, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("help") {
        return Ok(AutoresearchCommand::Help);
    }

    let tokens = shlex::split(trimmed).ok_or_else(|| autoresearch_usage().to_string())?;
    if tokens.is_empty() {
        return Ok(AutoresearchCommand::Help);
    }

    match tokens[0].as_str() {
        "init" => parse_init_command(&tokens[1..]),
        "status" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch status".to_string());
            }
            Ok(AutoresearchCommand::Status)
        }
        "portfolio" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch portfolio".to_string());
            }
            Ok(AutoresearchCommand::Portfolio)
        }
        "discover" => parse_discover_command(&tokens[1..]),
        "pause" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch pause".to_string());
            }
            Ok(AutoresearchCommand::Pause)
        }
        "resume" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch resume".to_string());
            }
            Ok(AutoresearchCommand::Resume)
        }
        "wrap-up" | "wrapup" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch wrap-up".to_string());
            }
            Ok(AutoresearchCommand::WrapUp)
        }
        "stop" | "off" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch stop".to_string());
            }
            Ok(AutoresearchCommand::Stop)
        }
        "clear" => {
            if tokens.len() != 1 {
                return Err("Usage: $autoresearch clear".to_string());
            }
            Ok(AutoresearchCommand::Clear)
        }
        "start" => parse_start_command(&tokens[1..]),
        _ => parse_start_command(&tokens),
    }
}

fn parse_init_command(tokens: &[String]) -> Result<AutoresearchCommand, String> {
    let mut open_mode = false;
    let mut request_start = 0;
    while request_start < tokens.len() {
        match tokens[request_start].as_str() {
            "--open" => {
                open_mode = true;
                request_start += 1;
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown $autoresearch init option {value}."));
            }
            _ => break,
        }
    }
    let request = tokens[request_start..].join(" ").trim().to_string();
    if request.is_empty() {
        return Err("Usage: $autoresearch init [--open] <request>".to_string());
    }
    Ok(AutoresearchCommand::Init { request, open_mode })
}

fn parse_discover_command(tokens: &[String]) -> Result<AutoresearchCommand, String> {
    if tokens.is_empty() {
        return Ok(AutoresearchCommand::Discover { focus: None });
    }
    let focus = tokens.join(" ").trim().to_string();
    Ok(AutoresearchCommand::Discover {
        focus: (!focus.is_empty()).then_some(focus),
    })
}

fn parse_start_command(tokens: &[String]) -> Result<AutoresearchCommand, String> {
    if tokens.is_empty() {
        return Err(
            "Usage: $autoresearch start [--mode optimize|research|scientist] [--max-runs <count>] <goal>"
                .to_string(),
        );
    }

    let mut max_runs = None;
    let mut mode = AutoresearchMode::Optimize;
    let mut goal_index = 0;
    while goal_index < tokens.len() {
        match tokens[goal_index].as_str() {
            "--mode" => {
                let Some(value) = tokens.get(goal_index + 1) else {
                    return Err(
                        "Usage: $autoresearch start --mode optimize|research|scientist <goal>"
                            .to_string(),
                    );
                };
                mode = match value.as_str() {
                    "optimize" => AutoresearchMode::Optimize,
                    "research" => AutoresearchMode::Research,
                    "scientist" => AutoresearchMode::Scientist,
                    other => {
                        return Err(format!(
                            "Unknown $autoresearch mode {other}; expected optimize, research, or scientist."
                        ));
                    }
                };
                goal_index += 2;
            }
            "--max-runs" => {
                let Some(value) = tokens.get(goal_index + 1) else {
                    return Err("Usage: $autoresearch start --max-runs <count> <goal>".to_string());
                };
                max_runs =
                    Some(value.parse::<u32>().map_err(|_| {
                        "Autoresearch max runs must be a whole number.".to_string()
                    })?);
                goal_index += 2;
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown $autoresearch option {value}."));
            }
            _ => break,
        }
    }

    let goal = tokens[goal_index..].join(" ").trim().to_string();
    if goal.is_empty() {
        return Err(
            "Usage: $autoresearch start [--mode optimize|research|scientist] [--max-runs <count>] <goal>"
                .to_string(),
        );
    }

    Ok(AutoresearchCommand::Start {
        goal,
        max_runs,
        mode,
    })
}

pub(crate) fn autoresearch_usage() -> &'static str {
    "Autoresearch\n\
Run autonomous benchmark, research, or scientist loops with native benchmark logging tools.\n\n\
Usage:\n\
  $autoresearch init [--open] <request>\n\
  $autoresearch start [--mode optimize|research|scientist] [--max-runs <count>] <goal>\n\
  $autoresearch <goal>\n\
  $autoresearch status\n\
  $autoresearch portfolio\n\
  $autoresearch discover [focus]\n\
  $autoresearch pause\n\
  $autoresearch resume\n\
  $autoresearch wrap-up\n\
  $autoresearch stop\n\
  $autoresearch clear\n\n\
Examples:\n\
  $autoresearch init \"Create an OCR project with CER < 5% on dataset X\"\n\
  $autoresearch init --open \"Explore OCR approaches with CER < 5% on dataset X\"\n\
  $autoresearch optimize unit test runtime without changing semantics\n\
  $autoresearch start --mode research --max-runs 50 search for better OCR architectures\n\
  $autoresearch start --mode scientist --max-runs 50 map promising OCR hypotheses\n\
  $autoresearch start --max-runs 50 reduce benchmark wall clock time\n\
  $autoresearch wrap-up"
}

pub(crate) fn autoresearch_command_mention_item() -> MentionItem {
    MentionItem {
        display_name: AUTORESEARCH_COMMAND_NAME.to_string(),
        description: Some("autoresearch command".to_string()),
        insert_text: format!("${AUTORESEARCH_COMMAND_NAME}"),
        search_terms: vec![
            AUTORESEARCH_COMMAND_NAME.to_string(),
            "benchmark".to_string(),
            "optimize".to_string(),
            "scientist".to_string(),
            "command".to_string(),
        ],
        path: Some(AUTORESEARCH_COMMAND_MENTION_PATH.to_string()),
        category_tag: Some("[Command]".to_string()),
        sort_rank: 1,
    }
}

pub(crate) fn first_token(text: &str) -> Option<(&str, Range<usize>)> {
    if text.starts_with(char::is_whitespace) {
        return None;
    }
    let end = text
        .char_indices()
        .find(|(_, c)| c.is_whitespace())
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    Some((&text[..end], 0..end))
}

pub(crate) fn parse_autoresearch_command_args(text: &str) -> Option<&str> {
    let (first_token, range) = first_token(text)?;
    if first_token != "$autoresearch" {
        return None;
    }
    Some(text[range.end..].trim())
}

pub(crate) fn should_dispatch_autoresearch_command(
    first_token: &str,
    bound_path: Option<&str>,
) -> bool {
    first_token == "$autoresearch"
        && (bound_path.is_none() || bound_path == Some(AUTORESEARCH_COMMAND_MENTION_PATH))
}

pub(crate) fn should_record_autoresearch_command_in_history(args: &str) -> bool {
    matches!(
        parse_autoresearch_command(args),
        Ok(AutoresearchCommand::Init { .. } | AutoresearchCommand::Start { .. })
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_shorthand_start() {
        let command = parse_autoresearch_command("optimize benchmarks").expect("parse");
        assert_eq!(
            command,
            AutoresearchCommand::Start {
                goal: "optimize benchmarks".to_string(),
                max_runs: None,
                mode: AutoresearchMode::Optimize,
            }
        );
    }

    #[test]
    fn parses_max_runs() {
        let command = parse_autoresearch_command("start --max-runs 10 optimize").expect("parse");
        assert_eq!(
            command,
            AutoresearchCommand::Start {
                goal: "optimize".to_string(),
                max_runs: Some(10),
                mode: AutoresearchMode::Optimize,
            }
        );
    }

    #[test]
    fn parses_research_mode() {
        let command = parse_autoresearch_command("start --mode research try new OCR architectures")
            .expect("parse");
        assert_eq!(
            command,
            AutoresearchCommand::Start {
                goal: "try new OCR architectures".to_string(),
                max_runs: None,
                mode: AutoresearchMode::Research,
            }
        );
    }

    #[test]
    fn parses_scientist_mode() {
        let command =
            parse_autoresearch_command("start --mode scientist map OCR hypotheses").expect("parse");
        assert_eq!(
            command,
            AutoresearchCommand::Start {
                goal: "map OCR hypotheses".to_string(),
                max_runs: None,
                mode: AutoresearchMode::Scientist,
            }
        );
    }

    #[test]
    fn parses_init_request() {
        let command = parse_autoresearch_command("init create an OCR project").expect("parse");
        assert_eq!(
            command,
            AutoresearchCommand::Init {
                request: "create an OCR project".to_string(),
                open_mode: false,
            }
        );
    }

    #[test]
    fn parses_open_init_request() {
        let command = parse_autoresearch_command("init --open discover strong OCR approaches")
            .expect("parse");
        assert_eq!(
            command,
            AutoresearchCommand::Init {
                request: "discover strong OCR approaches".to_string(),
                open_mode: true,
            }
        );
    }

    #[test]
    fn history_prefers_driving_commands() {
        assert!(should_record_autoresearch_command_in_history(
            "init create an OCR project"
        ));
        assert!(should_record_autoresearch_command_in_history(
            "start optimize tests"
        ));
        assert!(!should_record_autoresearch_command_in_history("status"));
        assert!(!should_record_autoresearch_command_in_history("pause"));
        assert!(!should_record_autoresearch_command_in_history("clear"));
    }
}
