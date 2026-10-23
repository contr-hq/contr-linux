use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;

use contr_compose::{ComposeOutput, ComposePage};
use contr_containers::{ContainersMsg, ContainersOutput, ContainersPage};
use contr_containers_api::{ContainerTab, OpenContainer, RunImage};
use contr_core::product;
use contr_daemon_ui::operations::{self, Activity, Kind, Operation};
use contr_daemon_ui::{
    Daemon, EngineAction, SettingsHandle, Status, TROUBLESHOOT_ACTION, describe, engine_name, remediation_label,
    while_running,
};
use contr_images::{ImagesMsg, ImagesOutput, ImagesPage};
use contr_kubernetes::{KubernetesOutput, KubernetesPage};
use contr_networks::{NetworksOutput, NetworksPage};
use contr_onboarding::{OnboardingOutput, OnboardingPage};
use contr_sdk::ClientIdentity;
use contr_sdk::api::common::v1::Remediation;
use contr_sdk::api::containers::v1::{
    Container, ContainerState, RestartContainerRequest, WatchContainersRequest, watch_containers_response,
};
use contr_sdk::api::daemon::v1::Capability;
use contr_sdk::api::diagnostics::v1::RunRemediationRequest;
use contr_sdk::api::engine::v1::EngineState;
use contr_sdk::api::kubernetes::v1::ClusterState;
use contr_sdk::api::operations::v1::OperationState;
use contr_sdk::api::updates::v1::{GetUpdateStatusRequest, UpdateStatus};
use contr_troubleshoot::{TroubleshootOutput, TroubleshootPage};
use contr_volumes::{VolumesOutput, VolumesPage};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentController, ComponentParts, ComponentSender, Controller};

use crate::activity::ActivityView;
use crate::client_settings::ClientSettings;
use crate::engine_card::EngineCard;
use crate::preferences::{self, PreferencesContext};
use crate::sections::Section;
use crate::tray::{self, ContrTray, RunningContainer, TrayState};
use crate::{notifications, sandbox, shortcuts, updates};

/// How often the client reads the daemon's latest update check.
const UPDATE_POLL: std::time::Duration = std::time::Duration::from_hours(1);

/// Name of the window's page that holds the sidebar and sections.
const MAIN_PAGE: &str = "main";

pub(crate) struct App {
    daemon: Daemon,
    status: Status,
    /// The daemon's operations as last shown, to tell which have finished since.
    activity: Activity,
    containers: BTreeMap<String, Container>,
    section: Section,
    tray: Option<ksni::Handle<ContrTray>>,
    tray_state: TrayState,
    tray_available: bool,
    settings: Rc<RefCell<ClientSettings>>,
    /// Started from the login item: the first activation leaves the window hidden.
    background: bool,
    problems: usize,
    containers_page: Controller<ContainersPage>,
    images_page: Controller<ImagesPage>,
    /// Held only to keep the page running; dropping a controller shuts its component down.
    _volumes_page: Controller<VolumesPage>,
    /// Held only to keep the page running.
    _networks_page: Controller<NetworksPage>,
    /// Held only to keep the page running.
    _compose_page: Controller<ComposePage>,
    /// Held only to keep the page running.
    _troubleshoot_page: Controller<TroubleshootPage>,
    /// Held only to keep the page running.
    _kubernetes_page: Controller<KubernetesPage>,
    cluster: ClusterState,
    /// The newer release the banner announces.
    notice: Option<updates::Notice>,
    /// Shown instead of the sections until the user finishes or skips setup.
    onboarding: Option<Controller<OnboardingPage>>,
}

#[derive(Debug)]
pub(crate) enum AppMsg {
    Select(Section),
    Present,
    OpenContainer(String),
    Open(OpenContainer),
    Run(RunImage),
    PullImage,
    Engine(EngineAction),
    RestartContainer(String),
    Problems(usize),
    CancelOperation(String),
    Fix(Remediation),
    /// Asks where to save the bundle of a finished bundle operation.
    SaveBundle(String),
    OpenRelease,
    Cluster(ClusterState),
    ClusterReady,
    Onboarded,
    Preferences,
    Shortcuts,
    Quit,
}

