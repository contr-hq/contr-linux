use std::path::{Path, PathBuf};
use std::time::SystemTime;

use contr_core::{paths, product};
use contr_daemon_ui::{
    Daemon, EngineAction, Status, describe, engine_kind, failure, remediation_label, run_remediation,
};
use contr_sdk::api::common::v1::Remediation;
use contr_sdk::api::containers::v1::{ContainerState, ListContainersRequest, PruneContainersRequest};
use contr_sdk::api::daemon::v1::Capability;
use contr_sdk::api::diagnostics::v1::{
    Check, CheckStatus, CreateBundleRequest, RunChecksRequest, create_bundle_response,
};
use contr_sdk::api::engine::v1::{EngineState, ResetRequest, ResetScope, reset_response};
use contr_sdk::api::images::v1::{ListImagesRequest, PruneImagesRequest};
use contr_sdk::api::settings::v1::ResetSettingsRequest;
use contr_sdk::{Client, Error};
use contr_ui_common::widgets::{GroupHeader, boxed_list, page_column, set_status, status_icon};
use contr_ui_common::{format, session};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{Component, ComponentParts, ComponentSender};
use tokio::io::AsyncWriteExt;

use crate::model::{self, Problem};

type Input = relm4::Sender<TroubleshootMsg>;

/// Journal lines the Logs section shows.
const LOG_LINES: &str = "200";

const BUNDLE_DESCRIPTION: &str = "A zip of versions, settings, check results and recent logs for support. Credentials, \
                                  your name, home folder and computer name are replaced.";

/// The Troubleshoot section: what keeps the engine from working, the fixes the daemon offers, logs, and recovery.
pub struct TroubleshootPage {
    daemon: Daemon,
    status: Status,
    checks: Vec<Check>,
    checked_at: Option<SystemTime>,
    /// The connection and engine state the last checks ran for.
    checked_for: Option<(u64, EngineState)>,
    checking: bool,
    fixing: Option<String>,
    logout_needed: bool,
    restart_needed: bool,
    log_source: u32,
    /// The last step of the diagnostics bundle being saved.
    bundling: Option<String>,
}

#[derive(Debug)]
pub enum TroubleshootMsg {
    RunChecks,
    CreateBundle,
    SaveBundle(PathBuf),
    Fix(Remediation),
    LogOut,
    LogSource(u32),
    CleanUp,
    ConfirmCleanUp,
    ResetSettings,
    ConfirmResetSettings,
    ResetEngine,
    ConfirmResetEngine,
}

#[derive(Debug)]
pub enum TroubleshootOutput {
    /// Problems for the sidebar badge.
    Problems(usize),
}

#[doc(hidden)]
#[derive(Debug)]
pub enum TroubleshootCmd {
    Status(Box<Status>),
    Checks(Result<Vec<Check>, String>),
    FixProgress(String),
    /// `Err(None)`: the user dismissed the fix's prompt.
    Fixed(Result<(bool, bool), Option<String>>),
    Logs(String),
    CleanUpPlan(Result<(usize, usize, u64), String>),
    BundleProgress(String),
    BundleSaved(Result<PathBuf, String>),
    Done(Result<String, String>),
}

pub struct TroubleshootWidgets {
    alert: gtk::Box,
    alert_title: gtk::Label,
    alert_detail: gtk::Label,
    alert_code: gtk::Label,
    alert_actions: gtk::Box,
    checks_header: GroupHeader,
    run_again: gtk::Button,
    checks: gtk::ListBox,
    logs: gtk::TextBuffer,
    reset_engine: adw::ActionRow,
    bundle: adw::ActionRow,
    bundle_button: gtk::Button,
}

impl Component for TroubleshootPage {
    type CommandOutput = TroubleshootCmd;
    type Input = TroubleshootMsg;
    type Output = TroubleshootOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = TroubleshootWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Troubleshoot", ""))
            .build();

