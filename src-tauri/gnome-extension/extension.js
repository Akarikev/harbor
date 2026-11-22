// harbor GNOME Shell integration: a top-bar button that opens harbor's popup
// directly under it. Apps can't place their own windows on GNOME Wayland, so the
// shell (this extension) positions harbor's window and keeps it pinned there.
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import GObject from 'gi://GObject';
import St from 'gi://St';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'io.harbor.Harbor';
const OBJECT_PATH = '/io/harbor/Harbor';
const INTERFACE = 'io.harbor.Harbor';
// Owned while the extension is enabled; harbor drops its tray icon when it sees it.
const PRESENCE_NAME = 'io.harbor.GnomeIntegration';
const APP_ID = 'io.harbor.app';
const GAP = 6;
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
        this.connect('button-press-event', () => {
            this._toggle();
            return true;
        });
    }

    _toggle() {
        Gio.DBus.session.call(BUS_NAME, OBJECT_PATH, INTERFACE, 'Toggle',
            null, null, Gio.DBusCallFlags.NO_AUTO_START, 2000, null, (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch {
                    // harbor isn't running: launch it once (a normal launch shows the popup).
                    // Clicks while it is still starting must not start more copies.
                    const now = GLib.get_monotonic_time();
                    if (now - this._launchedAt < LAUNCH_GRACE_US)
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
        // 'map' fires when the window actually appears, after Mutter's own initial
        // placement, so our position wins (window-created fires too early for that).
        this._mapped = global.window_manager.connect('map',
            (_wm, actor) => this._onMap(actor.get_meta_window()));
    }

    disable() {
        global.window_manager.disconnect(this._mapped);
        for (const ids of this._tracked.values()) {
            for (const [object, id] of ids)
                object.disconnect(id);
        }
        this._tracked.clear();
        Gio.bus_unwatch_name(this._harborWatch);
        Gio.bus_unown_name(this._presence);
        this._button.destroy();
        this._button = null;
    }

    _onMap(win) {
        if (!win || !isHarborWindow(win, this._harborPid))
            return;
        if (!this._tracked.has(win)) {
            // Harbor grows/shrinks with its content: keep the top pinned under the button.
            const ids = [[win, win.connect('size-changed', () => this._place(win))]];
            ids.push([win, win.connect('unmanaged', () => {
                for (const [object, id] of ids)
                    object.disconnect(id);
                this._tracked.delete(win);
            })]);
            this._tracked.set(win, ids);
        }
        this._place(win);
        Main.activateWindow(win);
    }

    _place(win) {
        if (!this._button)
            return;
        const [buttonX] = this._button.get_transformed_position();
        const buttonWidth = this._button.get_width();
        const monitor = Main.layoutManager.findIndexForActor(this._button);
        const area = Main.layoutManager.getWorkAreaForMonitor(monitor);
        const frame = win.get_frame_rect();
        const minX = area.x + GAP;
        const maxX = Math.max(minX, area.x + area.width - frame.width - GAP);
        const x = Math.round(Math.min(Math.max(buttonX + buttonWidth / 2 - frame.width / 2, minX), maxX));
        const y = area.y + GAP;
        if (frame.x !== x || frame.y !== y)
            win.move_frame(true, x, y);
    }
}
