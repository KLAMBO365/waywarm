use std::{
    thread,
    time::{Duration, Instant},
};

use anyhow::{Result, bail};
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
    config::{Levels, MAX_TRANSITION_MINUTES, MIN_BRIGHTNESS, Settings, parse_time},
    daemon::TransientBackend,
    gamma::warmth_to_kelvin,
    ipc::{query_state, replace_settings},
    protocol::RuntimeState,
    service::retire_legacy_service,
};

#[derive(Clone, Copy)]
#[repr(usize)]
enum Field {
    Mode,
    Filter,
    ManualWarmth,
    ManualBrightness,
    NightWarmth,
    NightBrightness,
    NightStart,
    DayStart,
    Transition,
}

impl Field {
    const ALL: [Self; 9] = [
        Self::Mode,
        Self::Filter,
        Self::ManualWarmth,
        Self::ManualBrightness,
        Self::NightWarmth,
        Self::NightBrightness,
        Self::NightStart,
        Self::DayStart,
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

    fn row_index(self, spacious: bool) -> usize {
        match (self, spacious) {
            (Self::Mode | Self::Filter, _) => self.index() + 2,
            (Self::ManualWarmth | Self::ManualBrightness, false) => self.index() + 3,
            (Self::ManualWarmth | Self::ManualBrightness, true) => self.index() + 4,
            (
                Self::NightWarmth
                | Self::NightBrightness
                | Self::NightStart
                | Self::DayStart
                | Self::Transition,
                false,
            ) => self.index() + 4,
            (
                Self::NightWarmth
                | Self::NightBrightness
                | Self::NightStart
                | Self::DayStart
                | Self::Transition,
                true,
            ) => self.index() + 6,
        }
    }

    fn is_toggle(self) -> bool {
        matches!(self, Self::Mode | Self::Filter)
    }
}

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

const TEXT: Color = Color::White;
const MUTED: Color = Color::DarkGray;
const BORDER: Color = Color::DarkGray;
const YELLOW: Color = Color::Yellow;

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
            _ => Ok(()),
        }
    }

    fn adjust(&mut self, direction: i16, modifiers: KeyModifiers) -> Result<()> {
        let fine = modifiers.contains(KeyModifiers::SHIFT);
        let percent_step = if fine { 1 } else { 5 };
        let time_step = if fine { 1 } else { 15 };
        let selected = self.selected;
        self.edit(move |settings| match selected {
            Field::Filter => set_filter(settings, direction > 0),
            Field::Mode => settings.automatic = direction > 0,
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
                    .style(Style::default().fg(YELLOW)),
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

        let output_text = if self.state.outputs.is_empty() {
            "No displays detected".into()
        } else {
            self.state.outputs.join("  ·  ")
        };
        let daemon_status = backend_status(self.transient, self.backend_available);
        let header_block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(BORDER));
        let header_inner = header_block.inner(areas.header);
        frame.render_widget(header_block, areas.header);
        let header_rows =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(header_inner);
        let header_columns =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(12)]).split(header_rows[0]);
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::styled(
                " WAYWARM ",
                Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
            )])),
            header_columns[0],
        );
        frame.render_widget(
            Paragraph::new("[ q ]  Quit")
                .alignment(Alignment::Right)
                .style(Style::default().fg(MUTED)),
            header_columns[1],
        );
        let daemon_value = format!("[{daemon_status}]");
        let prefix_width = 8 + daemon_value.chars().count() + 10;
        let output_width = header_rows[1].width.saturating_sub(prefix_width as u16) as usize;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" DAEMON ", Style::default().fg(MUTED)),
                Span::styled(
                    daemon_value,
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled("  OUTPUTS ", Style::default().fg(MUTED)),
                Span::styled(
                    truncate_with_ellipsis(&output_text, output_width),
                    Style::default().fg(TEXT),
                ),
            ])),
            header_rows[1],
        );

        let gauges = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .spacing(2)
            .split(areas.metrics);
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
            YELLOW,
            areas.compact,
        );
        render_metric(
            frame,
            gauges[1],
            "Brightness",
            format!("{}%", self.state.active_brightness),
            self.state.active_brightness,
            YELLOW,
            areas.compact,
        );

        render_settings(frame, areas.settings, &self.state.settings, self.selected);

        if let Some(help_area) = areas.help {
            let (selected_name, selected_help) = selected_help(self.selected);
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        " i ",
                        Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
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
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Min(15),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        ScreenAreas {
            header: areas[0],
            metrics: areas[1],
            settings: areas[2],
            help: None,
            notice: areas[3],
            footer: areas[4],
            compact,
        }
    } else {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Min(14),
            Constraint::Length(1),
            Constraint::Length(4),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
        ScreenAreas {
            header: areas[0],
            metrics: areas[2],
            settings: areas[4],
            help: Some(areas[6]),
            notice: areas[7],
            footer: areas[8],
            compact,
        }
    }
}

