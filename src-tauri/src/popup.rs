use gtk::{gdk, glib, prelude::*};
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::{
    env,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalRect, WebviewWindow, WebviewWindowBuilder};

use crate::append_log;

const POPUP_WIDTH: f64 = 400.0;
const MARGIN: i32 = 12;
const WEBVIEW_IDLE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementMode {
    LayerShell,
    X11,
    Center,
}

struct PopupPlacement {
    mode: PlacementMode,
    lifecycle: Mutex<PopupLifecycle>,
}

#[derive(Default)]
struct PopupLifecycle {
    last_focus_loss: Option<Instant>,
    idle_timer: Option<glib::SourceId>,
    destroying: bool,
    pending_show: Option<PopupRequest>,
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
    });
    append_log(&format!(">>> placement mode: {}", mode.name()));
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
    let window = WebviewWindowBuilder::from_config(app, config)?.build()?;
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

    // Tao queues show(), so present once GTK has mapped the window.
    gtk_window.connect_map(|window| window.present());
    let handle = app.clone();
    gtk_window.connect_unmap(move |_| schedule_idle_destruction(&handle));
    log_lifecycle(&format!(
        ">>> webview recreated in {} ms",
        start.elapsed().as_millis()
    ));
    Ok(window)
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
    if let Some(placement) = window.try_state::<PopupPlacement>() {
        placement.lifecycle().last_focus_loss = Some(Instant::now());
    }
    if let Err(error) = window.hide() {
        append_log(&format!(">>> popup hide on focus loss failed: {error}"));
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
        if visible {
            return window.expect("visible popup").hide();
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
        Ok(())
    } else {
        window.show()
    }
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
        assert_eq!(x11_popup_position(Some(1255.0), work_area, 1.0), (1055, 44).into());
        assert_eq!(x11_popup_position(Some(1919.0), work_area, 1.0), (1508, 44).into());
        assert_eq!(x11_popup_position(None, work_area, 1.0), (1508, 44).into());
    }

    #[test]
    fn uses_offset_monitor_origin_and_hidpi_scale() {
        let work_area = PhysicalRect {
            position: (-2560, 200).into(),
            size: (2560, 1440).into(),
        };
        assert_eq!(x11_popup_position(Some(-1280.0), work_area, 2.0), (-1680, 224).into());
        assert_eq!(x11_popup_position(Some(-20.0), work_area, 2.0), (-824, 224).into());
    }
}
