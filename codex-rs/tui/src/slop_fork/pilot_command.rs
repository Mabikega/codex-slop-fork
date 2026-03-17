use chrono::DateTime;
use chrono::Local;
use std::ops::Range;
use std::time::Duration;

use crate::bottom_pane::MentionItem;

pub(crate) const PILOT_COMMAND_NAME: &str = "pilot";
pub(crate) const PILOT_COMMAND_MENTION_PATH: &str = "slop-fork://command/pilot";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PilotCommand {
    Help,
    Status,
    Start {
        goal: String,
        deadline_at: Option<i64>,
    },
    Pause,
    Resume,
    WrapUp,
    Stop,
}

pub(crate) fn parse_pilot_command(
    args: &str,
    now: DateTime<Local>,
) -> Result<PilotCommand, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("help") {
        return Ok(PilotCommand::Help);
    }

    let tokens = shlex::split(trimmed).ok_or_else(|| pilot_usage().to_string())?;
    if tokens.is_empty() {
        return Ok(PilotCommand::Help);
    }

    match tokens[0].as_str() {
        "status" => {
            if tokens.len() != 1 {
                return Err("Usage: $pilot status".to_string());
            }
            Ok(PilotCommand::Status)
        }
        "pause" => {
            if tokens.len() != 1 {
                return Err("Usage: $pilot pause".to_string());
            }
            Ok(PilotCommand::Pause)
        }
        "resume" => {
            if tokens.len() != 1 {
                return Err("Usage: $pilot resume".to_string());
            }
            Ok(PilotCommand::Resume)
        }
        "wrap-up" | "wrapup" => {
            if tokens.len() != 1 {
                return Err("Usage: $pilot wrap-up".to_string());
            }
            Ok(PilotCommand::WrapUp)
        }
        "stop" => {
            if tokens.len() != 1 {
                return Err("Usage: $pilot stop".to_string());
            }
            Ok(PilotCommand::Stop)
        }
        "start" => parse_start_command(&tokens[1..], now),
        _ => Err(pilot_usage().to_string()),
    }
}

fn parse_start_command(tokens: &[String], now: DateTime<Local>) -> Result<PilotCommand, String> {
    if tokens.is_empty() {
        return Err("Usage: $pilot start [--for <duration>] <goal>".to_string());
    }

    let mut deadline_at = None;
    let mut goal_index = 0;
    while goal_index < tokens.len() {
        match tokens[goal_index].as_str() {
            "--for" => {
                let Some(value) = tokens.get(goal_index + 1) else {
                    return Err("Usage: $pilot start --for <duration> <goal>".to_string());
                };
                let duration = parse_duration(value)?;
                let seconds = i64::try_from(duration.as_secs())
                    .map_err(|_| "Pilot duration is too large.".to_string())?;
                deadline_at = Some(now.timestamp().saturating_add(seconds));
                goal_index += 2;
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown $pilot option {value}."));
            }
            _ => break,
        }
    }

    let goal = tokens[goal_index..].join(" ").trim().to_string();
    if goal.is_empty() {
        return Err("Usage: $pilot start [--for <duration>] <goal>".to_string());
    }

    Ok(PilotCommand::Start { goal, deadline_at })
}

fn parse_duration(raw: &str) -> Result<Duration, String> {
    let mut total_seconds = 0_u64;
    let mut digits = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_digit() {
            digits.push(ch);
            continue;
        }
        if digits.is_empty() {
            return Err("Pilot duration must look like 30m, 4h, or 1h30m.".to_string());
        }
        let value = digits
            .parse::<u64>()
            .map_err(|_| "Pilot duration must use whole numbers.".to_string())?;
        digits.clear();
        let unit_seconds = match ch {
            's' => 1,
            'm' => 60,
            'h' => 60 * 60,
            'd' => 60 * 60 * 24,
            _ => return Err("Pilot duration must use s, m, h, or d units.".to_string()),
        };
        total_seconds = total_seconds
            .checked_add(value.saturating_mul(unit_seconds))
            .ok_or_else(|| "Pilot duration is too large.".to_string())?;
    }
    if !digits.is_empty() {
        return Err("Pilot duration must end with a unit like m or h.".to_string());
    }
    if total_seconds == 0 {
        return Err("Pilot duration must be greater than zero.".to_string());
    }
    Ok(Duration::from_secs(total_seconds))
}

pub(crate) fn pilot_usage() -> &'static str {
    "Pilot\n\
Start an assistant-controlled autonomous run without synthetic user follow-up messages.\n\n\
Usage:\n\
  $pilot start [--for <duration>] <goal>\n\
  $pilot status\n\
  $pilot pause\n\
  $pilot resume\n\
  $pilot wrap-up\n\
  $pilot stop\n\n\
Examples:\n\
  $pilot start --for 4h Improve benchmark accuracy end-to-end\n\
  $pilot wrap-up"
}

pub(crate) fn pilot_command_mention_item() -> MentionItem {
    MentionItem {
        display_name: PILOT_COMMAND_NAME.to_string(),
        description: Some("pilot command, not a skill".to_string()),
        insert_text: format!("${PILOT_COMMAND_NAME}"),
        search_terms: vec![
            PILOT_COMMAND_NAME.to_string(),
            "autonomous".to_string(),
            "command".to_string(),
        ],
        path: Some(PILOT_COMMAND_MENTION_PATH.to_string()),
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

pub(crate) fn parse_pilot_command_args(text: &str) -> Option<&str> {
    let (first_token, range) = first_token(text)?;
    if !is_pilot_command_token(first_token) {
        return None;
    }
    Some(text[range.end..].trim())
}

pub(crate) fn is_pilot_command_token(token: &str) -> bool {
    token == "$pilot"
}

pub(crate) fn should_dispatch_pilot_command(first_token: &str, bound_path: Option<&str>) -> bool {
    is_pilot_command_token(first_token)
        && (bound_path.is_none() || bound_path == Some(PILOT_COMMAND_MENTION_PATH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parses_start_with_deadline() {
        let now = Local::now();
        let command = parse_pilot_command("start --for 1h30m improve benchmarks", now).unwrap();
        let PilotCommand::Start { goal, deadline_at } = command else {
            panic!("expected start command");
        };
        assert_eq!(goal, "improve benchmarks");
        assert_eq!(deadline_at, Some(now.timestamp() + 5400));
    }

    #[test]
    fn rejects_duration_without_unit() {
        let err = parse_pilot_command("start --for 10 improve benchmarks", Local::now())
            .expect_err("expected parse error");
        assert!(err.contains("unit"));
    }
}
