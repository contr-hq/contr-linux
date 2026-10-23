//! Work the daemon owns: pulls, pushes, Compose actions, image imports, bundles, fixes and engine operations. It goes
//! on after the page, window or client that started it goes away.

use std::path::Path;
use std::sync::Arc;

use contr_core::product;
use contr_sdk::api::compose::v1::Action;
use contr_sdk::api::diagnostics::v1::{RemediationOutcome, create_bundle_response, run_remediation_response};
use contr_sdk::api::engine::v1::ResetScope;
use contr_sdk::api::operations::v1::{
    CancelOperationRequest, FollowOperationRequest, OperationRequest, OperationState, StartOperationRequest,
    WatchOperationsRequest, follow_operation_response, operation_request, watch_operations_response,
};
use contr_sdk::{Client, Error};
use relm4::gtk::glib;
use tokio::sync::watch;

pub use contr_sdk::api::operations::v1::Operation;
pub use follow_operation_response::Event as FollowEvent;
pub use operation_request::Kind;

use crate::daemon::EngineAction;
use crate::{RETRY_DELAY, Status, describe, remediation_label};

/// Every operation the daemon keeps, newest first; empty while no daemon is connected.
#[derive(Clone, Debug, Default)]
pub struct Activity {
    pub operations: Vec<Operation>,
}

impl Activity {
    pub fn running(&self) -> impl Iterator<Item = &Operation> {
        self.operations
            .iter()
            .filter(|operation| operation.state() == OperationState::Running)
    }

    /// The running operation whose request `matches`.
    pub fn running_where(&self, matches: impl Fn(&Kind) -> bool) -> Option<&Operation> {
        self.running().find(|operation| kind(operation).is_some_and(&matches))
    }

    pub fn get(&self, id: &str) -> Option<&Operation> {
        self.operations.iter().find(|operation| operation.id == id)
    }

    fn apply(&mut self, event: watch_operations_response::Event) {
        match event {
            watch_operations_response::Event::Snapshot(snapshot) => self.operations = snapshot.operations,
            watch_operations_response::Event::Upserted(operation) => {
                self.operations.retain(|known| known.id != operation.id);
                self.operations.push(operation);
            }
            watch_operations_response::Event::RemovedId(id) => self.operations.retain(|known| known.id != id),
        }
        self.operations.sort_by(|a, b| {
            let started = |operation: &Operation| operation.started_at.map(|at| (at.seconds, at.nanos));
            started(b).cmp(&started(a))
        });
    }
}

pub fn kind(operation: &Operation) -> Option<&Kind> {
    operation.request.as_ref().and_then(|request| request.kind.as_ref())
}

/// The engine operation `kind` runs, for the engine card.
pub fn engine_action(kind: &Kind) -> Option<EngineAction> {
    match kind {
        Kind::StartEngine(_) => Some(EngineAction::Start),
        Kind::StopEngine(_) => Some(EngineAction::Stop),
        Kind::RestartEngine(_) => Some(EngineAction::Restart),
        Kind::ResetEngine(request) => Some(EngineAction::Reset(request.scope())),
        _ => None,
    }
}

fn compose_verb(action: Action) -> &'static str {
    match action {
        Action::Up => "Up",
        Action::Pull => "Pull",
        Action::Start => "Start",
        Action::Stop => "Stop",
        Action::Restart => "Restart",
        Action::Down => "Down",
        Action::Unspecified => "Action",
    }
}

fn images(count: usize) -> String {
    if count == 1 {
        "1 image".to_owned()
    } else {
        format!("{count} images")
    }
}

