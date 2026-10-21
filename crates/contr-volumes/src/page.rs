use std::collections::{BTreeMap, HashSet};
use std::time::SystemTime;

use contr_containers_api::{ContainerTab, OpenContainer};
use contr_daemon_ui::{
    Attachment, Daemon, EngineGate, Status, describe, engine_kind, follow_attachments, system_time, while_running,
};
use contr_sdk::api::volumes::v1::{
    CreateVolumeRequest, PruneVolumesRequest, RemoveVolumeRequest, Volume, WatchVolumesRequest, watch_volumes_response,
};
use contr_sdk::{Client, Error};
use contr_ui_common::format;
use contr_ui_common::widgets::{GroupHeader, boxed_list, page_column};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::model::{self, VolumeView};

/// The Volumes section: volumes containers mount, and the rest.
pub struct VolumesPage {
    status: Status,
    volumes: BTreeMap<String, Volume>,
    attachments: Vec<Attachment>,
    loaded: bool,
    query: String,
    expanded: HashSet<String>,
}

#[derive(Debug)]
pub enum VolumesMsg {
    Search(String),
    Expanded(String, bool),
    Open(String),
    Copy(String),
    Delete(String),
    ConfirmDelete(String),
    Create,
    ConfirmCreate(String),
    Prune,
    ConfirmPrune,
}

#[derive(Debug)]
pub enum VolumesOutput {
    OpenContainer(OpenContainer),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum VolumesCmd {
    Status(Box<Status>),
    Snapshot(Vec<Volume>),
    Upserted(Box<Volume>),
    Removed(String),
    Attachments(Vec<Attachment>),
    Done(Result<String, String>),
}

pub struct VolumesWidgets {
    title: adw::WindowTitle,
    search: gtk::SearchBar,
    search_button: gtk::ToggleButton,
    create: gtk::Button,
    content: gtk::Stack,
    used_header: GroupHeader,
    used: gtk::Box,
    used_list: gtk::ListBox,
    unused: gtk::Box,
    unused_list: gtk::ListBox,
}

impl Component for VolumesPage {
    type CommandOutput = VolumesCmd;
    type Input = VolumesMsg;
    type Output = VolumesOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = VolumesWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let title = adw::WindowTitle::new("Volumes", "");
        let create = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Create")
                    .build(),
            )
            .tooltip_text("Create a Volume")
            .build();
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search")
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&create);
        header.pack_end(&search_button);

        let entry = gtk::SearchEntry::builder().placeholder_text("Search volumes").build();
        let search = gtk::SearchBar::builder()
            .child(&adw::Clamp::builder().maximum_size(480).child(&entry).build())
            .build();
        search.connect_entry(&entry);
        search_button
            .bind_property("active", &search, "search-mode-enabled")
            .bidirectional()
            .build();
        let input = sender.input_sender().clone();
        entry.connect_search_changed(move |entry| input.emit(VolumesMsg::Search(entry.text().to_string())));

        let used_header = GroupHeader::new("In use", "");
        let used_list = boxed_list();
        let used = gtk::Box::new(gtk::Orientation::Vertical, 0);
        used.append(&used_header.root);
        used.append(&used_list);

        let unused_header = GroupHeader::new("Not used", "Removing a volume deletes its data for good.");
        let prune = gtk::Button::builder()
            .label("Remove Unused…")
            .valign(gtk::Align::End)
            .css_classes(["destructive-action"])
            .build();
        unused_header.suffix.append(&prune);
        let input = sender.input_sender().clone();
        prune.connect_clicked(move |_| input.emit(VolumesMsg::Prune));
        let unused_list = boxed_list();
        let unused = gtk::Box::new(gtk::Orientation::Vertical, 0);
        unused.append(&unused_header.root);
        unused.append(&unused_list);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(28)
            .build();
        column.append(&used);
        column.append(&unused);

        let create_first = gtk::Button::builder()
            .label("Create a Volume")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("contr-volumes-symbolic")
            .title("No Volumes")
            .description(
                "Volumes keep data that outlives a container. Compose projects and images create them as needed.",
            )
            .child(&create_first)
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
        for button in [&create, &create_first] {
            let input = sender.input_sender().clone();
            button.connect_clicked(move |_| input.emit(VolumesMsg::Create));
        }

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&search);
        body.append(&content);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(&daemon, "contr-volumes-symbolic", &body)));
        root.set_child(Some(&toolbar));
        follow(&daemon, &sender);

        let model = VolumesPage {
            status: daemon.status(),
            volumes: BTreeMap::new(),
            attachments: Vec::new(),
            loaded: false,
            query: String::new(),
            expanded: HashSet::new(),
        };
        let widgets = VolumesWidgets {
            title,
            search,
            search_button,
            create,
            content,
            used_header,
            used,
            used_list,
            unused,
            unused_list,
        };
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: VolumesMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            VolumesMsg::Search(query) => self.query = query,
            VolumesMsg::Expanded(name, expanded) => {
                if expanded {
                    self.expanded.insert(name);
                } else {
                    self.expanded.remove(&name);
                }
                return;
            }
            VolumesMsg::Open(id) => {
                let _ = sender.output(VolumesOutput::OpenContainer(OpenContainer {
                    id,
                    tab: ContainerTab::Inspect,
                }));
                return;
            }
            VolumesMsg::Copy(name) => {
                root.clipboard().set_text(&name);
                root.add_toast(adw::Toast::new("Copied volume name"));
                return;
            }
            VolumesMsg::Delete(name) => confirm(
                root,
                &format!("Delete {}?", model::display_name(&name)),
                "Its data is deleted for good.",
                "Delete",
                sender.input_sender(),
                VolumesMsg::ConfirmDelete(name),
            ),
            VolumesMsg::ConfirmDelete(name) => self.call(&sender, move |client| async move {
                let request = RemoveVolumeRequest {
                    name: name.clone(),
                    force: false,
                };
                client.volumes().remove_volume(request).await?;
                Ok(format!("Deleted {}", model::display_name(&name)))
            }),
            VolumesMsg::Create => ask_name(root, sender.input_sender()),
            VolumesMsg::ConfirmCreate(name) => self.call(&sender, move |client| async move {
                let request = CreateVolumeRequest {
                    name,
                    ..CreateVolumeRequest::default()
                };
                let volume = client.volumes().create_volume(request).await?.into_inner().volume;
                Ok(format!(
                    "Created {}",
                    volume
                        .map(|volume| model::display_name(&volume.name))
                        .unwrap_or_default()
                ))
            }),
            VolumesMsg::Prune => confirm(
                root,
                "Remove Unused Volumes?",
                "Every volume no container mounts is deleted with its data, named volumes included.",
                "Remove",
                sender.input_sender(),
                VolumesMsg::ConfirmPrune,
            ),
            VolumesMsg::ConfirmPrune => self.call(&sender, |client| async move {
                let response = client
                    .volumes()
                    .prune_volumes(PruneVolumesRequest { all: true })
                    .await?
                    .into_inner();
                Ok(match response.deleted.len() {
                    0 => "No unused volumes to remove".to_owned(),
                    count => format!(
                        "Removed {count} volume{} · freed {}",
                        if count == 1 { "" } else { "s" },
                        format::bytes(response.reclaimed_bytes)
                    ),
                })
            }),
        }
        self.refresh(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: VolumesCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            VolumesCmd::Status(status) => {
                self.status = *status;
                if !self.status.is_running() {
                    self.volumes.clear();
                    self.attachments.clear();
                    self.loaded = false;
                }
            }
            VolumesCmd::Snapshot(volumes) => {
                self.volumes = volumes
                    .into_iter()
                    .map(|volume| (volume.name.clone(), volume))
                    .collect();
                self.loaded = true;
            }
            VolumesCmd::Upserted(volume) => {
                self.volumes.insert(volume.name.clone(), *volume);
            }
            VolumesCmd::Removed(name) => {
                self.volumes.remove(&name);
            }
            VolumesCmd::Attachments(attachments) => self.attachments = attachments,
            VolumesCmd::Done(Ok(message) | Err(message)) => {
                if !message.is_empty() {
                    root.add_toast(adw::Toast::new(&message));
                }
            }
        }
        self.refresh(widgets, &sender);
    }
}

