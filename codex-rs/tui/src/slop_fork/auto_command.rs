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

#[derive(Debug, Clone)]
struct ShellToken {
    value: String,
    span: Range<usize>,
}

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
        send_now: bool,
    },
}

pub(crate) fn parse_auto_command(
    args: &str,
    default_scope: AutomationScope,
    now: DateTime<Local>,
    last_user_message: Option<&str>,
) -> Result<AutoCommand, String> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("help") {
        return Ok(AutoCommand::Help);
    }

    let tokens = split_shell_tokens(trimmed)?;
    if tokens.is_empty() {
        return Ok(AutoCommand::Help);
    }

    match tokens[0].value.as_str() {
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
                runtime_id: tokens[1].value.clone(),
            })
        }
        "pause" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto pause <runtime-id>".to_string());
            }
            Ok(AutoCommand::Pause {
                runtime_id: tokens[1].value.clone(),
            })
        }
        "resume" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto resume <runtime-id>".to_string());
            }
            Ok(AutoCommand::Resume {
                runtime_id: tokens[1].value.clone(),
            })
        }
        "rm" | "remove" | "delete" => {
            if tokens.len() != 2 {
                return Err("Usage: $auto rm <runtime-id>".to_string());
            }
            Ok(AutoCommand::Remove {
                runtime_id: tokens[1].value.clone(),
            })
        }
        "every" => {
            parse_every_command(trimmed, &tokens[1..], default_scope, now, last_user_message)
        }
        "on-complete" => {
            parse_on_complete_command(trimmed, &tokens[1..], default_scope, now, last_user_message)
        }
        _ => Err(auto_usage().to_string()),
    }
}

fn parse_on_complete_command(
    input: &str,
    tokens: &[ShellToken],
    default_scope: AutomationScope,
    now: DateTime<Local>,
    last_user_message: Option<&str>,
) -> Result<AutoCommand, String> {
    let mut scope = default_scope;
    let mut max_runs = None;
    let mut until_at = None;
    let mut round_robin = false;
    let mut policy_command = None;
    let mut send_now = false;
    let mut use_last_user_message = false;
    let mut messages = Vec::new();
    let mut index = 0;

    while index < tokens.len() {
        match tokens[index].value.as_str() {
            "--scope" => {
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
                    return Err("Usage: --scope <session|repo|global>".to_string());
                };
                scope = match value {
                    "session" => AutomationScope::Session,
                    "repo" => AutomationScope::Repo,
                    "global" => AutomationScope::Global,
                    _ => return Err("Usage: --scope <session|repo|global>".to_string()),
                };
                index += 2;
            }
            "--times" => {
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
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
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
                    return Err("Usage: --until <HH:MM>".to_string());
                };
                until_at = Some(parse_until_for_today(now, value)?.timestamp());
                index += 2;
            }
            "--round-robin" => {
                round_robin = true;
                index += 1;
            }
            "--now" => {
                send_now = true;
                index += 1;
            }
            "--last-user-message" | "-l" => {
                use_last_user_message = true;
                index += 1;
            }
            "--policy" => {
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
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
                messages.extend(tokens[index..].iter().map(|token| token.value.clone()));
                break;
            }
        }
    }

    if use_last_user_message && round_robin {
        return Err("--last-user-message cannot be combined with --round-robin.".to_string());
    }
    if send_now && policy_command.is_some() {
        return Err("--now cannot be combined with --policy.".to_string());
    }

    if use_last_user_message && !messages.is_empty() {
        return Err("Use either <message> or --last-user-message, not both.".to_string());
    }

    if messages.is_empty() && !use_last_user_message {
        return Err(
            "Usage: $auto on-complete [options] <message> or $auto on-complete --round-robin \"msg 1\" \"msg 2\" ...".to_string(),
        );
    }

    let message_source = if use_last_user_message {
        let Some(message) = last_user_message
            .map(str::trim)
            .filter(|message| !message.is_empty())
        else {
            return Err(
                "No previous text user message is available for --last-user-message.".to_string(),
            );
        };
        AutomationMessageSource::Static {
            message: message.to_string(),
        }
    } else if round_robin {
        if messages.len() < 2 {
            return Err(
                "--round-robin requires at least two messages, each quoted as its own argument."
                    .to_string(),
            );
        }
        AutomationMessageSource::RoundRobin { messages }
    } else {
        AutomationMessageSource::Static {
            message: rebuild_shell_text(input, &tokens[index..]),
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
        send_now,
    })
}

