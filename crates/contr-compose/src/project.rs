//! A Compose project's page: its actions, services and labelled logs.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use contr_daemon_ui::operations::{self, Activity, FollowEvent, Kind, Operation};
use contr_daemon_ui::{Daemon, EngineGate, describe};
use contr_sdk::Client;
use contr_sdk::api::compose::v1::{
    Action, Project, RemoveProjectRequest, RunActionRequest, StreamLogsRequest, run_action_response,
};
use contr_sdk::api::containers::v1::ContainerState;
use contr_sdk::api::operations::v1::OperationState;
use contr_ui_common::palette::SERVICE_COLORS;
use contr_ui_common::widgets::{GroupHeader, boxed_list, set_status};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentParts, ComponentSender};
use tokio::sync::mpsc;

use crate::model::{self, ServiceView};

const TAIL_LINES: u32 = 200;
/// Lines the failure dialog shows from the end of Compose's output.
const FAILURE_LINES: usize = 40;

pub(crate) struct ProjectInit {
    pub(crate) daemon: Daemon,
    pub(crate) project: Box<Project>,
}

pub(crate) struct ProjectPage {
    daemon: Daemon,
    project: Project,
    home: PathBuf,
    running_ids: Vec<String>,
    filter: Option<String>,
    /// The project's running Compose action, whichever client started it.
    running: Option<Operation>,
    /// The action this page asked the daemon to start, until the daemon answers.
    starting: Option<Action>,
    followed: Option<(String, tokio::task::JoinHandle<()>)>,
    output: Vec<String>,
    logs: Option<tokio::task::JoinHandle<()>>,
    activity: tokio::task::JoinHandle<()>,
    console: Rc<Console>,
}

struct Console {
    buffer: gtk::TextBuffer,
    view: gtk::TextView,
    tags: RefCell<HashMap<String, String>>,
}

#[derive(Debug)]
pub(crate) enum ProjectMsg {
    Updated(Box<Project>),
    Run(Action),
    AskDown,
    Down(bool),
    RestartService(String),
    OpenService(String),
    Filter(Option<String>),
    OpenFolder,
    CopyPath,
    Forget,
}

#[derive(Debug)]
pub(crate) enum ProjectOutput {
    OpenContainer(String),
    Toast(String),
    Forgotten,
}

#[derive(Debug)]
pub(crate) enum ProjectCmd {
    Activity(Box<Activity>),
    Started(Result<Box<Operation>, String>),
    /// A line of the followed operation's output.
    Output(String, String),
    /// The followed operation in its final state, with the code Compose exited with.
    Finished(Box<Operation>, Option<i32>),
    Forgot(Result<(), String>),
}

pub(crate) struct ProjectWidgets {
    title: adw::WindowTitle,
    path: gtk::Label,
    error: gtk::Label,
    buttons: Vec<(Action, gtk::Button)>,
    down: gtk::Button,
    progress: gtk::Box,
    progress_label: gtk::Label,
    services: gtk::ListBox,
    chips: gtk::Box,
    forget: gio::SimpleAction,
}

impl Component for ProjectPage {
    type CommandOutput = ProjectCmd;
    type Input = ProjectMsg;
    type Output = ProjectOutput;
    type Init = ProjectInit;
    type Root = adw::NavigationPage;
    type Widgets = ProjectWidgets;

    fn init_root() -> Self::Root {
        adw::NavigationPage::builder().title("Project").build()
    }

