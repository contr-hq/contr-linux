//! Desktop notifications for a container that stops with an error and for pulls and Compose runs that finish.

use contr_core::product;
use contr_sdk::api::containers::v1::{Container, ContainerState};
use relm4::gtk::{gio, prelude::*};

const PROJECT_LABEL: &str = "com.docker.compose.project";
const SERVICE_LABEL: &str = "com.docker.compose.service";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Crash {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) body: String,
}

/// A container this client saw running that has now exited with a non-zero code.
pub(crate) fn crash(before: Option<&Container>, after: &Container) -> Option<Crash> {
    let was_running = before.is_some_and(|before| before.state() == ContainerState::Running);
    if !was_running || after.state() != ContainerState::Exited {
        return None;
    }
    let code: i32 = after.status.strip_prefix("Exited (")?.split(')').next()?.parse().ok()?;
    if code == 0 {
        return None;
    }
    let name = after
        .labels
        .get(SERVICE_LABEL)
        .or_else(|| after.names.first())
        .cloned()
        .unwrap_or_else(|| after.id.clone());
    let body = match after.labels.get(PROJECT_LABEL) {
        Some(project) => format!("{project} · exited with code {code}"),
        None => format!("Exited with code {code}"),
    };
    Some(Crash {
        id: after.id.clone(),
        title: format!("{name} stopped"),
        body,
    })
}

pub(crate) fn send_crash(crash: &Crash) {
    let notification = gio::Notification::new(&crash.title);
    notification.set_body(Some(&crash.body));
    notification.add_button_with_target_value("View Logs", "app.open-logs", Some(&crash.id.to_variant()));
    notification.add_button_with_target_value("Restart", "app.restart-container", Some(&crash.id.to_variant()));
    notification.set_default_action_and_target_value("app.open-logs", Some(&crash.id.to_variant()));
    relm4::main_application().send_notification(Some(&format!("crash-{}", crash.id)), &notification);
}

pub(crate) fn finished(message: &str) {
    let notification = gio::Notification::new(product::DISPLAY_NAME);
    notification.set_body(Some(message));
    relm4::main_application().send_notification(None, &notification);
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn names_the_service_and_the_exit_code() {
        let mut labels = HashMap::new();
        labels.insert(PROJECT_LABEL.to_owned(), "shop-api".to_owned());
        labels.insert(SERVICE_LABEL.to_owned(), "worker".to_owned());
        let running = Container {
            id: "c1".to_owned(),
            state: ContainerState::Running.into(),
            labels: labels.clone(),
            ..Container::default()
        };
        let exited = Container {
            state: ContainerState::Exited.into(),
            status: "Exited (1) 1 second ago".to_owned(),
            ..running.clone()
        };
        let crash = crash(Some(&running), &exited).unwrap();
        assert_eq!(crash.title, "worker stopped");
        assert_eq!(crash.body, "shop-api · exited with code 1");
        assert_eq!(super::crash(None, &exited), None);
    }
}
