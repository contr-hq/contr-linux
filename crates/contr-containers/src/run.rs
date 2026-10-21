//! The Run dialog: a container from an image with ports, folders, options and environment, created through
//! `CreateContainer` and pulled first when the image is not local.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use contr_daemon_ui::{Daemon, describe};
use contr_sdk::api::containers::v1::{
    CreateContainerRequest, MountKind, MountSpec, PortPublication, Protocol, RestartMode, RestartPolicy,
};
use contr_sdk::api::images::v1::{InspectImageRequest, PullImageRequest};
use contr_sdk::api::networks::v1::{CheckHostPortsRequest, HostPort};
use contr_sdk::{Client, Error};
use contr_ui_common::format;
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};

/// Address the dialog publishes ports on, so they answer on this computer only.
const LOOPBACK: &str = "127.0.0.1";
const FIRST_WEB_PORT: u32 = 8080;
const RESTART_MODES: [(&str, RestartMode, &str); 4] = [
    ("Never", RestartMode::No, "no"),
    ("Always", RestartMode::Always, "always"),
    ("Unless stopped", RestartMode::UnlessStopped, "unless-stopped"),
    ("On failure", RestartMode::OnFailure, "on-failure"),
];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PortSpec {
    pub(crate) host: u32,
    pub(crate) container: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderSpec {
    pub(crate) source: PathBuf,
    pub(crate) destination: String,
    pub(crate) read_only: bool,
}

/// Everything the dialog would pass to `CreateContainer`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RunSpec {
    pub(crate) image: String,
    pub(crate) name: String,
    pub(crate) ports: Vec<PortSpec>,
    pub(crate) folders: Vec<FolderSpec>,
    pub(crate) env: Vec<String>,
    pub(crate) auto_remove: bool,
    pub(crate) restart: usize,
}

impl RunSpec {
    fn request(&self) -> CreateContainerRequest {
        CreateContainerRequest {
            image: self.image.clone(),
            name: self.name.clone(),
            env: self.env.clone(),
            ports: self
                .ports
                .iter()
                .map(|port| PortPublication {
                    container_port: port.container,
                    protocol: Protocol::Tcp.into(),
                    host_port: port.host,
                    host_ip: LOOPBACK.to_owned(),
                })
                .collect(),
            mounts: self
                .folders
                .iter()
                .map(|folder| MountSpec {
                    kind: MountKind::Bind.into(),
                    source: folder.source.to_string_lossy().into_owned(),
                    destination: folder.destination.clone(),
                    read_only: folder.read_only,
                })
                .collect(),
            restart_policy: Some(RestartPolicy {
                mode: RESTART_MODES
                    .get(self.restart)
                    .map_or(RestartMode::No, |mode| mode.1)
                    .into(),
                max_retries: 0,
            }),
            auto_remove: self.auto_remove,
            start: true,
            ..CreateContainerRequest::default()
        }
    }

    /// The `docker run` command that creates the same container, with the home directory written as `~`.
    pub(crate) fn command(&self, home: &Path) -> String {
        let mut parts = vec!["docker run -d".to_owned()];
        if self.auto_remove {
            parts.push("--rm".to_owned());
        }
        if !self.name.is_empty() {
            parts.push(format!("--name {}", self.name));
        }
        for port in &self.ports {
            parts.push(format!("-p {LOOPBACK}:{}:{}", port.host, port.container));
        }
        for folder in &self.folders {
            let source = format::home_relative(&folder.source.to_string_lossy(), home);
            let access = if folder.read_only { ":ro" } else { "" };
            parts.push(format!("-v {source}:{}{access}", folder.destination));
        }
        for variable in &self.env {
            parts.push(format!("-e {variable}"));
        }
        if let Some((_, _, flag)) = RESTART_MODES.get(self.restart).filter(|_| self.restart > 0) {
            parts.push(format!("--restart {flag}"));
        }
        parts.push(if self.image.is_empty() {
            "IMAGE".to_owned()
        } else {
            self.image.clone()
        });
        parts.join(" ")
    }
}

