import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {ExtensionState} from 'resource:///org/gnome/shell/misc/extensionUtils.js';

const BLUR_UUID = 'blur-my-shell@aunetx';
const SCHEMA = 'org.gnome.shell.extensions.blur-my-shell.popup';
const OPT_IN = 'bms-popup-blur-target';
const APPLIED = 'bms-popup-blurred';
const SECTIONS = ['quick-settings-grid', 'datemenu-calendar-column'];
const DEFAULT_RADIUS = 13; // Matches --card-radius in styles.css.

function themeNode(actor) {
    actor.ensure_style();
    return actor.get_theme_node();
}

function rgba({red, green, blue, alpha}) {
    return [red, green, blue, alpha];
}

function sectionBackground(actor, nodeFor) {
    for (const child of actor.get_children()) {
        if (child instanceof St.Widget && SECTIONS.some(name => child.has_style_class_name(name))) {
            const color = rgba(nodeFor(child).get_background_color());
            if (color[3])
                return color;
        }
        const color = sectionBackground(child, nodeFor);
        if (color)
            return color;
    }
    return null;
}

function menuColors(menu, nodeFor = themeNode) {
    const node = nodeFor(menu);
    const panel = rgba(node.get_background_color());
    return [...panel, ...(sectionBackground(menu, nodeFor) ?? panel), ...rgba(node.get_foreground_color())];
}

function stylesheetScheme(file) {
    const name = file?.get_basename();
    const explicit = name?.match(/-(light|dark)\.css$/)?.[1];
    if (explicit)
        return explicit;
    if (name === 'gnome-shell.css') {
        const directory = file.get_parent()?.get_basename();
        if (directory === 'Yaru' || directory === 'Yaru-dark')
            return directory === 'Yaru' ? 'light' : 'dark';
    }
    return null;
}

function pairedStylesheet(file, scheme) {
    const parent = file?.get_parent();
    if (!parent)
        return null;
    const name = file.get_basename();
    const sibling = parent.get_child(name.replace(/(?:-(?:light|dark))?\.css$/, `-${scheme}.css`));
    if (!sibling.equal(file) && sibling.query_exists(null))
        return sibling;

    // Ubuntu's bundled Yaru light stylesheet is named gnome-shell.css.
    const directory = parent.get_basename();
    if (name.startsWith('gnome-shell') && ['Yaru', 'Yaru-dark'].includes(directory)) {
        const bundledLight = parent.get_child('gnome-shell.css');
        if (scheme === 'light' && directory === 'Yaru' && bundledLight.query_exists(null))
            return bundledLight;
        const legacy = Gio.File.new_for_path(GLib.build_filenamev([
            global.datadir, 'theme', scheme === 'light' ? 'Yaru' : 'Yaru-dark', 'gnome-shell.css',
        ]));
        if (legacy.query_exists(null))
            return legacy;
    }
    return null;
}

function otherTheme(live, scheme, bms) {
    const application = pairedStylesheet(live.application_stylesheet, scheme);
    if (live.application_stylesheet && !application)
        return null;
    const defaults = pairedStylesheet(live.default_stylesheet, scheme)
        ?? (application ? live.default_stylesheet : null);
    if (!defaults)
        return null;
    const theme = new St.Theme({application_stylesheet: application, default_stylesheet: defaults});
    if (bms)
        theme.load_stylesheet(pairedStylesheet(bms, scheme) ?? bms);
    return theme;
}

function alternateNode(context, theme, scheme, adaptive, node, nodes) {
    if (!node)
        return null;
    if (!nodes.has(node)) {
        const parent = alternateNode(context, theme, scheme, adaptive, node.get_parent(), nodes);
        const classes = node.get_element_classes()?.map(name => adaptive
            ? name.replace(/^(bms-popup-(?:background|surface)-)(?:light|dark)$/, `$1${scheme}`)
            : name).join(' ');
        nodes.set(node, St.ThemeNode.new(context, parent, theme, node.get_element_type(),
            node.get_element_id(), classes ?? null, node.get_pseudo_classes()?.join(' ') ?? null, ''));
    }
    return nodes.get(node);
}

