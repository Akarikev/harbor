use gtk::{gdk, gio, glib, pango, prelude::*};
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::{
    env,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, EventLoopMessage, LogicalSize, Manager, PhysicalPosition, PhysicalRect, WebviewWindow, WebviewWindowBuilder};
use tauri_runtime_wry::tao::event::{Event, WindowEvent};
use tauri_runtime_wry::tao::event_loop::{ControlFlow, EventLoopProxy, EventLoopWindowTarget};
use tauri_runtime_wry::{Context, EventLoopIterationContext, Message, Plugin, PluginBuilder, WebContextStore};
use webkit2gtk::WebViewExt;

use crate::append_log;

#[derive(Default)]
pub(super) struct ReleaseWebContext {
    window_destroyed: bool,
}

impl PluginBuilder<EventLoopMessage> for ReleaseWebContext {
    type Plugin = Self;

    fn build(self, _: Context<EventLoopMessage>) -> Self {
        self
    }
}

impl Plugin<EventLoopMessage> for ReleaseWebContext {
    fn on_event(
        &mut self,
        event: &Event<Message<EventLoopMessage>>,
        _: &EventLoopWindowTarget<Message<EventLoopMessage>>,
        _: &EventLoopProxy<Message<EventLoopMessage>>,
        _: &mut ControlFlow,
        context: EventLoopIterationContext<'_, EventLoopMessage>,
        web_context: &WebContextStore,
    ) -> bool {
        if matches!(event, Event::WindowEvent { event: WindowEvent::Destroyed, .. }) {
            self.window_destroyed = true;
        } else if matches!(event, Event::MainEventsCleared) && std::mem::take(&mut self.window_destroyed)
            && context.windows.0.borrow().is_empty()
        {
            // Wry retains Linux contexts by default. Release after it has removed the
            // window, including its WebView, so WebKit can close the network process.
            web_context.lock().unwrap().clear();
        }
        false
    }
}

const POPUP_WIDTH: f64 = 376.0;
const MARGIN: i32 = 12;
// Hidden this long, the WebView and its WebKit processes are freed; reopening sooner is instant.
const WEBVIEW_IDLE: Duration = Duration::from_secs(5 * 60);
// Fallback only: the page normally reports popup_ready after its first list is
// rendered and sized. Never leave the popup invisible if that signal is lost.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(1500);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementMode {
    LayerShell,
    X11,
    Center,
}

struct PopupPlacement {
    mode: PlacementMode,
    lifecycle: Mutex<PopupLifecycle>,
    blur_palette: Mutex<Option<BlurPalette>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct BlurPalette {
    pub current: MenuColors,
    pub alternate: Option<MenuColors>,
    pub scheme: Option<ColorScheme>,
    pub corner_radius: Option<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ColorScheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MenuColors {
    pub panel: [u8; 4],
    pub bar: [u8; 4],
    pub text: Option<[u8; 4]>,
}

#[derive(Default)]
struct PopupLifecycle {
    last_focus_loss: Option<Instant>,
    idle_timer: Option<glib::SourceId>,
    destroying: bool,
    ready: bool,
    pending_show: Option<PopupRequest>,
    requested_height: Option<u32>,
    panel_work_height: Option<u32>,
}

struct PopupRequest {
    activation: Option<(i32, i32)>,
    token: Option<String>,
}

impl PlacementMode {
    fn name(self) -> &'static str {
        match self {
            Self::LayerShell => "layer-shell",
            Self::X11 => "x11",
            Self::Center => "center",
        }
    }
}

pub(super) fn choose_placement_mode() -> PlacementMode {
    let wayland_session = env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    if gtk::init().is_ok() {
        if let Some(display) = gdk::Display::default() {
            let backend = display.backend();
            if backend.is_x11() && !wayland_session {
                return PlacementMode::X11;
            }
            if backend.is_wayland() {
                let supported = gtk_layer_shell::is_supported();
                append_log(&format!(">>> layer-shell supported={supported}"));
                if supported {
                    return PlacementMode::LayerShell;
                }
            }
        }
    }

    PlacementMode::Center
}

pub(super) fn setup(app: &AppHandle, mode: PlacementMode) {
    app.manage(PopupPlacement {
        mode,
        lifecycle: Mutex::default(),
        blur_palette: Mutex::default(),
    });
    append_log(&format!(">>> placement mode: {}", mode.name()));
    if let Some(settings) = gtk::Settings::default() {
        let app = app.clone();
        settings.connect_gtk_font_name_notify(move |_| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.eval(font_script());
            }
        });
    }
}

