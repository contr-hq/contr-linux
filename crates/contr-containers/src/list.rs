use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use contr_containers_api::{ContainerTab, OpenContainer, RunImage};
use contr_daemon_ui::{Daemon, EngineGate, Status, describe, while_running};
use contr_sdk::api::containers::v1::{
    Container, ContainerState, ContainerStats, PruneContainersRequest, RemoveContainerRequest, RestartContainerRequest,
    StartContainerRequest, StopContainerRequest, WatchContainerStatsRequest, WatchContainersRequest,
    watch_containers_response,
};
use contr_sdk::{Client, Error};
use contr_ui_common::widgets::page_column;
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentController, ComponentParts, ComponentSender, Controller};

use crate::detail::{ContainerPage, ContainerPageInit, ContainerPageMsg, ContainerPageOutput};
use crate::model::{self, GroupView};
use crate::rows::GroupWidget;

/// The Containers section: the list grouped by Compose project, and the container pages it opens.
pub struct ContainersPage {
    daemon: Daemon,
    status: Status,
    containers: BTreeMap<String, Container>,
    stats: HashMap<String, ContainerStats>,
    loaded: bool,
    query: String,
    banner: Option<(String, String)>,
    detail: Option<(String, Controller<ContainerPage>)>,
    /// A container the Run dialog started, opened once the list shows it.
    pending_open: Option<String>,
    home: PathBuf,
}

#[derive(Debug)]
pub enum ContainersMsg {
    Open(OpenContainer),
    Run(RunImage),
    Started(String),
    Activated(String),
    Search(String),
    Start(String),
    Stop(String),
    Terminal(String),
    Delete(String),
    ConfirmDelete { id: String, volumes: bool },
    StartAll(String),
    StopAll(String),
    RestartAll(String),
    Prune,
    ConfirmPrune,
    BannerLogs,
    DetailClosed,
    Toast(String),
}

/// What the section asks the window for.
#[derive(Debug)]
pub enum ContainersOutput {
    /// Show the Containers section.
    Show,
    /// Show the Images section with its pull dialog.
    PullImage,
}

#[doc(hidden)]
#[derive(Debug)]
pub enum ContainersCmd {
    Status(Box<Status>),
    Snapshot(Vec<Container>),
    Upserted(Box<Container>),
    Removed(String),
    Stats(HashMap<String, ContainerStats>),
    Done(Result<String, String>),
}

pub struct ContainersWidgets {
    nav: adw::NavigationView,
    title: adw::WindowTitle,
    search: gtk::SearchBar,
    banner: adw::Banner,
    content: gtk::Stack,
    groups_box: gtk::Box,
    groups: Vec<(Option<String>, GroupWidget)>,
    menu_button: gtk::MenuButton,
    search_button: gtk::ToggleButton,
    run_button: gtk::Button,
}

impl Component for ContainersPage {
    type CommandOutput = ContainersCmd;
    type Input = ContainersMsg;
    type Output = ContainersOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = ContainersWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let title = adw::WindowTitle::new("Containers", "");
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text("Search")
            .build();
        let menu = gio::Menu::new();
        menu.append(Some("Remove Stopped Containers…"), Some("containers.prune"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("View Options")
            .menu_model(&menu)
            .build();
        let run_button = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Run")
                    .build(),
            )
            .tooltip_text("Run a Container")
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&run_button);
        header.pack_end(&menu_button);
        header.pack_end(&search_button);
        let input = sender.input_sender().clone();
        run_button.connect_clicked(move |_| input.emit(ContainersMsg::Run(RunImage { image: String::new() })));

        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search containers")
            .build();
        let search = gtk::SearchBar::builder()
            .child(&adw::Clamp::builder().maximum_size(480).child(&entry).build())
            .build();
        search.connect_entry(&entry);
        search_button
            .bind_property("active", &search, "search-mode-enabled")
            .bidirectional()
            .build();
        let input = sender.input_sender().clone();
        entry.connect_search_changed(move |entry| input.emit(ContainersMsg::Search(entry.text().to_string())));

