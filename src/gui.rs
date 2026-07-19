//! GTK-4 settings dashboard mirroring the TUI layout.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

use adw::prelude::*;
use anyhow::Result;
use chrono::Local;
use gtk::{
    Align, Orientation, PolicyType, glib,
    pango::{self, Weight},
};

use crate::{
    config::{MAX_TRANSITION_MINUTES, MIN_BRIGHTNESS, ScheduleTiming, Settings, parse_time},
    daemon::TransientBackend,
    gamma::warmth_to_kelvin,
    ipc::{query_state, replace_settings},
    protocol::RuntimeState,
    ui_common::{
        Field, MINUTES_PER_DAY, TimelinePhase, TimelineView, backend_status, connect_or_start,
        field_help, format_hhmm, is_preset_name_char,
    },
};

const APP_ID: &str = "dev.waywarm.Waywarm";

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

    fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            error: false,
            expires_at: Some(Instant::now() + Duration::from_secs(3)),
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

#[derive(Clone)]
struct GuiState {
    state: RuntimeState,
    transient: bool,
    backend_available: bool,
    notice: Option<Notice>,
    selected: Field,
}

impl Clone for Notice {
    fn clone(&self) -> Self {
        Self {
            text: self.text.clone(),
            error: self.error,
            expires_at: self.expires_at,
        }
    }
}

