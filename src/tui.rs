use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
use chrono::{Local, Timelike};
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, HighlightSpacing, LineGauge, List, ListItem, ListState,
        Padding, Paragraph, Wrap,
    },
};

use crate::{
    config::{
        Levels, MAX_TRANSITION_MINUTES, MIN_BRIGHTNESS, Schedule, ScheduleTiming, Settings,
        parse_time,
    },
    daemon::TransientBackend,
    gamma::warmth_to_kelvin,
    ipc::{query_state, replace_settings},
    protocol::RuntimeState,
    schedule::resolve_times,
    service::retire_legacy_service,
};

// ── palette ──────────────────────────────────────────────────────────────────

const TEXT: Color = Color::White;
const MUTED: Color = Color::DarkGray;
const BORDER: Color = Color::DarkGray;
const ACCENT: Color = Color::Yellow;
const WARM: Color = Color::LightYellow;
const COOL: Color = Color::Cyan;
/// Highlight for the current-time column on the schedule bar (same glyph, distinct style).
const NOW: Color = Color::White;

// ── fields ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
#[repr(usize)]
enum Field {
    Mode,
    Filter,
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
    const ALL: [Self; 14] = [
        Self::Mode,
        Self::Filter,
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

    fn index(self) -> usize {
        self as usize
    }

    fn previous(self) -> Self {
        let index = self.index().checked_sub(1).unwrap_or(Self::ALL.len() - 1);
        Self::ALL[index]
    }

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    /// Map a field onto its row in the settings list (including group headers / spacers).
    ///
    /// Layout without leading blank:
    /// GENERAL, Mode, Filter, [blank], MANUAL…, Warmth, Brightness, [blank], SCHEDULE…
    fn row_index(self, spacious: bool) -> usize {
        match (self, spacious) {
            (Self::Mode | Self::Filter, _) => self.index() + 1,
            (Self::ManualWarmth | Self::ManualBrightness, false) => self.index() + 2,
            (Self::ManualWarmth | Self::ManualBrightness, true) => self.index() + 3,
            (_, false) => self.index() + 3,
            (_, true) => self.index() + 5,
        }
    }

    fn is_toggle(self) -> bool {
        matches!(self, Self::Mode | Self::Filter | Self::Timing)
    }
}

// ── timeline model ───────────────────────────────────────────────────────────

const MINUTES_PER_DAY: u16 = 24 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimelinePhase {
    Day,
    EveningFade,
    Night,
    MorningFade,
}

impl TimelinePhase {
    fn glyph(self) -> &'static str {
        match self {
            Self::Day => "█",
            Self::Night => "█",
            Self::EveningFade | Self::MorningFade => "▒",
        }
    }