#[derive(Debug)]
pub(crate) enum AppCmd {
    Status(Box<Status>),
    Containers(ContainerEvent),
    Tray(Option<TrayHandle>),
    Updates {
        status: Box<UpdateStatus>,
        check: bool,
    },
    Activity(Box<Activity>),
    /// How a finished operation ended; `claimed` when the page that started it tells the user itself.
    Reported {
        operation: Box<Operation>,
        message: String,
        claimed: bool,
    },
    Notice(String),
    BundleSaved(Result<PathBuf, String>),
    Failed(String),
}

pub(crate) struct TrayHandle(ksni::Handle<ContrTray>);

impl std::fmt::Debug for TrayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrayHandle")
    }
}

#[derive(Debug)]
pub(crate) enum ContainerEvent {
    Snapshot(Vec<Container>),
    Upserted(Box<Container>),
    Removed(String),
}

pub(crate) struct AppWidgets {
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    rows: HashMap<Section, SidebarRow>,
    stack: gtk::Stack,
    card: EngineCard,
    engine_menu: gio::Menu,
    activity: ActivityView,
    toasts: adw::ToastOverlay,
    banner: adw::Banner,
}

struct SidebarRow {
    row: gtk::ListBoxRow,
    count: gtk::Label,
    dot: gtk::Box,
    busy: adw::Spinner,
}

