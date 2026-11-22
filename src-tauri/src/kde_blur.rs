use gtk::{gdk, gdk_pixbuf::PixbufLoader, gio, glib, prelude::*};
use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    rc::Rc,
};
use tauri::AppHandle;
use wayland_client::{
    backend::{Backend, ObjectId},
    delegate_noop,
    globals::{registry_queue_init, GlobalList, GlobalListContents},
    protocol::{wl_compositor::WlCompositor, wl_region::WlRegion, wl_registry, wl_surface::WlSurface},
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::ext::background_effect::v1::client::{
    ext_background_effect_manager_v1::{self as manager, ExtBackgroundEffectManagerV1},
    ext_background_effect_surface_v1::ExtBackgroundEffectSurfaceV1,
};
use wayland_protocols_plasma::blur::client::{
    org_kde_kwin_blur::OrgKdeKwinBlur, org_kde_kwin_blur_manager::OrgKdeKwinBlurManager,
};
use wayland_protocols_plasma::contrast::client::{
    org_kde_kwin_contrast::OrgKdeKwinContrast,
    org_kde_kwin_contrast_manager::OrgKdeKwinContrastManager,
};

use crate::popup::{set_blur_palette, BlurPalette};

// Match the inset and radius of #app in styles.css.
const INSET: i32 = 7;
const RADIUS: i32 = 13;

pub(super) fn install(app: &AppHandle, window: &gtk::ApplicationWindow) {
    if !window.display().backend().is_wayland()
        || !std::env::var("XDG_CURRENT_DESKTOP")
            .is_ok_and(|desktop| desktop.split(':').any(|name| name == "KDE"))
    {
        return;
    }

    let effect = Rc::new(RefCell::new(None::<SurfaceBlur>));
    let mapped = effect.clone();
    let handle = app.clone();
    window.connect_map(move |window| {
        let Some((palette, contrast)) = plasma_appearance() else {
            return;
        };
        let Some(native) = window.window() else {
            return;
        };
        let attached = WAYLAND.with(|cell| {
            let protocol = cell.get_or_init(|| Wayland::new(window.display()));
            protocol.as_ref()?.attach(&native, contrast)
        });
        if let Some(mut attached) = attached {
            let (width, height) = window.size();
            attached.resize(width, height);
            *mapped.borrow_mut() = Some(attached);
            set_blur_palette(&handle, Some(palette));
            window.queue_draw();
        }
    });
    let allocated = effect.clone();
    window.connect_size_allocate(move |_, allocation| {
        if let Some(effect) = allocated.borrow_mut().as_mut() {
            effect.resize(allocation.width(), allocation.height());
        }
    });
    let handle = app.clone();
    window.connect_unmap(move |_| {
        if effect.borrow_mut().take().is_some() {
            set_blur_palette(&handle, None);
        }
    });
}

fn key_file(path: &Path) -> Option<glib::KeyFile> {
    let file = glib::KeyFile::new();
    file.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    Some(file)
}

fn config(name: &str) -> impl Iterator<Item = glib::KeyFile> + '_ {
    std::iter::once(glib::user_config_dir())
        .chain(glib::system_config_dirs())
        .filter_map(move |dir| key_file(&dir.join(name)))
}

fn setting(file: &str, group: &str, key: &str) -> Option<String> {
    config(file).find_map(|file| file.string(group, key).ok().map(Into::into))
}

fn theme_dir(name: &str) -> Option<PathBuf> {
    if Path::new(name).file_name()?.to_str()? != name || name == ".." {
        return None;
    }
    std::iter::once(glib::user_data_dir())
        .chain(glib::system_data_dirs())
        .map(|dir| dir.join("plasma/desktoptheme").join(name))
        .find(|path| path.is_dir())
}

fn color(value: &str) -> Option<[u8; 3]> {
    let mut channels = value.split(',').map(|channel| channel.trim().parse().ok());
    Some([channels.next()??, channels.next()??, channels.next()??])
}