        let banner = adw::Banner::builder()
            .button_label("View logs")
            .css_classes(["error"])
            .build();
        let input = sender.input_sender().clone();
        banner.connect_button_clicked(move |_| input.emit(ContainersMsg::BannerLogs));

        let groups_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(28)
            .build();
        let run_first = gtk::Button::builder()
            .label("Run a Container")
            .css_classes(["pill", "suggested-action"])
            .build();
        let pull_first = gtk::Button::builder()
            .label("Pull an Image")
            .css_classes(["pill"])
            .build();
        let empty_actions = gtk::Box::builder().spacing(12).halign(gtk::Align::Center).build();
        empty_actions.append(&run_first);
        empty_actions.append(&pull_first);
        let empty = adw::StatusPage::builder()
            .icon_name("contr-containers-symbolic")
            .title("No Containers Yet")
            .description("Run one from an image, or start a project with docker compose up in a terminal. It shows up here right away.")
            .child(&empty_actions)
            .vexpand(true)
            .build();
        let input = sender.input_sender().clone();
        run_first.connect_clicked(move |_| input.emit(ContainersMsg::Run(RunImage { image: String::new() })));
        let output = sender.output_sender().clone();
        pull_first.connect_clicked(move |_| output.emit(ContainersOutput::PullImage));
        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let content = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        content.add_named(&page_column(&groups_box), Some("list"));
        content.add_named(&empty, Some("empty"));
        content.add_named(&loading, Some("loading"));

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&search);
        body.append(&banner);
        body.append(&content);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(&daemon, "contr-containers-symbolic", &body)));
        let nav = adw::NavigationView::new();
        nav.add(&adw::NavigationPage::with_tag(&toolbar, "Containers", "containers"));
        let input = sender.input_sender().clone();
        nav.connect_popped(move |_, _| input.emit(ContainersMsg::DetailClosed));
        root.set_child(Some(&nav));

        let actions = gio::SimpleActionGroup::new();
        let prune = gio::SimpleAction::new("prune", None);
        let input = sender.input_sender().clone();
        prune.connect_activate(move |_, _| input.emit(ContainersMsg::Prune));
        actions.add_action(&prune);
        let restart_all = gio::SimpleAction::new("restart-all", Some(glib::VariantTy::STRING));
        let input = sender.input_sender().clone();
        restart_all.connect_activate(move |_, project| {
            if let Some(project) = project.and_then(glib::Variant::get::<String>) {
                input.emit(ContainersMsg::RestartAll(project));
            }
        });
        actions.add_action(&restart_all);
        root.insert_action_group("containers", Some(&actions));

        follow(&daemon, &sender);

        let model = ContainersPage {
            status: daemon.status(),
            daemon,
            containers: BTreeMap::new(),
            stats: HashMap::new(),
            loaded: false,
            query: String::new(),
            banner: None,
            detail: None,
            pending_open: None,
            home: glib::home_dir(),
        };
        let widgets = ContainersWidgets {
            nav,
            title,
            search,
            banner,
            content,
            groups_box,
            groups: Vec::new(),
            menu_button,
            search_button,
            run_button,
        };
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ContainersMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ContainersMsg::Open(open) => self.open(widgets, &sender, &open.id, open.tab),
            ContainersMsg::Run(run) => {
                let taken = self.published_ports();
                let input = sender.input_sender().clone();
                crate::run::present(root, &self.daemon, &run.image, taken, move |id| {
                    input.emit(ContainersMsg::Started(id));
                });
            }
            ContainersMsg::Started(id) => {
                let _ = sender.output(ContainersOutput::Show);
                if self.containers.contains_key(&id) {
                    self.open(widgets, &sender, &id, ContainerTab::Logs);
                } else {
                    self.pending_open = Some(id);
                }
            }
            ContainersMsg::Activated(id) => self.open(widgets, &sender, &id, ContainerTab::Logs),
            ContainersMsg::Terminal(id) => self.open(widgets, &sender, &id, ContainerTab::Terminal),
            ContainersMsg::Search(query) => self.query = query,
            ContainersMsg::Start(id) => self.call(&sender, move |client| start(client, vec![id])),
            ContainersMsg::Stop(id) => self.call(&sender, move |client| stop(client, vec![id])),
            ContainersMsg::StartAll(project) => {
                let ids = self.project_ids(&project, false);
                self.call(&sender, move |client| start(client, ids));
            }
            ContainersMsg::StopAll(project) => {
                let ids = self.project_ids(&project, true);
                self.call(&sender, move |client| stop(client, ids));
            }
            ContainersMsg::RestartAll(project) => {
                let ids = self.project_ids(&project, true);
                self.call(&sender, move |client| restart(client, ids));
            }
            ContainersMsg::Delete(id) => {
                if let Some(container) = self.containers.get(&id) {
                    confirm_delete(root, container, sender.input_sender());
                }
            }
            ContainersMsg::ConfirmDelete { id, volumes } => {
                let name = self.containers.get(&id).map(model::name).unwrap_or_default();
                self.call(&sender, move |client| remove(client, id, name, volumes));
            }
            ContainersMsg::Prune => confirm_prune(root, sender.input_sender()),
            ContainersMsg::ConfirmPrune => self.call(&sender, prune),
            ContainersMsg::BannerLogs => {
                if let Some((id, _)) = self.banner.take() {
                    self.open(widgets, &sender, &id, ContainerTab::Logs);
                }
            }
            ContainersMsg::DetailClosed => {
                if widgets.nav.visible_page().and_then(|page| page.tag()).as_deref() == Some("containers") {
                    self.detail = None;
                }
            }
            ContainersMsg::Toast(message) => root.add_toast(adw::Toast::new(&message)),
        }
        self.refresh(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ContainersCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ContainersCmd::Status(status) => {
                self.status = *status;
                if !self.status.is_running() {
                    self.containers.clear();
                    self.stats.clear();
                    self.loaded = false;
                    self.banner = None;
                }
            }
            ContainersCmd::Snapshot(containers) => {
                self.containers = containers.into_iter().map(|c| (c.id.clone(), c)).collect();
                self.loaded = true;
                if let Some((id, detail)) = &self.detail
                    && let Some(container) = self.containers.get(id)
                {
                    detail.emit(ContainerPageMsg::Updated(Box::new(container.clone())));
                }
            }
            ContainersCmd::Upserted(container) => {
                if let Some(text) = model::crash(self.containers.get(&container.id), &container) {
                    self.banner = Some((container.id.clone(), text));
                } else if self.banner.as_ref().is_some_and(|(id, _)| id == &container.id)
                    && container.state() == ContainerState::Running
                {
                    self.banner = None;
                }
                if let Some((id, detail)) = &self.detail
                    && id == &container.id
                {
                    detail.emit(ContainerPageMsg::Updated(container.clone()));
                }
                let id = container.id.clone();
                self.containers.insert(id.clone(), *container);
                if self.pending_open.as_ref() == Some(&id) {
                    self.pending_open = None;
                    self.open(widgets, &sender, &id, ContainerTab::Logs);
                }
            }
            ContainersCmd::Removed(id) => {
                self.containers.remove(&id);
                if self.banner.as_ref().is_some_and(|(banner, _)| banner == &id) {
                    self.banner = None;
                }
                if self.detail.as_ref().is_some_and(|(detail, _)| detail == &id) {
                    widgets.nav.pop_to_tag("containers");
                    self.detail = None;
                }
            }
            ContainersCmd::Stats(stats) => self.stats = stats,
            ContainersCmd::Done(Ok(message)) if !message.is_empty() => root.add_toast(adw::Toast::new(&message)),
            ContainersCmd::Done(Ok(_)) => {}
            ContainersCmd::Done(Err(message)) => root.add_toast(adw::Toast::new(&message)),
        }
        self.refresh(widgets, &sender);
    }
}

