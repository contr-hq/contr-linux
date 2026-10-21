//! What the container list shows, computed from the engine's containers and their latest stats samples.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use contr_sdk::api::containers::v1::{Container, ContainerState, ContainerStats};
use contr_ui_common::format;
use contr_ui_common::widgets::StatusKind;

/// Labels Compose sets on the containers it creates.
pub(crate) mod labels {
    pub(crate) const PROJECT: &str = "com.docker.compose.project";
    pub(crate) const SERVICE: &str = "com.docker.compose.service";
    pub(crate) const NUMBER: &str = "com.docker.compose.container-number";
    pub(crate) const CONFIG_FILES: &str = "com.docker.compose.project.config_files";
    pub(crate) const WORKING_DIR: &str = "com.docker.compose.project.working_dir";
}

/// A published port as a link, or an exposed one as plain text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Port {
    Link { label: String, url: String },
    Exposed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) image: String,
    pub(crate) status: StatusKind,
    pub(crate) status_word: &'static str,
    /// Shown in the error colour before `subtitle`.
    pub(crate) failure: Option<String>,
    pub(crate) subtitle: String,
    pub(crate) port: Option<Port>,
    pub(crate) running: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupView {
    /// Compose project name; `None` for containers outside any project.
    pub(crate) project: Option<String>,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) rows: Vec<RowView>,
}

impl GroupView {
    pub(crate) fn any_running(&self) -> bool {
        self.rows.iter().any(|row| row.running)
    }

    pub(crate) fn any_stopped(&self) -> bool {
        self.rows.iter().any(|row| !row.running)
    }
}

/// Exit code from an engine status such as `Exited (1) 14 minutes ago`.
pub(crate) fn exit_code(status: &str) -> Option<i32> {
    let rest = status.strip_prefix("Exited (")?;
    rest.split(')').next()?.parse().ok()
}

pub(crate) fn name(container: &Container) -> String {
    container
        .names
        .first()
        .cloned()
        .unwrap_or_else(|| format::short_id(&container.id).to_owned())
}

/// The Compose service name for Compose containers, numbered when the service has replicas; else the name.
pub(crate) fn title(container: &Container) -> String {
    match (
        container.labels.get(labels::SERVICE),
        container.labels.get(labels::NUMBER),
    ) {
        (Some(service), Some(number)) if number != "1" => format!("{service}-{number}"),
        (Some(service), _) => service.clone(),
        (None, _) => name(container),
    }
}

pub(crate) fn status(container: &Container) -> (StatusKind, &'static str) {
    match container.state() {
        ContainerState::Running => (StatusKind::Running, "Running"),
        ContainerState::Paused => (StatusKind::Paused, "Paused"),
        ContainerState::Restarting => (StatusKind::Failed, "Restarting"),
        ContainerState::Dead => (StatusKind::Failed, "Dead"),
        ContainerState::Created => (StatusKind::Created, "Created"),
        ContainerState::Exited if exit_code(&container.status).is_some_and(|code| code != 0) => {
            (StatusKind::Failed, "Exited with an error")
        }
        _ => (StatusKind::Stopped, "Stopped"),
    }
}

fn port(container: &Container) -> Option<Port> {
    if container.state() != ContainerState::Running {
        return None;
    }
    if let Some(published) = container.ports.iter().find(|port| port.host_port != 0) {
        return Some(Port::Link {
            label: format!("localhost:{}", published.host_port),
            url: format!("http://localhost:{}", published.host_port),
        });
    }
    container
        .ports
        .first()
        .map(|exposed| Port::Exposed(format!(":{}", exposed.container_port)))
}

pub(crate) fn row(container: &Container, stats: Option<&ContainerStats>) -> RowView {
    let (status, status_word) = status(container);
    let running = container.state() == ContainerState::Running;
    let (failure, subtitle) = if status == StatusKind::Failed && !running {
        (Some(container.status.clone()), format!(" · {}", container.image))
    } else {
        let mut parts = vec![container.image.clone()];
        if !container.status.is_empty() {
            parts.push(container.status.clone());
        }
        if let Some(stats) = stats.filter(|_| running) {
            parts.push(format::percent(stats.cpu_percent));
            parts.push(format::bytes(stats.memory_usage_bytes));
        }
        (None, parts.join(" · "))
    };
    RowView {
        id: container.id.clone(),
        name: name(container),
        title: title(container),
        image: container.image.clone(),
        status,
        status_word,
        failure,
        subtitle,
        port: port(container),
        running,
    }
}

