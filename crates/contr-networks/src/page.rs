use std::collections::{BTreeMap, HashSet};

use contr_containers_api::{ContainerTab, OpenContainer};
use contr_daemon_ui::{
    Attachment, Daemon, EngineGate, Status, container_networks, describe, follow_attachments, while_running,
};
use contr_sdk::api::containers::v1::NetworkAttachment;
use contr_sdk::api::networks::v1::{
    ConnectNetworkRequest, CreateNetworkRequest, DisconnectNetworkRequest, Network, RemoveNetworkRequest, Subnet,
    WatchNetworksRequest, watch_networks_response,
};
use contr_sdk::{Client, Error};
use contr_ui_common::widgets::{GroupHeader, boxed_list, icon_button, page_column};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::model::{self, NetworkView};

const HOST_GATEWAY_SNIPPET: &str = "extra_hosts:\n  - \"host.docker.internal:host-gateway\"";

/// The Networks section: user networks with their containers, the built-in ones, and how a container reaches this
/// computer.
pub struct NetworksPage {
    status: Status,
    networks: BTreeMap<String, Network>,
    attachments: Vec<Attachment>,
    loaded: bool,
    expanded: HashSet<String>,
}

#[derive(Debug)]
pub enum NetworksMsg {
    Expanded(String, bool),
    Open(String),
    Delete(String),
    ConfirmDelete(String),
    Create,
    ConfirmCreate {
        name: String,
        subnet: String,
        internal: bool,
    },
    CopySnippet,
    /// Asks which container joins the network with this ID.
    Connect(String),
    ConfirmConnect {
        network: String,
        container: String,
        aliases: Vec<String>,
    },
    Disconnect {
        network: String,
        container: String,
        name: String,
    },
}