fn parse_every_command(
    input: &str,
    tokens: &[ShellToken],
    default_scope: AutomationScope,
    now: DateTime<Local>,
    last_user_message: Option<&str>,
) -> Result<AutoCommand, String> {
    let mut scope = default_scope;
    let mut max_runs = None;
    let mut until_at = None;
    let mut policy_command = None;
    let mut send_now = false;
    let mut use_last_user_message = false;
    let mut index = 0;

    while index < tokens.len() {
        match tokens[index].value.as_str() {
            "--scope" => {
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
                    return Err("Usage: --scope <session|repo|global>".to_string());
                };
                scope = match value {
                    "session" => AutomationScope::Session,
                    "repo" => AutomationScope::Repo,
                    "global" => AutomationScope::Global,
                    _ => return Err("Usage: --scope <session|repo|global>".to_string()),
                };
                index += 2;
            }
            "--times" => {
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
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
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
                    return Err("Usage: --until <HH:MM>".to_string());
                };
                until_at = Some(parse_until_for_today(now, value)?.timestamp());
                index += 2;
            }
            "--now" => {
                send_now = true;
                index += 1;
            }
            "--last-user-message" | "-l" => {
                use_last_user_message = true;
                index += 1;
            }
            "--policy" => {
                let Some(value) = tokens.get(index + 1).map(|token| token.value.as_str()) else {
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

    let remainder = rebuild_shell_text(input, &tokens[index..]);
    if send_now && policy_command.is_some() {
        return Err("--now cannot be combined with --policy.".to_string());
    }
    let request = if use_last_user_message {
        parse_schedule_only_request(&remainder).map_err(|message| {
            if message == "Usage: <interval|cron> <prompt>" {
                "Usage: $auto every [options] <interval|cron>".to_string()
            } else {
                message
            }
        })?
    } else {
        parse_timer_schedule_request(&remainder, /*default_schedule*/ None).map_err(|message| {
            if message == "Usage: <interval|cron> <prompt>" {
                "Usage: $auto every [options] <interval|cron> <prompt>".to_string()
            } else {
                message
            }
        })?
    };

    let message = if use_last_user_message {
        let Some(message) = last_user_message
            .map(str::trim)
            .filter(|message| !message.is_empty())
        else {
            return Err(
                "No previous text user message is available for --last-user-message.".to_string(),
            );
        };
        message.to_string()
    } else {
        request.prompt
    };

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
            message_source: AutomationMessageSource::Static { message },
            limits: AutomationLimits { max_runs, until_at },
            policy_command,
        },
        note: request.note,
        send_now,
    })
}

pub(crate) fn auto_usage() -> &'static str {
    "Auto\n\
Schedule follow-up prompts after Codex completes work or on a timer.\n\n\
Usage:\n\
  $auto on-complete [options] (<message> | --last-user-message|-l)\n\
  $auto on-complete --round-robin \"msg 1\" \"msg 2\" ...\n\
  $auto every [options] (<interval|cron> <prompt> | --last-user-message|-l <interval|cron>)\n\
  $auto list\n\
  $auto show <runtime-id>\n\
  $auto pause <runtime-id>\n\
  $auto resume <runtime-id>\n\
  $auto rm <runtime-id>\n\n\
Options:\n\
  --scope <session|repo|global>\n\
  --times <count>\n\
  --until <HH:MM>\n\
  --now\n\
  --policy '<command>'\n\n\
Examples:\n\
  $auto on-complete continue working on this\n\
  $auto every 30m run tests\n\
  $auto pause session:auto-1"
}

fn split_shell_tokens(input: &str) -> Result<Vec<ShellToken>, String> {
    let values = shlex::split(input)
        .ok_or_else(|| "Failed to parse $auto arguments. Check your quoting.".to_string())?;
    let spans = shell_token_spans(input)
        .ok_or_else(|| "Failed to parse $auto arguments. Check your quoting.".to_string())?;
    if values.len() != spans.len() {
        return Err("Failed to parse $auto arguments. Check your quoting.".to_string());
    }
    Ok(values
        .into_iter()
        .zip(spans)
        .map(|(value, span)| ShellToken { value, span })
        .collect())
}

