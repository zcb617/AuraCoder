use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
};
use chrono_tz::Tz;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntervalSchedule {
    every: i64,
    unit: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DailySchedule {
    time: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WeeklySchedule {
    every_weeks: i64,
    weekdays: Vec<u32>,
    time: String,
    anchor_date: String,
}

pub fn validate_schedule(
    schedule_type: &str,
    schedule: &Value,
    timezone: &str,
) -> Result<(), String> {
    parse_timezone(timezone)?;
    match schedule_type {
        "interval" => {
            interval_duration(schedule)?;
        }
        "daily" => {
            let config: DailySchedule = serde_json::from_value(schedule.clone())
                .map_err(|error| format!("invalid daily schedule: {error}"))?;
            parse_time(&config.time)?;
        }
        "weekly" => {
            let config = weekly_config(schedule)?;
            parse_time(&config.time)?;
            NaiveDate::parse_from_str(&config.anchor_date, "%Y-%m-%d")
                .map_err(|_| "weekly anchorDate must use YYYY-MM-DD".to_string())?;
        }
        _ => return Err(format!("unsupported schedule type: {schedule_type}")),
    }
    Ok(())
}

pub fn initial_next_run_at(
    schedule_type: &str,
    schedule: &Value,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    validate_schedule(schedule_type, schedule, timezone)?;
    match schedule_type {
        "interval" => Ok(now + interval_duration(schedule)?),
        "daily" => next_daily(schedule, timezone, now),
        "weekly" => next_weekly(schedule, timezone, now),
        _ => Err(format!("unsupported schedule type: {schedule_type}")),
    }
}

pub fn next_run_after_due(
    schedule_type: &str,
    schedule: &Value,
    timezone: &str,
    scheduled_for: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    validate_schedule(schedule_type, schedule, timezone)?;
    match schedule_type {
        "interval" => {
            let interval = interval_duration(schedule)?;
            let interval_seconds = interval.num_seconds();
            let elapsed_seconds = now
                .signed_duration_since(scheduled_for)
                .num_seconds()
                .max(0);
            let steps = (elapsed_seconds / interval_seconds) + 1;
            Ok(scheduled_for + Duration::seconds(interval_seconds * steps))
        }
        "daily" => next_daily(schedule, timezone, now),
        "weekly" => next_weekly(schedule, timezone, now),
        _ => Err(format!("unsupported schedule type: {schedule_type}")),
    }
}

fn interval_duration(schedule: &Value) -> Result<Duration, String> {
    let config: IntervalSchedule = serde_json::from_value(schedule.clone())
        .map_err(|error| format!("invalid interval schedule: {error}"))?;
    if config.every <= 0 {
        return Err("interval every must be greater than zero".to_string());
    }
    let duration = match config.unit.as_str() {
        "minutes" => Duration::minutes(config.every),
        "hours" => Duration::hours(config.every),
        "days" => Duration::days(config.every),
        _ => return Err("interval unit must be minutes, hours, or days".to_string()),
    };
    if duration.num_seconds() < 60 {
        return Err("interval must be at least one minute".to_string());
    }
    Ok(duration)
}

fn next_daily(
    schedule: &Value,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let config: DailySchedule = serde_json::from_value(schedule.clone())
        .map_err(|error| format!("invalid daily schedule: {error}"))?;
    let time = parse_time(&config.time)?;
    let tz = parse_timezone(timezone)?;
    let local_now = now.with_timezone(&tz);
    for offset in 0..=2 {
        let date = local_now.date_naive() + Duration::days(offset);
        let candidate = resolve_local_datetime(tz, date.and_time(time))?;
        if candidate.with_timezone(&Utc) > now {
            return Ok(candidate.with_timezone(&Utc));
        }
    }
    Err("failed to calculate next daily run".to_string())
}

fn next_weekly(
    schedule: &Value,
    timezone: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, String> {
    let config = weekly_config(schedule)?;
    let time = parse_time(&config.time)?;
    let anchor = NaiveDate::parse_from_str(&config.anchor_date, "%Y-%m-%d")
        .map_err(|_| "weekly anchorDate must use YYYY-MM-DD".to_string())?;
    let tz = parse_timezone(timezone)?;
    let local_now = now.with_timezone(&tz);
    let anchor_week_start = anchor - Duration::days(anchor.weekday().num_days_from_monday() as i64);

    for offset in 0..=(366 * 10) {
        let date = local_now.date_naive() + Duration::days(offset);
        let days_from_anchor = date.signed_duration_since(anchor_week_start).num_days();
        if days_from_anchor < 0 {
            continue;
        }
        let week_index = days_from_anchor / 7;
        if week_index % config.every_weeks != 0
            || !config
                .weekdays
                .contains(&date.weekday().number_from_monday())
        {
            continue;
        }
        let candidate = resolve_local_datetime(tz, date.and_time(time))?;
        if candidate.with_timezone(&Utc) > now {
            return Ok(candidate.with_timezone(&Utc));
        }
    }
    Err("failed to calculate next weekly run within ten years".to_string())
}

fn weekly_config(schedule: &Value) -> Result<WeeklySchedule, String> {
    let config: WeeklySchedule = serde_json::from_value(schedule.clone())
        .map_err(|error| format!("invalid weekly schedule: {error}"))?;
    if config.every_weeks <= 0 {
        return Err("weekly everyWeeks must be greater than zero".to_string());
    }
    if config.weekdays.is_empty()
        || config
            .weekdays
            .iter()
            .any(|weekday| !(1..=7).contains(weekday))
    {
        return Err("weekly weekdays must contain values from 1 to 7".to_string());
    }
    Ok(config)
}

fn parse_timezone(timezone: &str) -> Result<Tz, String> {
    timezone
        .parse::<Tz>()
        .map_err(|_| format!("unknown timezone: {timezone}"))
}

fn parse_time(value: &str) -> Result<NaiveTime, String> {
    NaiveTime::parse_from_str(value, "%H:%M")
        .map_err(|_| "time must use HH:mm in 24-hour format".to_string())
}

fn resolve_local_datetime(tz: Tz, value: NaiveDateTime) -> Result<DateTime<Tz>, String> {
    match tz.from_local_datetime(&value) {
        LocalResult::Single(value) => Ok(value),
        LocalResult::Ambiguous(first, second) => Ok(first.min(second)),
        LocalResult::None => {
            for offset in 1..=180 {
                let adjusted = value + Duration::minutes(offset);
                match tz.from_local_datetime(&adjusted) {
                    LocalResult::Single(value) => return Ok(value),
                    LocalResult::Ambiguous(first, second) => return Ok(first.min(second)),
                    LocalResult::None => continue,
                }
            }
            Err("local scheduled time does not exist in the selected timezone".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    #[test]
    fn interval_keeps_original_cadence_when_runs_were_missed() {
        let scheduled = Utc.with_ymd_and_hms(2026, 8, 9, 8, 0, 0).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 8, 9, 8, 16, 30).unwrap();
        let next = next_run_after_due(
            "interval",
            &json!({"every": 5, "unit": "minutes"}),
            "UTC",
            scheduled,
            now,
        )
        .unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 8, 9, 8, 20, 0).unwrap());
    }

    #[test]
    fn daily_uses_saved_timezone() {
        let now = Utc.with_ymd_and_hms(2026, 8, 9, 0, 30, 0).unwrap();
        let next =
            initial_next_run_at("daily", &json!({"time": "09:00"}), "Asia/Hong_Kong", now).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 8, 9, 1, 0, 0).unwrap());
    }

    #[test]
    fn weekly_honors_week_interval_and_selected_days() {
        let now = Utc.with_ymd_and_hms(2026, 8, 10, 2, 0, 0).unwrap();
        let next = initial_next_run_at(
            "weekly",
            &json!({
                "everyWeeks": 2,
                "weekdays": [3],
                "time": "09:00",
                "anchorDate": "2026-08-10"
            }),
            "Asia/Hong_Kong",
            now,
        )
        .unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 8, 12, 1, 0, 0).unwrap());
    }

    #[test]
    fn invalid_weekdays_are_rejected() {
        let error = validate_schedule(
            "weekly",
            &json!({
                "everyWeeks": 1,
                "weekdays": [],
                "time": "09:00",
                "anchorDate": "2026-08-10"
            }),
            "UTC",
        )
        .unwrap_err();
        assert!(error.contains("weekdays"));
    }
}