impl GuiState {
    fn new(state: RuntimeState, transient: bool) -> Self {
        Self {
            state,
            transient,
            backend_available: true,
            notice: None,
            selected: Field::Mode,
        }
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

    fn notice_text(&self) -> (String, bool) {
        if let Some(notice) = &self.notice {
            return (notice.text.clone(), notice.error);
        }
        if self.transient {
            ("Temporary session — changes last until exit".into(), false)
        } else {
            ("Connected service — changes save immediately".into(), false)
        }
    }
}

struct Widgets {
    daemon_label: gtk::Label,
    outputs_label: gtk::Label,
    warmth_value: gtk::Label,
    warmth_bar: gtk::LevelBar,
    brightness_value: gtk::Label,
    brightness_bar: gtk::LevelBar,
    timeline_title: gtk::Label,
    timeline_legend: gtk::Label,
    timeline_area: gtk::DrawingArea,
    timeline_view: Rc<RefCell<Option<TimelineView>>>,
    timeline_active: Rc<Cell<bool>>,
    mode_switch: gtk::Switch,
    filter_switch: gtk::Switch,
    preset_dropdown: gtk::DropDown,
    preset_apply: gtk::Button,
    preset_save: gtk::Button,
    preset_delete: gtk::Button,
    manual_warmth: gtk::Scale,
    manual_brightness: gtk::Scale,
    timing_dropdown: gtk::DropDown,
    day_warmth: gtk::Scale,
    day_brightness: gtk::Scale,
    night_warmth: gtk::Scale,
    night_brightness: gtk::Scale,
    night_start: gtk::Entry,
    day_start: gtk::Entry,
    latitude: gtk::SpinButton,
    longitude: gtk::SpinButton,
    transition: gtk::SpinButton,
    manual_group: gtk::ListBox,
    schedule_group: gtk::ListBox,
    night_start_row: gtk::ListBoxRow,
    day_start_row: gtk::ListBoxRow,
    latitude_row: gtk::ListBoxRow,
    longitude_row: gtk::ListBoxRow,
    help_label: gtk::Label,
    notice_label: gtk::Label,
}

pub fn run() -> Result<()> {
    let (state, transient_backend) = connect_or_start()?;
    let transient = transient_backend.is_some();
    let _backend_keepalive: Rc<RefCell<Option<TransientBackend>>> =
        Rc::new(RefCell::new(transient_backend));

    let gui = Rc::new(RefCell::new(GuiState::new(state, transient)));

    let app = adw::Application::builder().application_id(APP_ID).build();
    let backend_keepalive = _backend_keepalive.clone();
    let gui_for_activate = gui.clone();
    app.connect_activate(move |app| {
        let _hold = backend_keepalive.clone();
        build_window(app, gui_for_activate.clone(), _hold);
    });

    let code = app.run();
    if code != glib::ExitCode::SUCCESS {
        anyhow::bail!("GTK application exited with status {code:?}");
    }
    Ok(())
}

fn build_window(
    app: &adw::Application,
    gui: Rc<RefCell<GuiState>>,
    _backend: Rc<RefCell<Option<TransientBackend>>>,
) {
    let updating = Rc::new(Cell::new(false));
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Waywarm")
        .default_width(720)
        .default_height(900)
        .build();

    let header = adw::HeaderBar::new();
    let title = gtk::Label::builder()
        .label("Waywarm")
        .css_classes(["title-1"])
        .build();
    header.set_title_widget(Some(&title));

    let daemon_label = gtk::Label::builder()
        .css_classes(["caption", "accent"])
        .halign(Align::Start)
        .build();
    let outputs_label = gtk::Label::builder()
        .halign(Align::Start)
        .ellipsize(pango::EllipsizeMode::End)
        .hexpand(true)
        .build();

    let status_row = gtk::Box::new(Orientation::Horizontal, 12);
    status_row.set_margin_start(18);
    status_row.set_margin_end(18);
    status_row.set_margin_top(6);
    status_row.append(&daemon_label);
    status_row.append(&outputs_label);

    let (warmth_card, warmth_value, warmth_bar) = metric_card("Warmth");
    let (brightness_card, brightness_value, brightness_bar) = metric_card("Brightness");
    let metrics = gtk::Box::new(Orientation::Horizontal, 12);
    metrics.set_margin_start(18);
    metrics.set_margin_end(18);
    metrics.set_margin_top(12);
    metrics.set_homogeneous(true);
    metrics.append(&warmth_card);
    metrics.append(&brightness_card);

    let timeline_title = gtk::Label::builder()
        .label("Today")
        .halign(Align::Start)
        .css_classes(["heading"])
        .build();
    let timeline_legend = gtk::Label::builder()
        .halign(Align::Start)
        .css_classes(["dim-label", "caption"])
        .wrap(true)
        .build();
    let timeline_view = Rc::new(RefCell::new(None::<TimelineView>));
    let timeline_active = Rc::new(Cell::new(false));
    let timeline_area = gtk::DrawingArea::builder()
        .content_height(28)
        .hexpand(true)
        .build();
    {
        let view = timeline_view.clone();
        let active = timeline_active.clone();
        timeline_area.set_draw_func(move |_, cr, width, height| {
            paint_timeline(cr, width, height, view.borrow().as_ref(), active.get());
        });
    }

    let timeline_box = gtk::Box::new(Orientation::Vertical, 6);
    timeline_box.set_margin_start(18);
    timeline_box.set_margin_end(18);
    timeline_box.set_margin_top(12);
    timeline_box.append(&timeline_title);
    timeline_box.append(&timeline_area);
    timeline_box.append(&timeline_legend);

    let general = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let mode_switch = gtk::Switch::new();
    let filter_switch = gtk::Switch::new();
    general.append(&action_row(
        "Mode",
        "Automatic schedule",
        Some(mode_switch.clone().upcast()),
    ));
    general.append(&action_row(
        "Filter",
        "Color temperature filtering",
        Some(filter_switch.clone().upcast()),
    ));

    let preset_dropdown = gtk::DropDown::from_strings(&["(none)"]);
    preset_dropdown.set_hexpand(true);
    let preset_apply = gtk::Button::with_label("Apply");
    let preset_save = gtk::Button::with_label("Save");
    let preset_delete = gtk::Button::with_label("Delete");
    preset_delete.add_css_class("destructive-action");
    let preset_buttons = gtk::Box::new(Orientation::Horizontal, 6);
    preset_buttons.append(&preset_dropdown);
    preset_buttons.append(&preset_apply);
    preset_buttons.append(&preset_save);
    preset_buttons.append(&preset_delete);
    general.append(&action_row(
        "Preset",
        "Named profiles",
        Some(preset_buttons.upcast()),
    ));

    let manual_group = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let manual_warmth = percent_scale(0, 100);
    let manual_brightness = percent_scale(MIN_BRIGHTNESS, 100);
    manual_group.append(&scale_row("Warmth", &manual_warmth));
    manual_group.append(&scale_row("Brightness", &manual_brightness));

    let schedule_group = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let timing_dropdown = gtk::DropDown::from_strings(&["Fixed", "Location"]);
    schedule_group.append(&action_row(
        "Timing",
        "How day and night times are chosen",
        Some(timing_dropdown.clone().upcast()),
    ));
    let day_warmth = percent_scale(0, 100);
    let day_brightness = percent_scale(MIN_BRIGHTNESS, 100);
    let night_warmth = percent_scale(0, 100);
    let night_brightness = percent_scale(MIN_BRIGHTNESS, 100);
    schedule_group.append(&scale_row("Day warmth", &day_warmth));
    schedule_group.append(&scale_row("Day brightness", &day_brightness));
    schedule_group.append(&scale_row("Night warmth", &night_warmth));
    schedule_group.append(&scale_row("Night brightness", &night_brightness));

    let night_start = gtk::Entry::builder()
        .placeholder_text("HH:MM")
        .max_length(5)
        .width_chars(5)
        .build();
    let day_start = gtk::Entry::builder()
        .placeholder_text("HH:MM")
        .max_length(5)
        .width_chars(5)
        .build();
    let night_start_row = action_row(
        "Night begins",
        "Clock time",
        Some(night_start.clone().upcast()),
    );
    let day_start_row = action_row("Day begins", "Clock time", Some(day_start.clone().upcast()));
    schedule_group.append(&night_start_row);
    schedule_group.append(&day_start_row);

    let latitude = gtk::SpinButton::with_range(-90.0, 90.0, 0.1);
    latitude.set_digits(2);
    let longitude = gtk::SpinButton::with_range(-180.0, 180.0, 0.1);
    longitude.set_digits(2);
    let latitude_row = action_row("Latitude", "Degrees north", Some(latitude.clone().upcast()));
    let longitude_row = action_row(
        "Longitude",
        "Degrees east",
        Some(longitude.clone().upcast()),
    );
    schedule_group.append(&latitude_row);
    schedule_group.append(&longitude_row);

    let transition = gtk::SpinButton::with_range(0.0, f64::from(MAX_TRANSITION_MINUTES), 1.0);
    transition.set_digits(0);
    schedule_group.append(&action_row(
        "Fade duration",
        "Minutes",
        Some(transition.clone().upcast()),
    ));

    let help_label = gtk::Label::builder()
        .wrap(true)
        .halign(Align::Start)
        .css_classes(["dim-label"])
        .build();
    let notice_label = gtk::Label::builder()
        .wrap(true)
        .halign(Align::Start)
        .build();

    let controls = gtk::Box::new(Orientation::Vertical, 18);
    controls.set_margin_start(18);
    controls.set_margin_end(18);
    controls.set_margin_top(18);
    controls.set_margin_bottom(18);
    controls.append(&section_label("General"));
    controls.append(&general);
    controls.append(&section_label("Manual override"));
    controls.append(&manual_group);
    controls.append(&section_label("Schedule"));
    controls.append(&schedule_group);
    controls.append(&section_label("Help"));
    controls.append(&help_label);
    controls.append(&notice_label);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .child(&controls)
        .build();

    let root = gtk::Box::new(Orientation::Vertical, 0);
    root.append(&header);
    root.append(&status_row);
    root.append(&metrics);
    root.append(&timeline_box);
    root.append(&scrolled);
    window.set_content(Some(&root));

    let widgets = Rc::new(Widgets {
        daemon_label,
        outputs_label,
        warmth_value,
        warmth_bar,
        brightness_value,
        brightness_bar,
        timeline_title,
        timeline_legend,
        timeline_area,
        timeline_view,
        timeline_active,
        mode_switch,
        filter_switch,
        preset_dropdown,
        preset_apply,
        preset_save,
        preset_delete,
        manual_warmth,
        manual_brightness,
        timing_dropdown,
        day_warmth,
        day_brightness,
        night_warmth,
        night_brightness,
        night_start,
        day_start,
        latitude,
        longitude,
        transition,
        manual_group,
        schedule_group,
        night_start_row,
        day_start_row,
        latitude_row,
        longitude_row,
        help_label,
        notice_label,
    });

    bind_controls(gui.clone(), widgets.clone(), updating.clone());
    refresh_from(&gui, &widgets, &updating);

    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        glib::timeout_add_local(Duration::from_secs(1), move || {
            gui.borrow_mut().expire_notice();
            match query_state() {
                Ok(state) => {
                    let mut g = gui.borrow_mut();
                    let was_offline = !g.backend_available;
                    g.state = state;
                    g.backend_available = true;
                    if was_offline {
                        g.notice = Some(Notice::info("Display backend reconnected"));
                    } else if let Some(conflict) = &g.state.conflict {
                        let text = format!("Conflict: {conflict}");
                        let already = g.notice.as_ref().is_some_and(|notice| notice.text == text);
                        if !already {
                            g.notice = Some(Notice::error(text));
                        }
                    }
                    drop(g);
                    refresh_from(&gui, &widgets, &updating);
                }
                Err(error) => {
                    let mut g = gui.borrow_mut();
                    g.backend_available = false;
                    g.notice = Some(Notice::error(format!(
                        "Display backend unavailable: {error:#}"
                    )));
                    drop(g);
                    refresh_from(&gui, &widgets, &updating);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    window.present();
}

fn section_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .halign(Align::Start)
        .css_classes(["heading"])
        .build()
}

fn metric_card(title: &str) -> (gtk::Frame, gtk::Label, gtk::LevelBar) {
    let value = gtk::Label::builder()
        .halign(Align::Start)
        .css_classes(["title-4"])
        .build();
    let bar = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(100.0)
        .mode(gtk::LevelBarMode::Continuous)
        .hexpand(true)
        .build();
    let inner = gtk::Box::new(Orientation::Vertical, 8);
    inner.set_margin_top(12);
    inner.set_margin_bottom(12);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    let heading = gtk::Label::builder()
        .label(title)
        .halign(Align::Start)
        .css_classes(["caption-heading"])
        .build();
    inner.append(&heading);
    inner.append(&value);
    inner.append(&bar);
    let frame = gtk::Frame::new(None);
    frame.set_child(Some(&inner));
    (frame, value, bar)
}

fn action_row(title: &str, subtitle: &str, suffix: Option<gtk::Widget>) -> gtk::ListBoxRow {
    let row = adw::ActionRow::builder()
        .title(title)
        .subtitle(subtitle)
        .build();
    if let Some(widget) = suffix {
        widget.set_valign(Align::Center);
        row.add_suffix(&widget);
        if widget.is::<gtk::Switch>() {
            row.set_activatable_widget(Some(&widget));
        }
    }
    let list_row = gtk::ListBoxRow::new();
    list_row.set_child(Some(&row));
    list_row.set_activatable(false);
    list_row
}

fn scale_row(title: &str, scale: &gtk::Scale) -> gtk::ListBoxRow {
    let row = adw::ActionRow::builder().title(title).build();
    scale.set_hexpand(true);
    scale.set_width_request(180);
    scale.set_valign(Align::Center);
    row.add_suffix(scale);
    let list_row = gtk::ListBoxRow::new();
    list_row.set_child(Some(&row));
    list_row.set_activatable(false);
    list_row
}

fn percent_scale(min: u8, max: u8) -> gtk::Scale {
    let scale =
        gtk::Scale::with_range(Orientation::Horizontal, f64::from(min), f64::from(max), 1.0);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Right);
    scale.set_digits(0);
    scale
}

fn paint_timeline(
    cr: &gtk::cairo::Context,
    width: i32,
    height: i32,
    timeline: Option<&TimelineView>,
    active: bool,
) {
    let width = width.max(1) as f64;
    let height = height.max(1) as f64;
    cr.set_source_rgb(0.2, 0.2, 0.2);
    cr.rectangle(0.0, 0.0, width, height);
    let _ = cr.fill();

    let Some(timeline) = timeline else {
        return;
    };

    for segment in &timeline.segments {
        let x0 = f64::from(segment.start) / f64::from(MINUTES_PER_DAY) * width;
        let x1 = f64::from(segment.end) / f64::from(MINUTES_PER_DAY) * width;
        let (r, g, b) = segment.phase.rgb(active);
        cr.set_source_rgb(r, g, b);
        if matches!(
            segment.phase,
            TimelinePhase::EveningFade | TimelinePhase::MorningFade
        ) {
            // Hatch-like lighter band for fades.
            cr.rectangle(x0, height * 0.15, (x1 - x0).max(1.0), height * 0.7);
        } else {
            cr.rectangle(x0, 0.0, (x1 - x0).max(1.0), height);
        }
        let _ = cr.fill();
    }

    let now_x = f64::from(timeline.now_minute) / f64::from(MINUTES_PER_DAY) * width;
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(2.0);
    cr.move_to(now_x, 0.0);
    cr.line_to(now_x, height);
    let _ = cr.stroke();
}

fn bind_controls(gui: Rc<RefCell<GuiState>>, widgets: Rc<Widgets>, updating: Rc<Cell<bool>>) {
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let switch = widgets.mode_switch.clone();
        switch.connect_state_set(move |_, active| {
            if updating.get() {
                return glib::Propagation::Proceed;
            }
            apply_edit(&gui, &widgets, &updating, |settings| {
                settings.automatic = active
            });
            gui.borrow_mut().selected = Field::Mode;
            update_help(&gui.borrow(), &widgets);
            glib::Propagation::Proceed
        });
    }
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let switch = widgets.filter_switch.clone();
        switch.connect_state_set(move |_, active| {
            if updating.get() {
                return glib::Propagation::Proceed;
            }
            apply_edit(&gui, &widgets, &updating, |settings| {
                settings.enabled = active
            });
            gui.borrow_mut().selected = Field::Filter;
            update_help(&gui.borrow(), &widgets);
            glib::Propagation::Proceed
        });
    }
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let dropdown = widgets.timing_dropdown.clone();
        dropdown.connect_selected_notify(move |dropdown| {
            if updating.get() {
                return;
            }
            let location = dropdown.selected() == 1;
            apply_edit(&gui, &widgets, &updating, |settings| {
                settings.schedule.timing = if location {
                    ScheduleTiming::Location
                } else {
                    ScheduleTiming::Fixed
                };
            });
            gui.borrow_mut().selected = Field::Timing;
            update_help(&gui.borrow(), &widgets);
        });
    }