    fn color(self, active: bool) -> Color {
        if !active {
            return MUTED;
        }
        match self {
            Self::Day => COOL,
            Self::Night => WARM,
            Self::EveningFade | Self::MorningFade => ACCENT,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TimelineSegment {
    phase: TimelinePhase,
    /// Inclusive start minute in [0, 1440).
    start: u16,
    /// Exclusive end minute in (0, 1440]; 1440 means end-of-day.
    end: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TimelineView {
    segments: Vec<TimelineSegment>,
    day_start: u16,
    night_start: u16,
    transition_minutes: u16,
    now_minute: u16,
}

impl TimelineView {
    fn from_schedule(schedule: &Schedule, now: chrono::DateTime<Local>) -> Result<Self> {
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

    fn phase_at(&self, minute: u16) -> TimelinePhase {
        let minute = minute % MINUTES_PER_DAY;
        for segment in &self.segments {
            if minute >= segment.start && minute < segment.end {
                return segment.phase;
            }
        }
        // Fallback: last segment wrapping (should not hit if segments cover full day).
        self.segments
            .last()
            .map(|segment| segment.phase)
            .unwrap_or(TimelinePhase::Day)
    }
}

/// Build non-overlapping segments covering [0, 1440) that match schedule semantics.
fn build_timeline_segments(
    day_start: u16,
    night_start: u16,
    transition_minutes: u16,
) -> Vec<TimelineSegment> {
    // Event table: at each boundary the phase that begins.
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

    // Determine phase active at minute 0 from the last event of the previous day.
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

fn format_hhmm(minute: u16) -> String {
    let minute = minute % MINUTES_PER_DAY;
    format!("{:02}:{:02}", minute / 60, minute % 60)
}

// ── entry ────────────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    let (state, transient_backend) = connect_or_start()?;
    let mut app = App::new(state, transient_backend.is_some());
    let _transient_backend = transient_backend;
    ratatui::run(|terminal| app.run(terminal))
}

fn connect_or_start() -> Result<(RuntimeState, Option<TransientBackend>)> {
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

// ── app state ────────────────────────────────────────────────────────────────

struct App {
    state: RuntimeState,
    selected: Field,
    notice: Option<Notice>,
    transient: bool,
    backend_available: bool,
    last_refresh: Instant,
}

struct ScreenAreas {
    header: Rect,
    metrics: Rect,
    timeline: Rect,
    settings: Rect,
    help: Option<Rect>,
    notice: Rect,
    footer: Rect,
    compact: bool,
}

struct Notice {
    text: String,
    error: bool,
    expires_at: Option<Instant>,
}

impl Notice {
    fn saved() -> Self {
        Self {
            text: "Settings saved".into(),
            error: false,
            expires_at: Some(Instant::now() + Duration::from_secs(2)),
        }
    }

    fn reconnected() -> Self {
        Self {
            text: "Display backend reconnected".into(),
            error: false,
            expires_at: Some(Instant::now() + Duration::from_secs(2)),
        }
    }

    fn error(text: String) -> Self {
        Self {
            text,
            error: true,
            expires_at: None,
        }
    }
}

impl App {
    fn new(state: RuntimeState, transient: bool) -> Self {
        Self {
            state,
            selected: Field::Mode,
            notice: None,
            transient,
            backend_available: true,
            last_refresh: Instant::now(),
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            self.expire_notice();
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(200))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && self.handle_key(key)?
            {
                return Ok(());
            }
            if self.last_refresh.elapsed() >= Duration::from_secs(1) {
                match query_state() {
                    Ok(state) => {
                        self.state = state;
                        if !self.backend_available {
                            self.notice = Some(Notice::reconnected());
                        }
                        self.backend_available = true;
                    }
                    Err(error) => {
                        self.backend_available = false;
                        self.notice = Some(Notice::error(format!(
                            "Display backend unavailable: {error:#}"
                        )));
                    }
                }
                self.last_refresh = Instant::now();
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.previous();
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = self.selected.next();
            }
            KeyCode::BackTab => {
                self.selected = self.selected.previous();
            }
            KeyCode::Left | KeyCode::Char('h') => self.adjust(-1, key.modifiers)?,
            KeyCode::Right | KeyCode::Char('l') => self.adjust(1, key.modifiers)?,
            KeyCode::Enter | KeyCode::Char(' ') => self.toggle()?,
            _ => {}
        }
        Ok(false)
    }

    fn toggle(&mut self) -> Result<()> {
        match self.selected {
            Field::Filter => self.edit(toggle_filter),
            Field::Mode => self.edit(|settings| settings.automatic = !settings.automatic),
            Field::Timing => self.edit(|settings| {
                settings.schedule.timing = match settings.schedule.timing {
                    ScheduleTiming::Fixed => ScheduleTiming::Location,
                    ScheduleTiming::Location => ScheduleTiming::Fixed,
                };
            }),
            _ => Ok(()),
        }
    }

    fn adjust(&mut self, direction: i16, modifiers: KeyModifiers) -> Result<()> {
        let fine = modifiers.contains(KeyModifiers::SHIFT);
        let percent_step = if fine { 1 } else { 5 };
        let time_step = if fine { 1 } else { 15 };
        let coord_step = if fine { 0.1 } else { 1.0 };
        let selected = self.selected;
        self.edit(move |settings| match selected {
            Field::Filter => set_filter(settings, direction > 0),
            Field::Mode => settings.automatic = direction > 0,
            Field::Timing => {
                settings.schedule.timing = if direction > 0 {
                    ScheduleTiming::Location
                } else {
                    ScheduleTiming::Fixed
                };
            }
            Field::ManualWarmth => {
                let manual = manual_levels(settings);
                adjust_percent(&mut manual.warmth, direction, percent_step, 0);
            }
            Field::ManualBrightness => {
                let manual = manual_levels(settings);
                adjust_percent(
                    &mut manual.brightness,
                    direction,
                    percent_step,
                    MIN_BRIGHTNESS,
                );
            }
            Field::DayWarmth => {
                adjust_percent(
                    &mut settings.schedule.day.warmth,
                    direction,
                    percent_step,
                    0,
                );
            }
            Field::DayBrightness => {
                adjust_percent(
                    &mut settings.schedule.day.brightness,
                    direction,
                    percent_step,
                    MIN_BRIGHTNESS,
                );
            }
            Field::NightWarmth => {
                adjust_percent(
                    &mut settings.schedule.night.warmth,
                    direction,
                    percent_step,
                    0,
                );
            }
            Field::NightBrightness => {
                adjust_percent(
                    &mut settings.schedule.night.brightness,
                    direction,
                    percent_step,
                    MIN_BRIGHTNESS,
                );
            }
            Field::NightStart => {
                adjust_time(&mut settings.schedule.night_start, direction, time_step)
            }
            Field::DayStart => adjust_time(&mut settings.schedule.day_start, direction, time_step),
            Field::Latitude => adjust_coordinate(
                &mut settings.schedule.latitude,
                direction,
                coord_step,
                -90.0,
                90.0,
            ),
            Field::Longitude => adjust_coordinate(
                &mut settings.schedule.longitude,
                direction,
                coord_step,
                -180.0,
                180.0,
            ),
            Field::Transition => {
                let value = settings.schedule.transition_minutes as i32
                    + direction as i32 * time_step as i32;
                settings.schedule.transition_minutes =
                    value.clamp(0, i32::from(MAX_TRANSITION_MINUTES)) as u16;
            }
        })
    }

    fn edit(&mut self, change: impl FnOnce(&mut Settings)) -> Result<()> {
        let mut settings = self.state.settings.clone();
        change(&mut settings);
        if settings == self.state.settings {
            return Ok(());
        }
        match replace_settings(settings) {
            Ok(state) => {
                self.state = state;
                self.backend_available = true;
                self.notice = Some(Notice::saved());
            }
            Err(error) => {
                self.notice = Some(Notice::error(format!("Settings not saved: {error:#}")))
            }
        }
        self.last_refresh = Instant::now();
        Ok(())
    }

    fn expire_notice(&mut self) {
        if self
            .notice
            .as_ref()
            .and_then(|notice| notice.expires_at)
            .is_some_and(|expires_at| Instant::now() >= expires_at)
        {
            self.notice = None;
        }
    }

    fn notice_text(&self) -> (&str, bool) {
        self.notice.as_ref().map_or_else(
            || {
                if self.transient {
                    ("Temporary session — changes last until exit", false)
                } else {
                    ("Connected service — changes save immediately", false)
                }
            },
            |notice| (notice.text.as_str(), notice.error),
        )
    }

    fn render(&self, frame: &mut Frame) {
        frame.render_widget(
            Block::default().style(Style::default().fg(TEXT)),
            frame.area(),
        );
        if frame.area().width < 80 || frame.area().height < 24 {
            frame.render_widget(
                Paragraph::new("Waywarm needs a terminal of at least 80 × 24")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(ACCENT)),
                frame.area(),
            );
            return;
        }

        let horizontal_margin = if frame.area().width >= 100 { 2 } else { 1 };
        let content = frame.area().inner(Margin {
            horizontal: horizontal_margin,
            vertical: 0,
        });
        let areas = screen_areas(content);

        self.render_header(frame, areas.header);
        self.render_metrics(frame, areas.metrics, areas.compact);
        self.render_timeline(frame, areas.timeline, areas.compact);
        render_settings(frame, areas.settings, &self.state.settings, self.selected);

        if let Some(help_area) = areas.help {
            let (selected_name, selected_help) = selected_help(self.selected);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " i ",
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("{selected_name}. "),
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(selected_help, Style::default().fg(MUTED)),
                ]))
                .wrap(Wrap { trim: true })
                .block(panel_block(" HELP ").padding(Padding::horizontal(1))),
                help_area,
            );
        }

        let (notice, error) = self.notice_text();
        render_notice(frame, areas.notice, notice, error);
        render_footer(frame, areas.footer, self.selected);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let output_text = if self.state.outputs.is_empty() {
            "No displays detected".into()
        } else {
            self.state.outputs.join("  ·  ")
        };
        let daemon_status = backend_status(self.transient, self.backend_available);
        let status_color = match daemon_status {
            "OFFLINE" => ACCENT,
            "TEMPORARY" => WARM,
            _ => COOL,
        };
        let daemon_value = format!(" {daemon_status} ");

        let header_block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(BORDER));
        let header_inner = header_block.inner(area);
        frame.render_widget(header_block, area);