        let alert_icon = status_icon(contr_ui_common::widgets::StatusKind::Failed, "Problem");
        alert_icon.set_valign(gtk::Align::Center);
        alert_icon.set_pixel_size(22);
        alert_icon.add_css_class("alert-icon");
        let alert_title = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .css_classes(["title-4"])
            .build();
        let alert_detail = gtk::Label::builder().xalign(0.0).wrap(true).build();
        let alert_code = gtk::Label::builder()
            .xalign(0.0)
            .selectable(true)
            .css_classes(["dim-label", "mono", "caption"])
            .build();
        let alert_text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .hexpand(true)
            .build();
        alert_text.append(&alert_title);
        alert_text.append(&alert_detail);
        alert_text.append(&alert_code);
        let alert_actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Center)
            .build();
        let alert = gtk::Box::builder()
            .spacing(14)
            .visible(false)
            .css_classes(["card", "tinted-error", "pull-head"])
            .build();
        alert.append(&alert_icon);
        alert.append(&alert_text);
        alert.append(&alert_actions);

        let checks_header = GroupHeader::new("System checks", "");
        let run_again = gtk::Button::builder()
            .label("Run Again")
            .valign(gtk::Align::End)
            .css_classes(["raised-button"])
            .build();
        checks_header.suffix.append(&run_again);
        let input = sender.input_sender().clone();
        run_again.connect_clicked(move |_| input.emit(TroubleshootMsg::RunChecks));
        let checks = boxed_list();
        let checks_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        checks_section.append(&checks_header.root);
        checks_section.append(&checks);

        let logs_header = GroupHeader::new("Logs", "");
        let source = gtk::DropDown::from_strings(&[&format!("{} daemon", product::DISPLAY_NAME), "Engine"]);
        source.set_valign(gtk::Align::End);
        logs_header.suffix.append(
            &gtk::Label::builder()
                .label("Component")
                .css_classes(["dim-label"])
                .build(),
        );
        logs_header.suffix.append(&source);
        let input = sender.input_sender().clone();
        source.connect_selected_notify(move |drop_down| input.emit(TroubleshootMsg::LogSource(drop_down.selected())));
        let logs = gtk::TextBuffer::new(None);
        let logs_view = gtk::TextView::builder()
            .buffer(&logs)
            .editable(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .css_classes(["console-view"])
            .build();
        let logs_scroller = gtk::ScrolledWindow::builder()
            .child(&logs_view)
            .min_content_height(160)
            .max_content_height(260)
            .propagate_natural_height(true)
            .css_classes(["console"])
            .build();
        let logs_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        logs_section.append(&logs_header.root);
        logs_section.append(&logs_scroller);

        let help_header = GroupHeader::new("Get help", "");
        let help = boxed_list();
        let bundle = help_row(
            "Create a diagnostics bundle",
            BUNDLE_DESCRIPTION,
            "Create…",
            Some((sender.input_sender().clone(), || TroubleshootMsg::CreateBundle)),
        );
        let bundle_button = bundle
            .activatable_widget()
            .and_downcast::<gtk::Button>()
            .unwrap_or_default();
        help.append(&bundle);
        help.append(&help_row(
            "Clean up engine data",
            "Removes stopped containers and images no container uses. Shows what goes and asks first.",
            "Clean Up Engine Data…",
            Some((sender.input_sender().clone(), || TroubleshootMsg::CleanUp)),
        ));
        help.append(&help_row(
            &format!("Reset {} settings", product::DISPLAY_NAME),
            "Your engine, images and volumes aren’t touched",
            "Reset…",
            Some((sender.input_sender().clone(), || TroubleshootMsg::ResetSettings)),
        ));
        let reset_engine = help_row(
            "Reset the engine",
            "Deletes every image, container, volume and the build cache, and sets the engine up again.",
            "Reset Engine…",
            Some((sender.input_sender().clone(), || TroubleshootMsg::ResetEngine)),
        );
        reset_engine.set_visible(false);
        help.append(&reset_engine);
        let help_section = gtk::Box::new(gtk::Orientation::Vertical, 0);
        help_section.append(&help_header.root);
        help_section.append(&help);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(28)
            .build();
        for widget in [
            alert.upcast_ref::<gtk::Widget>(),
            checks_section.upcast_ref(),
            logs_section.upcast_ref(),
            help_section.upcast_ref(),
        ] {
            column.append(widget);
        }
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&page_column(&column)));
        root.set_child(Some(&toolbar));

        follow(&daemon, &sender);
        let model = TroubleshootPage {
            status: daemon.status(),
            daemon,
            checks: Vec::new(),
            checked_at: None,
            checked_for: None,
            checking: false,
            fixing: None,
            logout_needed: false,
            restart_needed: false,
            log_source: 0,
            bundling: None,
        };
        let widgets = TroubleshootWidgets {
            alert,
            alert_title,
            alert_detail,
            alert_code,
            alert_actions,
            checks_header,
            run_again,
            checks,
            logs,
            reset_engine,
            bundle,
            bundle_button,
        };
        sender.input(TroubleshootMsg::LogSource(0));
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: TroubleshootMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            TroubleshootMsg::RunChecks => self.run_checks(&sender),
            TroubleshootMsg::CreateBundle => {
                let input = sender.input_sender().clone();
                let date = glib::DateTime::now_local()
                    .and_then(|now| now.format("%Y-%m-%d"))
                    .map(String::from)
                    .unwrap_or_default();
                let dialog = gtk::FileDialog::builder()
                    .title("Save Diagnostics Bundle")
                    .initial_name(model::bundle_name(&date))
                    .build();
                dialog.save(
                    root.root().and_downcast_ref::<gtk::Window>(),
                    gio::Cancellable::NONE,
                    move |result| {
                        if let Some(path) = result.ok().and_then(|file| file.path()) {
                            input.emit(TroubleshootMsg::SaveBundle(path));
                        }
                    },
                );
            }
            TroubleshootMsg::SaveBundle(path) => self.save_bundle(&sender, path),
            TroubleshootMsg::Fix(remediation) => self.fix(&sender, remediation),
            TroubleshootMsg::LogOut => log_out(root),
            TroubleshootMsg::LogSource(source) => {
                self.log_source = source;
                let unit = self.engine_unit();
                sender.oneshot_command(async move { TroubleshootCmd::Logs(read_logs(source, unit).await) });
            }
            TroubleshootMsg::CleanUp => {
                let Some(client) = self.client() else { return };
                sender.oneshot_command(async move {
                    TroubleshootCmd::CleanUpPlan(clean_up_plan(&client).await.map_err(|error| describe(&error)))
                });
            }
            TroubleshootMsg::ConfirmCleanUp => self.call(&sender, |client| async move {
                let containers = client
                    .containers()
                    .prune_containers(PruneContainersRequest {})
                    .await?
                    .into_inner();
                let images = client
                    .images()
                    .prune_images(PruneImagesRequest { all: true })
                    .await?
                    .into_inner();
                Ok(format!(
                    "Removed {} containers and {} images · freed {}",
                    containers.deleted_ids.len(),
                    images.deleted.len(),
                    format::bytes(containers.reclaimed_bytes + images.reclaimed_bytes)
                ))
            }),
            TroubleshootMsg::ResetSettings => confirm(
                root,
                &format!("Reset {} Settings?", product::DISPLAY_NAME),
                "Every setting you changed goes back to its default; settings your organization manages stay. Your engine, images and volumes aren’t touched.",
                "Reset",
                sender.input_sender(),
                TroubleshootMsg::ConfirmResetSettings,
            ),
            TroubleshootMsg::ConfirmResetSettings => self.call(&sender, |client| async move {
                let response = client
                    .settings()
                    .reset_settings(ResetSettingsRequest {})
                    .await?
                    .into_inner();
                Ok(if response.restart_required.is_empty() {
                    "Settings reset".to_owned()
                } else {
                    "Settings reset; some take effect when the engine restarts".to_owned()
                })
            }),
            TroubleshootMsg::ResetEngine => confirm(
                root,
                "Reset the Engine?",
                "Every image, container and volume is deleted, with the build cache. This can’t be undone.",
                "Reset Engine",
                sender.input_sender(),
                TroubleshootMsg::ConfirmResetEngine,
            ),
            TroubleshootMsg::ConfirmResetEngine => self.call(&sender, |client| async move {
                let request = ResetRequest {
                    scope: ResetScope::Factory.into(),
                };
                let mut stream = client.engine().reset(request).await?.into_inner();
                while let Some(response) = stream.message().await? {
                    if let Some(reset_response::Event::State(_)) = response.event {
                        break;
                    }
                }
                Ok("The engine was reset".to_owned())
            }),
        }
        self.show(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: TroubleshootCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            TroubleshootCmd::Status(status) => {
                self.status = *status;
                let key = (self.status.connection, self.status.engine);
                if self.checked_for != Some(key) && self.status.client().is_some() {
                    self.checked_for = Some(key);
                    self.run_checks(&sender);
                }
            }
            TroubleshootCmd::Checks(result) => {
                self.checking = false;
                match result {
                    Ok(checks) => {
                        self.checks = checks;
                        self.checked_at = Some(SystemTime::now());
                    }
                    Err(message) => root.add_toast(adw::Toast::new(&message)),
                }
            }
            TroubleshootCmd::FixProgress(message) => self.fixing = Some(message),
            TroubleshootCmd::Fixed(result) => {
                self.fixing = None;
                match result {
                    Ok((logout, restart)) => {
                        self.logout_needed |= logout;
                        self.restart_needed |= restart;
                        root.add_toast(adw::Toast::new(if logout {
                            "Done. Log out and back in to finish."
                        } else if restart {
                            "Done. Restart the computer to finish."
                        } else {
                            "Fixed"
                        }));
                        self.run_checks(&sender);
                    }
                    Err(Some(message)) => root.add_toast(adw::Toast::new(&message)),
                    Err(None) => {}
                }
            }
            TroubleshootCmd::Logs(text) => {
                widgets.logs.set_text(&text);
                return;
            }
            TroubleshootCmd::BundleProgress(step) => self.bundling = Some(step),
            TroubleshootCmd::BundleSaved(result) => {
                self.bundling = None;
                match result {
                    Ok(path) => root.add_toast(saved_toast(root, &path)),
                    Err(message) => root.add_toast(adw::Toast::new(&message)),
                }
            }
            TroubleshootCmd::CleanUpPlan(Ok((containers, images, size))) => {
                let body = format!(
                    "{containers} stopped container{} and {images} image{} no container uses are removed ({}). Volumes stay.",
                    if containers == 1 { "" } else { "s" },
                    if images == 1 { "" } else { "s" },
                    format::bytes(size)
                );
                confirm(
                    root,
                    "Clean Up Engine Data?",
                    &body,
                    "Clean Up",
                    sender.input_sender(),
                    TroubleshootMsg::ConfirmCleanUp,
                );
            }
            TroubleshootCmd::CleanUpPlan(Err(message)) | TroubleshootCmd::Done(Err(message)) => {
                root.add_toast(adw::Toast::new(&message));
            }
            TroubleshootCmd::Done(Ok(message)) => root.add_toast(adw::Toast::new(&message)),
        }
        self.show(widgets, &sender);
    }
}

