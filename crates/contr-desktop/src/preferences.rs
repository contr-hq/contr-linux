//! The Preferences dialog: this client's own settings, the engine, and the daemon settings an administrator may lock.

use std::cell::RefCell;
use std::rc::Rc;

use contr_core::product;
use contr_daemon_ui::{Daemon, EngineAction, SettingsHandle, Status, describe, engine_kind, engine_name, field_mask};
use contr_sdk::api::daemon::v1::Capability;
use contr_sdk::api::engine::v1::{EngineState, StreamUsageRequest};
use contr_sdk::api::settings::v1::{
    CertificateSettings, EngineSettings, ProxyMode, ProxySettings, RegistrySettings, Settings, SettingsState,
    UpdateSettingsRequest,
};
use contr_ui_common::format;
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};

use crate::autostart;
use crate::client_settings::ClientSettings;

const PROXY_MODES: [(&str, ProxyMode); 3] = [
    ("System", ProxyMode::System),
    ("Manual", ProxyMode::Manual),
    ("None", ProxyMode::None),
];

/// What the dialog reads and changes outside the daemon.
#[derive(Clone)]
pub(crate) struct PreferencesContext {
    pub(crate) daemon: Daemon,
    pub(crate) settings: Rc<RefCell<ClientSettings>>,
    pub(crate) tray_available: bool,
}

struct Dialog {
    window: adw::PreferencesDialog,
    daemon: Daemon,
    usage: RefCell<Option<tokio::task::JoinHandle<()>>>,
}

pub(crate) fn present(parent: &impl IsA<gtk::Widget>, context: &PreferencesContext) {
    let dialog = adw::PreferencesDialog::builder().search_enabled(false).build();
    let state = Rc::new(Dialog {
        window: dialog.clone(),
        daemon: context.daemon.clone(),
        usage: RefCell::default(),
    });
    let status = context.daemon.status();
    dialog.add(&general(&state, context, &status));
    dialog.add(&engine_page(&state, &status));
    dialog.add(&network_page(&state, &status));
    dialog.add(&updates_page());
    let owner = RefCell::new(Some(Rc::clone(&state)));
    dialog.connect_closed(move |_| {
        if let Some(state) = owner.borrow_mut().take()
            && let Some(task) = state.usage.borrow_mut().take()
        {
            task.abort();
        }
    });
    dialog.present(Some(parent));
}

