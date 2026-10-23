//! The buttons for the fixes the daemon offers with its checks and errors.

use contr_sdk::api::common::v1::Remediation;

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
