use std::collections::BTreeMap;
use std::time::{Instant, SystemTime};

use contr_containers_api::{ContainerTab, OpenContainer, RunImage};
use contr_daemon_ui::{Daemon, EngineGate, Status, describe, system_time, while_running};
use contr_sdk::api::containers::v1::{Container, WatchContainersRequest, watch_containers_response};
use contr_sdk::api::images::v1::{
    Image, LayerProgress, PruneImagesRequest, PullImageRequest, RemoveImageRequest, WatchImagesRequest,
    pull_image_response, watch_images_response,
};
use contr_sdk::{Client, Error};
use contr_ui_common::format;
use contr_ui_common::widgets::{GroupHeader, boxed_list, icon_button, page_column};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::model::{self, ImageView, SHOWN, Sort};

/// The Images section: images used by containers, unused ones, and pulls in progress.
pub struct ImagesPage {
    status: Status,
    images: BTreeMap<String, Image>,
    containers: BTreeMap<String, Container>,
    loaded: bool,
    query: String,
    sort: Sort,
    show_all: bool,
    pulls: Vec<Pull>,
    next_pull: u64,
}

struct Pull {
    id: u64,
    reference: String,
    layers: Vec<LayerProgress>,
    started: Instant,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Debug)]
pub enum ImagesMsg {
    Search(String),
    Sort(Sort),
    AskPull,
    Pull(String),
    CancelPull(u64),
    Run(String),
    Open(String),
    Remove(String),
    ConfirmRemove { reference: String, force: bool },
    Copy(String),
    Prune,
    ConfirmPrune,
    ShowAll,
}

