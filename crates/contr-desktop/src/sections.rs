use contr_daemon_ui::{Daemon, EngineGate};
use relm4::adw;
use relm4::gtk;

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

/// Page of a section the client has no module for yet: its header and, while the engine is not running, the gate.
pub(crate) fn placeholder(section: Section, daemon: &Daemon) -> adw::NavigationView {
    let header = adw::HeaderBar::builder()
        .title_widget(&adw::WindowTitle::new(section.title(), ""))
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    if section == Section::Troubleshoot {
        toolbar.set_content(Some(&content));
    } else {
        toolbar.set_content(Some(&EngineGate::around(daemon, section.icon_name(), &content)));
    }
    let view = adw::NavigationView::new();
    view.add(&adw::NavigationPage::new(&toolbar, section.title()));
    view
}
