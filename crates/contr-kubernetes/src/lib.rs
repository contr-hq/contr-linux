//! The Kubernetes section: the single-node k3s cluster the daemon runs on the engine, its start-up, settings and
//! maintenance.

mod model;
mod page;

pub use page::{KubernetesCmd, KubernetesMsg, KubernetesOutput, KubernetesPage};
