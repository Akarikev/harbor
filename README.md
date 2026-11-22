# harbor

See every dev server running on localhost from your system tray.

harbor is a lightweight Linux tray utility built with [Tauri 2](https://v2.tauri.app/). It scans listening TCP ports, shows process metadata, and lets you open, copy, or kill dev servers — while protecting system services.

## Download

Pre-built binaries are published on [GitHub Releases](https://github.com/Akarikev/harbor/releases/latest):

| Format | Best for |
|--------|----------|
| **AppImage** | Most Linux distros (recommended) |
| **.deb** | Ubuntu, Debian, Pop!_OS, Mint |
| **.rpm** | Fedora, RHEL, openSUSE |

Deploy the static landing page from `website/` on Vercel (see [LAUNCH.md](LAUNCH.md)).

### AppImage quick start

```bash
chmod +x harbor_*_amd64.AppImage
./harbor_*_amd64.AppImage
```

### GNOME top bar

In harbor, open **Settings → GNOME** and click **Enable**, then sign out and back in. While the companion extension is active, its top-bar button replaces the tray icon.

### Autostart

Use **Settings → Start at login** to change autostart for your account. The Debian and RPM packages enable it by default through `/etc/xdg/autostart/harbor.desktop`. The AppImage starts on demand by default. Move it to a permanent location before enabling the setting, since harbor saves its absolute path.

## Features

- StatusNotifierItem tray icon and optional GNOME top-bar button
- CPU, memory, uptime, and energy indicators
- Kill / force-kill with confirmation
- Open localhost URLs in browser
- System process protection
- Dark / light / system theme
- Start at login

## System requirements

- Linux x86_64
- GTK 3, WebKitGTK 4.1, and gtk-layer-shell
- A StatusNotifierItem host or the harbor GNOME Shell extension for panel access

### Build dependencies (Ubuntu/Debian)

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libgtk-layer-shell-dev \
  librsvg2-dev \
  patchelf \
  build-essential \
  curl
```

## Build from source

Requires [Rust](https://rustup.rs/) and [Bun](https://bun.sh/).

```bash
git clone https://github.com/Akarikev/harbor.git
cd harbor
bun install
bun run dev      # development with frontend live reload
bun run build    # release bundles
```

Release artifacts appear in `src-tauri/target/release/bundle/`.

## Launch (production)

See [LAUNCH.md](LAUNCH.md) for the v0.1.0 release checklist.

## Acknowledgements

harbor was inspired by [Ports](https://ports-app.com) for macOS, built by [Alex Tapper](https://x.com/ABTapper). See [the original tweet](https://x.com/ABTapper/status/2068587240670843083) that sparked the idea. harbor is an independent Linux project — not affiliated with Ports or Cursor.

## License

MIT — see [LICENSE](LICENSE).