impl ContainersPage {
    fn refresh(&self, widgets: &mut ContainersWidgets, sender: &ComponentSender<Self>) {
        let running = self.status.is_running();
        widgets.title.set_subtitle(&if running {
            model::header_summary(&self.containers)
        } else {
            String::new()
        });
        widgets.search_button.set_sensitive(running);
        widgets.menu_button.set_sensitive(running);
        widgets.run_button.set_sensitive(running);
        if !running {
            widgets.search.set_search_mode(false);
        }
        match &self.banner {
            Some((_, text)) => {
                widgets.banner.set_title(text);
                widgets.banner.set_revealed(true);
            }
            None => widgets.banner.set_revealed(false),
        }

        let groups = model::groups(&self.containers, &self.stats, &self.home, &self.query);
        widgets
            .content
            .set_visible_child_name(match (self.loaded, self.containers.is_empty()) {
                (false, _) => "loading",
                (true, true) => "empty",
                (true, false) => "list",
            });
        reconcile(widgets, &groups, sender.input_sender());
    }

    fn open(&mut self, widgets: &ContainersWidgets, sender: &ComponentSender<Self>, id: &str, tab: ContainerTab) {
        let Some(container) = self.find(id).cloned() else {
            return;
        };
        if let Some((open, detail)) = &self.detail
            && open == &container.id
        {
            detail.emit(ContainerPageMsg::Show(tab));
            return;
        }
        widgets.nav.pop_to_tag("containers");
        let detail = ContainerPage::builder()
            .launch(ContainerPageInit {
                daemon: self.daemon.clone(),
                container: Box::new(container.clone()),
                tab,
            })
            .forward(sender.input_sender(), |output| match output {
                ContainerPageOutput::Toast(message) => ContainersMsg::Toast(message),
            });
        widgets.nav.push(detail.widget());
        self.detail = Some((container.id, detail));
    }

