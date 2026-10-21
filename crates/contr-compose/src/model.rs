//! What the Compose pages show: projects with their running counts, and each service's state.

use std::path::Path;

use contr_sdk::api::compose::v1::{Project, Service};
use contr_sdk::api::containers::v1::{Container, ContainerState};
use contr_ui_common::format;
use contr_ui_common::widgets::StatusKind;

/// Colours of service names in the logs, in service order; they repeat after the last.
pub(crate) const SERVICE_COLORS: [&str; 6] = ["#8ff0a4", "#f9f06b", "#99c1f1", "#f66151", "#dc8add", "#ffbe6f"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProjectView {
    pub(crate) name: String,
    pub(crate) path: String,
    pub(crate) running: usize,
    pub(crate) total: usize,
    pub(crate) error: Option<String>,
}

impl ProjectView {
    /// `3 of 4 running`, `2 running` or `stopped`.
    pub(crate) fn summary(&self) -> String {
        match self.running {
            0 if self.total == 0 => "not created".to_owned(),
            0 => "stopped".to_owned(),
            running if running == self.total => format!("{running} running"),
            running => format!("{running} of {} running", self.total),
        }
    }

    pub(crate) fn status(&self) -> StatusKind {
        match (self.running, self.total) {
            _ if self.error.is_some() => StatusKind::Failed,
            (0, _) => StatusKind::Stopped,
            (running, total) if running == total => StatusKind::Running,
            _ => StatusKind::Paused,
        }
    }
}

pub(crate) fn project_view(project: &Project, home: &Path) -> ProjectView {
    let containers = project.services.iter().flat_map(|service| service.containers.iter());
    let total = containers.clone().count();
    let running = containers
        .filter(|container| container.state() == ContainerState::Running)
        .count();
    let path = project
        .config_files
        .first()
        .map_or_else(|| project.working_dir.clone(), Clone::clone);
    ProjectView {
        name: project.name.clone(),
        path: format::home_relative(&path, home),
        running,
        total,
        error: (!project.config_error.is_empty()).then(|| project.config_error.clone()),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ServiceView {
    pub(crate) name: String,
    pub(crate) container: Option<String>,
    pub(crate) status: StatusKind,
    pub(crate) status_word: &'static str,
    /// Shown in the error colour before `subtitle`.
    pub(crate) failure: Option<String>,
    pub(crate) subtitle: String,
    pub(crate) failed: bool,
}

fn exit_code(status: &str) -> Option<i32> {
    status.strip_prefix("Exited (")?.split(')').next()?.parse().ok()
}

fn port(container: &Container) -> Option<String> {
    if let Some(published) = container.ports.iter().find(|port| port.host_port != 0) {
        return Some(format!("localhost:{}", published.host_port));
    }
    container
        .ports
        .first()
        .map(|exposed| format!(":{}", exposed.container_port))
}

/// The build context relative to the project, such as `./worker`.
fn build_source(service: &Service, working_dir: &str) -> Option<String> {
    if service.build_context.is_empty() {
        return None;
    }
    let relative = Path::new(&service.build_context)
        .strip_prefix(working_dir)
        .ok()
        .map(|path| format!("./{}", path.display()));
    Some(relative.unwrap_or_else(|| service.build_context.clone()))
}

pub(crate) fn service_view(service: &Service, working_dir: &str) -> ServiceView {
    let origin =
        build_source(service, working_dir).map_or_else(|| service.image.clone(), |dir| format!("built from {dir}"));
    let Some(container) = service.containers.first() else {
        return ServiceView {
            name: service.name.clone(),
            container: None,
            status: StatusKind::Created,
            status_word: "Not created",
            failure: None,
            subtitle: format!("{origin} · not created"),
            failed: false,
        };
    };
    let state = container.state();
    let failed = matches!(state, ContainerState::Exited | ContainerState::Dead)
        && exit_code(&container.status).is_some_and(|code| code != 0);
    let (status, status_word) = match state {
        ContainerState::Running => (StatusKind::Running, "Running"),
        ContainerState::Paused => (StatusKind::Paused, "Paused"),
        _ if failed => (StatusKind::Failed, "Exited with an error"),
        ContainerState::Created => (StatusKind::Created, "Created"),
        _ => (StatusKind::Stopped, "Stopped"),
    };
    let (failure, subtitle) = if failed {
        (Some(container.status.clone()), format!(" · {origin}"))
    } else {
        let mut parts = vec![origin];
        if container.status.contains("(healthy)") {
            parts.push("healthy".to_owned());
        } else if container.status.contains("(unhealthy)") {
            parts.push("unhealthy".to_owned());
        }
        match port(container).filter(|_| state == ContainerState::Running) {
            Some(port) => parts.push(port),
            None if state != ContainerState::Running => parts.push(container.status.clone()),
            None => {}
        }
        (None, parts.join(" · "))
    };
    ServiceView {
        name: service.name.clone(),
        container: Some(container.id.clone()),
        status,
        status_word,
        failure,
        subtitle,
        failed,
    }
}

/// `db     | ` with the name padded to the longest service name.
pub(crate) fn log_prefix(service: &str, width: usize) -> String {
    format!("{service:<width$} | ")
}

#[cfg(test)]
mod tests {
    use contr_sdk::api::containers::v1::{PortMapping, Protocol};

    use super::*;

    fn container(state: ContainerState, status: &str, port: Option<(u32, u32)>) -> Container {
        Container {
            id: format!("id-{status}"),
            state: state.into(),
            status: status.to_owned(),
            ports: port
                .map(|(container_port, host_port)| PortMapping {
                    container_port,
                    host_port,
                    host_ip: String::new(),
                    protocol: Protocol::Tcp.into(),
                })
                .into_iter()
                .collect(),
            ..Container::default()
        }
    }

    fn service(name: &str, image: &str, build: &str, containers: Vec<Container>) -> Service {
        Service {
            name: name.to_owned(),
            image: image.to_owned(),
            build_context: build.to_owned(),
            containers,
        }
    }

    #[test]
    fn services_read_like_the_design() {
        let api = service(
            "api",
            "node:22-alpine",
            "",
            vec![container(ContainerState::Running, "Up 2 hours", Some((3000, 8080)))],
        );
        assert_eq!(
            service_view(&api, "/src/shop-api").subtitle,
            "node:22-alpine · localhost:8080"
        );

        let db = service(
            "db",
            "postgres:17.2",
            "",
            vec![container(
                ContainerState::Running,
                "Up 2 hours (healthy)",
                Some((5432, 0)),
            )],
        );
        assert_eq!(
            service_view(&db, "/src/shop-api").subtitle,
            "postgres:17.2 · healthy · :5432"
        );

        let worker = service(
            "worker",
            "shop-api-worker",
            "/src/shop-api/worker",
            vec![container(ContainerState::Exited, "Exited (1) 14 minutes ago", None)],
        );
        let view = service_view(&worker, "/src/shop-api");
        assert!(view.failed);
        assert_eq!(view.failure.as_deref(), Some("Exited (1) 14 minutes ago"));
        assert_eq!(view.subtitle, " · built from ./worker");
    }

    #[test]
    fn projects_count_their_running_containers() {
        let project = Project {
            name: "shop-api".to_owned(),
            config_files: vec!["/home/minh/src/shop-api/compose.yaml".to_owned()],
            services: vec![
                service("api", "a", "", vec![container(ContainerState::Running, "Up", None)]),
                service(
                    "worker",
                    "w",
                    "",
                    vec![container(ContainerState::Exited, "Exited (1)", None)],
                ),
            ],
            ..Project::default()
        };
        let view = project_view(&project, Path::new("/home/minh"));
        assert_eq!(view.path, "~/src/shop-api/compose.yaml");
        assert_eq!(view.summary(), "1 of 2 running");
        assert_eq!(view.status(), StatusKind::Paused);
        assert_eq!(log_prefix("db", 6), "db     | ");
    }
}