    fn init(init: ProjectInit, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let ProjectInit { daemon, project } = init;
        root.set_title(&project.name);

        let title = adw::WindowTitle::new(&project.name, "");
        let menu = gio::Menu::new();
        menu.append(Some("Open Folder"), Some("project.open-folder"));
        menu.append(Some("Copy Path"), Some("project.copy-path"));
        let forget_section = gio::Menu::new();
        forget_section.append(Some("Forget Project"), Some("project.forget"));
        menu.append_section(None, &forget_section);
        let menu_button = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Project Menu")
            .menu_model(&menu)
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_end(&menu_button);

        let path = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["dim-label", "mono"])
            .build();
        let mut buttons = Vec::new();
        let actions_row = gtk::Box::builder().spacing(10).build();
        actions_row.append(&path);
        for (action, label, classes) in [
            (Action::Up, "Up", &["pill", "suggested-action"][..]),
            (Action::Pull, "Pull", &["raised-button"][..]),
            (Action::Restart, "Restart", &["raised-button"][..]),
            (Action::Stop, "Stop", &["raised-button"][..]),
        ] {
            let button = gtk::Button::builder()
                .label(label)
                .valign(gtk::Align::Center)
                .css_classes(classes.iter().map(|class| (*class).to_owned()).collect::<Vec<_>>())
                .build();
            let input = sender.input_sender().clone();
            button.connect_clicked(move |_| input.emit(ProjectMsg::Run(action)));
            actions_row.append(&button);
            buttons.push((action, button));
        }
        let down = gtk::Button::builder()
            .label("Down…")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        let input = sender.input_sender().clone();
        down.connect_clicked(move |_| input.emit(ProjectMsg::AskDown));
        actions_row.append(&down);

        let error = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .css_classes(["status-failed"])
            .build();
        let progress_label = gtk::Label::builder().xalign(0.0).css_classes(["dim-label"]).build();
        let progress = gtk::Box::builder().spacing(10).visible(false).build();
        progress.append(&adw::Spinner::new());
        progress.append(&progress_label);

        let services_header = GroupHeader::new("Services", "");
        let services = boxed_list();
        let input = sender.input_sender().clone();
        services.connect_row_activated(move |_, row| {
            let id = row.widget_name();
            if !id.is_empty() {
                input.emit(ProjectMsg::OpenService(id.to_string()));
            }
        });
        let services_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        services_section.append(&services_header.root);
        services_section.append(&services);