impl Component for App {
    type CommandOutput = AppCmd;
    type Input = AppMsg;
    type Output = ();
    /// Whether the client started from the login item.
    type Init = bool;
    type Root = adw::ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::builder()
            .title(product::DISPLAY_NAME)
            .default_width(1280)
            .default_height(820)
            .width_request(360)
            .height_request(294)
            .build()
    }

    fn init(background: bool, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let daemon = Daemon::connect(ClientIdentity {
            name: format!("{}-desktop", product::SLUG),
            version: product::VERSION.to_owned(),
        });
        let settings = Rc::new(RefCell::new(ClientSettings::load()));

        let (list, rows) = sidebar(&sender);
        let card = EngineCard::new();
        let engine_menu = gio::Menu::new();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&primary_menu(&engine_menu))
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        let activity = ActivityView::new(sender.input_sender().clone());
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.pack_end(&menu_button);
        sidebar_header.pack_end(&activity.button);
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&sidebar_header);
        sidebar_view.set_content(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .child(&list)
                .build(),
        ));
        sidebar_view.add_bottom_bar(&card.root);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        let containers_page =
            ContainersPage::builder()
                .launch(daemon.clone())
                .forward(sender.input_sender(), |output| match output {
                    ContainersOutput::Show => AppMsg::Select(Section::Containers),
                    ContainersOutput::PullImage => AppMsg::PullImage,
                });
        let images_page = ImagesPage::builder()
            .launch(daemon.clone())
            .forward(sender.input_sender(), |output| match output {
                ImagesOutput::OpenContainer(open) => AppMsg::Open(open),
                ImagesOutput::Run(run) => AppMsg::Run(run),
            });
        let volumes_page = VolumesPage::builder()
            .launch(daemon.clone())
            .forward(sender.input_sender(), |output| match output {
                VolumesOutput::OpenContainer(open) => AppMsg::Open(open),
            });
        let networks_page = NetworksPage::builder()
            .launch(daemon.clone())
            .forward(sender.input_sender(), |output| match output {
                NetworksOutput::OpenContainer(open) => AppMsg::Open(open),
            });
        let compose_page = ComposePage::builder()
            .launch(daemon.clone())
            .forward(sender.input_sender(), |output| match output {
                ComposeOutput::OpenContainer(open) => AppMsg::Open(open),
            });
        let troubleshoot_page =
            TroubleshootPage::builder()
                .launch(daemon.clone())
                .forward(sender.input_sender(), |output| match output {
                    TroubleshootOutput::Problems(count) => AppMsg::Problems(count),
                });
        let kubernetes_page =
            KubernetesPage::builder()
                .launch(daemon.clone())
                .forward(sender.input_sender(), |output| match output {
                    KubernetesOutput::State(state) => AppMsg::Cluster(state),
                    KubernetesOutput::Ready => AppMsg::ClusterReady,
                });
        for section in Section::ALL {
            match section {
                Section::Troubleshoot => stack.add_named(troubleshoot_page.widget(), Some(section.name())),
                Section::Compose => stack.add_named(compose_page.widget(), Some(section.name())),
                Section::Containers => stack.add_named(containers_page.widget(), Some(section.name())),
                Section::Images => stack.add_named(images_page.widget(), Some(section.name())),
                Section::Volumes => stack.add_named(volumes_page.widget(), Some(section.name())),
                Section::Networks => stack.add_named(networks_page.widget(), Some(section.name())),
                Section::Kubernetes => stack.add_named(kubernetes_page.widget(), Some(section.name())),
            };
        }
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(220.0)
            .max_sidebar_width(260.0)
            .sidebar(&adw::NavigationPage::new(&sidebar_view, product::DISPLAY_NAME))
            .content(&adw::NavigationPage::new(&toasts, Section::Containers.title()))
            .build();
        let content = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        let banner = adw::Banner::builder().button_label("Release Notes").build();
        let banner_input = sender.input_sender().clone();
        banner.connect_button_clicked(move |_| banner_input.emit(AppMsg::OpenRelease));
        split.set_vexpand(true);
        let main = gtk::Box::new(gtk::Orientation::Vertical, 0);
        main.append(&banner);
        main.append(&split);
        content.add_named(&main, Some(MAIN_PAGE));
        let onboarding = (!settings.borrow().onboarded).then(|| {
            let onboarding = OnboardingPage::builder().launch(daemon.clone()).forward(
                sender.input_sender(),
                |output| match output {
                    OnboardingOutput::Done => AppMsg::Onboarded,
                },
            );
            content.add_named(onboarding.widget(), None);
            content.set_visible_child(onboarding.widget());
            onboarding
        });
        root.set_content(Some(&content));

        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        root.add_breakpoint(narrow);

        install_actions(&root, &sender);
        let keep_running = Rc::clone(&settings);
        root.connect_close_request(move |window| {
            if keep_running.borrow().keep_running {
                window.set_visible(false);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        relm4::main_application().connect_activate({
            let input = sender.input_sender().clone();
            move |_| input.emit(AppMsg::Present)
        });

        follow_status(&daemon, &sender);
        follow_activity(&daemon, &sender);
        follow_updates(&daemon, &sender);
        follow_containers(&daemon, &sender);
        start_tray(&sender);

        let model = App {
            status: daemon.status(),
            activity: Activity::default(),
            daemon,
            containers: BTreeMap::new(),
            section: Section::Containers,
            tray: None,
            tray_state: TrayState::default(),
            tray_available: false,
            settings,
            background,
            problems: 0,
            containers_page,
            images_page,
            _volumes_page: volumes_page,
            _networks_page: networks_page,
            _compose_page: compose_page,
            _troubleshoot_page: troubleshoot_page,
            _kubernetes_page: kubernetes_page,
            cluster: ClusterState::Unspecified,
            notice: None,
            onboarding,
        };
        if let Some(first) = rows.get(&Section::Containers) {
            list.select_row(Some(&first.row));
        }
        let widgets = AppWidgets {
            split,
            list,
            rows,
            stack,
            card,
            engine_menu,
            activity,
            toasts,
            banner,
        };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: AppMsg, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AppMsg::Select(section) => self.section = section,
            AppMsg::Present if self.background => self.background = false,
            AppMsg::Present => root.present(),
            AppMsg::OpenContainer(id) => {
                root.present();
                sender.input(AppMsg::Open(OpenContainer {
                    id,
                    tab: ContainerTab::Logs,
                }));
            }
            AppMsg::Open(open) => {
                self.section = Section::Containers;
                self.containers_page.emit(ContainersMsg::Open(open));
            }
            AppMsg::Run(run) => self.containers_page.emit(ContainersMsg::Run(run)),
            AppMsg::PullImage => {
                self.section = Section::Images;
                self.images_page.emit(ImagesMsg::AskPull);
            }
            AppMsg::Engine(action) => {
                let daemon = self.daemon.clone();
                sender.oneshot_command(async move {
                    match daemon.start(action).await {
                        Ok(_) => AppCmd::Failed(String::new()),
                        Err(error) => AppCmd::Failed(describe(&error)),
                    }
                });
            }
            AppMsg::RestartContainer(id) => {
                let Some(client) = self.status.running_client().cloned() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let request = RestartContainerRequest {
                        id,
                        timeout_seconds: None,
                    };
                    match client.containers().restart_container(request).await {
                        Ok(_) => AppCmd::Failed(String::new()),
                        Err(status) => AppCmd::Failed(describe(&status.into())),
                    }
                });
            }
            AppMsg::Problems(count) => self.problems = count,
            AppMsg::CancelOperation(id) => {
                let Some(client) = self.status.client().cloned() else {
                    return;
                };
                sender.oneshot_command(async move {
                    match operations::cancel(&client, &id).await {
                        Ok(()) => AppCmd::Failed(String::new()),
                        Err(error) => AppCmd::Failed(describe(&error)),
                    }
                });
            }
            AppMsg::Fix(remediation) => {
                let Some(client) = self.status.client().cloned() else {
                    return;
                };
                let request = RunRemediationRequest {
                    action_id: remediation.action_id,
                };
                sender.oneshot_command(async move {
                    match operations::start(&client, Kind::RunRemediation(request)).await {
                        Ok(_) => AppCmd::Failed(String::new()),
                        Err(error) => AppCmd::Failed(describe(&error)),
                    }
                });
            }
            AppMsg::SaveBundle(id) => self.save_bundle(root, &sender, id),
            AppMsg::OpenRelease => {
                if let Some(notice) = self.notice.take() {
                    gtk::UriLauncher::new(&notice.url).launch(Some(root), gio::Cancellable::NONE, |_| {});
                    self.settings.borrow_mut().dismissed_update = Some(notice.version);
                    if let Err(error) = self.settings.borrow().save() {
                        tracing::warn!(%error, "could not save the client settings");
                    }
                }
            }
            AppMsg::Cluster(state) => self.cluster = state,
            AppMsg::ClusterReady => {
                if !root.is_active() {
                    notifications::finished("Kubernetes is ready");
                }
            }
            AppMsg::Onboarded => {
                self.settings.borrow_mut().onboarded = true;
                if let Err(error) = self.settings.borrow().save() {
                    tracing::warn!(%error, "could not save the client settings");
                }
                if let Some(onboarding) = self.onboarding.take() {
                    let page = onboarding.widget().clone();
                    if let Some(content) = page.parent().and_downcast::<gtk::Stack>() {
                        content.set_visible_child_name(MAIN_PAGE);
                        content.remove(&page);
                    }
                }
            }
            AppMsg::Preferences => preferences::present(
                root,
                &PreferencesContext {
                    daemon: self.daemon.clone(),
                    settings: Rc::clone(&self.settings),
                    tray_available: self.tray_available,
                },
            ),
            AppMsg::Shortcuts => shortcuts::present(root),
            AppMsg::Quit => relm4::main_application().quit(),
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: AppCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            AppCmd::Status(status) => self.status = *status,
            AppCmd::Updates { status, check } => {
                let dismissed = self.settings.borrow().dismissed_update.clone();
                self.notice = updates::notice(&status, check, dismissed.as_deref());
            }
            AppCmd::Containers(ContainerEvent::Snapshot(containers)) => {
                self.containers = containers.into_iter().map(|c| (c.id.clone(), c)).collect();
            }
            AppCmd::Containers(ContainerEvent::Upserted(container)) => {
                if self.settings.borrow().notify.container_errors
                    && !root.is_active()
                    && let Some(crash) = notifications::crash(self.containers.get(&container.id), &container)
                {
                    notifications::send_crash(&crash);
                }
                self.containers.insert(container.id.clone(), *container);
            }
            AppCmd::Containers(ContainerEvent::Removed(id)) => {
                self.containers.remove(&id);
            }
            AppCmd::Tray(Some(TrayHandle(handle))) => {
                self.tray = Some(handle);
                self.tray_available = true;
            }
            AppCmd::Tray(None) => self.tray_available = false,
            AppCmd::Activity(activity) => {
                self.report_finished(&sender, &activity);
                widgets.activity.show(&activity);
                self.activity = *activity;
                // Progress arrives several times a second; nothing else in the window depends on it.
                return;
            }
            AppCmd::Reported {
                operation,
                message,
                claimed,
            } => {
                if !claimed {
                    widgets
                        .toasts
                        .add_toast(result_toast(&operation, &message, sender.input_sender()));
                }
                if self.settings.borrow().notify.finished && !root.is_active() {
                    notifications::finished(&message);
                }
            }
            AppCmd::BundleSaved(Ok(path)) => {
                widgets
                    .toasts
                    .add_toast(contr_ui_common::widgets::saved_toast(&widgets.toasts, &path));
            }
            AppCmd::Notice(message) | AppCmd::BundleSaved(Err(message)) => {
                widgets.toasts.add_toast(adw::Toast::new(&message));
            }
            AppCmd::Failed(message) => {
                if !message.is_empty() {
                    widgets.toasts.add_toast(adw::Toast::new(&message));
                }
            }
        }
        if !self.status.is_running() {
            self.containers.clear();
        }
        self.update_tray();
        self.update_view(widgets, sender);
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        if let Some(notice) = &self.notice {
            widgets
                .banner
                .set_title(&format!("{} {} is available", product::DISPLAY_NAME, notice.version));
        }
        widgets.banner.set_revealed(self.notice.is_some());
        widgets.card.show(&self.status);

        if let Some(row) = widgets.rows.get(&Section::Containers) {
            let running = self.status.is_running();
            row.count.set_label(&self.containers.len().to_string());
            row.count.set_visible(running);
        }
        if let Some(row) = widgets.rows.get(&Section::Troubleshoot) {
            row.count.set_label(&self.problems.to_string());
            row.count.set_visible(self.problems > 0);
            row.count.add_css_class("sidebar-badge");
        }
        if let Some(row) = widgets.rows.get(&Section::Kubernetes) {
            let busy = matches!(self.cluster, ClusterState::Starting | ClusterState::Stopping);
            let ready = self.cluster == ClusterState::Ready;
            row.busy.set_visible(busy);
            row.dot.set_visible(ready);
            row.dot.set_tooltip_text(ready.then_some("Cluster ready"));
        }
        for (section, capability) in [
            (Section::Compose, Capability::Compose),
            (Section::Kubernetes, Capability::Kubernetes),
        ] {
            if let Some(row) = widgets.rows.get(&section) {
                row.row.set_visible(self.status.has(capability));
            }
        }
        if let Some(row) = widgets.rows.get(&self.section)
            && widgets.list.selected_row().as_ref() != Some(&row.row)
        {
            widgets.list.select_row(Some(&row.row));
        }
        if widgets.stack.visible_child_name().as_deref() != Some(self.section.name()) {
            widgets.stack.set_visible_child_name(self.section.name());
            if let Some(content) = widgets.split.content() {
                content.set_title(self.section.title());
            }
            widgets.split.set_show_content(true);
        }

        widgets.engine_menu.remove_all();
        match self.status.engine {
            EngineState::Running => widgets.engine_menu.append(Some("Stop Engine"), Some("app.stop-engine")),
            EngineState::Stopped | EngineState::Failed | EngineState::Uninitialized => {
                widgets
                    .engine_menu
                    .append(Some("Start Engine"), Some("app.start-engine"));
            }
            _ => {}
        }
        widgets.engine_menu.append(Some("Quit"), Some("app.quit"));
    }
}

