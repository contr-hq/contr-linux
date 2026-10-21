//! What Troubleshoot shows: check results, the problem they add up to, and the fixes the daemon offers.

use contr_daemon_ui::{Link, Status};
use contr_sdk::api::common::v1::{ErrorReason, Remediation};
use contr_sdk::api::diagnostics::v1::{Check, CheckStatus};
use contr_sdk::api::engine::v1::EngineState;
use contr_ui_common::widgets::StatusKind;

/// Label of the button that runs a remediation; IDs come from `contr_core::Remediation`.
pub(crate) fn remediation_label(remediation: &Remediation) -> String {
    match remediation.action_id.as_str() {
        "install-engine" => "Install an Engine…".to_owned(),
        "join-docker-group" => "Add Me to the docker Group…".to_owned(),
        "setup-rootless" => "Set Up Rootless Docker".to_owned(),
        "install-wsl" => "Install WSL…".to_owned(),
        other => other.to_owned(),
    }
}

pub(crate) fn check_status(check: &Check) -> (StatusKind, &'static str) {
    match check.status() {
        CheckStatus::Pass => (StatusKind::Running, "Passed"),
        CheckStatus::Warn => (StatusKind::Paused, "Warning"),
        CheckStatus::Fail => (StatusKind::Failed, "Problem"),
        CheckStatus::Skip | CheckStatus::Unspecified => (StatusKind::Created, "Skipped"),
    }
}

/// `1 problem, 1 warning`, or `everything passed`.
pub(crate) fn tally(checks: &[Check]) -> String {
    let count = |status: CheckStatus| checks.iter().filter(|check| check.status() == status).count();
    let plural = |count: usize, word: &str| {
        if count == 1 {
            format!("1 {word}")
        } else {
            format!("{count} {word}s")
        }
    };
    let mut parts = Vec::new();
    let problems = count(CheckStatus::Fail);
    let warnings = count(CheckStatus::Warn);
    if problems > 0 {
        parts.push(plural(problems, "problem"));
    }
    if warnings > 0 {
        parts.push(plural(warnings, "warning"));
    }
    if parts.is_empty() {
        "everything passed".to_owned()
    } else {
        parts.join(", ")
    }
}

/// The problem the alert at the top of the page names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Problem {
    pub(crate) title: String,
    pub(crate) detail: String,
    /// The `ErrorReason` name the daemon sent, for support.
    pub(crate) code: Option<&'static str>,
    pub(crate) remediations: Vec<Remediation>,
}

fn reason_name(reason: ErrorReason) -> Option<&'static str> {
    match reason {
        ErrorReason::Unspecified => None,
        ErrorReason::EngineNotRunning => Some("ENGINE_NOT_RUNNING"),
        ErrorReason::WslNotInstalled => Some("WSL_NOT_INSTALLED"),
        ErrorReason::VirtualizationDisabled => Some("VIRTUALIZATION_DISABLED"),
        ErrorReason::SocketPermissionDenied => Some("SOCKET_PERMISSION_DENIED"),
        ErrorReason::SettingLocked => Some("SETTING_LOCKED"),
        ErrorReason::FeatureUnavailable => Some("FEATURE_UNAVAILABLE"),
        ErrorReason::EngineNotInstalled => Some("ENGINE_NOT_INSTALLED"),
    }
}

/// The problem to lead with: an unreachable daemon, a failed engine, else the first failing check.
pub(crate) fn problem(status: &Status, checks: &[Check]) -> Option<Problem> {
    let failing: Vec<&Check> = checks
        .iter()
        .filter(|check| check.status() == CheckStatus::Fail)
        .collect();
    let mut remediations: Vec<Remediation> = Vec::new();
    for remediation in failing.iter().flat_map(|check| &check.remediations) {
        if !remediations
            .iter()
            .any(|known| known.action_id == remediation.action_id)
        {
            remediations.push(remediation.clone());
        }
    }
    if let Link::Unreachable(reason) = &status.link {
        return Some(Problem {
            title: "Contr can’t reach its service".to_owned(),
            detail: reason.clone(),
            code: None,
            remediations: Vec::new(),
        });
    }
    if status.engine == EngineState::Failed {
        let (title, detail) = match status.failure {
            ErrorReason::SocketPermissionDenied => (
                "Contr can’t connect to the engine",
                "The engine is running, but your account isn’t allowed to use its socket yet.",
            ),
            ErrorReason::EngineNotInstalled => (
                "No engine is installed",
                "Contr didn’t find Docker Engine or Podman on this computer.",
            ),
            _ => ("The engine failed", "Contr couldn’t start or reach the engine."),
        };
        let detail = failing
            .first()
            .map_or_else(|| detail.to_owned(), |check| format!("{detail} {}", check.detail));
        return Some(Problem {
            title: title.to_owned(),
            detail,
            code: reason_name(status.failure),
            remediations,
        });
    }
    let first = failing.first()?;
    Some(Problem {
        title: first.title.clone(),
        detail: first.detail.clone(),
        code: None,
        remediations,
    })
}

/// Problems the sidebar badge counts: failing checks, or one for a failed engine or unreachable daemon.
pub(crate) fn problem_count(status: &Status, checks: &[Check]) -> usize {
    let failing = checks
        .iter()
        .filter(|check| check.status() == CheckStatus::Fail)
        .count();
    let engine = usize::from(matches!(status.link, Link::Unreachable(_)) || status.engine == EngineState::Failed);
    failing.max(engine)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(id: &str, status: CheckStatus, remediation: Option<&str>) -> Check {
        Check {
            id: id.to_owned(),
            title: id.to_owned(),
            status: status.into(),
            detail: format!("{id} detail"),
            remediations: remediation
                .map(|action| Remediation {
                    action_id: action.to_owned(),
                    requires_elevation: true,
                })
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn tallies_problems_and_warnings() {
        let checks = [
            check("a", CheckStatus::Pass, None),
            check("b", CheckStatus::Fail, None),
            check("c", CheckStatus::Warn, None),
            check("d", CheckStatus::Warn, None),
        ];
        assert_eq!(tally(&checks), "1 problem, 2 warnings");
        assert_eq!(tally(&checks[..1]), "everything passed");
    }

    #[test]
    fn a_failed_engine_leads_with_its_reason_and_fixes() {
        let mut status = Status::default();
        status.link = Link::Connected;
        status.engine = EngineState::Failed;
        status.failure = ErrorReason::SocketPermissionDenied;
        let checks = [
            check("socket", CheckStatus::Fail, Some("join-docker-group")),
            check("api", CheckStatus::Fail, Some("join-docker-group")),
        ];
        let problem = problem(&status, &checks).unwrap();
        assert_eq!(problem.remediations.len(), 1);
        assert_eq!(problem.code, Some("SOCKET_PERMISSION_DENIED"));
        assert_eq!(
            remediation_label(&problem.remediations[0]),
            "Add Me to the docker Group…"
        );
        assert_eq!(problem_count(&status, &checks), 2);
    }

    #[test]
    fn healthy_systems_have_no_problem() {
        let mut status = Status::default();
        status.link = Link::Connected;
        status.engine = EngineState::Running;
        assert_eq!(problem(&status, &[check("a", CheckStatus::Pass, None)]), None);
        assert_eq!(problem_count(&status, &[]), 0);
    }
}
