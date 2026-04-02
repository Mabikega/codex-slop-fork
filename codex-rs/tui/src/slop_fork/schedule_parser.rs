use std::ops::Range;
use std::time::Duration;

#[cfg(test)]
use chrono::DateTime;
#[cfg(test)]
use chrono::Datelike;
#[cfg(test)]
use chrono::Local;
#[cfg(test)]
use chrono::TimeDelta;
#[cfg(test)]
use chrono::Timelike;
#[cfg(test)]
use chrono::Weekday;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimerScheduleRequest {
    pub(crate) prompt: String,
    pub(crate) schedule: TimerSchedule,
    pub(crate) schedule_label: String,
    pub(crate) note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TimerSchedule {
    Interval(Duration),
    Cron(CronExpression),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CronExpression {
    expression: String,
    minutes: CronField,
    hours: CronField,
    days_of_month: CronField,
    months: CronField,
    days_of_week: CronField,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CronField {
    any: bool,
    min: u32,
    allowed: Vec<bool>,
}

impl CronExpression {
    pub(crate) fn parse(expression: &str) -> Result<Self, String> {
        let fields = expression.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(
                "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                    .to_string(),
            );
        }
        Ok(Self {
            expression: expression.to_string(),
            minutes: CronField::parse(
                fields[0], /*min*/ 0, /*max*/ 59, "minute", /*day_of_week*/ false,
            )?,
            hours: CronField::parse(
                fields[1], /*min*/ 0, /*max*/ 23, "hour", /*day_of_week*/ false,
            )?,
            days_of_month: CronField::parse(
                fields[2],
                /*min*/ 1,
                /*max*/ 31,
                "day-of-month",
                /*day_of_week*/ false,
            )?,
            months: CronField::parse(
                fields[3], /*min*/ 1, /*max*/ 12, "month", /*day_of_week*/ false,
            )?,
            days_of_week: CronField::parse(
                fields[4],
                /*min*/ 0,
                /*max*/ 6,
                "day-of-week",
                /*day_of_week*/ true,
            )?,
        })
    }

    pub(crate) fn expression(&self) -> &str {
        &self.expression
    }

    #[cfg(test)]
    pub(crate) fn next_after(
        &self,
        after: DateTime<Local>,
        latest_allowed: DateTime<Local>,
    ) -> Option<DateTime<Local>> {
        let mut candidate = next_minute_boundary(after)?;
        while candidate <= latest_allowed {
            if self.matches(candidate) {
                return Some(candidate);
            }
            candidate += TimeDelta::minutes(1);
        }
        None
    }

    #[cfg(test)]
    fn matches(&self, date_time: DateTime<Local>) -> bool {
        if !self.minutes.matches(date_time.minute()) {
            return false;
        }
        if !self.hours.matches(date_time.hour()) {
            return false;
        }
        if !self.months.matches(date_time.month()) {
            return false;
        }
        let day_of_month_matches = self.days_of_month.matches(date_time.day());
        let day_of_week_matches = self
            .days_of_week
            .matches(day_of_week_value(date_time.weekday()));
        if self.days_of_month.any && self.days_of_week.any {
            day_of_month_matches && day_of_week_matches
        } else if self.days_of_month.any {
            day_of_week_matches
        } else if self.days_of_week.any {
            day_of_month_matches
        } else {
            day_of_month_matches || day_of_week_matches
        }
    }
}

impl CronField {
    fn parse(
        input: &str,
        min: u32,
        max: u32,
        field_name: &str,
        day_of_week: bool,
    ) -> Result<Self, String> {
        let mut allowed = vec![false; (max + 1) as usize];
        let any = input == "*";
        for segment in input.split(',') {
            if segment.is_empty() {
                return Err(format!(
                    "Cron {field_name} field contains an empty segment."
                ));
            }
            let (range_part, step) = if let Some((range_part, step_part)) = segment.split_once('/')
            {
                let step = step_part.parse::<u32>().map_err(|_| {
                    format!("Cron {field_name} field has invalid step value {step_part:?}.")
                })?;
                if step == 0 {
                    return Err(format!(
                        "Cron {field_name} field step must be greater than 0."
                    ));
                }
                (range_part, step)
            } else {
                (segment, 1)
            };

            let values = if range_part == "*" {
                (min..=max).collect::<Vec<_>>()
            } else if let Some((raw_start, raw_end)) = range_part.split_once('-') {
                let start = normalize_cron_value(
                    parse_cron_value(raw_start, min, max, field_name, day_of_week)?,
                    day_of_week,
                );
                let end = normalize_cron_value(
                    parse_cron_value(raw_end, min, max, field_name, day_of_week)?,
                    day_of_week,
                );
                expand_range(start, end, min, max, field_name, day_of_week)?
            } else {
                vec![normalize_cron_value(
                    parse_cron_value(range_part, min, max, field_name, day_of_week)?,
                    day_of_week,
                )]
            };

            for (index, value) in values.into_iter().enumerate() {
                if index % step as usize == 0 {
                    allowed[value as usize] = true;
                }
            }
        }
        Ok(Self { any, min, allowed })
    }

    #[cfg(test)]
    fn matches(&self, value: u32) -> bool {
        if value < self.min {
            return false;
        }
        self.allowed.get(value as usize).copied().unwrap_or(false)
    }
}

pub(crate) fn parse_timer_schedule_request(
    input: &str,
    default_schedule: Option<Duration>,
) -> Result<TimerScheduleRequest, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(timer_schedule_usage(default_schedule).to_string());
    }

    if let Some(request) = parse_prefixed_schedule(trimmed)? {
        if request.prompt.trim().is_empty() {
            return Err(timer_schedule_usage(default_schedule).to_string());
        }
        return Ok(request);
    }
    if let Some(request) = parse_trailing_every_schedule(trimmed)? {
        if request.prompt.trim().is_empty() {
            return Err(timer_schedule_usage(default_schedule).to_string());
        }
        return Ok(request);
    }
    if let Some(default_schedule) = default_schedule {
        return Ok(TimerScheduleRequest {
            prompt: trimmed.to_string(),
            schedule: TimerSchedule::Interval(default_schedule),
            schedule_label: compact_duration_label(default_schedule),
            note: None,
        });
    }
    Err(timer_schedule_usage(default_schedule).to_string())
}

