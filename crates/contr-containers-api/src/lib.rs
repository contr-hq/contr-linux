//! Requests other modules send, through the application, to show a container or start one.

/// View of the container page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContainerTab {
    #[default]
    Logs,
    Inspect,
    Terminal,
    Stats,
}

/// Shows the page of the container with this ID, name or unique ID prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenContainer {
    pub id: String,
    pub tab: ContainerTab,
}

/// Opens the Run dialog with this image reference filled in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunImage {
    pub image: String,
}
