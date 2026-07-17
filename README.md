<div align="center">

# Waywarm

**A small, friendly blue-light filter for wlroots-based Wayland desktops.**

[![CI](https://github.com/KLAMBO365/waywarm/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/KLAMBO365/waywarm/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/KLAMBO365/waywarm?style=flat-square&color=7aa2f7)](https://github.com/KLAMBO365/waywarm/releases)
[![License](https://img.shields.io/badge/license-MIT-9ece6a?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-e0af68?style=flat-square&logo=rust)](https://www.rust-lang.org/)

Control warmth, brightness, and smooth automatic transitions from a clean terminal UI.

<img src="assets/waywarm-tui.png" alt="Waywarm settings interface" width="820">

</div>

## Highlights

- Live warmth and brightness controls
- Automatic schedules with separate day and night targets
- Smooth transitions between day and night
- Standalone mode or an optional systemd user service
- Immediate, persistent settings

## Install

Requires Linux **x86_64** and a compositor supporting
`wlr-gamma-control-unstable-v1`. The curl installer and prebuilt release
binary are x86_64 only; other architectures need a source build.

Install or update to the latest release with:

```sh
curl --proto '=https' --tlsv1.2 -fsSL https://raw.githubusercontent.com/KLAMBO365/waywarm/main/install.sh | sh
```

The installer downloads the release, verifies its SHA-256 checksum, and installs
`waywarm` to `~/.local/bin`. Ensure that directory is in your `PATH`, then launch:

```console
waywarm
```

For automatic startup, open the service manager and choose
**Install or update and start**:

```console
waywarm daemon
```

<details>
<summary>Manual installation</summary>

Download the archive and checksum from the
[latest release](https://github.com/KLAMBO365/waywarm/releases/latest), then:

```console
sha256sum -c waywarm-*.tar.gz.sha256
tar -xzf waywarm-*.tar.gz
install -Dm755 waywarm-*/waywarm ~/.local/bin/waywarm
```

</details>

<details>
<summary>Build from source (Rust 1.88+)</summary>

```console
git clone https://github.com/KLAMBO365/waywarm.git
cd waywarm
make install
```

</details>

## Compatibility

Works with wlroots compositors such as **Sway**, **river**, and **Wayfire**.
GNOME, KDE Plasma, and newer Hyprland versions are not supported.

> [!IMPORTANT]
> Gamma control is exclusive. Stop `gammastep`, `wlsunset`, or similar tools
> before starting Waywarm.

## Configuration

Settings are stored at `~/.config/waywarm/config.toml` (or
`$XDG_CONFIG_HOME/waywarm/config.toml`). The TUI and CLI update this file
immediately when you change options.

## Controls

Use arrow keys or `h`/`j`/`k`/`l` to navigate and adjust values. Press
`Space` or `Enter` to toggle options, and `q` or `Esc` to quit. Changes are
applied and saved immediately.

## CLI

Scriptable commands talk to a running daemon (the optional service, or an open
settings UI). They fail clearly if nothing is listening.

```console
waywarm status
waywarm status --json
waywarm toggle
waywarm enable
waywarm disable
waywarm set --warmth 40 --brightness 90
waywarm set --mode automatic
waywarm set --day-warmth 10 --night-warmth 55
waywarm set --night-start 21:30 --transition 45
```

`set` updates and saves configuration the same way as the TUI. Manual
`--warmth` / `--brightness` switch into enabled manual mode unless you also
pass `--mode`. Use `--json` on `status`, `set`, `enable`, `disable`, or
`toggle` for machine-readable output (`RuntimeState` fields may grow over
time).

## License

[MIT](LICENSE)