fn general(state: &Rc<Dialog>, context: &PreferencesContext, status: &Status) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    let engine = adw::PreferencesGroup::builder().title("Engine").build();
    let engine_row = adw::ActionRow::builder()
        .title("Engine")
        .subtitle(glib::markup_escape_text(&engine_summary(status)))
        .build();
    let change = gtk::Button::builder()
        .label("Change…")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .tooltip_text("Choosing another engine isn’t available yet")
        .build();
    engine_row.add_suffix(&change);
    engine.add(&engine_row);
    engine.add(&context_row(status));
    page.add(&engine);

    let startup = adw::PreferencesGroup::builder().title("Startup and background").build();
    let login = adw::SwitchRow::builder()
        .title(format!("Start {} when I log in", product::DISPLAY_NAME))
        .active(autostart::enabled())
        .build();
    let dialog = Rc::downgrade(state);
    login.connect_active_notify(move |row| {
        if let Err(error) = autostart::set(row.is_active())
            && let Some(dialog) = dialog.upgrade()
        {
            dialog
                .window
                .add_toast(adw::Toast::new(&format!("Couldn’t change the login item: {error}")));
        }
    });
    startup.add(&login);
    let keep = adw::SwitchRow::builder()
        .title("Keep running when the window is closed")
        .subtitle("Shows a tray icon when your desktop supports one")
        .active(context.settings.borrow().keep_running)
        .build();
    bind_client(&keep, &context.settings, state, |settings, active| {
        settings.keep_running = active;
    });
    startup.add(&keep);
    if !context.tray_available {
        let warning = gtk::Label::builder()
            .label(format!(
                "GNOME needs the AppIndicator extension to show tray icons. Without it, {} keeps running in the \
                 background and you can reopen it from the app grid.",
                product::DISPLAY_NAME
            ))
            .wrap(true)
            .xalign(0.0)
            .css_classes(["group-description"])
            .build();
        let icon = gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .valign(gtk::Align::Start)
            .css_classes(["status-paused"])
            .build();
        let content = gtk::Box::builder().spacing(10).css_classes(["container-row"]).build();
        content.append(&icon);
        content.append(&warning);
        let row = gtk::ListBoxRow::builder()
            .child(&content)
            .activatable(false)
            .css_classes(["tinted-warning"])
            .build();
        startup.add(&row);
    }
    page.add(&startup);

    let notifications = adw::PreferencesGroup::builder().title("Notifications").build();
    let errors = adw::SwitchRow::builder()
        .title("When a container stops with an error")
        .active(context.settings.borrow().notify_container_errors)
        .build();
    bind_client(&errors, &context.settings, state, |settings, active| {
        settings.notify_container_errors = active;
    });
    let finished = adw::SwitchRow::builder()
        .title("When a pull or compose up finishes")
        .active(context.settings.borrow().notify_finished)
        .build();
    bind_client(&finished, &context.settings, state, |settings, active| {
        settings.notify_finished = active;
    });
    notifications.add(&errors);
    notifications.add(&finished);
    page.add(&notifications);

    let registry = adw::PreferencesGroup::builder().title("Registry").build();
    let mirror = adw::EntryRow::builder()
        .title("Registry mirror")
        .show_apply_button(true)
        .build();
    registry.add(&mirror);
    page.add(&registry);
    load_daemon_settings(state, status, move |dialog, state, configurable| {
        let mirrors = state
            .settings
            .as_ref()
            .and_then(|settings| settings.registry.as_ref())
            .map(|registry| registry.mirrors.join(", "))
            .unwrap_or_default();
        mirror.set_text(&mirrors);
        lock(&mirror, &state.locked_paths, "registry.mirrors");
        if !configurable {
            mirror.set_sensitive(false);
            registry.set_description(Some(&engine_owns_configuration(&dialog.daemon.status())));
        }
        let dialog = Rc::downgrade(dialog);
        mirror.connect_apply(move |row| {
            let mirrors: Vec<String> = row
                .text()
                .split([',', ' '])
                .filter(|mirror| !mirror.is_empty())
                .map(str::to_owned)
                .collect();
            if let Some(dialog) = dialog.upgrade() {
                update(
                    &dialog,
                    Settings {
                        registry: Some(RegistrySettings { mirrors }),
                        ..Settings::default()
                    },
                    &["registry.mirrors"],
                );
            }
        });
    });
    page
}

fn engine_page(state: &Rc<Dialog>, status: &Status) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Engine")
        .icon_name("contr-containers-symbolic")
        .build();
    let startup = adw::PreferencesGroup::builder().title("Startup").build();
    let start = adw::SwitchRow::builder()
        .title(format!("Start the engine when {} starts", product::DISPLAY_NAME))
        .build();
    startup.add(&start);
    page.add(&startup);
    load_daemon_settings(state, status, {
        let start = start.clone();
        move |dialog, state, _| {
            let enabled = state
                .settings
                .as_ref()
                .and_then(|settings| settings.engine.as_ref())
                .is_some_and(|engine| engine.start_with_daemon);
            start.set_active(enabled);
            lock(&start, &state.locked_paths, "engine.start_with_daemon");
            let dialog = Rc::downgrade(dialog);
            start.connect_active_notify(move |row| {
                if let Some(dialog) = dialog.upgrade() {
                    update(
                        &dialog,
                        Settings {
                            engine: Some(EngineSettings {
                                start_with_daemon: row.is_active(),
                                ..EngineSettings::default()
                            }),
                            ..Settings::default()
                        },
                        &["engine.start_with_daemon"],
                    );
                }
            });
        }
    });

    if let Some(info) = &status.engine_info {
        let details = adw::PreferencesGroup::builder().title("Details").build();
        for (title, value) in [
            ("Engine", engine_name(info)),
            ("Version", format!("{} · API {}", info.version, info.api_version)),
            ("Endpoint", info.endpoint.clone()),
            (
                "Docker context",
                if info.docker_context.is_empty() {
                    "None".to_owned()
                } else {
                    info.docker_context.clone()
                },
            ),
            (
                "System",
                format!("{} {} · kernel {}", info.os, info.arch, info.kernel_version),
            ),
        ] {
            details.add(
                &adw::ActionRow::builder()
                    .title(title)
                    .subtitle(glib::markup_escape_text(&value))
                    .subtitle_selectable(true)
                    .css_classes(["property"])
                    .build(),
            );
        }
        page.add(&details);
    }

    let usage = adw::PreferencesGroup::builder()
        .title("Usage")
        .description("What the engine’s running containers use, and the disk its data takes")
        .build();
    let cpu = adw::ActionRow::builder().title("CPU").build();
    let memory = adw::ActionRow::builder().title("Memory").build();
    let disk = adw::ActionRow::builder().title("Disk").build();
    let labels: Vec<gtk::Label> = [&cpu, &memory, &disk]
        .into_iter()
        .map(|row| {
            let label = gtk::Label::builder().css_classes(["dim-label"]).build();
            row.add_suffix(&label);
            usage.add(row);
            label
        })
        .collect();
    usage.set_visible(status.is_running());
    page.add(&usage);
    if let Some(client) = status.running_client().cloned() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let task = relm4::spawn(async move {
            let Ok(stream) = client.engine().stream_usage(StreamUsageRequest {}).await else {
                return;
            };
            let mut stream = stream.into_inner();
            while let Ok(Some(response)) = stream.message().await {
                if let Some(usage) = response.usage
                    && sender.send(usage).is_err()
                {
                    return;
                }
            }
        });
        state.usage.replace(Some(task));
        glib::spawn_future_local(async move {
            while let Some(usage) = receiver.recv().await {
                labels[0].set_label(&format!(
                    "{} of {} CPUs",
                    format::percent(usage.cpu_percent),
                    usage.cpus
                ));
                labels[1].set_label(&format::bytes_of(usage.memory_used_bytes, usage.memory_total_bytes));
                labels[2].set_label(&if usage.disk_used_bytes == 0 {
                    "Measuring…".to_owned()
                } else {
                    format::bytes(usage.disk_used_bytes)
                });
            }
        });
    }
    page
}

