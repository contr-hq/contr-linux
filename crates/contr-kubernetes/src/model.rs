//! What the Kubernetes page shows for each cluster state: the summary card, the start-up steps and the system pods.

use std::path::Path;
use std::time::{Duration, SystemTime};

use contr_daemon_ui::system_time;
use contr_sdk::api::kubernetes::v1::{Cluster, ClusterState, Pod, Step, StepKind, StepStatus};
use contr_ui_common::format;
use contr_ui_common::widgets::StatusKind;

/// The card at the top of the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Summary {
    pub(crate) title: &'static str,
    pub(crate) subtitle: String,
    /// `kubectl --context …`, shown in monospace after the subtitle.
    pub(crate) command: Option<String>,
    pub(crate) kind: StatusKind,
    /// The cluster is changing state; the card shows a spinner.
    pub(crate) busy: bool,
}

/// The version `kubernetes.version` names, or the default one when it names none.
pub(crate) fn chosen_version<'a>(cluster: &'a Cluster, setting: &'a str) -> &'a str {
    if setting.is_empty() {
        cluster.available_versions.first().map_or("", String::as_str)
    } else {
        setting
    }
}

pub(crate) fn summary(cluster: &Cluster, version: &str, now: SystemTime) -> Summary {
    let card = |title, subtitle: String, kind, busy| Summary {
        title,
        subtitle,
        command: None,
        kind,
        busy,
    };
    match cluster.state() {
        ClusterState::Ready => {
            let mut parts = vec![cluster.version.clone(), "1 node".to_owned()];
            if let Some(started) = system_time(cluster.started_at.as_ref()) {
                let up = now.duration_since(started).unwrap_or_default();
                parts.push(format!("up {}", format::span(up)));
            }
            Summary {
                command: (!cluster.context.is_empty()).then(|| format!("kubectl --context {}", cluster.context)),
                ..card("Cluster ready", parts.join(" · "), StatusKind::Running, false)
            }
        }
        ClusterState::Starting => card(
            "Turning on Kubernetes",
            format!("k3s {version} runs as a container on your engine. Usually under 3 minutes."),
            StatusKind::Created,
            true,
        ),
        ClusterState::Stopping => card(
            "Turning off Kubernetes",
            "The cluster’s container is stopping.".to_owned(),
            StatusKind::Created,
            true,
        ),
        ClusterState::WaitingForEngine => card(
            "Waiting for the engine",
            "The cluster starts once the engine runs.".to_owned(),
            StatusKind::Stopped,
            false,
        ),
        ClusterState::Failed => card(
            "Kubernetes couldn’t start",
            cluster.error.clone(),
            StatusKind::Failed,
            false,
        ),
        ClusterState::Disabled | ClusterState::Unspecified => card(
            "Kubernetes is off",
            "Turn it on to run a single-node k3s cluster on your engine.".to_owned(),
            StatusKind::Stopped,
            false,
        ),
    }
}

pub(crate) fn step_title(step: &Step, cluster: &Cluster, home: &Path) -> String {
    match step.kind() {
        StepKind::PullImage => "Pull the k3s image".to_owned(),
        StepKind::StartServer => "Start the server".to_owned(),
        StepKind::WaitForNode => "Wait for the node to be ready".to_owned(),
        StepKind::AddKubeconfig => format!(
            "Add the {} context to {}",
            cluster.context,
            format::home_relative(&cluster.kubeconfig, home)
        ),
        StepKind::Unspecified => "Prepare the cluster".to_owned(),
    }
}