#[derive(Debug)]
pub enum NetworksOutput {
    OpenContainer(OpenContainer),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum NetworksCmd {
    Status(Box<Status>),
    Snapshot(Vec<Network>),
    Upserted(Box<Network>),
    Removed(String),
    Attachments(Vec<Attachment>),
    /// A container joined or left a network; carries its networks now.
    Rejoined {
        message: String,
        container: String,
        networks: Vec<NetworkAttachment>,
    },
    Done(Result<String, String>),
}

pub struct NetworksWidgets {
    title: adw::WindowTitle,
    create: gtk::Button,
    content: gtk::Stack,
    yours: gtk::Box,
    yours_list: gtk::ListBox,
    built_in_list: gtk::ListBox,
}

impl Component for NetworksPage {
    type CommandOutput = NetworksCmd;
    type Input = NetworksMsg;
    type Output = NetworksOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = NetworksWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let title = adw::WindowTitle::new("Networks", "");
        let create = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Create")
                    .build(),
            )
            .tooltip_text("Create a Network")
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&create);
        let input = sender.input_sender().clone();
        create.connect_clicked(move |_| input.emit(NetworksMsg::Create));

        let yours_header = GroupHeader::new("Your networks", "");
        let yours_list = boxed_list();
        let yours = gtk::Box::new(gtk::Orientation::Vertical, 0);
        yours.append(&yours_header.root);
        yours.append(&yours_list);

        let built_in_header = GroupHeader::new("Built in", "Created by the engine. They can’t be removed.");
        let built_in_list = boxed_list();
        let built_in = gtk::Box::new(gtk::Orientation::Vertical, 0);
        built_in.append(&built_in_header.root);
        built_in.append(&built_in_list);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(28)
            .build();
        column.append(&yours);
        column.append(&built_in);
        column.append(&host_access(sender.input_sender()));

        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&page_column(&column), Some("list"));
        content.add_named(&loading, Some("loading"));

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(&daemon, "contr-networks-symbolic", &content)));
        root.set_child(Some(&toolbar));
        follow(&daemon, &sender);

        let model = NetworksPage {
            status: daemon.status(),
            networks: BTreeMap::new(),
            attachments: Vec::new(),
            loaded: false,
            expanded: HashSet::new(),
        };
        let widgets = NetworksWidgets {
            title,
            create,
            content,
            yours,
            yours_list,
            built_in_list,
        };
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: NetworksMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            NetworksMsg::Expanded(id, expanded) => {
                if expanded {
                    self.expanded.insert(id);
                } else {
                    self.expanded.remove(&id);
                }
                return;
            }
            NetworksMsg::Open(id) => {
                let _ = sender.output(NetworksOutput::OpenContainer(OpenContainer {
                    id,
                    tab: ContainerTab::Inspect,
                }));
                return;
            }
            NetworksMsg::Delete(id) => {
                let name = self
                    .networks
                    .get(&id)
                    .map(|network| network.name.clone())
                    .unwrap_or_default();
                confirm_delete(root, &name, id, sender.input_sender());
            }
            NetworksMsg::ConfirmDelete(id) => {
                let name = self
                    .networks
                    .get(&id)
                    .map(|network| network.name.clone())
                    .unwrap_or_default();
                self.call(&sender, move |client| async move {
                    client.networks().remove_network(RemoveNetworkRequest { id }).await?;
                    Ok(format!("Deleted {name}"))
                });
            }
            NetworksMsg::Create => ask_network(root, sender.input_sender()),
            NetworksMsg::Connect(id) => {
                let (yours, _) = model::sections(&self.networks, &self.attachments);
                if let Some(view) = yours.iter().find(|view| view.id == id) {
                    let choices = model::connectable(view, &self.attachments);
                    ask_connect(root, view, &choices, sender.input_sender());
                }
            }
            NetworksMsg::ConfirmConnect {
                network,
                container,
                aliases,
            } => {
                let message = format!("Connected to {network}");
                let request = ConnectNetworkRequest {
                    network,
                    container: container.clone(),
                    aliases,
                };
                self.rejoin(&sender, container, message, |client| async move {
                    client
                        .networks()
                        .connect_network(request)
                        .await
                        .map(|_| ())
                        .map_err(Error::from)
                });
            }
            NetworksMsg::Disconnect {
                network,
                container,
                name,
            } => {
                let message = format!("Disconnected {name} from {network}");
                let request = DisconnectNetworkRequest {
                    network,
                    container: container.clone(),
                    force: false,
                };
                self.rejoin(&sender, container, message, |client| async move {
                    client
                        .networks()
                        .disconnect_network(request)
                        .await
                        .map(|_| ())
                        .map_err(Error::from)
                });
            }
            NetworksMsg::ConfirmCreate { name, subnet, internal } => {
                self.call(&sender, move |client| async move {
                    let request = CreateNetworkRequest {
                        name: name.clone(),
                        internal,
                        subnets: if subnet.is_empty() {
                            Vec::new()
                        } else {
                            vec![Subnet {
                                cidr: subnet,
                                gateway: String::new(),
                            }]
                        },
                        ..CreateNetworkRequest::default()
                    };
                    client.networks().create_network(request).await?;
                    Ok(format!("Created {name}"))
                });
            }
            NetworksMsg::CopySnippet => {
                root.clipboard().set_text(HOST_GATEWAY_SNIPPET);
                root.add_toast(adw::Toast::new("Copied snippet"));
                return;
            }
        }
        self.refresh(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: NetworksCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            NetworksCmd::Status(status) => {
                self.status = *status;
                if !self.status.is_running() {
                    self.networks.clear();
                    self.attachments.clear();
                    self.loaded = false;
                }
            }
            NetworksCmd::Snapshot(networks) => {
                self.networks = networks
                    .into_iter()
                    .map(|network| (network.id.clone(), network))
                    .collect();
                self.loaded = true;
            }
            NetworksCmd::Upserted(network) => {
                self.networks.insert(network.id.clone(), *network);
            }
            NetworksCmd::Removed(id) => {
                self.networks.remove(&id);
            }
            NetworksCmd::Attachments(attachments) => self.attachments = attachments,
            NetworksCmd::Rejoined {
                message,
                container,
                networks,
            } => {
                if let Some(attachment) = self
                    .attachments
                    .iter_mut()
                    .find(|attachment| attachment.id == container)
                {
                    attachment.networks = networks;
                }
                root.add_toast(adw::Toast::new(&message));
            }
            NetworksCmd::Done(Ok(message) | Err(message)) => {
                if !message.is_empty() {
                    root.add_toast(adw::Toast::new(&message));
                }
            }
        }
        self.refresh(widgets, &sender);
    }
}