#[derive(Debug)]
pub enum ImagesOutput {
    OpenContainer(OpenContainer),
    Run(RunImage),
    /// A pull finished; carries the reference.
    Pulled(String),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum ImagesCmd {
    Status(Box<Status>),
    Images(ImageEvent),
    Containers(ContainerEvent),
    PullLayer(u64, LayerProgress),
    PullDone(u64, Result<String, String>),
    Done(Result<String, String>),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum ImageEvent {
    Snapshot(Vec<Image>),
    Upserted(Box<Image>),
    Removed(String),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum ContainerEvent {
    Snapshot(Vec<Container>),
    Upserted(Box<Container>),
    Removed(String),
}

pub struct ImagesWidgets {
    title: adw::WindowTitle,
    search: gtk::SearchBar,
    search_button: gtk::ToggleButton,
    pull_button: gtk::Button,
    sort_button: gtk::MenuButton,
    content: gtk::Stack,
    pulling: gtk::Box,
    pull_cards: gtk::Box,
    used: gtk::Box,
    used_list: gtk::ListBox,
    unused: gtk::Box,
    unused_list: gtk::ListBox,
    prune: gtk::Button,
}

impl Component for ImagesPage {
    type CommandOutput = ImagesCmd;
    type Input = ImagesMsg;
    type Output = ImagesOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = ImagesWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let title = adw::WindowTitle::new("Images", "");
        let pull_button = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("contr-pull-symbolic")
                    .label("Pull")
                    .build(),
            )
            .tooltip_text("Pull an Image")
            .build();
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search")
            .build();
        let sort_menu = gio::Menu::new();
        for (label, target) in [("Name", "name"), ("Size", "size"), ("Newest", "newest")] {
            let item = gio::MenuItem::new(Some(label), None);
            item.set_action_and_target_value(Some("images.sort"), Some(&target.to_variant()));
            sort_menu.append_item(&item);
        }
        let sort_section = gio::Menu::new();
        sort_section.append_section(Some("Sort By"), &sort_menu);
        let sort_button = gtk::MenuButton::builder()
            .icon_name("view-sort-descending-symbolic")
            .tooltip_text("Sort")
            .menu_model(&sort_section)
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&pull_button);
        header.pack_end(&sort_button);
        header.pack_end(&search_button);

        let entry = gtk::SearchEntry::builder().placeholder_text("Search images").build();
        let search = gtk::SearchBar::builder()
            .child(&adw::Clamp::builder().maximum_size(480).child(&entry).build())
            .build();
        search.connect_entry(&entry);
        search_button
            .bind_property("active", &search, "search-mode-enabled")
            .bidirectional()
            .build();
        let input = sender.input_sender().clone();
        entry.connect_search_changed(move |entry| input.emit(ImagesMsg::Search(entry.text().to_string())));

        let pulling_header = GroupHeader::new("Pulling", "");
        let pull_cards = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        let pulling = section(&pulling_header, &pull_cards);

        let used_header = GroupHeader::new("Used by containers", "");
        let used_list = boxed_list();
        let used = section(&used_header, &used_list);

        let unused_header = GroupHeader::new("Not used", "Safe to remove. You can pull or build them again later.");
        let prune = gtk::Button::builder()
            .valign(gtk::Align::End)
            .css_classes(["destructive-action"])
            .build();
        unused_header.suffix.append(&prune);
        let unused_list = boxed_list();
        let unused = section(&unused_header, &unused_list);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(28)
            .build();
        column.append(&pulling);
        column.append(&used);
        column.append(&unused);

        let pull_first = gtk::Button::builder()
            .label("Pull an Image")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("contr-images-symbolic")
            .title("No Images Yet")
            .description("Pull an image to run containers from it.")
            .child(&pull_first)
            .vexpand(true)
            .build();
        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&page_column(&column), Some("list"));
        content.add_named(&empty, Some("empty"));
        content.add_named(&loading, Some("loading"));

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&search);
        body.append(&content);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(&daemon, "contr-images-symbolic", &body)));
        root.set_child(Some(&toolbar));

        for button in [&pull_button, &pull_first] {
            let input = sender.input_sender().clone();
            button.connect_clicked(move |_| input.emit(ImagesMsg::AskPull));
        }
        let input = sender.input_sender().clone();
        prune.connect_clicked(move |_| input.emit(ImagesMsg::Prune));
        install_actions(&root, &sender);
        follow(&daemon, &sender);

        let model = ImagesPage {
            status: daemon.status(),
            images: BTreeMap::new(),
            containers: BTreeMap::new(),
            loaded: false,
            query: String::new(),
            sort: Sort::default(),
            show_all: false,
            pulls: Vec::new(),
            next_pull: 0,
        };
        let widgets = ImagesWidgets {
            title,
            search,
            search_button,
            pull_button,
            sort_button,
            content,
            pulling,
            pull_cards,
            used,
            used_list,
            unused,
            unused_list,
            prune,
        };
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ImagesMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ImagesMsg::Search(query) => self.query = query,
            ImagesMsg::Sort(sort) => self.sort = sort,
            ImagesMsg::AskPull => ask_pull(root, sender.input_sender()),
            ImagesMsg::Pull(reference) => self.pull(&sender, reference),
            ImagesMsg::CancelPull(id) => {
                if let Some(index) = self.pulls.iter().position(|pull| pull.id == id) {
                    let pull = self.pulls.remove(index);
                    pull.task.abort();
                    root.add_toast(adw::Toast::new(&format!("Stopped pulling {}", pull.reference)));
                }
            }
            ImagesMsg::Run(reference) => {
                let _ = sender.output(ImagesOutput::Run(RunImage { image: reference }));
            }
            ImagesMsg::Open(id) => {
                let _ = sender.output(ImagesOutput::OpenContainer(OpenContainer {
                    id,
                    tab: ContainerTab::Logs,
                }));
            }
            ImagesMsg::Remove(reference) => {
                let users = self
                    .images
                    .values()
                    .find(|image| image.repo_tags.contains(&reference) || image.id == reference)
                    .map_or(0, |image| {
                        self.containers
                            .values()
                            .filter(|container| container.image_id == image.id)
                            .count()
                    });
                confirm_remove(root, &reference, users, sender.input_sender());
            }
            ImagesMsg::ConfirmRemove { reference, force } => self.call(&sender, move |client| async move {
                let request = RemoveImageRequest {
                    reference: reference.clone(),
                    force,
                    keep_parents: false,
                };
                client.images().remove_image(request).await?;
                Ok(format!("Removed {reference}"))
            }),
            ImagesMsg::Copy(reference) => {
                root.clipboard().set_text(&reference);
                root.add_toast(adw::Toast::new("Copied image reference"));
            }
            ImagesMsg::Prune => confirm_prune(root, sender.input_sender()),
            ImagesMsg::ConfirmPrune => self.call(&sender, |client| async move {
                let response = client
                    .images()
                    .prune_images(PruneImagesRequest { all: true })
                    .await?
                    .into_inner();
                Ok(match response.deleted.len() {
                    0 => "No unused images to remove".to_owned(),
                    _ => format!(
                        "Removed unused images · freed {}",
                        format::bytes(response.reclaimed_bytes)
                    ),
                })
            }),
            ImagesMsg::ShowAll => self.show_all = true,
        }
        self.refresh(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ImagesCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ImagesCmd::Status(status) => {
                self.status = *status;
                if !self.status.is_running() {
                    self.images.clear();
                    self.containers.clear();
                    self.loaded = false;
                }
            }
            ImagesCmd::Images(ImageEvent::Snapshot(images)) => {
                self.images = images.into_iter().map(|image| (image.id.clone(), image)).collect();
                self.loaded = true;
            }
            ImagesCmd::Images(ImageEvent::Upserted(image)) => {
                self.images.insert(image.id.clone(), *image);
            }
            ImagesCmd::Images(ImageEvent::Removed(id)) => {
                self.images.remove(&id);
            }
            ImagesCmd::Containers(ContainerEvent::Snapshot(containers)) => {
                self.containers = containers.into_iter().map(|c| (c.id.clone(), c)).collect();
            }
            ImagesCmd::Containers(ContainerEvent::Upserted(container)) => {
                self.containers.insert(container.id.clone(), *container);
            }
            ImagesCmd::Containers(ContainerEvent::Removed(id)) => {
                self.containers.remove(&id);
            }
            ImagesCmd::PullLayer(id, layer) => {
                // The engine reports "Pulling from <repository>" under the tag's name, not a layer's.
                if layer.status.starts_with("Pulling from") {
                    return;
                }
                if let Some(pull) = self.pulls.iter_mut().find(|pull| pull.id == id) {
                    match pull.layers.iter_mut().find(|known| known.layer_id == layer.layer_id) {
                        Some(known) => {
                            let total = known.total_bytes.max(layer.total_bytes);
                            *known = layer;
                            known.total_bytes = total;
                        }
                        None => pull.layers.push(layer),
                    }
                }
                self.refresh_pulls(widgets, &sender);
                return;
            }
            ImagesCmd::PullDone(id, result) => {
                let reference = self
                    .pulls
                    .iter()
                    .position(|pull| pull.id == id)
                    .map(|index| self.pulls.remove(index).reference);
                match (result, reference) {
                    (Ok(_), Some(reference)) => {
                        let _ = sender.output(ImagesOutput::Pulled(reference.clone()));
                        let toast = adw::Toast::builder()
                            .title(format!("Pulled {reference}"))
                            .button_label("Run")
                            .build();
                        let input = sender.input_sender().clone();
                        toast.connect_button_clicked(move |_| input.emit(ImagesMsg::Run(reference.clone())));
                        root.add_toast(toast);
                    }
                    (Err(message), _) => root.add_toast(adw::Toast::new(&message)),
                    (Ok(_), None) => {}
                }
            }
            ImagesCmd::Done(Ok(message) | Err(message)) => {
                if !message.is_empty() {
                    root.add_toast(adw::Toast::new(&message));
                }
            }
        }
        self.refresh(widgets, &sender);
    }
}

