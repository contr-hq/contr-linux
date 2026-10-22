//! The Inspect view: the engine's description of the container, summarised, and its raw JSON.

use std::rc::Rc;
use std::time::SystemTime;

use contr_daemon_ui::{describe, system_time};
use contr_sdk::Client;
use contr_sdk::api::containers::v1::{
    ContainerDetails, HealthStatus, InspectContainerRequest, InspectContainerResponse, MountKind, Protocol, RestartMode,
};
use contr_ui_common::{format, palette};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};

pub(crate) struct InspectView {
    root: gtk::ScrolledWindow,
    groups: gtk::Box,
    raw: gtk::TextBuffer,
    raw_view: gtk::TextView,
    error: adw::StatusPage,
    stack: gtk::Stack,
}

impl InspectView {
    pub(crate) fn new() -> Self {
        let groups = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        let raw = gtk::TextBuffer::new(None);
        raw.create_tag(
            Some("found"),
            &[
                ("background", &palette::MATCH_BACKGROUND),
                ("foreground", &palette::MATCH_FOREGROUND),
            ],
        );
        let raw_view = gtk::TextView::builder()
            .buffer(&raw)
            .editable(false)
            .monospace(true)
            .css_classes(["console-view"])
            .build();
        let find = gtk::SearchEntry::builder()
            .placeholder_text("Find a key")
            .hexpand(true)
            .build();
        let copy = gtk::Button::builder().label("Copy").css_classes(["flat"]).build();
        let toolbar = gtk::Box::builder().spacing(8).css_classes(["console-toolbar"]).build();
        toolbar.append(&gtk::Label::builder().label("Raw JSON").css_classes(["heading"]).build());
        toolbar.append(&find);
        toolbar.append(&copy);
        let console = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["console"])
            .build();
        console.append(&toolbar);
        console.append(&raw_view);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_bottom(12)
            .build();
        column.append(&groups);
        column.append(&console);
        let clamp = adw::Clamp::builder().maximum_size(860).child(&column).build();

        let error = adw::StatusPage::builder()
            .icon_name("contr-inspect-symbolic")
            .title("Can’t Inspect the Container")
            .build();
        let loading = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&clamp, Some("content"));
        stack.add_named(&error, Some("error"));
        stack.add_named(&loading, Some("loading"));
        stack.set_visible_child_name("loading");
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&stack)
            .vexpand(true)
            .build();

        copy.connect_clicked({
            let raw = raw.clone();
            move |button| {
                button
                    .clipboard()
                    .set_text(&raw.text(&raw.start_iter(), &raw.end_iter(), false));
            }
        });
        find.connect_search_changed({
            let raw = raw.clone();
            let view = raw_view.clone();
            move |entry| find_in(&raw, &view, &entry.text(), false)
        });
        find.connect_activate({
            let raw = raw.clone();
            let view = raw_view.clone();
            move |entry| find_in(&raw, &view, &entry.text(), true)
        });

        Self {
            root,
            groups,
            raw,
            raw_view,
            error,
            stack,
        }
    }

    pub(crate) fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }

    pub(crate) fn load(&self, client: Option<Client>, id: &str) {
        let Some(client) = client else { return };
        let request = InspectContainerRequest { id: id.to_owned() };
        let groups = self.groups.clone();
        let raw = self.raw.clone();
        let error = self.error.clone();
        let stack = self.stack.clone();
        let raw_view = self.raw_view.clone();
        glib::spawn_future_local(async move {
            let result = relm4::spawn(async move { client.containers().inspect_container(request).await }).await;
            match result {
                Ok(Ok(response)) => {
                    show(&groups, &raw, &response.into_inner());
                    raw_view.queue_resize();
                    stack.set_visible_child_name("content");
                }
                Ok(Err(status)) => {
                    error.set_description(Some(&describe(&contr_sdk::Error::from(status))));
                    stack.set_visible_child_name("error");
                }
                Err(_) => {}
            }
        });
    }
}