fn blur_script(palette: Option<BlurPalette>) -> String {
    let blurred = palette.is_some();
    let scheme = match palette.and_then(|palette| palette.scheme) {
        Some(ColorScheme::Light) => "light",
        Some(ColorScheme::Dark) => "dark",
        None => "",
    };
    let radius = palette
        .and_then(|palette| palette.corner_radius)
        .map_or_else(String::new, |radius| format!("{radius}px"));
    let rgba = |color: Option<[u8; 4]>| color.map_or_else(
        || (String::new(), String::new()),
        |[r, g, b, a]| {
            let alpha = f64::from(a) / 255.0;
            (format!("rgba({r},{g},{b},{alpha})"), alpha.to_string())
        },
    );
    let (panel, panel_alpha) = rgba(palette.map(|palette| palette.current.panel));
    let (bar, bar_alpha) = rgba(palette.map(|palette| palette.current.bar));
    let alternate = palette.and_then(|palette| palette.alternate);
    let (alt_panel, _) = rgba(alternate.map(|colors| colors.panel));
    let (alt_bar, _) = rgba(alternate.map(|colors| colors.bar));
    // Opaque text in the theme's colour; secondary text is a solid mix of it
    // (not alpha, which lets the background bleed through the glyphs).
    let foreground = |text: Option<[u8; 4]>| text.map_or_else(
        || (String::new(), String::new()),
        |[r, g, b, _]| {
            let text = format!("rgb({r},{g},{b})");
            let muted = format!("color-mix(in srgb, {text} 74%, #7c7c80)");
            (text, muted)
        },
    );
    let (text, muted) = foreground(palette.and_then(|palette| palette.current.text));
    let (alt_text, alt_muted) = foreground(alternate.and_then(|colors| colors.text));
    format!(r#"(() => {{
        const apply = () => {{
            const root = document.documentElement;
            const style = root.style;
            root.toggleAttribute('data-system-blur', {blurred});
            const scheme = '{scheme}';
            if (scheme) root.dataset.paletteScheme = scheme;
            else delete root.dataset.paletteScheme;
            style.setProperty('--sys-panel', '{panel}');
            style.setProperty('--sys-panel-alpha', '{panel_alpha}');
            style.setProperty('--sys-bar', '{bar}');
            style.setProperty('--sys-bar-alpha', '{bar_alpha}');
            style.setProperty('--card-radius', '{radius}');
            style.setProperty('--sys-text', '{text}');
            style.setProperty('--sys-muted', '{muted}');
            style.setProperty('--alt-panel', '{alt_panel}');
            style.setProperty('--alt-bar', '{alt_bar}');
            style.setProperty('--alt-text', '{alt_text}');
            style.setProperty('--alt-muted', '{alt_muted}');
        }};
        if (document.documentElement) apply();
        else document.addEventListener('DOMContentLoaded', apply, {{ once: true }});
    }})()"#)
}

pub(super) fn set_blur_palette(app: &AppHandle, palette: Option<BlurPalette>) {
    let state = app.state::<PopupPlacement>();
    let mut current = state.blur_palette.lock().unwrap();
    if *current != palette {
        *current = palette;
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.eval(blur_script(palette));
        }
    }
}

fn font_script() -> String {
    fn css_family(name: Option<glib::GString>, fallback: &str) -> String {
        let family = name
            .and_then(|name| pango::FontDescription::from_string(&name).family())
            .filter(|family| !family.is_empty());
        family.map_or_else(|| fallback.to_owned(), |family| {
            let family = family.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{family}\", {fallback}")
        })
    }

    let interface = "org.gnome.desktop.interface";
    let mono = gio::SettingsSchemaSource::default()
        .and_then(|source| source.lookup(interface, true))
        .filter(|schema| schema.has_key("monospace-font-name"))
        .map(|_| gio::Settings::new(interface).string("monospace-font-name"));
    let ui = gtk::Settings::default().and_then(|settings| settings.gtk_font_name());
    let ui = serde_json::to_string(&css_family(ui, "system-ui, sans-serif")).unwrap();
    let mono = serde_json::to_string(&css_family(mono, "monospace")).unwrap();
    format!(r#"(() => {{
        const applyFonts = () => {{
            document.documentElement.style.setProperty('--font-ui', {ui});
            document.documentElement.style.setProperty('--font-mono', {mono});
        }};
        if (document.documentElement) applyFonts();
        else document.addEventListener('DOMContentLoaded', applyFonts, {{ once: true }});
    }})()"#)
}