fn running_summary(running: usize, total: usize) -> String {
    match running {
        0 => "stopped".to_owned(),
        _ if running == total => format!("{running} running"),
        _ => format!("{running} of {total} running"),
    }
}

/// Groups containers by Compose project, projects by name and the standalone group last, rows by title.
pub(crate) fn groups(
    containers: &BTreeMap<String, Container>,
    stats: &HashMap<String, ContainerStats>,
    home: &Path,
    query: &str,
) -> Vec<GroupView> {
    let query = query.trim().to_lowercase();
    let mut projects: BTreeMap<Option<String>, Vec<&Container>> = BTreeMap::new();
    for container in containers.values() {
        projects
            .entry(container.labels.get(labels::PROJECT).cloned())
            .or_default()
            .push(container);
    }

    let mut groups: Vec<GroupView> = projects
        .into_iter()
        .filter_map(|(project, members)| {
            let total = members.len();
            let running = members
                .iter()
                .filter(|container| container.state() == ContainerState::Running)
                .count();
            let mut rows: Vec<RowView> = members
                .iter()
                .map(|container| row(container, stats.get(&container.id)))
                .filter(|row| {
                    query.is_empty()
                        || row.title.to_lowercase().contains(&query)
                        || row.name.to_lowercase().contains(&query)
                        || row.image.to_lowercase().contains(&query)
                })
                .collect();
            if rows.is_empty() {
                return None;
            }
            rows.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.id.cmp(&b.id)));

            let (title, description) = match &project {
                Some(name) => {
                    let first = members.first();
                    let config = first
                        .and_then(|container| container.labels.get(labels::CONFIG_FILES))
                        .and_then(|files| files.split(',').next())
                        .or_else(|| {
                            first
                                .and_then(|container| container.labels.get(labels::WORKING_DIR))
                                .map(String::as_str)
                        })
                        .map(|path| format::home_relative(path, home));
                    let summary = running_summary(running, total);
                    let description = match config {
                        Some(config) => format!("{config} · {summary}"),
                        None => summary,
                    };
                    (name.clone(), description)
                }
                None => ("Standalone".to_owned(), "Not part of a compose project".to_owned()),
            };
            Some(GroupView {
                project,
                title,
                description,
                rows,
            })
        })
        .collect();
    // The standalone group sorts first as `None`; the design lists it last.
    let standalone_first = groups.first().is_some_and(|group| group.project.is_none());
    groups.rotate_left(usize::from(standalone_first));
    groups
}

/// `5 running · 2 stopped`, leaving out an empty part.
pub(crate) fn header_summary(containers: &BTreeMap<String, Container>) -> String {
    let running = containers
        .values()
        .filter(|container| container.state() == ContainerState::Running)
        .count();
    let stopped = containers.len() - running;
    match (running, stopped) {
        (0, 0) => String::new(),
        (running, 0) => format!("{running} running"),
        (0, stopped) => format!("{stopped} stopped"),
        (running, stopped) => format!("{running} running · {stopped} stopped"),
    }
}

/// A container that was running and has now exited with a non-zero code: what the banner says about it.
pub(crate) fn crash(before: Option<&Container>, after: &Container) -> Option<String> {
    let was_running = before.is_some_and(|before| before.state() == ContainerState::Running);
    let code = exit_code(&after.status).filter(|code| *code != 0)?;
    if !was_running || after.state() != ContainerState::Exited {
        return None;
    }
    Some(match after.labels.get(labels::PROJECT) {
        Some(project) => format!("{} in {project} stopped with exit code {code}", title(after)),
        None => format!("{} stopped with exit code {code}", title(after)),
    })
}

#[cfg(test)]
mod tests {
    use contr_sdk::api::containers::v1::{PortMapping, Protocol};

    use super::*;

