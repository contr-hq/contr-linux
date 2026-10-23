use std::collections::HashSet;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use contr_sdk::api::common::v1::ErrorReason;
use contr_sdk::api::daemon::v1::{BackendKind, Capability, EngineInfo, HandshakeResponse};
use contr_sdk::api::engine::v1::{
    EngineState, ResetRequest, ResetScope, RestartRequest, StartRequest, StopRequest, WatchStateRequest,
};
use contr_sdk::api::operations::v1::OperationState;
use contr_sdk::{Client, ClientIdentity, Error};
use tokio::sync::{broadcast, watch};

use crate::operations::{self, Activity, Kind, Operation};
use crate::{RETRY_DELAY, describe};

/// Notices a page can send while the shell is the one that shows them.
const NOTICES: usize = 16;

const ENGINE_INFO_ATTEMPTS: u32 = 5;
const ENGINE_INFO_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

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
    Reset(ResetScope),
}

impl EngineAction {
    fn kind(self) -> Kind {
        match self {
            Self::Start => Kind::StartEngine(StartRequest {}),
            Self::Stop => Kind::StopEngine(StopRequest {}),
            Self::Restart => Kind::RestartEngine(RestartRequest {}),
            Self::Reset(scope) => Kind::ResetEngine(ResetRequest { scope: scope.into() }),
        }
    }
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
    /// The running engine operation, whichever client started it, with its latest progress message.
    pub action: Option<(EngineAction, Option<String>)>,
}

impl Default for Status {
    /// Connecting, with nothing known yet.
    fn default() -> Self {
        Self::initial()
    }
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
    activity: Arc<watch::Sender<Activity>>,
    notices: broadcast::Sender<String>,
    claimed: Arc<Mutex<HashSet<String>>>,
}

impl Daemon {
    /// Connects in the background, starting the daemon when nothing answers, and reconnects whenever the
    /// connection breaks.
    pub fn connect(identity: ClientIdentity) -> Self {
        let (state, _) = watch::channel(Status::initial());
        let state = Arc::new(state);
        let (activity, _) = watch::channel(Activity::default());
        let activity = Arc::new(activity);
        let (notices, _) = broadcast::channel(NOTICES);
        relm4::spawn(supervise(Arc::clone(&state), identity));
        relm4::spawn(operations::follow_activity(Arc::clone(&state), Arc::clone(&activity)));
        Self {
            state,
            activity,
            notices,
            claimed: Arc::default(),
        }
    }

    pub fn activity(&self) -> Activity {
        self.activity.borrow().clone()
    }

    pub fn subscribe_activity(&self) -> watch::Receiver<Activity> {
        self.activity.subscribe()
    }

    /// Shows `message` in the window's toasts, from work that outlives the page that started it.
    pub fn notice(&self, message: impl Into<String>) {
        let _ = self.notices.send(message.into());
    }

    pub fn subscribe_notices(&self) -> broadcast::Receiver<String> {
        self.notices.subscribe()
    }

    /// Marks operation `id` as one whose end the caller reports, so the window's toasts leave it out.
    pub fn claim(&self, id: &str) {
        self.claimed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.to_owned());
    }

    /// Whether a caller claimed operation `id`; forgets the claim.
    pub fn take_claim(&self, id: &str) -> bool {
        self.claimed.lock().unwrap_or_else(PoisonError::into_inner).remove(id)
    }

    pub fn status(&self) -> Status {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.state.subscribe()
    }

    /// Starts `action` on the engine as an operation the daemon owns, without waiting for it to finish.
    pub async fn start(&self, action: EngineAction) -> Result<Operation, Error> {
        let state = Arc::clone(&self.state);
        relm4::spawn(async move { start_engine(&state, action).await })
            .await
            .map_err(|_| Error::Closed)?
    }

    /// Runs `action` on the engine as an operation the daemon owns and waits for it to finish; dropping the call
    /// does not stop it.
    pub async fn run(&self, action: EngineAction) -> Result<(), Error> {
        let state = Arc::clone(&self.state);
        relm4::spawn(async move {
            let started = start_engine(&state, action).await?;
            let client = state.borrow().client.clone().ok_or(Error::Closed)?;
            let finished = operations::follow(&client, &started.id, |_| {}).await?;
            match (finished.state(), finished.error) {
                (OperationState::Failed, Some(error)) => Err(Error::from(tonic::Status::new(
                    tonic::Code::from(error.code),
                    error.message,
                ))),
                _ => Ok(()),
            }
        })
        .await
        .map_err(|_| Error::Closed)?
    }
}

async fn start_engine(state: &watch::Sender<Status>, action: EngineAction) -> Result<Operation, Error> {
    let client = state.borrow().client.clone().ok_or(Error::Closed)?;
    if action == EngineAction::Stop {
        state.send_modify(|status| status.stopped_by_user = true);
    }
    operations::start(&client, action.kind()).await
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
            engine_info(client).await
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

/// `GetInfo` can describe the engine a moment after the state turns `Running`, so a missing description is asked for
/// again a few times.
async fn engine_info(client: &Client) -> Option<EngineInfo> {
    for _ in 0..ENGINE_INFO_ATTEMPTS {
        if let Some(info) = client.info().await.ok().and_then(|info| info.engine) {
            return Some(info);
        }
        tokio::time::sleep(ENGINE_INFO_DELAY).await;
    }
    None
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