/// What the operation does, such as `Pull nginx:alpine`, `Compose Up · shop-api` or `Install an Engine`.
pub fn title(operation: &Operation) -> String {
    match kind(operation) {
        Some(Kind::PullImage(request)) => format!("Pull {}", request.reference),
        Some(Kind::PushImage(request)) => format!("Push {}", request.reference),
        Some(Kind::ComposeAction(request)) => {
            format!("Compose {} · {}", compose_verb(request.action()), request.project)
        }
        Some(Kind::ImportImages(request)) => format!("Import {} into Kubernetes", images(request.images.len())),
        Some(Kind::CreateBundle(_)) => "Diagnostics bundle".to_owned(),
        Some(Kind::RunRemediation(request)) => remediation_label(&contr_sdk::api::common::v1::Remediation {
            action_id: request.action_id.clone(),
            requires_elevation: false,
        })
        .trim_end_matches('…')
        .to_owned(),
        Some(Kind::StartEngine(_)) => "Start the engine".to_owned(),
        Some(Kind::StopEngine(_)) => "Stop the engine".to_owned(),
        Some(Kind::RestartEngine(_)) => "Restart the engine".to_owned(),
        Some(Kind::ResetEngine(request)) => match request.scope() {
            ResetScope::Factory => "Reset the engine to factory settings".to_owned(),
            _ => "Reset the engine".to_owned(),
        },
        None => "Operation".to_owned(),
    }
}

/// A push or pull failed because the registry wants credentials `docker login` stores.
fn needs_login(message: &str) -> bool {
    let message = message.to_lowercase();
    [
        "no basic auth credentials",
        "unauthorized",
        "authentication required",
        "denied",
    ]
    .iter()
    .any(|sign| message.contains(sign))
}

/// `docker login registry.corp` for the registry `reference` names, or `docker login` for Docker Hub.
fn login_command(reference: &str) -> String {
    match reference.split_once('/') {
        Some((host, _)) if host.contains(['.', ':']) || host == "localhost" => format!("docker login {host}"),
        _ => "docker login".to_owned(),
    }
}

fn succeeded(operation: &Operation) -> String {
    match kind(operation) {
        Some(Kind::PullImage(request)) => format!("Pulled {}", request.reference),
        Some(Kind::PushImage(request)) => format!("Pushed {}", request.reference),
        Some(Kind::ComposeAction(request)) => {
            let project = &request.project;
            match request.action() {
                Action::Up => format!("{project} is up"),
                Action::Pull => format!("Pulled the images of {project}"),
                Action::Start => format!("Started {project}"),
                Action::Stop => format!("Stopped {project}"),
                Action::Restart => format!("Restarted {project}"),
                Action::Down => format!("Took {project} down"),
                Action::Unspecified => format!("{}: done", title(operation)),
            }
        }
        Some(Kind::ImportImages(request)) => format!("Imported {} into Kubernetes", images(request.images.len())),
        Some(Kind::CreateBundle(_)) => "The diagnostics bundle is ready".to_owned(),
        Some(Kind::StartEngine(_)) => "The engine is running".to_owned(),
        Some(Kind::StopEngine(_)) => "The engine stopped".to_owned(),
        Some(Kind::RestartEngine(_)) => "The engine restarted".to_owned(),
        Some(Kind::ResetEngine(_)) => "The engine was reset".to_owned(),
        Some(Kind::RunRemediation(_)) | None => format!("{}: done", title(operation)),
    }
}

/// One line on how a finished operation ended, for a toast or notification.
pub fn outcome(operation: &Operation) -> String {
    let title = title(operation);
    match operation.state() {
        OperationState::Succeeded => succeeded(operation),
        OperationState::Cancelled => format!("{title}: cancelled"),
        OperationState::Failed => match (&operation.error, kind(operation)) {
            (Some(error), Some(Kind::PushImage(request))) if needs_login(&error.message) => format!(
                "{title} failed: the registry refused it. Run {} and try again.",
                login_command(&request.reference)
            ),
            (Some(error), _) if !error.message.is_empty() => format!("{title} failed: {}", error.message),
            _ => format!("{title} failed"),
        },
        OperationState::Running | OperationState::Unspecified => title,
    }
}