    fn container(id: &str, project: Option<&str>, service: &str, state: ContainerState, status: &str) -> Container {
        let mut labels = HashMap::new();
        if let Some(project) = project {
            labels.insert(labels::PROJECT.to_owned(), project.to_owned());
            labels.insert(labels::SERVICE.to_owned(), service.to_owned());
            labels.insert(labels::NUMBER.to_owned(), "1".to_owned());
            labels.insert(
                labels::CONFIG_FILES.to_owned(),
                format!("/home/minh/src/{project}/compose.yaml"),
            );
        }
        Container {
            id: id.to_owned(),
            names: vec![format!("{}-{service}-1", project.unwrap_or("x"))],
            image: format!("{service}:latest"),
            state: state.into(),
            status: status.to_owned(),
            labels,
            ..Container::default()
        }
    }

    fn map(containers: Vec<Container>) -> BTreeMap<String, Container> {
        containers.into_iter().map(|c| (c.id.clone(), c)).collect()
    }

    #[test]
    fn groups_by_project_with_standalone_last() {
        let containers = map(vec![
            container("1", Some("shop-api"), "api", ContainerState::Running, "Up 2 hours"),
            container(
                "2",
                Some("shop-api"),
                "worker",
                ContainerState::Exited,
                "Exited (1) 14 minutes ago",
            ),
            container("3", None, "alpine", ContainerState::Exited, "Exited (0) 3 days ago"),
            container("4", Some("docs-site"), "web", ContainerState::Running, "Up 5 hours"),
        ]);
        let groups = groups(&containers, &HashMap::new(), Path::new("/home/minh"), "");
        let titles: Vec<_> = groups.iter().map(|group| group.title.as_str()).collect();
        assert_eq!(titles, ["docs-site", "shop-api", "Standalone"]);
        assert_eq!(groups[1].description, "~/src/shop-api/compose.yaml · 1 of 2 running");
        assert_eq!(groups[0].description, "~/src/docs-site/compose.yaml · 1 running");
        assert_eq!(groups[2].description, "Not part of a compose project");
    }

    #[test]
    fn failed_rows_lead_with_the_exit_status() {
        let worker = container(
            "2",
            Some("shop-api"),
            "worker",
            ContainerState::Exited,
            "Exited (1) 14 minutes ago",
        );
        let row = row(&worker, None);
        assert_eq!(row.status, StatusKind::Failed);
        assert_eq!(row.failure.as_deref(), Some("Exited (1) 14 minutes ago"));
        assert_eq!(row.subtitle, " · worker:latest");
        assert_eq!(row.title, "worker");
    }

    #[test]
    fn running_rows_show_usage_and_the_published_port() {
        let mut api = container("1", Some("shop-api"), "api", ContainerState::Running, "Up 2 hours");
        api.ports = vec![PortMapping {
            container_port: 3000,
            host_port: 8080,
            host_ip: "0.0.0.0".to_owned(),
            protocol: Protocol::Tcp.into(),
        }];
        let stats = ContainerStats {
            cpu_percent: 3.24,
            memory_usage_bytes: 142_000_000,
            ..ContainerStats::default()
        };
        let row = row(&api, Some(&stats));
        assert_eq!(row.subtitle, "api:latest · Up 2 hours · 3.2% · 142 MB");
        assert_eq!(
            row.port,
            Some(Port::Link {
                label: "localhost:8080".to_owned(),
                url: "http://localhost:8080".to_owned()
            })
        );
    }

    #[test]
    fn search_matches_titles_names_and_images() {
        let containers = map(vec![
            container("1", Some("shop-api"), "api", ContainerState::Running, "Up"),
            container("2", None, "alpine", ContainerState::Exited, "Exited (0)"),
        ]);
        let groups = groups(&containers, &HashMap::new(), Path::new("/home/minh"), "ALP");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].title, "Standalone");
    }

    #[test]
    fn summaries_and_crashes() {
        let running = container("1", Some("shop-api"), "worker", ContainerState::Running, "Up 1 minute");
        let crashed = container(
            "1",
            Some("shop-api"),
            "worker",
            ContainerState::Exited,
            "Exited (1) 1 second ago",
        );
        assert_eq!(
            crash(Some(&running), &crashed).as_deref(),
            Some("worker in shop-api stopped with exit code 1")
        );
        assert_eq!(crash(None, &crashed), None);
        assert_eq!(header_summary(&map(vec![running, crashed.clone()])), "1 stopped");
        assert_eq!(exit_code("Exited (137) 2 hours ago"), Some(137));
        assert_eq!(exit_code("Up 2 hours"), None);
    }
}
