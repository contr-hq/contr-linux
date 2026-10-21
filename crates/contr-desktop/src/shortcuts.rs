//! The Keyboard Shortcuts dialog.

use contr_core::product;
use relm4::adw::{self, prelude::*};
use relm4::gtk;

pub(crate) fn present(parent: &impl IsA<gtk::Widget>) {
    let general = adw::ShortcutsSection::new(Some("General"));
    for (title, accelerator) in [
        ("Preferences", "<Control>comma"),
        ("Keyboard Shortcuts", "<Control>question"),
        ("Close Window", "<Control>w"),
        (&*format!("Quit {}", product::DISPLAY_NAME), "<Control>q"),
    ] {
        general.add(adw::ShortcutsItem::new(title, accelerator));
    }
    let pages = adw::ShortcutsSection::new(Some("Pages"));
    for (title, accelerator) in [
        ("Next Match in Logs", "<Control>g"),
        ("Previous Match in Logs", "<Control><Shift>g"),
    ] {
        pages.add(adw::ShortcutsItem::new(title, accelerator));
    }
    let dialog = adw::ShortcutsDialog::new();
    dialog.add(general);
    dialog.add(pages);
    dialog.present(Some(parent));
}