fn shell_token_spans(input: &str) -> Option<Vec<Range<usize>>> {
    #[derive(Clone, Copy)]
    enum Mode {
        Normal,
        SingleQuote,
        DoubleQuote,
    }

    let mut spans = Vec::new();
    let mut index = 0;

    while index < input.len() {
        let ch = input[index..].chars().next()?;
        if ch.is_whitespace() {
            index += ch.len_utf8();
            continue;
        }

        let start = index;
        let mut mode = Mode::Normal;
        while index < input.len() {
            let ch = input[index..].chars().next()?;
            match mode {
                Mode::Normal if ch.is_whitespace() => break,
                Mode::Normal if ch == '\'' => {
                    mode = Mode::SingleQuote;
                    index += ch.len_utf8();
                }
                Mode::Normal if ch == '"' => {
                    mode = Mode::DoubleQuote;
                    index += ch.len_utf8();
                }
                Mode::Normal if ch == '\\' => {
                    index += ch.len_utf8();
                    if index < input.len() {
                        let escaped = input[index..].chars().next()?;
                        index += escaped.len_utf8();
                    }
                }
                Mode::Normal => {
                    index += ch.len_utf8();
                }
                Mode::SingleQuote if ch == '\'' => {
                    mode = Mode::Normal;
                    index += ch.len_utf8();
                }
                Mode::SingleQuote => {
                    index += ch.len_utf8();
                }
                Mode::DoubleQuote if ch == '"' => {
                    mode = Mode::Normal;
                    index += ch.len_utf8();
                }
                Mode::DoubleQuote if ch == '\\' => {
                    index += ch.len_utf8();
                    if index < input.len() {
                        let escaped = input[index..].chars().next()?;
                        index += escaped.len_utf8();
                    }
                }
                Mode::DoubleQuote => {
                    index += ch.len_utf8();
                }
            }
        }

        if !matches!(mode, Mode::Normal) {
            return None;
        }
        spans.push(start..index);
    }

    Some(spans)
}

fn rebuild_shell_text(input: &str, tokens: &[ShellToken]) -> String {
    let Some(first) = tokens.first() else {
        return String::new();
    };

    let mut rebuilt = first.value.clone();
    for window in tokens.windows(2) {
        rebuilt.push_str(&input[window[0].span.end..window[1].span.start]);
        rebuilt.push_str(&window[1].value);
    }
    rebuilt
}

fn parse_schedule_only_request(
    input: &str,
) -> Result<super::schedule_parser::TimerScheduleRequest, String> {
    const PLACEHOLDER_PROMPT: &str = "__codex_slop_fork_last_user_message__";

    let request = parse_timer_schedule_request(
        &format!("{input} {PLACEHOLDER_PROMPT}"),
        /*default_schedule*/ None,
    )?;
    if request.prompt == PLACEHOLDER_PROMPT {
        Ok(request)
    } else {
        Err("Usage: <interval|cron> <prompt>".to_string())
    }
}

pub(crate) fn auto_command_mention_item() -> MentionItem {
    MentionItem {
        display_name: AUTO_COMMAND_NAME.to_string(),
        description: Some("automation command".to_string()),
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

/// Returns whether this `$auto` subcommand defines a prompt that Codex will
/// eventually send to the model, either immediately (`--now`) or via the
/// automation registry later.
pub(crate) fn should_record_auto_command_in_history(args: &str) -> bool {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return false;
    }

    let Ok(tokens) = split_shell_tokens(trimmed) else {
        return false;
    };
    if tokens
        .iter()
        .any(|token| matches!(token.value.as_str(), "--last-user-message" | "-l"))
    {
        return false;
    }

    const PLACEHOLDER_LAST_USER_MESSAGE: &str = "__codex_slop_fork_history_probe__";
    matches!(
        parse_auto_command(
            trimmed,
            AutomationScope::Session,
            Local::now(),
            Some(PLACEHOLDER_LAST_USER_MESSAGE),
        ),
        Ok(AutoCommand::Create { .. })
    )
}

pub(crate) fn auto_command_requires_idle_session(args: &str) -> bool {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return false;
    }

    let Ok(tokens) = split_shell_tokens(trimmed) else {
        return false;
    };
    matches!(
        tokens.first().map(|token| token.value.as_str()),
        Some("every" | "on-complete")
    )
}

#[cfg(test)]
mod tests {
    use chrono::Local;
    use pretty_assertions::assert_eq;

