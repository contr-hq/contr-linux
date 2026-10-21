//! The Volumes section: volumes split by whether a container mounts them, with the containers that do, creation and
//! removal.

mod model;
mod page;

pub use page::{VolumesCmd, VolumesMsg, VolumesOutput, VolumesPage};
