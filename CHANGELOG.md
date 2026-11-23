# Changelog

All notable changes to harbor are documented in this file.

## [0.3.0]

### Added

- **Background blur that follows the desktop.** harbor never adds blur of its own: on GNOME it is blurred only when Blur my Shell is enabled (using its pipeline, with detection fixed for Blur my Shell's newer pipeline setup); on KDE Plasma only when KWin's blur effect is on (via `ext-background-effect-v1`, falling back to `org_kde_kwin_blur` on older KWin); on Hyprland/SwayFX the compositor's own rules apply. Other desktops keep the plain look.
- **Theme-matched colours.** The tint, text colour and corner radius come from the live theme (GNOME panel-menu theme nodes, the KDE colour scheme), so harbor looks like the other panel menus. Secondary text is a solid mix of the theme text colour so it never washes out.
- The GNOME top-bar button appears only while harbor is running, like a tray icon.
- `TESTING.md`: a cross-desktop release checklist (KDE, Xfce, wlroots).

### Changed

- Compact two-line rows: hover icons sit on the name line, and uptime, CPU, memory and energy are aligned 12 px columns. The launching app (terminal/editor) moved to the expanded details.
- Narrower popup (376 px window, 362 px card); copying a port shows a check mark on the button instead of a toast.
- Settings use plain rows with separators and text-style dropdowns; dropdown menus and tooltips have a solid surface so they read over any background.
- Sharper text rendering (no greyscale anti-aliasing).

### Fixed

- Reopening harbor after its idle WebView was freed no longer shows the window growing or stuttering: the list is scanned and rendered while the window is hidden, and it appears once at its final height (with a fallback so it always appears). The offscreen WebKit snapshot step was removed.
- An older harbor (for example a previously installed package autostarting) can no longer replace a newer installed GNOME extension; extension refreshes are version-gated.
- Meta text no longer clips at the top, and stays readable over light backgrounds.

### Security

- Theme colours from the GNOME extension arrive as raw bytes and are accepted only from the extension's own D-Bus connection.

### Known issues

- KDE, Xfce and wlroots desktops are code-reviewed but not yet tested live; see `TESTING.md`.

## [0.2.0]

This release is a full pass over the 0.1.0 implementation: correctness and safety fixes, a new tray and popup architecture, large memory and CPU savings, a redesigned UI, and GNOME integration.

### Fixed — correctness and safety

- Port scans no longer freeze the tray and window: scanning runs off the main thread (it previously blocked the UI for every scan, up to 10 s).
- CPU and uptime are correct for processes whose names contain spaces or parentheses (e.g. `next-server (v16.2.7)`); the clock tick is read from the system instead of assumed.
- Kill protection has a single source of truth in the backend: processes of other users and known system services are always protected; the UI no longer overrides it with name-based guesses. Your own processes stay killable with confirmation.
- A port bound on both IPv4 and IPv6 shows as one row (PIDs merged, protected if any owner is); the main PID per port is deterministic so rows don't rebuild on refresh.
- Before killing, harbor re-checks that the PIDs still own that port, so a PID reused since the last scan is never signalled.
- "Open in browser" no longer leaves a defunct process per click, accepts only `http://localhost:<port>` / `http://127.0.0.1:<port>`, and passes the desktop's activation token so the browser comes to the front.
- Scoped npm packages resolve to their name (`@openai/codex` → `codex`); the backend is the only source of process names.
- Expanded row details (CPU, memory, uptime, energy) update on every refresh instead of showing stale values.
- harbor no longer creates a duplicate tray icon, and no longer fails to start when no tray host is running yet.

### Changed — tray, placement and behaviour

- Replaced Tauri's tray and the deprecated `libayatana-appindicator` with a StatusNotifierItem tray over D-Bus (`ksni`): single click opens harbor on KDE, Xfce, Cinnamon and other SNI hosts; double click on GNOME's AppIndicator extension. The tray menu has a single "Open harbor" item (Quit is in the popup).
- Popup placement per desktop: wlr layer-shell anchored under the panel (KDE, Hyprland, Sway, niri, COSMIC), positioned at the tray icon on X11, compositor-placed on GNOME.
- The popup takes focus via the desktop's activation token, hides when it loses focus or on Escape, can't be dragged, and clicking the tray icon while it's open closes it.
- Single instance: launching harbor again opens the popup in the running instance. Autostart runs `harbor --hidden`; normal launches show the popup.

### Added

- **GNOME Shell companion extension** (enable from Settings → GNOME): harbor's own top-bar button opens the popup directly under it with one click, matches the gap of GNOME's own panel menus, keeps the top pinned as the popup resizes, closes on outside clicks, focus changes or other panel menus, and keeps harbor out of the dock. While it is active harbor hides its tray icon. Works on any GNOME distro without the AppIndicator extension.
- **Start at login** toggle (XDG autostart, works on all desktops; AppImage-aware).
- Live reload for HTML, CSS and JavaScript during `bun run dev`.

### Changed — performance and memory

- Scanning reads `/proc/net/tcp{,6}` directly with a cached socket-owner map instead of running `ss` (steady-state scan ~73 ms → ~3 ms CPU) and no longer depends on iproute2.
- Polling only runs while the popup is open (zero scans while hidden), scans never overlap, and stale results are discarded.
- CPU % and energy reflect current usage (delta between scans) instead of a lifetime average.
- The WebView is created on demand and freed after 60 s hidden, then memory is returned to the system (`malloc_trim`): about 24 MB while hidden after login versus ~150 MB before.
- Removed `tauri-plugin-opener` and its dependencies; the popup makes no network requests (system font instead of Google Fonts).
- The panel's grain texture is composited once instead of being repainted on every refresh (~6× less WebKit CPU while open).

### Changed — interface

- Redesigned popup: the desktop's own fonts (monospace only for port numbers), content-driven height with scrolling past a cap, translucent panel with solid header and footer, inline Kill/Force confirmation instead of a modal, hover actions, and an in-popup Settings view grouped into General, Killing and Appearance.
- Settings dropdowns render inside the popup (native dropdowns opened a separate window and closed harbor); the popup always reopens on the port list.
- Native-app behaviour: no page zoom (pinch, Ctrl+scroll, zoom keys), no browser context menu in release builds, no stray text selection, pill tooltips.

### Packaging

- deb/rpm depend on `gtk-layer-shell` instead of appindicator; the AppImage bundles `libgtk-layer-shell`; every bundle embeds the GNOME extension. README and build instructions updated.

## [0.1.0] - 2026-07-11

Initial release. Inspired by [Ports](https://ports-app.com) for macOS by Alex Tapper.

### Added

- System tray app for listing localhost TCP listeners
- Kill and force-kill for user-owned dev servers (Next, Vite, Node, etc.)
- Protection for system processes (root, other users, ulauncher, desktop services)
- Expandable port rows with hover actions (open, copy, kill)
- Settings: theme, auto-refresh, show all ports, confirm before kill, default kill action
- Custom dark-safe dropdowns in settings
- Harbor lighthouse tray icon

### Fixed

- Kill flow after confirm (PID resolution and IPC)
- Next.js `next-server` process name parsing when cmdline contains spaces
- System vs dev-server protection heuristics