impl ImagesPage {
    fn refresh(&self, widgets: &mut ImagesWidgets, sender: &ComponentSender<Self>) {
        let running = self.status.is_running();
        let total: u64 = self.images.values().map(|image| image.size_bytes).sum();
        widgets.title.set_subtitle(&if running && self.loaded {
            match self.images.len() {
                1 => format!("1 image · {}", format::bytes(total)),
                count => format!("{count} images · {}", format::bytes(total)),
            }
        } else {
            String::new()
        });
        for widget in [
            widgets.search_button.upcast_ref::<gtk::Widget>(),
            widgets.pull_button.upcast_ref(),
            widgets.sort_button.upcast_ref(),
        ] {
            widget.set_sensitive(running);
        }
        if !running {
            widgets.search.set_search_mode(false);
        }
        widgets
            .content
            .set_visible_child_name(match (self.loaded, self.images.is_empty() && self.pulls.is_empty()) {
                (false, _) => "loading",
                (true, true) => "empty",
                (true, false) => "list",
            });

        let sections = model::sections(
            &self.images,
            &self.containers,
            |image| system_time(image.created_at.as_ref()),
            &self.query,
            self.sort,
        );
        let now = SystemTime::now();
        let input = sender.input_sender();

        clear(&widgets.used_list);
        let limit = if self.show_all || !self.query.is_empty() {
            usize::MAX
        } else {
            SHOWN
        };
        for view in sections.used.iter().take(limit) {
            widgets.used_list.append(&image_row(view, now, input));
        }
        if sections.used.len() > limit {
            widgets
                .used_list
                .append(&show_all_row(sections.used.len() - limit, input));
        }
        widgets.used.set_visible(!sections.used.is_empty());

        clear(&widgets.unused_list);
        for view in &sections.unused {
            widgets.unused_list.append(&image_row(view, now, input));
        }
        if sections.untagged > 0 {
            widgets
                .unused_list
                .append(&untagged_row(sections.untagged, sections.untagged_size));
        }
        widgets
            .prune
            .set_label(&format!("Remove {}", format::bytes(sections.unused_size)));
        widgets
            .unused
            .set_visible(!sections.unused.is_empty() || sections.untagged > 0);

        self.refresh_pulls(widgets, sender);
    }