impl TroubleshootPage {
    fn client(&self) -> Option<Client> {
        self.status.client().cloned()
    }

    fn run_checks(&mut self, sender: &ComponentSender<Self>) {
        let Some(client) = self.client() else {
            self.checks.clear();
            return;
        };
        self.checking = true;
        sender.oneshot_command(async move {
            let result = client.diagnostics().run_checks(RunChecksRequest {}).await;
            TroubleshootCmd::Checks(
                result
                    .map(|response| response.into_inner().checks)
                    .map_err(|status| describe(&Error::from(status))),
            )
        });
    }

    fn fix(&mut self, sender: &ComponentSender<Self>, remediation: Remediation) {
        let Some(client) = self.client() else { return };
        self.fixing = Some(format!("{}…", remediation_label(&remediation).trim_end_matches('…')));
        let out = sender.command_sender().clone();
        relm4::spawn(async move {
            let result = run_remediation(client, &remediation, |progress| {
                out.emit(TroubleshootCmd::FixProgress(progress.message));
            })
            .await
            .map(|outcome| (outcome.logout_required, outcome.restart_required))
            .map_err(|error| failure(&error));
            out.emit(TroubleshootCmd::Fixed(result));
        });
    }

    fn save_bundle(&mut self, sender: &ComponentSender<Self>, path: PathBuf) {
        let Some(client) = self.client() else { return };
        self.bundling = Some("Starting…".to_owned());
        let out = sender.command_sender().clone();
        relm4::spawn(async move {
            let result = write_bundle(client, &path, |step| out.emit(TroubleshootCmd::BundleProgress(step)))
                .await
                .map(|()| path);
            out.emit(TroubleshootCmd::BundleSaved(result));
        });
    }

