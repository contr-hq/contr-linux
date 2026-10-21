//! Contr for Linux: the GTK4 and libadwaita client of the Contr daemon.

mod app;
mod client_settings;
mod engine_card;
mod sections;
mod tray;

use contr_core::product;
use relm4::RelmApp;
use relm4::gtk::prelude::*;
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_env(product::LOG_ENV).unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    contr_ui_common::register_resources()?;
    let _ = relm4::RELM_THREADS.set(2);

    let app = RelmApp::new(product::APP_ID);
    // relm4 creates the application before it has an ID, so GLib derives no resource base path from it.
    relm4::main_application().set_resource_base_path(Some(&format!("/{}", product::APP_ID.replace('.', "/"))));
    app.run::<app::App>(());
    Ok(())
}
