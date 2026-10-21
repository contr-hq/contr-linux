use std::future::Future;
use std::sync::Arc;
use std::time::SystemTime;

use contr_sdk::api::common::v1::{ErrorReason, Progress};
use contr_sdk::api::daemon::v1::{BackendKind, Capability, EngineInfo, HandshakeResponse};
use contr_sdk::api::engine::v1::{
    EngineState, RestartRequest, StartRequest, StopRequest, WatchStateRequest, restart_response, start_response,
    stop_response,
};
use contr_sdk::{Client, ClientIdentity, Error};
use tokio::sync::watch;

use crate::{RETRY_DELAY, describe};

/// How the client reaches the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Connected,
    /// Nothing answered and the daemon could not be started; holds the reason.
    Unreachable(String),
}

/// Engine lifecycle operation the user starts from the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EngineAction {
    Start,
    Stop,
    Restart,
}

/// What the client knows about the daemon and its engine at one moment.
#[derive(Clone, Debug)]
pub struct Status {
    pub link: Link,
    /// Increases with every new connection, so a watcher can tell a reconnect from a state change.
    pub connection: u64,
    client: Option<Client>,
    pub handshake: Option<HandshakeResponse>,
    /// `Unspecified` until the daemon reports the state.
    pub engine: EngineState,
    /// Set only while `engine` is `Failed`.
    pub failure: ErrorReason,
    /// Set only while `engine` is `Running`.
    pub engine_info: Option<EngineInfo>,
    /// When `engine` last changed; `None` until this client sees a change.
    pub changed_at: Option<SystemTime>,
    /// The last stop came from this client.
    pub stopped_by_user: bool,
    /// The engine operation this client is running, with its latest progress message.
    pub action: Option<(EngineAction, Option<String>)>,
}

impl Status {
    pub(crate) fn initial() -> Self {
        Self {
            link: Link::Connecting,
            connection: 0,
            client: None,
            handshake: None,
            engine: EngineState::Unspecified,
            failure: ErrorReason::Unspecified,
            engine_info: None,
            changed_at: None,
            stopped_by_user: false,
            action: None,
        }
    }

    pub fn client(&self) -> Option<&Client> {
        self.client.as_ref()
    }

    /// The client while the engine runs; engine RPCs fail with `ENGINE_NOT_RUNNING` otherwise.
    pub fn running_client(&self) -> Option<&Client> {
        self.client.as_ref().filter(|_| self.is_running())
    }

    pub fn is_running(&self) -> bool {
        self.link == Link::Connected && self.engine == EngineState::Running
    }

    /// Whether the daemon advertised `capability`; clients hide every feature whose capability is absent.
    pub fn has(&self, capability: Capability) -> bool {
        self.handshake
            .as_ref()
            .is_some_and(|handshake| handshake.capabilities().any(|offered| offered == capability))
    }

    pub fn backend(&self) -> BackendKind {
        self.handshake
            .as_ref()
            .map_or(BackendKind::Unspecified, HandshakeResponse::backend_kind)
    }
}

/// Handle to the application's one daemon connection; clones share it.
#[derive(Clone, Debug)]
pub struct Daemon {
    state: Arc<watch::Sender<Status>>,
}

impl Daemon {
    /// Connects in the background, starting the daemon when nothing answers, and reconnects whenever the
    /// connection breaks.
    pub fn connect(identity: ClientIdentity) -> Self {
        let (state, _) = watch::channel(Status::initial());
        let state = Arc::new(state);
        relm4::spawn(supervise(Arc::clone(&state), identity));
        Self { state }
    }

    pub fn status(&self) -> Status {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.state.subscribe()
    }

    /// Runs `action` on the engine and waits for it to finish; cancelling the call does not cancel the operation.
    pub async fn run(&self, action: EngineAction) -> Result<(), Error> {
        let state = Arc::clone(&self.state);
        relm4::spawn(async move {
            let client = state.borrow().client.clone().ok_or(Error::Closed)?;
            state.send_modify(|status| {
                status.action = Some((action, None));
                if action == EngineAction::Stop {
                    status.stopped_by_user = true;
                }
            });
            let result = drive(&state, &client, action).await;
            state.send_modify(|status| status.action = None);
            result
        })
        .await
        .map_err(|_| Error::Closed)?
    }
}