#[tauri::command]
pub(super) fn resize_popup(window: WebviewWindow, height: u32) -> Result<(), String> {
    let gtk_window = window.gtk_window().map_err(|err| err.to_string())?;
    let state = window.state::<PopupPlacement>();
    let panel_work_height = {
        let mut lifecycle = state.lifecycle();
        lifecycle.requested_height = Some(height);
        lifecycle.panel_work_height
    };
    let work_height = panel_work_height.or_else(|| {
        let display = gtk_window.display();
        gtk_window
            .window()
            .and_then(|native| display.monitor_at_window(&native))
            .or_else(|| display.primary_monitor())
            .map(|monitor| monitor.workarea().height().max(1) as u32)
    });
    let limit = work_height.map_or(560, |height| (height.saturating_mul(7) / 10).clamp(1, 560));
    let height = height.clamp(1, limit);
    if gtk_window.size().1 != height as i32 {
        window
            .set_size(LogicalSize::new(POPUP_WIDTH as u32, height))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub(super) fn set_panel_work_height(app: &AppHandle, height: Option<u32>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let state = handle.state::<PopupPlacement>();
        let requested_height = {
            let mut lifecycle = state.lifecycle();
            if lifecycle.panel_work_height == height {
                return;
            }
            lifecycle.panel_work_height = height;
            lifecycle.requested_height
        };
        if let Some((window, requested_height)) = handle.get_webview_window("main").zip(requested_height) {
            let _ = resize_popup(window, requested_height);
        }
    });
}

impl PopupLifecycle {
    fn cancel_idle(&mut self) {
        if let Some(timer) = self.idle_timer.take() {
            timer.remove();
        }
    }
}

impl PopupPlacement {
    fn lifecycle(&self) -> std::sync::MutexGuard<'_, PopupLifecycle> {
        self.lifecycle.lock().unwrap_or_else(|err| err.into_inner())
    }
}

fn log_lifecycle(message: &str) {
    append_log(message);
    eprintln!("{message}");
}