        let logs_title = gtk::Label::builder()
            .label("Logs")
            .css_classes(["group-title"])
            .margin_end(8)
            .build();
        let chips = gtk::Box::builder().spacing(6).build();
        let logs_header = gtk::Box::builder().spacing(6).margin_bottom(10).build();
        logs_header.append(&logs_title);
        logs_header.append(&chips);
        let buffer = gtk::TextBuffer::new(None);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .css_classes(["console-view"])
            .build();
        let console_scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .min_content_height(260)
            .css_classes(["console"])
            .build();
        let logs_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        logs_section.append(&logs_header);
        logs_section.append(&console_scroller);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        for widget in [
            actions_row.upcast_ref::<gtk::Widget>(),
            error.upcast_ref(),
            progress.upcast_ref(),
            services_section.upcast_ref(),
            logs_section.upcast_ref(),
        ] {
            column.append(widget);
        }
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(
            &daemon,
            "contr-compose-symbolic",
            &contr_ui_common::widgets::page_column(&column),
        )));
        root.set_child(Some(&toolbar));

        let actions = gio::SimpleActionGroup::new();
        for (name, message) in [
            ("open-folder", (|| ProjectMsg::OpenFolder) as fn() -> ProjectMsg),
            ("copy-path", || ProjectMsg::CopyPath),
            ("forget", || ProjectMsg::Forget),
        ] {
            let action = gio::SimpleAction::new(name, None);
            let input = sender.input_sender().clone();
            action.connect_activate(move |_, _| input.emit(message()));
            actions.add_action(&action);
        }
        let forget = actions
            .lookup_action("forget")
            .and_downcast::<gio::SimpleAction>()
            .unwrap_or_else(|| gio::SimpleAction::new("forget", None));
        root.insert_action_group("project", Some(&actions));

        let mut activity = daemon.subscribe_activity();
        let out = sender.command_sender().clone();
        let activity = relm4::spawn(async move {
            loop {
                out.emit(ProjectCmd::Activity(Box::new(activity.borrow_and_update().clone())));
                if activity.changed().await.is_err() {
                    return;
                }
            }
        });
        let mut model = ProjectPage {
            daemon,
            project: Project::default(),
            home: glib::home_dir(),
            running_ids: Vec::new(),
            filter: None,
            running: None,
            starting: None,
            followed: None,
            output: Vec::new(),
            logs: None,
            activity,
            console: Rc::new(Console {
                buffer,
                view,
                tags: RefCell::default(),
            }),
        };
        let mut widgets = ProjectWidgets {
            title,
            path,
            error,
            buttons,
            down,
            progress,
            progress_label,
            services,
            chips,
            forget,
        };
        model.update_project(*project, &mut widgets, &sender);
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ProjectMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ProjectMsg::Updated(project) => {
                self.update_project(*project, widgets, &sender);
                return;
            }
            ProjectMsg::Run(action) => self.run(&sender, action, Vec::new(), false),
            ProjectMsg::AskDown => ask_down(root, &self.project.name, sender.input_sender()),
            ProjectMsg::Down(remove_volumes) => self.run(&sender, Action::Down, Vec::new(), remove_volumes),
            ProjectMsg::RestartService(service) => self.run(&sender, Action::Restart, vec![service], false),
            ProjectMsg::OpenService(id) => {
                let _ = sender.output(ProjectOutput::OpenContainer(id));
            }
            ProjectMsg::Filter(filter) => {
                if filter != self.filter {
                    self.filter = filter;
                    self.follow_logs();
                }
            }
            ProjectMsg::OpenFolder => {
                let folder = gio::File::for_path(&self.project.working_dir);
                gtk::FileLauncher::new(Some(&folder)).launch(
                    root.root().and_downcast_ref::<gtk::Window>(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
            ProjectMsg::CopyPath => {
                root.clipboard()
                    .set_text(self.project.config_files.first().unwrap_or(&self.project.working_dir));
                let _ = sender.output(ProjectOutput::Toast("Copied the Compose file’s path".to_owned()));
            }
            ProjectMsg::Forget => {
                let Some(client) = self.client() else { return };
                let name = self.project.name.clone();
                sender.oneshot_command(async move {
                    let result = client.compose().remove_project(RemoveProjectRequest { name }).await;
                    ProjectCmd::Forgot(
                        result
                            .map(|_| ())
                            .map_err(|status| describe(&contr_sdk::Error::from(status))),
                    )
                });
            }
        }
        self.show_state(widgets);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ProjectCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ProjectCmd::Activity(activity) => {
                let name = &self.project.name;
                self.running = activity
                    .running_where(|kind| matches!(kind, Kind::ComposeAction(request) if request.project == *name))
                    .cloned();
                if let Some(id) = self.running.as_ref().map(|operation| operation.id.clone()) {
                    self.follow_action(&sender, id);
                }
            }
            ProjectCmd::Started(Ok(operation)) => {
                self.starting = None;
                let id = operation.id.clone();
                if operation.state() == OperationState::Running {
                    self.running = Some(*operation);
                }
                self.follow_action(&sender, id);
            }
            ProjectCmd::Started(Err(message)) => {
                self.starting = None;
                widgets.error.set_label(&message);
                widgets.error.set_visible(true);
            }
            ProjectCmd::Output(id, line) => {
                if self.followed.as_ref().is_some_and(|(followed, _)| *followed == id) {
                    widgets.progress_label.set_label(&line);
                    self.output.push(line);
                }
                return;
            }
            ProjectCmd::Finished(operation, code) => {
                if self
                    .followed
                    .as_ref()
                    .is_some_and(|(followed, _)| *followed == operation.id)
                {
                    self.followed = None;
                }
                if self.running.as_ref().is_some_and(|running| running.id == operation.id) {
                    self.running = None;
                }
                let action = compose_action(&operation);
                match (operation.state(), code, operation.error) {
                    (OperationState::Failed, Some(code), _) if code != 0 => {
                        show_failure(root, action, code, &self.output);
                    }
                    (OperationState::Failed, _, Some(error)) => {
                        widgets.error.set_label(&error.message);
                        widgets.error.set_visible(true);
                    }
                    _ => {}
                }
            }
            ProjectCmd::Forgot(Ok(())) => {
                let _ = sender.output(ProjectOutput::Forgotten);
            }
            ProjectCmd::Forgot(Err(message)) => {
                let _ = sender.output(ProjectOutput::Toast(message));
            }
        }
        self.show_state(widgets);
    }

    fn shutdown(&mut self, _widgets: &mut Self::Widgets, _output: relm4::Sender<ProjectOutput>) {
        self.activity.abort();
        for task in [self.logs.take(), self.followed.take().map(|(_, task)| task)]
            .into_iter()
            .flatten()
        {
            task.abort();
        }
    }
}