impl VolumesPage {
    fn refresh(&self, widgets: &mut VolumesWidgets, sender: &ComponentSender<Self>) {
        let running = self.status.is_running();
        widgets
            .title
            .set_subtitle(&match (running && self.loaded, self.volumes.len()) {
                (false, _) => String::new(),
                (true, 1) => "1 volume".to_owned(),
                (true, count) => format!("{count} volumes"),
            });
        widgets.search_button.set_sensitive(running);
        widgets.create.set_sensitive(running);
        if !running {
            widgets.search.set_search_mode(false);
        }
        widgets
            .content
            .set_visible_child_name(match (self.loaded, self.volumes.is_empty()) {
                (false, _) => "loading",
                (true, true) => "empty",
                (true, false) => "list",
            });
        let kind = self.status.engine_info.as_ref().map_or("The engine", engine_kind);
        widgets.used_header.set_description(
            &model::storage_dir(&self.volumes).map_or_else(String::new, |dir| format!("{kind} keeps these in {dir}")),
        );

        let (used, unused) = model::sections(
            &self.volumes,
            &self.attachments,
            |volume| system_time(volume.created_at.as_ref()),
            &self.query,
        );
        let now = SystemTime::now();
        for (list, views, section) in [
            (&widgets.used_list, &used, &widgets.used),
            (&widgets.unused_list, &unused, &widgets.unused),
        ] {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            for view in views {
                list.append(&self.row(view, now, sender.input_sender()));
            }
            section.set_visible(!views.is_empty());
        }
    }

