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
const FILES: [(&str, &str); 4] = [
    ("metadata.json", include_str!("../gnome-extension/metadata.json")),
    ("extension.js", include_str!("../gnome-extension/extension.js")),
    ("blur.js", include_str!("../gnome-extension/blur.js")),
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

    async fn set_blur(
        &self,
        active: bool,
        palette: Vec<u8>,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<()> {
        require_extension(&header, connection).await?;
        let palette = parse_palette(active, &palette)
            .map_err(|message| fdo::Error::InvalidArgs(message.into()))?;
        append_log(&format!(">>> GNOME blur active={active}"));
        popup::set_blur_palette(&self.app, palette);
        Ok(())
    }

    async fn set_panel_work_height(
        &self,
        height: u32,
        #[zbus(header)] header: zbus::message::Header<'_>,
        #[zbus(connection)] connection: &zbus::Connection,
    ) -> fdo::Result<()> {
        require_extension(&header, connection).await?;
        if height == 0 {
            return Err(fdo::Error::InvalidArgs("Expected a positive work-area height".into()));
        }
        popup::set_panel_work_height(&self.app, Some(height));
        Ok(())
    }
}

async fn require_extension(
    header: &zbus::message::Header<'_>,
    connection: &zbus::Connection,
) -> fdo::Result<()> {
    let owner = fdo::DBusProxy::new(connection)
        .await?
        .get_name_owner(PRESENCE_NAME.try_into().expect("valid bus name"))
        .await?;
    if header.sender() != Some(&owner) {
        return Err(fdo::Error::AccessDenied("Only the harbor extension can update panel state".into()));
    }
    Ok(())
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
        // Never replace a newer installed extension with this binary's older copy
        // (e.g. an older packaged harbor autostarting after a dev build installed it).
        if !embedded_is_older() {
            if let Err(err) = write_files() {
                append_log(&format!(">>> gnome extension refresh failed: {err}"));
            }
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

/// The paired format prefixes the current scheme and radius to two sets of menu colors.
/// Older extensions send current colors followed by an optional radius.
fn parse_palette(active: bool, palette: &[u8]) -> Result<Option<popup::BlurPalette>, &'static str> {
    use popup::{BlurPalette, ColorScheme, MenuColors};

    let colors = |bytes: &[u8]| match bytes {
        [r, g, b, a, br, bg, bb, ba, rest @ ..] => Some(MenuColors {
            panel: [*r, *g, *b, *a],
            bar: [*br, *bg, *bb, *ba],
            text: rest.try_into().ok(),
        }),
        _ => None,
    };
    Ok(match (active, palette) {
        (true, [scheme @ (0 | 1), radius, bytes @ ..]) if matches!(bytes.len(), 12 | 24) => {
            Some(BlurPalette {
                current: colors(&bytes[..12]).unwrap(),
                alternate: colors(&bytes[12..]),
                scheme: Some(if *scheme == 0 { ColorScheme::Light } else { ColorScheme::Dark }),
                corner_radius: Some(*radius),
            })
        }
        (true, bytes) if matches!(bytes.len(), 8 | 9 | 12 | 13) => {
            let end = if bytes.len() <= 9 { 8 } else { 12 };
            Some(BlurPalette {
                current: colors(&bytes[..end]).unwrap(),
                alternate: None,
                scheme: None,
                corner_radius: bytes.get(end).copied(),
            })
        }
        (_, []) => None,
        _ => return Err("Expected a current palette and an optional alternate palette"),
    })
}

async fn set_extension_active(app: &AppHandle, active: bool) {
    let was_active = EXTENSION_ACTIVE.swap(active, Ordering::Relaxed);
    append_log(&format!(">>> gnome extension active={active}"));
    if active {
        stop_tray(app).await;
    } else {
        if was_active {
            popup::set_blur_palette(app, None);
            popup::set_panel_work_height(app, None);
        }
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

/// `"version"` from an extension metadata.json.
fn metadata_version(metadata: &str) -> Option<u64> {
    let rest = metadata.split("\"version\"").nth(1)?;
    let digits: String = rest
        .trim_start_matches(|ch: char| ch == ':' || ch.is_whitespace())
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn embedded_is_older() -> bool {
    let installed = fs::read_to_string(extension_dir().join("metadata.json"))
        .ok()
        .and_then(|metadata| metadata_version(&metadata));
    let embedded = metadata_version(FILES[0].1);
    matches!((installed, embedded), (Some(installed), Some(embedded)) if embedded < installed)
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

#[cfg(test)]
mod tests {
    use super::{metadata_version, parse_palette};

    #[test]
    fn reads_extension_metadata_version() {
        assert_eq!(metadata_version("{\"uuid\": \"x\",\n  \"version\": 3\n}"), Some(3));
        assert_eq!(metadata_version("{\"version\":12}"), Some(12));
        assert_eq!(metadata_version("{}"), None);
    }

    #[test]
    fn decodes_palette_with_and_without_text_and_radius() {
        let base = [1, 2, 3, 4, 5, 6, 7, 8];
        let p = parse_palette(true, &base).unwrap().unwrap();
        assert_eq!((p.current.panel, p.current.bar, p.current.text, p.corner_radius), ([1, 2, 3, 4], [5, 6, 7, 8], None, None));
        let p = parse_palette(true, &[&base[..], &[20]].concat()).unwrap().unwrap();
        assert_eq!((p.current.text, p.corner_radius), (None, Some(20)));
        let p = parse_palette(true, &[&base[..], &[9, 10, 11, 255]].concat()).unwrap().unwrap();
        assert_eq!((p.current.text, p.corner_radius), (Some([9, 10, 11, 255]), None));
        let p = parse_palette(true, &[&base[..], &[9, 10, 11, 255, 20]].concat()).unwrap().unwrap();
        assert_eq!((p.current.text, p.corner_radius), (Some([9, 10, 11, 255]), Some(20)));
        assert!(parse_palette(false, &[]).unwrap().is_none());
        assert!(parse_palette(true, &[1, 2, 3]).is_err());
    }

    #[test]
    fn decodes_paired_palettes_and_rejects_invalid_pairs() {
        use crate::popup::{ColorScheme, MenuColors};

        let current = [1, 2, 3, 100, 4, 5, 6, 120, 7, 8, 9, 255];
        let alternate = [10, 11, 12, 200, 13, 14, 15, 220, 16, 17, 18, 240];
        let paired = [&[1, 20][..], &current, &alternate].concat();
        let p = parse_palette(true, &paired).unwrap().unwrap();
        assert_eq!(p.scheme, Some(ColorScheme::Dark));
        assert_eq!(p.corner_radius, Some(20));
        assert_eq!(p.current.panel, [1, 2, 3, 100]);
        assert_eq!(p.alternate, Some(MenuColors {
            panel: [10, 11, 12, 200],
            bar: [13, 14, 15, 220],
            text: Some([16, 17, 18, 240]),
        }));
        let p = parse_palette(true, &[&[0, 20][..], &current].concat()).unwrap().unwrap();
        assert_eq!(p.scheme, Some(ColorScheme::Light));
        assert!(p.alternate.is_none());
        assert!(parse_palette(true, &paired[..25]).is_err());
        assert!(parse_palette(false, &paired).is_err());
        assert!(parse_palette(true, &[&[2, 20][..], &current].concat()).is_err());
    }
}
