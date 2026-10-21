//! The Containers section: the container list grouped by Compose project, the Run dialog, and each container's page
//! with its logs, inspection, terminal and stats.

mod detail;
mod list;
mod model;
mod rows;
mod run;

pub use list::{ContainersCmd, ContainersMsg, ContainersOutput, ContainersPage};
