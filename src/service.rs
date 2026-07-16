use std::{
    env, fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result, bail};

const SERVICE_UNIT: &str = include_str!("../packaging/waywarm.service");
const SERVICE_NAME: &str = "waywarm.service";
const LEGACY_SERVICE_NAME: &str = "simplered.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceStatus {
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
}

impl ServiceStatus {
    pub fn installation_label(self) -> &'static str {
        match (self.installed, self.enabled) {
            (false, _) => "Not installed",
            (true, true) => "Installed • enabled",
            (true, false) => "Installed • disabled",
        }
    }
}

#[derive(Clone)]
pub struct ServiceManager {
    binary_path: PathBuf,
    unit_path: PathBuf,
}

impl ServiceManager {
    pub fn discover() -> Result<Self> {
        let home = PathBuf::from(env::var_os("HOME").context("HOME is not set")?);
        Ok(Self {
            binary_path: home.join(".local/bin/waywarm"),
            unit_path: home.join(".config/systemd/user/waywarm.service"),
        })
    }

    pub fn status(&self) -> ServiceStatus {
        ServiceStatus {
            installed: self.binary_path.is_file() && self.unit_path.is_file(),
            enabled: systemctl_success(["is-enabled", "--quiet", SERVICE_NAME]),
            active: systemctl_success(["is-active", "--quiet", SERVICE_NAME]),
        }
    }

    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    pub fn unit_path(&self) -> &Path {
        &self.unit_path
    }

    pub fn install_and_start(&self) -> Result<()> {
        retire_legacy_service()?;
        let source = env::current_exe().context("failed to locate the running executable")?;
        install_binary(&source, &self.binary_path)?;
        atomic_write(&self.unit_path, SERVICE_UNIT.as_bytes(), 0o644)?;
        self.import_wayland_environment()?;
        systemctl(["daemon-reload"])?;
        systemctl(["enable", SERVICE_NAME])?;
        systemctl(["restart", SERVICE_NAME])?;
        Ok(())
    }

    pub fn start(&self) -> Result<()> {
        self.require_installed()?;
        retire_legacy_service()?;
        self.import_wayland_environment()?;
        systemctl(["start", SERVICE_NAME])
    }

    pub fn stop(&self) -> Result<()> {
        self.require_installed()?;
        systemctl(["stop", SERVICE_NAME])
    }

    pub fn restart(&self) -> Result<()> {
        self.require_installed()?;
        retire_legacy_service()?;
        self.import_wayland_environment()?;
        systemctl(["restart", SERVICE_NAME])
    }

    pub fn uninstall(&self) -> Result<()> {
        let _ = run_systemctl(["disable", "--now", SERVICE_NAME]);
        remove_if_present(&self.unit_path)?;
        remove_if_present(&self.binary_path)?;
        systemctl(["daemon-reload"])?;
        let _ = run_systemctl(["reset-failed", SERVICE_NAME]);
        Ok(())
    }

    fn require_installed(&self) -> Result<()> {
        if !self.status().installed {
            bail!("the background service is not installed");
        }
        Ok(())
    }

    fn import_wayland_environment(&self) -> Result<()> {
        let variables: Vec<&str> = ["WAYLAND_DISPLAY", "XDG_CURRENT_DESKTOP", "XDG_RUNTIME_DIR"]
            .into_iter()
            .filter(|name| env::var_os(name).is_some())
            .collect();
        if variables.is_empty() {
            return Ok(());
        }
        let mut arguments = vec!["--user", "import-environment"];
        arguments.extend(variables);
        let output = Command::new("systemctl").args(arguments).output()?;
        ensure_success(output, "failed to import the Wayland environment")
    }
}

pub fn retire_legacy_service() -> Result<bool> {
    let active = systemctl_success(["is-active", "--quiet", LEGACY_SERVICE_NAME]);
    let enabled = systemctl_success(["is-enabled", "--quiet", LEGACY_SERVICE_NAME]);
    if !active && !enabled {
        return Ok(false);
    }

    let output = run_systemctl(["disable", "--now", LEGACY_SERVICE_NAME])?;
    ensure_success(output, "failed to retire the legacy SimpleRed service")?;
    Ok(true)
}

fn install_binary(source: &Path, destination: &Path) -> Result<()> {
    if source == destination {
        return Ok(());
    }
    let parent = destination.parent().context("binary path has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".waywarm-install-{}", std::process::id()));
    fs::copy(source, &temporary).with_context(|| {
        format!(
            "failed to copy {} to {}",
            source.display(),
            temporary.display()
        )
    })?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o755))?;
    fs::rename(&temporary, destination)?;
    Ok(())
}

fn atomic_write(path: &Path, contents: &[u8], mode: u32) -> Result<()> {
    let parent = path.parent().context("installation path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file_mut()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    temporary.write_all(contents)?;
    temporary.as_file_mut().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn remove_if_present(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn systemctl<const N: usize>(arguments: [&str; N]) -> Result<()> {
    let description = format!("systemctl --user {}", arguments.join(" "));
    ensure_success(run_systemctl(arguments)?, &description)
}

fn systemctl_success<const N: usize>(arguments: [&str; N]) -> bool {
    run_systemctl(arguments)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn run_systemctl<const N: usize>(arguments: [&str; N]) -> std::io::Result<Output> {
    Command::new("systemctl")
        .arg("--user")
        .args(arguments)
        .output()
}

fn ensure_success(output: Output, description: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    bail!("{description}: {}", error.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_labels_cover_service_states() {
        assert_eq!(
            ServiceStatus {
                installed: false,
                enabled: false,
                active: false,
            }
            .installation_label(),
            "Not installed"
        );
        assert_eq!(
            ServiceStatus {
                installed: true,
                enabled: true,
                active: true,
            }
            .installation_label(),
            "Installed • enabled"
        );
    }
}
