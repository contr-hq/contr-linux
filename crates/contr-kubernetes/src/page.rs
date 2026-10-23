use std::path::PathBuf;
use std::time::SystemTime;

use contr_daemon_ui::operations::{self, Kind};
use contr_daemon_ui::{Daemon, EngineGate, SettingsHandle, Status, describe, while_running};
use contr_sdk::api::images::v1::ListImagesRequest;
use contr_sdk::api::kubernetes::v1::{
    Cluster, ClusterState, ImportImagesRequest, ResetClusterRequest, SetCurrentContextRequest, WatchClusterRequest,
};
use contr_sdk::api::settings::v1::{KubernetesSettings, Settings, SettingsState, WatchSettingsRequest};
use contr_sdk::{Client, Error};
use contr_ui_common::format;
use contr_ui_common::widgets::{
    GroupHeader, StatusKind, boxed_list, icon_button, page_column, set_status, status_icon,
};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::model;

type Input = relm4::Sender<KubernetesMsg>;

const ENABLED: &str = "kubernetes.enabled";
const VERSION: &str = "kubernetes.version";
const TRAEFIK: &str = "kubernetes.traefik";

/// The Kubernetes section: turning the single-node cluster on and off, following its start-up, and maintaining it.
pub struct KubernetesPage {
    status: Status,
    cluster: Cluster,
    settings: Option<SettingsState>,
    /// Etag of the last settings change; until a cluster state follows it, the page shows the change as pending.
    awaited: Option<u64>,
    home: PathBuf,
}

#[derive(Debug)]
pub enum KubernetesMsg {
    Enable(bool),
    ChooseVersion(String),
    SetVersion(String),
    Traefik(bool),
    CurrentContext(bool),
    CopyApiServer,
    OpenKubeconfig,
    Import,
    ImportImages(Vec<String>),
    Reset,
    ConfirmReset,
    /// Shows the settings again, such as after the user declined a change.
    Refresh,
}

#[derive(Debug)]
pub enum KubernetesOutput {
    /// The cluster's state, for the sidebar.
    State(ClusterState),
    /// The cluster became ready after starting.
    Ready,
}

#[doc(hidden)]
#[derive(Debug)]
pub enum KubernetesCmd {
    Status(Box<Status>),
    Cluster(Box<Cluster>),
    Settings(Box<SettingsState>),
    Updated(Result<SettingsState, String>),
    Images(Result<Vec<String>, String>),
    Done(Result<String, String>),
}

pub struct KubernetesWidgets {
    title: adw::WindowTitle,
    summary_icon: gtk::Image,
    summary_spinner: adw::Spinner,
    summary_title: gtk::Label,
    summary_subtitle: gtk::Label,
    enabled_box: gtk::Box,
    enabled: gtk::Switch,
    enabled_handler: glib::SignalHandlerId,
    cancel: gtk::Button,
    progress: gtk::ProgressBar,
    steps: gtk::ListBox,
    note: gtk::Box,
    cluster_header: GroupHeader,
    cluster_list: gtk::ListBox,
    versions: gtk::StringList,
    version: adw::ComboRow,
    version_handler: glib::SignalHandlerId,
    api_server: adw::ActionRow,
    kubeconfig: adw::ActionRow,
    current: adw::SwitchRow,
    current_handler: glib::SignalHandlerId,
    traefik: adw::SwitchRow,
    traefik_handler: glib::SignalHandlerId,
    pods_group: gtk::Box,
    pods: gtk::ListBox,
    maintenance: gtk::Box,
    import: adw::ActionRow,
    footer: gtk::Label,
}

impl Component for KubernetesPage {
    type CommandOutput = KubernetesCmd;
    type Input = KubernetesMsg;
    type Output = KubernetesOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = KubernetesWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    #[allow(clippy::similar_names)]
    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let input = sender.input_sender();
        let title = adw::WindowTitle::new("Kubernetes", "k3s in a container on your engine");
        let header = adw::HeaderBar::builder().title_widget(&title).build();

