use chrono::DateTime;
use chrono::Local;
use codex_core::slop_fork::automation::AutomationLimits;
use codex_core::slop_fork::automation::AutomationMessageSource;
use codex_core::slop_fork::automation::AutomationPolicyCommand;
use codex_core::slop_fork::automation::AutomationScope;
use codex_core::slop_fork::automation::AutomationSpec;
use codex_core::slop_fork::automation::AutomationTrigger;
use codex_core::slop_fork::automation::parse_until_for_today;
use std::ops::Range;

use super::schedule_parser::TimerSchedule;
use super::schedule_parser::parse_timer_schedule_request;
use crate::bottom_pane::MentionItem;

pub(crate) const AUTO_COMMAND_NAME: &str = "auto";
pub(crate) const AUTO_COMMAND_MENTION_PATH: &str = "slop-fork://command/auto";

pub(crate) fn auto_command_skill_conflict_warning() -> String {
    format!(
        "A skill named '{AUTO_COMMAND_NAME}' is enabled. '${AUTO_COMMAND_NAME}' is reserved for the automation command, so use the $ popup to insert the skill explicitly."
    )
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AutoCommand {
    Help,
    List,
    Show {
        runtime_id: String,
    },
    Pause {
        runtime_id: String,
    },
    Resume {
        runtime_id: String,
    },
    Remove {
        runtime_id: String,
    },
    Create {
        scope: AutomationScope,
        spec: AutomationSpec,
        note: Option<String>,
    },
}

pub(crate) fn parse_auto_command(
    args: &str,
    default_scope: AutomationScope,
    now: DateTime<Local>,
) -> Result<AutoCommand, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("help") {
        return Ok(AutoCommand::Help);
    }

    let tokens = shlex::split(trimmed)
        .ok_or_else(|| "Failed to parse $auto arguments. Check your quoting.".to_string())?;
    if tokens.is_empty() {
        return Ok(AutoCommand::Help);
    }

    match tokens[0].as_str() {
        "list" => {
            if tokens.len() != 1 {
                return Err("Usage: $auto list".to_string());
            }
            Ok(AutoCommand::List)
        }
        "show" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto show <runtime-id>".to_string());
            }
            Ok(AutoCommand::Show {
                runtime_id: tokens[1].clone(),
            })
        }
        "pause" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto pause <runtime-id>".to_string());
            }
            Ok(AutoCommand::Pause {
                runtime_id: tokens[1].clone(),
            })
        }
        "resume" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto resume <runtime-id>".to_string());
            }
            Ok(AutoCommand::Resume {
                runtime_id: tokens[1].clone(),
            })
        }
        "rm" | "remove" | "delete" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto rm <runtime-id>".to_string());
            }
            Ok(AutoCommand::Remove {
                runtime_id: tokens[1].clone(),
            })
        }
        "every" => parse_every_command(&tokens[1..], default_scope, now),
        "on-complete" => parse_on_complete_command(&tokens[1..], default_scope, now),
        _ => Err(auto_usage().to_string()),
    }
}

fn parse_on_complete_command(
    tokens: &[String],
    default_scope: AutomationScope,
    now: DateTime<Local>,
) -> Result<AutoCommand, String> {
    let mut scope = default_scope;
    let mut max_runs = None;
    let mut until_at = None;
    let mut round_robin = false;
    let mut policy_command = None;
    let mut messages = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        match tokens[index].as_str() {
            "--scope" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --scope <session|repo|global>".to_string());
                };
                scope = match value.as_str() {
                    "session" => AutomationScope::Session,
                    "repo" => AutomationScope::Repo,
                    "global" => AutomationScope::Global,
                    _ => return Err("Usage: --scope <session|repo|global>".to_string()),
                };
                index += 2;
            }
            "--times" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --times <count>".to_string());
                };
                max_runs = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| "Usage: --times <count>".to_string())?,
                );
                index += 2;
            }
            "--until" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --until <HH:MM>".to_string());
                };
                until_at = Some(parse_until_for_today(now, value)?.timestamp());
                index += 2;
            }
            "--round-robin" => {
                round_robin = true;
                index += 1;
            }
            "--policy" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --policy '<command>'".to_string());
                };
                let command = shlex::split(value)
                    .ok_or_else(|| "Failed to parse --policy command.".to_string())?;
                if command.is_empty() {
                    return Err("Policy command cannot be empty.".to_string());
                }
                policy_command = Some(AutomationPolicyCommand {
                    command,
                    cwd: None,
                    timeout_ms: None,
                });
                index += 2;
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown $auto option {value}."));
            }
            _ => {
                messages.extend_from_slice(&tokens[index..]);
                break;
            }
        }
    }

    if messages.is_empty() {
        return Err(
            "Usage: $auto on-complete [options] <message> or $auto on-complete --round-robin \"msg 1\" \"msg 2\" ...".to_string(),
        );
    }

    let message_source = if round_robin {
        if messages.len() < 2 {
            return Err(
                "--round-robin requires at least two messages, each quoted as its own argument."
                    .to_string(),
            );
        }
        AutomationMessageSource::RoundRobin { messages }
    } else {
        AutomationMessageSource::Static {
            message: messages.join(" "),
        }
    };

    Ok(AutoCommand::Create {
        scope,
        spec: AutomationSpec {
            id: String::new(),
            enabled: true,
            trigger: AutomationTrigger::TurnCompleted,
            message_source,
            limits: AutomationLimits { max_runs, until_at },
            policy_command,
        },
        note: None,
    })
}