    /// The systemd unit that runs the engine, judged from the engine description.
    fn engine_unit(&self) -> (bool, String) {
        match self.status.engine_info.as_ref().map(engine_kind) {
            Some("Rootless Docker") => (true, "docker.service".to_owned()),
            Some("Podman") => (true, "podman.service".to_owned()),
            _ => (false, "docker.service".to_owned()),
        }
    }

    fn show(&self, widgets: &mut TroubleshootWidgets, sender: &ComponentSender<Self>) {
        let problem = model::problem(&self.status, &self.checks);
        let _ = sender.output(TroubleshootOutput::Problems(model::problem_count(
            &self.status,
            &self.checks,
        )));
        widgets
            .alert
            .set_visible(problem.is_some() || self.logout_needed || self.restart_needed);
        self.show_problem(widgets, problem.as_ref(), sender);

        let ran = self.checked_at.map(|at| {
            let seconds = at
                .duration_since(SystemTime::UNIX_EPOCH)
                .ok()
                .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
                .unwrap_or_default();
            glib::DateTime::from_unix_local(seconds)
                .and_then(|time| time.format("%H:%M"))
                .map(String::from)
                .unwrap_or_default()
        });
        widgets.checks_header.set_description(&match (self.checking, ran) {
            (true, _) => "Checking…".to_owned(),
            (false, Some(ran)) => format!("Ran at {ran} · {}", model::tally(&self.checks)),
            (false, None) => "Not run yet".to_owned(),
        });
        widgets
            .run_again
            .set_sensitive(!self.checking && self.status.client().is_some());
        while let Some(child) = widgets.checks.first_child() {
            widgets.checks.remove(&child);
        }
        for check in &self.checks {
            widgets
                .checks
                .append(&check_row(check, sender.input_sender(), self.fixing.is_some()));
        }
        widgets.checks.set_visible(!self.checks.is_empty());
        widgets
            .reset_engine
            .set_visible(self.status.has(Capability::Reset) && self.status.engine != EngineState::Unspecified);
        widgets
            .bundle
            .set_subtitle(self.bundling.as_deref().unwrap_or(BUNDLE_DESCRIPTION));
        widgets
            .bundle_button
            .set_sensitive(self.bundling.is_none() && self.client().is_some());
    }