    fn refresh_pulls(&self, widgets: &ImagesWidgets, sender: &ComponentSender<Self>) {
        while let Some(child) = widgets.pull_cards.first_child() {
            widgets.pull_cards.remove(&child);
        }
        for pull in &self.pulls {
            widgets.pull_cards.append(&pull_card(pull, sender.input_sender()));
        }
        widgets.pulling.set_visible(!self.pulls.is_empty());
    }

    fn pull(&mut self, sender: &ComponentSender<Self>, reference: String) {
        let reference = reference.trim().to_owned();
        let Some(client) = self.status.running_client().cloned() else {
            return;
        };
        if reference.is_empty() || self.pulls.iter().any(|pull| pull.reference == reference) {
            return;
        }
        let id = self.next_pull;
        self.next_pull += 1;
        let out = sender.command_sender().clone();
        let request = PullImageRequest {
            reference: reference.clone(),
            platform: String::new(),
        };
        let task = relm4::spawn(async move {
            let result = async {
                let mut stream = client.images().pull_image(request).await?.into_inner();
                while let Some(response) = stream.message().await? {
                    match response.event {
                        Some(pull_image_response::Event::Layer(layer)) => out.emit(ImagesCmd::PullLayer(id, layer)),
                        Some(pull_image_response::Event::Pulled(pulled)) => return Ok(pulled.reference),
                        Some(pull_image_response::Event::Status(_)) | None => {}
                    }
                }
                Err(Error::Protocol("the pull ended without an image"))
            }
            .await;
            out.emit(ImagesCmd::PullDone(id, result.map_err(|error| describe(&error))));
        });
        self.pulls.push(Pull {
            id,
            reference,
            layers: Vec::new(),
            started: Instant::now(),
            task,
        });
    }

