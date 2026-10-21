//! Widgets of the container list: one group per Compose project and one row per container, updated in place.

use std::collections::HashMap;

use contr_ui_common::widgets::{GroupHeader, boxed_list, icon_button, set_status};
use relm4::gtk::{self, gio, prelude::*};

use crate::list::ContainersMsg;
use crate::model::{GroupView, Port, RowView};

type Input = relm4::Sender<ContainersMsg>;

pub(crate) struct ContainerRow {
    pub(crate) row: gtk::ListBoxRow,
    status: gtk::Image,
    title: gtk::Label,
    failure: gtk::Label,
    subtitle: gtk::Label,
    link: gtk::LinkButton,
    exposed: gtk::Label,
    start: gtk::Button,
    stop: gtk::Button,
    terminal: gtk::Button,
    delete: gtk::Button,
}

impl ContainerRow {
    fn new(view: &RowView, input: &Input) -> Self {
        let status = gtk::Image::new();
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let failure = gtk::Label::builder()
            .css_classes(["row-subtitle-error", "group-description"])
            .build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "group-description"])
            .build();
        let subtitle_line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        subtitle_line.append(&failure);
        subtitle_line.append(&subtitle);
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        text.append(&title);
        text.append(&subtitle_line);

        let link = gtk::LinkButton::builder().valign(gtk::Align::Center).build();
        link.add_css_class("port-link");
        let exposed = gtk::Label::builder().css_classes(["dim-label", "port-link"]).build();
        let start = icon_button("contr-play-symbolic", &format!("Start {}", view.title));
        let stop = icon_button("contr-stop-symbolic", &format!("Stop {}", view.title));
        let terminal = icon_button("contr-terminal-symbolic", &format!("Open a terminal in {}", view.title));
        let delete = icon_button("contr-delete-symbolic", &format!("Delete {}", view.title));
        let chevron = gtk::Image::builder()
            .icon_name("go-next-symbolic")
            .css_classes(["dim-label"])
            .margin_start(4)
            .margin_end(6)
            .build();

        let content = gtk::Box::builder().spacing(12).css_classes(["container-row"]).build();
        for widget in [
            status.upcast_ref::<gtk::Widget>(),
            text.upcast_ref(),
            link.upcast_ref(),
            exposed.upcast_ref(),
            stop.upcast_ref(),
            start.upcast_ref(),
            terminal.upcast_ref(),
            delete.upcast_ref(),
            chevron.upcast_ref(),
        ] {
            content.append(widget);
        }
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(true)
            .name(&view.id)
            .build();

        for (button, message) in [
            (&start, ContainersMsg::Start as fn(String) -> ContainersMsg),
            (&stop, ContainersMsg::Stop),
            (&terminal, ContainersMsg::Terminal),
            (&delete, ContainersMsg::Delete),
        ] {
            let input = input.clone();
            let id = view.id.clone();
            button.connect_clicked(move |_| input.emit(message(id.clone())));
        }

        let row = Self {
            row,
            status,
            title,
            failure,
            subtitle,
            link,
            exposed,
            start,
            stop,
            terminal,
            delete,
        };
        row.update(view);
        row
    }

    fn update(&self, view: &RowView) {
        set_status(&self.status, view.status, view.status_word);
        self.title.set_label(&view.title);
        self.failure.set_label(view.failure.as_deref().unwrap_or_default());
        self.failure.set_visible(view.failure.is_some());
        self.subtitle.set_label(&view.subtitle);
        match &view.port {
            Some(Port::Link { label, url }) => {
                self.link.set_label(label);
                self.link.set_uri(url);
                self.link.set_visible(true);
                self.exposed.set_visible(false);
            }
            Some(Port::Exposed(label)) => {
                self.exposed.set_label(label);
                self.exposed.set_visible(true);
                self.link.set_visible(false);
            }
            None => {
                self.link.set_visible(false);
                self.exposed.set_visible(false);
            }
        }
        self.stop.set_visible(view.running);
        self.terminal.set_visible(view.running);
        self.start.set_visible(!view.running);
        self.delete.set_visible(!view.running);
    }
}

pub(crate) struct GroupWidget {
    pub(crate) root: gtk::Box,
    header: GroupHeader,
    start_all: gtk::Button,
    stop_all: gtk::Button,
    list: gtk::ListBox,
    rows: HashMap<String, ContainerRow>,
    order: Vec<String>,
}

impl GroupWidget {
    pub(crate) fn new(view: &GroupView, input: &Input) -> Self {
        let header = GroupHeader::new(&view.title, &view.description);
        let start_all = icon_button("contr-play-symbolic", &format!("Start all in {}", view.title));
        let stop_all = icon_button("contr-stop-symbolic", &format!("Stop all in {}", view.title));
        if let Some(project) = &view.project {
            header.suffix.append(&start_all);
            header.suffix.append(&stop_all);
            for (button, message) in [
                (&start_all, ContainersMsg::StartAll as fn(String) -> ContainersMsg),
                (&stop_all, ContainersMsg::StopAll),
            ] {
                let input = input.clone();
                let project = project.clone();
                button.connect_clicked(move |_| input.emit(message(project.clone())));
            }
            let menu = gio::Menu::new();
            let restart = gio::MenuItem::new(Some("Restart All"), None);
            restart.set_action_and_target_value(Some("containers.restart-all"), Some(&project.to_variant()));
            menu.append_item(&restart);
            let more = gtk::MenuButton::builder()
                .icon_name("view-more-symbolic")
                .menu_model(&menu)
                .tooltip_text(format!("More for {}", view.title))
                .valign(gtk::Align::Center)
                .css_classes(["flat", "circular"])
                .build();
            header.suffix.append(&more);
        }
        let list = boxed_list();
        let opener = input.clone();
        list.connect_row_activated(move |_, row| opener.emit(ContainersMsg::Activated(row.widget_name().to_string())));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&header.root);
        root.append(&list);
        let mut group = Self {
            root,
            header,
            start_all,
            stop_all,
            list,
            rows: HashMap::new(),
            order: Vec::new(),
        };
        group.update(view, input);
        group
    }

    pub(crate) fn update(&mut self, view: &GroupView, input: &Input) {
        self.header.title.set_label(&view.title);
        self.header.set_description(&view.description);
        self.start_all.set_visible(view.project.is_some() && view.any_stopped());
        self.stop_all.set_visible(view.project.is_some() && view.any_running());

        let order: Vec<String> = view.rows.iter().map(|row| row.id.clone()).collect();
        if order != self.order {
            while let Some(child) = self.list.first_child() {
                self.list.remove(&child);
            }
            self.rows.retain(|id, _| order.contains(id));
            for row in &view.rows {
                let widget = self
                    .rows
                    .entry(row.id.clone())
                    .or_insert_with(|| ContainerRow::new(row, input));
                self.list.append(&widget.row);
            }
            self.order = order;
        }
        for row in &view.rows {
            if let Some(widget) = self.rows.get(&row.id) {
                widget.update(row);
            }
        }
    }
}