fn create_window(app: &AppHandle, mode: PlacementMode) -> tauri::Result<WebviewWindow> {
    let start = Instant::now();
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == "main")
        .ok_or(tauri::Error::WindowNotFound)?;
    let window = WebviewWindowBuilder::from_config(app, config)?
        .initialization_script(font_script())
        .initialization_script(blur_script(*app.state::<PopupPlacement>().blur_palette.lock().unwrap()))
        .build()?;
    let gtk_window = window.gtk_window()?;
    if mode == PlacementMode::LayerShell && !gtk_window.is_realized() {
        gtk_window.init_layer_shell();
        gtk_window.set_layer(Layer::Top);
        gtk_window.set_keyboard_mode(KeyboardMode::OnDemand);
        gtk_window.set_exclusive_zone(0);
        gtk_window.set_anchor(Edge::Top, true);
        gtk_window.set_layer_shell_margin(Edge::Top, MARGIN);
    }
    if mode != PlacementMode::X11 && !gtk_window.is_layer_window() {
        gtk_window.set_position(gtk::WindowPosition::Center);
    }
    crate::kde_blur::install(app, &gtk_window);

    // Present once GTK has mapped the window.
    gtk_window.connect_map(|window| window.present());
    let handle = app.clone();
    gtk_window.connect_unmap(move |_| schedule_idle_destruction(&handle));
    window.with_webview(move |platform| {
        let view = platform.inner();
        view.connect_destroy(disconnect_webview_ipc);
        view.set_property("zoom-level", 1.0_f64);
        #[cfg(not(debug_assertions))]
        view.connect_local("context-menu", false, |_| Some(true.to_value()));
        // Consume pinch before the WebView's native gesture handler can scale the page.
        view.connect_event(|_, event| {
            if event.event_type() == gdk::EventType::TouchpadPinch {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        view.connect_notify_local(Some("zoom-level"), |view, _| {
            if view.property::<f64>("zoom-level") != 1.0 {
                view.set_property("zoom-level", 1.0_f64);
            }
        });
    })?;
    let native = gtk_window.downgrade();
    let handle = app.clone();
    glib::timeout_add_local_once(FIRST_FRAME_TIMEOUT.saturating_sub(start.elapsed()), move || {
        if let Some(window) = native.upgrade() {
            if !handle.state::<PopupPlacement>().lifecycle().ready {
                append_log(">>> popup ready (fallback timeout)");
            }
            first_frame_ready(&handle, &window);
        }
    });
    log_lifecycle(&format!(
        ">>> webview recreated in {} ms",
        start.elapsed().as_millis()
    ));
    Ok(window)
}

fn disconnect_webview_ipc(view: &webkit2gtk::WebView) {
    use glib::{gobject_ffi, subclass::SignalId, translate::IntoGlib};
    use std::ptr::null_mut;

    let Some(manager) = view.user_content_manager() else { return };
    let Some(signal) = SignalId::lookup("script-message-received", manager.type_()) else { return };
    // shortcut: Wry 0.55's IPC closure holds the view that owns this manager.
    // Recheck the cycle and signal on Wry upgrades.
    // SAFETY: GTK emits destroy on the main thread and the manager is borrowed
    // alive throughout this lookup and disconnect.
    unsafe {
        let manager = manager.as_ptr().cast();
        let handler = gobject_ffi::g_signal_handler_find(
            manager, gobject_ffi::G_SIGNAL_MATCH_ID, signal.into_glib(), 0,
            null_mut(), null_mut(), null_mut(),
        );
        if handler != 0 {
            gobject_ffi::g_signal_handler_disconnect(manager, handler);
        }
    }
}

/// The page has rendered its first list at its final height: map the window now.
#[tauri::command]
pub(super) fn popup_ready(window: WebviewWindow) {
    if let Ok(gtk_window) = window.gtk_window() {
        append_log(">>> popup ready (page rendered)");
        first_frame_ready(window.app_handle(), &gtk_window);
    }
}

fn first_frame_ready(app: &AppHandle, window: &gtk::ApplicationWindow) {
    let current = app
        .get_webview_window("main")
        .and_then(|window| window.gtk_window().ok());
    if current.as_ref() != Some(window) {
        return;
    }
    let state = app.state::<PopupPlacement>();
    let request = {
        let mut lifecycle = state.lifecycle();
        if lifecycle.ready || lifecycle.destroying {
            return;
        }
        lifecycle.ready = true;
        lifecycle.pending_show.take()
    };
    if let Some(request) = request {
        if let Err(error) = show(app, request) {
            append_log(&format!(">>> popup request failed: {error}"));
        }
    }
}

fn schedule_idle_destruction(app: &AppHandle) {
    let state = app.state::<PopupPlacement>();
    let mut lifecycle = state.lifecycle();
    if !lifecycle.destroying && lifecycle.idle_timer.is_none() {
        let app = app.clone();
        lifecycle.idle_timer = Some(glib::timeout_add_local_once(WEBVIEW_IDLE, move || {
            let state = app.state::<PopupPlacement>();
            state.lifecycle().idle_timer.take();
            if let Some(window) = app.get_webview_window("main") {
                if !window.is_visible().unwrap_or(true) {
                    state.lifecycle().destroying = true;
                    if let Err(error) = window.destroy() {
                        state.lifecycle().destroying = false;
                        log_lifecycle(&format!(">>> idle webview destroy failed: {error}"));
                    }
                }
            }
        }));
    }
}

pub(super) fn on_destroyed(app: &AppHandle) {
    let state = app.state::<PopupPlacement>();
    let (destroyed_after_idle, pending) = {
        let mut lifecycle = state.lifecycle();
        lifecycle.cancel_idle();
        lifecycle.ready = false;
        lifecycle.requested_height = None;
        (std::mem::take(&mut lifecycle.destroying), lifecycle.pending_show.take())
    };
    if destroyed_after_idle {
        log_lifecycle(">>> webview destroyed after idle");

        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        {
            let app = app.clone();
            // GTK and WebKit can release allocations after the Destroyed callback.
            glib::timeout_add_local_once(Duration::from_millis(500), move || {
                if app.get_webview_window("main").is_none() {
                    // SAFETY: malloc_trim synchronizes with the allocator and takes no pointers.
                    unsafe { libc::malloc_trim(0) };
                }
            });
        }
    }
    if let Some(request) = pending {
        if let Err(error) = show(app, request) {
            append_log(&format!(">>> popup request failed: {error}"));
        }
    }
}

pub(super) fn on_window_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    if window.label() != "main" {
        return;
    }
    let tauri::WindowEvent::Focused(focused) = event else {
        return;
    };
    append_log(&format!(">>> popup focus={focused}"));
    if *focused {
        return;
    }
    if let Ok(gtk_window) = window.gtk_window() {
        // A focus-out queued by an explicit Hide may arrive after a new Show.
        if gtk_window.is_visible() && !gtk_window.is_active() {
            window.state::<PopupPlacement>().lifecycle().last_focus_loss = Some(Instant::now());
            gtk_window.hide();
        }
    }
}

pub(super) fn request(app: &AppHandle, activation: Option<(i32, i32)>, token: Option<String>) {
    append_log(&format!(">>> activation token present={}", token.is_some()));
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if let Err(error) = show(&handle, PopupRequest { activation, token }) {
            append_log(&format!(">>> popup request failed: {error}"));
        }
    }) {
        append_log(&format!(">>> popup dispatch failed: {error}"));
    }
}

