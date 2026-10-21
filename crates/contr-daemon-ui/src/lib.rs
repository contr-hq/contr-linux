//! The client's single connection to the daemon: connecting, or starting the daemon when nothing answers, following
//! the engine state, and the engine gate pages show while the engine is not running.

mod attachments;
mod daemon;
mod gate;
mod settings;

pub use attachments::{Attachment, engine_kind, engine_name, follow_attachments};
pub use daemon::{Daemon, EngineAction, Link, Status, while_running};
pub use gate::{EngineGate, TROUBLESHOOT_ACTION};
pub use settings::{SettingsHandle, field_mask};

use std::time::{Duration, SystemTime};

/// Pause before a failed connection, watch or stream is tried again.
pub const RETRY_DELAY: Duration = Duration::from_secs(3);

/// Converts a protobuf timestamp into a `SystemTime`; `None` for an absent or pre-epoch one.
pub fn system_time(timestamp: Option<&prost_types::Timestamp>) -> Option<SystemTime> {
    let timestamp = timestamp?;
    let seconds = u64::try_from(timestamp.seconds).ok()?;
    let nanos = u32::try_from(timestamp.nanos).unwrap_or(0);
    Some(SystemTime::UNIX_EPOCH + Duration::new(seconds, nanos))
}

/// The gRPC status message of an SDK error, which the daemon writes for people.
pub fn describe(error: &contr_sdk::Error) -> String {
    match error {
        contr_sdk::Error::Rpc(status) => status.message().to_owned(),
        other => other.to_string(),
    }
}
