use std::collections::HashMap;

use contr_sdk::Client;
use contr_sdk::api::containers::v1::{
    Container, ContainerState, InspectContainerRequest, Mount, NetworkAttachment, WatchContainersRequest,
    watch_containers_response,
};
use contr_sdk::api::daemon::v1::EngineInfo;

/// The volumes a container mounts and the networks it joins; only `InspectContainer` reports them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attachment {
    pub id: String,
    /// The Compose service name for Compose containers, else the container name.
    pub name: String,
    pub running: bool,
    pub mounts: Vec<Mount>,
    pub networks: Vec<NetworkAttachment>,
}

/// Follows the engine's containers, inspects each one that appears or changes, and passes every container's
/// attachments to `emit` after each change. Returns when the watch ends.
pub async fn follow_attachments(client: Client, emit: impl Fn(Vec<Attachment>)) {
    let Ok(stream) = client.containers().watch_containers(WatchContainersRequest {}).await else {
        return;
    };
    let mut stream = stream.into_inner();
    let mut known: HashMap<String, Attachment> = HashMap::new();
    while let Ok(Some(response)) = stream.message().await {
        match response.event {
            Some(watch_containers_response::Event::Snapshot(snapshot)) => {
                known.clear();
                for container in snapshot.containers {
                    if let Some(attachment) = inspect(&client, &container).await {
                        known.insert(container.id.clone(), attachment);
                    }
                }
            }
            Some(watch_containers_response::Event::Upserted(container)) => {
                match inspect(&client, &container).await {
                    Some(attachment) => known.insert(container.id.clone(), attachment),
                    None => known.remove(&container.id),
                };
            }
            Some(watch_containers_response::Event::RemovedId(id)) => {
                known.remove(&id);
            }
            None => continue,
        }
        let mut all: Vec<Attachment> = known.values().cloned().collect();
        all.sort_by(|a, b| a.name.cmp(&b.name));
        emit(all);
    }
}

async fn inspect(client: &Client, container: &Container) -> Option<Attachment> {
    let request = InspectContainerRequest {
        id: container.id.clone(),
    };
    let details = client
        .containers()
        .inspect_container(request)
        .await
        .ok()?
        .into_inner()
        .details?;
    let name = container
        .labels
        .get("com.docker.compose.service")
        .cloned()
        .or_else(|| container.names.first().cloned())
        .unwrap_or_else(|| container.id.clone());
    Some(Attachment {
        id: container.id.clone(),
        name,
        running: container.state() == ContainerState::Running,
        mounts: details.mounts,
        networks: details.networks,
    })
}

/// The engine's product name and major version, such as `System Docker 28`.
pub fn engine_name(info: &EngineInfo) -> String {
    let kind = engine_kind(info);
    match info.version.split('.').next().filter(|major| !major.is_empty()) {
        Some(major) => format!("{kind} {major}"),
        None => kind.to_owned(),
    }
}

/// The engine's product name without its version, such as `System Docker`.
pub fn engine_kind(info: &EngineInfo) -> &'static str {
    if info.endpoint.contains("podman") {
        "Podman"
    } else if info.rootless {
        "Rootless Docker"
    } else {
        "System Docker"
    }
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
        assert_eq!(
            engine_kind(&info("unix:///var/run/docker.sock", false)),
            "System Docker"
        );
    }
}