pub(super) fn request_prepare(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || prepare(&handle)) {
        append_log(&format!(">>> popup prepare dispatch failed: {error}"));
    }
}

/// Start the WebView hidden ahead of a likely Show. The idle teardown frees it
/// again if the popup is not opened.
fn prepare(app: &AppHandle) {
    let placement = app.state::<PopupPlacement>();
    if placement.lifecycle().destroying {
        return;
    }
    match app.get_webview_window("main") {
        Some(window) => {
            // Already loaded and hidden: restart the countdown so it survives until the click.
            if !window.is_visible().unwrap_or(true) {
                placement.lifecycle().cancel_idle();
                schedule_idle_destruction(app);
            }
        }
        None => match create_window(app, placement.mode) {
            Ok(_) => schedule_idle_destruction(app),
            Err(error) => append_log(&format!(">>> popup prepare failed: {error}")),
        },
    }
}

pub(super) fn request_hide(app: &AppHandle) {
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || hide(&handle)) {
        append_log(&format!(">>> popup hide dispatch failed: {error}"));
    }
}

fn hide(app: &AppHandle) {
    app.state::<PopupPlacement>().lifecycle().pending_show = None;
    if let Some(window) = app
        .get_webview_window("main")
        .and_then(|window| window.gtk_window().ok())
    {
        window.hide();
        // A Show canceled before the first paint never emits unmap.
        schedule_idle_destruction(app);
    }
}

fn show(app: &AppHandle, request: PopupRequest) -> tauri::Result<()> {
    let placement = app.state::<PopupPlacement>();
    {
        let mut lifecycle = placement.lifecycle();
        if lifecycle.destroying {
            lifecycle.pending_show = Some(request);
            return Ok(());
        }
    }

    let window = app.get_webview_window("main");
    let visible = window.as_ref().map(|window| window.is_visible()).transpose()?.unwrap_or(false);
    if request.activation.is_some() {
        if visible || placement.lifecycle().pending_show.is_some() {
            hide(app);
            return Ok(());
        }
        if recently_lost_focus(placement.lifecycle().last_focus_loss, Instant::now()) {
            return Ok(());
        }
    }
    placement.lifecycle().cancel_idle();
    let window = match window {
        Some(window) => window,
        None => create_window(app, placement.mode)?,
    };
    if !placement.lifecycle().ready {
        placement.lifecycle().pending_show = Some(request);
        return Ok(());
    }
    let gtk_window = window.gtk_window()?;

    if !visible {
        let tray_point = request.activation.filter(|&point| point != (0, 0));
        match placement.mode {
            PlacementMode::LayerShell if gtk_window.is_layer_window() => {
                position_layer_shell(&gtk_window, tray_point);
            }
            PlacementMode::X11 => {
                if let Some((point, scale)) = position_x11(app, tray_point) {
                    window.set_position(point.to_logical::<f64>(scale))?;
                } else {
                    let _ = window.center();
                }
            }
            PlacementMode::Center | PlacementMode::LayerShell => (),
        }
    }
    if let Some(token) = request.token.as_deref().filter(|_| gtk_window.display().backend().is_wayland()) {
        gtk_window.set_startup_id(token);
    }
    if visible {
        gtk_window.present();
    } else if placement.mode != PlacementMode::Center {
        // Keep Tao's mapping order, including queued X11 positioning.
        return window.show();
    } else {
        gtk_window.show_all();
    }
    Ok(())
}