impl ProjectPage {
    fn client(&self) -> Option<Client> {
        self.daemon.status().running_client().cloned()
    }

    fn update_project(&mut self, project: Project, widgets: &mut ProjectWidgets, sender: &ComponentSender<Self>) {
        let view = model::project_view(&project, &self.home);
        widgets.title.set_subtitle(&view.summary());
        widgets.path.set_label(&view.path);
        widgets.path.set_tooltip_text(Some(&view.path));
        widgets.forget.set_enabled(project.added);
        if let Some(error) = &view.error {
            widgets.error.set_label(error);
        }
        widgets.error.set_visible(view.error.is_some());

        while let Some(child) = widgets.services.first_child() {
            widgets.services.remove(&child);
        }
        for service in &project.services {
            let view = model::service_view(service, &project.working_dir);
            widgets.services.append(&service_row(&view, sender.input_sender()));
        }

        let names: Vec<String> = project.services.iter().map(|service| service.name.clone()).collect();
        let current: Vec<String> = self
            .project
            .services
            .iter()
            .map(|service| service.name.clone())
            .collect();
        if names != current || widgets.chips.first_child().is_none() {
            chips(&widgets.chips, &names, self.filter.as_deref(), sender.input_sender());
        }
        let running_ids: Vec<String> = project
            .services
            .iter()
            .flat_map(|service| service.containers.iter())
            .filter(|container| container.state() == ContainerState::Running)
            .map(|container| container.id.clone())
            .collect();
        let restart_logs = running_ids != self.running_ids || self.logs.is_none();
        self.running_ids = running_ids;
        self.project = project;
        if restart_logs {
            self.follow_logs();
        }
        self.show_state(widgets);
    }

    /// The action running for the project, or the one this page is starting.
    fn action(&self) -> Option<Action> {
        self.running.as_ref().and_then(compose_action).or(self.starting)
    }

    fn show_state(&self, widgets: &ProjectWidgets) {
        let busy = self.action().is_some();
        let has_files = !self.project.config_files.is_empty() && self.project.config_error.is_empty();
        for (action, button) in &widgets.buttons {
            let needs_files = matches!(action, Action::Up | Action::Pull);
            button.set_sensitive(!busy && (has_files || !needs_files));
        }
        widgets.down.set_sensitive(!busy);
        widgets.progress.set_visible(busy);
        if let Some(action) = self.action()
            && self.output.is_empty()
        {
            widgets
                .progress_label
                .set_label(&format!("Running docker compose {}…", verb(action)));
        }
    }

    fn run(&mut self, sender: &ComponentSender<Self>, action: Action, services: Vec<String>, remove_volumes: bool) {
        let Some(client) = self.client() else { return };
        if self.action().is_some() {
            return;
        }
        self.starting = Some(action);
        self.output.clear();
        let request = RunActionRequest {
            project: self.project.name.clone(),
            action: action.into(),
            services,
            remove_volumes,
        };
        sender.oneshot_command(async move {
            let started = operations::start(&client, Kind::ComposeAction(request)).await;
            ProjectCmd::Started(started.map(Box::new).map_err(|error| describe(&error)))
        });
    }

