//! The XDG autostart entry that starts the client, without its window, when the user logs in.

use std::fs;
use std::io;
use std::path::PathBuf;

use contr_core::product;
use relm4::gtk::glib;

/// Argument the autostart entry passes so the client starts with its window hidden.
pub(crate) const BACKGROUND_ARG: &str = "--background";

fn path() -> PathBuf {
    glib::user_config_dir()
        .join("autostart")
        .join(format!("{}.desktop", product::APP_ID))
}

pub(crate) fn enabled() -> bool {
    path().is_file()
}

pub(crate) fn set(enabled: bool) -> io::Result<()> {
    let path = path();
    if !enabled {
        return match fs::remove_file(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    }
    let exe = std::env::current_exe()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(
        &path,
        format!(
            "[Desktop Entry]\nType=Application\nName={}\nExec=\"{}\" {BACKGROUND_ARG}\nIcon={}\nX-GNOME-Autostart-enabled=true\n",
            product::DISPLAY_NAME,
            exe.display(),
            product::APP_ID
        ),
    )
}