async fn supervise(state: Arc<watch::Sender<Status>>, identity: ClientIdentity) {
    let mut connection = 0;
    loop {
        match Client::connect_or_start(&identity).await {
            Ok(client) => {
                connection += 1;
                state.send_modify(|status| {
                    status.link = Link::Connected;
                    status.connection = connection;
                    status.handshake = Some(client.handshake().clone());
                    status.client = Some(client.clone());
                });
                if let Err(error) = follow_engine(&state, &client).await {
                    tracing::warn!(%error, "lost the daemon's engine state");
                }
                state.send_modify(|status| disconnect(status, Link::Connecting));
            }
            Err(error) => {
                tracing::warn!(%error, "cannot reach the daemon");
                state.send_modify(|status| disconnect(status, Link::Unreachable(describe(&error))));
            }
        }
        tokio::time::sleep(RETRY_DELAY).await;
    }
}

fn disconnect(status: &mut Status, link: Link) {
    status.link = link;
    status.client = None;
    status.engine = EngineState::Unspecified;
    status.failure = ErrorReason::Unspecified;
    status.engine_info = None;
    status.action = None;
}

async fn follow_engine(state: &watch::Sender<Status>, client: &Client) -> Result<(), Error> {
    let mut states = client.engine().watch_state(WatchStateRequest {}).await?.into_inner();
    while let Some(update) = states.message().await? {
        let engine = update.state();
        let engine_info = if engine == EngineState::Running {
            client.info().await.ok().and_then(|info| info.engine)
        } else {
            None
        };
        state.send_modify(|status| {
            if status.engine != engine && status.engine != EngineState::Unspecified {
                status.changed_at = Some(SystemTime::now());
            }
            if matches!(engine, EngineState::Starting | EngineState::Running) {
                status.stopped_by_user = false;
            }
            status.engine = engine;
            status.failure = update.failure_reason();
            status.engine_info = engine_info;
        });
    }
    Ok(())
}

enum Event {
    Progress(Progress),
    Done,
}

async fn drive(state: &watch::Sender<Status>, client: &Client, action: EngineAction) -> Result<(), Error> {
    match action {
        EngineAction::Start => {
            let stream = client.engine().start(StartRequest {}).await?.into_inner();
            follow_action(state, stream, |response| match response.event {
                Some(start_response::Event::Progress(progress)) => Some(Event::Progress(progress)),
                Some(start_response::Event::State(_)) => Some(Event::Done),
                None => None,
            })
            .await
        }
        EngineAction::Stop => {
            let stream = client.engine().stop(StopRequest {}).await?.into_inner();
            follow_action(state, stream, |response| match response.event {
                Some(stop_response::Event::Progress(progress)) => Some(Event::Progress(progress)),
                Some(stop_response::Event::State(_)) => Some(Event::Done),
                None => None,
            })
            .await
        }
        EngineAction::Restart => {
            let stream = client.engine().restart(RestartRequest {}).await?.into_inner();
            follow_action(state, stream, |response| match response.event {
                Some(restart_response::Event::Progress(progress)) => Some(Event::Progress(progress)),
                Some(restart_response::Event::State(_)) => Some(Event::Done),
                None => None,
            })
            .await
        }
    }
}

async fn follow_action<T>(
    state: &watch::Sender<Status>,
    mut stream: tonic::Streaming<T>,
    event: impl Fn(T) -> Option<Event>,
) -> Result<(), Error> {
    while let Some(response) = stream.message().await? {
        match event(response) {
            Some(Event::Progress(progress)) => state.send_modify(|status| {
                if let Some((_, message)) = &mut status.action {
                    *message = Some(progress.message);
                }
            }),
            Some(Event::Done) => return Ok(()),
            None => {}
        }
    }
    Ok(())
}

/// Calls `body` with a client whenever the engine runs, and again `RETRY_DELAY` after it returns; drops it when the
/// engine leaves `Running` or the connection changes. Returns when the daemon handle is gone.
pub async fn while_running<F, Fut>(mut status: watch::Receiver<Status>, mut body: F)
where
    F: FnMut(Client) -> Fut,
    Fut: Future<Output = ()>,
{
    loop {
        let (client, connection) = loop {
            {
                let current = status.borrow_and_update();
                if let Some(client) = current.running_client() {
                    break (client.clone(), current.connection);
                }
            }
            if status.changed().await.is_err() {
                return;
            }
        };

        let left = async {
            loop {
                if status.changed().await.is_err() {
                    return;
                }
                let current = status.borrow();
                if current.connection != connection || !current.is_running() {
                    return;
                }
            }
        };
        tokio::select! {
            () = body(client) => tokio::time::sleep(RETRY_DELAY).await,
            () = left => {}
        }
    }
}
