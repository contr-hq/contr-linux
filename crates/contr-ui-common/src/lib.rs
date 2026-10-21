//! Widgets, styles, icons and formatting the Contr Linux client's modules share.

pub mod format;
pub mod sparkline;
pub mod widgets;

use relm4::gtk::{gio, glib};

/// Registers the bundled styles and icons; must run before the application starts so `AdwApplication` loads
/// `style.css` from the resource base path.
pub fn register_resources() -> Result<(), glib::Error> {
    gio::resources_register_include!("resources.gresource")
}