pub(crate) fn compact_duration_label(duration: Duration) -> String {
    let minutes = duration.as_secs() / 60;
    if minutes.is_multiple_of(24 * 60) {
        return format!("{}d", minutes / (24 * 60));
    }
    if minutes.is_multiple_of(60) {
        return format!("{}h", minutes / 60);
    }
    format!("{minutes}m")
}

pub(crate) fn timer_schedule_usage(default_schedule: Option<Duration>) -> &'static str {
    if default_schedule.is_some() {
        "Usage: [interval|cron] <prompt>"
    } else {
        "Usage: <interval|cron> <prompt>"
    }
}

struct WordSpan<'a> {
    word: &'a str,
    span: Range<usize>,
}

fn split_whitespace_with_spans(input: &str) -> Vec<WordSpan<'_>> {
    let mut words = Vec::new();
    let mut start = None;

    for (idx, ch) in input.char_indices() {
        if ch.is_whitespace() {
            if let Some(start_idx) = start.take() {
                words.push(WordSpan {
                    word: &input[start_idx..idx],
                    span: start_idx..idx,
                });
            }
        } else if start.is_none() {
            start = Some(idx);
        }
    }

    if let Some(start_idx) = start {
        words.push(WordSpan {
            word: &input[start_idx..],
            span: start_idx..input.len(),
        });
    }

    words
}