/// The first host port from `start` that neither a container nor another program holds.
pub(crate) fn free_port(start: u32, taken: &HashMap<u32, String>, in_use: &[u32]) -> u32 {
    (start..start.saturating_add(100))
        .find(|port| !taken.contains_key(port) && !in_use.contains(port))
        .unwrap_or(start)
}

/// `80/tcp` as 80; ports of other protocols are not offered.
fn tcp_port(exposed: &str) -> Option<u32> {
    let (port, protocol) = exposed.split_once('/').unwrap_or((exposed, "tcp"));
    (protocol == "tcp").then(|| port.parse().ok()).flatten()
}

struct PortRow {
    row: gtk::ListBoxRow,
    host: gtk::Entry,
    container: gtk::Entry,
    problem: gtk::Label,
}

struct FolderRow {
    row: adw::ActionRow,
    spec: FolderSpec,
    read_only: gtk::Switch,
}

struct State {
    daemon: Daemon,
    home: PathBuf,
    taken: HashMap<u32, String>,
    dialog: adw::Dialog,
    image: adw::EntryRow,
    pulled: gtk::Label,
    name: adw::EntryRow,
    exposed: gtk::Label,
    ports: gtk::ListBox,
    port_rows: RefCell<Vec<PortRow>>,
    ports_edited: RefCell<bool>,
    folders: gtk::ListBox,
    folder_rows: RefCell<Vec<FolderRow>>,
    auto_remove: adw::SwitchRow,
    restart: adw::ComboRow,
    env: RefCell<Vec<adw::EntryRow>>,
    env_list: gtk::ListBox,
    env_count: gtk::Label,
    command: gtk::Label,
    run: gtk::Button,
    error: gtk::Label,
    local: RefCell<Option<bool>>,
    on_started: Box<dyn Fn(String)>,
}

/// Shows the dialog over `parent`; `taken` maps host ports that containers publish to their names, and `on_started`
/// receives the new container's ID.
pub(crate) fn present(
    parent: &impl IsA<gtk::Widget>,
    daemon: &Daemon,
    image: &str,
    taken: HashMap<u32, String>,
    on_started: impl Fn(String) + 'static,
) {
    let state = build(daemon, image, taken, Box::new(on_started));
    // Widget callbacks hold the state weakly; this handler owns it until the dialog closes.
    let owner = RefCell::new(Some(Rc::clone(&state)));
    state.dialog.connect_closed(move |_| {
        owner.borrow_mut().take();
    });
    state.dialog.present(Some(parent));
    if image.is_empty() {
        state.image.grab_focus();
    } else {
        state.inspect();
    }
    state.update_command();
}