fn panel_block(title: &'static str) -> Block<'static> {
    Block::default()
        .title(Span::styled(
            title,
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().fg(TEXT))
}

fn render_metric(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
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
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(content);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("CURRENT   ", Style::default().fg(MUTED)),
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
        rows[1],
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
        rows[2],
    );
}

fn render_settings(frame: &mut Frame, area: Rect, settings: &Settings, selected: Field) {
    let block = panel_block(" SETTINGS ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let manual_active = settings.enabled && !settings.automatic;
    let schedule_active = settings.enabled && settings.automatic;
    let mode = if settings.automatic {
        "AUTOMATIC"
    } else {
        "MANUAL"
    };
    let spacious = inner.height >= 14;
    let mut items = vec![
        ListItem::new(""),
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
            schedule_active,
            false,
            true,
        ),
        value_item(
            "Day begins",
            settings.schedule.day_start.clone(),
            schedule_active,
            false,
            true,
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
            .fg(if active { YELLOW } else { MUTED })
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
        format!("[{value}]")
    };
    let base_color = if active { TEXT } else { MUTED };
    let value_color = if !active {
        MUTED
    } else if accent {
        YELLOW
    } else {
        TEXT
    };
    ListItem::new(Line::from(vec![
        Span::styled(format!("{label:<24}"), Style::default().fg(base_color)),
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
                    .fg(if error { YELLOW } else { MUTED })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                truncate_with_ellipsis(message, available),
                Style::default().fg(if error { YELLOW } else { TEXT }),
            ),
        ])),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, selected: Field) {
    let controls = if selected.is_toggle() {
        vec![
            key(" ↑/↓ "),
            muted("Navigate   "),
            key(" ←/→ "),
            muted("Set   "),
            key(" Enter/Space "),
            muted("Toggle   "),
            key(" q "),
            muted("Quit"),
        ]
    } else {
        vec![
            key(" ↑/↓ "),
            muted("Navigate   "),
            key(" ←/→ "),
            muted("Adjust   "),
            key(" Shift+←/→ "),
            muted("Fine   "),
            key(" q "),
            muted("Quit"),
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

fn key(label: &'static str) -> Span<'static> {
    Span::styled(
        label,
        Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
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
            "Set when the evening transition starts in local time.",
        ),
        Field::DayStart => (
            "Day begins",
            "Set when the morning transition back to neutral starts in local time.",
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

#[cfg(test)]
mod tests {
    use chrono::Local;
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;
    use crate::schedule::current_levels;

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
    fn dashboard_renders_full_and_compact_terminal_states() {
        let app = App::new(test_state(Settings::default()), false);

        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("WAYWARM"));
        assert!(rendered.contains("Filter"));
        assert!(rendered.contains("MANUAL OVERRIDE"));
        assert!(rendered.contains("Fade duration"));
        assert!(rendered.contains("HELP"));
        assert!(rendered.contains("→ Mode"));
        assert!(!rendered.contains("› Schedule"));

        let mut compact = Terminal::new(TestBackend::new(80, 24)).unwrap();
        compact.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&compact);
        assert!(rendered.contains("Fade duration"));
        assert!(rendered.contains("Connected service"));
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

        assert!(rendered.contains("[AUTOMATIC]"));
        assert!(rendered.contains("[OFF]"));
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
        let mut terminal = Terminal::new(TestBackend::new(120, 36)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();

        for cell in terminal.backend().buffer().content() {
            assert!(matches!(
                cell.fg,
                Color::Reset | Color::White | Color::Yellow | Color::Gray | Color::DarkGray
            ));
            assert_eq!(cell.bg, Color::Reset);
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
