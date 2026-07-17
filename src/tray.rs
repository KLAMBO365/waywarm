//! StatusNotifierItem tray for quick toggle without opening the TUI.

use std::{
    process::Command,
    sync::{
        Arc, LazyLock,
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
        // Prefer embedded pixmaps; keep a theme fallback for hosts that ignore IconPixmap.
        if self.enabled {
            "night-light-symbolic".into()
        } else {
            "weather-clear-symbolic".into()
        }
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        if self.enabled {
            icons_on().to_vec()
        } else {
            icons_off().to_vec()
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
            icon_pixmap: self.icon_pixmap(),
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

fn icons_on() -> &'static [ksni::Icon] {
    static ICONS: LazyLock<Vec<ksni::Icon>> = LazyLock::new(|| {
        vec![
            decode_png_icon(include_bytes!("../assets/waywarm-tray-on-32.png")),
            decode_png_icon(include_bytes!("../assets/waywarm-tray-on-64.png")),
        ]
    });
    &ICONS
}

fn icons_off() -> &'static [ksni::Icon] {
    static ICONS: LazyLock<Vec<ksni::Icon>> = LazyLock::new(|| {
        vec![
            decode_png_icon(include_bytes!("../assets/waywarm-tray-off-32.png")),
            decode_png_icon(include_bytes!("../assets/waywarm-tray-off-64.png")),
        ]
    });
    &ICONS
}

fn decode_png_icon(bytes: &[u8]) -> ksni::Icon {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("tray icon png header");
    let mut rgba = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut rgba).expect("tray icon png frame");
    rgba.truncate(info.buffer_size());
    // PNG is RGBA; StatusNotifierItem wants ARGB32 in network (big-endian) byte order.
    let mut data = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        let [r, g, b, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
        data.extend_from_slice(&u32::from_be_bytes([a, r, g, b]).to_be_bytes());
    }
    ksni::Icon {
        width: info.width as i32,
        height: info.height as i32,
        data,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tray_icons_decode() {
        let on = icons_on();
        let off = icons_off();
        assert_eq!(on.len(), 2);
        assert_eq!(off.len(), 2);
        assert_eq!(on[0].width, 32);
        assert_eq!(on[1].width, 64);
        assert_eq!(on[0].data.len(), 32 * 32 * 4);
        assert!(!on[0].data.iter().all(|b| *b == 0));
        assert!(!off[0].data.iter().all(|b| *b == 0));
    }
}