impl App {
    /// Reports each operation that finished since the activity last changed, reading its events when the report
    /// needs them.
    fn report_finished(&self, sender: &ComponentSender<Self>, activity: &Activity) {
        let Some(client) = self.status.client() else {
            return;
        };
        for operation in operations::finished_since(&self.activity, activity) {
            let claimed = self.daemon.take_claim(&operation.id);
            if !operations::reported(operation) {
                continue;
            }
            let client = client.clone();
            let operation = operation.clone();
            sender.oneshot_command(async move {
                let message = operations::report(&client, &operation).await;
                AppCmd::Reported {
                    operation: Box::new(operation),
                    message,
                    claimed,
                }
            });
        }
    }

    fn save_bundle(&self, root: &adw::ApplicationWindow, sender: &ComponentSender<Self>, id: String) {
        let Some(client) = self.status.client().cloned() else {
            return;
        };
        let dialog = gtk::FileDialog::builder()
            .title("Save Diagnostics Bundle")
            .initial_name(operations::todays_bundle_name())
            .build();
        let out = sender.command_sender().clone();
        dialog.save(Some(root), gio::Cancellable::NONE, move |result| {
            let Some(path) = result.ok().and_then(|file| file.path()) else {
                return;
            };
            relm4::spawn(async move {
                let saved = operations::save_bundle(&client, &id, &path, |_| {}).await;
                out.emit(AppCmd::BundleSaved(saved.map(|()| path)));
            });
        });
    }

