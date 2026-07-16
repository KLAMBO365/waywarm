use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Alignment, Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, BorderType, Borders, HighlightSpacing, List, ListItem, ListState, Padding,
        Paragraph, Wrap,
    },
};

use crate::service::{ServiceManager, ServiceStatus};

// ── palette (aligned with settings dashboard) ────────────────────────────────

const TEXT: Color = Color::White;
const MUTED: Color = Color::DarkGray;
const BORDER: Color = Color::DarkGray;
const ACCENT: Color = Color::Yellow;
const WARM: Color = Color::LightYellow;
const COOL: Color = Color::Cyan;

// ── actions ──────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Install,
    Start,
    Stop,
    Restart,
    Uninstall,
    Quit,
}

impl Action {
    const ALL: [Self; 6] = [
        Self::Install,
        Self::Start,
        Self::Stop,
        Self::Restart,
        Self::Uninstall,
        Self::Quit,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Install => "Install or update and start",
            Self::Start => "Start",
            Self::Stop => "Stop",
            Self::Restart => "Restart",
            Self::Uninstall => "Uninstall",
            Self::Quit => "Quit",
        }
    }

    fn help(self) -> &'static str {
        match self {
            Self::Install => {
                "Copy the binary, install the user unit, enable it, and start the daemon."
            }
            Self::Start => "Start the installed user service if it is not already running.",
            Self::Stop => "Stop the running daemon without removing the installation.",
            Self::Restart => {
                "Restart the daemon and refresh imported Wayland environment variables."
            }
            Self::Uninstall => {
                "Stop the service, remove the user unit and installed binary. Requires confirmation."
            }
            Self::Quit => "Leave the service manager and return to the shell.",
        }
    }

    fn requires_install(self) -> bool {
        matches!(
            self,
            Self::Start | Self::Stop | Self::Restart | Self::Uninstall
        )
    }

    fn run(self, manager: &ServiceManager) -> Result<()> {
        match self {
            Self::Install => manager.install_and_start(),
            Self::Start => manager.start(),
            Self::Stop => manager.stop(),
            Self::Restart => manager.restart(),
            Self::Uninstall => manager.uninstall(),
            Self::Quit => unreachable!(),
        }
    }
}

// ── entry ────────────────────────────────────────────────────────────────────

pub fn run() -> Result<()> {
    let manager = ServiceManager::discover()?;
    let mut app = App::new(manager);
    ratatui::run(|terminal| app.run(terminal))
}

// ── app ──────────────────────────────────────────────────────────────────────

struct App {
    manager: ServiceManager,
    status: ServiceStatus,
    selected: usize,
    message: String,
    message_error: bool,
    confirm_uninstall: bool,
    pending: Option<Receiver<Result<String, String>>>,
    last_refresh: Instant,
}

impl App {
    fn new(manager: ServiceManager) -> Self {
        let status = manager.status();
        Self {
            manager,
            status,
            selected: 0,
            message: "Choose an action and press Enter".into(),
            message_error: false,
            confirm_uninstall: false,
            pending: None,
            last_refresh: Instant::now(),
        }
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        loop {
            terminal.draw(|frame| self.render(frame))?;
            if event::poll(Duration::from_millis(200))?
                && let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
                && self.handle_key(key.code)
            {
                return Ok(());
            }
            self.poll_action();
            if self.pending.is_none() && self.last_refresh.elapsed() >= Duration::from_secs(2) {
                self.status = self.manager.status();
                self.last_refresh = Instant::now();
            }
        }
    }