/// How a finished operation ended, reading its events where its final state alone does not say: an import that
/// skipped images, or a fix that needs a restart or a new login.
pub async fn report(client: &Client, operation: &Operation) -> String {
    if operation.state() != OperationState::Succeeded {
        return outcome(operation);
    }
    match kind(operation) {
        Some(Kind::ImportImages(_)) => {
            let mut skipped = Vec::new();
            let _ = follow(client, &operation.id, |event| {
                if let FollowEvent::ImportImages(response) = event
                    && !response.error.is_empty()
                {
                    skipped.push(format!("{}: {}", response.image, response.error));
                }
            })
            .await;
            match skipped.as_slice() {
                [] => outcome(operation),
                [one] => format!("Couldn’t import {one}"),
                many => format!("Couldn’t import {} of the images", many.len()),
            }
        }
        Some(Kind::RunRemediation(_)) => match remediation_outcome(client, &operation.id).await {
            Some(then) if then.logout_required => format!("{}. Log out and back in to finish.", outcome(operation)),
            Some(then) if then.restart_required => format!("{}. Restart the computer to finish.", outcome(operation)),
            _ => outcome(operation),
        },
        _ => outcome(operation),
    }
}

/// What the user must still do after fix operation `id` succeeded; `None` when it did not.
pub async fn remediation_outcome(client: &Client, id: &str) -> Option<RemediationOutcome> {
    let mut then = None;
    let finished = follow(client, id, |event| {
        if let FollowEvent::RunRemediation(response) = event
            && let Some(run_remediation_response::Event::Outcome(outcome)) = response.event
        {
            then = Some(outcome);
        }
    })
    .await
    .ok()?;
    then.filter(|_| finished.state() == OperationState::Succeeded)
}

/// Whether the window reports how `operation` ended: not for a fix whose prompt the user dismissed, nor for an engine
/// start, stop or restart that succeeded, which the engine card shows.
pub fn reported(operation: &Operation) -> bool {
    !matches!(
        (operation.state(), kind(operation)),
        (OperationState::Cancelled, Some(Kind::RunRemediation(_)))
            | (
                OperationState::Succeeded,
                Some(Kind::StartEngine(_) | Kind::StopEngine(_) | Kind::RestartEngine(_))
            )
    )
}

/// File name the save dialog suggests for a diagnostics bundle made on `date`, such as `contr-diagnostics-2026-10-22.zip`.
pub fn bundle_name(date: &str) -> String {
    if date.is_empty() {
        format!("{}-diagnostics.zip", product::SLUG)
    } else {
        format!("{}-diagnostics-{date}.zip", product::SLUG)
    }
}

/// [`bundle_name`] for today.
pub fn todays_bundle_name() -> String {
    let date = glib::DateTime::now_local()
        .and_then(|now| now.format("%Y-%m-%d"))
        .map(String::from)
        .unwrap_or_default();
    bundle_name(&date)
}

/// Writes the zip of bundle operation `id` to `path` once the bundle is whole, passing each step to `progress`; a
/// bundle that fails leaves nothing at `path`.
pub async fn save_bundle(
    client: &Client,
    id: &str,
    path: &Path,
    mut progress: impl FnMut(String),
) -> Result<(), String> {
    let mut zip = Vec::new();
    let finished = follow(client, id, |event| {
        let FollowEvent::CreateBundle(response) = event else {
            return;
        };
        match response.event {
            Some(create_bundle_response::Event::Progress(step)) => progress(step.message),
            Some(create_bundle_response::Event::Chunk(bytes)) => zip.extend_from_slice(&bytes),
            None => {}
        }
    })
    .await
    .map_err(|error| describe(&error))?;
    if finished.state() != OperationState::Succeeded {
        return Err(outcome(&finished));
    }
    if let Err(error) = tokio::fs::write(path, &zip).await {
        let _ = tokio::fs::remove_file(path).await;
        return Err(error.to_string());
    }
    Ok(())
}

