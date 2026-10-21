//! Settings of this client alone, kept apart from the daemon's settings file.

use std::fs;
use std::io;
use std::path::PathBuf;

use contr_core::product;
use relm4::gtk::glib;
use serde::{Deserialize, Serialize};

const FILE_NAME: &str = "desktop.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub(crate) struct ClientSettings {
    /// Closing the window hides it; only Quit ends the client.
    pub(crate) keep_running: bool,
    pub(crate) notify: Notify,
    /// The user finished or skipped first-run setup.
    pub(crate) onboarded: bool,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            keep_running: true,
            notify: Notify::default(),
            onboarded: false,
        }
    }
}

/// Which events send a desktop notification while the window is not focused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "snake_case")]
pub(crate) struct Notify {
    pub(crate) container_errors: bool,
    /// Pulls and Compose runs.
    pub(crate) finished: bool,
}

impl Default for Notify {
    fn default() -> Self {
        Self {
            container_errors: true,
            finished: false,
        }
    }
}

impl ClientSettings {
    fn path() -> PathBuf {
        glib::user_config_dir().join(product::SLUG).join(FILE_NAME)
    }

    /// The saved settings, or the defaults when none are saved or the file is unreadable.
    pub(crate) fn load() -> Self {
        fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub(crate) fn save(&self) -> io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings: ClientSettings = serde_json::from_str(r#"{"notify": {"finished": true}}"#).unwrap();
        assert!(settings.keep_running);
        assert!(settings.notify.container_errors);
        assert!(settings.notify.finished);
    }
}
