# Cross-desktop test checklist

harbor is developed on GNOME. Run this on the other desktops before a release.
Each run takes about 10 minutes in a VM.

## Setup (any distro)

- Use **GNOME Boxes** (`sudo apt install gnome-boxes`) with a live ISO; no install needed.
- Copy the AppImage (or the `.deb`/`.rpm` for that distro) into the VM and start it:
  `chmod +x harbor_*.AppImage && ./harbor_*.AppImage`
- Keep a terminal open with `tail -f /tmp/harbor.log` (debug builds) or run harbor from
  the terminal to see its output.

| Desktop | Suggested ISO | Path it exercises |
|---|---|---|
| KDE Plasma 6 (Wayland) | Fedora KDE or KDE neon | layer-shell placement, KDE tray, KWin blur |
| Xfce (X11) | Xubuntu | X11 placement, Xfce tray |
| Hyprland or Sway (optional) | Fedora Sway spin / a Hyprland ISO | wlroots layer-shell, waybar tray |

## Checks on every desktop

1. **Starts:** launching harbor shows the popup; the log has a `placement mode:` line
   (`layer-shell` on KDE/wlroots, `x11` on Xfce).
2. **Tray icon:** harbor's icon is in the tray. **Single left-click** opens the popup;
   clicking again closes it. Right-click shows the menu with *Open harbor*.
3. **Placement:** the popup opens under the tray icon (or top-right if the tray gave no
   position), stays on screen, and doesn't jump when ports appear or disappear.
4. **Dismissal:** clicking outside the popup closes it; Escape closes it; typing goes into
   the popup while it is open.
5. **Single instance:** launching harbor again (app menu or terminal) opens the existing
   popup; no second tray icon appears.
6. **No white flash** on first open, and none when reopening after leaving it closed for
   more than 60 s.
7. **Ports:** start `python3 -m http.server 8000`; it appears with uptime, CPU, memory and
   energy. Copy shows a check mark; *Open* opens the browser; kill (×) shows Kill/Force and
   removes the row.
8. **Settings:** dropdowns open inside the popup (the popup stays open), theme switches,
   *Start at login* toggles.
9. **Start at login:** with the toggle on, log out and in: harbor starts hidden with its
   tray icon. With it off, it doesn't start.
10. **Hidden-start memory:** after logging in, harbor uses little memory while hidden
    (no `WebKitWebProcess` running until the popup is first opened).

## Desktop-specific

- **KDE:** with *System Settings → Desktop Effects → Blur* **on**, the popup is blurred and
  tinted like Plasma's own menus; with Blur **off**, it uses the plain look.
- **Xfce (X11):** the popup appears near the tray icon; no Wayland warnings matter here.
- **Hyprland/Sway:** the popup is anchored below the bar; blur only if your compositor
  rules blur it (harbor adds none of its own).

## Reporting a problem

Note the desktop, distro and harbor version, the check number, what you saw, and attach
the harbor log output. A screenshot helps for placement or appearance issues.