    fn update_tray(&mut self) {
        let state = tray_state(&self.status, &self.containers);
        if state == self.tray_state {
            return;
        }
        self.tray_state = state.clone();
        if let Some(handle) = self.tray.clone() {
            relm4::spawn(async move {
                handle.update(|tray| tray.state = state).await;
            });
        }
    }
}

/// A toast for `message`, with the next step the operation's end suggests.
fn result_toast(operation: &Operation, message: &str, input: &relm4::Sender<AppMsg>) -> adw::Toast {
    let toast = adw::Toast::new(message);
    let next = match (operation.state(), operations::kind(operation)) {
        (OperationState::Succeeded, Some(Kind::PullImage(request))) => Some((
            "Run".to_owned(),
            AppMsg::Run(RunImage {
                image: request.reference.clone(),
            }),
        )),
        (OperationState::Succeeded, Some(Kind::CreateBundle(_))) => {
            Some(("Save…".to_owned(), AppMsg::SaveBundle(operation.id.clone())))
        }
        (OperationState::Failed, _) => {
            toast.set_timeout(10);
            operation
                .error
                .as_ref()
                .and_then(|error| error.remediations.first())
                .map(|remediation| (remediation_label(remediation), AppMsg::Fix(remediation.clone())))
        }
        _ => None,
    };
    if let Some((label, message)) = next {
        toast.set_button_label(Some(&label));
        let input = input.clone();
        let message = std::cell::Cell::new(Some(message));
        toast.connect_button_clicked(move |_| {
            if let Some(message) = message.take() {
                input.emit(message);
            }
        });
    }
    toast
}