fn plasma_appearance() -> Option<(BlurPalette, Option<[f64; 3]>)> {
    if setting("kwinrc", "Plugins", "blurEnabled").is_some_and(|enabled| enabled != "true") {
        return None;
    }
    let name = setting("plasmarc", "Theme", "name").unwrap_or_else(|| "default".into());
    let default = theme_dir("default").or_else(|| theme_dir("breeze"));
    let theme = theme_dir(&name).or_else(|| default.clone())?;
    let settings =
        key_file(&theme.join("plasmarc")).or_else(|| key_file(&theme.join("metadata.desktop")));
    if settings
        .as_ref()
        .and_then(|file| file.boolean("BlurBehindEffect", "enabled").ok())
        == Some(false)
    {
        return None;
    }
    let fallback = settings
        .as_ref()
        .and_then(|file| file.string("Settings", "FallbackTheme").ok())
        .and_then(|name| theme_dir(&name));
    // Plasma selects the translucent asset while KWin advertises blur.
    let svg = [Some(theme.clone()), fallback, default]
        .into_iter()
        .flatten()
        .flat_map(|theme| ["translucent/dialogs", "dialogs"].into_iter().map(move |dir| theme.join(dir)))
        .flat_map(|dir| ["background.svgz", "background.svg"].into_iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())?;
    let colors = key_file(&theme.join("colors"));
    let scheme = setting("kdeglobals", "General", "ColorScheme")
        .filter(|name| {
            Path::new(name).file_name().and_then(|name| name.to_str()) == Some(name.as_str())
        })
        .and_then(|name| {
            let filename = format!("{}.colors", name.replace(' ', ""));
            std::iter::once(glib::user_data_dir())
                .chain(glib::system_data_dirs())
                .find_map(|dir| key_file(&dir.join("color-schemes").join(&filename)))
        });
    let lookup = |group, key| {
        colors
            .as_ref()
            .and_then(|file| file.string(group, key).ok().map(Into::into))
            .or_else(|| setting("kdeglobals", group, key))
            .or_else(|| scheme.as_ref()?.string(group, key).ok().map(Into::into))
            .and_then(|value| color(&value))
    };
    let role = |group| lookup(group, "BackgroundNormal");
    let panel = dialog_color(&svg, role("Colors:Window"))?;
    let bar = role("Colors:Header").map_or(panel, |[r, g, b]| [r, g, b, panel[3]]);
    // The user's colour scheme text colour, like the GNOME path's theme foreground.
    let text = lookup("Colors:Window", "ForegroundNormal").map(|[r, g, b]| [r, g, b, 255]);
    let contrast = settings.as_ref().and_then(|file| {
        let group = "ContrastEffect";
        if !file.boolean(group, "enabled").unwrap_or(false)
            || setting("kwinrc", "Plugins", "contrastEnabled").as_deref() == Some("false")
        {
            return None;
        }
        let values = ["contrast", "intensity", "saturation"]
            .map(|key| file.double(group, key).unwrap_or(1.0));
        values
            .iter()
            .all(|value| value.is_finite() && (0.0..=10.0).contains(value))
            .then_some(values)
    });
    Some((BlurPalette { panel, bar, text, corner_radius: None }, contrast))
}

fn dialog_color(path: &Path, theme_color: Option<[u8; 3]>) -> Option<[u8; 4]> {
    let stream = gio::File::for_path(path).read(gio::Cancellable::NONE).ok()?;
    let mut svg = String::new();
    if path.extension()?.to_str()? == "svgz" {
        let gzip = gio::ZlibDecompressor::new(gio::ZlibCompressorFormat::Gzip);
        gio::ConverterInputStream::new(&stream, &gzip)
            .into_read()
            .take(2_000_000)
            .read_to_string(&mut svg)
            .ok()?;
    } else {
        stream.into_read().take(2_000_000).read_to_string(&mut svg).ok()?;
    }
    let mut style =
        String::from("<style>svg > :not(defs):not(style):not(#center) { display: none !important; }");
    if let Some([r, g, b]) = theme_color {
        style.push_str(&format!(".ColorScheme-Background {{ color: #{r:02x}{g:02x}{b:02x} !important; }}"));
    }
    style.push_str("</style>");
    svg.insert_str(svg.rfind("</svg>")?, &style);

    // Render the FrameSVG center tile, including its alpha and any layered fills.
    let loader = PixbufLoader::with_type("svg").ok()?;
    loader.set_size(256, 256);
    loader.write(svg.as_bytes()).ok()?;
    loader.close().ok()?;
    let pixels = loader.pixbuf()?;
    let bytes = pixels.read_pixel_bytes();
    let stride = pixels.rowstride() as usize;
    let channels = pixels.n_channels() as usize;
    let mut colors = HashMap::new();
    for row in bytes.as_ref().chunks(stride) {
        for pixel in row.chunks_exact(channels).take(pixels.width() as usize) {
            let [red, green, blue, ..] = *pixel else {
                continue;
            };
            let alpha = pixel.get(3).copied().unwrap_or(255);
            if alpha != 0 {
                *colors.entry([red, green, blue, alpha]).or_insert(0) += 1;
            }
        }
    }
    colors.into_iter().max_by_key(|(_, count)| *count).map(|(color, _)| color)
}

