use std::path::PathBuf;

use contr_containers_api::{ContainerTab, OpenContainer};
use contr_daemon_ui::{Daemon, EngineGate, Status, describe, while_running};
use contr_sdk::api::compose::v1::{AddProjectRequest, Project, WatchProjectsRequest};
use contr_ui_common::widgets::{GroupHeader, boxed_list, page_column, set_status};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentController, ComponentParts, ComponentSender, Controller};

use crate::model;
use crate::project::{ProjectInit, ProjectMsg, ProjectOutput, ProjectPage};

/// The Compose section: every project the engine knows, and each project's page.
pub struct ComposePage {
    daemon: Daemon,
    status: Status,
    projects: Vec<Project>,
    loaded: bool,
    home: PathBuf,
    detail: Option<(String, Controller<ProjectPage>)>,
}

#[derive(Debug)]
pub enum ComposeMsg {
    Activated(String),
    Add,
    AddFile(PathBuf),
    DetailClosed,
    OpenContainer(String),
    Toast(String),
    Finished(String),
    Forgotten,
}

#[derive(Debug)]
pub enum ComposeOutput {
    OpenContainer(OpenContainer),
    /// A Compose run finished; carries what to tell the user.
    Finished(String),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum ComposeCmd {
    Status(Box<Status>),
    Projects(Vec<Project>),
    Done(Result<String, String>),
}

pub struct ComposeWidgets {
    nav: adw::NavigationView,
    title: adw::WindowTitle,
    add: gtk::Button,
    content: gtk::Stack,
    list: gtk::ListBox,
}

impl Component for ComposePage {
    type CommandOutput = ComposeCmd;
    type Input = ComposeMsg;
    type Output = ComposeOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = ComposeWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let title = adw::WindowTitle::new("Compose", "");
        let add = gtk::Button::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("list-add-symbolic")
                    .label("Add")
                    .build(),
            )
            .tooltip_text("Add a Project from Its Compose File")
            .build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&add);

        let projects_header = GroupHeader::new("Projects", "From containers Compose created, and projects you added.");
        let list = boxed_list();
        let input = sender.input_sender().clone();
        list.connect_row_activated(move |_, row| input.emit(ComposeMsg::Activated(row.widget_name().to_string())));
        let section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        section.append(&projects_header.root);
        section.append(&list);

        let add_first = gtk::Button::builder()
            .label("Add a Project…")
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("contr-compose-symbolic")
            .title("No Compose Projects")
            .description("Projects show up here when you run docker compose up, or add one from its compose.yaml.")
            .child(&add_first)
            .vexpand(true)
            .build();
        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&page_column(&section), Some("list"));
        content.add_named(&empty, Some("empty"));
        content.add_named(&loading, Some("loading"));
        for button in [&add, &add_first] {
            let input = sender.input_sender().clone();
            button.connect_clicked(move |_| input.emit(ComposeMsg::Add));
        }

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(&daemon, "contr-compose-symbolic", &content)));
        let nav = adw::NavigationView::new();
        nav.add(&adw::NavigationPage::with_tag(&toolbar, "Compose", "compose"));
        let input = sender.input_sender().clone();
        nav.connect_popped(move |_, _| input.emit(ComposeMsg::DetailClosed));
        root.set_child(Some(&nav));
        follow(&daemon, &sender);

        let model = ComposePage {
            status: daemon.status(),
            daemon,
            projects: Vec::new(),
            loaded: false,
            home: glib::home_dir(),
            detail: None,
        };
        let widgets = ComposeWidgets {
            nav,
            title,
            add,
            content,
            list,
        };
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ComposeMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ComposeMsg::Activated(name) => self.open(widgets, &sender, &name),
            ComposeMsg::Add => choose_file(root, sender.input_sender()),
            ComposeMsg::AddFile(path) => {
                let Some(client) = self.status.running_client().cloned() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let request = AddProjectRequest {
                        config_files: vec![path.to_string_lossy().into_owned()],
                    };
                    let result = client.compose().add_project(request).await;
                    ComposeCmd::Done(
                        result
                            .map(|response| {
                                let name = response
                                    .into_inner()
                                    .project
                                    .map(|project| project.name)
                                    .unwrap_or_default();
                                format!("Added {name}")
                            })
                            .map_err(|status| describe(&contr_sdk::Error::from(status))),
                    )
                });
            }
            ComposeMsg::DetailClosed => {
                if widgets.nav.visible_page().and_then(|page| page.tag()).as_deref() == Some("compose") {
                    self.detail = None;
                }
            }
            ComposeMsg::OpenContainer(id) => {
                let _ = sender.output(ComposeOutput::OpenContainer(OpenContainer {
                    id,
                    tab: ContainerTab::Logs,
                }));
            }
            ComposeMsg::Toast(message) => root.add_toast(adw::Toast::new(&message)),
            ComposeMsg::Finished(message) => {
                root.add_toast(adw::Toast::new(&message));
                let _ = sender.output(ComposeOutput::Finished(message));
            }
            ComposeMsg::Forgotten => {
                widgets.nav.pop_to_tag("compose");
                self.detail = None;
            }
        }
        self.refresh(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: ComposeCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            ComposeCmd::Status(status) => {
                self.status = *status;
                if !self.status.is_running() {
                    self.projects.clear();
                    self.loaded = false;
                }
            }
            ComposeCmd::Projects(projects) => {
                self.projects = projects;
                self.loaded = true;
                if let Some((name, detail)) = &self.detail {
                    if let Some(project) = self.projects.iter().find(|project| &project.name == name) {
                        detail.emit(ProjectMsg::Updated(Box::new(project.clone())));
                    } else {
                        widgets.nav.pop_to_tag("compose");
                        self.detail = None;
                    }
                }
            }
            ComposeCmd::Done(Ok(message) | Err(message)) => root.add_toast(adw::Toast::new(&message)),
        }
        self.refresh(widgets, &sender);
    }
}

