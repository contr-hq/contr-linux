use contr_core::product;
use contr_daemon_ui::{Daemon, SettingsHandle, Status, describe, failure, remediation_label, run_remediation};
use contr_sdk::Error;
use contr_sdk::api::common::v1::{Progress, Remediation};
use contr_sdk::api::diagnostics::v1::{Check, RemediationOutcome, RunChecksRequest};
use contr_sdk::api::engine::v1::EngineState;
use contr_ui_common::session;
use contr_ui_common::widgets::{StatusKind, boxed_list, status_icon};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};
use relm4::{Component, ComponentParts, ComponentSender};

use crate::model::{self, EngineView, Finish, Fixes, Steps};

type Input = relm4::Sender<OnboardingMsg>;

/// First-run setup: the engine the daemon manages, the fixes that make it usable, and the log out some of them need.
pub struct OnboardingPage {
    status: Status,
    checks: Vec<Check>,
    /// The connection and engine state the last checks ran for.
    checked_for: Option<(u64, EngineState)>,
    fixing: Option<Remediation>,
    steps: Steps,
    finish: Option<Finish>,
    release_check_note: Option<String>,
}

#[derive(Debug)]
pub enum OnboardingMsg {
    Fix(Remediation),
    LogOut,
    Continue,
}

#[derive(Debug)]
pub enum OnboardingOutput {
    /// The user finished or skipped setup.
    Done,
}

#[doc(hidden)]
#[derive(Debug)]
pub enum OnboardingCmd {
    Status(Box<Status>),
    Checks(Result<Vec<Check>, String>),
    ReleaseCheckNote(Option<String>),
    Progress(Progress),
    /// `Err(None)`: the user dismissed the fix's prompt.
    Fixed(Result<RemediationOutcome, Option<String>>),
}

pub struct OnboardingWidgets {
    stack: gtk::Stack,
    description: gtk::Label,
    engines: gtk::ListBox,
    release_check: gtk::Label,
    fix_title: gtk::Label,
    progress: gtk::ProgressBar,
    fix_steps: gtk::ListBox,
    finish_title: gtk::Label,
    finish_description: gtk::Label,
    finish_steps: gtk::ListBox,
    log_out: gtk::Button,
    later: gtk::Button,
    finish_note: gtk::Label,
}

impl Component for OnboardingPage {
    type CommandOutput = OnboardingCmd;
    type Input = OnboardingMsg;
    type Output = OnboardingOutput;
    type Init = Daemon;
    type Root = adw::ToastOverlay;
    type Widgets = OnboardingWidgets;

    fn init_root() -> Self::Root {
        adw::ToastOverlay::new()
    }

