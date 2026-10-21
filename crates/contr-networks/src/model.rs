//! What the Networks page shows: user networks with their members, and the engine's built-in networks.

use std::collections::BTreeMap;

use contr_daemon_ui::Attachment;
use contr_sdk::api::networks::v1::Network;

/// Networks every engine creates; they cannot be removed.
pub(crate) const BUILT_IN: [&str; 3] = ["bridge", "host", "none"];
const COMPOSE_NETWORK_LABEL: &str = "com.docker.compose.network";
const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) address: String,
    pub(crate) running: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetworkView {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) subtitle: String,
    pub(crate) members: Vec<Member>,
    /// The Compose project that created the network and removes it on `down`.
    pub(crate) compose_project: Option<String>,
}

fn containers(count: usize) -> String {
    match count {
        0 => "no containers".to_owned(),
        1 => "1 container".to_owned(),
        count => format!("{count} containers"),
    }
}

fn members(network: &Network, attachments: &[Attachment]) -> Vec<Member> {
    let mut members: Vec<Member> = attachments
        .iter()
        .flat_map(|attachment| {
            attachment
                .networks
                .iter()
                .filter(|joined| joined.network == network.name || joined.network == network.id)
                .map(|joined| Member {
                    id: attachment.id.clone(),
                    name: attachment.name.clone(),
                    address: joined.ipv4_address.clone(),
                    running: attachment.running,
                })
        })
        .collect();
    members.sort_by(|a, b| a.name.cmp(&b.name));
    members
}

/// User networks sorted by name, then the built-in ones in their usual order.
pub(crate) fn sections(
    networks: &BTreeMap<String, Network>,
    attachments: &[Attachment],
) -> (Vec<NetworkView>, Vec<NetworkView>) {
    let mut yours = Vec::new();
    let mut built_in = Vec::new();
    for network in networks.values() {
        let members = members(network, attachments);
        let subnet = network.subnets.first().map(|subnet| subnet.cidr.clone());
        let builtin = BUILT_IN.contains(&network.name.as_str());
        let subtitle = match network.name.as_str() {
            "host" if builtin => "Shares this computer’s network".to_owned(),
            "none" if builtin => "No network".to_owned(),
            _ => {
                let mut parts = Vec::new();
                if !builtin {
                    parts.push(network.driver.clone());
                }
                if let Some(subnet) = &subnet {
                    parts.push(subnet.clone());
                }
                if network.internal {
                    parts.push("internal".to_owned());
                }
                parts.push(containers(members.len()));
                parts.join(" · ")
            }
        };
        let view = NetworkView {
            id: network.id.clone(),
            name: network.name.clone(),
            subtitle,
            members,
            compose_project: network
                .labels
                .get(COMPOSE_NETWORK_LABEL)
                .and(network.labels.get(COMPOSE_PROJECT_LABEL))
                .cloned(),
        };
        if builtin {
            built_in.push(view);
        } else {
            yours.push(view);
        }
    }
    yours.sort_by(|a, b| a.name.cmp(&b.name));
    built_in.sort_by_key(|view| BUILT_IN.iter().position(|name| *name == view.name));
    (yours, built_in)
}

/// Containers that can join the network `view` shows: every container not on it yet, as `(id, name)` by name.
pub(crate) fn connectable(view: &NetworkView, attachments: &[Attachment]) -> Vec<(String, String)> {
    let mut choices: Vec<(String, String)> = attachments
        .iter()
        .filter(|attachment| !view.members.iter().any(|member| member.id == attachment.id))
        .map(|attachment| (attachment.id.clone(), attachment.name.clone()))
        .collect();
    choices.sort_by(|a, b| a.1.cmp(&b.1));
    choices
}

/// `api, api.local` as the aliases `api` and `api.local`.
pub(crate) fn aliases(text: &str) -> Vec<String> {
    text.split([',', ' '])
        .map(str::trim)
        .filter(|alias| !alias.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use contr_sdk::api::containers::v1::NetworkAttachment;
    use contr_sdk::api::networks::v1::Subnet;

    use super::*;

    fn network(name: &str, subnet: Option<&str>, project: Option<&str>) -> Network {
        let mut labels = HashMap::new();
        if let Some(project) = project {
            labels.insert(COMPOSE_NETWORK_LABEL.to_owned(), "default".to_owned());
            labels.insert(COMPOSE_PROJECT_LABEL.to_owned(), project.to_owned());
        }
        Network {
            id: format!("id-{name}"),
            name: name.to_owned(),
            driver: "bridge".to_owned(),
            subnets: subnet
                .map(|cidr| Subnet {
                    cidr: cidr.to_owned(),
                    gateway: String::new(),
                })
                .into_iter()
                .collect(),
            labels,
            ..Network::default()
        }
    }

    #[test]
    fn splits_user_and_built_in_networks() {
        let networks: BTreeMap<String, Network> = [
            network("shop-api_default", Some("172.20.0.0/16"), Some("shop-api")),
            network("host", None, None),
            network("bridge", Some("172.17.0.0/16"), None),
            network("none", None, None),
        ]
        .into_iter()
        .map(|network| (network.id.clone(), network))
        .collect();
        let api = Attachment {
            id: "c1".to_owned(),
            name: "api".to_owned(),
            running: true,
            mounts: Vec::new(),
            networks: vec![NetworkAttachment {
                network: "shop-api_default".to_owned(),
                ipv4_address: "172.20.0.4".to_owned(),
                ..NetworkAttachment::default()
            }],
        };
        let (yours, built_in) = sections(&networks, &[api]);
        assert_eq!(yours.len(), 1);
        assert_eq!(yours[0].subtitle, "bridge · 172.20.0.0/16 · 1 container");
        assert_eq!(yours[0].compose_project.as_deref(), Some("shop-api"));
        assert_eq!(yours[0].members[0].address, "172.20.0.4");
        let names: Vec<_> = built_in.iter().map(|view| view.name.as_str()).collect();
        assert_eq!(names, ["bridge", "host", "none"]);
        assert_eq!(built_in[0].subtitle, "172.17.0.0/16 · no containers");
        assert_eq!(built_in[1].subtitle, "Shares this computer’s network");
    }

    #[test]
    fn only_containers_off_the_network_can_join_it() {
        let view = NetworkView {
            id: "n".to_owned(),
            name: "shop".to_owned(),
            subtitle: String::new(),
            members: vec![Member {
                id: "a".to_owned(),
                name: "api".to_owned(),
                address: String::new(),
                running: true,
            }],
            compose_project: None,
        };
        let attachment = |id: &str, name: &str| Attachment {
            id: id.to_owned(),
            name: name.to_owned(),
            running: true,
            mounts: Vec::new(),
            networks: Vec::new(),
        };
        let attachments = [attachment("a", "api"), attachment("w", "worker"), attachment("d", "db")];
        assert_eq!(
            connectable(&view, &attachments),
            [("d".to_owned(), "db".to_owned()), ("w".to_owned(), "worker".to_owned())]
        );
        assert_eq!(aliases(" api, api.local  cache "), ["api", "api.local", "cache"]);
        assert!(aliases("  ").is_empty());
    }
}