fn network_page(state: &Rc<Dialog>, status: &Status) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Network")
        .icon_name("network-wired-symbolic")
        .build();
    let proxy = adw::PreferencesGroup::builder()
        .title("Proxy")
        .description("How the engine reaches servers outside this computer for pulls, pushes and builds")
        .build();
    let mode = adw::ComboRow::builder()
        .title("Proxy")
        .model(&gtk::StringList::new(&PROXY_MODES.map(|mode| mode.0)))
        .build();
    let http = adw::EntryRow::builder()
        .title("HTTP proxy")
        .show_apply_button(true)
        .build();
    let https = adw::EntryRow::builder()
        .title("HTTPS proxy")
        .show_apply_button(true)
        .build();
    let exclude = adw::EntryRow::builder()
        .title("Bypass for")
        .show_apply_button(true)
        .build();
    let pass = adw::SwitchRow::builder()
        .title("Pass the proxy to containers")
        .subtitle("New containers and builds get HTTP_PROXY, HTTPS_PROXY and NO_PROXY")
        .build();
    for row in [
        mode.upcast_ref::<gtk::Widget>(),
        http.upcast_ref(),
        https.upcast_ref(),
        exclude.upcast_ref(),
        pass.upcast_ref(),
    ] {
        proxy.add(row);
    }
    page.add(&proxy);
    let certificates = adw::PreferencesGroup::builder().title("Certificates").build();
    let trust = adw::SwitchRow::builder()
        .title("Trust this computer’s certificates")
        .subtitle("Includes those an organization deploys to inspect HTTPS traffic")
        .build();
    certificates.add(&trust);
    page.add(&certificates);

    load_daemon_settings(state, status, move |dialog, state, configurable| {
        let settings = state.settings.clone().unwrap_or_default();
        let current = settings.proxy.clone().unwrap_or_default();
        let index = PROXY_MODES
            .iter()
            .position(|(_, mode)| *mode == current.mode())
            .unwrap_or(0);
        mode.set_selected(u32::try_from(index).unwrap_or(0));
        http.set_text(&current.http);
        https.set_text(&current.https);
        exclude.set_text(&current.exclude);
        pass.set_active(current.pass_to_containers);
        trust.set_active(
            settings
                .certificates
                .as_ref()
                .is_some_and(|certificates| certificates.trust_system),
        );
        let manual = current.mode() == ProxyMode::Manual;
        for row in [&http, &https, &exclude] {
            row.set_visible(manual);
        }
        lock(&mode, &state.locked_paths, "proxy.mode");
        lock(&http, &state.locked_paths, "proxy.http");
        lock(&https, &state.locked_paths, "proxy.https");
        lock(&exclude, &state.locked_paths, "proxy.exclude");
        lock(&pass, &state.locked_paths, "proxy.pass_to_containers");
        lock(&trust, &state.locked_paths, "certificates.trust_system");
        if !configurable {
            let note = engine_owns_configuration(&dialog.daemon.status());
            for group in [&proxy, &certificates] {
                group.set_description(Some(&note));
                group.set_sensitive(false);
            }
        }

        let weak = Rc::downgrade(dialog);
        let proxy_rows = (mode.clone(), http.clone(), https.clone(), exclude.clone(), pass.clone());
        let save_proxy = Rc::new(move || {
            let (mode, http, https, exclude, pass) = &proxy_rows;
            let selected = PROXY_MODES
                .get(usize::try_from(mode.selected()).unwrap_or(0))
                .map_or(ProxyMode::System, |(_, mode)| *mode);
            for row in [http, https, exclude] {
                row.set_visible(selected == ProxyMode::Manual);
            }
            if let Some(dialog) = weak.upgrade() {
                update(
                    &dialog,
                    Settings {
                        proxy: Some(ProxySettings {
                            mode: selected.into(),
                            http: http.text().trim().to_owned(),
                            https: https.text().trim().to_owned(),
                            exclude: exclude.text().trim().to_owned(),
                            pass_to_containers: pass.is_active(),
                        }),
                        ..Settings::default()
                    },
                    &[
                        "proxy.mode",
                        "proxy.http",
                        "proxy.https",
                        "proxy.exclude",
                        "proxy.pass_to_containers",
                    ],
                );
            }
        });
        let save = Rc::clone(&save_proxy);
        mode.connect_selected_notify(move |_| save());
        for row in [&http, &https, &exclude] {
            let save = Rc::clone(&save_proxy);
            row.connect_apply(move |_| save());
        }
        let save = Rc::clone(&save_proxy);
        pass.connect_active_notify(move |_| save());
        let weak = Rc::downgrade(dialog);
        trust.connect_active_notify(move |row| {
            if let Some(dialog) = weak.upgrade() {
                update(
                    &dialog,
                    Settings {
                        certificates: Some(CertificateSettings {
                            trust_system: row.is_active(),
                        }),
                        ..Settings::default()
                    },
                    &["certificates.trust_system"],
                );
            }
        });
    });
    page
}

