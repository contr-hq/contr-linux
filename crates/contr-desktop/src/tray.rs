//! The `StatusNotifierItem` tray icon; GNOME shows it only with the `AppIndicator` extension.

use contr_core::product;
use ksni::menu::{StandardItem, SubMenu};
use ksni::{Icon, MenuItem, ToolTip};
use relm4::gtk::gdk_pixbuf::Pixbuf;

use crate::app::AppMsg;
use contr_daemon_ui::EngineAction;

const ICON_SIZES: [i32; 3] = [22, 32, 48];

/// What the tray menu shows; the window rebuilds it from the daemon status and the container list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TrayState {
    pub(crate) title: String,
    pub(crate) subtitle: String,
    pub(crate) running: Vec<RunningContainer>,
    pub(crate) engine: Option<EngineAction>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunningContainer {
    pub(crate) name: String,
    pub(crate) address: String,
}

pub(crate) struct ContrTray {
    pub(crate) state: TrayState,
    icons: Vec<Icon>,
    input: relm4::Sender<AppMsg>,
}

impl ContrTray {
    pub(crate) fn new(input: relm4::Sender<AppMsg>, icons: Vec<Icon>) -> Self {
        Self {
            state: TrayState::default(),
            icons,
            input,
        }
    }
}

impl ksni::Tray for ContrTray {
    fn id(&self) -> String {
        product::APP_ID.to_owned()
    }

    fn title(&self) -> String {
        product::DISPLAY_NAME.to_owned()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        self.icons.clone()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            title: product::DISPLAY_NAME.to_owned(),
            description: self.state.title.clone(),
            ..ToolTip::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.input.emit(AppMsg::Present);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = vec![label(&self.state.title)];
        if !self.state.subtitle.is_empty() {
            items.push(label(&self.state.subtitle));
        }
        items.push(MenuItem::Separator);
        items.push(item(&format!("Open {}", product::DISPLAY_NAME), |tray| {
            tray.input.emit(AppMsg::Present);
        }));
        if !self.state.running.is_empty() {
            let submenu = self
                .state
                .running
                .iter()
                .map(|container| {
                    let text = if container.address.is_empty() {
                        container.name.clone()
                    } else {
                        format!("{}    {}", container.name, container.address)
                    };
                    item(&text, |tray| tray.input.emit(AppMsg::Present))
                })
                .collect();
            items.push(
                SubMenu {
                    label: "Running containers".to_owned(),
                    submenu,
                    ..SubMenu::default()
                }
                .into(),
            );
        }
        match self.state.engine {
            Some(EngineAction::Restart) => items.push(item("Restart engine", |tray| {
                tray.input.emit(AppMsg::Engine(EngineAction::Restart));
            })),
            Some(EngineAction::Start) => items.push(item("Start engine", |tray| {
                tray.input.emit(AppMsg::Engine(EngineAction::Start));
            })),
            Some(EngineAction::Stop) | None => {}
        }
        items.push(MenuItem::Separator);
        items.push(item(&format!("Quit {}", product::DISPLAY_NAME), |tray| {
            tray.input.emit(AppMsg::Quit);
        }));
        items
    }
}

/// `DBusMenu` reads `_` as a mnemonic marker; doubling keeps names such as `pensive_hopper` intact.
fn escape(text: &str) -> String {
    text.replace('_', "__")
}

fn label(text: &str) -> MenuItem<ContrTray> {
    StandardItem {
        label: escape(text),
        enabled: false,
        ..StandardItem::default()
    }
    .into()
}

fn item(text: &str, activate: impl Fn(&mut ContrTray) + Send + 'static) -> MenuItem<ContrTray> {
    StandardItem {
        label: escape(text),
        activate: Box::new(activate),
        ..StandardItem::default()
    }
    .into()
}

/// The application icon as ARGB32 pixmaps, since the shell cannot read the icon from this process's resources.
pub(crate) fn icon_pixmaps() -> Vec<Icon> {
    let path = format!(
        "/{}/icons/scalable/apps/{}.svg",
        product::APP_ID.replace('.', "/"),
        product::APP_ID
    );
    ICON_SIZES
        .into_iter()
        .filter_map(|size| Pixbuf::from_resource_at_scale(&path, size, size, true).ok())
        .map(|pixbuf| argb(&pixbuf))
        .collect()
}

fn argb(pixbuf: &Pixbuf) -> Icon {
    let width = pixbuf.width();
    let height = pixbuf.height();
    let channels = usize::try_from(pixbuf.n_channels()).unwrap_or(4);
    let stride = usize::try_from(pixbuf.rowstride()).unwrap_or_default();
    let bytes = pixbuf.read_pixel_bytes();
    let mut data = Vec::with_capacity(usize::try_from(width * height * 4).unwrap_or_default());
    for row in 0..usize::try_from(height).unwrap_or_default() {
        for column in 0..usize::try_from(width).unwrap_or_default() {
            let at = row * stride + column * channels;
            let pixel = bytes.get(at..at + channels).unwrap_or(&[0, 0, 0, 0]);
            let alpha = if channels == 4 { pixel[3] } else { 255 };
            data.extend_from_slice(&[alpha, pixel[0], pixel[1], pixel[2]]);
        }
    }
    Icon { width, height, data }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn underscores_survive_the_mnemonic_parser() {
        assert_eq!(escape("pensive_hopper"), "pensive__hopper");
    }
}