    fn call<F, Fut>(&self, sender: &ComponentSender<Self>, call: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, Error>> + Send,
    {
        let Some(client) = self.status.running_client().cloned() else {
            return;
        };
        sender.oneshot_command(async move { ImagesCmd::Done(call(client).await.map_err(|error| describe(&error))) });
    }
}

fn section(header: &GroupHeader, child: &impl IsA<gtk::Widget>) -> gtk::Box {
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.append(&header.root);
    section.append(child);
    section
}

fn clear(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn image_row(view: &ImageView, now: SystemTime, input: &relm4::Sender<ImagesMsg>) -> gtk::ListBoxRow {
    let repository = gtk::Label::builder().label(&view.repository).xalign(0.0).build();
    let tag = gtk::Label::builder()
        .label(&view.tag)
        .css_classes(["dim-label"])
        .build();
    let title = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    title.append(&repository);
    title.append(&tag);

    let mut subtitle = glib::markup_escape_text(&view.summary(now)).to_string();
    if !view.users.is_empty() {
        let links: Vec<String> = view
            .users
            .iter()
            .map(|user| {
                format!(
                    "<a href=\"{}\">{}</a>",
                    glib::markup_escape_text(&user.id),
                    glib::markup_escape_text(&user.name)
                )
            })
            .collect();
        subtitle = format!("{subtitle} · used by {}", links.join(", "));
    }
    let subtitle = gtk::Label::builder()
        .label(&subtitle)
        .use_markup(true)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "group-description"])
        .build();
    let open = input.clone();
    subtitle.connect_activate_link(move |_, id| {
        open.emit(ImagesMsg::Open(id.to_owned()));
        glib::Propagation::Stop
    });

    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(&title);
    text.append(&subtitle);

    let run = icon_button("contr-play-symbolic", &format!("Run {}", view.reference));
    let reference = view.reference.clone();
    let run_input = input.clone();
    run.connect_clicked(move |_| run_input.emit(ImagesMsg::Run(reference.clone())));

    let menu = gio::Menu::new();
    for (label, action) in [
        ("Run…", "images.run"),
        ("Copy Reference", "images.copy"),
        ("Pull Again", "images.pull"),
    ] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(Some(action), Some(&view.reference.to_variant()));
        menu.append_item(&item);
    }
    let remove = gio::Menu::new();
    let item = gio::MenuItem::new(Some("Remove…"), None);
    item.set_action_and_target_value(Some("images.remove"), Some(&view.reference.to_variant()));
    remove.append_item(&item);
    menu.append_section(None, &remove);
    let more = gtk::MenuButton::builder()
        .icon_name("view-more-symbolic")
        .menu_model(&menu)
        .tooltip_text(format!("More for {}", view.reference))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();

    let content = gtk::Box::builder().spacing(12).css_classes(["container-row"]).build();
    content.append(&text);
    content.append(&run);
    content.append(&more);
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

fn show_all_row(hidden: usize, input: &relm4::Sender<ImagesMsg>) -> gtk::ListBoxRow {
    let link = gtk::LinkButton::builder()
        .label(format!("Show {hidden} more"))
        .uri("show-all:")
        .halign(gtk::Align::Start)
        .margin_start(6)
        .margin_top(4)
        .margin_bottom(4)
        .build();
    let input = input.clone();
    link.connect_activate_link(move |_| {
        input.emit(ImagesMsg::ShowAll);
        glib::Propagation::Stop
    });
    gtk::ListBoxRow::builder().child(&link).activatable(false).build()
}

