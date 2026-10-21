/// A sidebar entry and the page it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Section {
    Containers,
    Images,
    Volumes,
    Networks,
    Compose,
    Kubernetes,
    Troubleshoot,
}

impl Section {
    pub(crate) const ALL: [Self; 7] = [
        Self::Containers,
        Self::Images,
        Self::Volumes,
        Self::Networks,
        Self::Compose,
        Self::Kubernetes,
        Self::Troubleshoot,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Containers => "Containers",
            Self::Images => "Images",
            Self::Volumes => "Volumes",
            Self::Networks => "Networks",
            Self::Compose => "Compose",
            Self::Kubernetes => "Kubernetes",
            Self::Troubleshoot => "Troubleshoot",
        }
    }

    pub(crate) fn icon_name(self) -> &'static str {
        match self {
            Self::Containers => "contr-containers-symbolic",
            Self::Images => "contr-images-symbolic",
            Self::Volumes => "contr-volumes-symbolic",
            Self::Networks => "contr-networks-symbolic",
            Self::Compose => "contr-compose-symbolic",
            Self::Kubernetes => "contr-kubernetes-symbolic",
            Self::Troubleshoot => "contr-troubleshoot-symbolic",
        }
    }

    /// Stack child name of the section's page.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Containers => "containers",
            Self::Images => "images",
            Self::Volumes => "volumes",
            Self::Networks => "networks",
            Self::Compose => "compose",
            Self::Kubernetes => "kubernetes",
            Self::Troubleshoot => "troubleshoot",
        }
    }

    pub(crate) fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.name() == name)
    }
}