export class PanelBlur {
    constructor(inset, publish) {
        this._inset = inset;
        this._publish = publish;
        const {quickSettings, dateMenu} = Main.panel.statusArea;
        this._menus = [
            [quickSettings?.menu?.box, 'blur-quick-settings'],
            [dateMenu?.menu?.box, 'blur-calendar'],
        ].filter(([actor]) => actor);
        this._signals = this._menus.map(([actor]) =>
            [actor, actor.connect('style-changed', () => this.refresh())]);
        const theme = St.ThemeContext.get_for_stage(global.stage);
        this._signals.push([theme, theme.connect('changed', () => this.refresh())]);
        const shellSettings = St.Settings.get();
        this._signals.push([shellSettings, shellSettings.connect('notify::color-scheme', () => this.refresh())]);
        const manager = Main.extensionManager;
        this._signals.push([manager, manager.connect('extension-state-changed', (_manager, extension) => {
            if (extension.uuid === BLUR_UUID)
                this._watchSettings();
        })]);
        this._watchSettings();
    }

    _watchSettings() {
        if (this._settings)
            this._settings.disconnect(this._settingsId);
        this._settings = null;
        this._alternate = null;
        this._extension = Main.extensionManager.lookup(BLUR_UUID);
        const extension = this._extension;
        if (extension?.state === (ExtensionState.ACTIVE ?? ExtensionState.ENABLED)) {
            const schemas = extension.dir.get_child('schemas');
            const defaults = Gio.SettingsSchemaSource.get_default();
            const source = schemas.get_child('gschemas.compiled').query_exists(null)
                ? Gio.SettingsSchemaSource.new_from_directory(schemas.get_path(), defaults, false)
                : defaults;
            const schema = source.lookup(SCHEMA, true);
            if (schema) {
                this._settings = new Gio.Settings({settings_schema: schema});
                this._settingsId = this._settings.connect('changed', () => this.refresh());
            }
        }
        this.refresh();
    }

    setWindow(window) {
        if (window !== this._window) {
            this._destroyTarget();
            this._window = window;
        }
        this.position();
        this.refresh();
    }

    position() {
        if (!this._target)
            return;
        const frame = this._window.get_frame_rect();
        const buffer = this._window.get_buffer_rect();
        const inset = this._inset;
        this._target.set_position(frame.x - buffer.x + inset, frame.y - buffer.y + inset);
        this._target.set_size(Math.max(1, frame.width - 2 * inset), Math.max(1, frame.height - 2 * inset));
    }

    get cornerRadius() {
        return this._radius ?? DEFAULT_RADIUS;
    }

    refresh(force = false) {
        if (force)
            this._last = null;
        if (this._idle)
            return;
        this._idle = GLib.idle_add(GLib.PRIORITY_DEFAULT_IDLE, () => {
            this._idle = 0;
            this._sync();
            return GLib.SOURCE_REMOVE;
        });
    }