    fn init(daemon: Daemon, root: Self::Root, sender: ComponentSender<Self>) -> ComponentParts<Self> {
        let input = sender.input_sender();

        let description = text("");
        let engines = boxed_list();
        let continue_button = pill("Continue", true);
        continue_button.set_margin_top(12);
        let continue_input = input.clone();
        continue_button.connect_clicked(move |_| continue_input.emit(OnboardingMsg::Continue));
        let choose = column();
        choose.append(&app_icon());
        choose.append(&title("Choose an Engine"));
        choose.append(&description);
        choose.append(
            &gtk::Label::builder()
                .label("Engine on this computer")
                .xalign(0.0)
                .margin_top(18)
                .css_classes(["heading"])
                .build(),
        );
        choose.append(&engines);
        choose.append(&continue_button);
        let release_check = text("");
        release_check.add_css_class("dim-label");
        release_check.add_css_class("caption");
        release_check.set_visible(false);
        choose.append(&release_check);

        let fix_title = title("");
        let progress = gtk::ProgressBar::builder().margin_top(12).build();
        let fix_steps = boxed_list();
        let fixing = column();
        fixing.append(&app_icon());
        fixing.append(&fix_title);
        fixing.append(&text(
            "Your system may ask for your password. The fix goes on if you close Contr.",
        ));
        fixing.append(&progress);
        fixing.append(&fix_steps);

        let finish_title = title("");
        let finish_description = text("");
        let finish_steps = boxed_list();
        let save_first = gtk::Box::builder()
            .spacing(10)
            .css_classes(["card", "tinted-accent", "note-card"])
            .build();
        save_first.append(&gtk::Image::from_icon_name("dialog-information-symbolic"));
        save_first.append(
            &gtk::Label::builder()
                .label("Save your work in other apps first.")
                .xalign(0.0)
                .wrap(true)
                .build(),
        );
        let log_out = pill("Log Out…", true);
        let log_out_input = input.clone();
        log_out.connect_clicked(move |_| log_out_input.emit(OnboardingMsg::LogOut));
        let later = pill("Later", false);
        let later_input = input.clone();
        later.connect_clicked(move |_| later_input.emit(OnboardingMsg::Continue));
        let finish_note = text("Until you log in again, Contr can’t connect to the engine.");
        finish_note.add_css_class("dim-label");
        finish_note.add_css_class("caption");
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .margin_top(12)
            .build();
        buttons.append(&log_out);
        buttons.append(&later);
        let finished = column();
        finished.append(
            &gtk::Image::builder()
                .icon_name("object-select-symbolic")
                .pixel_size(32)
                .halign(gtk::Align::Center)
                .css_classes(["finish-icon"])
                .build(),
        );
        finished.append(&finish_title);
        finished.append(&finish_description);
        finished.append(&finish_steps);
        finished.append(&save_first);
        finished.append(&buttons);
        finished.append(&finish_note);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .build();
        stack.add_named(&scrolled(&choose), Some("choose"));
        stack.add_named(&scrolled(&fixing), Some("fixing"));
        stack.add_named(&scrolled(&finished), Some("finish"));

        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
        view.set_content(Some(&stack));
        root.set_child(Some(&view));

        follow_status(&daemon, &sender);

        let model = OnboardingPage {
            status: daemon.status(),
            checks: Vec::new(),
            checked_for: None,
            fixing: None,
            steps: Steps::default(),
            finish: None,
            release_check_note: None,
        };
        let mut widgets = OnboardingWidgets {
            stack,
            description,
            engines,
            release_check,
            fix_title,
            progress,
            fix_steps,
            finish_title,
            finish_description,
            finish_steps,
            log_out,
            later,
            finish_note,
        };
        model.show(&mut widgets, &sender);
        ComponentParts { model, widgets }
    }

    fn update_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: OnboardingMsg,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            OnboardingMsg::Fix(remediation) => self.fix(&sender, remediation),
            OnboardingMsg::LogOut => {
                let root = root.clone();
                glib::spawn_future_local(async move {
                    if session::log_out().await.is_err() {
                        root.add_toast(adw::Toast::new("Log out from the system menu to finish"));
                    }
                });
            }
            OnboardingMsg::Continue => {
                let _ = sender.output(OnboardingOutput::Done);
            }
        }
        self.show(widgets, &sender);
    }

    fn update_cmd_with_view(
        &mut self,
        widgets: &mut Self::Widgets,
        message: OnboardingCmd,
        sender: ComponentSender<Self>,
        root: &Self::Root,
    ) {
        match message {
            OnboardingCmd::Status(status) => {
                self.status = *status;
                let key = (self.status.connection, self.status.engine);
                if self.checked_for != Some(key) && self.status.client().is_some() {
                    self.checked_for = Some(key);
                    self.run_checks(&sender);
                    self.load_release_check_note(&sender);
                }
            }
            OnboardingCmd::ReleaseCheckNote(note) => self.release_check_note = note,
            OnboardingCmd::Checks(Ok(checks)) => self.checks = checks,
            OnboardingCmd::Checks(Err(message)) => root.add_toast(adw::Toast::new(&message)),
            OnboardingCmd::Progress(progress) => self.steps.push(progress),
            OnboardingCmd::Fixed(result) => {
                self.fixing = None;
                match result {
                    Ok(outcome) => {
                        self.finish = model::finish(outcome);
                        if self.finish.is_none() {
                            root.add_toast(adw::Toast::new("Fixed"));
                            self.run_checks(&sender);
                        }
                    }
                    Err(Some(message)) => root.add_toast(adw::Toast::new(&message)),
                    Err(None) => {}
                }
            }
        }
        self.show(widgets, &sender);
    }
}

impl OnboardingPage {
    fn run_checks(&self, sender: &ComponentSender<Self>) {
        let Some(client) = self.status.client().cloned() else {
            return;
        };
        sender.oneshot_command(async move {
            let result = client.diagnostics().run_checks(RunChecksRequest {}).await;
            OnboardingCmd::Checks(
                result
                    .map(|response| response.into_inner().checks)
                    .map_err(|status| describe(&Error::from(status))),
            )
        });
    }

