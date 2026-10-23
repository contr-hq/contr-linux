//! The activity button in the sidebar header: running and recent work the daemon owns, with its progress, Cancel
//! where the daemon allows it, the fixes for a failure, and Save… for a finished bundle.

use contr_daemon_ui::operations::{self, Activity, Kind, Operation};
use contr_daemon_ui::remediation_label;
use contr_sdk::api::operations::v1::OperationState;
use contr_ui_common::widgets::{StatusKind, boxed_list, icon_button, status_icon};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};

use crate::app::AppMsg;

/// Height the popover's list grows to before it scrolls.
const LIST_HEIGHT: i32 = 420;

pub(crate) struct ActivityView {
    pub(crate) button: gtk::MenuButton,
    spinner: adw::Spinner,
    icon: gtk::Image,
    list: gtk::ListBox,
    rows: Vec<OperationRow>,
    input: relm4::Sender<AppMsg>,
}

struct OperationRow {
    id: String,
    /// The state the row's buttons were made for.
    state: OperationState,
    row: gtk::ListBoxRow,
    status: gtk::Stack,
    detail: gtk::Label,
    progress: gtk::ProgressBar,
    actions: gtk::Box,
}

impl ActivityView {
    pub(crate) fn new(input: relm4::Sender<AppMsg>) -> Self {
        let spinner = adw::Spinner::new();
        let icon = gtk::Image::from_icon_name("object-select-symbolic");
        let face = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        face.append(&spinner);
        face.append(&icon);
        let list = boxed_list();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(LIST_HEIGHT)
            .child(&list)
            .build();
        let heading = gtk::Label::builder()
            .label("Activity")
            .xalign(0.0)
            .css_classes(["heading"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .width_request(360)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        content.append(&heading);
        content.append(&scroller);
        let popover = gtk::Popover::builder().child(&content).build();
        let button = gtk::MenuButton::builder()
            .child(&face)
            .popover(&popover)
            .tooltip_text("Activity")
            .visible(false)
            .build();
        button.update_property(&[gtk::accessible::Property::Label("Activity")]);
        Self {
            button,
            spinner,
            icon,
            list,
            rows: Vec::new(),
            input,
        }
    }

    pub(crate) fn show(&mut self, activity: &Activity) {
        let running = activity.running().count();
        let failed = activity
            .operations
            .iter()
            .any(|operation| operation.state() == OperationState::Failed);
        self.button.set_visible(!activity.operations.is_empty());
        self.spinner.set_visible(running > 0);
        self.icon.set_visible(running == 0);
        self.icon.set_icon_name(Some(if failed {
            "dialog-warning-symbolic"
        } else {
            "object-select-symbolic"
        }));
        self.button.set_tooltip_text(Some(&match running {
            0 => "Activity".to_owned(),
            1 => "Activity · 1 running".to_owned(),
            count => format!("Activity · {count} running"),
        }));

        let same_rows = self.rows.len() == activity.operations.len()
            && self
                .rows
                .iter()
                .zip(&activity.operations)
                .all(|(row, operation)| row.id == operation.id);
        if !same_rows {
            self.list.remove_all();
            self.rows = activity
                .operations
                .iter()
                .map(|operation| {
                    let row = OperationRow::new(operation);
                    self.list.append(&row.row);
                    row
                })
                .collect();
        }
        for (row, operation) in self.rows.iter_mut().zip(&activity.operations) {
            row.show(operation, &self.input, !same_rows);
        }
    }
}

impl OperationRow {
    fn new(operation: &Operation) -> Self {
        let status = gtk::Stack::builder().valign(gtk::Align::Center).build();
        status.add_named(&adw::Spinner::new(), Some("running"));
        let title = gtk::Label::builder()
            .label(operations::title(operation))
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .css_classes(["dim-label", "caption"])
            .build();
        let progress = gtk::ProgressBar::builder().visible(false).margin_top(4).build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        text.append(&title);
        text.append(&detail);
        text.append(&progress);
        let actions = gtk::Box::builder().spacing(6).valign(gtk::Align::Center).build();
        let content = gtk::Box::builder()
            .spacing(12)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(10)
            .margin_end(6)
            .build();
        content.append(&status);
        content.append(&text);
        content.append(&actions);
        Self {
            id: operation.id.clone(),
            state: OperationState::Unspecified,
            row: gtk::ListBoxRow::builder().child(&content).activatable(false).build(),
            status,
            detail,
            progress,
            actions,
        }
    }

    /// Updates the row in place; its buttons are made again only when the state changes or `rebuilt` is set.
    fn show(&mut self, operation: &Operation, input: &relm4::Sender<AppMsg>, rebuilt: bool) {
        let state = operation.state();
        let fraction = operation.progress.as_ref().and_then(|progress| progress.fraction);
        self.progress
            .set_visible(state == OperationState::Running && fraction.is_some());
        if let Some(fraction) = fraction {
            self.progress.set_fraction(f64::from(fraction));
        }
        self.detail.set_label(&detail(operation));
        if state == OperationState::Failed {
            self.detail.add_css_class("status-failed");
        } else {
            self.detail.remove_css_class("status-failed");
        }
        if state == self.state && !rebuilt {
            return;
        }
        self.state = state;
        let name = match state {
            OperationState::Running | OperationState::Unspecified => "running".to_owned(),
            finished => {
                let name = format!("{finished:?}");
                if self.status.child_by_name(&name).is_none() {
                    let (kind, word) = match finished {
                        OperationState::Succeeded => (StatusKind::Running, "Done"),
                        OperationState::Failed => (StatusKind::Failed, "Failed"),
                        _ => (StatusKind::Stopped, "Cancelled"),
                    };
                    self.status.add_named(&status_icon(kind, word), Some(&name));
                }
                name
            }
        };
        self.status.set_visible_child_name(&name);
        while let Some(child) = self.actions.first_child() {
            self.actions.remove(&child);
        }
        for button in buttons(operation, input) {
            self.actions.append(&button);
        }
    }
}

/// The latest step of a running operation, or how a finished one ended.
fn detail(operation: &Operation) -> String {
    match operation.state() {
        OperationState::Running | OperationState::Unspecified => operation
            .progress
            .as_ref()
            .map(|progress| progress.message.clone())
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Starting…".to_owned()),
        OperationState::Succeeded => match finished_at(operation) {
            Some(at) => format!("Done at {at}"),
            None => "Done".to_owned(),
        },
        OperationState::Cancelled => "Cancelled".to_owned(),
        OperationState::Failed => operation
            .error
            .as_ref()
            .map(|error| error.message.clone())
            .filter(|message| !message.is_empty())
            .unwrap_or_else(|| "Failed".to_owned()),
    }
}

fn finished_at(operation: &Operation) -> Option<String> {
    let at = glib::DateTime::from_unix_local(operation.finished_at?.seconds).ok()?;
    at.format("%H:%M").ok().map(String::from)
}

fn buttons(operation: &Operation, input: &relm4::Sender<AppMsg>) -> Vec<gtk::Button> {
    let id = operation.id.clone();
    let action = |button: &gtk::Button, message: AppMsg| {
        let input = input.clone();
        let message = std::cell::Cell::new(Some(message));
        button.connect_clicked(move |button| {
            if let Some(message) = message.take() {
                button.set_sensitive(false);
                input.emit(message);
            }
        });
    };
    match operation.state() {
        OperationState::Running if operation.cancellable => {
            let cancel = icon_button("window-close-symbolic", "Cancel");
            action(&cancel, AppMsg::CancelOperation(id));
            vec![cancel]
        }
        OperationState::Succeeded if matches!(operations::kind(operation), Some(Kind::CreateBundle(_))) => {
            let save = gtk::Button::builder().label("Save…").valign(gtk::Align::Center).build();
            let input = input.clone();
            save.connect_clicked(move |_| input.emit(AppMsg::SaveBundle(id.clone())));
            vec![save]
        }
        OperationState::Failed => operation
            .error
            .iter()
            .flat_map(|error| &error.remediations)
            .map(|remediation| {
                let fix = gtk::Button::builder()
                    .label(remediation_label(remediation))
                    .valign(gtk::Align::Center)
                    .build();
                action(&fix, AppMsg::Fix(remediation.clone()));
                fix
            })
            .collect(),
        _ => Vec::new(),
    }
}