/// `236 MB · 31 s`: what the step reported and, once it finished, how long it took.
pub(crate) fn step_detail(step: &Step) -> String {
    let took = system_time(step.started_at.as_ref())
        .zip(system_time(step.finished_at.as_ref()))
        .and_then(|(started, finished)| finished.duration_since(started).ok())
        .filter(|_| step.status() == StepStatus::Done)
        .map(seconds);
    [Some(step.detail.clone()).filter(|detail| !detail.is_empty()), took]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn seconds(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        format!("{seconds} s")
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

/// Share of the steps that are done, for the progress bar.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn progress(steps: &[Step]) -> f64 {
    if steps.is_empty() {
        return 0.0;
    }
    let done = steps.iter().filter(|step| step.status() == StepStatus::Done).count();
    done as f64 / steps.len() as f64
}

pub(crate) fn step_status(step: &Step) -> (StatusKind, &'static str) {
    match step.status() {
        StepStatus::Done => (StatusKind::Running, "Done"),
        StepStatus::Failed => (StatusKind::Failed, "Failed"),
        StepStatus::Running => (StatusKind::Paused, "Running"),
        StepStatus::Pending | StepStatus::Unspecified => (StatusKind::Created, "Waiting"),
    }
}

/// The pod's status icon and `Running · 1/1`.
pub(crate) fn pod_view(pod: &Pod) -> (StatusKind, String) {
    let kind = match pod.phase.as_str() {
        "Running" if pod.ready_containers == pod.containers => StatusKind::Running,
        "Running" => StatusKind::Paused,
        "Succeeded" => StatusKind::Stopped,
        "Failed" => StatusKind::Failed,
        _ => StatusKind::Created,
    };
    (
        kind,
        format!("{} · {}/{}", pod.phase, pod.ready_containers, pod.containers),
    )
}

/// What kubectl uses now, under the switch that changes it.
pub(crate) fn context_subtitle(cluster: &Cluster) -> String {
    if cluster.current_context.is_empty() {
        "kubectl has no current context.".to_owned()
    } else if cluster.current_context == cluster.context {
        format!("kubectl uses {} right now.", cluster.context)
    } else {
        format!(
            "kubectl uses {} right now. Contr only changes that if you turn this on.",
            cluster.current_context
        )
    }
}

/// The cluster state already follows the settings change whose etag was `awaited`.
pub(crate) fn caught_up(cluster: &Cluster, awaited: u64) -> bool {
    cluster.settings_etag.parse::<u64>().is_ok_and(|etag| etag >= awaited)
}

#[cfg(test)]
mod tests {
    use prost_types::Timestamp;

    use super::*;

    fn at(seconds: i64) -> Timestamp {
        Timestamp { seconds, nanos: 0 }
    }

    #[test]
    fn a_ready_cluster_reads_like_the_design() {
        let cluster = Cluster {
            state: ClusterState::Ready.into(),
            version: "v1.37.1+k3s1".to_owned(),
            context: "contr".to_owned(),
            started_at: Some(at(1_000)),
            ..Cluster::default()
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + 90_000);
        let summary = summary(&cluster, "v1.37.1", now);
        assert_eq!(summary.subtitle, "v1.37.1+k3s1 · 1 node · up 1 day");
        assert_eq!(summary.command.as_deref(), Some("kubectl --context contr"));
    }

    #[test]
    fn steps_show_their_detail_and_time_once_done() {
        let step = Step {
            kind: StepKind::PullImage.into(),
            status: StepStatus::Done.into(),
            detail: "236 MB".to_owned(),
            started_at: Some(at(10)),
            finished_at: Some(at(41)),
        };
        assert_eq!(step_detail(&step), "236 MB · 31 s");
        let running = Step {
            status: StepStatus::Running.into(),
            detail: "Publishing 127.0.0.1:6443".to_owned(),
            started_at: Some(at(41)),
            ..Step::default()
        };
        assert_eq!(step_detail(&running), "Publishing 127.0.0.1:6443");
        assert!((progress(&[step, running]) - 0.5).abs() < f64::EPSILON);

        let cluster = Cluster {
            context: "contr".to_owned(),
            kubeconfig: "/home/minh/.kube/config".to_owned(),
            ..Cluster::default()
        };
        let add = Step {
            kind: StepKind::AddKubeconfig.into(),
            ..Step::default()
        };
        assert_eq!(
            step_title(&add, &cluster, Path::new("/home/minh")),
            "Add the contr context to ~/.kube/config"
        );
    }

    #[test]
    fn pods_and_contexts_read_plainly() {
        let pod = Pod {
            name: "coredns".to_owned(),
            phase: "Running".to_owned(),
            ready_containers: 1,
            containers: 1,
        };
        assert_eq!(pod_view(&pod), (StatusKind::Running, "Running · 1/1".to_owned()));
        let cluster = Cluster {
            context: "contr".to_owned(),
            current_context: "staging-eu".to_owned(),
            settings_etag: "7".to_owned(),
            ..Cluster::default()
        };
        assert_eq!(
            context_subtitle(&cluster),
            "kubectl uses staging-eu right now. Contr only changes that if you turn this on."
        );
        assert!(caught_up(&cluster, 7));
        assert!(!caught_up(&cluster, 8));
        assert_eq!(chosen_version(&cluster, "v1.36.5"), "v1.36.5");
    }
}