impl NetworksPage {
    fn refresh(&self, widgets: &mut NetworksWidgets, sender: &ComponentSender<Self>) {
        let running = self.status.is_running();
        widgets
            .title
            .set_subtitle(&match (running && self.loaded, self.networks.len()) {
                (false, _) => String::new(),
                (true, 1) => "1 network".to_owned(),
                (true, count) => format!("{count} networks"),
            });
        widgets.create.set_sensitive(running);
        widgets
            .content
            .set_visible_child_name(if self.loaded { "list" } else { "loading" });

        let (yours, built_in) = model::sections(&self.networks, &self.attachments);
        let input = sender.input_sender();
        for list in [&widgets.yours_list, &widgets.built_in_list] {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
        }
        for view in &yours {
            widgets.yours_list.append(&self.expander(view, input));
        }
        widgets.yours.set_visible(!yours.is_empty());
        for view in &built_in {
            let row = adw::ActionRow::builder().title(&view.name).build();
            row.add_suffix(
                &gtk::Label::builder()
                    .label(&view.subtitle)
                    .css_classes(["dim-label", "group-description"])
                    .build(),
            );
            widgets.built_in_list.append(&row);
        }
    }

    fn expander(&self, view: &NetworkView, input: &relm4::Sender<NetworksMsg>) -> adw::ExpanderRow {
        let row = adw::ExpanderRow::builder()
            .title(glib::markup_escape_text(&view.name))
            .subtitle(glib::markup_escape_text(&view.subtitle))
            .expanded(self.expanded.contains(&view.id))
            .build();
        let id = view.id.clone();
        let expanded = input.clone();
        row.connect_expanded_notify(move |row| expanded.emit(NetworksMsg::Expanded(id.clone(), row.is_expanded())));

        for member in &view.members {
            let dot = gtk::Box::builder()
                .css_classes([if member.running {
                    "sidebar-dot"
                } else {
                    "sidebar-dot-off"
                }])
                .valign(gtk::Align::Center)
                .build();
            let link = gtk::Label::builder()
                .label(format!(
                    "<a href=\"{}\">{}</a>",
                    glib::markup_escape_text(&member.id),
                    glib::markup_escape_text(&member.name)
                ))
                .use_markup(true)
                .hexpand(true)
                .xalign(0.0)
                .build();
            let open = input.clone();
            link.connect_activate_link(move |_, id| {
                open.emit(NetworksMsg::Open(id.to_owned()));
                glib::Propagation::Stop
            });
            let address = gtk::Label::builder()
                .label(&member.address)
                .css_classes(["dim-label", "mono", "group-description"])
                .build();
            let disconnect = icon_button("list-remove-symbolic", &format!("Disconnect {}", member.name));
            let message = (view.name.clone(), member.id.clone(), member.name.clone());
            let input = input.clone();
            disconnect.connect_clicked(move |_| {
                let (network, container, name) = message.clone();
                input.emit(NetworksMsg::Disconnect {
                    network,
                    container,
                    name,
                });
            });
            let content = gtk::Box::builder().spacing(12).css_classes(["container-row"]).build();
            content.append(&dot);
            content.append(&link);
            content.append(&address);
            content.append(&disconnect);
            row.add_row(&gtk::ListBoxRow::builder().child(&content).activatable(false).build());
        }

        let footer = gtk::Box::builder().spacing(8).css_classes(["container-row"]).build();
        let connect = gtk::Button::builder()
            .label("Connect a Container…")
            .valign(gtk::Align::Center)
            .sensitive(!model::connectable(view, &self.attachments).is_empty())
            .build();
        let id = view.id.clone();
        let connect_input = input.clone();
        connect.connect_clicked(move |_| connect_input.emit(NetworksMsg::Connect(id.clone())));
        footer.append(&connect);
        if let Some(project) = &view.compose_project {
            footer.append(
                &gtk::Label::builder()
                    .label(format!("Managed by Compose. Run Down on {project} to remove it."))
                    .xalign(0.0)
                    .hexpand(true)
                    .wrap(true)
                    .css_classes(["dim-label", "group-description"])
                    .build(),
            );
        } else {
            let hint = gtk::Label::builder()
                .label(if view.members.is_empty() {
                    ""
                } else {
                    "Disconnect its containers before deleting it."
                })
                .xalign(0.0)
                .hexpand(true)
                .css_classes(["dim-label", "group-description"])
                .build();
            let delete = gtk::Button::builder()
                .label("Delete")
                .valign(gtk::Align::Center)
                .sensitive(view.members.is_empty())
                .css_classes(["destructive-action"])
                .build();
            let id = view.id.clone();
            let input = input.clone();
            delete.connect_clicked(move |_| input.emit(NetworksMsg::Delete(id.clone())));
            footer.append(&hint);
            footer.append(&delete);
        }
        row.add_row(&gtk::ListBoxRow::builder().child(&footer).activatable(false).build());
        row
    }