    fn handle_key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') | KeyCode::Esc if self.pending.is_none() => return true,
            KeyCode::Char('q') | KeyCode::Esc => {
                self.set_message("Wait for the current service action to finish".into(), true);
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                self.selected = self
                    .selected
                    .checked_sub(1)
                    .unwrap_or(Action::ALL.len() - 1);
                self.confirm_uninstall = false;
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.selected = (self.selected + 1) % Action::ALL.len();
                self.confirm_uninstall = false;
            }
            KeyCode::Enter | KeyCode::Char(' ') => return self.activate(),
            _ => {}
        }
        false
    }

    fn set_message(&mut self, message: String, error: bool) {
        self.message = message;
        self.message_error = error;
    }

    fn activate(&mut self) -> bool {
        if self.pending.is_some() {
            self.set_message("A service action is already running".into(), true);
            return false;
        }
        let action = Action::ALL[self.selected];
        if action == Action::Quit {
            return true;
        }
        if action == Action::Uninstall && !self.confirm_uninstall {
            self.confirm_uninstall = true;
            self.set_message(
                "Press Enter again to uninstall the service and installed binary".into(),
                true,
            );
            return false;
        }
        self.confirm_uninstall = false;
        let manager = self.manager.clone();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = action.run(&manager);
            let message = result
                .map(|()| format!("{} completed", action.label()))
                .map_err(|error| format!("Action failed: {error:#}"));
            let _ = sender.send(message);
        });
        self.set_message(format!("{}…", action.label()), false);
        self.pending = Some(receiver);
        self.last_refresh = Instant::now();
        false
    }

    fn poll_action(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        match receiver.try_recv() {
            Ok(result) => {
                match result {
                    Ok(message) => self.set_message(message, false),
                    Err(error) => self.set_message(error, true),
                }
                self.pending = None;
                self.status = self.manager.status();
                self.last_refresh = Instant::now();
            }
            Err(TryRecvError::Disconnected) => {
                self.set_message("Service action ended without a result".into(), true);
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn render(&self, frame: &mut Frame) {
        frame.render_widget(
            Block::default().style(Style::default().fg(TEXT)),
            frame.area(),
        );

        if frame.area().width < 60 || frame.area().height < 18 {
            frame.render_widget(
                Paragraph::new("Waywarm needs a terminal of at least 60 × 18")
                    .alignment(Alignment::Center)
                    .style(Style::default().fg(ACCENT)),
                frame.area(),
            );
            return;
        }

        let content = frame.area().inner(Margin {
            horizontal: if frame.area().width >= 90 { 2 } else { 1 },
            vertical: 0,
        });
        let area = centered_area(content, 72, 22.min(content.height));
        let sections = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        self.render_header(frame, sections[0]);
        self.render_status(frame, sections[1]);
        self.render_actions(frame, sections[2]);
        self.render_help(frame, sections[3]);
        self.render_notice(frame, sections[4]);
        self.render_footer(frame, sections[5]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(BORDER));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let columns = Layout::horizontal([Constraint::Min(1), Constraint::Length(12)]).split(inner);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " WAYWARM ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled("·", Style::default().fg(MUTED)),
                Span::styled(" service ", Style::default().fg(MUTED)),
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
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let block = panel_block(" STATUS ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let (run_label, run_color) = if self.status.active {
            (" RUNNING ", COOL)
        } else {
            (" STOPPED ", ACCENT)
        };
        let (install_label, install_color) = if self.status.installed {
            (" INSTALLED ", WARM)
        } else {
            (" NOT INSTALLED ", MUTED)
        };
        let (enabled_label, enabled_color) = if !self.status.installed {
            (" — ", MUTED)
        } else if self.status.enabled {
            (" ENABLED ", COOL)
        } else {
            (" DISABLED ", MUTED)
        };

        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }));

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Daemon  ", Style::default().fg(MUTED)),
                chip(run_label, run_color),
                Span::raw("  "),
                chip(install_label, install_color),
                Span::raw("  "),
                chip(enabled_label, enabled_color),
            ])),
            rows[0],
        );

        let binary = truncate_with_ellipsis(
            &self.manager.binary_path().display().to_string(),
            rows[1].width.saturating_sub(10) as usize,
        );
        let unit = truncate_with_ellipsis(
            &self.manager.unit_path().display().to_string(),
            rows[2].width.saturating_sub(10) as usize,
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Binary  ", Style::default().fg(MUTED)),
                Span::styled(binary, Style::default().fg(TEXT)),
            ])),
            rows[1],
        );
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("Unit    ", Style::default().fg(MUTED)),
                Span::styled(unit, Style::default().fg(TEXT)),
            ])),
            rows[2],
        );
    }

    fn render_actions(&self, frame: &mut Frame, area: Rect) {
        let block = panel_block(" ACTIONS ");
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let items: Vec<ListItem<'_>> = Action::ALL
            .iter()
            .map(|action| {
                let available = !action.requires_install() || self.status.installed;
                let mut label = action.label().to_owned();
                if *action == Action::Uninstall && self.confirm_uninstall {
                    label = format!("{label}  ·  confirm?");
                }
                if self.pending.is_some() && Action::ALL.get(self.selected) == Some(action) {
                    label = format!("{label}  …");
                }
                let style = if available {
                    Style::default().fg(TEXT)
                } else {
                    Style::default().fg(MUTED)
                };
                ListItem::new(Span::styled(label, style))
            })
            .collect();

        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(
            List::new(items)
                .highlight_symbol("→ ")
                .highlight_spacing(HighlightSpacing::Always)
                .highlight_style(Style::default().fg(TEXT).add_modifier(Modifier::BOLD)),
            inner,
            &mut state,
        );
    }

    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let action = Action::ALL[self.selected];
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " i ",
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{}. ", action.label()),
                    Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(action.help(), Style::default().fg(MUTED)),
            ]))
            .wrap(Wrap { trim: true })
            .block(panel_block(" HELP ").padding(Padding::horizontal(1))),
            area,
        );
    }

    fn render_notice(&self, frame: &mut Frame, area: Rect) {
        let (prefix, color) = if self.message_error || self.confirm_uninstall {
            (" ! ", ACCENT)
        } else if self.pending.is_some() {
            (" … ", WARM)
        } else {
            (" · ", MUTED)
        };
        let available = area.width.saturating_sub(prefix.len() as u16) as usize;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_with_ellipsis(&self.message, available),
                    Style::default().fg(if self.message_error || self.confirm_uninstall {
                        ACCENT
                    } else {
                        TEXT
                    }),
                ),
            ])),
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                key_chip("↑/↓"),
                muted(" Navigate  "),
                key_chip("Enter"),
                muted(" Run  "),
                key_chip("q"),
                muted(" Quit"),
            ]))
            .alignment(Alignment::Center),
            area,
        );
    }
}

