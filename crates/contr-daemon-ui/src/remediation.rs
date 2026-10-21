//! Running the fixes the daemon offers with its checks and errors.

use contr_sdk::api::common::v1::{Progress, Remediation};
use contr_sdk::api::diagnostics::v1::{RemediationOutcome, RunRemediationRequest, run_remediation_response};
use contr_sdk::{Client, Error};

/// Label of the button that runs a remediation; IDs come from `contr_core::Remediation`.
pub fn remediation_label(remediation: &Remediation) -> String {
    match remediation.action_id.as_str() {
        "install-engine" => "Install an Engine…".to_owned(),
        "join-docker-group" => "Add Me to the docker Group…".to_owned(),
        "setup-rootless" => "Set Up Rootless Docker".to_owned(),
        "install-wsl" => "Install WSL…".to_owned(),
        other => other.to_owned(),
    }
}

/// Runs `remediation`, passing each step to `progress`; the outcome says what the user must still do.
///
/// Dropping the future stops following the remediation, not the remediation itself.
pub async fn run_remediation(
    client: Client,
    remediation: &Remediation,
    progress: impl Fn(Progress),
) -> Result<RemediationOutcome, Error> {
    let request = RunRemediationRequest {
        action_id: remediation.action_id.clone(),
    };
    let mut stream = client.diagnostics().run_remediation(request).await?.into_inner();
    while let Some(response) = stream.message().await? {
        match response.event {
            Some(run_remediation_response::Event::Progress(step)) => progress(step),
            Some(run_remediation_response::Event::Outcome(outcome)) => return Ok(outcome),
            None => {}
        }
    }
    Ok(RemediationOutcome::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_actions_read_as_buttons() {
        let remediation = |action: &str| Remediation {
            action_id: action.to_owned(),
            requires_elevation: true,
        };
        assert_eq!(
            remediation_label(&remediation("join-docker-group")),
            "Add Me to the docker Group…"
        );
        assert_eq!(remediation_label(&remediation("future-fix")), "future-fix");
    }
}
