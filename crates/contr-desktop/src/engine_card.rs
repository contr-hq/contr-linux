use std::time::{SystemTime, UNIX_EPOCH};

use contr_daemon_ui::{EngineAction, Link, Status};
use contr_sdk::api::common::v1::ErrorReason;
use contr_sdk::api::daemon::v1::EngineInfo;
use contr_sdk::api::engine::v1::EngineState;
use relm4::adw;
use relm4::gtk::{self, glib, prelude::*};

/// The card at the bottom of the sidebar that names the engine and its state.
pub(crate) struct EngineCard {
    pub(crate) root: gtk::Box,
    icon: gtk::Image,
    spinner: adw::Spinner,
    title: gtk::Label,
    subtitle: gtk::Label,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardKind {
    Running,
    Busy,
    Stopped,
    Failed,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CardText {
    pub(crate) kind: CardKind,
    pub(crate) title: &'static str,
    pub(crate) subtitle: String,
}

impl EngineCard {
    pub(crate) fn new() -> Self {
        let icon = gtk::Image::builder().valign(gtk::Align::Start).margin_top(2).build();
        let spinner = adw::Spinner::builder()
            .width_request(16)
            .height_request(16)
            .valign(gtk::Align::Start)
            .margin_top(2)
            .visible(false)
            .build();
        let title = gtk::Label::builder().xalign(0.0).css_classes(["title"]).build();
        let subtitle = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["subtitle", "dim-label"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .build();
        text.append(&title);
        text.append(&subtitle);
        let root = gtk::Box::builder()
            .spacing(10)
            .css_classes(["card", "engine-card"])
            .build();
        root.append(&icon);
        root.append(&spinner);
        root.append(&text);
        Self {
            root,
            icon,
            spinner,
            title,
            subtitle,
        }
    }

    pub(crate) fn show(&self, status: &Status) {
        let text = describe(status);
        let (icon, class) = match text.kind {
            CardKind::Running => ("contr-running-symbolic", "status-running"),
            CardKind::Busy | CardKind::Stopped => ("contr-stopped-symbolic", "status-stopped"),
            CardKind::Failed => ("contr-failed-symbolic", "status-failed"),
        };
        for old in ["status-running", "status-stopped", "status-failed"] {
            self.icon.remove_css_class(old);
        }
        self.icon.set_icon_name(Some(icon));
        self.icon.add_css_class(class);
        self.icon.set_visible(text.kind != CardKind::Busy);
        self.spinner.set_visible(text.kind == CardKind::Busy);
        if text.kind == CardKind::Failed {
            self.root.add_css_class("failed");
        } else {
            self.root.remove_css_class("failed");
        }
        self.title.set_label(text.title);
        self.subtitle.set_label(&text.subtitle);
        self.subtitle.set_visible(!text.subtitle.is_empty());
    }
}

/// The engine's product name and major version, such as `System Docker 28`.
pub(crate) fn engine_name(info: &EngineInfo) -> String {
    let kind = if info.endpoint.contains("podman") {
        "Podman"
    } else if info.rootless {
        "Rootless Docker"
    } else {
        "System Docker"
    };
    match info.version.split('.').next().filter(|major| !major.is_empty()) {
        Some(major) => format!("{kind} {major}"),
        None => kind.to_owned(),
    }
}

pub(crate) fn describe(status: &Status) -> CardText {
    let card = |kind, title, subtitle: String| CardText { kind, title, subtitle };

    if let Link::Unreachable(reason) = &status.link {
        return card(CardKind::Failed, "Can’t reach Contr", reason.clone());
    }
    if let Some((action, progress)) = &status.action {
        let title = match action {
            EngineAction::Start => "Engine starting",
            EngineAction::Stop => "Engine stopping",
            EngineAction::Restart => "Engine restarting",
        };
        return card(CardKind::Busy, title, progress.clone().unwrap_or_default());
    }

    match status.engine {
        EngineState::Unspecified => card(CardKind::Busy, "Connecting", String::new()),
        EngineState::Running => {
            let subtitle = status.engine_info.as_ref().map_or_else(String::new, |info| {
                if info.docker_context.is_empty() {
                    engine_name(info)
                } else {
                    format!("{} · context {}", engine_name(info), info.docker_context)
                }
            });
            card(CardKind::Running, "Engine running", subtitle)
        }
        EngineState::Stopped => {
            let subtitle = status.changed_at.map_or_else(String::new, |at| {
                let time = clock(at);
                if status.stopped_by_user {
                    format!("Stopped by you at {time}")
                } else {
                    format!("Stopped at {time}")
                }
            });
            card(CardKind::Stopped, "Engine stopped", subtitle)
        }
        EngineState::Uninitialized => card(CardKind::Stopped, "Engine not set up", String::new()),
        EngineState::Provisioning | EngineState::Starting => card(CardKind::Busy, "Engine starting", String::new()),
        EngineState::Stopping => card(CardKind::Busy, "Engine stopping", String::new()),
        EngineState::Updating => card(CardKind::Busy, "Engine updating", String::new()),
        EngineState::Resetting => card(CardKind::Busy, "Engine resetting", String::new()),
        EngineState::Failed => {
            let subtitle = match status.failure {
                ErrorReason::SocketPermissionDenied => "Permission denied on the socket",
                ErrorReason::EngineNotInstalled => "No engine is installed",
                _ => "The engine failed",
            };
            card(CardKind::Failed, "Can’t reach the engine", subtitle.to_owned())
        }
    }
}

/// Local wall-clock time such as `18:02`.
fn clock(at: SystemTime) -> String {
    let seconds = at
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default();
    glib::DateTime::from_unix_local(seconds)
        .and_then(|time| time.format("%H:%M"))
        .map(String::from)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(endpoint: &str, rootless: bool) -> EngineInfo {
        EngineInfo {
            endpoint: endpoint.to_owned(),
            version: "28.5.1".to_owned(),
            rootless,
            ..EngineInfo::default()
        }
    }

    #[test]
    fn names_the_engine_kind_and_major_version() {
        assert_eq!(
            engine_name(&info("unix:///var/run/docker.sock", false)),
            "System Docker 28"
        );
        assert_eq!(
            engine_name(&info("unix:///run/user/1000/docker.sock", true)),
            "Rootless Docker 28"
        );
        assert_eq!(
            engine_name(&info("unix:///run/user/1000/podman/podman.sock", true)),
            "Podman 28"
        );
    }
}