fn build(daemon: &Daemon, image: &str, taken: HashMap<u32, String>, on_started: Box<dyn Fn(String)>) -> Rc<State> {
    let cancel = gtk::Button::builder().label("Cancel").build();
    let run = gtk::Button::builder()
        .label("Run")
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&run);

    let error = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .css_classes(["status-failed"])
        .build();

    let image_row = adw::EntryRow::builder().title("Image").text(image).build();
    let image_state = gtk::Label::builder().css_classes(["group-description"]).build();
    image_row.add_suffix(&image_state);
    let name = adw::EntryRow::builder().title("Name (optional)").build();
    let identity = adw::PreferencesGroup::new();
    identity.add(&image_row);
    identity.add(&name);

    let exposed = gtk::Label::builder()
        .css_classes(["dim-label", "group-description"])
        .build();
    let ports = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add_port = adw::ButtonRow::builder()
        .title("Add Port")
        .start_icon_name("list-add-symbolic")
        .build();
    ports.append(&add_port);
    let ports_group = adw::PreferencesGroup::builder()
        .title("Ports")
        .header_suffix(&exposed)
        .build();
    ports_group.add(&ports);

    let folders = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add_folder = adw::ButtonRow::builder()
        .title("Add Folder…")
        .start_icon_name("list-add-symbolic")
        .build();
    folders.append(&add_folder);
    let folders_group = adw::PreferencesGroup::builder().title("Folders").build();
    folders_group.add(&folders);

    let auto_remove = adw::SwitchRow::builder()
        .title("Delete When It Stops")
        .subtitle("Good for quick previews and one-off commands")
        .build();
    let restart = adw::ComboRow::builder()
        .title("Restart Policy")
        .model(&gtk::StringList::new(&RESTART_MODES.map(|mode| mode.0)))
        .build();
    let env_count = gtk::Label::builder().label("None").css_classes(["dim-label"]).build();
    let env_row = adw::ActionRow::builder()
        .title("Environment Variables")
        .activatable(true)
        .build();
    env_row.add_suffix(&env_count);
    env_row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let options = adw::PreferencesGroup::builder().title("Options").build();
    options.add(&auto_remove);
    options.add(&restart);
    options.add(&env_row);

    let command = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .xalign(0.0)
        .selectable(true)
        .css_classes(["dim-label", "group-description"])
        .build();
    let copy = gtk::Button::builder()
        .label("Copy")
        .css_classes(["flat"])
        .valign(gtk::Align::Start)
        .build();
    let command_line = gtk::Box::builder().spacing(6).margin_start(6).margin_end(6).build();
    command_line.append(&command);
    command_line.append(&copy);
    command.set_hexpand(true);

    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(20)
        .margin_top(6)
        .margin_bottom(22)
        .margin_start(18)
        .margin_end(18)
        .build();
    for widget in [
        error.upcast_ref::<gtk::Widget>(),
        identity.upcast_ref(),
        ports_group.upcast_ref(),
        folders_group.upcast_ref(),
        options.upcast_ref(),
        command_line.upcast_ref(),
    ] {
        column.append(widget);
    }
    let main = adw::ToolbarView::new();
    main.add_top_bar(&header);
    main.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .child(&column)
            .build(),
    ));

    let env_list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let add_env = adw::ButtonRow::builder()
        .title("Add Variable")
        .start_icon_name("list-add-symbolic")
        .build();
    env_list.append(&add_env);
    let env_group = adw::PreferencesGroup::builder()
        .description("Each entry is NAME=value and is added to the image’s environment.")
        .build();
    env_group.add(&env_list);
    let env_page_content = adw::Clamp::builder()
        .child(&env_group)
        .margin_top(12)
        .margin_bottom(22)
        .margin_start(18)
        .margin_end(18)
        .build();
    let env_toolbar = adw::ToolbarView::new();
    env_toolbar.add_top_bar(&adw::HeaderBar::builder().show_end_title_buttons(false).build());
    env_toolbar.set_content(Some(&env_page_content));
    let env_page = adw::NavigationPage::new(&env_toolbar, "Environment Variables");

    let navigation = adw::NavigationView::new();
    navigation.add(&adw::NavigationPage::new(&main, "Run a Container"));
    let dialog = adw::Dialog::builder()
        .title("Run a Container")
        .content_width(560)
        .content_height(760)
        .child(&navigation)
        .build();

    let state = Rc::new(State {
        daemon: daemon.clone(),
        home: glib::home_dir(),
        taken,
        dialog: dialog.clone(),
        image: image_row.clone(),
        pulled: image_state,
        name: name.clone(),
        exposed,
        ports: ports.clone(),
        port_rows: RefCell::default(),
        ports_edited: RefCell::new(false),
        folders: folders.clone(),
        folder_rows: RefCell::default(),
        auto_remove: auto_remove.clone(),
        restart: restart.clone(),
        env: RefCell::default(),
        env_list: env_list.clone(),
        env_count,
        command,
        run: run.clone(),
        error,
        local: RefCell::new(None),
        on_started,
    });

    cancel.connect_clicked({
        let dialog = dialog.clone();
        move |_| {
            dialog.close();
        }
    });
    let weak = Rc::downgrade(&state);
    let with = move |action: fn(&Rc<State>)| {
        let weak = weak.clone();
        move || {
            if let Some(state) = weak.upgrade() {
                action(&state);
            }
        }
    };
    run.connect_clicked({
        let call = with(State::run);
        move |_| call()
    });
    add_port.connect_activated({
        let call = with(|state| {
            *state.ports_edited.borrow_mut() = true;
            state.add_port(0, 0);
        });
        move |_| call()
    });
    add_folder.connect_activated({
        let call = with(State::choose_folder);
        move |_| call()
    });
    add_env.connect_activated({
        let call = with(|state| state.add_env(""));
        move |_| call()
    });
    env_row.connect_activated(move |_| navigation.push(&env_page));
    copy.connect_clicked({
        let call = with(|state| state.dialog.clipboard().set_text(&state.command.label()));
        move |_| call()
    });

    let debounce: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
    image_row.connect_changed({
        let call = with(State::inspect);
        let update = with(State::update_command);
        move |_| {
            update();
            if let Some(source) = debounce.borrow_mut().take() {
                source.remove();
            }
            let call = call.clone();
            let debounce_inner = Rc::clone(&debounce);
            *debounce.borrow_mut() = Some(glib::timeout_add_local_once(
                std::time::Duration::from_millis(400),
                move || {
                    debounce_inner.borrow_mut().take();
                    call();
                },
            ));
        }
    });
    let update = with(State::update_command);
    name.connect_changed(move |_| update());
    let update = with(State::update_command);
    auto_remove.connect_active_notify(move |_| update());
    let update = with(State::update_command);
    restart.connect_selected_notify(move |_| update());
    state
}