fn updates_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Updates")
        .icon_name("software-update-available-symbolic")
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Updates")
        .description(format!(
            "{} for Linux is updated by your package manager or Flatpak, with the rest of the system.",
            product::DISPLAY_NAME
        ))
        .build();
    group.add(
        &adw::ActionRow::builder()
            .title("Version")
            .subtitle(env!("CARGO_PKG_VERSION"))
            .css_classes(["property"])
            .build(),
    );
    page.add(&group);
    page
}

fn engine_summary(status: &Status) -> String {
    let state = match status.engine {
        EngineState::Running => "running",
        EngineState::Stopped => "stopped",
        EngineState::Starting | EngineState::Provisioning => "starting",
        EngineState::Failed => "failed",
        _ => "unknown",
    };
    match &status.engine_info {
        Some(info) => format!(
            "{} · {state} · {}",
            engine_name(info),
            info.endpoint.trim_start_matches("unix://")
        ),
        None => format!("Engine {state}"),
    }
}

fn engine_owns_configuration(status: &Status) -> String {
    let kind = status.engine_info.as_ref().map_or("This engine", engine_kind);
    format!("{kind} keeps its own configuration, so these settings don’t apply to it.")
}

/// The switch that makes the Docker CLI use the daemon's context, through `docker context`.
fn context_row(status: &Status) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder()
        .title(format!("Make {} the current docker context", product::DOCKER_CONTEXT))
        .subtitle("The docker command then uses Contr without --context")
        .build();
    let context = status
        .engine_info
        .as_ref()
        .map(|info| info.docker_context.clone())
        .filter(|context| !context.is_empty());
    let Some(context) = context else {
        row.set_sensitive(false);
        row.set_subtitle(&format!("{} created no Docker context", product::DISPLAY_NAME));
        return row;
    };
    let handler: Rc<RefCell<Option<glib::SignalHandlerId>>> = Rc::default();
    let switch = row.clone();
    let target = context.clone();
    let id = row.connect_active_notify(move |row| {
        let wanted = if row.is_active() {
            target.clone()
        } else {
            "default".to_owned()
        };
        let row = row.clone();
        glib::spawn_future_local(async move {
            let done = relm4::spawn(docker(vec!["context".to_owned(), "use".to_owned(), wanted])).await;
            if !matches!(done, Ok(Some(_))) {
                row.set_subtitle("The docker command couldn’t switch contexts");
            }
        });
    });
    handler.replace(Some(id));
    glib::spawn_future_local(async move {
        let current = relm4::spawn(docker(vec!["context".to_owned(), "show".to_owned()])).await;
        let current = current.ok().flatten().unwrap_or_default();
        if let Some(id) = handler.borrow().as_ref() {
            switch.block_signal(id);
            switch.set_active(current.trim() == context);
            switch.unblock_signal(id);
        }
    });
    row
}

