//! The desktop session the client runs in.

use relm4::gtk::{gio, glib, prelude::*};

/// Asks GNOME's session manager to log out; it shows its own confirmation.
pub async fn log_out() -> Result<(), glib::Error> {
    let bus = gio::bus_get_future(gio::BusType::Session).await?;
    bus.call_future(
        Some("org.gnome.SessionManager"),
        "/org/gnome/SessionManager",
        "org.gnome.SessionManager",
        "Logout",
        Some(&(0_u32,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
    )
    .await?;
    Ok(())
}