fn recently_lost_focus(last: Option<Instant>, now: Instant) -> bool {
    last.is_some_and(|last| now.duration_since(last) < Duration::from_millis(400))
}

fn position_layer_shell(window: &gtk::ApplicationWindow, tray_point: Option<(i32, i32)>) {
    let display = window.display();
    let monitor = tray_point.and_then(|(x, y)| display.monitor_at_point(x, y));
    let left = monitor.as_ref().zip(tray_point).map(|(monitor, (x, _))| {
        let rect = monitor.geometry();
        let width = POPUP_WIDTH as i32;
        x.saturating_sub(rect.x())
            .saturating_sub(width / 2)
            .clamp(0, (rect.width() - width).max(0))
    });

    if let Some(monitor) = monitor
        .or_else(|| display.primary_monitor())
        .or_else(|| display.monitor(0))
    {
        window.set_monitor(&monitor);
    }
    window.set_anchor(Edge::Left, left.is_some());
    window.set_anchor(Edge::Right, left.is_none());
    window.set_layer_shell_margin(Edge::Left, left.unwrap_or(0));
    window.set_layer_shell_margin(Edge::Right, if left.is_none() { MARGIN } else { 0 });
}

fn position_x11(
    app: &AppHandle,
    tray_point: Option<(i32, i32)>,
) -> Option<(PhysicalPosition<i32>, f64)> {
    let monitor = tray_point
        .and_then(|(x, y)| {
            // Tao passes this point to GDK's logical monitor lookup on X11.
            let scale = f64::from(gdk::Display::default()?.default_group().scale_factor());
            let point = PhysicalPosition::new(x, y).to_logical::<f64>(scale);
            app.monitor_from_point(point.x, point.y).ok().flatten()
        })
        .or_else(|| app.primary_monitor().ok().flatten())?;
    let scale = monitor.scale_factor();
    let anchor_x = tray_point.map(|(x, _)| f64::from(x));
    Some((x11_popup_position(anchor_x, *monitor.work_area(), scale), scale))
}

fn x11_popup_position(
    anchor_x: Option<f64>,
    work_area: PhysicalRect<i32, u32>,
    scale: f64,
) -> PhysicalPosition<i32> {
    let width = POPUP_WIDTH * scale;
    let margin = f64::from(MARGIN) * scale;
    let left = f64::from(work_area.position.x);
    let max_x = (left + f64::from(work_area.size.width) - width - margin).max(left + margin);
    let x = anchor_x.map_or(max_x, |x| (x - width / 2.0).clamp(left + margin, max_x));
    let y = f64::from(work_area.position.y) + margin;
    PhysicalPosition::new(x, y).cast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_suppresses_activation_for_recent_focus_loss() {
        let blur = Instant::now();
        assert!(!recently_lost_focus(None, blur));
        assert!(recently_lost_focus(Some(blur), blur + Duration::from_millis(399)));
        assert!(!recently_lost_focus(Some(blur), blur + Duration::from_millis(400)));
    }

    #[test]
    fn positions_popup_below_work_area_top_and_clamps_right_edge() {
        let work_area = PhysicalRect {
            position: (0, 32).into(),
            size: (1920, 1048).into(),
        };
        assert_eq!(x11_popup_position(Some(1255.0), work_area, 1.0), (1067, 44).into());
        assert_eq!(x11_popup_position(Some(1919.0), work_area, 1.0), (1532, 44).into());
        assert_eq!(x11_popup_position(None, work_area, 1.0), (1532, 44).into());
    }

    #[test]
    fn uses_offset_monitor_origin_and_hidpi_scale() {
        let work_area = PhysicalRect {
            position: (-2560, 200).into(),
            size: (2560, 1440).into(),
        };
        assert_eq!(x11_popup_position(Some(-1280.0), work_area, 2.0), (-1656, 224).into());
        assert_eq!(x11_popup_position(Some(-20.0), work_area, 2.0), (-776, 224).into());
    }
}
