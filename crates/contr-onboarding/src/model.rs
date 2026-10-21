//! What onboarding shows: the engine the daemon manages, the fixes its checks offer, and the steps of a running fix.

use contr_daemon_ui::{Link, Status, engine_name};
use contr_sdk::api::common::v1::{ErrorReason, Progress, Remediation};
use contr_sdk::api::diagnostics::v1::{Check, CheckStatus, RemediationOutcome};
use contr_sdk::api::engine::v1::EngineState;
use contr_ui_common::widgets::StatusKind;

const INSTALL_ENGINE: &str = "install-engine";
const JOIN_DOCKER_GROUP: &str = "join-docker-group";

/// The row for the engine the daemon manages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EngineView {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) state: &'static str,
    pub(crate) kind: StatusKind,
    /// Why the engine cannot be used yet, shown above its fixes.
    pub(crate) explanation: Option<&'static str>,
}

impl EngineView {
    /// The engine works for this user, or would once started.
    pub(crate) fn ready(&self) -> bool {
        !matches!(self.kind, StatusKind::Failed | StatusKind::Paused)
    }
}

/// `None` when no engine is installed, so only the install row remains.
pub(crate) fn engine_view(status: &Status) -> Option<EngineView> {
    let view = |title: &str, subtitle: String, state, kind, explanation| EngineView {
        title: title.to_owned(),
        subtitle,
        state,
        kind,
        explanation,
    };
    if let Link::Unreachable(reason) = &status.link {
        return Some(view(
            "Contr service",
            reason.clone(),
            "Not reachable",
            StatusKind::Failed,
            None,
        ));
    }
    let info = status.engine_info.as_ref();
    let title = info.map_or_else(|| "Container engine".to_owned(), engine_name);
    let location = info.map_or_else(String::new, |info| {
        let endpoint = info.endpoint.strip_prefix("unix://").unwrap_or(&info.endpoint);
        format!("Version {} · {endpoint}", info.version)
    });
    Some(match (status.engine, status.failure) {
        (EngineState::Failed, ErrorReason::EngineNotInstalled) => return None,
        (EngineState::Failed, ErrorReason::SocketPermissionDenied) => view(
            "Docker Engine",
            "Installed and running".to_owned(),
            "No access yet",
            StatusKind::Paused,
            Some("The engine is running, but your account can’t use its socket yet."),
        ),
        (EngineState::Failed, _) => view(
            &title,
            location,
            "Not working",
            StatusKind::Failed,
            Some("Contr couldn’t start or reach the engine. Troubleshoot shows what failed."),
        ),
        (EngineState::Running, _) => view(&title, location, "Running", StatusKind::Running, None),
        (EngineState::Stopped, _) => view(&title, location, "Stopped", StatusKind::Stopped, None),
        (EngineState::Starting | EngineState::Provisioning, _) => {
            view(&title, location, "Starting", StatusKind::Created, None)
        }
        (EngineState::Stopping, _) => view(&title, location, "Stopping", StatusKind::Created, None),
        (EngineState::Updating | EngineState::Resetting, _) => {
            view(&title, location, "Busy", StatusKind::Created, None)
        }
        (EngineState::Uninitialized, _) => view(&title, location, "Not set up", StatusKind::Created, None),
        (EngineState::Unspecified, _) => view("Looking for an engine…", String::new(), "", StatusKind::Created, None),
    })
}

/// The fixes the failing checks offer, split into the engine install and the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Fixes {
    pub(crate) access: Vec<Remediation>,
    pub(crate) install: Option<Remediation>,
}

impl Fixes {
    /// Joining the docker group makes the user root-equivalent, which the page says before offering it.
    pub(crate) fn grants_root(&self) -> bool {
        self.access
            .iter()
            .any(|remediation| remediation.action_id == JOIN_DOCKER_GROUP)
    }
}

pub(crate) fn fixes(checks: &[Check]) -> Fixes {
    let mut fixes = Fixes::default();
    let offered = checks
        .iter()
        .filter(|check| check.status() == CheckStatus::Fail)
        .flat_map(|check| &check.remediations);
    for remediation in offered {
        if remediation.action_id == INSTALL_ENGINE {
            fixes.install.get_or_insert_with(|| remediation.clone());
        } else if !fixes
            .access
            .iter()
            .any(|known| known.action_id == remediation.action_id)
        {
            fixes.access.push(remediation.clone());
        }
    }
    fixes
}