fn parse_every_command(
    tokens: &[String],
    default_scope: AutomationScope,
    now: DateTime<Local>,
) -> Result<AutoCommand, String> {
    let mut scope = default_scope;
    let mut max_runs = None;
    let mut until_at = None;
    let mut policy_command = None;
    let mut index = 0;

    while index < tokens.len() {
        match tokens[index].as_str() {
            "--scope" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --scope <session|repo|global>".to_string());
                };
                scope = match value.as_str() {
                    "session" => AutomationScope::Session,
                    "repo" => AutomationScope::Repo,
                    "global" => AutomationScope::Global,
                    _ => return Err("Usage: --scope <session|repo|global>".to_string()),
                };
                index += 2;
            }
            "--times" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --times <count>".to_string());
                };
                max_runs = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| "Usage: --times <count>".to_string())?,
                );
                index += 2;
            }
            "--until" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --until <HH:MM>".to_string());
                };
                until_at = Some(parse_until_for_today(now, value)?.timestamp());
                index += 2;
            }
            "--policy" => {
                let Some(value) = tokens.get(index + 1) else {
                    return Err("Usage: --policy '<command>'".to_string());
                };
                let command = shlex::split(value)
                    .ok_or_else(|| "Failed to parse --policy command.".to_string())?;
                if command.is_empty() {
                    return Err("Policy command cannot be empty.".to_string());
                }
                policy_command = Some(AutomationPolicyCommand {
                    command,
                    cwd: None,
                    timeout_ms: None,
                });
                index += 2;
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown $auto option {value}."));
            }
            _ => break,
        }
    }

    let remainder = tokens[index..].join(" ");
    let request = parse_timer_schedule_request(&remainder, None).map_err(|message| {
        if message == "Usage: <interval|cron> <prompt>" {
            "Usage: $auto every [options] <interval|cron> <prompt>".to_string()
        } else {
            message
        }
    })?;

    let trigger = match request.schedule {
        TimerSchedule::Interval(cadence) => AutomationTrigger::Interval {
            every_seconds: cadence.as_secs(),
        },
        TimerSchedule::Cron(expression) => AutomationTrigger::Cron {
            expression: expression.expression().to_string(),
        },
    };

    Ok(AutoCommand::Create {
        scope,
        spec: AutomationSpec {
            id: String::new(),
            enabled: true,
            trigger,
            message_source: AutomationMessageSource::Static {
                message: request.prompt,
            },
            limits: AutomationLimits { max_runs, until_at },
            policy_command,
        },
        note: request.note,
    })
}

pub(crate) fn auto_usage() -> &'static str {
    "Usage: $auto on-complete [--scope session|repo|global] [--times N] [--until HH:MM] [--policy 'cmd'] <message>\n       $auto on-complete --round-robin \"msg 1\" \"msg 2\" ...\n       $auto every [--scope session|repo|global] [--times N] [--until HH:MM] [--policy 'cmd'] <interval|cron> <prompt>\n       $auto list | $auto show <runtime-id> | $auto pause <runtime-id> | $auto resume <runtime-id> | $auto rm <runtime-id>"
}

pub(crate) fn auto_command_mention_item() -> MentionItem {
    MentionItem {
        display_name: AUTO_COMMAND_NAME.to_string(),
        description: Some("automation command, not a skill".to_string()),
        insert_text: format!("${AUTO_COMMAND_NAME}"),
        search_terms: vec![
            AUTO_COMMAND_NAME.to_string(),
            "automation".to_string(),
            "command".to_string(),
        ],
        path: Some(AUTO_COMMAND_MENTION_PATH.to_string()),
        category_tag: Some("[Command]".to_string()),
        sort_rank: 0,
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

pub(crate) fn parse_auto_command_args(text: &str) -> Option<&str> {
    let (first_token, range) = first_token(text)?;
    if !is_auto_command_token(first_token) {
        return None;
    }
    Some(text[range.end..].trim())
}

pub(crate) fn is_auto_command_token(token: &str) -> bool {
    token == "$auto"
}

pub(crate) fn should_dispatch_auto_command(first_token: &str, bound_path: Option<&str>) -> bool {
    is_auto_command_token(first_token)
        && (bound_path.is_none() || bound_path == Some(AUTO_COMMAND_MENTION_PATH))
}