    bind_scale(
        widgets.manual_warmth.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::ManualWarmth,
        |settings, value| {
            settings.enabled = true;
            settings.automatic = false;
            settings.manual.warmth = value;
        },
    );
    bind_scale(
        widgets.manual_brightness.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::ManualBrightness,
        |settings, value| {
            settings.enabled = true;
            settings.automatic = false;
            settings.manual.brightness = value.max(MIN_BRIGHTNESS);
        },
    );
    bind_scale(
        widgets.day_warmth.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::DayWarmth,
        |settings, value| settings.schedule.day.warmth = value,
    );
    bind_scale(
        widgets.day_brightness.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::DayBrightness,
        |settings, value| {
            settings.schedule.day.brightness = value.max(MIN_BRIGHTNESS);
        },
    );
    bind_scale(
        widgets.night_warmth.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::NightWarmth,
        |settings, value| settings.schedule.night.warmth = value,
    );
    bind_scale(
        widgets.night_brightness.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::NightBrightness,
        |settings, value| {
            settings.schedule.night.brightness = value.max(MIN_BRIGHTNESS);
        },
    );

    bind_time_entry(
        widgets.night_start.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::NightStart,
        |settings, value| settings.schedule.night_start = value,
    );
    bind_time_entry(
        widgets.day_start.clone(),
        gui.clone(),
        widgets.clone(),
        updating.clone(),
        Field::DayStart,
        |settings, value| settings.schedule.day_start = value,
    );

    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let spin = widgets.latitude.clone();
        spin.connect_value_changed(move |spin| {
            if updating.get() {
                return;
            }
            let value = spin.value();
            apply_edit(&gui, &widgets, &updating, |settings| {
                settings.schedule.latitude = value;
            });
            gui.borrow_mut().selected = Field::Latitude;
            update_help(&gui.borrow(), &widgets);
        });
    }
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let spin = widgets.longitude.clone();
        spin.connect_value_changed(move |spin| {
            if updating.get() {
                return;
            }
            let value = spin.value();
            apply_edit(&gui, &widgets, &updating, |settings| {
                settings.schedule.longitude = value;
            });
            gui.borrow_mut().selected = Field::Longitude;
            update_help(&gui.borrow(), &widgets);
        });
    }
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let spin = widgets.transition.clone();
        spin.connect_value_changed(move |spin| {
            if updating.get() {
                return;
            }
            let value = spin.value() as u16;
            apply_edit(&gui, &widgets, &updating, |settings| {
                settings.schedule.transition_minutes = value.min(MAX_TRANSITION_MINUTES);
            });
            gui.borrow_mut().selected = Field::Transition;
            update_help(&gui.borrow(), &widgets);
        });
    }

    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let button = widgets.preset_apply.clone();
        button.connect_clicked(move |_| {
            let name = selected_preset_name(&gui.borrow(), &widgets);
            let Some(name) = name else {
                gui.borrow_mut().notice =
                    Some(Notice::error("No presets yet — save one first".into()));
                refresh_from(&gui, &widgets, &updating);
                return;
            };
            apply_edit(&gui, &widgets, &updating, |settings| {
                let _ = settings.apply_preset(&name);
            });
            gui.borrow_mut().notice = Some(Notice::info(format!("Applied preset {name:?}")));
            gui.borrow_mut().selected = Field::Preset;
            update_help(&gui.borrow(), &widgets);
            refresh_from(&gui, &widgets, &updating);
        });
    }
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let button = widgets.preset_save.clone();
        button.connect_clicked(move |_| {
            prompt_save_preset(gui.clone(), widgets.clone(), updating.clone());
        });
    }
    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let button = widgets.preset_delete.clone();
        button.connect_clicked(move |_| {
            let name = selected_preset_name(&gui.borrow(), &widgets);
            let Some(name) = name else {
                gui.borrow_mut().notice = Some(Notice::error("No preset selected".into()));
                refresh_from(&gui, &widgets, &updating);
                return;
            };
            let dialog = adw::AlertDialog::builder()
                .heading("Delete preset")
                .body(format!("Delete preset {name:?}?"))
                .build();
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("delete", "Delete");
            dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let gui = gui.clone();
            let widgets = widgets.clone();
            let updating = updating.clone();
            dialog.connect_response(None, move |_, response| {
                if response != "delete" {
                    return;
                }
                apply_edit(&gui, &widgets, &updating, |settings| {
                    let _ = settings.delete_preset(&name);
                });
                gui.borrow_mut().notice = Some(Notice::info(format!("Preset {name:?} deleted")));
                refresh_from(&gui, &widgets, &updating);
            });
            dialog.present(None::<&gtk::Widget>);
        });
    }

    {
        let gui = gui.clone();
        let widgets = widgets.clone();
        let updating = updating.clone();
        let dropdown = widgets.preset_dropdown.clone();
        dropdown.connect_selected_notify(move |_| {
            if updating.get() {
                return;
            }
            gui.borrow_mut().selected = Field::Preset;
            update_help(&gui.borrow(), &widgets);
        });
    }
}