    /// Shows the output of operation `id` from its start, until it finishes.
    fn follow_action(&mut self, sender: &ComponentSender<Self>, id: String) {
        if self.followed.as_ref().is_some_and(|(followed, _)| *followed == id) {
            return;
        }
        if let Some((_, task)) = self.followed.take() {
            task.abort();
        }
        let Some(client) = self.client() else { return };
        self.output.clear();
        let out = sender.command_sender().clone();
        let followed = id.clone();
        let task = relm4::spawn(async move {
            let mut exit_code = None;
            let finished = operations::follow(&client, &id, |event| {
                let FollowEvent::ComposeAction(response) = event else {
                    return;
                };
                match response.event {
                    Some(run_action_response::Event::Output(line)) => out.emit(ProjectCmd::Output(id.clone(), line)),
                    Some(run_action_response::Event::ExitCode(code)) => exit_code = Some(code),
                    None => {}
                }
            })
            .await;
            if let Ok(operation) = finished {
                out.emit(ProjectCmd::Finished(Box::new(operation), exit_code));
            }
        });
        self.followed = Some((followed, task));
    }

    fn follow_logs(&mut self) {
        if let Some(task) = self.logs.take() {
            task.abort();
        }
        self.console.buffer.set_text("");
        let Some(client) = self.client() else { return };
        let request = StreamLogsRequest {
            project: self.project.name.clone(),
            services: self.filter.iter().cloned().collect(),
            follow: true,
            tail_lines: Some(TAIL_LINES),
            timestamps: false,
        };
        let names: Vec<String> = self
            .project
            .services
            .iter()
            .map(|service| service.name.clone())
            .collect();
        let width = names.iter().map(String::len).max().unwrap_or(0);
        for (index, name) in names.iter().enumerate() {
            let tag = format!("service-{index}");
            if self.console.buffer.tag_table().lookup(&tag).is_none() {
                let color = SERVICE_COLORS[index % SERVICE_COLORS.len()];
                self.console.buffer.create_tag(Some(&tag), &[("foreground", &color)]);
            }
            self.console.tags.borrow_mut().insert(name.clone(), tag);
        }

        let (lines, mut receiver) = mpsc::unbounded_channel::<(String, String)>();
        self.logs = Some(relm4::spawn(async move {
            let Ok(stream) = client.compose().stream_logs(request).await else {
                return;
            };
            let mut stream = stream.into_inner();
            let mut pending: HashMap<String, String> = HashMap::new();
            while let Ok(Some(chunk)) = stream.message().await {
                let key = chunk.container_id.clone();
                let text = pending.entry(key).or_default();
                text.push_str(&String::from_utf8_lossy(&chunk.data));
                while let Some(end) = text.find('\n') {
                    let line: String = text.drain(..=end).collect();
                    let line = line.trim_end_matches(['\n', '\r']).to_owned();
                    if lines.send((chunk.service.clone(), line)).is_err() {
                        return;
                    }
                }
            }
        }));
        let console = Rc::downgrade(&self.console);
        glib::spawn_future_local(async move {
            while let Some((service, line)) = receiver.recv().await {
                let Some(console) = console.upgrade() else { return };
                console.append(&service, &line, width);
            }
        });
    }
}

impl Console {
    fn append(&self, service: &str, line: &str, width: usize) {
        let adjustment = self.view.vadjustment();
        let at_bottom = adjustment
            .as_ref()
            .is_none_or(|adjustment| adjustment.value() + adjustment.page_size() >= adjustment.upper() - 4.0);
        let mut end = self.buffer.end_iter();
        match self.tags.borrow().get(service) {
            Some(tag) => self
                .buffer
                .insert_with_tags_by_name(&mut end, &model::log_prefix(service, width), &[tag]),
            None => self.buffer.insert(&mut end, &model::log_prefix(service, width)),
        }
        self.buffer.insert(&mut end, &format!("{line}\n"));
        if at_bottom {
            let mark = self.buffer.create_mark(None, &self.buffer.end_iter(), false);
            self.view.scroll_mark_onscreen(&mark);
            self.buffer.delete_mark(&mark);
        }
    }
}

fn verb(action: Action) -> &'static str {
    match action {
        Action::Up => "up",
        Action::Pull => "pull",
        Action::Start => "start",
        Action::Stop => "stop",
        Action::Restart => "restart",
        Action::Down => "down",
        Action::Unspecified => "",
    }
}

fn compose_action(operation: &Operation) -> Option<Action> {
    match operations::kind(operation) {
        Some(Kind::ComposeAction(request)) => Some(request.action()),
        _ => None,
    }
}

