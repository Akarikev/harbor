// harbor GNOME Shell integration: a top-bar button that opens harbor's popup
// directly under it. Apps can't place their own windows on GNOME Wayland, so the
// shell (this extension) positions harbor's window and keeps it pinned there.
import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import Meta from 'gi://Meta';
import St from 'gi://St';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import {Extension, InjectionManager} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'io.harbor.Harbor';
const OBJECT_PATH = '/io/harbor/Harbor';
const INTERFACE = 'io.harbor.Harbor';
// Owned while the extension is enabled; harbor drops its tray icon when it sees it.
const PRESENCE_NAME = 'io.harbor.GnomeIntegration';
const APP_ID = 'io.harbor.app';
// Matches #app's inset in src/styles.css.
const CARD_INSET = 7;
const LAUNCH_GRACE_US = 10 * GLib.USEC_PER_SEC;

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
        this.connect('button-press-event', () => {
            if (this.menu.isOpen)
                this.menu.close();
            else
                this.menu.open();
            return true;
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
        Main.panel.addToStatusArea(this.uuid, this._button);
        this._presence = Gio.bus_own_name(Gio.BusType.SESSION, PRESENCE_NAME,
            Gio.BusNameOwnerFlags.NONE, null, null, null);
        this._harborPid = 0;
        this._harborWatch = Gio.bus_watch_name(Gio.BusType.SESSION, BUS_NAME,
            Gio.BusNameWatcherFlags.NONE,
            () => lookupHarborPid(pid => (this._harborPid = pid)),
            () => (this._harborPid = 0));
        this._tracked = new Map();
        this._laters = global.compositor?.get_laters?.();
        this._mapped = global.window_manager.connect('map',
            (_wm, actor) => this._onMap(actor.get_meta_window()));
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
    }

    disable() {
        global.window_manager.disconnect(this._mapped);
        global.stage.disconnect(this._outsideClick);
        global.display.disconnect(this._windowFocus);
        global.stage.disconnect(this._menuFocus);
        this._injections.clear();
        for (const win of this._tracked.keys())
            this._untrack(win);
        Gio.bus_unwatch_name(this._harborWatch);
        Gio.bus_unown_name(this._presence);
        this._button.destroy();
        this._button = null;
    }

    _onMap(win) {
        if (!win || !isHarborWindow(win, this._harborPid))
            return;
        if (!this._tracked.has(win)) {
            const ids = [
                win.connect('size-changed', () => this._queuePlace(win)),
                win.connect('position-changed', () => this._queuePlace(win)),
                win.connect('shown', () => this._present(win)),
                win.connect('unmanaged', () => this._untrack(win)),
            ];
            this._tracked.set(win, {ids, pending: 0});
        }
        this._present(win);
    }

    _present(win) {
        this._popup = win;
        this._button.setPopupOpen(true);
        this._place(win);
        if (!win.has_focus())
            Main.activateWindow(win);
    }

    _untrack(win) {
        const {ids, pending} = this._tracked.get(win);
        this._tracked.delete(win);
        if (pending) {
            if (this._laters)
                this._laters.remove(pending);
            else
                Meta.later_remove(pending);
        }
        for (const id of ids)
            win.disconnect(id);
        if (this._popup === win) {
            this._popup = null;
            this._button.setPopupOpen(false);
        }
    }

    _queuePlace(win) {
        const state = this._tracked.get(win);
        if (state.pending || !win.get_compositor_private()?.visible)
            return;
        // A resize or a window-placement extension can move the window after
        // map. Re-pin it before the next frame without re-entering move_frame.
        const place = () => {
            if (win.get_compositor_private()?.visible)
                this._place(win);
            state.pending = 0;
            return GLib.SOURCE_REMOVE;
        };
        state.pending = this._laters
            ? this._laters.add(Meta.LaterType.BEFORE_REDRAW, place)
            : Meta.later_add(Meta.LaterType.BEFORE_REDRAW, place);
    }

    _hide() {
        this._popup = null;
        this._button.closePopup();
    }

    _onCapturedEvent(event) {
        const type = event.type();
        if (!this._button.menu.isOpen)
            return Clutter.EVENT_PROPAGATE;

        if (type === Clutter.EventType.MOTION) {
            const manager = Main.panel.menuManager;
            if (!manager.activeMenu) {
                // With no modal grab the manager doesn't get the crossing events.
                const target = global.stage.get_event_actor(event);
                const menu = manager._findMenuForSource(target);
                if (menu && menu !== this._button.menu) {
                    this._hide();
                    manager._changeMenu(menu);
                }
            }
        } else if (type === Clutter.EventType.BUTTON_PRESS || type === Clutter.EventType.TOUCH_BEGIN) {
            const [x, y] = event.get_coords();
            const [bx, by] = this._button.get_transformed_position();
            const [bw, bh] = this._button.get_transformed_size();
            const f = this._popup?.get_frame_rect();
            const inside = (left, top, width, height) =>
                x >= left && x < left + width && y >= top && y < top + height;
            if (!inside(bx, by, bw, bh) &&
                !(f && inside(f.x + CARD_INSET, f.y + CARD_INSET,
                    f.width - 2 * CARD_INSET, f.height - 2 * CARD_INSET)))
                this._hide();
        }
        return Clutter.EVENT_PROPAGATE;
    }

    _place(win) {
        if (!this._button)
            return;
        const [buttonX] = this._button.get_transformed_position();
        const [buttonWidth] = this._button.get_transformed_size();
        const [, panelY] = Main.panel.get_transformed_position();
        const [, panelHeight] = Main.panel.get_transformed_size();
        const monitor = Main.layoutManager.findIndexForActor(this._button);
        const area = Main.layoutManager.getWorkAreaForMonitor(monitor);
        const frame = win.get_frame_rect();
        const {dateMenu, quickSettings} = Main.panel.statusArea;
        const menu = dateMenu?.menu?.actor ?? quickSettings?.menu?.actor;
        menu?.ensure_style();
        const theme = menu?.get_theme_node();
        const rise = theme?.get_length('-arrow-rise') ?? 0;
        const gap = theme?.get_length('-boxpointer-gap') ?? 0;
        const cardWidth = frame.width - 2 * CARD_INSET;
        const minX = area.x + rise;
        const maxX = Math.max(minX, area.x + area.width - cardWidth - rise);
        const cardX = Math.min(Math.max(buttonX + (buttonWidth - cardWidth) / 2, minX), maxX);
        const x = Math.round(cardX - CARD_INSET);
        const y = Math.round(Math.max(area.y, panelY + panelHeight + gap + rise) - CARD_INSET);
        if (frame.x !== x || frame.y !== y)
            win.move_frame(true, x, y);
    }
}