        // Compact header is a single status row; spacious keeps brand + meta rows.
        if header_inner.height <= 1 {
            let columns = Layout::horizontal([Constraint::Min(1), Constraint::Length(12)])
                .split(header_inner);
            // " WAYWARM " + status chip + spaces ≈ fixed prefix before outputs.
            let prefix_chars = 9 + daemon_value.chars().count() + 1;
            let output_width = columns[0].width.saturating_sub(prefix_chars as u16) as usize;
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " WAYWARM ",
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        daemon_value,
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD)
                            .add_modifier(Modifier::REVERSED),
                    ),
                    Span::styled(" ", Style::default().fg(MUTED)),
                    Span::styled(
                        truncate_with_ellipsis(&output_text, output_width),
                        Style::default().fg(TEXT),
                    ),
                ])),
                columns[0],
            );
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        "[ q ]",
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" Quit", Style::default().fg(MUTED)),
                ]))
                .alignment(Alignment::Right),
                columns[1],
            );
            return;
        }

        let header_rows =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(header_inner);
        let header_columns =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(12)]).split(header_rows[0]);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " WAYWARM ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled("·", Style::default().fg(MUTED)),
                Span::styled(" dashboard ", Style::default().fg(MUTED)),
            ])),
            header_columns[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    "[ q ]",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" Quit", Style::default().fg(MUTED)),
            ]))
            .alignment(Alignment::Right),
            header_columns[1],
        );

        let prefix_width = 8 + daemon_value.chars().count() + 10;
        let output_width = header_rows[1].width.saturating_sub(prefix_width as u16) as usize;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" DAEMON ", Style::default().fg(MUTED)),
                Span::styled(
                    daemon_value,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::REVERSED),
                ),
                Span::styled("  OUTPUTS ", Style::default().fg(MUTED)),
                Span::styled(
                    truncate_with_ellipsis(&output_text, output_width),
                    Style::default().fg(TEXT),
                ),
            ])),
            header_rows[1],
        );
    }

    fn render_metrics(&self, frame: &mut Frame, area: Rect, compact: bool) {
        let gauges = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(2)
            .split(area);
        render_metric(
            frame,
            gauges[0],
            "Warmth",
            format!(
                "{}%  ·  {} K",
                self.state.active_warmth,
                warmth_to_kelvin(self.state.active_warmth)
            ),
            self.state.active_warmth,
            WARM,
            compact,
        );
        render_metric(
            frame,
            gauges[1],
            "Brightness",
            format!("{}%", self.state.active_brightness),
            self.state.active_brightness,
            ACCENT,
            compact,
        );
    }

    fn render_timeline(&self, frame: &mut Frame, area: Rect, compact: bool) {
        let schedule_active = self.state.settings.enabled && self.state.settings.automatic;
        let now = Local::now();
        let timeline = TimelineView::from_schedule(&self.state.settings.schedule, now).ok();

        let title = match (&timeline, schedule_active, compact) {
            (Some(view), true, true) => format!(
                " TODAY · day {} · night {} ",
                format_hhmm(view.day_start),
                format_hhmm(view.night_start)
            ),
            (Some(_), false, _) => " TODAY · inactive ".to_owned(),
            _ => " TODAY ".to_owned(),
        };
        let block = panel_block_owned(title);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let Some(timeline) = timeline else {
            frame.render_widget(
                Paragraph::new("Schedule times are invalid")
                    .style(Style::default().fg(ACCENT))
                    .alignment(Alignment::Center),
                inner,
            );
            return;
        };

        // Height-3 panel → 1 inner row: bar only (times live in the title on compact).
        if inner.height <= 1 || compact {
            let bar = render_timeline_bar(inner.width as usize, &timeline, schedule_active);
            if inner.height <= 1 {
                frame.render_widget(Paragraph::new(bar), inner);
            } else {
                let rows =
                    Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
                let legend = format!(
                    " day {} · night {} · fade {}m · now {}",
                    format_hhmm(timeline.day_start),
                    format_hhmm(timeline.night_start),
                    timeline.transition_minutes,
                    format_hhmm(timeline.now_minute),
                );
                frame.render_widget(Paragraph::new(bar), rows[0]);
                frame.render_widget(
                    Paragraph::new(legend).style(Style::default().fg(MUTED)),
                    rows[1],
                );
            }
            return;
        }

        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  24h schedule", Style::default().fg(MUTED)),
                Span::styled("  ·  ", Style::default().fg(MUTED)),
                phase_legend(TimelinePhase::Day, schedule_active),
                Span::raw(" "),
                phase_legend(TimelinePhase::EveningFade, schedule_active),
                Span::raw(" "),
                phase_legend(TimelinePhase::Night, schedule_active),
            ])),
            rows[0],
        );

        let bar = render_timeline_bar(rows[1].width as usize, &timeline, schedule_active);
        frame.render_widget(Paragraph::new(bar), rows[1]);

        let legend = format!(
            "  Day begins {}   Night begins {}   Fade {} min   Now {}",
            format_hhmm(timeline.day_start),
            format_hhmm(timeline.night_start),
            timeline.transition_minutes,
            format_hhmm(timeline.now_minute),
        );
        frame.render_widget(
            Paragraph::new(legend).style(Style::default().fg(MUTED)),
            rows[2],
        );
    }
}

