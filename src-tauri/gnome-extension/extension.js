// harbor GNOME Shell integration: a top-bar button that opens harbor's popup
// directly under it. Apps can't place their own windows on GNOME Wayland, so the
// shell (this extension) positions harbor's window and keeps it pinned there.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';
import {PanelBlur} from './blur.js';

const BUS_NAME = 'io.harbor.Harbor';
const OBJECT_PATH = '/io/harbor/Harbor';
const INTERFACE = 'io.harbor.Harbor';
// Owned while the extension is enabled; harbor drops its tray icon when it sees it.
const PRESENCE_NAME = 'io.harbor.GnomeIntegration';
const APP_ID = 'io.harbor.app';
// Matches #app's inset in src/styles.css.
const CARD_INSET = 7;
const LAUNCH_GRACE_US = 10 * GLib.USEC_PER_SEC;

// Stage pixels per CSS pixel: 1 in logical-pixel sessions, 2 on X11 or without
// scale-monitor-framebuffer at 200%.
function uiScale() {
    return St.ThemeContext.get_for_stage(global.stage).scale_factor;
}

function cardInset() {
    return CARD_INSET * uiScale();
}

function cardRect(frame) {
    const inset = cardInset();
    return {
        x: frame.x + inset,
        y: frame.y + inset,
        width: frame.width - 2 * inset,
        height: frame.height - 2 * inset,
    };
}

// Matched by the PID that owns harbor's D-Bus name, so it works whatever app id
// or title GTK reports; app id is the fallback before harbor's PID is known.
function isHarborWindow(win, harborPid) {
    if (harborPid && win.get_pid() === harborPid)
        return true;
    const wmClass = (win.get_wm_class() ?? '').toLowerCase();
    const appId = win.get_gtk_application_id() ?? '';
    return wmClass === 'harbor' || wmClass === APP_ID || appId === APP_ID;
}

function lookupHarborPid(callback) {
    Gio.DBus.session.call('org.freedesktop.DBus', '/org/freedesktop/DBus',
        'org.freedesktop.DBus', 'GetConnectionUnixProcessID', new GLib.Variant('(s)', [BUS_NAME]),
        null, Gio.DBusCallFlags.NONE, 1000, null, (connection, result) => {
            try {
                callback(connection.call_finish(result).deepUnpack()[0]);
            } catch {
                callback(0);
            }
        });
}

const HarborButton = GObject.registerClass(
class HarborButton extends PanelMenu.Button {
    _init(extension) {
        super._init(0.5, 'harbor', true);
        this._launchedAt = -LAUNCH_GRACE_US;
        this.add_child(new St.Icon({
            gicon: Gio.icon_new_for_string(extension.dir.get_child('harbor-symbolic.svg').get_path()),
            style_class: 'system-status-icon',
        }));
        // Register the dummy menu as a switch target. Emitting its open-state
        // signal would take a Shell modal grab away from the Wayland client.
        this.menu.open = () => this._open();
        this.menu.close = () => extension._hide();
        // Handle clicks and taps here: GNOME 50 disables PanelMenu.Button's own click
        // gesture for dummy menus, and 45–49 only call the dummy menu's no-op toggle().
        const toggle = () => {
            if (this.menu.isOpen)
                this.menu.close();
            else
                this.menu.open();
            return Clutter.EVENT_STOP;
        };
        this.connect('button-press-event', toggle);
        this.connect('touch-event', (_actor, event) =>
            event.type() === Clutter.EventType.TOUCH_BEGIN ? toggle() : Clutter.EVENT_PROPAGATE);
        // Hovering usually precedes a click: let harbor start its WebView now.
        this.connect('enter-event', () => {
            if (!this.menu.isOpen) {
                Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'Prepare',
                    null, null, Gio.DBusCallFlags.NO_AUTO_START, -1, null, null);
            }
            return Clutter.EVENT_PROPAGATE;
        });
    }

    setPopupOpen(open) {
        this.menu.isOpen = open;
        if (open)
            this.add_style_pseudo_class('active');
        else
            this.remove_style_pseudo_class('active');
    }

    _open() {
        if (this.menu.isOpen)
            return;
        Main.panel.menuManager.activeMenu?.close();
        this.setPopupOpen(true);
        Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'Show',
            null, null, Gio.DBusCallFlags.NO_AUTO_START, 2000, null, (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch {
                    // harbor isn't running: launch it once (a normal launch shows the popup).
                    // Clicks while it is still starting must not start more copies.
                    const now = GLib.get_monotonic_time();
                    if (!this.menu.isOpen || now - this._launchedAt < LAUNCH_GRACE_US)
                        return;
                    this._launchedAt = now;
                    const app = Shell.AppSystem.get_default().lookup_app('harbor.desktop');
                    if (app)
                        app.activate();
                    else
                        GLib.spawn_command_line_async('harbor');
                }
            });
    }

    closePopup() {
        if (!this.menu.isOpen)
            return;
        this.setPopupOpen(false);
        Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'Hide',
            null, null, Gio.DBusCallFlags.NO_AUTO_START, 2000, null, null);
    }
});