thread_local! {
    // One registry per GTK display. Older compositors cannot destroy a registry.
    static WAYLAND: OnceCell<Option<Wayland>> = const { OnceCell::new() };
}

struct Wayland {
    connection: Connection,
    globals: GlobalList,
    queue: RefCell<EventQueue<Capabilities>>,
    compositor: WlCompositor,
    legacy: RefCell<Option<(u32, OrgKdeKwinBlurManager)>>,
    contrast: RefCell<Option<(u32, OrgKdeKwinContrastManager)>>,
    _display: gdk::Display,
}

impl Wayland {
    fn new(display: gdk::Display) -> Option<Self> {
        // SAFETY: this runs on GTK's thread on a Wayland display. We retain the
        // GdkDisplay, and the foreign backend leaves its C objects owned by GDK.
        let connection = unsafe {
            let native = gdk_wayland_sys::gdk_wayland_display_get_wl_display(display.as_ptr().cast());
            if native.is_null() {
                return None;
            }
            Connection::from_backend(Backend::from_foreign_display(native.cast()))
        };
        let (globals, queue) = registry_queue_init(&connection).ok()?;
        let compositor = unsafe {
            let native = gdk_wayland_sys::gdk_wayland_display_get_wl_compositor(display.as_ptr().cast());
            if native.is_null() {
                return None;
            }
            let id = ObjectId::from_ptr(WlCompositor::interface(), native.cast()).ok()?;
            WlCompositor::from_id(&connection, id).ok()?
        };
        Some(Self {
            connection,
            globals,
            queue: RefCell::new(queue),
            compositor,
            legacy: RefCell::new(None),
            contrast: RefCell::new(None),
            _display: display,
        })
    }

    fn attach(&self, window: &gdk::Window, contrast: Option<[f64; 3]>) -> Option<SurfaceBlur> {
        // SAFETY: the surface is borrowed only during this map callback, on GTK's thread.
        let surface = unsafe {
            let native = gdk_wayland_sys::gdk_wayland_window_get_wl_surface(window.as_ptr().cast());
            if native.is_null() {
                return None;
            }
            let id = ObjectId::from_ptr(WlSurface::interface(), native.cast()).ok()?;
            WlSurface::from_id(&self.connection, id).ok()?
        };
        let mut queue = self.queue.borrow_mut();
        let mut caps = Capabilities::default();
        queue.roundtrip(&mut caps).ok()?;
        let qh = queue.handle();
        let effect = if let Ok(manager) = self
            .globals
            .bind::<ExtBackgroundEffectManagerV1, _, _>(&qh, 1..=1, ())
        {
            let ready = queue.roundtrip(&mut caps).is_ok() && caps.blur;
            let effect = ready.then(|| ProtocolBlur::Ext(manager.get_background_effect(&surface, &qh, ())));
            manager.destroy();
            effect?
        } else {
            let manager = self.bind_cached(&self.legacy, &qh)?;
            ProtocolBlur::Kde(manager.create(&surface, &qh, ()))
        };
        let contrast = contrast.and_then(|[contrast, intensity, saturation]| {
            let manager = self.bind_cached(&self.contrast, &qh)?;
            let effect = manager.create(&surface, &qh, ());
            effect.set_contrast(contrast);
            effect.set_intensity(intensity);
            effect.set_saturation(saturation);
            Some(effect)
        });
        Some(SurfaceBlur {
            effect,
            contrast,
            compositor: self.compositor.clone(),
            qh,
            size: (0, 0),
        })
    }