fn tray_state(status: &Status, containers: &BTreeMap<String, Container>) -> TrayState {
    let card = crate::engine_card::describe(status);
    let running: Vec<RunningContainer> = containers
        .values()
        .filter(|container| container.state() == ContainerState::Running)
        .map(|container| RunningContainer {
            id: container.id.clone(),
            name: container.names.first().cloned().unwrap_or_else(|| container.id.clone()),
            address: container
                .ports
                .iter()
                .find(|port| port.host_port != 0)
                .map(|port| format!("localhost:{}", port.host_port))
                .unwrap_or_default(),
        })
        .collect();
    let subtitle = match (&status.engine_info, status.is_running()) {
        (Some(info), true) => format!("{} · {} running", engine_name(info), running.len()),
        _ => card.subtitle,
    };
    TrayState {
        title: card.title.to_owned(),
        subtitle,
        running,
        engine: match status.engine {
            EngineState::Running => Some(EngineAction::Restart),
            EngineState::Stopped | EngineState::Failed | EngineState::Uninitialized => Some(EngineAction::Start),
            _ => None,
        },
    }
}

fn sidebar(sender: &ComponentSender<App>) -> (gtk::ListBox, HashMap<Section, SidebarRow>) {
    let list = gtk::ListBox::builder()
        .css_classes(["navigation-sidebar", "sidebar-list"])
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    let mut rows = HashMap::new();
    for section in Section::ALL {
        let icon = gtk::Image::from_icon_name(section.icon_name());
        let label = gtk::Label::builder()
            .label(section.title())
            .xalign(0.0)
            .hexpand(true)
            .build();
        let count = gtk::Label::builder()
            .css_classes(["sidebar-count"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["sidebar-dot"])
            .visible(false)
            .build();
        let busy = adw::Spinner::builder().visible(false).build();
        let content = gtk::Box::builder().spacing(12).build();
        content.append(&icon);
        content.append(&label);
        content.append(&count);
        content.append(&dot);
        content.append(&busy);
        let row = gtk::ListBoxRow::builder().child(&content).name(section.name()).build();
        list.append(&row);
        rows.insert(section, SidebarRow { row, count, dot, busy });
    }
    list.set_header_func(|row, _| {
        let header = (row.widget_name() == Section::Troubleshoot.name()).then(|| {
            gtk::Separator::builder()
                .margin_top(6)
                .margin_bottom(6)
                .margin_start(4)
                .margin_end(4)
                .build()
        });
        row.set_header(header.as_ref());
    });
    let input = sender.input_sender().clone();
    list.connect_row_selected(move |_, row| {
        if let Some(section) = row.and_then(|row| Section::from_name(&row.widget_name())) {
            input.emit(AppMsg::Select(section));
        }
    });
    (list, rows)
}

fn primary_menu(engine: &gio::Menu) -> gio::Menu {
    let menu = gio::Menu::new();
    let general = gio::Menu::new();
    general.append(Some("Preferences"), Some("app.preferences"));
    general.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    general.append(Some(&format!("About {}", product::DISPLAY_NAME)), Some("app.about"));
    menu.append_section(None, &general);
    menu.append_section(None, engine);
    menu
}

fn install_actions(window: &adw::ApplicationWindow, sender: &ComponentSender<App>) {
    let app = relm4::main_application();
    let action = |name: &str, message: fn() -> AppMsg| {
        let action = gio::SimpleAction::new(name, None);
        let input = sender.input_sender().clone();
        action.connect_activate(move |_, _| input.emit(message()));
        action
    };
    app.add_action(&action("quit", || AppMsg::Quit));
    app.add_action(&action("start-engine", || AppMsg::Engine(EngineAction::Start)));
    app.add_action(&action("stop-engine", || AppMsg::Engine(EngineAction::Stop)));
    app.add_action(&action("preferences", || AppMsg::Preferences));
    app.add_action(&action("shortcuts", || AppMsg::Shortcuts));
    for (name, message) in [
        ("open-logs", AppMsg::OpenContainer as fn(String) -> AppMsg),
        ("restart-container", AppMsg::RestartContainer),
    ] {
        let action = gio::SimpleAction::new(name, Some(glib::VariantTy::STRING));
        let input = sender.input_sender().clone();
        action.connect_activate(move |_, id| {
            if let Some(id) = id.and_then(glib::Variant::get::<String>) {
                input.emit(message(id));
            }
        });
        app.add_action(&action);
    }
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate({
        let window = window.downgrade();
        move |_, _| {
            if let Some(window) = window.upgrade() {
                about_dialog().present(Some(&window));
            }
        }
    });
    app.add_action(&about);

    let troubleshoot = gio::SimpleAction::new(TROUBLESHOOT_ACTION.trim_start_matches("win."), None);
    let input = sender.input_sender().clone();
    troubleshoot.connect_activate(move |_, _| input.emit(AppMsg::Select(Section::Troubleshoot)));
    window.add_action(&troubleshoot);
}

fn about_dialog() -> adw::AboutDialog {
    adw::AboutDialog::builder()
        .application_name(product::DISPLAY_NAME)
        .application_icon(product::APP_ID)
        .version(product::VERSION)
        .license_type(gtk::License::Apache20)
        .website(env!("CARGO_PKG_REPOSITORY"))
        .issue_url(format!("{}/issues", env!("CARGO_PKG_REPOSITORY")))
        .comments("Docker is a trademark of Docker, Inc. Contr is not affiliated with or endorsed by Docker, Inc.")
        .build()
}

fn follow_status(daemon: &Daemon, sender: &ComponentSender<App>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    let current = status.borrow_and_update().clone();
                    out.emit(AppCmd::Status(Box::new(current)));
                    if status.changed().await.is_err() {
                        return;
                    }
                }
            })
            .drop_on_shutdown()
    });
}