    fn load_release_check_note(&self, sender: &ComponentSender<Self>) {
        let Some(client) = self.status.client().cloned() else {
            return;
        };
        sender.oneshot_command(async move {
            let state = SettingsHandle::new(client).get().await;
            OnboardingCmd::ReleaseCheckNote(state.ok().and_then(|state| model::release_check_note(&state)))
        });
    }

    fn fix(&mut self, sender: &ComponentSender<Self>, remediation: Remediation) {
        let Some(client) = self.status.client().cloned() else {
            return;
        };
        self.fixing = Some(remediation.clone());
        self.steps = Steps::default();
        let out = sender.command_sender().clone();
        relm4::spawn(async move {
            let result = run_remediation(client, &remediation, |progress| {
                out.emit(OnboardingCmd::Progress(progress));
            })
            .await
            .map_err(|error| failure(&error));
            out.emit(OnboardingCmd::Fixed(result));
        });
    }

    fn show(&self, widgets: &mut OnboardingWidgets, sender: &ComponentSender<Self>) {
        let input = sender.input_sender();
        let page = if self.fixing.is_some() {
            "fixing"
        } else if self.finish.is_some() {
            "finish"
        } else {
            "choose"
        };
        widgets.stack.set_visible_child_name(page);

        let engine = model::engine_view(&self.status);
        let fixes = model::fixes(&self.checks);
        widgets.description.set_label(if engine.is_some() {
            "Contr manages a container engine that’s already on this computer. This is the one it found."
        } else {
            "Contr manages a container engine on this computer, and didn’t find one yet."
        });
        widgets
            .release_check
            .set_label(self.release_check_note.as_deref().unwrap_or_default());
        widgets.release_check.set_visible(self.release_check_note.is_some());
        widgets.engines.remove_all();
        if let Some(engine) = &engine {
            widgets.engines.append(&engine_row(engine, &fixes, input));
        }
        if let Some(install) = &fixes.install {
            widgets.engines.append(&install_row(install, input));
        } else if engine.is_none() {
            widgets.engines.append(&row_content(
                &gtk::Image::from_icon_name("dialog-warning-symbolic"),
                "No engine found",
                "Contr didn’t find Docker Engine or Podman on this computer.",
            ));
        }

        if let Some(fixing) = &self.fixing {
            widgets.fix_title.set_label(model::fix_title(fixing));
            match self.steps.fraction {
                Some(fraction) => widgets.progress.set_fraction(f64::from(fraction)),
                None => widgets.progress.pulse(),
            }
            fill_steps(&widgets.fix_steps, &self.steps.messages, true);
        }

        if let Some(finish) = self.finish {
            let (title, description) = match finish {
                Finish::LogOut => (
                    "Log Out to Finish",
                    "Your new access to the engine applies to sessions that start after this, so log out and back \
                     in once.",
                ),
                Finish::Restart => ("Restart to Finish", "The fix takes effect after the computer restarts."),
            };
            widgets.finish_title.set_label(title);
            widgets.finish_description.set_label(description);
            fill_steps(&widgets.finish_steps, &self.steps.messages, false);
            widgets.finish_steps.set_visible(!self.steps.messages.is_empty());
            widgets.log_out.set_visible(finish == Finish::LogOut);
            widgets.finish_note.set_visible(finish == Finish::LogOut);
            widgets
                .later
                .set_label(if finish == Finish::LogOut { "Later" } else { "Continue" });
        }
    }
}

fn engine_row(engine: &EngineView, fixes: &Fixes, input: &Input) -> gtk::ListBoxRow {
    let header = gtk::Box::builder().spacing(12).build();
    header.append(&status_icon(engine.kind, engine.state));
    header.append(&titles(&engine.title, &engine.subtitle));
    if !engine.state.is_empty() {
        let state = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
        if engine.ready() {
            state.add_css_class("dim-label");
        } else {
            state.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            state.add_css_class("warning");
        }
        state.append(&gtk::Label::new(Some(engine.state)));
        header.append(&state);
    }

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&header);
    if !engine.ready() {
        let details = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_start(28)
            .build();
        if let Some(explanation) = engine.explanation {
            details.append(&gtk::Label::builder().label(explanation).xalign(0.0).wrap(true).build());
        }
        if !fixes.access.is_empty() {
            details.append(&fix_buttons(&fixes.access, input));
        }
        if fixes.grants_root() {
            let warning = gtk::Box::builder()
                .spacing(10)
                .css_classes(["card", "tinted-warning", "note-card"])
                .build();
            warning.append(&gtk::Image::from_icon_name("dialog-warning-symbolic"));
            warning.append(
                &gtk::Label::builder()
                    .label(
                        "Anyone in the docker group can get full root access to this computer. After this you’ll \
                         have to log out and back in before Contr can connect.",
                    )
                    .xalign(0.0)
                    .wrap(true)
                    .build(),
            );
            details.append(&warning);
        }
        if details.first_child().is_some() {
            content.append(&details);
        }
    }
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

