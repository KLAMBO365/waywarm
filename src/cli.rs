use anyhow::{Result, bail};

use crate::{
    config::{
        Levels, MAX_TRANSITION_MINUTES, MIN_BRIGHTNESS, ScheduleTiming, Settings, parse_time,
    },
    ipc::{query_state, replace_settings},
    protocol::RuntimeState,
};

/// Run a CLI subcommand with the remaining argv tokens (not including the subcommand name).
pub fn run(command: &str, args: impl IntoIterator<Item = String>) -> Result<()> {
    let args: Vec<String> = args.into_iter().collect();
    match command {
        "status" => status(&args),
        "set" => set(&args),
        "enable" => set_enabled(true, &args),
        "disable" => set_enabled(false, &args),
        "toggle" => toggle(&args),
        _ => bail!("unknown CLI command {command:?}; use --help"),
    }
}

fn status(args: &[String]) -> Result<()> {
    match parse_flag_only(args, "status")? {
        FlagOnly::Help(help) => {
            println!("{help}");
            return Ok(());
        }
        FlagOnly::Json(json) => {
            let state = query_state().map_err(daemon_hint)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                print!("{}", format_status_human(&state));
            }
        }
    }
    Ok(())
}

fn set(args: &[String]) -> Result<()> {
    let (options, json) = match parse_set_args(args)? {
        SetParse::Help(help) => {
            println!("{help}");
            return Ok(());
        }
        SetParse::Options { options, json } => (options, json),
    };
    if options.is_empty() {
        bail!("nothing to change; pass at least one set option (see --help)");
    }
    let mut state = query_state().map_err(daemon_hint)?;
    apply_set_options(&mut state.settings, &options)?;
    state.settings.validate()?;
    let state = replace_settings(state.settings).map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    }
    Ok(())
}

fn set_enabled(enabled: bool, args: &[String]) -> Result<()> {
    let command = if enabled { "enable" } else { "disable" };
    let json = match parse_flag_only(args, command)? {
        FlagOnly::Help(help) => {
            println!("{help}");
            return Ok(());
        }
        FlagOnly::Json(json) => json,
    };
    let mut state = query_state().map_err(daemon_hint)?;
    if state.settings.enabled == enabled {
        if json {
            println!("{}", serde_json::to_string_pretty(&state)?);
        }
        return Ok(());
    }
    state.settings.enabled = enabled;
    let state = replace_settings(state.settings).map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    }
    Ok(())
}

fn toggle(args: &[String]) -> Result<()> {
    let json = match parse_flag_only(args, "toggle")? {
        FlagOnly::Help(help) => {
            println!("{help}");
            return Ok(());
        }
        FlagOnly::Json(json) => json,
    };
    let mut state = query_state().map_err(daemon_hint)?;
    state.settings.enabled = !state.settings.enabled;
    let state = replace_settings(state.settings).map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    }
    Ok(())
}

fn daemon_hint(error: anyhow::Error) -> anyhow::Error {
    let message = format!("{error:#}");
    if message.contains("outdated") || message.contains("unsupported IPC") {
        return anyhow::anyhow!("{message}");
    }
    anyhow::anyhow!(
        "{message}\nstart the background service with `waywarm daemon`, or open the settings UI first"
    )
}

enum FlagOnly {
    Json(bool),
    Help(String),
}

fn parse_flag_only(args: &[String], command: &str) -> Result<FlagOnly> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => return Ok(FlagOnly::Help(command_help(command))),
            other => bail!("unexpected argument {other:?} for `{command}`"),
        }
    }
    Ok(FlagOnly::Json(json))
}

