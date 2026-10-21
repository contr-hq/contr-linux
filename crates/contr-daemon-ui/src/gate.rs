use std::cell::RefCell;
use std::rc::Rc;

use contr_sdk::api::common::v1::ErrorReason;
use contr_sdk::api::daemon::v1::BackendKind;
use contr_sdk::api::engine::v1::EngineState;
use contr_sdk::api::settings::v1::{EngineSettings, Settings};
use relm4::adw;
use relm4::gtk::{self, glib, prelude::*};

use crate::{Daemon, EngineAction, Link, SettingsHandle, Status, describe};

/// Window action that shows the Troubleshoot page.
pub const TROUBLESHOOT_ACTION: &str = "win.troubleshoot";
const START_WITH_DAEMON: &str = "engine.start_with_daemon";

/// Status page an engine page shows instead of its content while the engine is not running.
#[derive(Clone)]
pub struct EngineGate {
    page: adw::StatusPage,
    spinner: adw::Spinner,
    primary: gtk::Button,
    secondary: gtk::Button,
    autostart: gtk::CheckButton,
    autostart_handler: Rc<glib::SignalHandlerId>,
    hint: gtk::Label,
    error: Rc<RefCell<Option<String>>>,
    stack: Option<gtk::Stack>,
    daemon: Daemon,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Start,
    Troubleshoot,
}