fn phase_legend(phase: TimelinePhase, active: bool) -> Span<'static> {
    let label = match phase {
        TimelinePhase::Day => " day ",
        TimelinePhase::EveningFade | TimelinePhase::MorningFade => " fade ",
        TimelinePhase::Night => " night ",
    };
    Span::styled(
        label,
        Style::default()
            .fg(if active { TEXT } else { MUTED })
            .bg(phase.color(active)),
    )
}

fn render_timeline_bar(width: usize, timeline: &TimelineView, active: bool) -> Line<'static> {
    if width == 0 {
        return Line::default();
    }

    let now_col = ((u32::from(timeline.now_minute) * width as u32) / u32::from(MINUTES_PER_DAY))
        .min(width.saturating_sub(1) as u32) as usize;

    let mut spans = Vec::with_capacity(width);
    let mut col = 0;
    while col < width {
        let minute = ((col as u32 * u32::from(MINUTES_PER_DAY)) / width as u32) as u16;
        let phase = timeline.phase_at(minute);

        // Collapse consecutive same-styled columns into one span for efficiency.
        let mut end = col + 1;
        while end < width {
            let end_minute = ((end as u32 * u32::from(MINUTES_PER_DAY)) / width as u32) as u16;
            if timeline.phase_at(end_minute) != phase || end == now_col || col == now_col {
                break;
            }
            end += 1;
        }

        if col == now_col {
            // Same phase glyph as neighbors; reverse video marks "now" without a special icon.
            spans.push(Span::styled(
                phase.glyph(),
                Style::default()
                    .fg(NOW)
                    .bg(phase.color(active))
                    .add_modifier(Modifier::BOLD),
            ));
            col += 1;
            continue;
        }

        let count = end - col;
        let fill = phase.glyph().repeat(count);
        spans.push(Span::styled(fill, Style::default().fg(phase.color(active))));
        col = end;
    }
    Line::from(spans)
}