fn bind_scale(
    scale: gtk::Scale,
    gui: Rc<RefCell<GuiState>>,
    widgets: Rc<Widgets>,
    updating: Rc<Cell<bool>>,
    field: Field,
    mutate: impl Fn(&mut Settings, u8) + 'static,
) {
    scale.connect_value_changed(move |scale| {
        if updating.get() {
            return;
        }
        let value = scale.value().round() as u8;
        apply_edit(&gui, &widgets, &updating, |settings| {
            mutate(settings, value)
        });
        gui.borrow_mut().selected = field;
        update_help(&gui.borrow(), &widgets);
    });
}

fn bind_time_entry(
    entry: gtk::Entry,
    gui: Rc<RefCell<GuiState>>,
    widgets: Rc<Widgets>,
    updating: Rc<Cell<bool>>,
    field: Field,
    mutate: impl Fn(&mut Settings, String) + 'static,
) {
    entry.connect_activate(move |entry| {
        if updating.get() {
            return;
        }
        let text = entry.text().to_string();
        if parse_time(&text).is_err() {
            gui.borrow_mut().notice =
                Some(Notice::error(format!("Invalid time {text:?}; use HH:MM")));
            refresh_from(&gui, &widgets, &updating);
            return;
        }
        apply_edit(&gui, &widgets, &updating, |settings| mutate(settings, text));
        gui.borrow_mut().selected = field;
        update_help(&gui.borrow(), &widgets);
    });
}