/// The operations of `previous` that have finished in `current`.
pub fn finished_since<'a>(previous: &Activity, current: &'a Activity) -> Vec<&'a Operation> {
    current
        .operations
        .iter()
        .filter(|operation| operation.state() != OperationState::Running)
        .filter(|operation| {
            previous
                .get(&operation.id)
                .is_some_and(|before| before.state() == OperationState::Running)
        })
        .collect()
}

/// Starts `kind`, or returns the operation already running the same work.
pub async fn start(client: &Client, kind: Kind) -> Result<Operation, Error> {
    let request = StartOperationRequest {
        request: Some(OperationRequest { kind: Some(kind) }),
    };
    client
        .operations()
        .start_operation(request)
        .await?
        .into_inner()
        .operation
        .ok_or(Error::Protocol("a started operation without the operation"))
}

/// Replays the operation's events, then follows it, and returns it in its final state. Dropping the future stops
/// following, not the operation.
pub async fn follow(client: &Client, id: &str, mut on_event: impl FnMut(FollowEvent)) -> Result<Operation, Error> {
    let request = FollowOperationRequest { id: id.to_owned() };
    let mut stream = client.operations().follow_operation(request).await?.into_inner();
    while let Some(response) = stream.message().await? {
        match response.event {
            Some(FollowEvent::Finished(operation)) => return Ok(operation),
            Some(event) => on_event(event),
            None => {}
        }
    }
    Err(Error::Protocol("an operation's events ended before it finished"))
}

pub async fn cancel(client: &Client, id: &str) -> Result<(), Error> {
    client
        .operations()
        .cancel_operation(CancelOperationRequest { id: id.to_owned() })
        .await?;
    Ok(())
}

/// Keeps `activity` current while a daemon is connected, starting over after every reconnect, and reports a running
/// engine operation in the status, whichever client started it.
pub(crate) async fn follow_activity(state: Arc<watch::Sender<Status>>, activity: Arc<watch::Sender<Activity>>) {
    let mut status = state.subscribe();
    loop {
        let (client, connection) = loop {
            {
                let current = status.borrow_and_update();
                if let Some(client) = current.client() {
                    break (client.clone(), current.connection);
                }
            }
            if status.changed().await.is_err() {
                return;
            }
        };
        let watch = async {
            let Ok(stream) = client.operations().watch_operations(WatchOperationsRequest {}).await else {
                return;
            };
            let mut stream = stream.into_inner();
            while let Ok(Some(response)) = stream.message().await {
                if let Some(event) = response.event {
                    activity.send_modify(|activity| activity.apply(event));
                    let engine = activity.borrow().running().find_map(|operation| {
                        let action = kind(operation).and_then(engine_action)?;
                        Some((
                            action,
                            operation.progress.as_ref().map(|progress| progress.message.clone()),
                        ))
                    });
                    state.send_if_modified(|status| {
                        let changed = status.action != engine;
                        status.action.clone_from(&engine);
                        changed
                    });
                }
            }
        };
        let left = async {
            loop {
                if status.changed().await.is_err() {
                    return;
                }
                if status.borrow().connection != connection {
                    return;
                }
            }
        };
        tokio::select! {
            () = watch => tokio::time::sleep(RETRY_DELAY).await,
            () = left => {}
        }
        activity.send_modify(|activity| activity.operations.clear());
    }
}

#[cfg(test)]
mod tests {
    use contr_sdk::api::compose::v1::RunActionRequest;
    use contr_sdk::api::images::v1::PullImageRequest;
    use contr_sdk::api::operations::v1::OperationError;
    use prost_types::Timestamp;

    use super::*;

    fn operation(id: &str, kind: Kind, state: OperationState, started: i64) -> Operation {
        Operation {
            id: id.to_owned(),
            request: Some(OperationRequest { kind: Some(kind) }),
            state: state.into(),
            started_at: Some(Timestamp {
                seconds: started,
                nanos: 0,
            }),
            ..Operation::default()
        }
    }

