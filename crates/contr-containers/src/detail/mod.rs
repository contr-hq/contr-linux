//! The page of one container: its state and actions over the Logs, Inspect, Terminal and Stats views.

mod inspect;
mod logs;
mod stats;
mod terminal;

use contr_containers_api::ContainerTab;
use contr_daemon_ui::{Daemon, EngineGate, describe};
use contr_sdk::api::containers::v1::{
    Container, ContainerState, PauseContainerRequest, RemoveContainerRequest, RenameContainerRequest,
    RestartContainerRequest, StartContainerRequest, StopContainerRequest, UnpauseContainerRequest,
};
use contr_sdk::{Client, Error};
use contr_ui_common::format;
use contr_ui_common::widgets::StatusKind;
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::model::{self, labels};

pub(crate) struct ContainerPageInit {
    pub(crate) daemon: Daemon,
    pub(crate) container: Box<Container>,
    pub(crate) tab: ContainerTab,
}

pub(crate) struct ContainerPage {
    daemon: Daemon,
    container: Container,
}

#[derive(Debug)]
pub(crate) enum ContainerPageMsg {
    Updated(Box<Container>),
    Show(ContainerTab),
    Start,
    Stop,
    Restart,
    Pause,
    Resume,
    Delete,
    ConfirmDelete(bool),
    Rename,
    ConfirmRename(String),
    CopyId,
}

#[derive(Debug)]
pub(crate) enum ContainerPageOutput {
    Toast(String),
}

pub(crate) struct ContainerPageWidgets {
    tabs: adw::ToggleGroup,
    title: gtk::Label,
    state: gtk::Label,
    meta: gtk::Label,
    port: gtk::Button,
    start: gtk::Button,
    stop: gtk::Button,
    restart: gtk::Button,
    resume: gtk::Button,
    delete: gtk::Button,
    logs: logs::LogsView,
    inspect: inspect::InspectView,
    terminal: terminal::TerminalView,
    stats: stats::StatsView,
    actions: gio::SimpleActionGroup,
}

const TABS: [(ContainerTab, &str, &str, &str); 4] = [
    (ContainerTab::Logs, "logs", "Logs", "contr-logs-symbolic"),
    (ContainerTab::Inspect, "inspect", "Inspect", "contr-inspect-symbolic"),
    (
        ContainerTab::Terminal,
        "terminal",
        "Terminal",
        "contr-terminal-symbolic",
    ),
    (ContainerTab::Stats, "stats", "Stats", "contr-stats-symbolic"),
];

fn tab_name(tab: ContainerTab) -> &'static str {
    TABS.iter()
        .find(|(t, ..)| *t == tab)
        .map_or("logs", |(_, name, ..)| name)
}

impl Component for ContainerPage {
    type CommandOutput = ();
    type Input = ContainerPageMsg;
    type Output = ContainerPageOutput;
    type Init = ContainerPageInit;
    type Root = adw::NavigationPage;
    type Widgets = ContainerPageWidgets;

    fn init_root() -> Self::Root {
        adw::NavigationPage::builder().title("Container").build()
    }

