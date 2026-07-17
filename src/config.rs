use std::{collections::BTreeMap, env, fs, io::Write, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

pub const CONFIG_VERSION: u32 = 1;
pub const MIN_BRIGHTNESS: u8 = 10;
pub const MAX_TRANSITION_MINUTES: u16 = 240;
const MINUTES_PER_DAY: u16 = 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    pub warmth: u8,
    pub brightness: u8,
}

impl Levels {
    pub const NEUTRAL: Self = Self {
        warmth: 0,
        brightness: 100,
    };

    pub fn validate(self) -> Result<()> {
        if self.warmth > 100 {
            bail!("warmth must be between 0 and 100 percent");
        }
        if !(MIN_BRIGHTNESS..=100).contains(&self.brightness) {
            bail!("brightness must be between 10 and 100 percent");
        }
        Ok(())
    }
}

impl Default for Levels {
    fn default() -> Self {
        Self::NEUTRAL
    }
}

/// How automatic day/night transition times are chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleTiming {
    /// Use fixed `day_start` / `night_start` clock times.
    #[default]
    Fixed,
    /// Derive times from civil dawn/dusk at `latitude` / `longitude`.
    Location,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub night_start: String,
    pub day_start: String,
    pub transition_minutes: u16,
    /// Daytime target while automatic mode is active (defaults to neutral).
    pub day: Levels,
    pub night: Levels,
    /// Fixed clock times, or civil twilight from coordinates.
    pub timing: ScheduleTiming,
    /// Degrees north; used when `timing` is [`ScheduleTiming::Location`].
    pub latitude: f64,
    /// Degrees east; used when `timing` is [`ScheduleTiming::Location`].
    pub longitude: f64,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            night_start: "21:00".into(),
            day_start: "07:00".into(),
            transition_minutes: 30,
            day: Levels::NEUTRAL,
            night: Levels {
                warmth: 50,
                brightness: 90,
            },
            timing: ScheduleTiming::Fixed,
            latitude: 0.0,
            longitude: 0.0,
        }
    }
}

impl Schedule {
    pub fn validate(&self) -> Result<()> {
        if self.transition_minutes > MAX_TRANSITION_MINUTES {
            bail!("transition must be between 0 and 240 minutes");
        }
        self.day.validate()?;
        self.night.validate()?;

        match self.timing {
            ScheduleTiming::Fixed => self.validate_fixed_times(),
            ScheduleTiming::Location => {
                if !(-90.0..=90.0).contains(&self.latitude) {
                    bail!("latitude must be between -90 and 90 degrees");
                }
                if !(-180.0..=180.0).contains(&self.longitude) {
                    bail!("longitude must be between -180 and 180 degrees");
                }
                // Keep fallback clock times valid for polar days / failed sun calc.
                self.validate_fixed_times()
            }
        }
    }

