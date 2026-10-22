//! Contr for Linux: the GTK4 and libadwaita client of the Contr daemon.

mod app;
mod autostart;
mod client_settings;
mod engine_card;
mod notifications;
mod preferences;
mod sandbox;
mod sections;
mod shortcuts;
mod tray;
mod updates;

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

    let args: Vec<String> = std::env::args().collect();
    let background = args.iter().any(|arg| arg == autostart::BACKGROUND_ARG);
    let args = args
        .into_iter()
        .filter(|arg| arg != autostart::BACKGROUND_ARG)
        .collect();

    let app = RelmApp::new(product::APP_ID).with_args(args).visible_on_activate(false);
    // relm4 creates the application before it has an ID, so GLib derives no resource base path from it.
    relm4::main_application().set_resource_base_path(Some(&format!("/{}", product::APP_ID.replace('.', "/"))));
    app.run::<app::App>(background);
    Ok(())
}