    /// Runs `change` on `container`'s networks, then reads them again for the page.
    fn rejoin<F, Fut>(&self, sender: &ComponentSender<Self>, container: String, message: String, change: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), Error>> + Send,
    {
        let Some(client) = self.status.running_client().cloned() else {
            return;
        };
        sender.oneshot_command(async move {
            if let Err(error) = change(client.clone()).await {
                return NetworksCmd::Done(Err(describe(&error)));
            }
            let networks = container_networks(&client, &container).await.unwrap_or_default();
            NetworksCmd::Rejoined {
                message,
                container,
                networks,
            }
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
        sender.oneshot_command(async move { NetworksCmd::Done(call(client).await.map_err(|error| describe(&error))) });
    }
}

/// How a container reaches services on this computer; Linux engines add no `host.docker.internal` themselves.
fn host_access(input: &relm4::Sender<NetworksMsg>) -> gtk::Box {
    let header = GroupHeader::new("Reaching this computer from a container", "");
    let intro = gtk::Label::builder()
        .label("On Linux, <span font_family=\"monospace\">host.docker.internal</span> isn’t added for you. Add it to the services that need it:")
        .use_markup(true)
        .wrap(true)
        .xalign(0.0)
        .build();
    let snippet = gtk::Label::builder()
        .label(HOST_GATEWAY_SNIPPET)
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .css_classes(["mono", "group-description"])
        .build();
    let copy = icon_button("edit-copy-symbolic", "Copy Snippet");
    let input = input.clone();
    copy.connect_clicked(move |_| input.emit(NetworksMsg::CopySnippet));
    let code = gtk::Box::builder().spacing(8).css_classes(["code-block"]).build();
    code.append(&snippet);
    code.append(&copy);
    let note = gtk::Label::builder()
        .label("Published ports answer at localhost on this computer. The Run dialog publishes on 127.0.0.1 only, so other computers on your network can’t reach them.")
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label", "group-description"])
        .build();
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .css_classes(["card", "pull-head"])
        .build();
    card.append(&intro);
    card.append(&code);
    card.append(&note);
    let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
    section.append(&header.root);
    section.append(&card);
    section
}

fn confirm_delete(parent: &adw::ToastOverlay, name: &str, id: String, input: &relm4::Sender<NetworksMsg>) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Delete {name}?")),
        Some("Containers can no longer join it."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    dialog.connect_response(Some("delete"), move |_, _| {
        input.emit(NetworksMsg::ConfirmDelete(id.clone()));
    });
    dialog.present(Some(parent));
}

fn ask_connect(
    parent: &adw::ToastOverlay,
    view: &NetworkView,
    choices: &[(String, String)],
    input: &relm4::Sender<NetworksMsg>,
) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Connect a Container to {}", view.name)),
        Some("Containers on the network reach it by its name and by any aliases you add."),
    );
    let names: Vec<&str> = choices.iter().map(|(_, name)| name.as_str()).collect();
    let container = gtk::DropDown::from_strings(&names);
    let aliases = gtk::Entry::builder()
        .placeholder_text("Aliases, separated by commas (optional)")
        .activates_default(true)
        .build();
    let fields = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    fields.append(&container);
    fields.append(&aliases);
    dialog.set_extra_child(Some(&fields));
    dialog.add_responses(&[("cancel", "Cancel"), ("connect", "Connect")]);
    dialog.set_response_appearance("connect", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("connect"));
    dialog.set_close_response("cancel");
    let ids: Vec<String> = choices.iter().map(|(id, _)| id.clone()).collect();
    let network = view.name.clone();
    let input = input.clone();
    dialog.connect_response(Some("connect"), move |_, _| {
        let chosen = usize::try_from(container.selected())
            .ok()
            .and_then(|index| ids.get(index));
        if let Some(id) = chosen {
            input.emit(NetworksMsg::ConfirmConnect {
                network: network.clone(),
                container: id.clone(),
                aliases: model::aliases(&aliases.text()),
            });
        }
    });
    dialog.present(Some(parent));
}

