//! Shared helpers for the settings TUI and GTK dashboard.

use std::{thread, time::Duration};

use anyhow::{Result, bail};
use chrono::{Local, Timelike};

use crate::{
    config::Schedule, daemon::TransientBackend, ipc::query_state, protocol::RuntimeState,
    schedule::resolve_times, service::retire_legacy_service,
};

pub const MINUTES_PER_DAY: u16 = 24 * 60;

/// Navigable settings fields shared by both frontends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Field {
    Mode,
    Filter,
    Preset,
    ManualWarmth,
    ManualBrightness,
    Timing,
    DayWarmth,
    DayBrightness,
    NightWarmth,
    NightBrightness,
    NightStart,
    DayStart,
    Latitude,
    Longitude,
    Transition,
}

impl Field {
    pub const ALL: [Self; 15] = [
        Self::Mode,
        Self::Filter,
        Self::Preset,
        Self::ManualWarmth,
        Self::ManualBrightness,
        Self::Timing,
        Self::DayWarmth,
        Self::DayBrightness,
        Self::NightWarmth,
        Self::NightBrightness,
        Self::NightStart,
        Self::DayStart,
        Self::Latitude,
        Self::Longitude,
        Self::Transition,
    ];

    pub fn index(self) -> usize {
        self as usize
    }

    pub fn previous(self) -> Self {
        let index = self.index().checked_sub(1).unwrap_or(Self::ALL.len() - 1);
        Self::ALL[index]
    }

    pub fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// Map a field onto its row in the settings list (including group headers / spacers).
    pub fn row_index(self, spacious: bool) -> usize {
        match (self, spacious) {
            (Self::Mode | Self::Filter | Self::Preset, _) => self.index() + 1,
            (Self::ManualWarmth | Self::ManualBrightness, false) => self.index() + 2,
            (Self::ManualWarmth | Self::ManualBrightness, true) => self.index() + 3,
            (_, false) => self.index() + 3,
            (_, true) => self.index() + 5,
        }
    }