/// Title of the page that follows a running fix.
pub(crate) fn fix_title(remediation: &Remediation) -> &'static str {
    match remediation.action_id.as_str() {
        INSTALL_ENGINE => "Installing an Engine",
        JOIN_DOCKER_GROUP => "Giving You Access",
        "setup-rootless" => "Setting Up Rootless Docker",
        _ => "Fixing the Engine",
    }
}

/// The steps a running fix reported, in order, and how far it got.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Steps {
    pub(crate) messages: Vec<String>,
    /// Completed share in 0.0 to 1.0; `None` while the daemon cannot tell.
    pub(crate) fraction: Option<f32>,
}

impl Steps {
    pub(crate) fn push(&mut self, progress: Progress) {
        let repeated = self.messages.last() == Some(&progress.message);
        if !progress.message.is_empty() && !repeated {
            self.messages.push(progress.message);
        }
        self.fraction = progress.fraction;
    }
}

/// What the user must still do after a fix succeeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Finish {
    LogOut,
    Restart,
}

/// A restart also starts a new session, so it wins over a log out.
pub(crate) fn finish(outcome: RemediationOutcome) -> Option<Finish> {
    if outcome.restart_required {
        Some(Finish::Restart)
    } else if outcome.logout_required {
        Some(Finish::LogOut)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use contr_sdk::api::daemon::v1::EngineInfo;

    use super::*;

    fn check(status: CheckStatus, actions: &[&str]) -> Check {
        Check {
            status: status.into(),
            remediations: actions
                .iter()
                .map(|action| Remediation {
                    action_id: (*action).to_owned(),
                    requires_elevation: true,
                })
                .collect(),
            ..Check::default()
        }
    }

    #[test]
    fn failing_checks_offer_access_fixes_once_and_the_install_apart() {
        let checks = [
            check(CheckStatus::Fail, &["setup-rootless", "join-docker-group"]),
            check(CheckStatus::Fail, &["join-docker-group", "install-engine"]),
            check(CheckStatus::Warn, &["ignored"]),
        ];
        let fixes = fixes(&checks);
        let access: Vec<&str> = fixes.access.iter().map(|fix| fix.action_id.as_str()).collect();
        assert_eq!(access, ["setup-rootless", "join-docker-group"]);
        assert_eq!(
            fixes.install.map(|fix| fix.action_id),
            Some("install-engine".to_owned())
        );
        assert!(self::fixes(&checks[..1]).grants_root());
    }

    #[test]
    fn a_refused_socket_reads_as_no_access_yet() {
        let mut status = Status::default();
        status.link = Link::Connected;
        status.engine = EngineState::Failed;
        status.failure = ErrorReason::SocketPermissionDenied;
        let view = engine_view(&status).unwrap();
        assert_eq!(view.state, "No access yet");
        assert!(!view.ready());

        status.failure = ErrorReason::EngineNotInstalled;
        assert_eq!(engine_view(&status), None);
    }

    #[test]
    fn a_running_engine_names_itself_and_its_socket() {
        let mut status = Status::default();
        status.link = Link::Connected;
        status.engine = EngineState::Running;
        status.engine_info = Some(EngineInfo {
            endpoint: "unix:///var/run/docker.sock".to_owned(),
            version: "29.9.1".to_owned(),
            ..EngineInfo::default()
        });
        let view = engine_view(&status).unwrap();
        assert_eq!(view.title, "System Docker 29");
        assert_eq!(view.subtitle, "Version 29.9.1 · /var/run/docker.sock");
        assert!(view.ready());
    }

    #[test]
    fn steps_skip_repeats_and_restart_wins_over_log_out() {
        let mut steps = Steps::default();
        for message in ["Installing packages", "Installing packages", "Starting docker.service"] {
            steps.push(Progress {
                message: message.to_owned(),
                fraction: Some(0.5),
            });
        }
        assert_eq!(steps.messages, ["Installing packages", "Starting docker.service"]);
        let outcome = RemediationOutcome {
            restart_required: true,
            logout_required: true,
        };
        assert_eq!(finish(outcome), Some(Finish::Restart));
        assert_eq!(finish(RemediationOutcome::default()), None);
    }
}