        let summary_icon = gtk::Image::builder()
            .icon_name("contr-kubernetes-symbolic")
            .pixel_size(22)
            .valign(gtk::Align::Center)
            .css_classes(["cluster-icon"])
            .build();
        let summary_spinner = adw::Spinner::builder()
            .width_request(44)
            .height_request(44)
            .valign(gtk::Align::Center)
            .build();
        let summary_title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-3"])
            .build();
        let summary_subtitle = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let summary_text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        summary_text.append(&summary_title);
        summary_text.append(&summary_subtitle);
        let enabled = gtk::Switch::builder().valign(gtk::Align::Center).build();
        let enable_input = input.clone();
        let enabled_handler =
            enabled.connect_active_notify(move |switch| enable_input.emit(KubernetesMsg::Enable(switch.is_active())));
        let enabled_box = gtk::Box::builder().spacing(10).valign(gtk::Align::Center).build();
        enabled_box.append(&gtk::Label::builder().label("Enabled").css_classes(["heading"]).build());
        enabled_box.append(&enabled);
        let cancel = gtk::Button::builder()
            .label("Cancel")
            .valign(gtk::Align::Center)
            .build();
        let cancel_input = input.clone();
        cancel.connect_clicked(move |_| cancel_input.emit(KubernetesMsg::Enable(false)));
        let summary_row = gtk::Box::builder()
            .spacing(14)
            .margin_top(16)
            .margin_bottom(16)
            .margin_start(16)
            .margin_end(16)
            .build();
        summary_row.append(&summary_icon);
        summary_row.append(&summary_spinner);
        summary_row.append(&summary_text);
        summary_row.append(&enabled_box);
        summary_row.append(&cancel);
        let progress = gtk::ProgressBar::builder().css_classes(["cluster-progress"]).build();
        let steps = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["card-list"])
            .build();
        let summary = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["card"])
            .build();
        summary.append(&summary_row);
        summary.append(&progress);
        summary.append(&steps);

        let note = gtk::Box::builder()
            .spacing(10)
            .css_classes(["card", "tinted-accent", "note-card"])
            .build();
        note.append(&gtk::Image::from_icon_name("dialog-information-symbolic"));
        note.append(
            &gtk::Label::builder()
                .label("Kubernetes runs on System Docker. It isn’t available with rootless Docker or Podman yet.")
                .xalign(0.0)
                .wrap(true)
                .build(),
        );

        let versions = gtk::StringList::new(&[]);
        let version = adw::ComboRow::builder()
            .title("Version")
            .subtitle("Changing it recreates the cluster")
            .model(&versions)
            .build();
        let version_input = input.clone();
        let version_handler = version.connect_selected_item_notify(move |row| {
            if let Some(item) = row.selected_item().and_downcast::<gtk::StringObject>() {
                version_input.emit(KubernetesMsg::ChooseVersion(item.string().into()));
            }
        });
        let api_server = adw::ActionRow::builder()
            .title("API server")
            .subtitle_selectable(true)
            .css_classes(["mono-subtitle"])
            .build();
        let copy = icon_button("edit-copy-symbolic", "Copy");
        let copy_input = input.clone();
        copy.connect_clicked(move |_| copy_input.emit(KubernetesMsg::CopyApiServer));
        api_server.add_suffix(&copy);
        let kubeconfig = adw::ActionRow::builder().title("Kubeconfig").build();
        let open = icon_button("contr-open-external-symbolic", "Open the Kubeconfig File");
        let open_input = input.clone();
        open.connect_clicked(move |_| open_input.emit(KubernetesMsg::OpenKubeconfig));
        kubeconfig.add_suffix(&open);
        let current = adw::SwitchRow::new();
        let current_input = input.clone();
        let current_handler = current
            .connect_active_notify(move |row| current_input.emit(KubernetesMsg::CurrentContext(row.is_active())));
        let traefik = adw::SwitchRow::builder()
            .title("Traefik ingress controller")
            .subtitle("Routes Ingress resources to your services")
            .build();
        let traefik_input = input.clone();
        let traefik_handler =
            traefik.connect_active_notify(move |row| traefik_input.emit(KubernetesMsg::Traefik(row.is_active())));
        let cluster_list = boxed_list();
        for row in [
            version.upcast_ref::<gtk::Widget>(),
            api_server.upcast_ref(),
            kubeconfig.upcast_ref(),
            current.upcast_ref(),
            traefik.upcast_ref(),
        ] {
            cluster_list.append(row);
        }
        let cluster_header = GroupHeader::new("Cluster", "");
        let cluster_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        cluster_group.append(&cluster_header.root);
        cluster_group.append(&cluster_list);

        let pods_header = GroupHeader::new("System pods", "kube-system");
        let pods = boxed_list();
        let pods_group = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        pods_group.append(&pods_header.root);
        pods_group.append(&pods);

        let import = adw::ActionRow::builder()
            .title("Use local images in the cluster")
            .subtitle("Copies images from your engine into the cluster, so pods can use them without a registry")
            .build();
        let import_button = gtk::Button::builder()
            .label("Import Images…")
            .valign(gtk::Align::Center)
            .build();
        let import_input = input.clone();
        import_button.connect_clicked(move |_| import_input.emit(KubernetesMsg::Import));
        import.add_suffix(&import_button);
        let reset = adw::ActionRow::builder()
            .title("Reset cluster")
            .subtitle("Deletes all workloads and cluster data. Your images stay.")
            .build();
        let reset_button = gtk::Button::builder()
            .label("Reset…")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-text"])
            .build();
        let reset_input = input.clone();
        reset_button.connect_clicked(move |_| reset_input.emit(KubernetesMsg::Reset));
        reset.add_suffix(&reset_button);
        let maintenance_list = boxed_list();
        maintenance_list.append(&import);
        maintenance_list.append(&reset);
        let maintenance = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        maintenance.append(&GroupHeader::new("Maintenance", "").root);
        maintenance.append(&maintenance_list);

        let footer = gtk::Label::builder()
            .label("You can leave this page. Contr sends a notification when the cluster is ready.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label", "caption"])
            .build();

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        for part in [
            summary.upcast_ref::<gtk::Widget>(),
            note.upcast_ref(),
            cluster_group.upcast_ref(),
            pods_group.upcast_ref(),
            maintenance.upcast_ref(),
            footer.upcast_ref(),
        ] {
            column.append(part);
        }

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&EngineGate::around(
            &daemon,
            "contr-kubernetes-symbolic",
            &page_column(&column),
        )));
        root.set_child(Some(&toolbar));
        follow(&daemon, &sender);

        let model = KubernetesPage {
            status: daemon.status(),
            cluster: Cluster::default(),
            settings: None,
            awaited: None,
            home: glib::home_dir(),
        };
        let mut widgets = KubernetesWidgets {
            title,
            summary_icon,
            summary_spinner,
            summary_title,
            summary_subtitle,
            enabled_box,
            enabled,
            enabled_handler,
            cancel,
            progress,
            steps,
            note,
            cluster_header,
            cluster_list,
            versions,
            version,
            version_handler,
            api_server,
            kubeconfig,
            current,
            current_handler,
            traefik,
            traefik_handler,
            pods_group,
            pods,
            maintenance,
            import,
            footer,
        };
        model.show(&mut widgets);
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: KubernetesMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        let input = sender.input_sender();
        match message {
            KubernetesMsg::Enable(enabled) => self.update_settings(
                &sender,
                KubernetesSettings {
                    enabled,
                    ..KubernetesSettings::default()
                },
                ENABLED,
            ),
            KubernetesMsg::ChooseVersion(version) => {
                if version != self.version() {
                    confirm(
                        root,
                        input,
                        &format!("Change to Kubernetes {version}?"),
                        "Changing the version deletes the cluster’s workloads and data. Your images stay.",
                        "Change Version",
                        KubernetesMsg::SetVersion(version),
                    );
                }
            }
            KubernetesMsg::SetVersion(version) => self.update_settings(
                &sender,
                KubernetesSettings {
                    version,
                    ..KubernetesSettings::default()
                },
                VERSION,
            ),
            KubernetesMsg::Traefik(traefik) => self.update_settings(
                &sender,
                KubernetesSettings {
                    traefik,
                    ..KubernetesSettings::default()
                },
                TRAEFIK,
            ),
            KubernetesMsg::CurrentContext(current) => {
                if current != self.is_current() {
                    self.call(&sender, move |client| async move {
                        client
                            .kubernetes()
                            .set_current_context(SetCurrentContextRequest { current })
                            .await?;
                        Ok(String::new())
                    });
                }
            }
            KubernetesMsg::CopyApiServer => {
                root.clipboard().set_text(&self.cluster.api_server);
                root.add_toast(adw::Toast::new("Copied the API server address"));
            }
            KubernetesMsg::OpenKubeconfig => {
                let file = gio::File::for_path(&self.cluster.kubeconfig);
                gtk::FileLauncher::new(Some(&file)).launch(
                    root.root().and_downcast_ref::<gtk::Window>(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
            KubernetesMsg::Import => {
                let Some(client) = self.status.running_client().cloned() else {
                    return;
                };
                sender.oneshot_command(async move {
                    let result = client.images().list_images(ListImagesRequest {}).await;
                    KubernetesCmd::Images(
                        result
                            .map(|response| {
                                let mut tags: Vec<String> = response
                                    .into_inner()
                                    .images
                                    .into_iter()
                                    .flat_map(|image| image.repo_tags)
                                    .filter(|tag| tag != "<none>:<none>")
                                    .collect();
                                tags.sort();
                                tags
                            })
                            .map_err(|status| describe(&Error::from(status))),
                    )
                });
            }
            KubernetesMsg::ImportImages(images) => self.call(&sender, move |client| async move {
                operations::start(&client, Kind::ImportImages(ImportImagesRequest { images })).await?;
                Ok(String::new())
            }),
            KubernetesMsg::Reset => confirm(
                root,
                input,
                "Reset the Cluster?",
                "This deletes all workloads and cluster data, then starts a new cluster. Your images stay.",
                "Reset",
                KubernetesMsg::ConfirmReset,
            ),
            KubernetesMsg::ConfirmReset => self.call(&sender, |client| async move {
                client.kubernetes().reset_cluster(ResetClusterRequest {}).await?;
                Ok(String::new())
            }),
            KubernetesMsg::Refresh => {}
        }
        self.show(widgets);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: KubernetesCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            KubernetesCmd::Status(status) => self.status = *status,
            KubernetesCmd::Cluster(cluster) => {
                let was = self.cluster.state();
                self.cluster = *cluster;
                if self
                    .awaited
                    .is_some_and(|awaited| model::caught_up(&self.cluster, awaited))
                {
                    self.awaited = None;
                }
                let state = self.cluster.state();
                let _ = sender.output(KubernetesOutput::State(state));
                if was == ClusterState::Starting && state == ClusterState::Ready {
                    let _ = sender.output(KubernetesOutput::Ready);
                }
            }
            KubernetesCmd::Settings(settings) => self.settings = Some(*settings),
            KubernetesCmd::Updated(Ok(settings)) => {
                self.awaited = settings.etag.parse().ok();
                self.settings = Some(settings);
            }
            KubernetesCmd::Updated(Err(message)) | KubernetesCmd::Done(Err(message)) => {
                root.add_toast(adw::Toast::new(&message));
            }
            KubernetesCmd::Done(Ok(message)) => {
                if !message.is_empty() {
                    root.add_toast(adw::Toast::new(&message));
                }
            }
            KubernetesCmd::Images(Ok(images)) => import_dialog(root, sender.input_sender(), &images),
            KubernetesCmd::Images(Err(message)) => root.add_toast(adw::Toast::new(&message)),
        }
        self.show(widgets);
    }
}

