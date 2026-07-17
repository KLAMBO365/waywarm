//! Detect competing blue-light / gamma clients.

use std::{fs, path::Path, process};

/// Well-known gamma / night-light helpers that fight over wlr-gamma-control.
const COMPETITOR_NAMES: &[&str] = &[
    "gammastep",
    "gammastep-indicator",
    "wlsunset",
    "redshift",
    "redshift-gtk",
];

/// Return display names of competing processes found on this machine.
pub fn competing_gamma_processes() -> Vec<String> {
    let self_pid = process::id();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(pid_text) = file_name.to_str() else {
            continue;
        };
        if !pid_text.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        if let Some(name) = competitor_name(&entry.path()) {
            let label = format!("{name} (pid {pid})");
            if !found.iter().any(|existing| existing == &label) {
                found.push(label);
            }
        }
    }
    found.sort();
    found
}

fn competitor_name(proc_dir: &Path) -> Option<&'static str> {
    let comm = fs::read_to_string(proc_dir.join("comm")).ok()?;
    let comm = comm.trim();
    for name in COMPETITOR_NAMES {
        if comm == *name {
            return Some(*name);
        }
    }

    // Some distros launch helpers under a different comm; check cmdline tokens.
    let cmdline = fs::read(proc_dir.join("cmdline")).ok()?;
    let text = String::from_utf8_lossy(&cmdline);
    for token in text.split('\0') {
        let base = Path::new(token)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(token);
        for name in COMPETITOR_NAMES {
            if base == *name {
                return Some(*name);
            }
        }
    }
    None
}

/// Build a user-facing conflict summary from failed outputs and process scan.
pub fn conflict_message(failed_outputs: &[String], competitors: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    if !competitors.is_empty() {
        parts.push(format!(
            "another gamma client appears to be running: {}",
            competitors.join(", ")
        ));
    }
    if !failed_outputs.is_empty() {
        parts.push(format!(
            "gamma control unavailable on: {}",
            failed_outputs.join(", ")
        ));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conflict_message_combines_sources() {
        assert!(conflict_message(&[], &[]).is_none());
        let message = conflict_message(&["eDP-1".into()], &["gammastep (pid 1)".into()]).unwrap();
        assert!(message.contains("gammastep"));
        assert!(message.contains("eDP-1"));
    }

    #[test]
    fn competitor_table_covers_common_tools() {
        assert!(COMPETITOR_NAMES.contains(&"gammastep"));
        assert!(COMPETITOR_NAMES.contains(&"wlsunset"));
        assert!(COMPETITOR_NAMES.contains(&"redshift"));
    }
}