    fn init(init: ContainerPageInit, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let ContainerPageInit { daemon, container, tab } = init;
        root.set_title(&model::title(&container));

        let tabs = adw::ToggleGroup::builder().css_classes(["flat"]).build();
        for (_, name, label, icon) in TABS {
            let content = adw::ButtonContent::builder().label(label).icon_name(icon).build();
            tabs.add(adw::Toggle::builder().name(name).child(&content).tooltip(label).build());
        }
        let menu = gio::Menu::new();
        let lifecycle = gio::Menu::new();
        lifecycle.append(Some("Restart"), Some("container.restart"));
        lifecycle.append(Some("Pause"), Some("container.pause"));
        lifecycle.append(Some("Resume"), Some("container.resume"));
        lifecycle.append(Some("Rename…"), Some("container.rename"));
        lifecycle.append(Some("Copy ID"), Some("container.copy-id"));
        menu.append_section(None, &lifecycle);
        let danger = gio::Menu::new();
        danger.append(Some("Delete…"), Some("container.delete"));
        menu.append_section(None, &danger);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Container Menu")
            .menu_model(&menu)
            .build();
        let header = adw::HeaderBar::builder().title_widget(&tabs).build();
        header.pack_end(&menu_button);

        let title = gtk::Label::builder().xalign(0.0).css_classes(["detail-title"]).build();
        let state = gtk::Label::builder()
            .css_classes(["detail-state", "detail-meta"])
            .build();
        let meta = gtk::Label::builder()
            .css_classes(["dim-label", "detail-meta"])
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let id = gtk::Label::builder()
            .label(format::short_id(&container.id))
            .css_classes(["dim-label", "mono", "group-description"])
            .build();
        let meta_line = gtk::Box::builder().spacing(12).build();
        meta_line.append(&state);
        meta_line.append(&meta);
        meta_line.append(&id);
        let heading = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        heading.append(&title);
        heading.append(&meta_line);

        let port_content = adw::ButtonContent::builder()
            .icon_name("contr-open-external-symbolic")
            .build();
        let port = gtk::Button::builder()
            .child(&port_content)
            .valign(gtk::Align::Center)
            .css_classes(["raised-button"])
            .build();
        let start = button("Start", &["pill", "suggested-action"]);
        let resume = button("Resume", &["pill", "suggested-action"]);
        let restart = button("Restart", &["raised-button"]);
        let stop = button("Stop", &["raised-button", "destructive-text"]);
        let delete = button("Delete…", &["destructive-action"]);
        let header_row = gtk::Box::builder().spacing(12).build();
        for widget in [
            &heading.clone().upcast::<gtk::Widget>(),
            port.upcast_ref(),
            resume.upcast_ref(),
            start.upcast_ref(),
            restart.upcast_ref(),
            stop.upcast_ref(),
            delete.upcast_ref(),
        ] {
            header_row.append(widget);
        }
        for (widget, message) in [
            (&start, (|| ContainerPageMsg::Start) as fn() -> ContainerPageMsg),
            (&resume, || ContainerPageMsg::Resume),
            (&restart, || ContainerPageMsg::Restart),
            (&stop, || ContainerPageMsg::Stop),
            (&delete, || ContainerPageMsg::Delete),
        ] {
            let input = sender.input_sender().clone();
            widget.connect_clicked(move |_| input.emit(message()));
        }
        port.connect_clicked(|button| {
            let uri = button.widget_name();
            if uri.starts_with("http") {
                gtk::UriLauncher::new(&uri).launch(
                    button.root().and_downcast_ref::<gtk::Window>(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        });

        let logs = logs::LogsView::new(&container);
        let inspect = inspect::InspectView::new();
        let terminal = terminal::TerminalView::new(&container);
        let stats = stats::StatsView::new();
        let views = gtk::Stack::builder().vexpand(true).build();
        views.add_named(logs.widget(), Some("logs"));
        views.add_named(inspect.widget(), Some("inspect"));
        views.add_named(terminal.widget(), Some("terminal"));
        views.add_named(stats.widget(), Some("stats"));
        tabs.connect_active_name_notify({
            let views = views.clone();
            move |tabs| {
                if let Some(name) = tabs.active_name() {
                    views.set_visible_child_name(&name);
                }
            }
        });

        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(16)
            .margin_top(20)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        body.append(&header_row);
        body.append(&views);
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(&daemon, "contr-containers-symbolic", &body)));
        root.set_child(Some(&toolbar));

        let actions = gio::SimpleActionGroup::new();
        for (name, message) in [
            ("restart", (|| ContainerPageMsg::Restart) as fn() -> ContainerPageMsg),
            ("pause", || ContainerPageMsg::Pause),
            ("resume", || ContainerPageMsg::Resume),
            ("rename", || ContainerPageMsg::Rename),
            ("copy-id", || ContainerPageMsg::CopyId),
            ("delete", || ContainerPageMsg::Delete),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let input = sender.input_sender().clone();
            action.connect_activate(move |_, _| input.emit(message()));
            actions.add_action(&action);
        }
        root.insert_action_group("container", Some(&actions));

        let model = ContainerPage {
            daemon,
            container: *container,
        };
        let mut widgets = ContainerPageWidgets {
            tabs,
            title,
            state,
            meta,
            port,
            start,
            stop,
            restart,
            resume,
            delete,
            logs,
            inspect,
            terminal,
            stats,
            actions,
        };
        model.apply(&mut widgets, true);
        widgets.tabs.set_active_name(Some(tab_name(tab)));
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ContainerPageMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ContainerPageMsg::Updated(container) => {
                let changed = container.state != self.container.state;
                self.container = *container;
                self.apply(widgets, changed);
            }
            ContainerPageMsg::Show(tab) => widgets.tabs.set_active_name(Some(tab_name(tab))),
            ContainerPageMsg::Start => self.call(|client, id| async move {
                client
                    .containers()
                    .start_container(StartContainerRequest { id })
                    .await?;
                Ok(String::new())
            }),
            ContainerPageMsg::Stop => self.call(|client, id| async move {
                let request = StopContainerRequest {
                    id,
                    timeout_seconds: None,
                };
                client.containers().stop_container(request).await?;
                Ok(String::new())
            }),
            ContainerPageMsg::Restart => self.call(|client, id| async move {
                let request = RestartContainerRequest {
                    id,
                    timeout_seconds: None,
                };
                client.containers().restart_container(request).await?;
                Ok(String::new())
            }),
            ContainerPageMsg::Pause => self.call(|client, id| async move {
                client
                    .containers()
                    .pause_container(PauseContainerRequest { id })
                    .await?;
                Ok(String::new())
            }),
            ContainerPageMsg::Resume => self.call(|client, id| async move {
                client
                    .containers()
                    .unpause_container(UnpauseContainerRequest { id })
                    .await?;
                Ok(String::new())
            }),
            ContainerPageMsg::Delete => confirm_delete(root, &self.container, sender.input_sender()),
            ContainerPageMsg::ConfirmDelete(remove_volumes) => {
                let name = model::name(&self.container);
                self.call(move |client, id| async move {
                    let request = RemoveContainerRequest {
                        id,
                        force: true,
                        remove_volumes,
                    };
                    client.containers().remove_container(request).await?;
                    Ok(format!("Deleted {name}"))
                });
            }
            ContainerPageMsg::Rename => rename(root, &model::name(&self.container), sender.input_sender()),
            ContainerPageMsg::ConfirmRename(name) => self.call(move |client, id| async move {
                client
                    .containers()
                    .rename_container(RenameContainerRequest { id, name: name.clone() })
                    .await?;
                Ok(format!("Renamed to {name}"))
            }),
            ContainerPageMsg::CopyId => {
                root.clipboard().set_text(&self.container.id);
                let _ = sender.output(ContainerPageOutput::Toast("Copied container ID".to_owned()));
            }
        }
    }