impl KubernetesPage {
    fn kubernetes_settings(&self) -> KubernetesSettings {
        self.settings
            .as_ref()
            .and_then(|state| state.settings.as_ref())
            .and_then(|settings| settings.kubernetes.clone())
            .unwrap_or_default()
    }

    fn version(&self) -> String {
        model::chosen_version(&self.cluster, &self.kubernetes_settings().version).to_owned()
    }

    fn is_current(&self) -> bool {
        !self.cluster.context.is_empty() && self.cluster.current_context == self.cluster.context
    }

    fn locked(&self, path: &str) -> bool {
        self.settings
            .as_ref()
            .is_some_and(|state| state.locked_paths.iter().any(|locked| locked == path))
    }

    fn update_settings(&self, sender: &ComponentSender<Self>, kubernetes: KubernetesSettings, path: &'static str) {
        let Some(client) = self.status.running_client().cloned() else {
            return;
        };
        sender.oneshot_command(async move {
            let settings = Settings {
                kubernetes: Some(kubernetes),
                ..Settings::default()
            };
            let result = SettingsHandle::new(client).update(settings, &[path]).await;
            KubernetesCmd::Updated(result.map_err(|error| describe(&error)))
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
        sender
            .oneshot_command(async move { KubernetesCmd::Done(call(client).await.map_err(|error| describe(&error))) });
    }

    fn show(&self, widgets: &mut KubernetesWidgets) {
        let cluster = &self.cluster;
        let state = cluster.state();
        let settings = self.kubernetes_settings();
        let version = self.version();
        let pending = self.awaited.is_some();
        let summary = model::summary(cluster, &version, SystemTime::now());

        widgets.title.set_subtitle(match state {
            ClusterState::Starting => "Turning on",
            ClusterState::Stopping => "Turning off",
            _ => "k3s in a container on your engine",
        });
        widgets.summary_title.set_label(summary.title);
        let subtitle = glib::markup_escape_text(&summary.subtitle);
        widgets.summary_subtitle.set_markup(&match &summary.command {
            Some(command) => format!("{subtitle} · <tt>{}</tt>", glib::markup_escape_text(command)),
            None => subtitle.to_string(),
        });
        widgets.summary_subtitle.set_visible(!summary.subtitle.is_empty());
        set_status(&widgets.summary_icon, summary.kind, summary.title);
        widgets
            .summary_icon
            .set_icon_name(Some(if summary.kind == StatusKind::Failed {
                "contr-failed-symbolic"
            } else {
                "contr-kubernetes-symbolic"
            }));
        widgets.summary_icon.set_visible(!summary.busy && !pending);
        widgets.summary_spinner.set_visible(summary.busy || pending);
        let starting = state == ClusterState::Starting;
        widgets.cancel.set_visible(starting);
        widgets.enabled_box.set_visible(!starting);
        widgets.enabled.block_signal(&widgets.enabled_handler);
        widgets.enabled.set_active(settings.enabled);
        widgets.enabled.unblock_signal(&widgets.enabled_handler);
        widgets.enabled.set_sensitive(!pending && !self.locked(ENABLED));

        let show_steps = matches!(state, ClusterState::Starting | ClusterState::Failed) && !cluster.steps.is_empty();
        widgets.progress.set_visible(starting);
        widgets.progress.set_fraction(model::progress(&cluster.steps));
        widgets.steps.set_visible(show_steps);
        widgets.steps.remove_all();
        if show_steps {
            for step in &cluster.steps {
                widgets.steps.append(&step_row(
                    &model::step_title(step, cluster, &self.home),
                    model::step_status(step),
                    &model::step_detail(step),
                ));
            }
        }
        widgets
            .note
            .set_visible(matches!(state, ClusterState::Starting | ClusterState::Failed));

        let ready = state == ClusterState::Ready;
        widgets.cluster_header.set_description(if starting {
            "Available when the node is ready"
        } else {
            ""
        });
        widgets
            .cluster_list
            .set_sensitive(!starting && state != ClusterState::Stopping);
        self.show_versions(widgets, &version);
        widgets.version.set_visible(!starting);
        widgets.version.set_sensitive(!pending && !self.locked(VERSION));
        widgets.api_server.set_visible(ready || starting);
        widgets
            .api_server
            .set_subtitle(if ready { &cluster.api_server } else { "—" });
        widgets.kubeconfig.set_visible(ready);
        widgets.kubeconfig.set_subtitle(&format!(
            "Merged into {} as context {}",
            format::home_relative(&cluster.kubeconfig, &self.home),
            cluster.context
        ));
        widgets.current.set_visible(ready || starting);
        widgets.current.set_title(&format!(
            "Make {} the current kubectl context",
            if cluster.context.is_empty() {
                "the cluster"
            } else {
                cluster.context.as_str()
            }
        ));
        widgets.current.set_subtitle(&model::context_subtitle(cluster));
        widgets.current.block_signal(&widgets.current_handler);
        widgets.current.set_active(self.is_current());
        widgets.current.unblock_signal(&widgets.current_handler);
        widgets.traefik.set_visible(!starting);
        widgets.traefik.set_sensitive(!pending && !self.locked(TRAEFIK));
        widgets.traefik.block_signal(&widgets.traefik_handler);
        widgets.traefik.set_active(settings.traefik);
        widgets.traefik.unblock_signal(&widgets.traefik_handler);

        widgets.pods_group.set_visible(ready && !cluster.system_pods.is_empty());
        widgets.pods.remove_all();
        for pod in &cluster.system_pods {
            let (kind, detail) = model::pod_view(pod);
            widgets
                .pods
                .append(&step_row(&pod.name, (kind, pod.phase.as_str()), &detail));
        }
        widgets
            .maintenance
            .set_visible(matches!(state, ClusterState::Ready | ClusterState::Failed));
        widgets.import.set_visible(ready);
        widgets.footer.set_visible(starting);
    }

    fn show_versions(&self, widgets: &KubernetesWidgets, version: &str) {
        let versions = &self.cluster.available_versions;
        let listed: Vec<String> = (0..widgets.versions.n_items())
            .filter_map(|index| widgets.versions.string(index).map(Into::into))
            .collect();
        widgets.version.block_signal(&widgets.version_handler);
        if &listed != versions {
            let names: Vec<&str> = versions.iter().map(String::as_str).collect();
            widgets.versions.splice(0, widgets.versions.n_items(), &names);
        }
        if let Some(index) = versions.iter().position(|known| known == version)
            && let Ok(index) = u32::try_from(index)
        {
            widgets.version.set_selected(index);
        }
        widgets.version.unblock_signal(&widgets.version_handler);
    }
}

fn step_row(title: &str, (kind, word): (StatusKind, &str), detail: &str) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(16)
        .margin_end(16)
        .build();
    if word == "Running" && kind == StatusKind::Paused {
        content.append(&adw::Spinner::new());
    } else {
        content.append(&status_icon(kind, word));
    }
    content.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .hexpand(true)
            .wrap(true)
            .build(),
    );
    content.append(
        &gtk::Label::builder()
            .label(detail)
            .xalign(1.0)
            .css_classes([if kind == StatusKind::Failed {
                "row-subtitle-error"
            } else {
                "dim-label"
            }])
            .build(),
    );
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

fn confirm(root: &adw::ToastOverlay, input: &Input, heading: &str, body: &str, action: &str, message: KubernetesMsg) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let message = std::cell::Cell::new(Some(message));
    dialog.connect_response(None, move |_, response| {
        if response == "confirm"
            && let Some(message) = message.take()
        {
            input.emit(message);
        } else {
            input.emit(KubernetesMsg::Refresh);
        }
    });
    dialog.present(Some(root));
}

