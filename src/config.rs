use std::{env, fs, io::Write, path::PathBuf};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Schedule {
    pub night_start: String,
    pub day_start: String,
    pub transition_minutes: u16,
    pub night: Levels,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            night_start: "21:00".into(),
            day_start: "07:00".into(),
            transition_minutes: 30,
            night: Levels {
                warmth: 50,
                brightness: 90,
            },
        }
    }
}

impl Schedule {
    pub fn validate(&self) -> Result<()> {
        let night = parse_time(&self.night_start)?;
        let day = parse_time(&self.day_start)?;
        if night == day {
            bail!("day and night start times must differ");
        }
        if self.transition_minutes > MAX_TRANSITION_MINUTES {
            bail!("transition must be between 0 and 240 minutes");
        }
        let gap = night.abs_diff(day);
        let shortest_gap = gap.min(MINUTES_PER_DAY - gap);
        if self.transition_minutes > shortest_gap {
            bail!("transition is longer than the gap between day and night start times");
        }
        self.night.validate()
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
        self.schedule.validate()
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
}
