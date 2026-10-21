//! Small widgets the design repeats across pages: status icons, group headers and the page clamp.

use relm4::adw;
use relm4::gtk::{self, prelude::*};

/// How a container or check is doing, as the design colours it; every icon also carries its word as a tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusKind {
    Running,
    Failed,
    Stopped,
    Created,
    Paused,
}

impl StatusKind {
    fn icon_name(self) -> &'static str {
        match self {
            Self::Running => "contr-running-symbolic",
            Self::Failed => "contr-failed-symbolic",
            Self::Stopped => "contr-stopped-symbolic",
            Self::Created => "contr-created-symbolic",
            Self::Paused => "contr-paused-symbolic",
        }
    }

    fn css_class(self) -> &'static str {
        match self {
            Self::Running => "status-running",
            Self::Failed => "status-failed",
            Self::Stopped => "status-stopped",
            Self::Created => "status-created",
            Self::Paused => "status-paused",
        }
    }
}

/// Sets `image` to the icon and colour of `kind`, labelled `word` for tooltips and screen readers.
pub fn set_status(image: &gtk::Image, kind: StatusKind, word: &str) {
    for other in [
        StatusKind::Running,
        StatusKind::Failed,
        StatusKind::Stopped,
        StatusKind::Created,
        StatusKind::Paused,
    ] {
        image.remove_css_class(other.css_class());
    }
    image.set_icon_name(Some(kind.icon_name()));
    image.add_css_class(kind.css_class());
    image.set_tooltip_text(Some(word));
    image.update_property(&[gtk::accessible::Property::Label(word)]);
}

/// A status icon for `kind`.
pub fn status_icon(kind: StatusKind, word: &str) -> gtk::Image {
    let image = gtk::Image::new();
    set_status(&image, kind, word);
    image
}

/// Title and description above a boxed list, with a box at the end for the group's buttons.
pub struct GroupHeader {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub description: gtk::Label,
    pub suffix: gtk::Box,
}

impl GroupHeader {
    pub fn new(title: &str, description: &str) -> Self {
        let title = gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["group-title"])
            .build();
        let description = gtk::Label::builder()
            .label(description)
            .xalign(0.0)
            .wrap(true)
            .visible(!description.is_empty())
            .css_classes(["group-description", "dim-label"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .valign(gtk::Align::End)
            .build();
        text.append(&title);
        text.append(&description);
        let suffix = gtk::Box::builder().spacing(6).valign(gtk::Align::End).build();
        let root = gtk::Box::builder().spacing(6).margin_bottom(10).build();
        root.append(&text);
        root.append(&suffix);
        Self {
            root,
            title,
            description,
            suffix,
        }
    }

    pub fn set_description(&self, description: &str) {
        self.description.set_label(description);
        self.description.set_visible(!description.is_empty());
    }
}

/// A boxed list in the design's card style.
pub fn boxed_list() -> gtk::ListBox {
    gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build()
}

/// A flat circular icon button with a tooltip and an accessible label.
pub fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

/// The scrolled, centred column every list page uses: 860 px wide at most, as in the design.
pub fn page_column(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(860)
        .tightening_threshold(600)
        .margin_top(24)
        .margin_bottom(40)
        .margin_start(16)
        .margin_end(16)
        .child(child)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
}