fn untagged_row(count: usize, size: u64) -> gtk::ListBoxRow {
    let title = gtk::Label::builder()
        .label(if count == 1 {
            "1 untagged image".to_owned()
        } else {
            format!("{count} untagged images")
        })
        .xalign(0.0)
        .css_classes(["italic"])
        .build();
    let subtitle = gtk::Label::builder()
        .label("Left over from builds and pulls of newer versions")
        .xalign(0.0)
        .css_classes(["dim-label", "group-description"])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(&title);
    text.append(&subtitle);
    let size = gtk::Label::builder()
        .label(format::bytes(size))
        .css_classes(["dim-label"])
        .build();
    let content = gtk::Box::builder()
        .spacing(12)
        .css_classes(["container-row"])
        .margin_end(6)
        .build();
    content.append(&text);
    content.append(&size);
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

fn pull_card(pull: &Pull, input: &relm4::Sender<ImagesMsg>) -> gtk::Box {
    let (fraction, summary) = model::pull_summary(&pull.layers, pull.started.elapsed());
    let reference = gtk::Label::builder().label(&pull.reference).xalign(0.0).build();
    let summary = gtk::Label::builder()
        .label(&summary)
        .xalign(0.0)
        .css_classes(["dim-label", "group-description"])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    text.append(&reference);
    text.append(&summary);
    let cancel = icon_button("window-close-symbolic", &format!("Stop pulling {}", pull.reference));
    let id = pull.id;
    let input = input.clone();
    cancel.connect_clicked(move |_| input.emit(ImagesMsg::CancelPull(id)));
    let top = gtk::Box::builder().spacing(12).build();
    top.append(&text);
    top.append(&cancel);
    let bar = gtk::ProgressBar::builder().fraction(fraction).build();

    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .css_classes(["pull-head"])
        .build();
    head.append(&top);
    head.append(&bar);

    let layers = gtk::Grid::builder()
        .column_spacing(14)
        .row_spacing(6)
        .css_classes(["pull-layers"])
        .build();
    for (row, layer) in pull.layers.iter().map(model::layer_view).enumerate() {
        let row = i32::try_from(row).unwrap_or(i32::MAX);
        let id = gtk::Label::builder()
            .label(&layer.id)
            .xalign(0.0)
            .css_classes(["dim-label", "mono", "group-description"])
            .build();
        let bar = gtk::ProgressBar::builder()
            .fraction(layer.fraction)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let status = gtk::Label::builder()
            .label(&layer.text)
            .xalign(0.0)
            .width_chars(16)
            .css_classes(["group-description"])
            .build();
        if layer.done {
            bar.add_css_class("success");
            status.add_css_class("status-running");
        }
        layers.attach(&id, 0, row, 1, 1);
        layers.attach(&bar, 1, row, 1, 1);
        layers.attach(&status, 2, row, 1, 1);
    }

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["card"])
        .build();
    card.append(&head);
    if !pull.layers.is_empty() {
        card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        card.append(&layers);
    }
    card
}

fn install_actions(root: &adw::ToastOverlay, sender: &ComponentSender<ImagesPage>) {
    let actions = gio::SimpleActionGroup::new();
    for (name, message) in [
        ("run", ImagesMsg::Run as fn(String) -> ImagesMsg),
        ("copy", ImagesMsg::Copy),
        ("pull", ImagesMsg::Pull),
        ("remove", ImagesMsg::Remove),
    ] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let input = sender.input_sender().clone();
        action.connect_activate(move |_, target| {
            if let Some(reference) = target.and_then(glib::Variant::get::<String>) {
                input.emit(message(reference));
            }
        });
        actions.add_action(&action);
    }
    let sort = gio::SimpleAction::new_stateful("sort", Some(glib::VariantTy::STRING), &"name".to_variant());
    let input = sender.input_sender().clone();
    sort.connect_activate(move |action, target| {
        let Some(target) = target else { return };
        action.set_state(target);
        input.emit(ImagesMsg::Sort(match target.get::<String>().as_deref() {
            Some("size") => Sort::Size,
            Some("newest") => Sort::Newest,
            _ => Sort::Name,
        }));
    });
    actions.add_action(&sort);
    root.insert_action_group("images", Some(&actions));
}