#[derive(Debug, Default, Clone, PartialEq)]
struct SetOptions {
    enabled: Option<bool>,
    automatic: Option<bool>,
    warmth: Option<u8>,
    brightness: Option<u8>,
    day_warmth: Option<u8>,
    day_brightness: Option<u8>,
    night_warmth: Option<u8>,
    night_brightness: Option<u8>,
    night_start: Option<String>,
    day_start: Option<String>,
    transition: Option<u16>,
    timing: Option<ScheduleTiming>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

impl SetOptions {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

enum SetParse {
    Help(String),
    Options { options: SetOptions, json: bool },
}

fn parse_set_args(args: &[String]) -> Result<SetParse> {
    let mut options = SetOptions::default();
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" | "-h" => return Ok(SetParse::Help(command_help("set"))),
            "--json" => json = true,
            "--on" => options.enabled = Some(true),
            "--off" => options.enabled = Some(false),
            "--mode" => {
                let value = next_value(args, &mut index, "--mode")?;
                options.automatic = Some(parse_mode(&value)?);
            }
            "--warmth" => {
                let value = next_value(args, &mut index, "--warmth")?;
                options.warmth = Some(parse_percent(&value, 0, "warmth")?);
            }
            "--brightness" => {
                let value = next_value(args, &mut index, "--brightness")?;
                options.brightness = Some(parse_percent(&value, MIN_BRIGHTNESS, "brightness")?);
            }
            "--day-warmth" => {
                let value = next_value(args, &mut index, "--day-warmth")?;
                options.day_warmth = Some(parse_percent(&value, 0, "day-warmth")?);
            }
            "--day-brightness" => {
                let value = next_value(args, &mut index, "--day-brightness")?;
                options.day_brightness =
                    Some(parse_percent(&value, MIN_BRIGHTNESS, "day-brightness")?);
            }
            "--night-warmth" => {
                let value = next_value(args, &mut index, "--night-warmth")?;
                options.night_warmth = Some(parse_percent(&value, 0, "night-warmth")?);
            }
            "--night-brightness" => {
                let value = next_value(args, &mut index, "--night-brightness")?;
                options.night_brightness =
                    Some(parse_percent(&value, MIN_BRIGHTNESS, "night-brightness")?);
            }
            "--night-start" => {
                let value = next_value(args, &mut index, "--night-start")?;
                parse_time(&value)?;
                options.night_start = Some(value);
            }
            "--day-start" => {
                let value = next_value(args, &mut index, "--day-start")?;
                parse_time(&value)?;
                options.day_start = Some(value);
            }
            "--transition" => {
                let value = next_value(args, &mut index, "--transition")?;
                options.transition = Some(parse_transition(&value)?);
            }
            "--timing" => {
                let value = next_value(args, &mut index, "--timing")?;
                options.timing = Some(parse_timing(&value)?);
            }
            "--latitude" | "--lat" => {
                let value = next_value(args, &mut index, "--latitude")?;
                options.latitude = Some(parse_coordinate(&value, "latitude", -90.0, 90.0)?);
            }
            "--longitude" | "--lon" => {
                let value = next_value(args, &mut index, "--longitude")?;
                options.longitude = Some(parse_coordinate(&value, "longitude", -180.0, 180.0)?);
            }
            other if other.starts_with('-') => {
                bail!("unknown option {other:?}; use `waywarm set --help`")
            }
            other => bail!("unexpected argument {other:?}; use `waywarm set --help`"),
        }
        index += 1;
    }
    Ok(SetParse::Options { options, json })
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"))
}

fn parse_mode(value: &str) -> Result<bool> {
    match value {
        "automatic" | "auto" => Ok(true),
        "manual" => Ok(false),
        _ => bail!("invalid mode {value:?}; expected automatic, auto, or manual"),
    }
}

fn parse_percent(value: &str, minimum: u8, name: &str) -> Result<u8> {
    let parsed: u8 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid {name} {value:?}; expected an integer"))?;
    if parsed < minimum || parsed > 100 {
        bail!("{name} must be between {minimum} and 100");
    }
    Ok(parsed)
}

fn parse_transition(value: &str) -> Result<u16> {
    let parsed: u16 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid transition {value:?}; expected an integer"))?;
    if parsed > MAX_TRANSITION_MINUTES {
        bail!("transition must be between 0 and {MAX_TRANSITION_MINUTES} minutes");
    }
    Ok(parsed)
}

fn parse_timing(value: &str) -> Result<ScheduleTiming> {
    match value {
        "fixed" | "clock" => Ok(ScheduleTiming::Fixed),
        "location" | "sun" => Ok(ScheduleTiming::Location),
        _ => bail!("invalid timing {value:?}; expected fixed or location"),
    }
}

fn parse_coordinate(value: &str, name: &str, min: f64, max: f64) -> Result<f64> {
    let parsed: f64 = value
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid {name} {value:?}; expected a number"))?;
    if !(min..=max).contains(&parsed) {
        bail!("{name} must be between {min} and {max}");
    }
    Ok(parsed)
}

