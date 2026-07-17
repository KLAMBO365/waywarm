use anyhow::{Result, bail};
use chrono::{DateTime, Local, TimeZone, Timelike};
use sunrise::{Coordinates, DawnType, SolarDay, SolarEvent};

use crate::config::{Levels, Schedule, ScheduleTiming, Settings, parse_time};

/// Effective day/night start minutes for the schedule at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedTimes {
    pub day_start: u16,
    pub night_start: u16,
    pub transition_minutes: u16,
}

pub fn current_levels(settings: &Settings, now: DateTime<Local>) -> Result<Levels> {
    if !settings.enabled {
        return Ok(Levels::NEUTRAL);
    }
    if !settings.automatic {
        return Ok(settings.manual);
    }
    let minute = now.hour() as f64 * 60.0 + now.minute() as f64 + now.second() as f64 / 60.0;
    let times = resolve_times(&settings.schedule, now)?;
    scheduled_levels_with_times(&settings.schedule, times, minute)
}

pub fn scheduled_levels(schedule: &Schedule, minute: f64) -> Result<Levels> {
    let times = ResolvedTimes {
        day_start: parse_time(&schedule.day_start)?,
        night_start: parse_time(&schedule.night_start)?,
        transition_minutes: schedule.transition_minutes,
    };
    scheduled_levels_with_times(schedule, times, minute)
}

pub fn scheduled_levels_with_times(
    schedule: &Schedule,
    times: ResolvedTimes,
    minute: f64,
) -> Result<Levels> {
    let day = times.day_start as f64;
    let night = times.night_start as f64;
    let duration = times.transition_minutes as f64;
    let day_levels = schedule.day;
    let night_levels = schedule.night;

    let day_progress = transition_progress(minute, day, duration);
    if day_progress < 1.0 {
        return Ok(interpolate(night_levels, day_levels, day_progress));
    }

    let since_day = forward_distance(day, minute);
    let until_night = forward_distance(day, night);
    if since_day < until_night {
        return Ok(day_levels);
    }

    let night_progress = transition_progress(minute, night, duration);
    if night_progress < 1.0 {
        return Ok(interpolate(day_levels, night_levels, night_progress));
    }
    Ok(night_levels)
}

/// Resolve clock or civil-twilight start times for `now`'s local calendar day.
pub fn resolve_times(schedule: &Schedule, now: DateTime<Local>) -> Result<ResolvedTimes> {
    match schedule.timing {
        ScheduleTiming::Fixed => Ok(ResolvedTimes {
            day_start: parse_time(&schedule.day_start)?,
            night_start: parse_time(&schedule.night_start)?,
            transition_minutes: schedule.transition_minutes,
        }),
        ScheduleTiming::Location => match civil_twilight_minutes(schedule, now) {
            Ok(times) => Ok(clamp_transition(times, schedule.transition_minutes)),
            Err(_) => Ok(ResolvedTimes {
                day_start: parse_time(&schedule.day_start)?,
                night_start: parse_time(&schedule.night_start)?,
                transition_minutes: schedule.transition_minutes,
            }),
        },
    }
}

fn civil_twilight_minutes(schedule: &Schedule, now: DateTime<Local>) -> Result<ResolvedTimes> {
    let Some(coord) = Coordinates::new(schedule.latitude, schedule.longitude) else {
        bail!("invalid latitude/longitude");
    };
    let date = now.date_naive();
    let solar = SolarDay::new(coord, date);
    let dawn = solar
        .event_time(SolarEvent::Dawn(DawnType::Civil))
        .ok_or_else(|| anyhow::anyhow!("civil dawn does not occur at this location today"))?;
    let dusk = solar
        .event_time(SolarEvent::Dusk(DawnType::Civil))
        .ok_or_else(|| anyhow::anyhow!("civil dusk does not occur at this location today"))?;

    let day_start = utc_to_local_minute(dawn, now.offset())?;
    let night_start = utc_to_local_minute(dusk, now.offset())?;
    if day_start == night_start {
        bail!("civil dawn and dusk coincide");
    }
    Ok(ResolvedTimes {
        day_start,
        night_start,
        transition_minutes: schedule.transition_minutes,
    })
}

fn utc_to_local_minute(
    utc: chrono::DateTime<chrono::Utc>,
    offset: &chrono::FixedOffset,
) -> Result<u16> {
    let local = offset.from_utc_datetime(&utc.naive_utc());
    Ok((local.hour() * 60 + local.minute()) as u16)
}

fn clamp_transition(mut times: ResolvedTimes, requested: u16) -> ResolvedTimes {
    let gap = times.night_start.abs_diff(times.day_start);
    let shortest = gap.min(24 * 60 - gap);
    times.transition_minutes = requested.min(shortest);
    times
}

