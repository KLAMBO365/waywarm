//! StatusNotifierItem tray for quick toggle without opening the TUI.

use std::{
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use ksni::blocking::TrayMethods;

use crate::ipc::{query_state, replace_settings};

struct WaywarmTray {
    enabled: bool,
    automatic: bool,
    conflict: Option<String>,
    should_quit: Arc<AtomicBool>,
}

impl WaywarmTray {
    fn refresh_from_daemon(&mut self) {
        match query_state() {
            Ok(state) => {
                self.enabled = state.settings.enabled;
                self.automatic = state.settings.automatic;
                self.conflict = state.conflict;
            }
            Err(_) => {
                // Keep last known values; tooltip still notes offline below.
                self.conflict = Some("daemon unreachable".into());
            }
        }
    }

    fn toggle_filter(&mut self) {
        let Ok(mut state) = query_state() else {
            self.conflict = Some("daemon unreachable".into());
            return;
        };
        state.settings.enabled = !state.settings.enabled;
        match replace_settings(state.settings) {
            Ok(state) => {
                self.enabled = state.settings.enabled;
                self.automatic = state.settings.automatic;
                self.conflict = state.conflict;
            }
            Err(error) => {
                self.conflict = Some(format!("toggle failed: {error:#}"));
            }
        }
    }
}

impl ksni::Tray for WaywarmTray {
    fn id(&self) -> String {
        "waywarm".into()
    }

    fn title(&self) -> String {
        "Waywarm".into()
    }

    fn icon_name(&self) -> String {
        if self.enabled {
            "weather-clear-night".into()
        } else {
            "weather-clear".into()
        }
    }

    fn status(&self) -> ksni::Status {
        if self.conflict.is_some() {
            ksni::Status::NeedsAttention
        } else {
            ksni::Status::Active
        }
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let mode = if self.automatic {
            "automatic"
        } else {
            "manual"
        };
        let filter = if self.enabled { "on" } else { "off" };
        let mut description = format!("Filter {filter} · {mode}");
        if let Some(conflict) = &self.conflict {
            description.push('\n');
            description.push_str(conflict);
        }
        ksni::ToolTip {
            title: "Waywarm".into(),
            description,
            icon_name: self.icon_name(),
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.toggle_filter();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let filter_label = if self.enabled {
            "Disable filter"
        } else {
            "Enable filter"
        };
        vec![
            StandardItem {
                label: filter_label.into(),
                activate: Box::new(|this: &mut Self| this.toggle_filter()),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Open settings".into(),
                icon_name: "preferences-system".into(),
                activate: Box::new(|_| {
                    let _ = Command::new("waywarm").spawn();
                }),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit tray".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| {
                    this.should_quit.store(true, Ordering::Relaxed);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Run the StatusNotifier tray until quit. Requires a running daemon.
pub fn run() -> Result<()> {
    let state = query_state().context(
        "Waywarm daemon is not running; start it with `waywarm daemon` or open the settings UI first",
    )?;
    let should_quit = Arc::new(AtomicBool::new(false));
    let tray = WaywarmTray {
        enabled: state.settings.enabled,
        automatic: state.settings.automatic,
        conflict: state.conflict,
        should_quit: should_quit.clone(),
    };
    let handle = tray
        .spawn()
        .context("failed to create StatusNotifier tray (is a tray host running?)")?;

    while !should_quit.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        handle.update(|tray: &mut WaywarmTray| tray.refresh_from_daemon());
    }

    handle.shutdown().wait();
    Ok(())
}