    fn validate_fixed_times(&self) -> Result<()> {
        let night = parse_time(&self.night_start)?;
        let day = parse_time(&self.day_start)?;
        if night == day {
            bail!("day and night start times must differ");
        }
        let gap = night.abs_diff(day);
        let shortest_gap = gap.min(MINUTES_PER_DAY - gap);
        if self.transition_minutes > shortest_gap {
            bail!("transition is longer than the gap between day and night start times");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub enabled: bool,
    pub automatic: bool,
    pub manual: Levels,
    pub schedule: Schedule,
    /// Named snapshots of automatic/manual/schedule (filter enable is separate).
    #[serde(default)]
    pub presets: BTreeMap<String, Preset>,
}

/// A reusable filter profile that does not include the on/off toggle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub automatic: bool,
    pub manual: Levels,
    pub schedule: Schedule,
}

impl Preset {
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            automatic: settings.automatic,
            manual: settings.manual,
            schedule: settings.schedule.clone(),
        }
    }

    pub fn apply_to(&self, settings: &mut Settings) {
        settings.automatic = self.automatic;
        settings.manual = self.manual;
        settings.schedule = self.schedule.clone();
    }

    pub fn validate(&self) -> Result<()> {
        self.manual.validate()?;
        self.schedule.validate()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            enabled: true,
            automatic: true,
            manual: Levels {
                warmth: 50,
                brightness: 90,
            },
            schedule: Schedule::default(),
            presets: BTreeMap::new(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.version != CONFIG_VERSION {
            bail!(
                "unsupported configuration version {} (expected {})",
                self.version,
                CONFIG_VERSION
            );
        }
        self.manual.validate()?;
        self.schedule.validate()?;
        for (name, preset) in &self.presets {
            if name.trim().is_empty() {
                bail!("preset names must not be empty");
            }
            if name.chars().any(|c| c.is_control()) {
                bail!("preset name {name:?} contains invalid characters");
            }
            preset
                .validate()
                .with_context(|| format!("invalid preset {name:?}"))?;
        }
        Ok(())
    }

    pub fn save_preset(&mut self, name: impl Into<String>) -> Result<()> {
        let name = name.into();
        if name.trim().is_empty() {
            bail!("preset name must not be empty");
        }
        if name.chars().any(|c| c.is_control()) {
            bail!("preset name contains invalid characters");
        }
        let preset = Preset::from_settings(self);
        preset.validate()?;
        self.presets.insert(name, preset);
        Ok(())
    }

    pub fn apply_preset(&mut self, name: &str) -> Result<()> {
        let preset = self
            .presets
            .get(name)
            .with_context(|| format!("unknown preset {name:?}"))?
            .clone();
        preset.apply_to(self);
        Ok(())
    }

    pub fn delete_preset(&mut self, name: &str) -> Result<()> {
        if self.presets.remove(name).is_none() {
            bail!("unknown preset {name:?}");
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn discover() -> Result<Self> {
        let base = match env::var_os("XDG_CONFIG_HOME") {
            Some(path) => PathBuf::from(path),
            None => PathBuf::from(env::var_os("HOME").context("HOME is not set")?).join(".config"),
        };
        Ok(Self::at(base.join("waywarm/config.toml")))
    }

    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn load_or_create(&self) -> Result<Settings> {
        if !self.path.exists() {
            let settings = Settings::default();
            self.save(&settings)?;
            return Ok(settings);
        }
        let source = fs::read_to_string(&self.path)
            .with_context(|| format!("failed to read {}", self.path.display()))?;
        let settings: Settings = toml::from_str(&source)
            .with_context(|| format!("invalid configuration in {}", self.path.display()))?;
        settings.validate()?;
        Ok(settings)
    }

    pub fn save(&self, settings: &Settings) -> Result<()> {
        settings.validate()?;
        let parent = self
            .path
            .parent()
            .context("configuration path has no parent")?;
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
        let text = toml::to_string_pretty(settings)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.as_file_mut().set_permissions(unix_mode(0o600))?;
        temp.write_all(text.as_bytes())?;
        temp.as_file_mut().sync_all()?;
        temp.persist(&self.path)
            .map_err(|error| error.error)
            .with_context(|| format!("failed to replace {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(unix)]
fn unix_mode(mode: u32) -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt;
    fs::Permissions::from_mode(mode)
}

pub fn parse_time(value: &str) -> Result<u16> {
    let (hour_text, minute_text) = value
        .split_once(':')
        .with_context(|| format!("invalid time {value:?}; expected HH:MM"))?;
    let hour: u16 = hour_text
        .parse()
        .with_context(|| format!("invalid hour in {value:?}"))?;
    let minute: u16 = minute_text
        .parse()
        .with_context(|| format!("invalid minute in {value:?}"))?;
    if hour > 23 || minute > 59 || hour_text.len() != 2 || minute_text.len() != 2 {
        bail!("invalid time {value:?}; expected HH:MM");
    }
    Ok(hour * 60 + minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(dir.path().join("nested/config.toml"));
        let expected = Settings::default();
        store.save(&expected).unwrap();
        assert_eq!(store.load_or_create().unwrap(), expected);
    }

    #[test]
    fn rejects_invalid_values_and_versions() {
        let mut settings = Settings::default();
        settings.manual.brightness = 0;
        assert!(settings.validate().is_err());
        settings.manual.brightness = 100;
        settings.version = 99;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn rejects_overlapping_transitions() {
        let mut settings = Settings::default();
        settings.schedule.day_start = "20:59".into();
        assert!(settings.validate().is_err());
    }

    #[test]
    fn parses_strict_times() {
        assert_eq!(parse_time("07:30").unwrap(), 450);
        assert!(parse_time("7:30").is_err());
        assert!(parse_time("24:00").is_err());
    }

    #[test]
    fn missing_day_levels_default_to_neutral() {
        let source = r#"
version = 1
enabled = true
automatic = true

[manual]
warmth = 40
brightness = 90

[schedule]
night_start = "21:00"
day_start = "07:00"
transition_minutes = 30

[schedule.night]
warmth = 50
brightness = 90
"#;
        let settings: Settings = toml::from_str(source).unwrap();
        settings.validate().unwrap();
        assert_eq!(settings.schedule.day, Levels::NEUTRAL);
        assert_eq!(settings.schedule.night.warmth, 50);
        assert_eq!(settings.schedule.timing, ScheduleTiming::Fixed);
    }

    #[test]
    fn accepts_location_timing() {
        let mut settings = Settings::default();
        settings.schedule.timing = ScheduleTiming::Location;
        settings.schedule.latitude = 48.8566;
        settings.schedule.longitude = 2.3522;
        settings.validate().unwrap();
    }

    #[test]
    fn rejects_invalid_coordinates() {
        let mut settings = Settings::default();
        settings.schedule.timing = ScheduleTiming::Location;
        settings.schedule.latitude = 100.0;
        assert!(settings.validate().is_err());
    }

    #[test]
    fn preset_save_apply_and_delete() {
        let mut settings = Settings::default();
        settings.manual.warmth = 70;
        settings.save_preset("reading").unwrap();
        assert!(settings.presets.contains_key("reading"));

        settings.manual.warmth = 10;
        settings.apply_preset("reading").unwrap();
        assert_eq!(settings.manual.warmth, 70);

        settings.delete_preset("reading").unwrap();
        assert!(settings.presets.is_empty());
        assert!(settings.apply_preset("reading").is_err());
    }

    #[test]
    fn rejects_empty_preset_names() {
        let mut settings = Settings::default();
        assert!(settings.save_preset("").is_err());
        assert!(settings.save_preset("   ").is_err());
    }
}