export default class HarborExtension extends Extension {
    enable() {
        this._button = new HarborButton(this);
        this._button.connect('destroy', () => (this._button = null));
        Main.panel.addToStatusArea(this.uuid, this._button);
        // Like a tray icon: the button only exists while harbor is running.
        this._button.container.hide();
        this._blur = new PanelBlur(cardInset, (active, colors) =>
            Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'SetBlur',
                new GLib.Variant('(bay)', [active, colors]), null,
                Gio.DBusCallFlags.NO_AUTO_START, 2000, null, null));
        this._presence = Gio.bus_own_name(Gio.BusType.SESSION, PRESENCE_NAME,
            Gio.BusNameOwnerFlags.NONE, null, () => this._blur.refresh(true), null);
        this._harborPid = 0;
        this._panelWorkHeight = null;
        this._harborWatch = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            () => {
                this._button?.container.show();
                lookupHarborPid(pid => {
                    if (this._button) {
                        this._harborPid = pid;
                        this._trackVisiblePopup();
                    }
                });
                this._blur.refresh(true);
            },
            () => {
                this._harborPid = 0;
                this._panelWorkHeight = null;
                this._button?.container.hide();
            });
        this._tracked = new Map();
        this._menuSources = new Set();
        this._mapped = global.window_manager.connect('map',
            (_wm, actor) => this._onMap(actor.get_meta_window()));
        const queuePopup = () => {
            if (this._popup)
                this._queuePlace(this._popup);
        };
        Main.layoutManager.connectObject('monitors-changed', queuePopup, this);
        global.display.connectObject('workareas-changed', queuePopup, this);
        this._panelActors = [Main.layoutManager.panelBox, Main.panel, this._button];
        for (const actor of this._panelActors) {
            actor.connectObject('notify::allocation', queuePopup,
                'notify::translation-x', queuePopup, 'notify::translation-y', queuePopup, this);
        }
        this._outsideClick = global.stage.connect('captured-event',
            (_stage, event) => this._onCapturedEvent(event));
        this._windowFocus = global.display.connect('notify::focus-window', () => {
            const focus = global.display.focus_window;
            if (this._popup && focus && focus !== this._popup)
                this._hide();
        });
        this._menuFocus = global.stage.connect('notify::key-focus', () => {
            const focus = global.stage.get_key_focus();
            // pushModal sets key focus before the panel manager updates activeMenu.
            if (focus && Main.panel.menuManager._menus.some(menu =>
                menu.isOpen && (menu.actor === focus || menu.actor.contains(focus))))
                this._hide();
        });

        // Mutter's skip-taskbar property is read-only. The Shell dash and Ubuntu
        // Dock both source their unpinned running icons from this list.
        this._injections = new InjectionManager();
        const apps = Shell.AppSystem.get_default();
        this._injections.overrideMethod(apps, 'get_running', original => () =>
            original.call(apps).filter(app => {
                const windows = app.get_windows();
                return !windows.length || !windows.every(win => isHarborWindow(win, this._harborPid));
            }));
        this._trackVisiblePopup();
    }

    disable() {
        global.window_manager.disconnect(this._mapped);
        Main.layoutManager.disconnectObject(this);
        global.display.disconnectObject(this);
        for (const actor of this._panelActors)
            actor.disconnectObject(this);
        this._panelActors = null;
        global.stage.disconnect(this._outsideClick);
        global.display.disconnect(this._windowFocus);
        global.stage.disconnect(this._menuFocus);
        this._stopMenuHover();
        this._injections.clear();
        for (const win of this._tracked.keys())
            this._untrack(win);
        Gio.bus_unwatch_name(this._harborWatch);
        Gio.bus_unown_name(this._presence);
        this._blur.destroy();
        this._button?.destroy();
        this._button = null;
    }

    _trackVisiblePopup() {
        // Shell may briefly reload us when another extension is toggled.
        for (const actor of global.get_window_actors()) {
            const win = actor.get_meta_window();
            if (actor.mapped && actor.visible && !this._tracked.has(win))
                this._onMap(win);
        }
    }

    _onMap(win) {
        if (!win || !isHarborWindow(win, this._harborPid))
            return;
        if (!this._tracked.has(win)) {
            const ids = [
                // Undo moves by anyone else (e.g. window-restoring extensions) at once:
                // a correction deferred to the next frame shows the popup misplaced for one frame.
                win.connect('size-changed', () => this._place(win)),
                win.connect('position-changed', () => this._place(win)),
                win.connect('shown', () => this._present(win)),
                win.connect('unmanaged', () => this._untrack(win)),
            ];
            this._tracked.set(win, {ids, pending: 0});
        }
        this._present(win);
    }

    _present(win) {
        this._popup = win;
        this._button?.setPopupOpen(true);
        // Place it before its first frame; layout changes re-place it via _queuePlace.
        this._place(win);
        this._blur.setWindow(win);
        this._trackMenuHover();
        if (!win.has_focus())
            Main.activateWindow(win);
    }

    _untrack(win) {
        const {ids, pending} = this._tracked.get(win);
        this._tracked.delete(win);
        if (pending)
            global.stage.disconnect(pending);
        for (const id of ids)
            win.disconnect(id);
        if (this._popup === win) {
            this._popup = null;
            this._blur.setWindow(null);
            this._stopMenuHover();
            this._button?.setPopupOpen(false);
        }
    }

    _queuePlace(win) {
        const state = this._tracked.get(win);
        const actor = win.get_compositor_private();
        if (state.pending || !actor?.visible)
            return;
        // Monitor events publish the new layout before Clutter allocates the
        // panel and button. Read their positions after layout, before painting.
        state.pending = global.stage.connect('before-paint', () => {
            global.stage.disconnect(state.pending);
            if (actor.visible)
                this._place(win);
            state.pending = 0;
        });
        actor.queue_redraw();
    }

    _hide() {
        this._popup = null;
        this._blur.setWindow(null);
        this._stopMenuHover();
        this._button?.closePopup();
    }

    _trackMenuHover() {
        if (!this._button)
            return;
        const manager = Main.panel.menuManager;
        for (const menu of manager._menus) {
            const source = menu.sourceActor;
            if (menu === this._button.menu || !source?.track_hover || this._menuSources.has(source))
                continue;
            this._menuSources.add(source);
            // Panel actors own hover even when the preceding pointer events
            // were routed to the Wayland popup instead of Shell's stage.
            source.connectObject('notify::hover', () => {
                if (this._popup && source.hover && !manager.activeMenu?.isOpen) {
                    this._hide();
                    menu.open();
                }
            }, 'destroy', () => this._menuSources.delete(source), this._button);
        }
    }

    _stopMenuHover() {
        if (this._button) {
            for (const source of this._menuSources)
                source.disconnectObject(this._button);
        }
        this._menuSources.clear();
    }

    _onCapturedEvent(event) {
        const type = event.type();
        if (!this._button?.menu.isOpen)
            return Clutter.EVENT_PROPAGATE;

        if (type === Clutter.EventType.BUTTON_PRESS || type === Clutter.EventType.TOUCH_BEGIN) {
            const [x, y] = event.get_coords();
            const [bx, by] = this._button.get_transformed_position();
            const [bw, bh] = this._button.get_transformed_size();
            const inside = (left, top, width, height) =>
                x >= left && x < left + width && y >= top && y < top + height;
            if (inside(bx, by, bw, bh))
                return Clutter.EVENT_PROPAGATE;
            const frame = this._popup?.get_frame_rect();
            if (frame) {
                const card = cardRect(frame);
                if (inside(card.x, card.y, card.width, card.height)) {
                    const radius = Math.min(this._blur.cornerRadius * uiScale(), card.width / 2, card.height / 2);
                    const dx = Math.max(0, radius - (x - card.x), x - (card.x + card.width - radius));
                    const dy = Math.max(0, radius - (y - card.y), y - (card.y + card.height - radius));
                    if (dx * dx + dy * dy <= radius * radius)
                        return Clutter.EVENT_PROPAGATE;
                }
            }
            this._hide();
        }
        return Clutter.EVENT_PROPAGATE;
    }

    _place(win) {
        if (!this._button)
            return;
        // The button, not Main.panel: extensions like Dash to Panel move it into
        // their own panel, which may sit at the bottom of the screen.
        const [buttonX, buttonY] = this._button.get_transformed_position();
        const [buttonWidth, buttonHeight] = this._button.get_transformed_size();
        const monitor = Main.layoutManager.findMonitorForActor(this._button);
        if (!monitor)
            return;
        const area = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        // harbor sizes its window in CSS pixels.
        const workHeight = Math.round(area.height / uiScale());
        if (workHeight > 0 && workHeight !== this._panelWorkHeight) {
            this._panelWorkHeight = workHeight;
            Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'SetPanelWorkHeight',
                new GLib.Variant('(u)', [workHeight]), null,
                Gio.DBusCallFlags.NO_AUTO_START, -1, null, null);
        }
        const frame = win.get_frame_rect();
        const {dateMenu, quickSettings} = Main.panel.statusArea;
        const menu = dateMenu?.menu?.actor ?? quickSettings?.menu?.actor;
        menu?.ensure_style();
        const theme = menu?.get_theme_node();
        const rise = theme?.get_length('-arrow-rise') ?? 0;
        const gap = theme?.get_length('-boxpointer-gap') ?? 0;
        const {width: cardWidth, height: cardHeight} = cardRect(frame);
        const inset = cardInset();
        const minX = area.x + rise;
        const maxX = Math.max(minX, area.x + area.width - cardWidth - rise);
        const cardX = Math.min(Math.max(buttonX + (buttonWidth - cardWidth) / 2, minX), maxX);
        const bottomPanel = buttonY + buttonHeight / 2 > monitor.y + monitor.height / 2;
        const cardY = bottomPanel
            ? Math.min(area.y + area.height, buttonY - gap - rise) - cardHeight
            : Math.max(area.y, buttonY + buttonHeight + gap + rise);
        const x = Math.round(cardX - inset);
        const y = Math.round(cardY - inset);
        if (frame.x !== x || frame.y !== y)
            win.move_frame(true, x, y);
        this._blur.position();
    }
}
