//! "Start at login" via the XDG autostart spec. deb/rpm ship a system entry in
//! /etc/xdg/autostart (on by default); a same-named user entry in
//! ~/.config/autostart overrides it, with Hidden=true meaning "off for this user".
use gtk::glib;
use std::{fs, path::PathBuf};

const FILE: &str = "harbor.desktop";

fn user_entry() -> PathBuf {
    glib::user_config_dir().join("autostart").join(FILE)
}

fn system_entry_exists() -> bool {
    glib::system_config_dirs()
        .iter()
        .any(|dir| dir.join("autostart").join(FILE).is_file())
}

/// The user entry decides when present; otherwise the system entry does.
fn enabled() -> bool {
    match fs::read_to_string(user_entry()) {
        Ok(entry) => !is_hidden(&entry),
        Err(_) => system_entry_exists(),
    }
}

fn is_hidden(entry: &str) -> bool {
    entry.lines().any(|line| {
        let line = line.trim();
        line.eq_ignore_ascii_case("Hidden=true") || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

/// How to launch this copy of harbor: the AppImage file when running from one,
/// the PATH command for packaged installs, else this executable (dev builds).
fn exec_command() -> String {
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return quote(&appimage.to_string_lossy());
    }
    match std::env::current_exe() {
        Ok(exe) if exe.starts_with("/usr") => "harbor".into(),
        Ok(exe) => quote(&exe.to_string_lossy()),
        Err(_) => "harbor".into(),
    }
}

/// Desktop-entry Exec quoting: wrap in double quotes, escape ", `, $ and \.
fn quote(path: &str) -> String {
    let mut quoted = String::from("\"");
    for ch in path.chars() {
        if matches!(ch, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

fn entry(enabled: bool) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=harbor\nComment=Localhost port monitor\nExec={} --hidden\nIcon=harbor\nNoDisplay=true\nHidden={}\nX-GNOME-Autostart-enabled={}\n",
        exec_command(),
        !enabled,
        enabled
    )
}

#[tauri::command]
pub(crate) fn autostart_enabled() -> bool {
    enabled()
}

#[tauri::command]
pub(crate) fn set_autostart(enabled: bool) -> Result<bool, String> {
    let path = user_entry();
    if enabled && system_entry_exists() {
        // The packaged system entry already starts harbor; drop any user "off" override.
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("Could not update autostart: {err}")),
        }
    } else {
        let write = || -> std::io::Result<()> {
            fs::create_dir_all(path.parent().expect("autostart dir"))?;
            fs::write(&path, entry(enabled))
        };
        write().map_err(|err| format!("Could not update autostart: {err}"))?;
    }
    Ok(self::enabled())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_entries_count_as_off() {
        assert!(is_hidden("[Desktop Entry]\nHidden=true\n"));
        assert!(is_hidden("[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"));
        assert!(!is_hidden("[Desktop Entry]\nHidden=false\nX-GNOME-Autostart-enabled=true\n"));
    }

    #[test]
    fn quotes_exec_paths_per_desktop_entry_spec() {
        assert_eq!(quote("/home/a b/harbor.AppImage"), "\"/home/a b/harbor.AppImage\"");
        assert_eq!(quote("/tmp/$x\"y`z\\"), "\"/tmp/\\$x\\\"y\\`z\\\\\"");
    }
}