/// Follows the daemon's operations, and the notices pages send about work that outlived them.
fn follow_activity(daemon: &Daemon, sender: &ComponentSender<App>) {
    let mut activity = daemon.subscribe_activity();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(AppCmd::Activity(Box::new(activity.borrow_and_update().clone())));
                    if activity.changed().await.is_err() {
                        return;
                    }
                }
            })
            .drop_on_shutdown()
    });

    let mut notices = daemon.subscribe_notices();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    match notices.recv().await {
                        Ok(message) => out.emit(AppCmd::Notice(message)),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            })
            .drop_on_shutdown()
    });
}

/// Reads the daemon's latest update check whenever a connection exists, then hourly; the daemon itself checks daily.
fn follow_updates(daemon: &Daemon, sender: &ComponentSender<App>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    let client = loop {
                        if let Some(client) = status.borrow_and_update().client().cloned() {
                            break client;
                        }
                        if status.changed().await.is_err() {
                            return;
                        }
                    };
                    let check = SettingsHandle::new(client.clone()).get().await.is_ok_and(|state| {
                        state
                            .settings
                            .and_then(|settings| settings.updates)
                            .is_some_and(|updates| updates.check)
                    });
                    let response = client.updates().get_update_status(GetUpdateStatusRequest {}).await;
                    if let Some(update) = response.ok().and_then(|response| response.into_inner().status) {
                        out.emit(AppCmd::Updates {
                            status: Box::new(update),
                            check,
                        });
                    }
                    tokio::time::sleep(UPDATE_POLL).await;
                }
            })
            .drop_on_shutdown()
    });
}