fn backend_status(transient: bool, available: bool) -> &'static str {
    if !available {
        "OFFLINE"
    } else if transient {
        "TEMPORARY"
    } else {
        "ONLINE"
    }
}

fn screen_areas(area: Rect) -> ScreenAreas {
    let compact = area.height < 34;
    if compact {
        // 80×24: single-line header, dense metrics/timeline, control list (may scroll).
        // 2 + 3 + 3 + 14 + 1 + 1 = 24
        let areas = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(14),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        ScreenAreas {
            header: areas[0],
            metrics: areas[1],
            timeline: areas[2],
            settings: areas[3],
            help: None,
            notice: areas[4],
            footer: areas[5],
            compact,
        }
    } else {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(5),
            Constraint::Length(1),
            Constraint::Min(12),
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        ScreenAreas {
            header: areas[0],
            metrics: areas[2],
            timeline: areas[4],
            settings: areas[6],
            help: Some(areas[8]),
            notice: areas[9],
            footer: areas[10],
            compact,
        }
    }
}

fn panel_block(title: &'static str) -> Block<'static> {
    panel_block_owned(title.to_owned())
}

fn panel_block_owned(title: String) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().fg(TEXT))
}

fn render_metric(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    value: String,
    percent: u8,
    color: Color,
    compact: bool,
) {
    let metric_block = Block::default()
        .title(Span::styled(
            format!(" {name} ").to_uppercase(),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().fg(TEXT));
    let metric_inner = metric_block.inner(area);
    frame.render_widget(metric_block, area);
    if compact {
        // Height-3 panel → 1 inner row: value overlaid as gauge label.
        // Height-4+ → value line + gauge.
        if metric_inner.height <= 1 {
            frame.render_widget(
                LineGauge::default()
                    .ratio(percent as f64 / 100.0)
                    .label(value)
                    .filled_symbol("█")
                    .unfilled_symbol("░")
                    .filled_style(Style::default().fg(color))
                    .unfilled_style(Style::default().fg(BORDER)),
                metric_inner,
            );
            return;
        }
        let rows =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(metric_inner);
        frame.render_widget(
            Paragraph::new(value)
                .alignment(Alignment::Center)
                .style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
            rows[0],
        );
        frame.render_widget(
            LineGauge::default()
                .ratio(percent as f64 / 100.0)
                .label("")
                .filled_symbol("█")
                .unfilled_symbol("░")
                .filled_style(Style::default().fg(color))
                .unfilled_style(Style::default().fg(BORDER)),
            rows[1],
        );
        return;
    }
    let content = metric_inner.inner(Margin {
        horizontal: 2,
        vertical: 0,
    });
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(content);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("CURRENT  ", Style::default().fg(MUTED)),
            Span::styled(
                value,
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
        ])),
        rows[0],
    );
    frame.render_widget(
        LineGauge::default()
            .ratio(percent as f64 / 100.0)
            .label("")
            .filled_symbol("█")
            .unfilled_symbol("░")
            .filled_style(Style::default().fg(color))
            .unfilled_style(Style::default().fg(BORDER)),
        rows[2],
    );
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("0%", Style::default().fg(MUTED)),
            Span::styled(
                format!(
                    "{:>width$}",
                    "100%",
                    width = content.width.saturating_sub(2) as usize
                ),
                Style::default().fg(MUTED),
            ),
        ])),
        rows[3],
    );
}

