//! The Compose section: the projects the engine knows and those the user added, and each project's actions,
//! services and logs, through `ComposeService`.

mod list;
mod model;
mod project;

pub use list::{ComposeCmd, ComposeMsg, ComposeOutput, ComposePage};