    pub fn is_toggle(self) -> bool {
        matches!(
            self,
            Self::Mode | Self::Filter | Self::Timing | Self::Preset
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimelinePhase {
    Day,
    EveningFade,
    Night,
    MorningFade,
}

impl TimelinePhase {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Day | Self::Night => "█",
            Self::EveningFade | Self::MorningFade => "▒",
        }
    }

    /// RGB colors for GTK painting (day=cyan, night=warm, fade=accent).
    pub fn rgb(self, active: bool) -> (f64, f64, f64) {
        if !active {
            return (0.45, 0.45, 0.45);
        }
        match self {
            Self::Day => (0.0, 0.75, 0.85),
            Self::Night => (0.95, 0.85, 0.45),
            Self::EveningFade | Self::MorningFade => (0.95, 0.80, 0.15),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimelineSegment {
    pub phase: TimelinePhase,
    /// Inclusive start minute in [0, 1440).
    pub start: u16,
    /// Exclusive end minute in (0, 1440]; 1440 means end-of-day.
    pub end: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineView {
    pub segments: Vec<TimelineSegment>,
    pub day_start: u16,
    pub night_start: u16,
    pub transition_minutes: u16,
    pub now_minute: u16,
}

impl TimelineView {
    pub fn from_schedule(schedule: &Schedule, now: chrono::DateTime<Local>) -> Result<Self> {
        let times = resolve_times(schedule, now)?;
        let now_minute = (now.hour() * 60 + now.minute() + if now.second() >= 30 { 1 } else { 0 })
            as u16
            % MINUTES_PER_DAY;
        let segments =
            build_timeline_segments(times.day_start, times.night_start, times.transition_minutes);
        Ok(Self {
            segments,
            day_start: times.day_start,
            night_start: times.night_start,
            transition_minutes: times.transition_minutes,
            now_minute,
        })
    }

    pub fn phase_at(&self, minute: u16) -> TimelinePhase {
        let minute = minute % MINUTES_PER_DAY;
        for segment in &self.segments {
            if minute >= segment.start && minute < segment.end {
                return segment.phase;
            }
        }
        self.segments
            .last()
            .map(|segment| segment.phase)
            .unwrap_or(TimelinePhase::Day)
    }
}

/// Build non-overlapping segments covering [0, 1440) that match schedule semantics.
pub fn build_timeline_segments(
    day_start: u16,
    night_start: u16,
    transition_minutes: u16,
) -> Vec<TimelineSegment> {
    let mut events: Vec<(u16, TimelinePhase)> = Vec::with_capacity(4);
    if transition_minutes == 0 {
        events.push((day_start, TimelinePhase::Day));
        events.push((night_start, TimelinePhase::Night));
    } else {
        let morning_end = add_minutes(day_start, transition_minutes);
        let evening_end = add_minutes(night_start, transition_minutes);
        events.push((day_start, TimelinePhase::MorningFade));
        events.push((morning_end, TimelinePhase::Day));
        events.push((night_start, TimelinePhase::EveningFade));
        events.push((evening_end, TimelinePhase::Night));
    }
    events.sort_by_key(|(minute, _)| *minute);
    events.dedup_by_key(|(minute, _)| *minute);

    if events.is_empty() {
        return vec![TimelineSegment {
            phase: TimelinePhase::Day,
            start: 0,
            end: MINUTES_PER_DAY,
        }];
    }

    let phase_at_midnight = events
        .last()
        .map(|(_, phase)| *phase)
        .unwrap_or(TimelinePhase::Day);

    let mut segments = Vec::new();
    let mut cursor = 0u16;
    let mut phase = phase_at_midnight;

    for (minute, next_phase) in &events {
        if *minute > cursor {
            segments.push(TimelineSegment {
                phase,
                start: cursor,
                end: *minute,
            });
        }
        cursor = *minute;
        phase = *next_phase;
    }
    if cursor < MINUTES_PER_DAY {
        segments.push(TimelineSegment {
            phase,
            start: cursor,
            end: MINUTES_PER_DAY,
        });
    }
    segments
}

fn add_minutes(start: u16, duration: u16) -> u16 {
    (start + duration) % MINUTES_PER_DAY
}

pub fn format_hhmm(minute: u16) -> String {
    let minute = minute % MINUTES_PER_DAY;
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

pub fn connect_or_start() -> Result<(RuntimeState, Option<TransientBackend>)> {
    retire_legacy_service()?;
    if let Ok(state) = query_state() {
        return Ok((state, None));
    }

    let mut backend = TransientBackend::start();

    for _ in 0..60 {
        thread::sleep(Duration::from_millis(50));
        if let Ok(state) = query_state() {
            backend.check_running()?;
            return Ok((state, Some(backend)));
        }
        backend.check_running()?;
    }
    bail!("temporary Waywarm backend did not become ready within 3 seconds")
}

pub fn backend_status(transient: bool, available: bool) -> &'static str {
    if !available {
        "OFFLINE"
    } else if transient {
        "TEMPORARY"
    } else {
        "ONLINE"
    }
}

pub fn field_help(selected: Field) -> (&'static str, &'static str) {
    match selected {
        Field::Filter => (
            "Filter",
            "Enable or disable color filtering. Turning it off restores neutral display colors.",
        ),
        Field::Preset => (
            "Preset",
            "Choose a preset, then apply, save, or delete. Names: letters, digits, - and _.",
        ),
        Field::Mode => (
            "Mode",
            "Automatic follows the schedule. Manual mode holds your chosen warmth and brightness.",
        ),
        Field::ManualWarmth => (
            "Manual warmth",
            "Adjust the immediate color temperature. Changing this switches from Automatic to Manual mode.",
        ),
        Field::ManualBrightness => (
            "Manual brightness",
            "Adjust immediate display brightness. Changing this switches from Automatic to Manual mode.",
        ),
        Field::Timing => (
            "Timing",
            "Fixed uses clock times. Location derives day and night starts from civil dawn and dusk.",
        ),
        Field::DayWarmth => (
            "Day warmth",
            "Choose the warmth held during the day. Zero keeps a neutral white point.",
        ),
        Field::DayBrightness => (
            "Day brightness",
            "Choose the brightness held during the day while automatic mode is active.",
        ),
        Field::NightWarmth => (
            "Night warmth",
            "Choose the warmth reached after the evening fade. Higher percentages reduce more blue light.",
        ),
        Field::NightBrightness => (
            "Night brightness",
            "Choose the brightness reached after the evening fade.",
        ),
        Field::NightStart => (
            "Night begins",
            "Fixed timing start, or fallback when location twilight is unavailable.",
        ),
        Field::DayStart => (
            "Day begins",
            "Fixed timing start, or fallback when location twilight is unavailable.",
        ),
        Field::Latitude => (
            "Latitude",
            "Observer latitude in degrees for civil dawn and dusk (location timing).",
        ),
        Field::Longitude => (
            "Longitude",
            "Observer longitude in degrees for civil dawn and dusk (location timing).",
        ),
        Field::Transition => (
            "Fade duration",
            "Set how gradually Waywarm moves between day and night settings.",
        ),
    }
}

pub fn is_preset_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone};

    use super::*;
    use crate::config::Schedule;

    fn noon_local() -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(2024, 6, 15, 12, 0, 0)
            .single()
            .unwrap()
    }

    fn midnight_local() -> chrono::DateTime<Local> {
        Local
            .with_ymd_and_hms(2024, 6, 15, 0, 0, 0)
            .single()
            .unwrap()
    }

    #[test]
    fn backend_status_distinguishes_service_temporary_and_unavailable() {
        assert_eq!(backend_status(false, true), "ONLINE");
        assert_eq!(backend_status(true, true), "TEMPORARY");
        assert_eq!(backend_status(false, false), "OFFLINE");
        assert_eq!(backend_status(true, false), "OFFLINE");
    }

    #[test]
    fn timeline_segments_match_default_schedule() {
        let schedule = Schedule::default();
        let view = TimelineView::from_schedule(&schedule, noon_local()).unwrap();

        assert_eq!(view.phase_at(12 * 60), TimelinePhase::Day);
        assert_eq!(view.phase_at(21 * 60 + 10), TimelinePhase::EveningFade);
        assert_eq!(view.phase_at(23 * 60), TimelinePhase::Night);
        assert_eq!(view.phase_at(2 * 60), TimelinePhase::Night);
        assert_eq!(view.phase_at(7 * 60 + 10), TimelinePhase::MorningFade);
        assert_eq!(view.phase_at(8 * 60), TimelinePhase::Day);
    }

    #[test]
    fn timeline_supports_instant_transitions() {
        let schedule = Schedule {
            transition_minutes: 0,
            ..Schedule::default()
        };
        let view = TimelineView::from_schedule(&schedule, midnight_local()).unwrap();
        assert_eq!(view.phase_at(21 * 60), TimelinePhase::Night);
        assert_eq!(view.phase_at(7 * 60), TimelinePhase::Day);
        assert_eq!(view.phase_at(6 * 60 + 59), TimelinePhase::Night);
    }

    #[test]
    fn timeline_covers_full_day_without_gaps() {
        let segments = build_timeline_segments(7 * 60, 21 * 60, 30);
        assert!(!segments.is_empty());
        assert_eq!(segments.first().unwrap().start, 0);
        assert_eq!(segments.last().unwrap().end, MINUTES_PER_DAY);
        for window in segments.windows(2) {
            assert_eq!(window[0].end, window[1].start);
        }
        let covered: u16 = segments.iter().map(|s| s.end - s.start).sum();
        assert_eq!(covered, MINUTES_PER_DAY);
    }

    #[test]
    fn preset_name_chars_are_restricted() {
        assert!(is_preset_name_char('a'));
        assert!(is_preset_name_char('Z'));
        assert!(is_preset_name_char('9'));
        assert!(is_preset_name_char('-'));
        assert!(is_preset_name_char('_'));
        assert!(!is_preset_name_char(' '));
        assert!(!is_preset_name_char('/'));
    }
}