fn render_settings(frame: &mut Frame, area: Rect, settings: &Settings, selected: Field) {
    let block = panel_block(" CONTROLS ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let manual_active = settings.enabled && !settings.automatic;
    let schedule_active = settings.enabled && settings.automatic;
    let location_active = schedule_active && settings.schedule.timing == ScheduleTiming::Location;
    let fixed_times_active = schedule_active && settings.schedule.timing == ScheduleTiming::Fixed;
    let mode = if settings.automatic {
        "AUTOMATIC"
    } else {
        "MANUAL"
    };
    let timing = match settings.schedule.timing {
        ScheduleTiming::Fixed => "FIXED",
        ScheduleTiming::Location => "LOCATION",
    };
    // Compact lists need every row: 3 group headers + 14 fields = 17 lines.
    let spacious = inner.height >= 19;
    let mut items = vec![
        group_item("GENERAL", true),
        value_item("Mode", mode, true, true, false),
        value_item(
            "Filter",
            if settings.enabled { "ON" } else { "OFF" },
            true,
            settings.enabled,
            false,
        ),
    ];
    if spacious {
        items.push(ListItem::new(""));
    }
    items.extend([
        group_item("MANUAL OVERRIDE", manual_active),
        value_item(
            "Warmth",
            format!(
                "{}%  ·  {} K",
                settings.manual.warmth,
                warmth_to_kelvin(settings.manual.warmth)
            ),
            manual_active,
            false,
            true,
        ),
        value_item(
            "Brightness",
            format!("{}%", settings.manual.brightness),
            manual_active,
            false,
            true,
        ),
    ]);
    if spacious {
        items.push(ListItem::new(""));
    }
    items.extend([
        group_item("SCHEDULE", schedule_active),
        value_item("Timing", timing, schedule_active, true, false),
        value_item(
            "Day warmth",
            format!(
                "{}%  ·  {} K",
                settings.schedule.day.warmth,
                warmth_to_kelvin(settings.schedule.day.warmth)
            ),
            schedule_active,
            false,
            true,
        ),
        value_item(
            "Day brightness",
            format!("{}%", settings.schedule.day.brightness),
            schedule_active,
            false,
            true,
        ),
        value_item(
            "Night warmth",
            format!(
                "{}%  ·  {} K",
                settings.schedule.night.warmth,
                warmth_to_kelvin(settings.schedule.night.warmth)
            ),
            schedule_active,
            false,
            true,
        ),
        value_item(
            "Night brightness",
            format!("{}%", settings.schedule.night.brightness),
            schedule_active,
            false,
            true,
        ),
        value_item(
            "Night begins",
            settings.schedule.night_start.clone(),
            fixed_times_active || schedule_active,
            false,
            fixed_times_active,
        ),
        value_item(
            "Day begins",
            settings.schedule.day_start.clone(),
            fixed_times_active || schedule_active,
            false,
            fixed_times_active,
        ),
        value_item(
            "Latitude",
            format!("{:.2}°", settings.schedule.latitude),
            location_active,
            false,
            location_active,
        ),
        value_item(
            "Longitude",
            format!("{:.2}°", settings.schedule.longitude),
            location_active,
            false,
            location_active,
        ),
        value_item(
            "Fade duration",
            format!("{} min", settings.schedule.transition_minutes),
            schedule_active,
            false,
            true,
        ),
    ]);
    let mut state = ListState::default().with_selected(Some(selected.row_index(spacious)));
    frame.render_stateful_widget(
        List::new(items)
            .highlight_symbol("→ ")
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
        inner,
        &mut state,
    );
}

fn group_item(label: &'static str, active: bool) -> ListItem<'static> {
    ListItem::new(Span::styled(
        label,
        Style::default()
            .fg(if active { ACCENT } else { MUTED })
            .add_modifier(Modifier::BOLD),
    ))
}

fn value_item(
    label: &str,
    value: impl Into<String>,
    active: bool,
    accent: bool,
    adjustable: bool,
) -> ListItem<'static> {
    let value = value.into();
    let value = if adjustable {
        format!("‹ {value} ›")
    } else {
        format!("[ {value} ]")
    };
    let base_color = if active { TEXT } else { MUTED };
    let value_color = if !active {
        MUTED
    } else if accent {
        ACCENT
    } else {
        TEXT
    };
    ListItem::new(Line::from(vec![
        Span::styled(format!("{label:<22}"), Style::default().fg(base_color)),
        Span::styled(
            value,
            Style::default()
                .fg(value_color)
                .add_modifier(Modifier::BOLD),
        ),
    ]))
}

fn render_notice(frame: &mut Frame, area: Rect, message: &str, error: bool) {
    let prefix = if error { " ! " } else { " · " };
    let available = area.width.saturating_sub(prefix.len() as u16) as usize;
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                prefix,
                Style::default()
                    .fg(if error { ACCENT } else { MUTED })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_with_ellipsis(message, available),
                Style::default().fg(if error { ACCENT } else { TEXT }),
            ),
        ])),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, selected: Field) {
    let controls = if selected.is_toggle() {
        vec![
            key_chip("↑/↓"),
            muted(" Navigate  "),
            key_chip("←/→"),
            muted(" Set  "),
            key_chip("Enter"),
            muted(" Toggle  "),
            key_chip("q"),
            muted(" Quit"),
        ]
    } else {
        vec![
            key_chip("↑/↓"),
            muted(" Navigate  "),
            key_chip("←/→"),
            muted(" Adjust  "),
            key_chip("Shift+←/→"),
            muted(" Fine  "),
            key_chip("q"),
            muted(" Quit"),
        ]
    };
    frame.render_widget(
        Paragraph::new(Line::from(controls)).alignment(Alignment::Center),
        area,
    );
}