    fn bind_cached<P: Proxy + 'static>(
        &self,
        cache: &RefCell<Option<(u32, P)>>,
        qh: &QueueHandle<Capabilities>,
    ) -> Option<P>
    where
        Capabilities: Dispatch<P, ()>,
    {
        let name = self.globals.contents().with_list(|globals| {
            globals.iter().find(|global| global.interface == P::interface().name)
                .map(|global| global.name)
        });
        let mut cache = cache.borrow_mut();
        if let Some((old_name, proxy)) = cache.as_ref() {
            if Some(*old_name) == name {
                return Some(proxy.clone());
            }
            let _ = self.connection.backend().destroy_object(&proxy.id());
            *cache = None;
        }
        let name = name?;
        let proxy = self.globals.bind::<P, _, _>(qh, 1..=1, ()).ok()?;
        *cache = Some((name, proxy.clone()));
        Some(proxy)
    }
}

#[derive(Default)]
struct Capabilities {
    blur: bool,
}

impl Dispatch<ExtBackgroundEffectManagerV1, ()> for Capabilities {
    fn event(state: &mut Self, _: &ExtBackgroundEffectManagerV1, event: manager::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let manager::Event::Capabilities { flags: WEnum::Value(flags) } = event {
            state.blur = flags.contains(manager::Capability::Blur);
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Capabilities {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event,
             _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(Capabilities: ignore WlCompositor);
delegate_noop!(Capabilities: ignore WlRegion);
delegate_noop!(Capabilities: ignore ExtBackgroundEffectSurfaceV1);
delegate_noop!(Capabilities: ignore OrgKdeKwinBlurManager);
delegate_noop!(Capabilities: ignore OrgKdeKwinBlur);
delegate_noop!(Capabilities: ignore OrgKdeKwinContrastManager);
delegate_noop!(Capabilities: ignore OrgKdeKwinContrast);

enum ProtocolBlur {
    Ext(ExtBackgroundEffectSurfaceV1),
    Kde(OrgKdeKwinBlur),
}

struct SurfaceBlur {
    effect: ProtocolBlur,
    contrast: Option<OrgKdeKwinContrast>,
    compositor: WlCompositor,
    qh: QueueHandle<Capabilities>,
    size: (i32, i32),
}

impl SurfaceBlur {
    fn resize(&mut self, width: i32, height: i32) {
        if self.size == (width, height) || width <= 2 * INSET || height <= 2 * INSET {
            return;
        }
        self.size = (width, height);
        let region = self.compositor.create_region(&self.qh, ());
        let (w, h) = (width - 2 * INSET, height - 2 * INSET);
        let radius = RADIUS.min(w / 2).min(h / 2);
        region.add(INSET, INSET + radius, w, h - 2 * radius);
        for row in 0..radius {
            let dy = f64::from(radius - row) - 0.5;
            let edge = radius - (f64::from(radius * radius) - dy * dy).sqrt().floor() as i32;
            region.add(INSET + edge, INSET + row, w - 2 * edge, 1);
            region.add(INSET + edge, INSET + h - 1 - row, w - 2 * edge, 1);
        }
        match &self.effect {
            ProtocolBlur::Ext(effect) => effect.set_blur_region(Some(&region)),
            ProtocolBlur::Kde(effect) => {
                effect.set_region(Some(&region));
                effect.commit();
            }
        }
        if let Some(contrast) = &self.contrast {
            contrast.set_region(Some(&region));
            contrast.commit();
        }
        region.destroy();
    }
}

impl Drop for SurfaceBlur {
    fn drop(&mut self) {
        match &self.effect {
            ProtocolBlur::Ext(effect) => effect.destroy(),
            ProtocolBlur::Kde(effect) => effect.release(),
        }
        if let Some(contrast) = &self.contrast {
            contrast.release();
        }
    }
}