impl State {
    fn client(&self) -> Option<Client> {
        self.daemon.status().running_client().cloned()
    }

    fn spec(&self) -> RunSpec {
        RunSpec {
            image: self.image.text().trim().to_owned(),
            name: self.name.text().trim().to_owned(),
            ports: self
                .port_rows
                .borrow()
                .iter()
                .filter_map(|row| {
                    let host = row.host.text().trim().parse().ok()?;
                    let container = row.container.text().trim().parse().ok()?;
                    Some(PortSpec { host, container })
                })
                .collect(),
            folders: self
                .folder_rows
                .borrow()
                .iter()
                .map(|row| FolderSpec {
                    read_only: row.read_only.is_active(),
                    ..row.spec.clone()
                })
                .collect(),
            env: self
                .env
                .borrow()
                .iter()
                .map(|row| row.text().trim().to_owned())
                .filter(|entry| entry.contains('='))
                .collect(),
            auto_remove: self.auto_remove.is_active(),
            restart: usize::try_from(self.restart.selected()).unwrap_or(0),
        }
    }

    fn update_command(self: &Rc<Self>) {
        let spec = self.spec();
        self.command.set_markup(&format!(
            "Same as <span font_family=\"monospace\">{}</span>",
            glib::markup_escape_text(&spec.command(&self.home))
        ));
        let count = spec.env.len();
        self.env_count.set_label(&match count {
            0 => "None".to_owned(),
            count => count.to_string(),
        });
        let has_problem = self.port_rows.borrow().iter().any(|row| row.problem.is_visible());
        self.run.set_sensitive(!spec.image.is_empty() && !has_problem);
        self.run.set_label(match *self.local.borrow() {
            Some(false) => "Pull and Run",
            _ => "Run",
        });
    }