/// Runs the Docker CLI and returns its standard output, or `None` when it fails.
async fn docker(args: Vec<String>) -> Option<String> {
    let output = tokio::process::Command::new("docker").args(&args).output().await.ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn bind_client(
    row: &adw::SwitchRow,
    settings: &Rc<RefCell<ClientSettings>>,
    state: &Rc<Dialog>,
    apply: fn(&mut ClientSettings, bool),
) {
    let settings = Rc::clone(settings);
    let dialog = Rc::downgrade(state);
    row.connect_active_notify(move |row| {
        apply(&mut settings.borrow_mut(), row.is_active());
        if let Err(error) = settings.borrow().save()
            && let Some(dialog) = dialog.upgrade()
        {
            dialog
                .window
                .add_toast(adw::Toast::new(&format!("Couldn’t save the setting: {error}")));
        }
    });
}

/// Fetches the daemon's settings once and hands them to `show` with whether the engine applies them.
fn load_daemon_settings(
    state: &Rc<Dialog>,
    status: &Status,
    show: impl FnOnce(&Rc<Dialog>, &SettingsState, bool) + 'static,
) {
    let Some(client) = status.client().cloned() else { return };
    let configurable = status.has(Capability::EngineConfiguration);
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        let result = relm4::spawn(async move { SettingsHandle::new(client).get().await }).await;
        let (Some(dialog), Ok(Ok(settings))) = (weak.upgrade(), result) else {
            return;
        };
        show(&dialog, &settings, configurable);
    });
}

/// Makes `row` read-only with a note when an administrator fixes `path`.
fn lock(row: &impl IsA<gtk::Widget>, locked: &[String], path: &str) {
    if !locked.iter().any(|locked| locked == path) {
        return;
    }
    let row = row.upcast_ref::<gtk::Widget>();
    row.set_sensitive(false);
    row.set_tooltip_text(Some("Managed by your organization"));
    if let Some(action) = row.downcast_ref::<adw::ActionRow>() {
        action.add_suffix(
            &gtk::Image::builder()
                .icon_name("changes-prevent-symbolic")
                .tooltip_text("Managed by your organization")
                .build(),
        );
    }
}

fn update(dialog: &Rc<Dialog>, settings: Settings, paths: &'static [&'static str]) {
    let Some(client) = dialog.daemon.status().client().cloned() else {
        return;
    };
    let weak = Rc::downgrade(dialog);
    glib::spawn_future_local(async move {
        let result = relm4::spawn(async move {
            let request = UpdateSettingsRequest {
                settings: Some(settings),
                update_mask: Some(field_mask(paths)),
                etag: String::new(),
            };
            client
                .settings()
                .update_settings(request)
                .await
                .map(tonic::Response::into_inner)
        })
        .await;
        let Some(dialog) = weak.upgrade() else { return };
        match result {
            Ok(Ok(response)) if !response.restart_required.is_empty() => {
                let toast = adw::Toast::builder()
                    .title("Takes effect when the engine restarts")
                    .button_label("Restart Engine")
                    .build();
                let daemon = dialog.daemon.clone();
                toast.connect_button_clicked(move |_| {
                    let daemon = daemon.clone();
                    glib::spawn_future_local(async move {
                        let _ = daemon.run(EngineAction::Restart).await;
                    });
                });
                dialog.window.add_toast(toast);
            }
            Ok(Err(status)) => dialog
                .window
                .add_toast(adw::Toast::new(&describe(&contr_sdk::Error::from(status)))),
            Ok(Ok(_)) | Err(_) => {}
        }
    });
}
