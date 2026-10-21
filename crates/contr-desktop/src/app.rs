use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use contr_core::product;
use contr_daemon_ui::{Daemon, EngineAction, Status, TROUBLESHOOT_ACTION, while_running};
use contr_sdk::ClientIdentity;
use contr_sdk::api::containers::v1::{Container, ContainerState, WatchContainersRequest, watch_containers_response};
use contr_sdk::api::daemon::v1::Capability;
use contr_sdk::api::engine::v1::EngineState;
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::client_settings::ClientSettings;
use crate::engine_card::{EngineCard, engine_name};
use crate::sections::{self, Section};
use crate::tray::{self, ContrTray, RunningContainer, TrayState};

pub(crate) struct App {
    daemon: Daemon,
    status: Status,
    containers: BTreeMap<String, Container>,
    section: Section,
    tray: Option<ksni::Handle<ContrTray>>,
    tray_state: TrayState,
}

#[derive(Debug)]
pub(crate) enum AppMsg {
    Select(Section),
    Present,
    Engine(EngineAction),
    Quit,
}

#[derive(Debug)]
pub(crate) enum AppCmd {
    Status(Box<Status>),
    Containers(ContainerEvent),
    Tray(Option<TrayHandle>),
    EngineFailed(String),
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
    toasts: adw::ToastOverlay,
}

struct SidebarRow {
    row: gtk::ListBoxRow,
    count: gtk::Label,
}

impl Component for App {
    type CommandOutput = AppCmd;
    type Input = AppMsg;
    type Output = ();
    type Init = ();
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

    fn init((): (), root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let daemon = Daemon::connect(ClientIdentity {
            name: format!("{}-desktop", product::SLUG),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        });
        let settings = ClientSettings::load();

        let (list, rows) = sidebar(&sender);
        let card = EngineCard::new();
        let engine_menu = gio::Menu::new();
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&primary_menu(&engine_menu))
            .primary(true)
            .tooltip_text("Main Menu")
            .build();
        let sidebar_header = adw::HeaderBar::new();
        sidebar_header.pack_end(&menu_button);
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
        for section in Section::ALL {
            stack.add_named(&sections::placeholder(section, &daemon), Some(section.name()));
        }
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&stack));

        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(220.0)
            .max_sidebar_width(260.0)
            .sidebar(&adw::NavigationPage::new(&sidebar_view, product::DISPLAY_NAME))
            .content(&adw::NavigationPage::new(&toasts, Section::Containers.title()))
            .build();
        root.set_content(Some(&split));

        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        root.add_breakpoint(narrow);

        install_actions(&root, &sender);
        let keep_running = Rc::new(Cell::new(settings.keep_running));
        root.connect_close_request(move |window| {
            if keep_running.get() {
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
        follow_containers(&daemon, &sender);
        start_tray(&sender);

        let model = App {
            status: daemon.status(),
            daemon,
            containers: BTreeMap::new(),
            section: Section::Containers,
            tray: None,
            tray_state: TrayState::default(),
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
            toasts,
        };
        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: AppMsg, sender: ComponentSender<Self>, root: &Self::Root) {
        match message {
            AppMsg::Select(section) => self.section = section,
            AppMsg::Present => root.present(),
            AppMsg::Engine(action) => {
                let daemon = self.daemon.clone();
                sender.oneshot_command(async move {
                    match daemon.run(action).await {
                        Ok(()) => AppCmd::Tray(None),
                        Err(error) => AppCmd::EngineFailed(contr_daemon_ui::describe(&error)),
                    }
                });
            }
            AppMsg::Quit => relm4::main_application().quit(),
        }
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: AppCmd,
        sender: ComponentSender<Self>,
        _root: &Self::Root,
    ) {
        match message {
            AppCmd::Status(status) => self.status = *status,
            AppCmd::Containers(ContainerEvent::Snapshot(containers)) => {
                self.containers = containers.into_iter().map(|c| (c.id.clone(), c)).collect();
            }
            AppCmd::Containers(ContainerEvent::Upserted(container)) => {
                self.containers.insert(container.id.clone(), *container);
            }
            AppCmd::Containers(ContainerEvent::Removed(id)) => {
                self.containers.remove(&id);
            }
            AppCmd::Tray(Some(TrayHandle(handle))) => self.tray = Some(handle),
            AppCmd::Tray(None) => {}
            AppCmd::EngineFailed(message) => {
                widgets.toasts.add_toast(adw::Toast::new(&message));
            }
        }
        if !self.status.is_running() {
            self.containers.clear();
        }
        self.update_tray();
        self.update_view(widgets, sender);
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        widgets.card.show(&self.status);

        if let Some(row) = widgets.rows.get(&Section::Containers) {
            let running = self.status.is_running();
            row.count.set_label(&self.containers.len().to_string());
            row.count.set_visible(running);
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

fn tray_state(status: &Status, containers: &BTreeMap<String, Container>) -> TrayState {
    let card = crate::engine_card::describe(status);
    let running: Vec<RunningContainer> = containers
        .values()
        .filter(|container| container.state() == ContainerState::Running)
        .map(|container| RunningContainer {
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
            .visible(false)
            .build();
        let content = gtk::Box::builder().spacing(12).build();
        content.append(&icon);
        content.append(&label);
        content.append(&count);
        let row = gtk::ListBoxRow::builder().child(&content).name(section.name()).build();
        list.append(&row);
        rows.insert(section, SidebarRow { row, count });
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
        .version(env!("CARGO_PKG_VERSION"))
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
        match tray.spawn().await {
            Ok(handle) => AppCmd::Tray(Some(TrayHandle(handle))),
            Err(error) => {
                tracing::info!(%error, "no tray host on the session bus");
                AppCmd::Tray(None)
            }
        }
    });
}
