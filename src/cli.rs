use anyhow::{Context, Result, bail};

use crate::{
    config::{
        ConfigStore, Levels, MAX_TRANSITION_MINUTES, MIN_BRIGHTNESS, Preset, ScheduleTiming,
        Settings, parse_time,
    },
    daemon,
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
        "preset" => preset(&args),
        "export" => export(&args),
        "apply" => apply(&args),
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
    let (options, json) = match parse_set_args(args, "set")? {
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

fn preset(args: &[String]) -> Result<()> {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        println!("{}", command_help("preset"));
        return Ok(());
    }
    let action = args[0].as_str();
    let rest = &args[1..];
    match action {
        "list" => preset_list(rest),
        "save" => preset_save(rest),
        "apply" => preset_apply(rest),
        "delete" | "rm" => preset_delete(rest),
        other => bail!("unknown preset action {other:?}; use `waywarm preset --help`"),
    }
}

fn preset_list(args: &[String]) -> Result<()> {
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => {
                println!("{}", command_help("preset"));
                return Ok(());
            }
            other => bail!("unexpected argument {other:?} for `preset list`"),
        }
    }
    let state = query_state().map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state.settings.presets)?);
        return Ok(());
    }
    if state.settings.presets.is_empty() {
        println!("(no presets)");
        return Ok(());
    }
    for name in state.settings.presets.keys() {
        println!("{name}");
    }
    Ok(())
}

fn preset_save(args: &[String]) -> Result<()> {
    let Some((name, json)) = parse_preset_name_args(args)? else {
        println!("{}", command_help("preset"));
        return Ok(());
    };
    let mut state = query_state().map_err(daemon_hint)?;
    state.settings.save_preset(name)?;
    let state = replace_settings(state.settings).map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    }
    Ok(())
}

fn preset_apply(args: &[String]) -> Result<()> {
    let Some((name, json)) = parse_preset_name_args(args)? else {
        println!("{}", command_help("preset"));
        return Ok(());
    };
    let mut state = query_state().map_err(daemon_hint)?;
    state.settings.apply_preset(&name)?;
    let state = replace_settings(state.settings).map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    }
    Ok(())
}

fn preset_delete(args: &[String]) -> Result<()> {
    let Some((name, json)) = parse_preset_name_args(args)? else {
        println!("{}", command_help("preset"));
        return Ok(());
    };
    let mut state = query_state().map_err(daemon_hint)?;
    state.settings.delete_preset(&name)?;
    let state = replace_settings(state.settings).map_err(daemon_hint)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&state)?);
    }
    Ok(())
}

fn export(args: &[String]) -> Result<()> {
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        println!("{}", command_help("export"));
        return Ok(());
    }
    let kind = args[0].as_str();
    let rest = &args[1..];
    match kind {
        "preset" => export_preset(rest),
        other => bail!("unknown export kind {other:?}; use `waywarm export --help`"),
    }
}

fn export_preset(args: &[String]) -> Result<()> {
    let mut name = None;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{}", command_help("export"));
                return Ok(());
            }
            other if other.starts_with('-') => {
                bail!("unexpected argument {other:?}; use `waywarm export --help`")
            }
            other if name.is_none() => name = Some(other.to_owned()),
            other => bail!("unexpected argument {other:?}; use `waywarm export --help`"),
        }
    }
    let name =
        name.ok_or_else(|| anyhow::anyhow!("missing preset name; see `waywarm export --help`"))?;
    let store = ConfigStore::discover()?;
    let settings = store.load().with_context(|| {
        format!(
            "no Waywarm config at {}; save a preset from the settings UI first",
            store.path().display()
        )
    })?;
    let preset = settings
        .presets
        .get(&name)
        .with_context(|| format!("unknown preset {name:?}"))?;
    println!("{}", format_apply_command(preset));
    Ok(())
}

fn apply(args: &[String]) -> Result<()> {
    let options = match parse_set_args(args, "apply")? {
        SetParse::Help(help) => {
            println!("{help}");
            return Ok(());
        }
        SetParse::Options { options, json } => {
            if json {
                bail!(
                    "`--json` is not supported for `apply`; omit it or use `set` against a daemon"
                );
            }
            options
        }
    };
    if options.is_empty() {
        bail!("nothing to apply; pass at least one option (see `waywarm apply --help`)");
    }
    if options.enabled == Some(false) {
        bail!("`apply` keeps the filter on; omit `--off`");
    }
    let mut settings = Settings::default();
    apply_set_options(&mut settings, &options)?;
    settings.enabled = true;
    settings.validate()?;
    daemon::run_session(settings)
}