fn follow_containers(daemon: &Daemon, sender: &ComponentSender<App>) {
    let status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(while_running(status, move |client| {
                let out = out.clone();
                async move {
                    let stream = client.containers().watch_containers(WatchContainersRequest {}).await;
                    let Ok(stream) = stream else { return };
                    let mut stream = stream.into_inner();
                    while let Ok(Some(response)) = stream.message().await {
                        let event = match response.event {
                            Some(watch_containers_response::Event::Snapshot(snapshot)) => {
                                ContainerEvent::Snapshot(snapshot.containers)
                            }
                            Some(watch_containers_response::Event::Upserted(container)) => {
                                ContainerEvent::Upserted(Box::new(container))
                            }
                            Some(watch_containers_response::Event::RemovedId(id)) => ContainerEvent::Removed(id),
                            None => continue,
                        };
                        out.emit(AppCmd::Containers(event));
                    }
                }
            }))
            .drop_on_shutdown()
    });
}

fn start_tray(sender: &ComponentSender<App>) {
    let tray = ContrTray::new(sender.input_sender().clone(), tray::icon_pixmaps());
    sender.oneshot_command(async move {
        use ksni::TrayMethods as _;
        // A sandbox may not own the StatusNotifierItem-PID-ID name; the watcher then finds the tray by its unique name.
        match tray.disable_dbus_name(sandbox::flatpak()).spawn().await {
            Ok(handle) => AppCmd::Tray(Some(TrayHandle(handle))),
            Err(error) => {
                tracing::info!(%error, "no tray host on the session bus");
                AppCmd::Tray(None)
            }
        }
    });
}