fn apply_set_options(settings: &mut Settings, options: &SetOptions) -> Result<()> {
    if let Some(enabled) = options.enabled {
        settings.enabled = enabled;
    }
    if let Some(automatic) = options.automatic {
        settings.automatic = automatic;
    }
    if options.warmth.is_some() || options.brightness.is_some() {
        settings.enabled = true;
        settings.automatic = false;
    }
    if let Some(warmth) = options.warmth {
        settings.manual.warmth = warmth;
    }
    if let Some(brightness) = options.brightness {
        settings.manual.brightness = brightness;
    }
    if let Some(warmth) = options.day_warmth {
        settings.schedule.day.warmth = warmth;
    }
    if let Some(brightness) = options.day_brightness {
        settings.schedule.day.brightness = brightness;
    }
    if let Some(warmth) = options.night_warmth {
        settings.schedule.night.warmth = warmth;
    }
    if let Some(brightness) = options.night_brightness {
        settings.schedule.night.brightness = brightness;
    }
    if let Some(night_start) = &options.night_start {
        settings.schedule.night_start = night_start.clone();
    }
    if let Some(day_start) = &options.day_start {
        settings.schedule.day_start = day_start.clone();
    }
    if let Some(transition) = options.transition {
        settings.schedule.transition_minutes = transition;
    }
    if let Some(timing) = options.timing {
        settings.schedule.timing = timing;
    }
    if let Some(latitude) = options.latitude {
        settings.schedule.latitude = latitude;
    }
    if let Some(longitude) = options.longitude {
        settings.schedule.longitude = longitude;
    }

    // Mode flags after warmth/brightness still win so scripts can set manual levels then switch mode.
    if let Some(automatic) = options.automatic {
        settings.automatic = automatic;
    }
    if let Some(enabled) = options.enabled {
        settings.enabled = enabled;
    }

    settings.manual.validate()?;
    settings.schedule.day.validate()?;
    settings.schedule.night.validate()?;
    if options.night_start.is_some()
        || options.day_start.is_some()
        || options.transition.is_some()
        || options.timing.is_some()
        || options.latitude.is_some()
        || options.longitude.is_some()
    {
        settings.schedule.validate()?;
    }
    Ok(())
}

fn format_status_human(state: &RuntimeState) -> String {
    let settings = &state.settings;
    let filter = if settings.enabled { "on" } else { "off" };
    let mode = if settings.automatic {
        "automatic"
    } else {
        "manual"
    };
    let timing = match settings.schedule.timing {
        ScheduleTiming::Fixed => format!(
            "fixed · night {} · day {}",
            settings.schedule.night_start, settings.schedule.day_start
        ),
        ScheduleTiming::Location => format!(
            "location · {:.4}°, {:.4}° (fallback night {} · day {})",
            settings.schedule.latitude,
            settings.schedule.longitude,
            settings.schedule.night_start,
            settings.schedule.day_start
        ),
    };
    let outputs = if state.outputs.is_empty() {
        "(none)".into()
    } else {
        state.outputs.join(", ")
    };
    format!(
        "Filter:     {filter}\n\
         Mode:       {mode}\n\
         Active:     {}\n\
         Manual:     {}\n\
         Day:        {}\n\
         Night:      {}\n\
         Schedule:   {timing} · fade {}m\n\
         Backend:    {}\n\
         Outputs:    {outputs}\n",
        format_levels_line(state.active_warmth, state.active_brightness),
        format_levels(settings.manual),
        format_levels(settings.schedule.day),
        format_levels(settings.schedule.night),
        settings.schedule.transition_minutes,
        state.backend,
    )
}

fn format_levels(levels: Levels) -> String {
    format_levels_line(levels.warmth, levels.brightness)
}

fn format_levels_line(warmth: u8, brightness: u8) -> String {
    format!("warmth {warmth}% · brightness {brightness}%")
}

