use gtk::{gdk, prelude::*};
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::{
    env,
    sync::Mutex,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalRect};

use crate::append_log;

const POPUP_WIDTH: f64 = 400.0;
const MARGIN: i32 = 12;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlacementMode {
    LayerShell,
    X11,
    Center,
}

struct PopupPlacement {
    mode: PlacementMode,
    last_focus_loss: Mutex<Option<Instant>>,
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

pub(super) fn setup(app: &AppHandle, mut mode: PlacementMode) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        // Tauri creates the configured visible:false window before running setup.
        let gtk_window = window.gtk_window()?;
        let realized = gtk_window.is_realized();
        let mapped = gtk_window.is_mapped();
        append_log(&format!(">>> popup in setup realized={realized} mapped={mapped}"));
        if mode == PlacementMode::LayerShell {
            if !realized {
                gtk_window.init_layer_shell();
                gtk_window.set_layer(Layer::Top);
                gtk_window.set_keyboard_mode(KeyboardMode::OnDemand);
                gtk_window.set_exclusive_zone(0);
                gtk_window.set_anchor(Edge::Top, true);
                gtk_window.set_layer_shell_margin(Edge::Top, MARGIN);
            }
            if !gtk_window.is_layer_window() {
                mode = PlacementMode::Center;
            }
        }
        if mode == PlacementMode::Center {
            gtk_window.set_position(gtk::WindowPosition::Center);
        }

        // Tao queues show(), so present once GTK has mapped the window.
        gtk_window.connect_map(|window| window.present());
    }
    app.manage(PopupPlacement {
        mode,
        last_focus_loss: Mutex::new(None),
    });
    append_log(&format!(">>> placement mode: {}", mode.name()));
    Ok(())
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
        if let Ok(mut time) = placement.last_focus_loss.lock() {
            *time = Some(Instant::now());
        }
    }
    if let Err(error) = window.hide() {
        append_log(&format!(">>> popup hide on focus loss failed: {error}"));
    }
}

pub(super) fn request(app: &AppHandle, activation: Option<(i32, i32)>, token: Option<String>) {
    append_log(&format!(">>> activation token present={}", token.is_some()));
    let handle = app.clone();
    if let Err(error) = app.run_on_main_thread(move || {
        if let Err(error) = show(&handle, activation, token.as_deref()) {
            append_log(&format!(">>> popup request failed: {error}"));
        }
    }) {
        append_log(&format!(">>> popup dispatch failed: {error}"));
    }
}

fn show(app: &AppHandle, activation: Option<(i32, i32)>, token: Option<&str>) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window("main") else {
        return Ok(());
    };
    let gtk_window = window.gtk_window()?;
    let visible = gtk_window.is_visible();
    if visible && activation.is_some() {
        return window.hide();
    }

    if let Some(placement) = app.try_state::<PopupPlacement>().filter(|_| !visible) {
        if activation.is_some()
            && placement
                .last_focus_loss
                .lock()
                .ok()
                .is_some_and(|last| recently_lost_focus(*last, Instant::now()))
        {
            return Ok(());
        }

        let tray_point = activation.filter(|&point| point != (0, 0));
        match placement.mode {
            PlacementMode::LayerShell => {
                position_layer_shell(&gtk_window, tray_point);
            }
            PlacementMode::X11 => {
                if let Some((point, scale)) = position_x11(app, tray_point) {
                    window.set_position(point.to_logical::<f64>(scale))?;
                } else {
                    let _ = window.center();
                }
            }
            PlacementMode::Center => (),
        }
    }
    if let Some(token) = token.filter(|_| gtk_window.display().backend().is_wayland()) {
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