/// Format a preset as a daemon-free `waywarm apply …` command line.
fn format_apply_command(preset: &Preset) -> String {
    let mut parts = vec!["waywarm".into(), "apply".into()];
    if preset.automatic {
        parts.push("--mode".into());
        parts.push("automatic".into());
        parts.push("--day-warmth".into());
        parts.push(preset.schedule.day.warmth.to_string());
        parts.push("--day-brightness".into());
        parts.push(preset.schedule.day.brightness.to_string());
        parts.push("--night-warmth".into());
        parts.push(preset.schedule.night.warmth.to_string());
        parts.push("--night-brightness".into());
        parts.push(preset.schedule.night.brightness.to_string());
        parts.push("--night-start".into());
        parts.push(preset.schedule.night_start.clone());
        parts.push("--day-start".into());
        parts.push(preset.schedule.day_start.clone());
        parts.push("--transition".into());
        parts.push(preset.schedule.transition_minutes.to_string());
        parts.push("--timing".into());
        match preset.schedule.timing {
            ScheduleTiming::Fixed => parts.push("fixed".into()),
            ScheduleTiming::Location => {
                parts.push("location".into());
                parts.push("--latitude".into());
                parts.push(format_coordinate(preset.schedule.latitude));
                parts.push("--longitude".into());
                parts.push(format_coordinate(preset.schedule.longitude));
            }
        }
    } else {
        parts.push("--mode".into());
        parts.push("manual".into());
        parts.push("--warmth".into());
        parts.push(preset.manual.warmth.to_string());
        parts.push("--brightness".into());
        parts.push(preset.manual.brightness.to_string());
    }
    parts.join(" ")
}

fn format_coordinate(value: f64) -> String {
    let text = format!("{value}");
    if text.contains('.') || text.contains('e') || text.contains('E') {
        text
    } else {
        format!("{value:.1}")
    }
}

/// Returns `None` when help was requested.
fn parse_preset_name_args(args: &[String]) -> Result<Option<(String, bool)>> {
    let mut name = None;
    let mut json = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--help" | "-h" => return Ok(None),
            other if other.starts_with('-') => {
                bail!("unexpected argument {other:?}")
            }
            other if name.is_none() => name = Some(other.to_owned()),
            other => bail!("unexpected argument {other:?}"),
        }
    }
    let name =
        name.ok_or_else(|| anyhow::anyhow!("missing preset name; see `waywarm preset --help`"))?;
    Ok(Some((name, json)))
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

