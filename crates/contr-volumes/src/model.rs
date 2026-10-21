//! What the Volumes page shows: volumes split by whether a container mounts them.

use std::collections::BTreeMap;
use std::time::SystemTime;

use contr_daemon_ui::Attachment;
use contr_sdk::api::containers::v1::MountKind;
use contr_sdk::api::volumes::v1::Volume;

const PROJECT_LABEL: &str = "com.docker.compose.project";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Usage {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) destination: String,
    pub(crate) running: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VolumeView {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) driver: String,
    pub(crate) created: Option<SystemTime>,
    pub(crate) users: Vec<Usage>,
}

impl VolumeView {
    /// Why Delete is unavailable, naming the containers that hold the volume.
    pub(crate) fn delete_hint(&self) -> Option<String> {
        let names = |running: bool| -> Vec<&str> {
            self.users
                .iter()
                .filter(|user| user.running == running)
                .map(|user| user.name.as_str())
                .collect()
        };
        let running = names(true);
        let stopped = names(false);
        if !running.is_empty() {
            Some(format!("Stop {} before deleting this volume.", running.join(", ")))
        } else if !stopped.is_empty() {
            Some(format!("Delete {} before deleting this volume.", stopped.join(", ")))
        } else {
            None
        }
    }
}

/// An anonymous volume's name: 64 hexadecimal digits the engine generated.
pub(crate) fn is_anonymous(name: &str) -> bool {
    name.len() == 64 && name.chars().all(|c| c.is_ascii_hexdigit())
}

/// `7c41e09a…b2f3` for an anonymous volume; other names as they are.
pub(crate) fn display_name(name: &str) -> String {
    if is_anonymous(name) {
        format!("{}…{}", &name[..8], &name[60..])
    } else {
        name.to_owned()
    }
}

/// The directory the engine keeps volumes in, from a mountpoint such as `/var/lib/docker/volumes/<name>/_data`.
pub(crate) fn storage_dir(volumes: &BTreeMap<String, Volume>) -> Option<String> {
    volumes.values().find_map(|volume| {
        let suffix = format!("/{}/_data", volume.name);
        volume
            .mountpoint
            .strip_suffix(&suffix)
            .filter(|dir| !dir.is_empty())
            .map(str::to_owned)
    })
}

/// Volumes some container mounts, then the rest, each sorted by name.
pub(crate) fn sections(
    volumes: &BTreeMap<String, Volume>,
    attachments: &[Attachment],
    created: impl Fn(&Volume) -> Option<SystemTime>,
    query: &str,
) -> (Vec<VolumeView>, Vec<VolumeView>) {
    let query = query.trim().to_lowercase();
    let mut used = Vec::new();
    let mut unused = Vec::new();
    for volume in volumes.values() {
        if !query.is_empty() && !volume.name.to_lowercase().contains(&query) {
            continue;
        }
        let users: Vec<Usage> = attachments
            .iter()
            .flat_map(|attachment| {
                attachment
                    .mounts
                    .iter()
                    .filter(|mount| mount.kind() == MountKind::Volume && mount.name == volume.name)
                    .map(|mount| Usage {
                        id: attachment.id.clone(),
                        name: attachment.name.clone(),
                        destination: mount.destination.clone(),
                        running: attachment.running,
                    })
            })
            .collect();
        let subtitle = match volume.labels.get(PROJECT_LABEL) {
            Some(project) => project.clone(),
            None if is_anonymous(&volume.name) => "anonymous".to_owned(),
            None => format!("{} driver", volume.driver),
        };
        let view = VolumeView {
            name: volume.name.clone(),
            title: display_name(&volume.name),
            subtitle,
            driver: volume.driver.clone(),
            created: created(volume),
            users,
        };
        if view.users.is_empty() {
            unused.push(view);
        } else {
            used.push(view);
        }
    }
    (used, unused)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use contr_sdk::api::containers::v1::Mount;

    use super::*;

    const ANONYMOUS: &str = "7c41e09a0000000000000000000000000000000000000000000000000000b2f3";

    fn volume(name: &str, project: Option<&str>) -> Volume {
        let mut labels = HashMap::new();
        if let Some(project) = project {
            labels.insert(PROJECT_LABEL.to_owned(), project.to_owned());
        }
        Volume {
            name: name.to_owned(),
            driver: "local".to_owned(),
            mountpoint: format!("/var/lib/docker/volumes/{name}/_data"),
            labels,
            ..Volume::default()
        }
    }

    #[test]
    fn splits_mounted_volumes_from_the_rest() {
        let volumes: BTreeMap<String, Volume> = [
            volume("shop-api_pgdata", Some("shop-api")),
            volume(ANONYMOUS, None),
            volume("scratch", None),
        ]
        .into_iter()
        .map(|volume| (volume.name.clone(), volume))
        .collect();
        let db = Attachment {
            id: "c1".to_owned(),
            name: "db".to_owned(),
            running: true,
            mounts: vec![Mount {
                kind: MountKind::Volume.into(),
                name: "shop-api_pgdata".to_owned(),
                destination: "/var/lib/postgresql/data".to_owned(),
                ..Mount::default()
            }],
            networks: Vec::new(),
        };
        let (used, unused) = sections(&volumes, &[db], |_| None, "");
        assert_eq!(used.len(), 1);
        assert_eq!(used[0].subtitle, "shop-api");
        assert_eq!(
            used[0].delete_hint().as_deref(),
            Some("Stop db before deleting this volume.")
        );
        assert_eq!(unused.len(), 2);
        assert_eq!(unused[0].title, "7c41e09a…b2f3");
        assert_eq!(unused[0].subtitle, "anonymous");
        assert_eq!(unused[1].subtitle, "local driver");
        assert_eq!(storage_dir(&volumes).as_deref(), Some("/var/lib/docker/volumes"));
    }
}