    /// Host ports the containers publish, with the name of the container that holds each.
    fn published_ports(&self) -> HashMap<u32, String> {
        self.containers
            .values()
            .flat_map(|container| {
                let name = model::title(container);
                container
                    .ports
                    .iter()
                    .filter(|port| port.host_port != 0)
                    .map(move |port| (port.host_port, name.clone()))
            })
            .collect()
    }

    fn find(&self, id: &str) -> Option<&Container> {
        self.containers.get(id).or_else(|| {
            self.containers
                .values()
                .find(|container| container.id.starts_with(id) || container.names.iter().any(|name| name == id))
        })
    }

    fn project_ids(&self, project: &str, running: bool) -> Vec<String> {
        self.containers
            .values()
            .filter(|container| container.labels.get(model::labels::PROJECT).map(String::as_str) == Some(project))
            .filter(|container| (container.state() == ContainerState::Running) == running)
            .map(|container| container.id.clone())
            .collect()
    }

    fn call<F, Fut>(&self, sender: &ComponentSender<Self>, call: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, Error>> + Send,
    {
        let Some(client) = self.status.running_client().cloned() else {
            return;
        };
        sender
            .oneshot_command(async move { ContainersCmd::Done(call(client).await.map_err(|error| describe(&error))) });
    }
}

fn reconcile(widgets: &mut ContainersWidgets, groups: &[GroupView], input: &relm4::Sender<ContainersMsg>) {
    let keys: Vec<Option<String>> = groups.iter().map(|group| group.project.clone()).collect();
    let current: Vec<Option<String>> = widgets.groups.iter().map(|(key, _)| key.clone()).collect();
    if keys != current {
        while let Some(child) = widgets.groups_box.first_child() {
            widgets.groups_box.remove(&child);
        }
        let mut old: HashMap<Option<String>, GroupWidget> = widgets.groups.drain(..).collect();
        for group in groups {
            let widget = old
                .remove(&group.project)
                .unwrap_or_else(|| GroupWidget::new(group, input));
            widgets.groups_box.append(&widget.root);
            widgets.groups.push((group.project.clone(), widget));
        }
    }
    for ((_, widget), group) in widgets.groups.iter_mut().zip(groups) {
        widget.update(group, input);
    }
}

fn follow(daemon: &Daemon, sender: &ComponentSender<ContainersPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(ContainersCmd::Status(Box::new(status.borrow_and_update().clone())));
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
                    let Ok(stream) = client.containers().watch_containers(WatchContainersRequest {}).await else {
                        return;
                    };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(match response.event {
                            Some(watch_containers_response::Event::Snapshot(snapshot)) => {
                                ContainersCmd::Snapshot(snapshot.containers)
                            }
                            Some(watch_containers_response::Event::Upserted(container)) => {
                                ContainersCmd::Upserted(Box::new(container))
                            }
                            Some(watch_containers_response::Event::RemovedId(id)) => ContainersCmd::Removed(id),
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
                    let request = client
                        .containers()
                        .watch_container_stats(WatchContainerStatsRequest {})
                        .await;
                    let Ok(stream) = request else { return };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(ContainersCmd::Stats(response.stats));
                    }
                }
            }))
            .drop_on_shutdown()
    });
}