fn ask_pull(parent: &adw::ToastOverlay, input: &relm4::Sender<ImagesMsg>) {
    let dialog = adw::AlertDialog::new(
        Some("Pull an Image"),
        Some("A reference without a tag pulls the latest tag."),
    );
    let entry = gtk::Entry::builder()
        .placeholder_text("nginx:alpine")
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("pull", "Pull")]);
    dialog.set_response_appearance("pull", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("pull"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let focus = entry.clone();
    dialog.connect_response(Some("pull"), move |_, _| {
        input.emit(ImagesMsg::Pull(entry.text().to_string()));
    });
    dialog.present(Some(parent));
    dialog.set_focus(Some(&focus));
}

fn confirm_remove(parent: &adw::ToastOverlay, reference: &str, users: usize, input: &relm4::Sender<ImagesMsg>) {
    let body = match users {
        0 => "The image is deleted from this computer. You can pull it again later.".to_owned(),
        1 => "A container uses this image. Removing it anyway keeps the container and untags the image.".to_owned(),
        count => format!("{count} containers use this image. Removing it anyway keeps them and untags the image."),
    };
    let dialog = adw::AlertDialog::new(Some(&format!("Remove {reference}?")), Some(&body));
    let label = if users == 0 { "Remove" } else { "Remove Anyway" };
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", label)]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let reference = reference.to_owned();
    dialog.connect_response(Some("remove"), move |_, _| {
        input.emit(ImagesMsg::ConfirmRemove {
            reference: reference.clone(),
            force: users > 0,
        });
    });
    dialog.present(Some(parent));
}

fn confirm_prune(parent: &adw::ToastOverlay, input: &relm4::Sender<ImagesMsg>) {
    let dialog = adw::AlertDialog::new(
        Some("Remove Unused Images?"),
        Some("Every image no container uses is deleted. You can pull or build them again later."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    dialog.connect_response(Some("remove"), move |_, _| input.emit(ImagesMsg::ConfirmPrune));
    dialog.present(Some(parent));
}

fn follow(daemon: &Daemon, sender: &ComponentSender<ImagesPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(ImagesCmd::Status(Box::new(status.borrow_and_update().clone())));
                    if status.changed().await.is_err() {
                        return;
                    }
                }
            })
            .drop_on_shutdown()
    });

    let status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(while_running(status, move |client| {
                let out = out.clone();
                async move {
                    let Ok(stream) = client.images().watch_images(WatchImagesRequest {}).await else {
                        return;
                    };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(ImagesCmd::Images(match response.event {
                            Some(watch_images_response::Event::Snapshot(snapshot)) => {
                                ImageEvent::Snapshot(snapshot.images)
                            }
                            Some(watch_images_response::Event::Upserted(image)) => {
                                ImageEvent::Upserted(Box::new(image))
                            }
                            Some(watch_images_response::Event::RemovedId(id)) => ImageEvent::Removed(id),
                            None => continue,
                        }));
                    }
                }
            }))
            .drop_on_shutdown()
    });

    let status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(while_running(status, move |client| {
                let out = out.clone();
                async move {
                    let request = client.containers().watch_containers(WatchContainersRequest {}).await;
                    let Ok(stream) = request else { return };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(ImagesCmd::Containers(match response.event {
                            Some(watch_containers_response::Event::Snapshot(snapshot)) => {
                                ContainerEvent::Snapshot(snapshot.containers)
                            }
                            Some(watch_containers_response::Event::Upserted(container)) => {
                                ContainerEvent::Upserted(Box::new(container))
                            }
                            Some(watch_containers_response::Event::RemovedId(id)) => ContainerEvent::Removed(id),
                            None => continue,
                        }));
                    }
                }
            }))
            .drop_on_shutdown()
    });
}