    fn pull(reference: &str) -> Kind {
        Kind::PullImage(PullImageRequest {
            reference: reference.to_owned(),
            platform: String::new(),
        })
    }

    #[test]
    fn titles_and_outcomes_read_plainly() {
        let up = operation(
            "a",
            Kind::ComposeAction(RunActionRequest {
                project: "shop-api".to_owned(),
                action: Action::Up.into(),
                ..RunActionRequest::default()
            }),
            OperationState::Succeeded,
            1,
        );
        assert_eq!(title(&up), "Compose Up · shop-api");
        assert_eq!(outcome(&up), "shop-api is up");
        let failed = Operation {
            error: Some(OperationError {
                message: "manifest unknown".to_owned(),
                ..OperationError::default()
            }),
            ..operation("b", pull("nginx:nope"), OperationState::Failed, 2)
        };
        assert_eq!(outcome(&failed), "Pull nginx:nope failed: manifest unknown");
    }

    #[test]
    fn a_refused_push_names_the_docker_login_to_run() {
        let refused = Operation {
            error: Some(OperationError {
                message: "push access denied: no basic auth credentials".to_owned(),
                ..OperationError::default()
            }),
            ..operation(
                "c",
                Kind::PushImage(contr_sdk::api::images::v1::PushImageRequest {
                    reference: "registry.corp/team/app:1.2".to_owned(),
                }),
                OperationState::Failed,
                3,
            )
        };
        assert_eq!(
            outcome(&refused),
            "Push registry.corp/team/app:1.2 failed: the registry refused it. Run docker login registry.corp and try again."
        );
        assert_eq!(login_command("minh/app:1.0"), "docker login");
        assert_eq!(login_command("localhost/app"), "docker login localhost");
        assert!(!needs_login("manifest unknown"));
    }

    #[test]
    fn bundles_are_named_by_date() {
        assert_eq!(
            bundle_name("2026-10-22"),
            format!("{}-diagnostics-2026-10-22.zip", product::SLUG)
        );
    }

    #[test]
    fn dismissed_fixes_and_engine_starts_go_unreported() {
        let fix = Kind::RunRemediation(contr_sdk::api::diagnostics::v1::RunRemediationRequest {
            action_id: "join-docker-group".to_owned(),
        });
        assert!(!reported(&operation("a", fix.clone(), OperationState::Cancelled, 1)));
        assert!(reported(&operation("b", fix, OperationState::Failed, 2)));
        let start = Kind::StartEngine(contr_sdk::api::engine::v1::StartRequest {});
        assert!(!reported(&operation("c", start.clone(), OperationState::Succeeded, 3)));
        assert!(reported(&operation("d", start, OperationState::Failed, 4)));
    }

    #[test]
    fn the_activity_keeps_the_newest_first_and_reports_what_finished() {
        let mut activity = Activity::default();
        activity.apply(watch_operations_response::Event::Upserted(operation(
            "old",
            pull("a"),
            OperationState::Running,
            1,
        )));
        activity.apply(watch_operations_response::Event::Upserted(operation(
            "new",
            pull("b"),
            OperationState::Running,
            2,
        )));
        assert_eq!(activity.operations[0].id, "new");
        let before = activity.clone();
        activity.apply(watch_operations_response::Event::Upserted(operation(
            "old",
            pull("a"),
            OperationState::Succeeded,
            1,
        )));
        let finished: Vec<&str> = finished_since(&before, &activity)
            .iter()
            .map(|operation| operation.id.as_str())
            .collect();
        assert_eq!(finished, ["old"]);
        assert!(
            activity
                .running_where(|kind| matches!(kind, Kind::PullImage(request) if request.reference == "b"))
                .is_some()
        );
        activity.apply(watch_operations_response::Event::RemovedId("new".to_owned()));
        assert_eq!(activity.operations.len(), 1);
    }
}