fn selected_preset_name(gui: &GuiState, widgets: &Widgets) -> Option<String> {
    let names: Vec<_> = gui.state.settings.presets.keys().cloned().collect();
    if names.is_empty() {
        return None;
    }
    let index = widgets.preset_dropdown.selected() as usize;
    names.get(index).cloned()
}

fn prompt_save_preset(gui: Rc<RefCell<GuiState>>, widgets: Rc<Widgets>, updating: Rc<Cell<bool>>) {
    let entry = gtk::Entry::builder()
        .placeholder_text("preset-name")
        .max_length(32)
        .build();
    if let Some(current) = selected_preset_name(&gui.borrow(), &widgets) {
        entry.set_text(&current);
    }

    let dialog = adw::AlertDialog::builder()
        .heading("Save preset")
        .body("Letters, digits, - and _ only.")
        .extra_child(&entry)
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("save", "Save");
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");

    dialog.connect_response(None, move |_, response| {
        if response != "save" {
            return;
        }
        let name = entry.text().trim().to_owned();
        if name.is_empty() {
            gui.borrow_mut().notice = Some(Notice::error("Preset name must not be empty".into()));
            refresh_from(&gui, &widgets, &updating);
            return;
        }
        if !name.chars().all(is_preset_name_char) {
            gui.borrow_mut().notice = Some(Notice::error(
                "Preset names may use letters, digits, - and _".into(),
            ));
            refresh_from(&gui, &widgets, &updating);
            return;
        }
        let name_for_notice = name.clone();
        apply_edit(&gui, &widgets, &updating, |settings| {
            let _ = settings.save_preset(name);
        });
        gui.borrow_mut().notice = Some(Notice::info(format!("Preset {name_for_notice:?} saved")));
        refresh_from(&gui, &widgets, &updating);
    });
    dialog.present(None::<&gtk::Widget>);
}