fn import_dialog(root: &adw::ToastOverlay, input: &Input, images: &[String]) {
    let dialog = adw::AlertDialog::new(
        Some("Import Images"),
        Some("Pods in the cluster can use the images you import without pulling them from a registry."),
    );
    let list = boxed_list();
    let mut checks = Vec::new();
    for image in images {
        let check = gtk::CheckButton::builder().valign(gtk::Align::Center).build();
        let row = adw::ActionRow::builder()
            .title(image)
            .activatable_widget(&check)
            .build();
        row.add_prefix(&check);
        list.append(&row);
        checks.push((image.clone(), check));
    }
    if images.is_empty() {
        dialog.set_body("The engine has no tagged images to import.");
        dialog.add_response("cancel", "Close");
    } else {
        dialog.add_response("cancel", "Cancel");
        dialog.set_extra_child(Some(
            &gtk::ScrolledWindow::builder()
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(320)
                .child(&list)
                .build(),
        ));
        dialog.add_response("import", "Import");
        dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    }
    dialog.set_close_response("cancel");
    let input = input.clone();
    dialog.connect_response(Some("import"), move |_, _| {
        let chosen: Vec<String> = checks
            .iter()
            .filter(|(_, check)| check.is_active())
            .map(|(image, _)| image.clone())
            .collect();
        if !chosen.is_empty() {
            input.emit(KubernetesMsg::ImportImages(chosen));
        }
    });
    dialog.present(Some(root));
}

fn follow(daemon: &Daemon, sender: &ComponentSender<KubernetesPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(KubernetesCmd::Status(Box::new(status.borrow_and_update().clone())));
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
                    let cluster = async {
                        let Ok(stream) = client.kubernetes().watch_cluster(WatchClusterRequest {}).await else {
                            return;
                        };
                        let mut stream = stream.into_inner();
                        while let Ok(Some(response)) = stream.message().await {
                            if let Some(cluster) = response.cluster {
                                out.emit(KubernetesCmd::Cluster(Box::new(cluster)));
                            }
                        }
                    };
                    let settings = async {
                        let Ok(stream) = client.settings().watch_settings(WatchSettingsRequest {}).await else {
                            return;
                        };
                        let mut stream = stream.into_inner();
                        while let Ok(Some(response)) = stream.message().await {
                            if let Some(state) = response.state {
                                out.emit(KubernetesCmd::Settings(Box::new(state)));
                            }
                        }
                    };
                    tokio::join!(cluster, settings);
                }
            }))
            .drop_on_shutdown()
    });
}