fn transition_progress(now: f64, start: f64, duration: f64) -> f64 {
    if duration == 0.0 {
        return 1.0;
    }
    (forward_distance(start, now) / duration).clamp(0.0, 1.0)
}

fn forward_distance(from: f64, to: f64) -> f64 {
    (to - from).rem_euclid(1440.0)
}

fn interpolate(from: Levels, to: Levels, progress: f64) -> Levels {
    Levels {
        warmth: lerp(from.warmth, to.warmth, progress),
        brightness: lerp(from.brightness, to.brightness, progress),
    }
}

fn lerp(from: u8, to: u8, progress: f64) -> u8 {
    (from as f64 + (to as f64 - from as f64) * progress).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn schedule() -> Schedule {
        Schedule::default()
    }

    #[test]
    fn returns_day_and_night_targets() {
        assert_eq!(
            scheduled_levels(&schedule(), 12.0 * 60.0).unwrap(),
            schedule().day
        );
        assert_eq!(
            scheduled_levels(&schedule(), 23.0 * 60.0).unwrap(),
            schedule().night
        );
        assert_eq!(
            scheduled_levels(&schedule(), 2.0 * 60.0).unwrap(),
            schedule().night
        );
    }

    #[test]
    fn interpolates_both_transitions() {
        assert_eq!(
            scheduled_levels(&schedule(), 21.0 * 60.0 + 15.0).unwrap(),
            Levels {
                warmth: 25,
                brightness: 95
            }
        );
        assert_eq!(
            scheduled_levels(&schedule(), 7.0 * 60.0 + 15.0).unwrap(),
            Levels {
                warmth: 25,
                brightness: 95
            }
        );
    }

    #[test]
    fn supports_instant_transitions() {
        let mut value = schedule();
        value.transition_minutes = 0;
        assert_eq!(scheduled_levels(&value, 21.0 * 60.0).unwrap(), value.night);
        assert_eq!(scheduled_levels(&value, 7.0 * 60.0).unwrap(), value.day);
    }

    #[test]
    fn uses_custom_day_levels() {
        let mut value = schedule();
        value.day = Levels {
            warmth: 20,
            brightness: 100,
        };
        value.night = Levels {
            warmth: 60,
            brightness: 80,
        };
        assert_eq!(scheduled_levels(&value, 12.0 * 60.0).unwrap(), value.day);
        assert_eq!(scheduled_levels(&value, 23.0 * 60.0).unwrap(), value.night);
        assert_eq!(
            scheduled_levels(&value, 21.0 * 60.0 + 15.0).unwrap(),
            Levels {
                warmth: 40,
                brightness: 90
            }
        );
        assert_eq!(
            scheduled_levels(&value, 7.0 * 60.0 + 15.0).unwrap(),
            Levels {
                warmth: 40,
                brightness: 90
            }
        );
    }

    #[test]
    fn location_timing_resolves_civil_twilight() {
        let mut value = schedule();
        value.timing = ScheduleTiming::Location;
        // Toronto
        value.latitude = 43.6532;
        value.longitude = -79.3832;
        let now = Local
            .with_ymd_and_hms(2023, 6, 21, 12, 0, 0)
            .single()
            .unwrap();
        let times = resolve_times(&value, now).unwrap();
        assert_ne!(times.day_start, times.night_start);
        // Resolved times should differ from the unused fixed defaults on midsummer.
        assert!(
            times.day_start != 7 * 60 || times.night_start != 21 * 60,
            "expected civil twilight, got day={} night={}",
            times.day_start,
            times.night_start
        );
    }

    #[test]
    fn location_falls_back_when_sun_events_missing() {
        let mut value = schedule();
        value.timing = ScheduleTiming::Location;
        // Near north pole in midwinter — often no civil dawn/dusk.
        value.latitude = 89.0;
        value.longitude = 0.0;
        value.day_start = "08:00".into();
        value.night_start = "16:00".into();
        let now = Local
            .with_ymd_and_hms(2023, 12, 21, 12, 0, 0)
            .single()
            .unwrap();
        let times = resolve_times(&value, now).unwrap();
        assert_eq!(times.day_start, 8 * 60);
        assert_eq!(times.night_start, 16 * 60);
    }

    #[test]
    fn fixed_timing_ignores_coordinates() {
        let mut value = schedule();
        value.timing = ScheduleTiming::Fixed;
        value.latitude = 43.0;
        value.longitude = -79.0;
        let now = Local::now();
        let times = resolve_times(&value, now).unwrap();
        assert_eq!(times.day_start, 7 * 60);
        assert_eq!(times.night_start, 21 * 60);
    }
}
