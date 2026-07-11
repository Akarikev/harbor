# Changelog

All notable changes to harbor are documented in this file.

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