    use super::AutoCommand;
    use super::AutomationMessageSource;
    use super::AutomationScope;
    use super::auto_command_requires_idle_session;
    use super::parse_auto_command;
    use super::should_record_auto_command_in_history;

    #[test]
    fn parses_on_complete_now_with_last_user_message_snapshot() {
        let command = parse_auto_command(
            "on-complete --now --last-user-message",
            AutomationScope::Session,
            Local::now(),
            Some("hi"),
        )
        .expect("command");

        let AutoCommand::Create {
            scope,
            spec,
            note,
            send_now,
        } = command
        else {
            panic!("expected create command");
        };
        assert_eq!(scope, AutomationScope::Session);
        assert_eq!(note, None);
        assert_eq!(send_now, true);
        assert_eq!(
            spec.message_source,
            AutomationMessageSource::Static {
                message: "hi".to_string()
            }
        );
    }

    #[test]
    fn parses_every_with_last_user_message_without_prompt() {
        let command = parse_auto_command(
            "every --last-user-message 10m",
            AutomationScope::Session,
            Local::now(),
            Some("run auth"),
        )
        .expect("command");

        let AutoCommand::Create { spec, send_now, .. } = command else {
            panic!("expected create command");
        };
        assert_eq!(send_now, false);
        assert_eq!(
            spec.message_source,
            AutomationMessageSource::Static {
                message: "run auth".to_string()
            }
        );
    }

    #[test]
    fn rejects_last_user_message_without_previous_text() {
        let err = parse_auto_command(
            "on-complete --last-user-message",
            AutomationScope::Session,
            Local::now(),
            /*last_user_message*/ None,
        )
        .expect_err("missing last message should fail");

        assert_eq!(
            err,
            "No previous text user message is available for --last-user-message.".to_string()
        );
    }

    #[test]
    fn rejects_now_with_policy() {
        let err = parse_auto_command(
            "every --now --policy 'echo hi' 10m check deploy",
            AutomationScope::Session,
            Local::now(),
            Some("hi"),
        )
        .expect_err("now plus policy should fail");

        assert_eq!(err, "--now cannot be combined with --policy.".to_string());
    }

    #[test]
    fn preserves_multiline_on_complete_message() {
        let command = parse_auto_command(
            "on-complete first line\nsecond line",
            AutomationScope::Session,
            Local::now(),
            /*last_user_message*/ None,
        )
        .expect("command");

        let AutoCommand::Create { spec, .. } = command else {
            panic!("expected create command");
        };
        assert_eq!(
            spec.message_source,
            AutomationMessageSource::Static {
                message: "first line\nsecond line".to_string()
            }
        );
    }

    #[test]
    fn preserves_multiline_every_prompt() {
        let command = parse_auto_command(
            "every 10m first line\nsecond line",
            AutomationScope::Session,
            Local::now(),
            /*last_user_message*/ None,
        )
        .expect("command");

        let AutoCommand::Create { spec, .. } = command else {
            panic!("expected create command");
        };
        assert_eq!(
            spec.message_source,
            AutomationMessageSource::Static {
                message: "first line\nsecond line".to_string()
            }
        );
    }

    #[test]
    fn history_includes_prompt_driving_auto_commands_only() {
        assert!(should_record_auto_command_in_history(
            "on-complete continue"
        ));
        assert!(should_record_auto_command_in_history("every 10m continue"));
        assert!(!should_record_auto_command_in_history(
            "on-complete --last-user-message"
        ));
        assert!(!should_record_auto_command_in_history("every -l 10m"));
        assert!(!should_record_auto_command_in_history("on-complete"));
        assert!(!should_record_auto_command_in_history(
            "every --bogus 10m continue"
        ));
        assert!(!should_record_auto_command_in_history("list"));
        assert!(!should_record_auto_command_in_history(
            "pause session:auto-1"
        ));
    }

    #[test]
    fn idle_requirement_only_applies_to_auto_create_commands() {
        assert!(auto_command_requires_idle_session("on-complete keep going"));
        assert!(auto_command_requires_idle_session("every 10m run tests"));
        assert!(!auto_command_requires_idle_session("list"));
        assert!(!auto_command_requires_idle_session("pause session:auto-1"));
        assert!(!auto_command_requires_idle_session(""));
        assert!(!auto_command_requires_idle_session("unknown"));
    }
}
