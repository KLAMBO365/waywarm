use anyhow::Result;
use chrono::{DateTime, Local, Timelike};

use crate::config::{Levels, Schedule, Settings, parse_time};

pub fn current_levels(settings: &Settings, now: DateTime<Local>) -> Result<Levels> {
    if !settings.enabled {
        return Ok(Levels::NEUTRAL);
    }
    if !settings.automatic {
        return Ok(settings.manual);
    }
    let minute = now.hour() as f64 * 60.0 + now.minute() as f64 + now.second() as f64 / 60.0;
    scheduled_levels(&settings.schedule, minute)
}

pub fn scheduled_levels(schedule: &Schedule, minute: f64) -> Result<Levels> {
    let day = parse_time(&schedule.day_start)? as f64;
    let night = parse_time(&schedule.night_start)? as f64;
    let duration = schedule.transition_minutes as f64;
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
}