    /// Checks whether the image is local and offers the ports it exposes.
    fn inspect(self: &Rc<Self>) {
        let reference = self.image.text().trim().to_owned();
        let Some(client) = self.client() else { return };
        if reference.is_empty() {
            return;
        }
        let state = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let probe = client.clone();
            let request = InspectImageRequest {
                reference: reference.clone(),
            };
            let inspected = relm4::spawn(async move { probe.images().inspect_image(request).await }).await;
            let Some(state) = state.upgrade() else { return };
            if state.image.text().trim() != reference {
                return;
            }
            let exposed = match inspected {
                Ok(Ok(response)) => {
                    state.set_local(Some(true));
                    response
                        .into_inner()
                        .details
                        .map(|details| details.exposed_ports)
                        .unwrap_or_default()
                }
                Ok(Err(status)) if status.code() == tonic::Code::NotFound => {
                    state.set_local(Some(false));
                    Vec::new()
                }
                _ => {
                    state.set_local(None);
                    Vec::new()
                }
            };
            state.exposed.set_label(&if exposed.is_empty() {
                String::new()
            } else {
                format!("The image exposes {}", exposed.join(", "))
            });
            if *state.ports_edited.borrow() {
                return;
            }
            state.clear_ports();
            let wanted: Vec<u32> = exposed.iter().filter_map(|port| tcp_port(port)).collect();
            let in_use = in_use(
                &client,
                (FIRST_WEB_PORT..FIRST_WEB_PORT + 20)
                    .chain(wanted.iter().copied())
                    .collect(),
            )
            .await;
            let mut chosen = Vec::new();
            for container in wanted {
                let start = if container >= 1024 { container } else { FIRST_WEB_PORT };
                let mut taken = state.taken.clone();
                for port in &chosen {
                    taken.insert(*port, String::new());
                }
                let host = free_port(start, &taken, &in_use);
                chosen.push(host);
                state.add_port(host, container);
            }
        });
    }

    fn set_local(self: &Rc<Self>, local: Option<bool>) {
        *self.local.borrow_mut() = local;
        for class in ["status-running", "dim-label"] {
            self.pulled.remove_css_class(class);
        }
        match local {
            Some(true) => {
                self.pulled.set_label("✓ Pulled");
                self.pulled.add_css_class("status-running");
            }
            Some(false) => {
                self.pulled.set_label("Not pulled yet");
                self.pulled.add_css_class("dim-label");
            }
            None => self.pulled.set_label(""),
        }
        self.update_command();
    }

    fn clear_ports(self: &Rc<Self>) {
        for row in self.port_rows.borrow_mut().drain(..) {
            self.ports.remove(&row.row);
        }
        self.update_command();
    }

    fn add_port(self: &Rc<Self>, host: u32, container: u32) {
        let text = |port: u32| if port == 0 { String::new() } else { port.to_string() };
        let entry = |value: String, label: &str| {
            gtk::Entry::builder()
                .text(value)
                .width_chars(6)
                .max_width_chars(6)
                .input_purpose(gtk::InputPurpose::Digits)
                .tooltip_text(label)
                .valign(gtk::Align::Center)
                .build()
        };
        let host_entry = entry(text(host), "Port on this computer");
        let container_entry = entry(text(container), "Port in the container");
        let problem = gtk::Label::builder()
            .css_classes(["status-failed", "group-description"])
            .visible(false)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("contr-delete-symbolic")
            .tooltip_text("Remove Port")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        let content = gtk::Box::builder().spacing(8).css_classes(["container-row"]).build();
        content.append(
            &gtk::Label::builder()
                .label("localhost:")
                .css_classes(["dim-label"])
                .build(),
        );
        content.append(&host_entry);
        content.append(&gtk::Label::new(Some("→")));
        content.append(&container_entry);
        content.append(&gtk::Label::new(Some("/tcp")));
        let spacer = gtk::Box::builder().hexpand(true).build();
        content.append(&spacer);
        content.append(&problem);
        content.append(&remove);
        let row = gtk::ListBoxRow::builder().child(&content).activatable(false).build();
        let position = i32::try_from(self.port_rows.borrow().len()).unwrap_or(0);
        self.ports.insert(&row, position);

        let weak = Rc::downgrade(self);
        let check_row = row.clone();
        let check = move || {
            if let Some(state) = weak.upgrade() {
                *state.ports_edited.borrow_mut() = true;
                state.check_port(&check_row);
            }
        };
        host_entry.connect_changed({
            let check = check.clone();
            move |_| check()
        });
        container_entry.connect_changed(move |_| check());
        let weak = Rc::downgrade(self);
        let remove_row = row.clone();
        remove.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                *state.ports_edited.borrow_mut() = true;
                state.port_rows.borrow_mut().retain(|port| port.row != remove_row);
                state.ports.remove(&remove_row);
                state.update_command();
            }
        });
        self.port_rows.borrow_mut().push(PortRow {
            row: row.clone(),
            host: host_entry,
            container: container_entry,
            problem,
        });
        self.check_port(&row);
    }

    /// Flags a host port another container publishes or another program holds.
    fn check_port(self: &Rc<Self>, row: &gtk::ListBoxRow) {
        let host = {
            let rows = self.port_rows.borrow();
            let Some(port) = rows.iter().find(|port| &port.row == row) else {
                return;
            };
            let host: Option<u32> = port.host.text().trim().parse().ok();
            if let Some(name) = host.and_then(|host| self.taken.get(&host)) {
                show_problem(port, Some(&format!("Used by {name}")));
                None
            } else {
                show_problem(port, None);
                host
            }
        };
        self.update_command();
        let (Some(host), Some(client)) = (host, self.client()) else {
            return;
        };
        let weak = Rc::downgrade(self);
        let row = row.clone();
        glib::spawn_future_local(async move {
            let busy = in_use(&client, vec![host]).await;
            let Some(state) = weak.upgrade() else { return };
            let rows = state.port_rows.borrow();
            let Some(port) = rows.iter().find(|port| port.row == row) else {
                return;
            };
            if port.host.text().trim().parse::<u32>().ok() == Some(host) && busy.contains(&host) {
                show_problem(port, Some("In use on this computer"));
            }
            drop(rows);
            state.update_command();
        });
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder().title("Choose a Folder to Share").build();
        let window = self.dialog.root().and_downcast::<gtk::Window>();
        let weak = Rc::downgrade(self);
        dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
            let (Ok(folder), Some(state)) = (result, weak.upgrade()) else {
                return;
            };
            let Some(source) = folder.path() else { return };
            state.ask_destination(source);
        });
    }

    fn ask_destination(self: &Rc<Self>, source: PathBuf) {
        let suggested = format!(
            "/{}",
            source
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let alert = adw::AlertDialog::new(
            Some("Path in the Container"),
            Some(&format!(
                "Where {} appears inside the container.",
                format::home_relative(&source.to_string_lossy(), &self.home)
            )),
        );
        let entry = gtk::Entry::builder().text(suggested).activates_default(true).build();
        alert.set_extra_child(Some(&entry));
        alert.add_responses(&[("cancel", "Cancel"), ("add", "Add")]);
        alert.set_response_appearance("add", adw::ResponseAppearance::Suggested);
        alert.set_default_response(Some("add"));
        alert.set_close_response("cancel");
        let weak = Rc::downgrade(self);
        let focus = entry.clone();
        alert.connect_response(Some("add"), move |_, _| {
            let destination = entry.text().trim().to_owned();
            if let (Some(state), true) = (weak.upgrade(), destination.starts_with('/')) {
                state.add_folder(FolderSpec {
                    source: source.clone(),
                    destination,
                    read_only: false,
                });
            }
        });
        alert.present(Some(&self.dialog));
        alert.set_focus(Some(&focus));
    }

    fn add_folder(self: &Rc<Self>, spec: FolderSpec) {
        let read_only = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(spec.read_only)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("contr-delete-symbolic")
            .tooltip_text("Remove Folder")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&format::home_relative(
                &spec.source.to_string_lossy(),
                &self.home,
            )))
            .subtitle(glib::markup_escape_text(&format!("→ {}", spec.destination)))
            .build();
        row.add_prefix(&gtk::Image::from_icon_name("folder-symbolic"));
        row.add_suffix(&gtk::Label::new(Some("Read-only")));
        row.add_suffix(&read_only);
        row.add_suffix(&remove);
        let position = i32::try_from(self.folder_rows.borrow().len()).unwrap_or(0);
        self.folders.insert(&row, position);
        let update = Rc::downgrade(self);
        read_only.connect_active_notify(move |_| {
            if let Some(state) = update.upgrade() {
                state.update_command();
            }
        });
        let weak = Rc::downgrade(self);
        let remove_row = row.clone();
        remove.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.folder_rows.borrow_mut().retain(|folder| folder.row != remove_row);
                state.folders.remove(&remove_row);
                state.update_command();
            }
        });
        self.folder_rows.borrow_mut().push(FolderRow { row, spec, read_only });
        self.update_command();
    }

    fn add_env(self: &Rc<Self>, value: &str) {
        let row = adw::EntryRow::builder().title("NAME=value").text(value).build();
        let remove = gtk::Button::builder()
            .icon_name("contr-delete-symbolic")
            .tooltip_text("Remove Variable")
            .valign(gtk::Align::Center)
            .css_classes(["flat", "circular"])
            .build();
        row.add_suffix(&remove);
        let position = i32::try_from(self.env.borrow().len()).unwrap_or(0);
        self.env_list.insert(&row, position);
        let update = Rc::downgrade(self);
        row.connect_changed(move |_| {
            if let Some(state) = update.upgrade() {
                state.update_command();
            }
        });
        let weak = Rc::downgrade(self);
        let remove_row = row.clone();
        remove.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                state.env.borrow_mut().retain(|entry| entry != &remove_row);
                state.env_list.remove(&remove_row);
                state.update_command();
            }
        });
        self.env.borrow_mut().push(row.clone());
        row.grab_focus();
        self.update_command();
    }

    fn run(self: &Rc<Self>) {
        let Some(client) = self.client() else {
            self.show_error("The engine isn’t running.");
            return;
        };
        let spec = self.spec();
        let pull_first = *self.local.borrow() == Some(false);
        self.run.set_sensitive(false);
        self.run.set_label(if pull_first { "Pulling…" } else { "Starting…" });
        self.error.set_visible(false);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = relm4::spawn(async move {
                if pull_first {
                    pull(&client, &spec.image).await?;
                }
                let response = client.containers().create_container(spec.request()).await?;
                Ok::<_, Error>(response.into_inner().id)
            })
            .await;
            let Some(state) = weak.upgrade() else { return };
            match result {
                Ok(Ok(id)) => {
                    state.dialog.close();
                    (state.on_started)(id);
                }
                Ok(Err(error)) => state.show_error(&describe(&error)),
                Err(_) => state.show_error("The request was interrupted."),
            }
        });
    }

    fn show_error(self: &Rc<Self>, message: &str) {
        self.error.set_label(message);
        self.error.set_visible(true);
        self.update_command();
    }
}