fn chip(label: &'static str, color: Color) -> Span<'static> {
    Span::styled(
        label,
        Style::default()
            .fg(color)
            .add_modifier(Modifier::BOLD)
            .add_modifier(Modifier::REVERSED),
    )
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

fn panel_block(title: &'static str) -> Block<'static> {
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

fn centered_area(area: Rect, width: u16, height: u16) -> Rect {
    let horizontal = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(width.min(area.width)),
        Constraint::Fill(1),
    ])
    .split(area);
    Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height.min(area.height)),
        Constraint::Fill(1),
    ])
    .split(horizontal[1])[1]
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend};

    use super::*;

    fn test_manager() -> ServiceManager {
        ServiceManager::discover().expect("HOME should be set in tests")
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

    #[test]
    fn action_help_covers_every_entry() {
        for action in Action::ALL {
            assert!(
                !action.help().is_empty(),
                "{:?} missing help",
                action.label()
            );
        }
    }

    #[test]
    fn service_dashboard_renders_chrome_and_actions() {
        let mut app = App::new(test_manager());
        app.status = ServiceStatus {
            installed: true,
            enabled: true,
            active: true,
        };

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&terminal);

        assert!(rendered.contains("WAYWARM"));
        assert!(rendered.contains("service"));
        assert!(rendered.contains("STATUS"));
        assert!(rendered.contains("ACTIONS"));
        assert!(rendered.contains("HELP"));
        assert!(rendered.contains("Install or update and start"));
        assert!(rendered.contains("RUNNING") || rendered.contains("STOPPED"));
        assert!(rendered.contains("→ Install") || rendered.contains("Install or update"));
    }

    #[test]
    fn small_terminals_show_minimum_size_message() {
        let app = App::new(test_manager());
        let mut terminal = Terminal::new(TestBackend::new(50, 12)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        assert!(buffer_text(&terminal).contains("at least 60 × 18"));
    }

    #[test]
    fn uninstall_confirm_is_visible_in_notice() {
        let mut app = App::new(test_manager());
        app.status = ServiceStatus {
            installed: true,
            enabled: false,
            active: false,
        };
        app.selected = Action::ALL
            .iter()
            .position(|action| *action == Action::Uninstall)
            .unwrap();
        app.confirm_uninstall = true;
        app.set_message(
            "Press Enter again to uninstall the service and installed binary".into(),
            true,
        );

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let rendered = buffer_text(&terminal);
        assert!(rendered.contains("confirm?"));
        assert!(rendered.contains("Press Enter again"));
    }

    #[test]
    fn dashboard_uses_only_the_supported_terminal_colors() {
        let mut app = App::new(test_manager());
        app.status = ServiceStatus {
            installed: true,
            enabled: true,
            active: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
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

    #[test]
    fn start_requires_install_but_install_does_not() {
        assert!(Action::Start.requires_install());
        assert!(Action::Stop.requires_install());
        assert!(!Action::Install.requires_install());
        assert!(!Action::Quit.requires_install());
    }
}