    _sync() {
        const actor = this._window?.get_compositor_private();
        const settings = this._settings;
        const enabled = key => !settings.settings_schema.has_key(key) || settings.get_boolean(key);
        const popup = this._extension?.stateObj?._popup ?? global.blur_my_shell?._popup;
        const nativePipeline = typeof popup?.targets?.is_opt_in === 'function';
        const menu = settings && settings.get_boolean('blur') && enabled('blur-menus')
            && (!nativePipeline || popup.enabled)
            ? this._menus.find(([, key]) => enabled(key))?.[0] : null;
        if (!actor || !menu) {
            this._destroyTarget();
            this._send(false, []);
            return;
        }

        if (this._window.blur_actor || [actor, ...actor.get_children()].some(child =>
            child !== this._target && child.name !== 'bms-popup-blurred-widget' &&
            child.get_effects().some(effect => effect instanceof Shell.BlurEffect ||
                effect.get_name()?.toLowerCase().includes('blur')))) {
            this._destroyTarget();
            this._send(true, []);
            return;
        }

        if (this._target && this._target.has_style_class_name(OPT_IN) !== nativePipeline)
            this._destroyTarget();
        if (!this._target) {
            // Modern Blur my Shell renders its own configured pipeline on opt-in
            // surfaces, including effects a Gaussian Shell.BlurEffect cannot copy.
            this._target = new St.Widget({
                style_class: nativePipeline ? OPT_IN : '',
                reactive: false,
            });
            this._target.connect('style-changed', () => this.refresh());
            this._target.connect('destroy', target => {
                if (this._target === target)
                    this._target = null;
            });
            if (!nativePipeline) {
                this._target.add_effect(new Shell.BlurEffect({mode: Shell.BlurMode.BACKGROUND}));
            }
            actor.insert_child_at_index(this._target, 0);
            this.position();
            if (nativePipeline)
                popup.queue_try_blur?.(this._target);
        }

        const radius = settings.settings_schema.has_key('menu-corner-radius')
            ? Math.max(0, Math.min(255, settings.get_int('menu-corner-radius'))) : DEFAULT_RADIUS;
        const style = `border-radius: ${radius}px; background-color: transparent;`;
        if (this._target.get_style() !== style)
            this._target.set_style(style);

        const [effect] = this._target.get_effects();
        if (!nativePipeline && effect) {
            effect.radius = settings.get_int('sigma') * 2;
            effect.brightness = settings.get_double('brightness');
        }
        const active = !nativePipeline || this._target.has_style_class_name(APPLIED);
        if (active)
            this._send(true, this._palettes(menu, settings, radius), radius);
        else
            this._send(false, []);
    }

    _palettes(menu, settings, radius) {
        const context = St.ThemeContext.get_for_stage(global.stage);
        const live = context.get_theme();
        const variant = stylesheetScheme(live.application_stylesheet ?? live.default_stylesheet)
            ?? Main.getStyleVariant?.();
        const dark = ['light', 'dark'].includes(variant) ? variant === 'dark'
            : St.Settings.get().color_scheme === St.SystemColorScheme.PREFER_DARK;
        const current = [Number(dark), radius, ...menuColors(menu)];
        const other = dark ? 'light' : 'dark';
        const stylesheet = this._extension.stylesheet;
        const cached = this._alternate;
        if (cached?.live !== live || cached.scheme !== other || cached.stylesheet !== stylesheet) {
            let theme = null;
            try {
                theme = otherTheme(live, other, stylesheet);
            } catch (error) {
                console.warn(`harbor: alternate shell theme unavailable: ${error.message}`);
            }
            this._alternate = {live, scheme: other, stylesheet, theme};
        }
        const {theme} = this._alternate;
        if (!theme)
            return current;

        // BMS's automatic override keeps the same popup selectors but changes
        // the light/dark classes on their ancestors. Fixed overrides stay fixed.
        const adaptive = settings.settings_schema.has_key('style-popup')
            && ![0, 1, 2].includes(settings.get_int('style-popup'));
        const nodes = new Map();
        const nodeFor = actor => alternateNode(context, theme, other, adaptive, themeNode(actor), nodes);
        return [...current, ...menuColors(menu, nodeFor)];
    }

    _send(active, colors, radius) {
        this._radius = radius ?? null;
        const key = [active, ...colors].join();
        if (this._last === key)
            return;
        this._last = key;
        this._publish(active, colors);
    }

    _destroyTarget() {
        const target = this._target;
        this._target = null;
        target?.destroy();
    }

    destroy() {
        for (const [source, id] of this._signals)
            source.disconnect(id);
        if (this._settings)
            this._settings.disconnect(this._settingsId);
        this._destroyTarget();
        if (this._idle)
            GLib.Source.remove(this._idle);
    }
}