fn service_row(view: &ServiceView, input: &relm4::Sender<ProjectMsg>) -> gtk::ListBoxRow {
    let status = gtk::Image::new();
    set_status(&status, view.status, view.status_word);
    let title = gtk::Label::builder().label(&view.name).xalign(0.0).build();
    let failure = gtk::Label::builder()
        .label(view.failure.as_deref().unwrap_or_default())
        .visible(view.failure.is_some())
        .css_classes(["row-subtitle-error", "group-description"])
        .build();
    let subtitle = gtk::Label::builder()
        .label(&view.subtitle)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim-label", "group-description"])
        .build();
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    line.append(&failure);
    line.append(&subtitle);
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(&title);
    text.append(&line);
    let content = gtk::Box::builder().spacing(12).css_classes(["container-row"]).build();
    content.append(&status);
    content.append(&text);
    if view.failed {
        let restart = gtk::Button::builder()
            .label("Restart")
            .valign(gtk::Align::Center)
            .css_classes(["raised-button"])
            .build();
        let name = view.name.clone();
        let input = input.clone();
        restart.connect_clicked(move |_| input.emit(ProjectMsg::RestartService(name.clone())));
        content.append(&restart);
    } else if view.container.is_some() {
        content.append(
            &gtk::Image::builder()
                .icon_name("go-next-symbolic")
                .css_classes(["dim-label"])
                .margin_end(6)
                .build(),
        );
    }
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(view.container.is_some() && !view.failed)
        .build();
    if let Some(container) = &view.container {
        row.set_widget_name(container);
    }
    row
}

fn chips(chips: &gtk::Box, services: &[String], filter: Option<&str>, input: &relm4::Sender<ProjectMsg>) {
    while let Some(child) = chips.first_child() {
        chips.remove(&child);
    }
    let all = gtk::ToggleButton::builder()
        .label("All")
        .active(filter.is_none())
        .css_classes(["chip-toggle"])
        .build();
    let emit = input.clone();
    all.connect_toggled(move |button| {
        if button.is_active() {
            emit.emit(ProjectMsg::Filter(None));
        }
    });
    chips.append(&all);
    for service in services {
        let chip = gtk::ToggleButton::builder()
            .label(service)
            .group(&all)
            .active(filter == Some(service.as_str()))
            .css_classes(["chip-toggle"])
            .build();
        let emit = input.clone();
        let name = service.clone();
        chip.connect_toggled(move |button| {
            if button.is_active() {
                emit.emit(ProjectMsg::Filter(Some(name.clone())));
            }
        });
        chips.append(&chip);
    }
}

fn ask_down(parent: &adw::NavigationPage, project: &str, input: &relm4::Sender<ProjectMsg>) {
    let dialog = adw::AlertDialog::new(
        Some(&format!("Take {project} Down?")),
        Some("Its containers and networks are removed. Named volumes are kept unless you remove them too."),
    );
    let volumes = gtk::CheckButton::with_label("Also remove named volumes");
    volumes.set_halign(gtk::Align::Center);
    dialog.set_extra_child(Some(&volumes));
    dialog.add_responses(&[("cancel", "Cancel"), ("down", "Down")]);
    dialog.set_response_appearance("down", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    dialog.connect_response(Some("down"), move |_, _| {
        input.emit(ProjectMsg::Down(volumes.is_active()));
    });
    dialog.present(Some(parent));
}

fn show_failure(parent: &adw::NavigationPage, action: Option<Action>, code: i32, output: &[String]) {
    let verb = action.map_or("", verb);
    let tail = output[output.len().saturating_sub(FAILURE_LINES)..].join("\n");
    let dialog = adw::AlertDialog::new(
        Some(&format!("docker compose {verb} Failed")),
        Some(&format!("Compose exited with code {code}.")),
    );
    let text = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .css_classes(["console-view"])
        .build();
    text.buffer().set_text(&tail);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .min_content_height(200)
        .min_content_width(420)
        .css_classes(["console"])
        .build();
    dialog.set_extra_child(Some(&scroller));
    dialog.add_response("close", "Close");
    dialog.present(Some(parent));
}