    fn shutdown(&mut self, widgets: &mut Self::Widgets, _output: relm4::Sender<ContainerPageOutput>) {
        widgets.logs.stop();
        widgets.stats.stop();
        widgets.terminal.stop();
    }
}

impl ContainerPage {
    /// Shows the container's state; `state_changed` restarts the streams that depend on it.
    fn apply(&self, widgets: &mut ContainerPageWidgets, state_changed: bool) {
        let container = &self.container;
        let state = container.state();
        let running = state == ContainerState::Running;
        let paused = state == ContainerState::Paused;
        let (kind, word) = model::status(container);

        widgets.title.set_label(&model::title(container));
        let state_text = match state {
            ContainerState::Running => "Running".to_owned(),
            ContainerState::Paused => "Paused".to_owned(),
            _ if !container.status.is_empty() => container.status.clone(),
            _ => word.to_owned(),
        };
        widgets.state.set_label(&state_text);
        for class in ["status-running", "status-failed", "status-paused", "dim-label"] {
            widgets.state.remove_css_class(class);
        }
        widgets.state.add_css_class(match kind {
            StatusKind::Running => "status-running",
            StatusKind::Failed => "status-failed",
            StatusKind::Paused => "status-paused",
            StatusKind::Stopped | StatusKind::Created => "dim-label",
        });
        let mut meta: Vec<&str> = Vec::new();
        if let Some(project) = container.labels.get(labels::PROJECT) {
            meta.push(project);
        }
        meta.push(&container.image);
        widgets.meta.set_label(&meta.join(" · "));

        let published = container
            .ports
            .iter()
            .find(|port| port.host_port != 0)
            .filter(|_| running);
        widgets.port.set_visible(published.is_some());
        if let Some(port) = published {
            if let Some(content) = widgets.port.child().and_downcast::<adw::ButtonContent>() {
                content.set_label(&format!("localhost:{}", port.host_port));
            }
            widgets
                .port
                .set_widget_name(&format!("http://localhost:{}", port.host_port));
        }
        widgets.start.set_visible(!running && !paused);
        widgets.resume.set_visible(paused);
        widgets.restart.set_visible(running);
        widgets.stop.set_visible(running || paused);
        widgets.delete.set_visible(!running && !paused);
        for (name, enabled) in [("restart", running), ("pause", running), ("resume", paused)] {
            if let Some(action) = widgets.actions.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        }

        if let Some(toggle) = widgets.tabs.toggle_by_name("terminal") {
            toggle.set_enabled(running);
            toggle.set_tooltip(if running {
                "Terminal"
            } else {
                "Start the container to open a terminal"
            });
        }
        if !running && widgets.tabs.active_name().as_deref() == Some("terminal") {
            widgets.tabs.set_active_name(Some("logs"));
        }

        if state_changed {
            let client = self.daemon.status().running_client().cloned();
            widgets.logs.follow(client.clone(), container);
            widgets.inspect.load(client.clone(), &container.id);
            widgets.stats.follow(client.clone(), &container.id, running);
            widgets.terminal.set_container(client, container);
        }
    }