fn show(groups: &gtk::Box, raw: &gtk::TextBuffer, response: &InspectContainerResponse) {
    while let Some(child) = groups.first_child() {
        groups.remove(&child);
    }
    let json: serde_json::Value = serde_json::from_str(&response.raw_json).unwrap_or_default();
    raw.set_text(&serde_json::to_string_pretty(&json).unwrap_or_else(|_| response.raw_json.clone()));

    let container = response.container.clone().unwrap_or_default();
    let details = response.details.clone().unwrap_or_default();
    let host = &json["HostConfig"];

    let mut command = details.entrypoint.clone();
    command.extend(details.command.iter().cloned());
    groups.append(&group(
        "Configuration",
        &[
            ("Image", container.image.clone()),
            ("Command", or_none(&command.join(" "), "Image default")),
            ("Working folder", or_none(&details.working_dir, "Image default")),
            ("User", or_none(&details.user, "Image default")),
            ("Restart policy", restart_policy(&details)),
            ("Hostname", details.hostname.clone()),
        ],
    ));

    let mut state = vec![("Status", container.status.clone())];
    if let Some(started) = system_time(details.started_at.as_ref()) {
        state.push(("Started", when(started)));
    }
    if let Some(finished) = system_time(details.finished_at.as_ref()).filter(|_| container.state != 2) {
        state.push(("Exited", format!("{} · code {}", when(finished), details.exit_code)));
    }
    state.push(("Restarts", details.restart_count.to_string()));
    state.push(("Health check", health(details.health())));
    groups.append(&group("State", &state));

    let cpus = host["NanoCpus"].as_i64().unwrap_or_default();
    let memory = host["Memory"].as_u64().unwrap_or_default();
    #[allow(clippy::cast_precision_loss)]
    let cpu_limit = if cpus == 0 {
        "No limit".to_owned()
    } else {
        format!("{} CPUs", cpus as f64 / 1e9)
    };
    let memory_limit = if memory == 0 {
        "No limit".to_owned()
    } else {
        format::bytes(memory)
    };
    groups.append(&group("Limits", &[("CPU", cpu_limit), ("Memory", memory_limit)]));

    if !container.ports.is_empty() {
        let ports: Vec<(String, String)> = container
            .ports
            .iter()
            .map(|port| {
                let protocol = match port.protocol() {
                    Protocol::Udp => "udp",
                    Protocol::Sctp => "sctp",
                    _ => "tcp",
                };
                let published = if port.host_port == 0 {
                    "Not published".to_owned()
                } else {
                    format!("{}:{}", or_none(&port.host_ip, "0.0.0.0"), port.host_port)
                };
                (format!("{}/{protocol}", port.container_port), published)
            })
            .collect();
        groups.append(&group_owned("Ports", &ports));
    }

    if !details.mounts.is_empty() {
        let mounts: Vec<(String, String)> = details
            .mounts
            .iter()
            .map(|mount| {
                let kind = match mount.kind() {
                    MountKind::Bind => "Folder",
                    MountKind::Volume => "Volume",
                    MountKind::Tmpfs => "Memory",
                    _ => "Mount",
                };
                let source = if mount.name.is_empty() {
                    &mount.source
                } else {
                    &mount.name
                };
                let access = if mount.read_only { " · read-only" } else { "" };
                (mount.destination.clone(), format!("{kind} {source}{access}"))
            })
            .collect();
        groups.append(&group_owned("Mounts", &mounts));
    }

    if !details.networks.is_empty() {
        let networks: Vec<(String, String)> = details
            .networks
            .iter()
            .map(|network| (network.network.clone(), or_none(&network.ipv4_address, "No address")))
            .collect();
        groups.append(&group_owned("Networks", &networks));
    }

    if !details.env.is_empty() {
        groups.append(&environment(&details.env));
    }

    if !container.labels.is_empty() {
        let mut labels: Vec<(String, String)> = container.labels.clone().into_iter().collect();
        labels.sort();
        groups.append(&group_owned("Labels", &labels));
    }
}

fn group(title: &str, rows: &[(&str, String)]) -> adw::PreferencesGroup {
    let rows: Vec<(String, String)> = rows
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.clone()))
        .collect();
    group_owned(title, &rows)
}

fn group_owned(title: &str, rows: &[(String, String)]) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title(title).build();
    for (key, value) in rows {
        group.add(&property(key, value));
    }
    group
}

fn property(key: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(glib::markup_escape_text(key))
        .subtitle(glib::markup_escape_text(value))
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build()
}

/// Environment rows hide their values until asked, since images often pass secrets this way.
fn environment(env: &[String]) -> adw::PreferencesGroup {
    let show = gtk::ToggleButton::builder()
        .label("Show Values")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Environment")
        .header_suffix(&show)
        .build();
    let rows: Rc<Vec<(adw::ActionRow, String)>> = Rc::new(
        env.iter()
            .map(|entry| {
                let (key, value) = entry.split_once('=').unwrap_or((entry, ""));
                let row = property(key, "••••••");
                group.add(&row);
                (row, value.to_owned())
            })
            .collect(),
    );
    show.connect_toggled(move |show| {
        for (row, value) in rows.iter() {
            let shown = if show.is_active() {
                glib::markup_escape_text(value)
            } else {
                "••••••".into()
            };
            row.set_subtitle(&shown);
        }
    });
    group
}

fn or_none(value: &str, none: &str) -> String {
    if value.trim().is_empty() {
        none.to_owned()
    } else {
        value.to_owned()
    }
}

fn restart_policy(details: &ContainerDetails) -> String {
    let Some(policy) = &details.restart_policy else {
        return "Never".to_owned();
    };
    match policy.mode() {
        RestartMode::Always => "Always".to_owned(),
        RestartMode::UnlessStopped => "Unless stopped".to_owned(),
        RestartMode::OnFailure if policy.max_retries > 0 => format!("On failure, at most {} times", policy.max_retries),
        RestartMode::OnFailure => "On failure".to_owned(),
        RestartMode::No | RestartMode::Unspecified => "Never".to_owned(),
    }
}

fn health(health: HealthStatus) -> String {
    match health {
        HealthStatus::Starting => "Starting",
        HealthStatus::Healthy => "Healthy",
        HealthStatus::Unhealthy => "Unhealthy",
        HealthStatus::None | HealthStatus::Unspecified => "None defined",
    }
    .to_owned()
}

fn when(time: SystemTime) -> String {
    let seconds = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default();
    let date = glib::DateTime::from_unix_local(seconds)
        .and_then(|date| date.format("%-d %b %Y, %H:%M"))
        .map(String::from)
        .unwrap_or_default();
    format!("{date} · {}", format::ago(time, SystemTime::now()))
}

fn find_in(buffer: &gtk::TextBuffer, view: &gtk::TextView, query: &str, next: bool) {
    buffer.remove_tag_by_name("found", &buffer.start_iter(), &buffer.end_iter());
    if query.is_empty() {
        return;
    }
    let from = if next {
        buffer.iter_at_mark(&buffer.get_insert())
    } else {
        buffer.start_iter()
    };
    let flags = gtk::TextSearchFlags::CASE_INSENSITIVE;
    let found = from
        .forward_search(query, flags, None)
        .or_else(|| buffer.start_iter().forward_search(query, flags, None));
    if let Some((mut start, end)) = found {
        buffer.apply_tag_by_name("found", &start, &end);
        buffer.place_cursor(&end);
        view.scroll_to_iter(&mut start, 0.1, true, 0.0, 0.3);
    }
}
