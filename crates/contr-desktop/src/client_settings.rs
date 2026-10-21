//! Settings of this client alone, kept apart from the daemon's settings file.

use std::fs;
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
    pub(crate) notify_container_errors: bool,
    pub(crate) notify_finished: bool,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            keep_running: true,
            notify_container_errors: true,
            notify_finished: false,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_take_their_defaults() {
        let settings: ClientSettings = serde_json::from_str(r#"{"notify_finished": true}"#).unwrap();
        assert!(settings.keep_running);
        assert!(settings.notify_finished);
    }
}
