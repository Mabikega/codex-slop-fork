use std::ops::Range;

use crate::bottom_pane::MentionItem;

pub(crate) const AUTORESEARCH_COMMAND_NAME: &str = "autoresearch";
pub(crate) const AUTORESEARCH_COMMAND_MENTION_PATH: &str = "slop-fork://command/autoresearch";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoresearchCommand {
    Help,
    Init { request: String },
    Status,
    Start { goal: String, max_runs: Option<u32> },
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
    let request = tokens.join(" ").trim().to_string();
    if request.is_empty() {
        return Err("Usage: $autoresearch init <request>".to_string());
    }
    Ok(AutoresearchCommand::Init { request })
}

fn parse_start_command(tokens: &[String]) -> Result<AutoresearchCommand, String> {
    if tokens.is_empty() {
        return Err("Usage: $autoresearch start [--max-runs <count>] <goal>".to_string());
    }

    let mut max_runs = None;
    let mut goal_index = 0;
    while goal_index < tokens.len() {
        match tokens[goal_index].as_str() {
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
        return Err("Usage: $autoresearch start [--max-runs <count>] <goal>".to_string());
    }

    Ok(AutoresearchCommand::Start { goal, max_runs })
}

pub(crate) fn autoresearch_usage() -> &'static str {
    "Autoresearch\n\
Run an autonomous benchmark loop with native benchmark logging tools.\n\n\
Usage:\n\
  $autoresearch init <request>\n\
  $autoresearch start [--max-runs <count>] <goal>\n\
  $autoresearch <goal>\n\
  $autoresearch status\n\
  $autoresearch pause\n\
  $autoresearch resume\n\
  $autoresearch wrap-up\n\
  $autoresearch stop\n\
  $autoresearch clear\n\n\
Examples:\n\
  $autoresearch init \"Create an OCR project with CER < 5% on dataset X\"\n\
  $autoresearch optimize unit test runtime without changing semantics\n\
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
