//! GNOME Shell integration: a D-Bus service the companion extension calls, the
//! tray-icon handoff while the extension is active, and installing the extension.
use futures_lite::StreamExt;
use gtk::{gio, glib, prelude::*};
use serde::Serialize;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::AppHandle;
use zbus::{connection, fdo, interface};

use crate::{append_log, popup, start_tray, stop_tray};

const UUID: &str = "harbor@harbor.io";
const BUS_NAME: &str = "io.harbor.Harbor";
const OBJECT_PATH: &str = "/io/harbor/Harbor";
/// Owned by the extension while it is enabled (see gnome-extension/extension.js).
const PRESENCE_NAME: &str = "io.harbor.GnomeIntegration";
const FILES: [(&str, &str); 3] = [
    ("metadata.json", include_str!("../gnome-extension/metadata.json")),
    ("extension.js", include_str!("../gnome-extension/extension.js")),
    ("harbor-symbolic.svg", include_str!("../gnome-extension/harbor-symbolic.svg")),
];

static EXTENSION_ACTIVE: AtomicBool = AtomicBool::new(false);

struct HarborService {
    app: AppHandle,
}

#[interface(name = "io.harbor.Harbor", spawn = false)]
impl HarborService {
    /// The extension's button: show the popup, or close it if open.
    fn toggle(&self) {
        // Some(..) selects toggle semantics; (0, 0) means "no tray coordinates".
        popup::request(&self.app, Some((0, 0)), None);
    }

    fn show(&self) {
        popup::request(&self.app, None, None);
    }

    fn hide(&self) {
        popup::request_hide(&self.app);
    }
}

/// Serve the D-Bus API and keep the tray icon off while the extension is active.
/// Falls back to the tray icon if the session bus is unavailable.
pub(crate) async fn run(app: AppHandle) {
    if let Err(err) = serve(app.clone()).await {
        append_log(&format!(">>> gnome integration unavailable: {err}"));
        start_tray(app).await;
    }
}

async fn serve(app: AppHandle) -> zbus::Result<()> {
    if is_gnome() && installed() {
        // Keep an installed extension in step with this harbor version.
        if let Err(err) = write_files() {
            append_log(&format!(">>> gnome extension refresh failed: {err}"));
        }
    }
    let connection = connection::Builder::session()?
        .name(BUS_NAME)?
        .serve_at(OBJECT_PATH, HarborService { app: app.clone() })?
        .build()
        .await?;
    let dbus = fdo::DBusProxy::new(&connection).await?;
    let mut changes = dbus
        .receive_name_owner_changed_with_args(&[(0, PRESENCE_NAME)])
        .await?;
    let present = dbus
        .name_has_owner(PRESENCE_NAME.try_into().expect("valid bus name"))
        .await?;
    set_extension_active(&app, present).await;
    while let Some(signal) = changes.next().await {
        let present = signal.args().map(|args| args.new_owner().is_some()).unwrap_or(false);
        set_extension_active(&app, present).await;
    }
    Ok(())
}

async fn set_extension_active(app: &AppHandle, active: bool) {
    EXTENSION_ACTIVE.store(active, Ordering::Relaxed);
    append_log(&format!(">>> gnome extension active={active}"));
    if active {
        stop_tray(app).await;
    } else {
        start_tray(app.clone()).await;
    }
}

fn is_gnome() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktops| desktops.split(':').any(|desktop| desktop.eq_ignore_ascii_case("gnome")))
        && gio::SettingsSchemaSource::default()
            .and_then(|source| source.lookup("org.gnome.shell", true))
            .is_some()
}

fn extension_dir() -> PathBuf {
    glib::user_data_dir().join("gnome-shell/extensions").join(UUID)
}

fn installed() -> bool {
    extension_dir().join("metadata.json").is_file()
}

fn write_files() -> std::io::Result<()> {
    let dir = extension_dir();
    fs::create_dir_all(&dir)?;
    for (name, contents) in FILES {
        let path = dir.join(name);
        if fs::read_to_string(&path).ok().as_deref() != Some(contents) {
            fs::write(path, contents)?;
        }
    }
    Ok(())
}

#[derive(Serialize)]
pub(crate) struct GnomeStatus {
    /// Running on GNOME, so the integration can be offered at all.
    available: bool,
    installed: bool,
    /// In GNOME's enabled list (takes effect after logging back in on Wayland).
    enabled: bool,
    /// The extension is running right now.
    active: bool,
    /// The user has turned off all user extensions in GNOME.
    user_extensions_disabled: bool,
}

fn status() -> GnomeStatus {
    let available = is_gnome();
    let shell = available.then(|| gio::Settings::new("org.gnome.shell"));
    GnomeStatus {
        available,
        installed: installed(),
        enabled: shell
            .as_ref()
            .is_some_and(|shell| shell.strv("enabled-extensions").iter().any(|uuid| uuid == UUID)),
        active: EXTENSION_ACTIVE.load(Ordering::Relaxed),
        user_extensions_disabled: shell
            .as_ref()
            .is_some_and(|shell| shell.boolean("disable-user-extensions")),
    }
}

#[tauri::command]
pub(crate) fn gnome_integration_status() -> GnomeStatus {
    status()
}

#[tauri::command]
pub(crate) fn enable_gnome_integration() -> Result<GnomeStatus, String> {
    if !is_gnome() {
        return Err("GNOME integration is only available on GNOME".into());
    }
    write_files().map_err(|err| format!("Could not install the GNOME extension: {err}"))?;
    let shell = gio::Settings::new("org.gnome.shell");
    let mut enabled: Vec<String> = shell.strv("enabled-extensions").iter().map(|uuid| uuid.to_string()).collect();
    if !enabled.iter().any(|uuid| uuid == UUID) {
        enabled.push(UUID.into());
        shell
            .set_strv("enabled-extensions", enabled.iter().map(String::as_str).collect::<Vec<_>>())
            .map_err(|err| err.to_string())?;
    }
    let disabled: Vec<String> = shell.strv("disabled-extensions").iter().map(|uuid| uuid.to_string()).collect();
    if disabled.iter().any(|uuid| uuid == UUID) {
        shell
            .set_strv(
                "disabled-extensions",
                disabled.iter().filter(|uuid| *uuid != UUID).map(String::as_str).collect::<Vec<_>>(),
            )
            .map_err(|err| err.to_string())?;
    }
    gio::Settings::sync();
    Ok(status())
}