fn parse_set_args(args: &[String], command: &str) -> Result<SetParse> {
    let mut options = SetOptions::default();
    let mut json = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        match arg {
            "--help" | "-h" => return Ok(SetParse::Help(command_help(command))),
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
                bail!("unknown option {other:?}; use `waywarm {command} --help`")
            }
            other => bail!("unexpected argument {other:?}; use `waywarm {command} --help`"),
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
    let presets = if settings.presets.is_empty() {
        "(none)".into()
    } else {
        settings
            .presets
            .keys()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let outputs = if state.outputs.is_empty() {
        "(none)".into()
    } else {
        state.outputs.join(", ")
    };
    let mut text = format!(
        "Filter:     {filter}\n\
         Mode:       {mode}\n\
         Active:     {}\n\
         Manual:     {}\n\
         Day:        {}\n\
         Night:      {}\n\
         Schedule:   {timing} · fade {}m\n\
         Presets:    {presets}\n\
         Backend:    {}\n\
         Outputs:    {outputs}\n",
        format_levels_line(state.active_warmth, state.active_brightness),
        format_levels(settings.manual),
        format_levels(settings.schedule.day),
        format_levels(settings.schedule.night),
        settings.schedule.transition_minutes,
        state.backend,
    );
    if let Some(conflict) = &state.conflict {
        text.push_str(&format!("Conflict:   {conflict}\n"));
    }
    text
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
        "preset" => "Usage: waywarm preset <action> [name] [--json]\n\n\
Actions:\n\
  list                 List saved preset names\n\
  save <name>          Save current levels/schedule as a preset\n\
  apply <name>         Apply a saved preset (keeps filter on/off)\n\
  delete <name>        Remove a preset (alias: rm)\n\
\n\
Presets store automatic/manual mode, levels, and schedule — not the filter\n\
enable toggle. Requires a running Waywarm daemon."
            .into(),
        "export" => "Usage: waywarm export preset <name>\n\n\
Print a daemon-free `waywarm apply …` command that reproduces the named\n\
preset. Reads ~/.config/waywarm/config.toml (no daemon required). Paste the\n\
line into a compositor config, for example `exec <printed command>` in sway."
            .into(),
        "apply" => "Usage: waywarm apply [options]\n\n\
Run a foreground gamma session with the given settings. Does not require a\n\
pre-running daemon and does not rewrite config.toml. Suitable for compositor\n\
startup (`exec` in sway / i3). Stops on SIGINT/SIGTERM.\n\
\n\
Options:\n\
  --mode automatic|manual      Schedule mode (aliases: auto)\n\
  --warmth <0-100>             Manual warmth\n\
  --brightness <10-100>        Manual brightness\n\
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
\n\
The filter is always enabled. Conflicts with an already-running Waywarm daemon."
            .into(),
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
        let SetParse::Options { options, json } = parse_set_args(&args, "set").unwrap() else {
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
        assert!(parse_set_args(&["--warmth".into(), "200".into()], "set").is_err());
        assert!(parse_set_args(&["--brightness".into(), "5".into()], "set").is_err());
        assert!(parse_set_args(&["--mode".into(), "schedule".into()], "set").is_err());
        assert!(parse_set_args(&["--night-start".into(), "9:00".into()], "set").is_err());
        assert!(parse_set_args(&["--transition".into(), "999".into()], "set").is_err());
        assert!(parse_set_args(&["--unknown".into()], "set").is_err());
        assert!(matches!(
            parse_set_args(&["--help".into()], "set").unwrap(),
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
            conflict: None,
        };
        let text = format_status_human(&state);
        assert!(text.contains("Filter:"));
        assert!(text.contains("Mode:"));
        assert!(text.contains("Active:"));
        assert!(text.contains("Day:"));
        assert!(text.contains("Night:"));
        assert!(text.contains("Presets:"));
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

    #[test]
    fn format_apply_command_manual_preset() {
        let preset = Preset {
            automatic: false,
            manual: Levels {
                warmth: 50,
                brightness: 90,
            },
            schedule: Schedule::default(),
        };
        assert_eq!(
            format_apply_command(&preset),
            "waywarm apply --mode manual --warmth 50 --brightness 90"
        );
    }

    #[test]
    fn format_apply_command_automatic_fixed() {
        let preset = Preset {
            automatic: true,
            manual: Levels::default(),
            schedule: Schedule {
                night_start: "21:00".into(),
                day_start: "07:00".into(),
                transition_minutes: 30,
                day: Levels {
                    warmth: 0,
                    brightness: 100,
                },
                night: Levels {
                    warmth: 50,
                    brightness: 90,
                },
                timing: ScheduleTiming::Fixed,
                latitude: 0.0,
                longitude: 0.0,
            },
        };
        assert_eq!(
            format_apply_command(&preset),
            "waywarm apply --mode automatic --day-warmth 0 --day-brightness 100 --night-warmth 50 --night-brightness 90 --night-start 21:00 --day-start 07:00 --transition 30 --timing fixed"
        );
    }

    #[test]
    fn format_apply_command_automatic_location() {
        let preset = Preset {
            automatic: true,
            manual: Levels::default(),
            schedule: Schedule {
                night_start: "21:00".into(),
                day_start: "07:00".into(),
                transition_minutes: 45,
                day: Levels {
                    warmth: 10,
                    brightness: 100,
                },
                night: Levels {
                    warmth: 55,
                    brightness: 85,
                },
                timing: ScheduleTiming::Location,
                latitude: 48.8566,
                longitude: 2.3522,
            },
        };
        assert_eq!(
            format_apply_command(&preset),
            "waywarm apply --mode automatic --day-warmth 10 --day-brightness 100 --night-warmth 55 --night-brightness 85 --night-start 21:00 --day-start 07:00 --transition 45 --timing location --latitude 48.8566 --longitude 2.3522"
        );
    }

    #[test]
    fn format_apply_command_round_trips_through_apply_options() {
        let preset = Preset {
            automatic: true,
            manual: Levels {
                warmth: 12,
                brightness: 88,
            },
            schedule: Schedule {
                night_start: "22:15".into(),
                day_start: "06:45".into(),
                transition_minutes: 20,
                day: Levels {
                    warmth: 5,
                    brightness: 95,
                },
                night: Levels {
                    warmth: 70,
                    brightness: 80,
                },
                timing: ScheduleTiming::Location,
                latitude: -33.8688,
                longitude: 151.2093,
            },
        };
        let command = format_apply_command(&preset);
        let tokens: Vec<String> = command
            .split_whitespace()
            .skip(2) // waywarm apply
            .map(str::to_owned)
            .collect();
        let SetParse::Options { options, .. } = parse_set_args(&tokens, "apply").unwrap() else {
            panic!("expected options");
        };
        let mut settings = Settings::default();
        apply_set_options(&mut settings, &options).unwrap();
        settings.enabled = true;
        assert!(settings.automatic);
        assert_eq!(settings.schedule, preset.schedule);
    }

    #[test]
    fn format_apply_command_manual_round_trips() {
        let preset = Preset {
            automatic: false,
            manual: Levels {
                warmth: 42,
                brightness: 75,
            },
            schedule: Schedule::default(),
        };
        let command = format_apply_command(&preset);
        let tokens: Vec<String> = command
            .split_whitespace()
            .skip(2)
            .map(str::to_owned)
            .collect();
        let SetParse::Options { options, .. } = parse_set_args(&tokens, "apply").unwrap() else {
            panic!("expected options");
        };
        let mut settings = Settings::default();
        apply_set_options(&mut settings, &options).unwrap();
        assert!(!settings.automatic);
        assert_eq!(settings.manual, preset.manual);
    }
}