fn ask_network(parent: &adw::ToastOverlay, input: &relm4::Sender<NetworksMsg>) {
    let dialog = adw::AlertDialog::new(Some("Create a Network"), None);
    let name = gtk::Entry::builder()
        .placeholder_text("Name")
        .activates_default(true)
        .build();
    let subnet = gtk::Entry::builder()
        .placeholder_text("Subnet, such as 172.30.0.0/16 (optional)")
        .activates_default(true)
        .build();
    let internal = gtk::CheckButton::with_label("Internal: no access outside the network");
    let fields = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    fields.append(&name);
    fields.append(&subnet);
    fields.append(&internal);
    dialog.set_extra_child(Some(&fields));
    dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
    dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("create"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let focus = name.clone();
    dialog.connect_response(Some("create"), move |_, _| {
        let network = name.text().trim().to_owned();
        if !network.is_empty() {
            input.emit(NetworksMsg::ConfirmCreate {
                name: network,
                subnet: subnet.text().trim().to_owned(),
                internal: internal.is_active(),
            });
        }
    });
    dialog.present(Some(parent));
    dialog.set_focus(Some(&focus));
}

fn follow(daemon: &Daemon, sender: &ComponentSender<NetworksPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(NetworksCmd::Status(Box::new(status.borrow_and_update().clone())));
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
                    let Ok(stream) = client.networks().watch_networks(WatchNetworksRequest {}).await else {
                        return;
                    };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(match response.event {
                            Some(watch_networks_response::Event::Snapshot(snapshot)) => {
                                NetworksCmd::Snapshot(snapshot.networks)
                            }
                            Some(watch_networks_response::Event::Upserted(network)) => {
                                NetworksCmd::Upserted(Box::new(network))
                            }
                            Some(watch_networks_response::Event::RemovedId(id)) => NetworksCmd::Removed(id),
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
                    follow_attachments(client, |attachments| out.emit(NetworksCmd::Attachments(attachments))).await;
                }
            }))
            .drop_on_shutdown()
    });
}