fn fix_buttons(fixes: &[Remediation], input: &Input) -> gtk::Box {
    let buttons = gtk::Box::builder().spacing(12).build();
    for (index, fix) in fixes.iter().enumerate() {
        let button = gtk::Button::with_label(&remediation_label(fix));
        if index == 0 {
            button.add_css_class("suggested-action");
        }
        let input = input.clone();
        let remediation = fix.clone();
        button.connect_clicked(move |_| input.emit(OnboardingMsg::Fix(remediation.clone())));
        buttons.append(&button);
    }
    if fixes.iter().any(|fix| fix.requires_elevation) {
        buttons.append(
            &gtk::Label::builder()
                .label("Asks for your password")
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    buttons
}

fn install_row(install: &Remediation, input: &Input) -> gtk::ListBoxRow {
    let row = row_content(
        &gtk::Image::from_icon_name("list-add-symbolic"),
        "Install an Engine",
        "Installs the container engine packages from your distribution. Needs your password.",
    );
    let button = gtk::Button::builder()
        .label("Install…")
        .valign(gtk::Align::Center)
        .build();
    let input = input.clone();
    let remediation = install.clone();
    button.connect_clicked(move |_| input.emit(OnboardingMsg::Fix(remediation.clone())));
    if let Some(content) = row.child().and_downcast::<gtk::Box>() {
        content.append(&button);
    }
    row
}

fn row_content(icon: &gtk::Image, title: &str, subtitle: &str) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(icon);
    content.append(&titles(title, subtitle));
    gtk::ListBoxRow::builder().child(&content).activatable(false).build()
}

fn titles(title: &str, subtitle: &str) -> gtk::Box {
    let titles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .build();
    titles.append(&gtk::Label::builder().label(title).xalign(0.0).wrap(true).build());
    if !subtitle.is_empty() {
        titles.append(
            &gtk::Label::builder()
                .label(subtitle)
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label", "caption"])
                .build(),
        );
    }
    titles
}

/// One row per step; while `running`, the last step shows a spinner instead of a check mark.
fn fill_steps(list: &gtk::ListBox, steps: &[String], running: bool) {
    list.remove_all();
    list.set_visible(!steps.is_empty());
    for (index, step) in steps.iter().enumerate() {
        let current = running && index + 1 == steps.len();
        let icon: gtk::Widget = if current {
            adw::Spinner::new().upcast()
        } else {
            status_icon(StatusKind::Running, "Done").upcast()
        };
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&icon);
        content.append(&gtk::Label::builder().label(step).xalign(0.0).wrap(true).build());
        list.append(&gtk::ListBoxRow::builder().child(&content).activatable(false).build());
    }
}

fn column() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .build()
}

fn scrolled(child: &gtk::Box) -> gtk::ScrolledWindow {
    let clamp = adw::Clamp::builder()
        .maximum_size(640)
        .margin_top(12)
        .margin_bottom(36)
        .margin_start(16)
        .margin_end(16)
        .child(child)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build()
}

fn app_icon() -> gtk::Image {
    gtk::Image::builder()
        .icon_name(product::APP_ID)
        .pixel_size(64)
        .halign(gtk::Align::Center)
        .build()
}

fn title(label: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(label)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["title-1"])
        .build()
}

fn text(label: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(label)
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build()
}

fn pill(label: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}

fn follow_status(daemon: &Daemon, sender: &ComponentSender<OnboardingPage>) {
    let mut status = daemon.subscribe();
    sender.command(move |out, shutdown| {
        shutdown
            .register(async move {
                loop {
                    out.emit(OnboardingCmd::Status(Box::new(status.borrow_and_update().clone())));
                    if status.changed().await.is_err() {
                        return;
                    }
                }
            })
            .drop_on_shutdown()
    });
}