fn apply_edit(
    gui: &Rc<RefCell<GuiState>>,
    widgets: &Rc<Widgets>,
    updating: &Rc<Cell<bool>>,
    change: impl FnOnce(&mut Settings),
) {
    let mut settings = gui.borrow().state.settings.clone();
    change(&mut settings);
    if settings == gui.borrow().state.settings {
        return;
    }
    match replace_settings(settings) {
        Ok(state) => {
            let mut g = gui.borrow_mut();
            g.state = state;
            g.backend_available = true;
            if g.notice
                .as_ref()
                .is_none_or(|notice| notice.expires_at.is_some())
            {
                g.notice = Some(Notice::saved());
            }
            drop(g);
            refresh_from(gui, widgets, updating);
        }
        Err(error) => {
            gui.borrow_mut().notice = Some(Notice::error(format!("Settings not saved: {error:#}")));
            refresh_from(gui, widgets, updating);
        }
    }
}

fn update_help(gui: &GuiState, widgets: &Widgets) {
    let (name, help) = field_help(gui.selected);
    widgets
        .help_label
        .set_markup(&format!("<b>{name}.</b> {help}"));
}

fn refresh_from(gui: &Rc<RefCell<GuiState>>, widgets: &Widgets, updating: &Cell<bool>) {
    // Snapshot first so widget signals cannot re-enter the RefCell mid-refresh.
    let snapshot = gui.borrow().clone();
    updating.set(true);
    refresh_ui(&snapshot, widgets);
    updating.set(false);
}

