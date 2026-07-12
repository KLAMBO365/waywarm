use std::{
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;
use ratatui::{
    DefaultTerminal, Frame,
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Alignment, Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::service::{ServiceManager, ServiceStatus};

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

pub fn run() -> Result<()> {
    let manager = ServiceManager::discover()?;
    let mut app = App::new(manager);
    ratatui::run(|terminal| app.run(terminal))
}

struct App {
    manager: ServiceManager,
    status: ServiceStatus,
    selected: usize,
    message: String,
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
                self.message = "Wait for the current service action to finish".into();
            }
            KeyCode::Up | KeyCode::Char('k') => {
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

    fn activate(&mut self) -> bool {
        if self.pending.is_some() {
            self.message = "A service action is already running".into();
            return false;
        }
        let action = Action::ALL[self.selected];
        if action == Action::Quit {
            return true;
        }
        if action == Action::Uninstall && !self.confirm_uninstall {
            self.confirm_uninstall = true;
            self.message = "Press Enter again to uninstall the service and installed binary".into();
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
        self.message = format!("{}…", action.label());
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
                self.message = result.unwrap_or_else(|error| error);
                self.pending = None;
                self.status = self.manager.status();
                self.last_refresh = Instant::now();
            }
            Err(TryRecvError::Disconnected) => {
                self.message = "Service action ended without a result".into();
                self.pending = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn render(&self, frame: &mut Frame) {
        let area = centered_area(frame.area(), 64, 17);
        let sections = Layout::vertical([
            Constraint::Length(4),
            Constraint::Min(7),
            Constraint::Length(3),
        ])
        .split(area);

        let (daemon_status, daemon_color) = if self.status.active {
            ("running", Color::Green)
        } else {
            ("stopped", Color::Red)
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "Waywarm background service",
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                )),
                Line::from(vec![
                    Span::raw("Daemon: "),
                    Span::styled(
                        daemon_status,
                        Style::default()
                            .fg(daemon_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!(" • {}", self.status.installation_label())),
                ]),
            ])
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL)),
            sections[0],
        );

        let items: Vec<ListItem<'_>> = Action::ALL
            .iter()
            .map(|action| ListItem::new(action.label()))
            .collect();
        let mut state = ListState::default().with_selected(Some(self.selected));
        frame.render_stateful_widget(
            List::new(items)
                .block(Block::default().title(" Actions ").borders(Borders::ALL))
                .highlight_symbol("▶ ")
                .highlight_spacing(HighlightSpacing::Always)
                .highlight_style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            sections[1],
            &mut state,
        );

        frame.render_widget(
            Paragraph::new(vec![
                Line::from(self.message.clone()),
                Line::from("↑/↓ select  Enter run  q quit"),
            ])
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(Color::DarkGray)),
            sections[2],
        );
    }
}

fn centered_area(area: ratatui::layout::Rect, width: u16, height: u16) -> ratatui::layout::Rect {
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
