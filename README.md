<div align="center">

# Waywarm

**A small, friendly blue-light filter for wlroots-based Wayland desktops.**

[![Release](https://img.shields.io/badge/release-0.1.0-7aa2f7?style=flat-square)](https://github.com/KLAMBO365/waywarm/releases)
[![License](https://img.shields.io/badge/license-MIT-9ece6a?style=flat-square)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-e0af68?style=flat-square&logo=rust)](https://www.rust-lang.org/)

Control warmth, brightness, and smooth automatic transitions from a clean terminal UI.

<img src="assets/waywarm-tui.png" alt="Waywarm settings interface" width="820">

</div>

## Highlights

- Live warmth and brightness controls
- Automatic schedules with smooth transitions
- Standalone mode or an optional systemd user service
- Immediate, persistent settings

## Install

Requires Linux and a compositor supporting
`wlr-gamma-control-unstable-v1`.

Download the archive and checksum from the
[latest release](https://github.com/KLAMBO365/waywarm/releases/latest), then:

```console
sha256sum -c waywarm-*.tar.gz.sha256
tar -xzf waywarm-*.tar.gz
install -Dm755 waywarm-*/waywarm ~/.local/bin/waywarm
```

Ensure `~/.local/bin` is in your `PATH`, then launch:

```console
waywarm
```

For automatic startup, open the service manager and choose
**Install or update and start**:

```console
waywarm daemon
```

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

## Controls

Use arrow keys or `h`/`j`/`k`/`l` to navigate and adjust values. Press
`Space` or `Enter` to toggle options, and `q` or `Esc` to quit. Changes are
applied and saved immediately.

## License

[MIT](LICENSE)