async fn start(client: Client, ids: Vec<String>) -> Result<String, Error> {
    for id in ids {
        client
            .containers()
            .start_container(StartContainerRequest { id })
            .await?;
    }
    Ok(String::new())
}

async fn stop(client: Client, ids: Vec<String>) -> Result<String, Error> {
    for id in ids {
        client
            .containers()
            .stop_container(StopContainerRequest {
                id,
                timeout_seconds: None,
            })
            .await?;
    }
    Ok(String::new())
}

async fn restart(client: Client, ids: Vec<String>) -> Result<String, Error> {
    for id in ids {
        client
            .containers()
            .restart_container(RestartContainerRequest {
                id,
                timeout_seconds: None,
            })
            .await?;
    }
    Ok(String::new())
}

async fn remove(client: Client, id: String, name: String, remove_volumes: bool) -> Result<String, Error> {
    client
        .containers()
        .remove_container(RemoveContainerRequest {
            id,
            force: true,
            remove_volumes,
        })
        .await?;
    Ok(format!("Deleted {name}"))
}

async fn prune(client: Client) -> Result<String, Error> {
    let response = client
        .containers()
        .prune_containers(PruneContainersRequest {})
        .await?
        .into_inner();
    Ok(match response.deleted_ids.len() {
        0 => "No stopped containers to remove".to_owned(),
        1 => "Removed 1 container".to_owned(),
        count => format!("Removed {count} containers"),
    })
}

fn confirm_delete(parent: &adw::ToastOverlay, container: &Container, input: &relm4::Sender<ContainersMsg>) {
    let name = model::title(container);
    let running = container.state() == ContainerState::Running;
    let body = if running {
        format!(
            "{name} is stopped and deleted. Its volumes and the image {} are kept.",
            container.image
        )
    } else {
        format!(
            "The container is deleted. Its volumes and the image {} are kept.",
            container.image
        )
    };
    let dialog = adw::AlertDialog::new(Some(&format!("Delete {name}?")), Some(&body));
    let volumes = gtk::CheckButton::with_label("Also delete anonymous volumes");
    volumes.set_halign(gtk::Align::Center);
    dialog.set_extra_child(Some(&volumes));
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let id = container.id.clone();
    dialog.connect_response(Some("delete"), move |_, _| {
        input.emit(ContainersMsg::ConfirmDelete {
            id: id.clone(),
            volumes: volumes.is_active(),
        });
    });
    dialog.present(Some(parent));
}

fn confirm_prune(parent: &adw::ToastOverlay, input: &relm4::Sender<ContainersMsg>) {
    let dialog = adw::AlertDialog::new(
        Some("Remove Stopped Containers?"),
        Some("Every stopped container is deleted. Their volumes and images are kept."),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("remove", "Remove")]);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    dialog.connect_response(Some("remove"), move |_, _| input.emit(ContainersMsg::ConfirmPrune));
    dialog.present(Some(parent));
}
