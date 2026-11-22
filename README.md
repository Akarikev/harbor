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

### Autostart

The Debian and RPM packages start harbor automatically at login. To disable it, use your desktop's Startup Applications settings or copy `/etc/xdg/autostart/harbor.desktop` to `~/.config/autostart/` and set `Hidden=true` in the copy.

For AppImage autostart, create `~/.config/autostart/harbor.desktop` with the absolute path to your AppImage:

```ini
[Desktop Entry]
Type=Application
Name=harbor
Exec=/path/to/harbor.AppImage --hidden
```

## Features

- Tray icon with port list popup
- CPU, memory, uptime, and energy indicators
- Kill / force-kill with confirmation
- Open localhost URLs in browser
- System process protection
- Dark / light / system theme

## System requirements

- Linux x86_64
- GTK 3 + WebKitGTK 4.1
- System tray support (X11 or Wayland with appindicator)

### Build dependencies (Ubuntu/Debian)

```bash
sudo apt update
sudo apt install -y \
  libwebkit2gtk-4.1-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  patchelf \
  build-essential \
  curl
```

## Build from source

Requires [Rust](https://rustup.rs/) and [Bun](https://bun.sh/) (or Node.js + npm).

```bash
git clone https://github.com/Akarikev/harbor.git
cd harbor
bun install
bun run dev      # development
bun run build    # release bundles
```

Release artifacts appear in `src-tauri/target/release/bundle/`.

## Launch (production)

See [LAUNCH.md](LAUNCH.md) for the v0.1.0 release checklist.

## Acknowledgements

harbor was inspired by [Ports](https://ports-app.com) for macOS, built by [Alex Tapper](https://x.com/ABTapper). See [the original tweet](https://x.com/ABTapper/status/2068587240670843083) that sparked the idea. harbor is an independent Linux project — not affiliated with Ports or Cursor.

## License

MIT — see [LICENSE](LICENSE).