    /// Runs `call` on its own task, so it finishes after the page closes; its message goes to the window's toasts.
    fn call<F, Fut>(&self, call: F)
    where
        F: FnOnce(Client, String) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, Error>> + Send,
    {
        let Some(client) = self.daemon.status().running_client().cloned() else {
            return;
        };
        let id = self.container.id.clone();
        let daemon = self.daemon.clone();
        relm4::spawn(async move {
            match call(client, id).await {
                Ok(message) if message.is_empty() => {}
                Ok(message) => daemon.notice(message),
                Err(error) => daemon.notice(describe(&error)),
            }
        });
    }
}

fn button(label: &str, classes: &[&str]) -> gtk::Button {
    gtk::Button::builder()
        .label(label)
        .valign(gtk::Align::Center)
        .css_classes(classes.iter().map(|class| (*class).to_owned()).collect::<Vec<_>>())
        .build()
}

fn confirm_delete(parent: &adw::NavigationPage, container: &Container, input: &relm4::Sender<ContainerPageMsg>) {
    let name = model::title(container);
    let dialog = adw::AlertDialog::new(
        Some(&format!("Delete {name}?")),
        Some(&format!(
            "The container is deleted. Its volumes and the image {} are kept.",
            container.image
        )),
    );
    let volumes = gtk::CheckButton::with_label("Also delete anonymous volumes");
    volumes.set_halign(gtk::Align::Center);
    dialog.set_extra_child(Some(&volumes));
    dialog.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    dialog.connect_response(Some("delete"), move |_, _| {
        input.emit(ContainerPageMsg::ConfirmDelete(volumes.is_active()));
    });
    dialog.present(Some(parent));
}

fn rename(parent: &adw::NavigationPage, current: &str, input: &relm4::Sender<ContainerPageMsg>) {
    let dialog = adw::AlertDialog::new(Some("Rename Container"), None);
    let entry = gtk::Entry::builder().text(current).activates_default(true).build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("rename"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let current = current.to_owned();
    let focus = entry.clone();
    dialog.connect_response(Some("rename"), move |_, _| {
        let name = entry.text().trim().to_owned();
        if !name.is_empty() && name != current {
            input.emit(ContainerPageMsg::ConfirmRename(name));
        }
    });
    dialog.present(Some(parent));
    dialog.set_focus(Some(&focus));
}