    fn row(&self, view: &VolumeView, now: SystemTime, input: &relm4::Sender<VolumesMsg>) -> adw::ExpanderRow {
        let row = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(&view.title))
            .subtitle(glib::markup_escape_text(&view.subtitle))
            .expanded(self.expanded.contains(&view.name))
            .build();
        let name = view.name.clone();
        let expanded = input.clone();
        row.connect_expanded_notify(move |row| expanded.emit(VolumesMsg::Expanded(name.clone(), row.is_expanded())));

        for (index, user) in view.users.iter().enumerate() {
            let link = gtk::Label::builder()
                .label(format!(
                    "<a href=\"{}\">{}</a>",
                    glib::markup_escape_text(&user.id),
                    glib::markup_escape_text(&user.name)
                ))
                .use_markup(true)
                .build();
            let open = input.clone();
            link.connect_activate_link(move |_, id| {
                open.emit(VolumesMsg::Open(id.to_owned()));
                glib::Propagation::Stop
            });
            let path = gtk::Label::builder()
                .label(&user.destination)
                .css_classes(["dim-label", "mono", "group-description"])
                .build();
            let mounted = adw::ActionRow::builder()
                .title(if index == 0 { "Mounted in" } else { "" })
                .build();
            mounted.add_suffix(&link);
            mounted.add_suffix(&path);
            row.add_row(&mounted);
        }

        let created = adw::ActionRow::builder().title("Created").build();
        let when = view
            .created
            .map_or_else(String::new, |created| format::ago(created, now));
        let details = if when.is_empty() {
            format!("{} driver", view.driver)
        } else {
            format!("{when} · {} driver", view.driver)
        };
        created.add_suffix(&gtk::Label::builder().label(details).css_classes(["dim-label"]).build());
        row.add_row(&created);

        let hint = gtk::Label::builder()
            .label(view.delete_hint().unwrap_or_default())
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .css_classes(["dim-label", "group-description"])
            .build();
        let copy = gtk::Button::builder()
            .label("Copy name")
            .valign(gtk::Align::Center)
            .build();
        let delete = gtk::Button::builder()
            .label("Delete")
            .valign(gtk::Align::Center)
            .sensitive(view.users.is_empty())
            .css_classes(["destructive-action"])
            .build();
        let name = view.name.clone();
        let copy_input = input.clone();
        copy.connect_clicked(move |_| copy_input.emit(VolumesMsg::Copy(name.clone())));
        let name = view.name.clone();
        let delete_input = input.clone();
        delete.connect_clicked(move |_| delete_input.emit(VolumesMsg::Delete(name.clone())));
        let footer = gtk::Box::builder().spacing(8).css_classes(["container-row"]).build();
        footer.append(&hint);
        footer.append(&copy);
        footer.append(&delete);
        row.add_row(&gtk::ListBoxRow::builder().child(&footer).activatable(false).build());
        row
    }

    fn call<F, Fut>(&self, sender: &ComponentSender<Self>, call: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, Error>> + Send,
    {
        let Some(client) = self.status.running_client().cloned() else {
            return;
        };
        sender.oneshot_command(async move { VolumesCmd::Done(call(client).await.map_err(|error| describe(&error))) });
    }
}

fn confirm(
    parent: &adw::ToastOverlay,
    heading: &str,
    body: &str,
    action: &str,
    input: &relm4::Sender<VolumesMsg>,
    message: VolumesMsg,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let message = std::cell::Cell::new(Some(message));
    dialog.connect_response(Some("confirm"), move |_, _| {
        if let Some(message) = message.take() {
            input.emit(message);
        }
    });
    dialog.present(Some(parent));
}

fn ask_name(parent: &adw::ToastOverlay, input: &relm4::Sender<VolumesMsg>) {
    let dialog = adw::AlertDialog::new(
        Some("Create a Volume"),
        Some("Leave the name empty to let the engine choose one."),
    );
    let entry = gtk::Entry::builder()
        .placeholder_text("Name")
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let focus = entry.clone();
    dialog.connect_response(Some("create"), move |_, _| {
        input.emit(VolumesMsg::ConfirmCreate(entry.text().trim().to_owned()));
    });
    dialog.present(Some(parent));
    dialog.set_focus(Some(&focus));
}

fn follow(daemon: &Daemon, sender: &ComponentSender<VolumesPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(VolumesCmd::Status(Box::new(status.borrow_and_update().clone())));
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
                    let Ok(stream) = client.volumes().watch_volumes(WatchVolumesRequest {}).await else {
                        return;
                    };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(match response.event {
                            Some(watch_volumes_response::Event::Snapshot(snapshot)) => {
                                VolumesCmd::Snapshot(snapshot.volumes)
                            }
                            Some(watch_volumes_response::Event::Upserted(volume)) => {
                                VolumesCmd::Upserted(Box::new(volume))
                            }
                            Some(watch_volumes_response::Event::RemovedName(name)) => VolumesCmd::Removed(name),
                            None => continue,
                        });
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
                    follow_attachments(client, |attachments| out.emit(VolumesCmd::Attachments(attachments))).await;
                }
            }))
            .drop_on_shutdown()
    });
}