#[derive(Debug, PartialEq, Eq)]
struct Presentation {
    title: &'static str,
    description: String,
    busy: bool,
    primary: Option<(&'static str, Command)>,
    secondary: Option<(&'static str, Command)>,
    autostart: bool,
    hint: Option<&'static str>,
}

impl EngineGate {
    /// A stack that shows `content` while the engine runs and a gate drawn with `icon_name` otherwise.
    pub fn around(daemon: &Daemon, icon_name: &str, content: &impl IsA<gtk::Widget>) -> gtk::Stack {
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .vexpand(true)
            .build();
        stack.add_named(content, Some("content"));
        let gate = Self::build(daemon, icon_name, Some(stack.clone()));
        stack.add_named(gate.widget(), Some("gate"));
        gate.follow();
        stack
    }

    /// A gate drawn with `icon_name`, the icon of the page it stands in for.
    pub fn new(daemon: &Daemon, icon_name: &str) -> Self {
        let gate = Self::build(daemon, icon_name, None);
        gate.follow();
        gate
    }

    fn build(daemon: &Daemon, icon_name: &str, stack: Option<gtk::Stack>) -> Self {
        let spinner = adw::Spinner::builder().width_request(32).height_request(32).build();
        let primary = gtk::Button::builder()
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .margin_top(12)
            .build();
        let secondary = gtk::Button::builder()
            .halign(gtk::Align::Center)
            .css_classes(["pill"])
            .build();
        let autostart = gtk::CheckButton::builder()
            .label("Start the engine when I log in")
            .halign(gtk::Align::Center)
            .margin_top(8)
            .build();
        let hint = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .use_markup(true)
            .margin_top(12)
            .css_classes(["dim-label", "group-description"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        for child in [
            spinner.upcast_ref::<gtk::Widget>(),
            primary.upcast_ref(),
            secondary.upcast_ref(),
            autostart.upcast_ref(),
            hint.upcast_ref(),
        ] {
            content.append(child);
        }
        let page = adw::StatusPage::builder()
            .icon_name(icon_name)
            .vexpand(true)
            .child(&content)
            .build();

        let autostart_handler = Rc::new(autostart.connect_toggled({
            let daemon = daemon.clone();
            move |check| set_autostart(&daemon, check)
        }));

        let gate = Self {
            page,
            spinner,
            primary,
            secondary,
            autostart,
            autostart_handler,
            hint,
            error: Rc::default(),
            stack,
            daemon: daemon.clone(),
        };
        gate.primary.connect_clicked({
            let gate = gate.clone();
            move |button| gate.command(button)
        });
        gate.secondary.connect_clicked({
            let gate = gate.clone();
            move |button| gate.command(button)
        });
        gate.hint.connect_activate_link(|label, _| {
            let _ = label.activate_action(TROUBLESHOOT_ACTION, None);
            glib::Propagation::Stop
        });
        gate
    }

    pub fn widget(&self) -> &adw::StatusPage {
        &self.page
    }

    fn follow(&self) {
        let gate = self.clone();
        let mut status = self.daemon.subscribe();
        glib::spawn_future_local(async move {
            let mut connection = None;
            loop {
                let current = status.borrow_and_update().clone();
                if connection != Some(current.connection) && current.link == Link::Connected {
                    connection = Some(current.connection);
                    gate.load_autostart(&current);
                }
                gate.show(&current);
                if status.changed().await.is_err() {
                    return;
                }
            }
        });
    }

    fn show(&self, status: &Status) {
        if let Some(stack) = &self.stack {
            stack.set_visible_child_name(if status.is_running() { "content" } else { "gate" });
        }
        let shown = present(status);
        self.page.set_title(shown.title);
        let error = self.error.borrow().clone();
        self.page
            .set_description(Some(&glib::markup_escape_text(&error.unwrap_or(shown.description))));
        self.spinner.set_visible(shown.busy);
        show_button(&self.primary, shown.primary);
        show_button(&self.secondary, shown.secondary);
        self.autostart.set_visible(shown.autostart);
        self.hint.set_visible(shown.hint.is_some());
        self.hint.set_markup(shown.hint.unwrap_or_default());
    }

    fn command(&self, button: &gtk::Button) {
        let command = if button.widget_name() == "troubleshoot" {
            Command::Troubleshoot
        } else {
            Command::Start
        };
        match command {
            Command::Troubleshoot => {
                let _ = button.activate_action(TROUBLESHOOT_ACTION, None);
            }
            Command::Start => {
                self.error.replace(None);
                let gate = self.clone();
                glib::spawn_future_local(async move {
                    if let Err(error) = gate.daemon.run(EngineAction::Start).await {
                        gate.error.replace(Some(describe(&error)));
                        gate.show(&gate.daemon.status());
                    }
                });
            }
        }
    }

    fn load_autostart(&self, status: &Status) {
        let Some(client) = status.client().cloned() else {
            return;
        };
        let gate = self.clone();
        glib::spawn_future_local(async move {
            let Ok(Ok(state)) = relm4::spawn(async move { SettingsHandle::new(client).get().await }).await else {
                return;
            };
            let enabled = state
                .settings
                .and_then(|settings| settings.engine)
                .is_some_and(|engine| engine.start_with_daemon);
            gate.autostart.block_signal(&gate.autostart_handler);
            gate.autostart.set_active(enabled);
            gate.autostart.unblock_signal(&gate.autostart_handler);
            gate.autostart
                .set_sensitive(!state.locked_paths.iter().any(|path| path == START_WITH_DAEMON));
        });
    }
}

fn set_autostart(daemon: &Daemon, check: &gtk::CheckButton) {
    let Some(client) = daemon.status().client().cloned() else {
        return;
    };
    let enabled = check.is_active();
    let settings = Settings {
        engine: Some(EngineSettings {
            start_with_daemon: enabled,
            ..EngineSettings::default()
        }),
        ..Settings::default()
    };
    relm4::spawn(async move {
        if let Err(error) = SettingsHandle::new(client).update(settings, &[START_WITH_DAEMON]).await {
            tracing::warn!(%error, "cannot change engine autostart");
        }
    });
}

fn show_button(button: &gtk::Button, shown: Option<(&str, Command)>) {
    button.set_visible(shown.is_some());
    if let Some((label, command)) = shown {
        button.set_label(label);
        button.set_widget_name(match command {
            Command::Start => "start",
            Command::Troubleshoot => "troubleshoot",
        });
    }
}

const TROUBLESHOOT_HINT: &str = "<a href=\"troubleshoot\">Troubleshoot</a>";

fn present(status: &Status) -> Presentation {
    let idle = |title, description: &str| Presentation {
        title,
        description: description.to_owned(),
        busy: false,
        primary: None,
        secondary: None,
        autostart: false,
        hint: None,
    };
    let busy = |title, description: &str| Presentation {
        busy: true,
        ..idle(title, description)
    };

    if let Link::Unreachable(reason) = &status.link {
        return Presentation {
            hint: Some(TROUBLESHOOT_HINT),
            ..busy("Can’t Reach the Contr Service", reason)
        };
    }
    if let Some((action, progress)) = &status.action {
        let title = match action {
            EngineAction::Start => "Starting the Engine",
            EngineAction::Stop => "Stopping the Engine",
            EngineAction::Restart => "Restarting the Engine",
        };
        return busy(title, progress.as_deref().unwrap_or_default());
    }

    match status.engine {
        EngineState::Unspecified => busy("Connecting", ""),
        EngineState::Stopped => Presentation {
            primary: Some(("Start Engine", Command::Start)),
            autostart: true,
            hint: Some(if status.backend() == BackendKind::Host {
                "Starting a system engine can ask for your password · <a href=\"troubleshoot\">Troubleshoot</a>"
            } else {
                TROUBLESHOOT_HINT
            }),
            ..idle(
                "The Engine Is Stopped",
                "Containers, images and volumes are kept. Start the engine to see and run them again.",
            )
        },
        EngineState::Uninitialized => Presentation {
            primary: Some(("Start Engine", Command::Start)),
            hint: Some(TROUBLESHOOT_HINT),
            ..idle(
                "The Engine Isn’t Set Up",
                "Starting the engine sets it up first. Nothing runs until you start it.",
            )
        },
        EngineState::Provisioning | EngineState::Starting => busy("Starting the Engine", ""),
        EngineState::Running => idle("The Engine Is Running", ""),
        EngineState::Stopping => busy("Stopping the Engine", ""),
        EngineState::Updating => busy("Updating the Engine", ""),
        EngineState::Resetting => busy("Resetting the Engine", ""),
        EngineState::Failed => {
            let (title, description) = failure_text(status.failure);
            Presentation {
                primary: Some(("Troubleshoot", Command::Troubleshoot)),
                secondary: Some(("Try Again", Command::Start)),
                ..idle(title, description)
            }
        }
    }
}

fn failure_text(reason: ErrorReason) -> (&'static str, &'static str) {
    match reason {
        ErrorReason::SocketPermissionDenied => (
            "Contr Can’t Use the Engine",
            "The engine is running, but your account isn’t allowed to use its socket yet.",
        ),
        ErrorReason::EngineNotInstalled => (
            "No Engine Is Installed",
            "Contr didn’t find Docker Engine or Podman on this computer.",
        ),
        _ => (
            "The Engine Failed",
            "Contr couldn’t start the engine. Troubleshoot shows what went wrong.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected(engine: EngineState) -> Status {
        let mut status = Status::initial();
        status.link = Link::Connected;
        status.engine = engine;
        status
    }

    #[test]
    fn stopped_engine_offers_start_and_autostart() {
        let shown = present(&connected(EngineState::Stopped));
        assert_eq!(shown.title, "The Engine Is Stopped");
        assert_eq!(shown.primary, Some(("Start Engine", Command::Start)));
        assert!(shown.autostart);
        assert!(!shown.busy);
    }

    #[test]
    fn failed_engine_names_the_socket_problem() {
        let mut status = connected(EngineState::Failed);
        status.failure = ErrorReason::SocketPermissionDenied;
        let shown = present(&status);
        assert_eq!(shown.title, "Contr Can’t Use the Engine");
        assert_eq!(shown.primary, Some(("Troubleshoot", Command::Troubleshoot)));
    }

    #[test]
    fn an_action_in_progress_shows_its_progress() {
        let mut status = connected(EngineState::Stopped);
        status.action = Some((EngineAction::Start, Some("Starting docker.service".to_owned())));
        let shown = present(&status);
        assert!(shown.busy);
        assert_eq!(shown.description, "Starting docker.service");
    }

    #[test]
    fn an_unreachable_daemon_says_why() {
        let mut status = Status::initial();
        status.link = Link::Unreachable("connection refused".to_owned());
        assert_eq!(present(&status).description, "connection refused");
    }
}