impl ComposePage {
    fn refresh(&self, widgets: &mut ComposeWidgets, _sender: &ComponentSender<Self>) {
        let running = self.status.is_running();
        widgets
            .title
            .set_subtitle(&match (running && self.loaded, self.projects.len()) {
                (false, _) => String::new(),
                (true, 1) => "1 project".to_owned(),
                (true, count) => format!("{count} projects"),
            });
        widgets.add.set_sensitive(running);
        widgets
            .content
            .set_visible_child_name(match (self.loaded, self.projects.is_empty()) {
                (false, _) => "loading",
                (true, true) => "empty",
                (true, false) => "list",
            });
        while let Some(child) = widgets.list.first_child() {
            widgets.list.remove(&child);
        }
        for project in &self.projects {
            widgets
                .list
                .append(&project_row(&model::project_view(project, &self.home)));
        }
    }

    fn open(&mut self, widgets: &ComposeWidgets, sender: &ComponentSender<Self>, name: &str) {
        let Some(project) = self.projects.iter().find(|project| project.name == name).cloned() else {
            return;
        };
        widgets.nav.pop_to_tag("compose");
        let detail = ProjectPage::builder()
            .launch(ProjectInit {
                daemon: self.daemon.clone(),
                project: Box::new(project),
            })
            .forward(sender.input_sender(), |output| match output {
                ProjectOutput::OpenContainer(id) => ComposeMsg::OpenContainer(id),
                ProjectOutput::Toast(message) => ComposeMsg::Toast(message),
                ProjectOutput::Finished(message) => ComposeMsg::Finished(message),
                ProjectOutput::Forgotten => ComposeMsg::Forgotten,
            });
        widgets.nav.push(detail.widget());
        self.detail = Some((name.to_owned(), detail));
    }
}

fn project_row(view: &model::ProjectView) -> gtk::ListBoxRow {
    let status = gtk::Image::new();
    set_status(&status, view.status(), &view.summary());
    let title = gtk::Label::builder().label(&view.name).xalign(0.0).build();
    let subtitle = gtk::Label::builder()
        .label(match &view.error {
            Some(error) => error.clone(),
            None => format!("{} · {}", view.path, view.summary()),
        })
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes([
            "group-description",
            if view.error.is_some() {
                "row-subtitle-error"
            } else {
                "dim-label"
            },
        ])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    text.append(&title);
    text.append(&subtitle);
    let content = gtk::Box::builder().spacing(12).css_classes(["container-row"]).build();
    content.append(&status);
    content.append(&text);
    content.append(
        &gtk::Image::builder()
            .icon_name("go-next-symbolic")
            .css_classes(["dim-label"])
            .margin_end(6)
            .build(),
    );
    gtk::ListBoxRow::builder().child(&content).name(&view.name).build()
}

fn choose_file(parent: &adw::ToastOverlay, input: &relm4::Sender<ComposeMsg>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("Compose files"));
    for pattern in ["*.yaml", "*.yml"] {
        filter.add_pattern(pattern);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let dialog = gtk::FileDialog::builder()
        .title("Add a Compose Project")
        .filters(&filters)
        .build();
    let input = input.clone();
    dialog.open(
        parent.root().and_downcast_ref::<gtk::Window>(),
        gio::Cancellable::NONE,
        move |result| {
            if let Some(path) = result.ok().and_then(|file| file.path()) {
                input.emit(ComposeMsg::AddFile(path));
            }
        },
    );
}

fn follow(daemon: &Daemon, sender: &ComponentSender<ComposePage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(ComposeCmd::Status(Box::new(status.borrow_and_update().clone())));
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
                    let Ok(stream) = client.compose().watch_projects(WatchProjectsRequest {}).await else {
                        return;
                    };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        out.emit(ComposeCmd::Projects(response.projects));
                    }
                }
            }))
            .drop_on_shutdown()
    });
}
