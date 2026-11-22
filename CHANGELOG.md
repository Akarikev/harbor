# Changelog

All notable changes to harbor are documented in this file.

## [0.2.0]

### Added

- GNOME Shell integration with a top-bar button, popup placement under the button, and dismissal on outside click or Escape. Enable it from harbor’s settings.
- Start at login setting and single-instance launches. Opening harbor again shows the popup in the running instance.
- Live reload for HTML, CSS, and JavaScript during `bun run dev`.

### Changed

- Replaced the tray backend with StatusNotifierItem over D-Bus. The popup uses layer-shell where supported and tray placement on X11.
- Redesigned the popup with desktop fonts, content-driven height, launcher names, inline kill confirmation, and in-popup settings.
- Reduced refresh redraws by compositing the grain overlay separately. Disabled browser zoom and navigation in the popup.
- Scans now read listening sockets from `/proc` and reuse cached socket owners. CPU and energy indicators reflect recent usage, and polling stops while the popup is hidden.
- The WebView is created on demand and released after 60 seconds hidden.
- Replaced the appindicator library dependency with gtk-layer-shell.

### Fixed

- Revalidate that a process still owns its port before signalling it. Keep multi-owner rows stable and refresh expanded stats in place.
- Correctly resolve scoped npm package names and open localhost URLs without leaving defunct processes.
- Keep GNOME placement stable across reopens and resizes, hide the unpinned running icon from the dock, and prevent the cold-open white flash.

### Known issues

- Hover switching between harbor and other panel menus may fail on GNOME.

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