fn command_help(command: &str) -> String {
    match command {
        "status" => "Usage: waywarm status [--json]".into(),
        "enable" => "Usage: waywarm enable [--json]".into(),
        "disable" => "Usage: waywarm disable [--json]".into(),
        "toggle" => "Usage: waywarm toggle [--json]".into(),
        "set" => "Usage: waywarm set [options]\n\n\
Options:\n\
  --on | --off                 Enable or disable the filter\n\
  --mode automatic|manual      Schedule mode (aliases: auto)\n\
  --warmth <0-100>             Manual warmth (implies enabled + manual)\n\
  --brightness <10-100>        Manual brightness (implies enabled + manual)\n\
  --day-warmth <0-100>         Daytime schedule warmth\n\
  --day-brightness <10-100>    Daytime schedule brightness\n\
  --night-warmth <0-100>       Night schedule warmth\n\
  --night-brightness <10-100>  Night schedule brightness\n\
  --night-start <HH:MM>        Evening transition start (fixed / fallback)\n\
  --day-start <HH:MM>          Morning transition start (fixed / fallback)\n\
  --transition <0-240>         Fade duration in minutes\n\
  --timing fixed|location      Clock times or civil dawn/dusk (aliases: clock, sun)\n\
  --latitude <deg>             Latitude for location timing (alias: --lat)\n\
  --longitude <deg>            Longitude for location timing (alias: --lon)\n\
  --json                       Print resulting state as JSON\n\
\n\
Requires a running Waywarm daemon (service or open settings UI)."
            .into(),
        _ => "use waywarm --help".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Schedule, ScheduleTiming};

    #[test]
    fn parse_set_args_accepts_common_flags() {
        let args = vec![
            "--warmth".into(),
            "40".into(),
            "--brightness".into(),
            "80".into(),
            "--mode".into(),
            "auto".into(),
            "--night-start".into(),
            "22:30".into(),
            "--json".into(),
        ];
        let SetParse::Options { options, json } = parse_set_args(&args).unwrap() else {
            panic!("expected options");
        };
        assert!(json);
        assert_eq!(options.warmth, Some(40));
        assert_eq!(options.brightness, Some(80));
        assert_eq!(options.automatic, Some(true));
        assert_eq!(options.night_start.as_deref(), Some("22:30"));
    }

    #[test]
    fn parse_set_args_rejects_bad_values() {
        assert!(parse_set_args(&["--warmth".into(), "200".into()]).is_err());
        assert!(parse_set_args(&["--brightness".into(), "5".into()]).is_err());
        assert!(parse_set_args(&["--mode".into(), "schedule".into()]).is_err());
        assert!(parse_set_args(&["--night-start".into(), "9:00".into()]).is_err());
        assert!(parse_set_args(&["--transition".into(), "999".into()]).is_err());
        assert!(parse_set_args(&["--unknown".into()]).is_err());
        assert!(matches!(
            parse_set_args(&["--help".into()]).unwrap(),
            SetParse::Help(_)
        ));
    }

    #[test]
    fn warmth_implies_enabled_manual_mode() {
        let mut settings = Settings {
            enabled: false,
            automatic: true,
            ..Settings::default()
        };
        let options = SetOptions {
            warmth: Some(40),
            ..SetOptions::default()
        };
        apply_set_options(&mut settings, &options).unwrap();
        assert!(settings.enabled);
        assert!(!settings.automatic);
        assert_eq!(settings.manual.warmth, 40);
    }

    #[test]
    fn enable_preserves_mode() {
        let mut settings = Settings {
            enabled: false,
            automatic: true,
            ..Settings::default()
        };
        let options = SetOptions {
            enabled: Some(true),
            ..SetOptions::default()
        };
        apply_set_options(&mut settings, &options).unwrap();
        assert!(settings.enabled);
        assert!(settings.automatic);
    }

    #[test]
    fn explicit_mode_wins_after_warmth() {
        let mut settings = Settings::default();
        let options = SetOptions {
            warmth: Some(30),
            automatic: Some(true),
            ..SetOptions::default()
        };
        apply_set_options(&mut settings, &options).unwrap();
        assert!(settings.enabled);
        assert!(settings.automatic);
        assert_eq!(settings.manual.warmth, 30);
    }

    #[test]
    fn schedule_fields_update() {
        let mut settings = Settings::default();
        let options = SetOptions {
            day_warmth: Some(15),
            day_brightness: Some(100),
            night_warmth: Some(70),
            night_brightness: Some(85),
            night_start: Some("22:00".into()),
            day_start: Some("06:30".into()),
            transition: Some(45),
            timing: Some(ScheduleTiming::Location),
            latitude: Some(48.8566),
            longitude: Some(2.3522),
            ..SetOptions::default()
        };
        apply_set_options(&mut settings, &options).unwrap();
        assert_eq!(
            settings.schedule,
            Schedule {
                night_start: "22:00".into(),
                day_start: "06:30".into(),
                transition_minutes: 45,
                day: Levels {
                    warmth: 15,
                    brightness: 100,
                },
                night: Levels {
                    warmth: 70,
                    brightness: 85,
                },
                timing: ScheduleTiming::Location,
                latitude: 48.8566,
                longitude: 2.3522,
            }
        );
    }

    #[test]
    fn format_status_includes_key_labels() {
        let state = RuntimeState {
            settings: Settings::default(),
            outputs: vec!["eDP-1".into(), "HDMI-A-1".into()],
            backend: "wlr-gamma-control-v1".into(),
            active_warmth: 25,
            active_brightness: 95,
        };
        let text = format_status_human(&state);
        assert!(text.contains("Filter:"));
        assert!(text.contains("Mode:"));
        assert!(text.contains("Active:"));
        assert!(text.contains("Day:"));
        assert!(text.contains("Night:"));
        assert!(text.contains("warmth 25%"));
        assert!(text.contains("eDP-1, HDMI-A-1"));
        assert!(text.contains("wlr-gamma-control-v1"));
    }

    #[test]
    fn empty_set_options_detected() {
        assert!(SetOptions::default().is_empty());
        assert!(
            !SetOptions {
                warmth: Some(1),
                ..SetOptions::default()
            }
            .is_empty()
        );
    }
}
