//! Small fixed-offset weekly schedules; no locale, timezone database or cron.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CalendarSchedule {
    time_of_day: String,
    days_of_week: Vec<u8>,
    utc_offset_minutes: i16,
}

impl CalendarSchedule {
    fn minute_of_day(&self) -> Result<u16, String> {
        if self.time_of_day.len() != 5 || !self.time_of_day.is_ascii() {
            return Err("Schedule time must use HH:MM".into());
        }
        let (hour, minute) = self
            .time_of_day
            .split_once(':')
            .ok_or("Schedule time must use HH:MM")?;
        if hour.len() != 2
            || minute.len() != 2
            || !hour
                .bytes()
                .chain(minute.bytes())
                .all(|byte| byte.is_ascii_digit())
        {
            return Err("Schedule time must use HH:MM".into());
        }
        let hour = hour.parse::<u16>().map_err(|_| "Invalid schedule hour")?;
        let minute = minute
            .parse::<u16>()
            .map_err(|_| "Invalid schedule minute")?;
        if hour > 23 || minute > 59 {
            return Err("Schedule time is out of range".into());
        }
        Ok(hour * 60 + minute)
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        self.minute_of_day()?;
        if !(-840..=840).contains(&self.utc_offset_minutes)
            || self.days_of_week.is_empty()
            || self.days_of_week.len() > 7
            || self.days_of_week.iter().any(|day| *day > 6)
            || self.days_of_week.iter().enumerate().any(|(index, day)| {
                self.days_of_week
                    .get(..index)
                    .is_some_and(|previous| previous.contains(day))
            })
        {
            return Err("Choose unique weekdays 0–6 and a valid fixed UTC offset".into());
        }
        Ok(())
    }

    pub(crate) fn next_after(&self, at: f64) -> Result<f64, String> {
        self.validate()?;
        if !at.is_finite() || !(0.0..=253_402_300_799.0).contains(&at) {
            return Err("Schedule time is out of range".into());
        }
        let offset = i64::from(self.utc_offset_minutes) * 60;
        let local = at.floor() as i64 + offset;
        let day = local.div_euclid(86_400);
        for ahead in 0..=7 {
            let candidate_day = day + ahead;
            let weekday = (candidate_day + 4).rem_euclid(7) as u8;
            if self.days_of_week.contains(&weekday) {
                let candidate =
                    candidate_day * 86_400 + i64::from(self.minute_of_day()?) * 60 - offset;
                if candidate as f64 > at {
                    return Ok(candidate as f64);
                }
            }
        }
        Err("Schedule has no next run".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn weekly_fixed_offset_schedule_rolls_days_and_weeks_without_drift() {
        let schedule: CalendarSchedule = serde_json::from_value(
            json!({"time_of_day":"03:30","days_of_week":[4],"utc_offset_minutes":60}),
        )
        .unwrap();
        assert_eq!(schedule.next_after(0.0).unwrap(), 9_000.0);
        assert_eq!(schedule.next_after(9_000.0).unwrap(), 613_800.0);
        assert_eq!(schedule.next_after(9_000.5).unwrap(), 613_800.0);
    }

    #[test]
    fn invalid_or_duplicate_days_and_out_of_range_times_are_rejected() {
        for value in [
            json!({"time_of_day":"24:00","days_of_week":[1],"utc_offset_minutes":0}),
            json!({"time_of_day":"03:30","days_of_week":[1,1],"utc_offset_minutes":0}),
            json!({"time_of_day":"03:30","days_of_week":[],"utc_offset_minutes":0}),
            json!({"time_of_day":"03:30","days_of_week":[7],"utc_offset_minutes":0}),
            json!({"time_of_day":"03:30","days_of_week":[1],"utc_offset_minutes":900}),
        ] {
            assert!(
                serde_json::from_value::<CalendarSchedule>(value)
                    .unwrap()
                    .validate()
                    .is_err()
            );
        }
    }
}
