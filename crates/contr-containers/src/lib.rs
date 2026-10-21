//! The Containers section: the container list grouped by Compose project, and each container's page with its logs,
//! inspection, terminal and stats.

mod detail;
mod list;
mod model;
mod rows;

pub use list::{ContainersCmd, ContainersMsg, ContainersPage};