fn refresh_ui(gui: &GuiState, widgets: &Widgets) {
    let settings = &gui.state.settings;
    let status = backend_status(gui.transient, gui.backend_available);
    widgets
        .daemon_label
        .set_label(&format!("Daemon · {status}"));
    let outputs = if gui.state.outputs.is_empty() {
        "No displays detected".to_owned()
    } else {
        gui.state.outputs.join("  ·  ")
    };
    widgets
        .outputs_label
        .set_label(&format!("Outputs · {outputs}"));

    let warmth = gui.state.active_warmth;
    widgets
        .warmth_value
        .set_label(&format!("{warmth}%  ·  {} K", warmth_to_kelvin(warmth)));
    widgets.warmth_bar.set_value(f64::from(warmth));

    let brightness = gui.state.active_brightness;
    widgets
        .brightness_value
        .set_label(&format!("{brightness}%"));
    widgets.brightness_bar.set_value(f64::from(brightness));

    let schedule_active = settings.enabled && settings.automatic;
    let timeline = TimelineView::from_schedule(&settings.schedule, Local::now()).ok();
    match &timeline {
        Some(view) if schedule_active => {
            widgets.timeline_title.set_label(&format!(
                "Today · day {} · night {}",
                format_hhmm(view.day_start),
                format_hhmm(view.night_start)
            ));
            widgets.timeline_legend.set_label(&format!(
                "Day begins {}   Night begins {}   Fade {} min   Now {}",
                format_hhmm(view.day_start),
                format_hhmm(view.night_start),
                view.transition_minutes,
                format_hhmm(view.now_minute),
            ));
        }
        Some(_) => {
            widgets.timeline_title.set_label("Today · inactive");
            widgets.timeline_legend.set_label(
                "Schedule coloring is inactive while the filter is off or in manual mode.",
            );
        }
        None => {
            widgets.timeline_title.set_label("Today");
            widgets
                .timeline_legend
                .set_label("Schedule times are invalid");
        }
    }
    *widgets.timeline_view.borrow_mut() = timeline;
    widgets.timeline_active.set(schedule_active);
    widgets.timeline_area.queue_draw();

    widgets.mode_switch.set_state(settings.automatic);
    widgets.mode_switch.set_active(settings.automatic);
    widgets.filter_switch.set_state(settings.enabled);
    widgets.filter_switch.set_active(settings.enabled);

    let names: Vec<String> = settings.presets.keys().cloned().collect();
    let model: Vec<&str> = if names.is_empty() {
        vec!["(none)"]
    } else {
        names.iter().map(String::as_str).collect()
    };
    let selected = widgets.preset_dropdown.selected();
    widgets
        .preset_dropdown
        .set_model(Some(&gtk::StringList::new(&model)));
    if !names.is_empty() {
        widgets
            .preset_dropdown
            .set_selected(selected.min((names.len() - 1) as u32));
    }
    let has_presets = !names.is_empty();
    widgets.preset_apply.set_sensitive(has_presets);
    widgets.preset_delete.set_sensitive(has_presets);

    set_scale(&widgets.manual_warmth, settings.manual.warmth);
    set_scale(&widgets.manual_brightness, settings.manual.brightness);
    widgets
        .timing_dropdown
        .set_selected(match settings.schedule.timing {
            ScheduleTiming::Fixed => 0,
            ScheduleTiming::Location => 1,
        });
    set_scale(&widgets.day_warmth, settings.schedule.day.warmth);
    set_scale(&widgets.day_brightness, settings.schedule.day.brightness);
    set_scale(&widgets.night_warmth, settings.schedule.night.warmth);
    set_scale(
        &widgets.night_brightness,
        settings.schedule.night.brightness,
    );
    widgets.night_start.set_text(&settings.schedule.night_start);
    widgets.day_start.set_text(&settings.schedule.day_start);
    widgets.latitude.set_value(settings.schedule.latitude);
    widgets.longitude.set_value(settings.schedule.longitude);
    widgets
        .transition
        .set_value(f64::from(settings.schedule.transition_minutes));

    let manual_active = settings.enabled && !settings.automatic;
    let schedule_active = settings.enabled && settings.automatic;
    let location_active = schedule_active && settings.schedule.timing == ScheduleTiming::Location;
    let fixed_times_active = schedule_active && settings.schedule.timing == ScheduleTiming::Fixed;

    widgets.manual_group.set_sensitive(manual_active);
    widgets.schedule_group.set_sensitive(schedule_active);
    widgets
        .night_start_row
        .set_sensitive(fixed_times_active || schedule_active);
    widgets
        .day_start_row
        .set_sensitive(fixed_times_active || schedule_active);
    widgets.night_start.set_sensitive(fixed_times_active);
    widgets.day_start.set_sensitive(fixed_times_active);
    widgets.latitude_row.set_sensitive(location_active);
    widgets.longitude_row.set_sensitive(location_active);
    widgets.latitude.set_sensitive(location_active);
    widgets.longitude.set_sensitive(location_active);

    update_help(gui, widgets);

    let (text, error) = gui.notice_text();
    widgets.notice_label.set_label(&text);
    if error {
        widgets.notice_label.add_css_class("error");
        widgets.notice_label.set_attributes(Some(&{
            let attrs = pango::AttrList::new();
            attrs.insert(pango::AttrInt::new_weight(Weight::Bold));
            attrs
        }));
    } else {
        widgets.notice_label.remove_css_class("error");
        widgets.notice_label.set_attributes(None);
    }
}

fn set_scale(scale: &gtk::Scale, value: u8) {
    let adjustment = scale.adjustment();
    if (adjustment.value() - f64::from(value)).abs() > 0.01 {
        scale.set_value(f64::from(value));
    }
}