    fn show_problem(&self, widgets: &TroubleshootWidgets, problem: Option<&Problem>, sender: &ComponentSender<Self>) {
        while let Some(child) = widgets.alert_actions.first_child() {
            widgets.alert_actions.remove(&child);
        }
        let (title, detail) = match problem {
            Some(problem) => (problem.title.clone(), problem.detail.clone()),
            None if self.logout_needed => (
                "Log out to finish".to_owned(),
                "The fix applies to sessions that start after it. Log out and back in once.".to_owned(),
            ),
            None => (
                "Restart to finish".to_owned(),
                "The fix takes effect after the computer restarts.".to_owned(),
            ),
        };
        widgets.alert_title.set_label(&title);
        widgets.alert_detail.set_label(&detail);
        let code = problem.and_then(|problem| problem.code);
        widgets.alert_code.set_label(code.unwrap_or_default());
        widgets.alert_code.set_visible(code.is_some());

        if let Some(message) = &self.fixing {
            let busy = gtk::Box::builder().spacing(8).build();
            busy.append(&adw::Spinner::new());
            busy.append(
                &gtk::Label::builder()
                    .label(message)
                    .wrap(true)
                    .css_classes(["group-description"])
                    .build(),
            );
            widgets.alert_actions.append(&busy);
            return;
        }
        if self.logout_needed {
            let log_out = gtk::Button::builder()
                .label("Log Out…")
                .css_classes(["pill", "suggested-action"])
                .build();
            let input = sender.input_sender().clone();
            log_out.connect_clicked(move |_| input.emit(TroubleshootMsg::LogOut));
            widgets.alert_actions.append(&log_out);
            widgets.alert_actions.append(
                &gtk::Label::builder()
                    .label("Save your work first")
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
            return;
        }
        let remediations = problem
            .map(|problem| problem.remediations.as_slice())
            .unwrap_or_default();
        for (index, remediation) in remediations.iter().enumerate() {
            let button = gtk::Button::builder()
                .label(remediation_label(remediation))
                .css_classes(["pill"])
                .build();
            if index == 0 {
                button.add_css_class("suggested-action");
            }
            let input = sender.input_sender().clone();
            let fix = remediation.clone();
            button.connect_clicked(move |_| input.emit(TroubleshootMsg::Fix(fix.clone())));
            widgets.alert_actions.append(&button);
            if remediation.requires_elevation {
                widgets.alert_actions.append(
                    &gtk::Label::builder()
                        .label("Asks for your password")
                        .css_classes(["dim-label", "caption"])
                        .build(),
                );
            }
        }
        if problem.is_some_and(|problem| problem.remediations.is_empty()) && self.status.engine == EngineState::Failed {
            let retry = gtk::Button::builder().label("Try Again").css_classes(["pill"]).build();
            let daemon = self.daemon.clone();
            retry.connect_clicked(move |_| {
                let daemon = daemon.clone();
                glib::spawn_future_local(async move {
                    let _ = daemon.run(EngineAction::Start).await;
                });
            });
            widgets.alert_actions.append(&retry);
        }
    }

    fn call<F, Fut>(&self, sender: &ComponentSender<Self>, call: F)
    where
        F: FnOnce(Client) -> Fut + Send + 'static,
        Fut: Future<Output = Result<String, Error>> + Send,
    {
        let Some(client) = self.client() else { return };
        sender.oneshot_command(
            async move { TroubleshootCmd::Done(call(client).await.map_err(|error| describe(&error))) },
        );
    }
}

fn check_row(check: &Check, input: &relm4::Sender<TroubleshootMsg>, fixing: bool) -> gtk::ListBoxRow {
    let (kind, word) = model::check_status(check);
    let icon = gtk::Image::new();
    set_status(&icon, kind, word);
    let title = gtk::Label::builder()
        .label(&check.title)
        .xalign(0.0)
        .wrap(true)
        .hexpand(true)
        .build();
    let detail = gtk::Label::builder()
        .label(&check.detail)
        .xalign(1.0)
        .wrap(true)
        .natural_wrap_mode(gtk::NaturalWrapMode::None)
        .max_width_chars(60)
        .css_classes([
            "group-description",
            if check.status() == CheckStatus::Fail {
                "row-subtitle-error"
            } else {
                "dim-label"
            },
        ])
        .build();
    let content = gtk::Box::builder()
        .spacing(12)
        .css_classes(["container-row"])
        .margin_end(6)
        .build();
    content.append(&icon);
    content.append(&title);
    content.append(&detail);
    // A failing check's remediations are offered by the alert above the checks.
    let remediations = if check.status() == CheckStatus::Fail {
        &[][..]
    } else {
        check.remediations.as_slice()
    };
    for remediation in remediations {
        let button = gtk::Button::builder()
            .label(remediation_label(remediation))
            .valign(gtk::Align::Center)
            .sensitive(!fixing)
            .css_classes(["raised-button"])
            .build();
        let input = input.clone();
        let fix = remediation.clone();
        button.connect_clicked(move |_| input.emit(TroubleshootMsg::Fix(fix.clone())));
        content.append(&button);
    }
    let row = gtk::ListBoxRow::builder().child(&content).activatable(false).build();
    if check.status() == CheckStatus::Fail {
        row.add_css_class("tinted-error");
    }
    row
}

fn help_row(
    title: &str,
    subtitle: &str,
    action: &str,
    message: Option<(Input, fn() -> TroubleshootMsg)>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
    let destructive = action.starts_with("Reset");
    let button = gtk::Button::builder()
        .label(action)
        .valign(gtk::Align::Center)
        .css_classes([if destructive {
            "destructive-action"
        } else {
            "raised-button"
        }])
        .build();
    if let Some((input, message)) = message {
        button.connect_clicked(move |_| input.emit(message()));
    }
    row.add_suffix(&button);
    row.set_activatable_widget(Some(&button));
    row
}

fn confirm(
    parent: &adw::ToastOverlay,
    heading: &str,
    body: &str,
    action: &str,
    input: &relm4::Sender<TroubleshootMsg>,
    message: TroubleshootMsg,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", action)]);
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let input = input.clone();
    let message = std::cell::Cell::new(Some(message));
    dialog.connect_response(Some("confirm"), move |_, _| {
        if let Some(message) = message.take() {
            input.emit(message);
        }
    });
    dialog.present(Some(parent));
}

fn log_out(root: &adw::ToastOverlay) {
    let root = root.clone();
    glib::spawn_future_local(async move {
        if session::log_out().await.is_err() {
            root.add_toast(adw::Toast::new("Log out from the system menu to finish"));
        }
    });
}

/// Streams the daemon's bundle into a temporary file, then copies it to `path`, so a bundle that fails midway leaves
/// no zip there. A sandboxed client may write only `path` itself, so the temporary file is not beside it.
async fn write_bundle(client: Client, path: &Path, progress: impl Fn(String)) -> Result<(), String> {
    let temporary = std::env::temp_dir().join(format!("{}-diagnostics-{}.zip.part", product::SLUG, std::process::id()));
    let result = async {
        let mut file = tokio::fs::File::create(&temporary)
            .await
            .map_err(|error| error.to_string())?;
        let rpc = |status: tonic::Status| describe(&Error::from(status));
        let mut stream = client
            .diagnostics()
            .create_bundle(CreateBundleRequest {})
            .await
            .map_err(rpc)?
            .into_inner();
        while let Some(response) = stream.message().await.map_err(rpc)? {
            match response.event {
                Some(create_bundle_response::Event::Progress(step)) => progress(step.message),
                Some(create_bundle_response::Event::Chunk(bytes)) => {
                    file.write_all(&bytes).await.map_err(|error| error.to_string())?;
                }
                None => {}
            }
        }
        file.flush().await.map_err(|error| error.to_string())?;
        if let Err(error) = tokio::fs::copy(&temporary, path).await {
            let _ = tokio::fs::remove_file(path).await;
            return Err(error.to_string());
        }
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(&temporary).await;
    result
}

fn saved_toast(root: &adw::ToastOverlay, path: &Path) -> adw::Toast {
    let name = path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
    let toast = adw::Toast::builder()
        .title(format!("Saved {name}"))
        .button_label("Show in Files")
        .build();
    let root = root.clone();
    let file = gio::File::for_path(path);
    toast.connect_button_clicked(move |_| {
        gtk::FileLauncher::new(Some(&file)).open_containing_folder(
            root.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            |_| {},
        );
    });
    toast
}

/// Stopped containers, unused images and the bytes the images take.
async fn clean_up_plan(client: &Client) -> Result<(usize, usize, u64), Error> {
    let containers = client
        .containers()
        .list_containers(ListContainersRequest {})
        .await?
        .into_inner()
        .containers;
    let images = client
        .images()
        .list_images(ListImagesRequest {})
        .await?
        .into_inner()
        .images;
    let stopped = containers
        .iter()
        .filter(|container| container.state() != ContainerState::Running && container.state() != ContainerState::Paused)
        .count();
    let used: std::collections::HashSet<&str> = containers
        .iter()
        .filter(|container| container.state() == ContainerState::Running || container.state() == ContainerState::Paused)
        .map(|container| container.image_id.as_str())
        .collect();
    let unused: Vec<_> = images
        .iter()
        .filter(|image| !used.contains(image.id.as_str()))
        .collect();
    Ok((stopped, unused.len(), unused.iter().map(|image| image.size_bytes).sum()))
}

/// The last journal lines of the daemon (source 0) or the engine, as `HH:MM:SS name: message`.
async fn read_logs(source: u32, (user, engine_unit): (bool, String)) -> String {
    let (user, unit) = if source == 0 {
        (true, product::DAEMON_UNIT.to_owned())
    } else {
        (user, engine_unit)
    };
    let mut args = vec![
        "journalctl",
        "--no-pager",
        "-o",
        "json",
        "-n",
        LOG_LINES,
        "-u",
        unit.as_str(),
    ];
    if user {
        args.insert(1, "--user");
    }
    let output = command_output(&args).await;
    let lines: Vec<String> = output.lines().filter_map(journal_line).collect();
    if !lines.is_empty() {
        return lines.join("\n");
    }
    if source == 0
        && let Some(log) = paths::runtime_dir().map(|dir| dir.join(product::DAEMON_LOG_FILE))
        && let Ok(text) = std::fs::read_to_string(&log)
    {
        let lines: Vec<&str> = text.lines().collect();
        return lines[lines.len().saturating_sub(200)..].join("\n");
    }
    format!("No journal entries for {unit}.")
}

/// Standard output of a short command; empty when it cannot run.
async fn command_output(args: &[&str]) -> String {
    let Some((program, rest)) = args.split_first() else {
        return String::new();
    };
    match tokio::process::Command::new(program).args(rest).output().await {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => String::new(),
    }
}

fn journal_line(line: &str) -> Option<String> {
    let entry: serde_json::Value = serde_json::from_str(line).ok()?;
    let micros: i64 = entry["__REALTIME_TIMESTAMP"].as_str()?.parse().ok()?;
    let time = glib::DateTime::from_unix_local(micros / 1_000_000)
        .ok()?
        .format("%H:%M:%S")
        .ok()?;
    let name = entry["SYSLOG_IDENTIFIER"].as_str().unwrap_or("");
    let message = match &entry["MESSAGE"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(bytes) => {
            let bytes: Vec<u8> = bytes
                .iter()
                .filter_map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect();
            String::from_utf8_lossy(&bytes).into_owned()
        }
        _ => return None,
    };
    Some(format!("{time} {name}: {message}"))
}

fn follow(daemon: &Daemon, sender: &ComponentSender<TroubleshootPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(TroubleshootCmd::Status(Box::new(status.borrow_and_update().clone())));
                    if status.changed().await.is_err() {
                        return;
                    }
                }
            })
            .drop_on_shutdown()
    });
}