fn truncate_with_ellipsis(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length <= width {
        return value.into();
    }
    if width <= 1 {
        return "…".chars().take(width).collect();
    }
    value.chars().take(width - 1).chain(['…']).collect()
}

fn key_chip(label: &'static str) -> Span<'static> {
    Span::styled(
        format!(" {label} "),
        Style::default()
            .fg(ACCENT)
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    )
}

fn muted(label: &'static str) -> Span<'static> {
    Span::styled(label, Style::default().fg(MUTED))
}

fn selected_help(selected: Field) -> (&'static str, &'static str) {
    match selected {
        Field::Filter => (
            "Filter",
            "Enable or disable color filtering. Turning it off restores neutral display colors.",
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

fn manual_levels(settings: &mut Settings) -> &mut Levels {
    settings.enabled = true;
    settings.automatic = false;
    &mut settings.manual
}

fn toggle_filter(settings: &mut Settings) {
    settings.enabled = !settings.enabled;
}

fn set_filter(settings: &mut Settings, enabled: bool) {
    settings.enabled = enabled;
}

fn adjust_percent(value: &mut u8, direction: i16, step: i16, minimum: u8) {
    *value = (*value as i16 + direction * step).clamp(minimum as i16, 100) as u8;
}

fn adjust_time(value: &mut String, direction: i16, step: i16) {
    if let Ok(minutes) = parse_time(value) {
        let next = (minutes as i16 + direction * step).rem_euclid(1440) as u16;
        *value = format!("{:02}:{:02}", next / 60, next % 60);
    }
}

fn adjust_coordinate(value: &mut f64, direction: i16, step: f64, minimum: f64, maximum: f64) {
    *value = (*value + f64::from(direction) * step).clamp(minimum, maximum);
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use chrono::{Local, TimeZone};
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::schedule::current_levels;

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
    fn values_clamp_and_times_wrap() {
        let mut percentage = 98;
        adjust_percent(&mut percentage, 1, 5, 0);
        assert_eq!(percentage, 100);
        adjust_percent(&mut percentage, -1, 95, 10);
        assert_eq!(percentage, 10);
        let mut time = "23:55".to_owned();
        adjust_time(&mut time, 1, 15);
        assert_eq!(time, "00:10");
    }

    #[test]
    fn manual_adjustments_enable_manual_filtering() {
        let mut settings = Settings {
            enabled: false,
            automatic: true,
            ..Settings::default()
        };
        let manual = manual_levels(&mut settings);
        adjust_percent(&mut manual.warmth, 1, 5, 0);

        assert!(settings.enabled);
        assert!(!settings.automatic);
        assert_eq!(settings.manual.warmth, 55);
        assert_eq!(settings.manual.brightness, 90);
        assert_eq!(
            current_levels(&settings, Local::now()).unwrap(),
            settings.manual
        );
    }

    #[test]
    fn filter_changes_preserve_the_selected_mode() {
        let mut settings = Settings {
            enabled: true,
            automatic: true,
            ..Settings::default()
        };

        toggle_filter(&mut settings);
        assert!(!settings.enabled);
        assert!(settings.automatic);

        set_filter(&mut settings, true);
        assert!(settings.enabled);
        assert!(settings.automatic);
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

        // Midday is day.
        assert_eq!(view.phase_at(12 * 60), TimelinePhase::Day);
        // During evening fade (21:00–21:30).
        assert_eq!(view.phase_at(21 * 60 + 10), TimelinePhase::EveningFade);
        // Late night is night.
        assert_eq!(view.phase_at(23 * 60), TimelinePhase::Night);
        // Early morning still night.
        assert_eq!(view.phase_at(2 * 60), TimelinePhase::Night);
        // Morning fade (07:00–07:30).
        assert_eq!(view.phase_at(7 * 60 + 10), TimelinePhase::MorningFade);
        // After morning fade is day.
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
    fn timeline_bar_includes_now_marker() {
        let schedule = Schedule::default();
        let view = TimelineView::from_schedule(&schedule, noon_local()).unwrap();
        let line = render_timeline_bar(48, &view, true);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text.chars().count(), 48);
        // Now is a same-glyph column with a distinct background, not a special icon.
        let now_styled = line.spans.iter().any(|span| {
            span.content.as_ref() == "█"
                && span.style.bg == Some(COOL)
                && span.style.fg == Some(NOW)
        });
        assert!(now_styled, "expected color-highlighted now column in bar");
        assert!(!text.contains('◆'));
    }

    #[test]
    fn dashboard_renders_full_and_compact_terminal_states() {
        let app = App::new(test_state(Settings::default()), false);

        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("WAYWARM"));
        assert!(rendered.contains("Filter"));
        assert!(rendered.contains("MANUAL OVERRIDE"));
        assert!(rendered.contains("Day warmth"));
        assert!(rendered.contains("Night warmth"));
        assert!(rendered.contains("Timing"));
        assert!(rendered.contains("HELP"));
        assert!(rendered.contains("CONTROLS"));
        assert!(rendered.contains("TODAY"));
        assert!(rendered.contains("→ Mode"));
        assert!(!rendered.contains("› Schedule"));

        let mut compact = Terminal::new(TestBackend::new(80, 24)).unwrap();
        compact.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&compact);
        // Compact view scrolls the longer schedule list; top schedule rows stay visible.
        assert!(rendered.contains("Day warmth"));
        assert!(rendered.contains("Night warmth"));
        assert!(rendered.contains("Connected service"));
        assert!(rendered.contains("TODAY"));
        assert!(!rendered.contains("at least"));
        assert!(!rendered.contains("HELP"));

        let mut small = Terminal::new(TestBackend::new(60, 20)).unwrap();
        small.draw(|frame| app.render(frame)).unwrap();
        assert!(buffer_text(&small).contains("at least 80 × 24"));
    }

    #[test]
    fn settings_keep_mode_visible_when_filter_is_off() {
        let settings = Settings {
            enabled: false,
            automatic: true,
            ..Settings::default()
        };
        let app = App::new(test_state(settings), false);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("AUTOMATIC"));
        assert!(rendered.contains("OFF"));
    }

    #[test]
    fn footer_matches_the_selected_control_type() {
        let mut app = App::new(test_state(Settings::default()), false);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let toggle_footer = buffer_text(&terminal);
        assert!(toggle_footer.contains("Toggle"));
        assert!(!toggle_footer.contains("Fine"));

        app.selected = Field::ManualWarmth;
        terminal.draw(|frame| app.render(frame)).unwrap();
        let adjust_footer = buffer_text(&terminal);
        assert!(adjust_footer.contains("Fine"));
        assert!(!adjust_footer.contains("Toggle"));
    }

    #[test]
    fn dashboard_uses_only_the_supported_terminal_colors() {
        let app = App::new(test_state(Settings::default()), false);
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        for cell in terminal.backend().buffer().content() {
            assert!(
                matches!(
                    cell.fg,
                    Color::Reset
                        | Color::White
                        | Color::Yellow
                        | Color::LightYellow
                        | Color::Cyan
                        | Color::Gray
                        | Color::DarkGray
                ),
                "unexpected fg color: {:?}",
                cell.fg
            );
            assert!(
                matches!(
                    cell.bg,
                    Color::Reset
                        | Color::White
                        | Color::Yellow
                        | Color::LightYellow
                        | Color::Cyan
                        | Color::Gray
                        | Color::DarkGray
                ),
                "unexpected bg color: {:?}",
                cell.bg
            );
        }
    }

    fn test_state(settings: Settings) -> RuntimeState {
        RuntimeState {
            settings,
            outputs: vec!["DP-1".into()],
            backend: "test".into(),
            active_warmth: 50,
            active_brightness: 90,
        }
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}