fn show_problem(port: &PortRow, problem: Option<&str>) {
    port.problem.set_label(problem.unwrap_or_default());
    port.problem.set_visible(problem.is_some());
    if problem.is_some() {
        port.host.add_css_class("error");
    } else {
        port.host.remove_css_class("error");
    }
}

/// Host ports among `ports` that another program on this computer holds; empty when the check fails.
async fn in_use(client: &Client, ports: Vec<u32>) -> Vec<u32> {
    let client = client.clone();
    let request = CheckHostPortsRequest {
        ports: ports
            .into_iter()
            .map(|port| HostPort {
                port,
                protocol: Protocol::Tcp.into(),
            })
            .collect(),
    };
    let result = relm4::spawn(async move { client.networks().check_host_ports(request).await }).await;
    match result {
        Ok(Ok(response)) => response
            .into_inner()
            .ports
            .into_iter()
            .filter(|status| status.in_use)
            .filter_map(|status| status.port.map(|port| port.port))
            .collect(),
        _ => Vec::new(),
    }
}

async fn pull(client: &Client, reference: &str) -> Result<(), Error> {
    let request = PullImageRequest {
        reference: reference.to_owned(),
        platform: String::new(),
    };
    let mut stream = client.images().pull_image(request).await?.into_inner();
    while stream.message().await?.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_equivalent_docker_run_command() {
        let spec = RunSpec {
            image: "nginx:1.27-alpine".to_owned(),
            name: "site-preview".to_owned(),
            ports: vec![PortSpec {
                host: 8082,
                container: 80,
            }],
            folders: vec![FolderSpec {
                source: PathBuf::from("/home/minh/src/site"),
                destination: "/usr/share/nginx/html".to_owned(),
                read_only: true,
            }],
            env: vec!["MODE=dev".to_owned()],
            auto_remove: true,
            restart: 0,
        };
        assert_eq!(
            spec.command(Path::new("/home/minh")),
            "docker run -d --rm --name site-preview -p 127.0.0.1:8082:80 \
             -v ~/src/site:/usr/share/nginx/html:ro -e MODE=dev nginx:1.27-alpine"
        );
        let request = spec.request();
        assert_eq!(request.ports[0].host_ip, LOOPBACK);
        assert!(request.start && request.auto_remove);
    }

    #[test]
    fn picks_the_first_free_port() {
        let taken: HashMap<u32, String> = [(8080, "api".to_owned()), (8081, "web".to_owned())]
            .into_iter()
            .collect();
        assert_eq!(free_port(8080, &taken, &[]), 8082);
        assert_eq!(free_port(8080, &taken, &[8082]), 8083);
        assert_eq!(tcp_port("80/tcp"), Some(80));
        assert_eq!(tcp_port("53/udp"), None);
    }
}