fn prompt_from_word(input: &str, words: &[WordSpan<'_>], index: usize) -> String {
    words
        .get(index)
        .map_or_else(String::new, |word| input[word.span.start..].to_string())
}

fn prompt_before_word(input: &str, words: &[WordSpan<'_>], index: usize) -> String {
    words.get(index).map_or_else(String::new, |word| {
        input[..word.span.start].trim_end().to_string()
    })
}

fn parse_prefixed_schedule(input: &str) -> Result<Option<TimerScheduleRequest>, String> {
    let words = split_whitespace_with_spans(input);
    if words
        .first()
        .is_some_and(|word| word.word.eq_ignore_ascii_case("cron:"))
    {
        if words.len() < 6 {
            return Err(
                "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                    .to_string(),
            );
        }

        let candidate = words[1..6]
            .iter()
            .map(|word| word.word)
            .collect::<Vec<_>>()
            .join(" ");
        let cron = maybe_parse_cron_expression(&format!("cron:{candidate}"))?.ok_or_else(|| {
            "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                .to_string()
        })?;
        return Ok(Some(TimerScheduleRequest {
            prompt: prompt_from_word(input, &words, /*index*/ 6),
            schedule: TimerSchedule::Cron(cron.clone()),
            schedule_label: format!("cron {}", cron.expression()),
            note: Some("Cron schedules use your local timezone.".to_string()),
        }));
    }
    if input
        .trim_start()
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cron:"))
        && words.len() < 5
    {
        return Err(
            "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                .to_string(),
        );
    }
    if words.len() >= 5 {
        let candidate = words[..5]
            .iter()
            .map(|word| word.word)
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(cron) = maybe_parse_cron_expression(&candidate)? {
            return Ok(Some(TimerScheduleRequest {
                prompt: prompt_from_word(input, &words, /*index*/ 5),
                schedule: TimerSchedule::Cron(cron.clone()),
                schedule_label: format!("cron {}", cron.expression()),
                note: Some("Cron schedules use your local timezone.".to_string()),
            }));
        }
    }

    let Some(split_at) = input.find(char::is_whitespace) else {
        return Ok(None);
    };
    let first = input[..split_at].trim();
    let rest = input[split_at..].trim();
    let Some((cadence, note)) = parse_interval_token(first).ok() else {
        return Ok(None);
    };
    Ok(Some(TimerScheduleRequest {
        prompt: rest.to_string(),
        schedule: TimerSchedule::Interval(cadence),
        schedule_label: compact_duration_label(cadence),
        note,
    }))
}

fn parse_trailing_every_schedule(input: &str) -> Result<Option<TimerScheduleRequest>, String> {
    let words = split_whitespace_with_spans(input);
    if words.len() < 3 {
        return Ok(None);
    }
    if words.len() >= 2
        && words[words.len() - 2].word.eq_ignore_ascii_case("every")
        && words
            .last()
            .and_then(|word| word.word.get(..5))
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cron:"))
    {
        return Err(
            "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                .to_string(),
        );
    }

    if words.len() >= 7 {
        let every_index = words.len().saturating_sub(7);
        if words[every_index].word.eq_ignore_ascii_case("every") {
            let candidate = words[every_index + 1..]
                .iter()
                .map(|word| word.word)
                .collect::<Vec<_>>()
                .join(" ");
            if looks_like_six_field_cron(&candidate) {
                return Err(
                    "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                        .to_string(),
                );
            }
        }
    }
    if words.len() >= 6 {
        let every_index = words.len().saturating_sub(6);
        if words[every_index].word.eq_ignore_ascii_case("every") {
            let candidate = words[every_index + 1..]
                .iter()
                .map(|word| word.word)
                .collect::<Vec<_>>()
                .join(" ");
            if looks_like_six_field_cron(&candidate) {
                return Err(
                    "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                        .to_string(),
                );
            }
            if let Some(cron) = maybe_parse_cron_expression(&candidate)? {
                return Ok(Some(TimerScheduleRequest {
                    prompt: prompt_before_word(input, &words, every_index),
                    schedule: TimerSchedule::Cron(cron.clone()),
                    schedule_label: format!("cron {}", cron.expression()),
                    note: Some("Cron schedules use your local timezone.".to_string()),
                }));
            }
        }
    }

    let last = words.last().map_or("", |word| word.word);
    let penultimate = words
        .get(words.len().saturating_sub(2))
        .map_or("", |word| word.word);
    let antepenultimate = words
        .get(words.len().saturating_sub(3))
        .map_or("", |word| word.word);

    if !antepenultimate.eq_ignore_ascii_case("every") {
        return Ok(None);
    }
    if let Ok((cadence, note)) = parse_interval_pair(penultimate, last) {
        return Ok(Some(TimerScheduleRequest {
            prompt: prompt_before_word(input, &words, words.len() - 3),
            schedule: TimerSchedule::Interval(cadence),
            schedule_label: compact_duration_label(cadence),
            note,
        }));
    }

    if penultimate.eq_ignore_ascii_case("every")
        && let Ok((cadence, note)) = parse_interval_token(last)
    {
        return Ok(Some(TimerScheduleRequest {
            prompt: prompt_before_word(input, &words, words.len() - 2),
            schedule: TimerSchedule::Interval(cadence),
            schedule_label: compact_duration_label(cadence),
            note,
        }));
    }

    Ok(None)
}

fn maybe_parse_cron_expression(candidate: &str) -> Result<Option<CronExpression>, String> {
    let (normalized, explicit) = normalize_cron_candidate(candidate);
    let fields = normalized.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 5 {
        return if explicit {
            Err(
                "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
                    .to_string(),
            )
        } else {
            Ok(None)
        };
    }

    let looks_like_cron = explicit
        || fields
            .iter()
            .all(|field| looks_like_cron_field_token(field));
    if !looks_like_cron {
        return Ok(None);
    }

    CronExpression::parse(&normalized).map(Some)
}

fn normalize_cron_candidate(candidate: &str) -> (String, bool) {
    let mut normalized = candidate.trim().to_string();
    let mut explicit = false;

    loop {
        let mut changed = false;
        if let Some(stripped) = strip_matching_quotes(&normalized) {
            normalized = stripped.to_string();
            explicit = true;
            changed = true;
        }
        if normalized
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cron:"))
        {
            normalized = normalized[5..].trim().to_string();
            explicit = true;
            changed = true;
        }
        if !changed {
            break;
        }
    }

    (normalized, explicit)
}

fn looks_like_six_field_cron(candidate: &str) -> bool {
    let (normalized, _) = normalize_cron_candidate(candidate);
    let fields = normalized.split_whitespace().collect::<Vec<_>>();
    fields.len() == 6
        && fields
            .iter()
            .all(|field| looks_like_cron_field_token(field))
}

fn looks_like_cron_field_token(field: &str) -> bool {
    !field.is_empty()
        && field
            .chars()
            .all(|ch| ch.is_ascii_digit() || matches!(ch, '*' | ',' | '-' | '/'))
}

fn strip_matching_quotes(input: &str) -> Option<&str> {
    let trimmed = input.trim();
    if trimmed.len() < 2 {
        return None;
    }
    let first = trimmed.chars().next()?;
    let last = trimmed.chars().last()?;
    if (first == '"' || first == '\'') && first == last {
        return trimmed.get(1..trimmed.len().saturating_sub(1));
    }
    None
}

fn parse_interval_pair(number: &str, unit_word: &str) -> Result<(Duration, Option<String>), ()> {
    let value = number.parse::<u64>().map_err(|_| ())?;
    duration_from_parts(value, unit_word)
}

fn parse_interval_token(token: &str) -> Result<(Duration, Option<String>), ()> {
    if token.len() < 2 {
        return Err(());
    }
    let split_at = token.len().saturating_sub(1);
    let (value, unit) = token.split_at(split_at);
    let value = value.parse::<u64>().map_err(|_| ())?;
    duration_from_parts(value, unit)
}

fn duration_from_parts(value: u64, unit: &str) -> Result<(Duration, Option<String>), ()> {
    let normalized_unit = unit.to_ascii_lowercase();
    let seconds = match normalized_unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => value,
        "m" | "min" | "mins" | "minute" | "minutes" => value.saturating_mul(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => value.saturating_mul(60 * 60),
        "d" | "day" | "days" => value.saturating_mul(24 * 60 * 60),
        _ => return Err(()),
    };
    let rounded_minutes = seconds.saturating_add(59) / 60;
    let cadence = Duration::from_secs(rounded_minutes.max(1).saturating_mul(60));
    let note = (seconds % 60 != 0).then(|| {
        format!(
            "Seconds are rounded up to the nearest minute. Using {}.",
            compact_duration_label(cadence)
        )
    });
    Ok((cadence, note))
}

#[cfg(test)]
fn day_of_week_value(day: Weekday) -> u32 {
    day.num_days_from_sunday()
}

fn parse_cron_value(
    raw: &str,
    min: u32,
    max: u32,
    field_name: &str,
    day_of_week: bool,
) -> Result<u32, String> {
    let value = raw.parse::<u32>().map_err(|_| {
        format!("Cron {field_name} field has invalid value {raw:?}; expected a number.")
    })?;
    if day_of_week {
        if value > 7 {
            return Err(format!(
                "Cron {field_name} field value {value} is out of range {min}-{max}."
            ));
        }
        return Ok(value);
    }
    if value < min || value > max {
        return Err(format!(
            "Cron {field_name} field value {value} is out of range {min}-{max}."
        ));
    }
    Ok(value)
}

fn normalize_cron_value(value: u32, day_of_week: bool) -> u32 {
    if day_of_week && value == 7 { 0 } else { value }
}

fn expand_range(
    start: u32,
    end: u32,
    min: u32,
    max: u32,
    field_name: &str,
    day_of_week: bool,
) -> Result<Vec<u32>, String> {
    if day_of_week {
        if start == 7 && end == 7 {
            return Ok(vec![0]);
        }
        if start > end {
            return Err(format!(
                "Cron {field_name} field range {start}-{end} must be ascending."
            ));
        }
        if end == 7 {
            let mut values = (start..=6).collect::<Vec<_>>();
            values.push(0);
            return Ok(values);
        }
    }

    if start < min || end > max || start > end {
        return Err(format!(
            "Cron {field_name} field range {start}-{end} is invalid for {min}-{max}."
        ));
    }
    Ok((start..=end).collect())
}

#[cfg(test)]
fn next_minute_boundary(after: DateTime<Local>) -> Option<DateTime<Local>> {
    let candidate = after + TimeDelta::minutes(1);
    let candidate = candidate
        - TimeDelta::seconds(i64::from(candidate.second()))
        - TimeDelta::nanoseconds(i64::from(candidate.nanosecond()));
    Some(candidate)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Local;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    use super::CronExpression;
    use super::TimerSchedule;
    use super::parse_timer_schedule_request;

    #[test]
    fn parses_prefixed_interval_schedule() {
        let request = parse_timer_schedule_request("5m /review", /*default_schedule*/ None)
            .expect("schedule");
        assert_eq!(request.prompt, "/review");
        assert_eq!(
            request.schedule,
            TimerSchedule::Interval(Duration::from_secs(5 * 60))
        );
    }

    #[test]
    fn parses_trailing_every_interval_schedule() {
        let request = parse_timer_schedule_request(
            "run tests every 5 minutes",
            /*default_schedule*/ None,
        )
        .expect("schedule");
        assert_eq!(request.prompt, "run tests");
        assert_eq!(
            request.schedule,
            TimerSchedule::Interval(Duration::from_secs(5 * 60))
        );
    }

    #[test]
    fn parses_trailing_every_interval_schedule_with_multiline_prompt() {
        let request = parse_timer_schedule_request(
            "run tests\nthen report every 5 minutes",
            /*default_schedule*/ None,
        )
        .expect("schedule");
        assert_eq!(request.prompt, "run tests\nthen report");
        assert_eq!(
            request.schedule,
            TimerSchedule::Interval(Duration::from_secs(5 * 60))
        );
    }

    #[test]
    fn parses_prefixed_cron_schedule() {
        let request = parse_timer_schedule_request(
            "*/15 * * * * check deploy",
            /*default_schedule*/ None,
        )
        .expect("schedule");
        assert_eq!(request.prompt, "check deploy");
        let TimerSchedule::Cron(cron) = request.schedule else {
            panic!("expected cron schedule");
        };
        assert_eq!(cron.expression(), "*/15 * * * *");
    }

    #[test]
    fn parses_trailing_every_cron_schedule() {
        let request = parse_timer_schedule_request(
            "check deploy every \"0 14 * * 1-5\"",
            /*default_schedule*/ None,
        )
        .expect("schedule");
        assert_eq!(request.prompt, "check deploy");
        let TimerSchedule::Cron(cron) = request.schedule else {
            panic!("expected cron schedule");
        };
        assert_eq!(cron.expression(), "0 14 * * 1-5");
    }

    #[test]
    fn parses_prefixed_numeric_cron_schedule() {
        let request =
            parse_timer_schedule_request("0 14 1 1 1 check deploy", /*default_schedule*/ None)
                .expect("schedule");
        assert_eq!(request.prompt, "check deploy");
        let TimerSchedule::Cron(cron) = request.schedule else {
            panic!("expected cron schedule");
        };
        assert_eq!(cron.expression(), "0 14 1 1 1");
    }

    #[test]
    fn parses_prefixed_cron_schedule_with_numeric_prompt_token() {
        let request =
            parse_timer_schedule_request("0 14 * * 1-5 404", /*default_schedule*/ None)
                .expect("schedule");
        assert_eq!(request.prompt, "404");
        let TimerSchedule::Cron(cron) = request.schedule else {
            panic!("expected cron schedule");
        };
        assert_eq!(cron.expression(), "0 14 * * 1-5");
    }

    #[test]
    fn parses_prefixed_cron_schedule_with_cron_like_prompt_token() {
        let request =
            parse_timer_schedule_request("*/15 * * * * 1-5", /*default_schedule*/ None)
                .expect("schedule");
        assert_eq!(request.prompt, "1-5");
        let TimerSchedule::Cron(cron) = request.schedule else {
            panic!("expected cron schedule");
        };
        assert_eq!(cron.expression(), "*/15 * * * *");
    }

    #[test]
    fn parses_explicit_cron_prefix_schedule() {
        let request = parse_timer_schedule_request(
            "cron: */15 * * * * check deploy",
            /*default_schedule*/ None,
        )
        .expect("schedule");
        assert_eq!(request.prompt, "check deploy");
        let TimerSchedule::Cron(cron) = request.schedule else {
            panic!("expected cron schedule");
        };
        assert_eq!(cron.expression(), "*/15 * * * *");
    }

    #[test]
    fn rejects_six_field_cron_schedule() {
        let err = parse_timer_schedule_request(
            "check deploy every 0 12 * * * *",
            /*default_schedule*/ None,
        )
        .expect_err("invalid cron");
        assert_eq!(
            err,
            "Cron schedules must have five fields: minute hour day-of-month month day-of-week."
        );
    }

    #[test]
    fn rounds_seconds_up_to_minutes() {
        let request =
            parse_timer_schedule_request("30s check deploy", /*default_schedule*/ None)
                .expect("schedule");
        assert_eq!(
            request.schedule,
            TimerSchedule::Interval(Duration::from_secs(60))
        );
        assert_eq!(
            request.note,
            Some("Seconds are rounded up to the nearest minute. Using 1m.".to_string())
        );
    }

    #[test]
    fn supports_default_interval_when_configured() {
        let default_interval = Duration::from_secs(10 * 60);
        let request = parse_timer_schedule_request("check every PR", Some(default_interval))
            .expect("schedule");
        assert_eq!(request.prompt, "check every PR");
        assert_eq!(request.schedule, TimerSchedule::Interval(default_interval));
    }

    #[test]
    fn cron_next_after_supports_dom_or_dow_semantics() {
        let cron = CronExpression::parse("0 14 * * 1-5").expect("cron");
        let now = Local
            .with_ymd_and_hms(2026, 3, 14, 13, 0, 0)
            .single()
            .unwrap();
        let latest = Local
            .with_ymd_and_hms(2026, 3, 20, 14, 0, 0)
            .single()
            .unwrap();
        let next = cron.next_after(now, latest).expect("next cron fire");
        assert_eq!(
            next,
            Local
                .with_ymd_and_hms(2026, 3, 16, 14, 0, 0)
                .single()
                .unwrap()
        );
    }
}
