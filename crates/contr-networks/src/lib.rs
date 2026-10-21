//! The Networks section: user networks with their containers and addresses, the engine's built-in networks, and how a
//! container reaches this computer.

mod model;
mod page;

pub use page::{NetworksCmd, NetworksMsg, NetworksOutput, NetworksPage};
