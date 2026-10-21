//! The Terminal view: a VTE terminal attached to a shell the daemon runs in the container through `Exec`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use contr_daemon_ui::describe;
use contr_sdk::api::containers::v1::{Container, ContainerState, ExecStart, InspectContainerRequest, TerminalSize};
use contr_sdk::{Client, ExecEvent};
use relm4::gtk::{self, gdk, glib, pango};
use tokio::sync::mpsc;
use vte4::prelude::*;

const SHELLS: [&str; 2] = ["/bin/sh", "/bin/bash"];
/// Time the shell gets to exit on end of input before the session is dropped.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
    Close,
}

enum Output {
    Data(Vec<u8>),
    Exited(i32),
    Failed(String),
}

struct Session {
    input: mpsc::UnboundedSender<Input>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
struct State {
    client: Option<Client>,
    container: String,
    running: bool,
    session: Option<Session>,
    size: (u16, u16),
    /// Connects when the view is shown; cleared once the user disconnects.
    auto: bool,
}

struct Inner {
    root: gtk::Box,
    terminal: vte4::Terminal,
    shell: gtk::DropDown,
    user: gtk::DropDown,
    users: gtk::StringList,
    dot: gtk::Box,
    status: gtk::Label,
    connect: gtk::Button,
    state: RefCell<State>,
}

#[derive(Clone)]
pub(crate) struct TerminalView(Rc<Inner>);

impl TerminalView {
    pub(crate) fn new(container: &Container) -> Self {
        let shell = gtk::DropDown::from_strings(&SHELLS);
        let users = gtk::StringList::new(&["root"]);
        let user = gtk::DropDown::builder().model(&users).build();
        let dot = gtk::Box::builder()
            .css_classes(["sidebar-dot"])
            .valign(gtk::Align::Center)
            .build();
        let status = gtk::Label::builder().css_classes(["dim-label"]).build();
        let copy = gtk::Button::builder().label("Copy").css_classes(["flat"]).build();
        let clear = gtk::Button::builder().label("Clear").css_classes(["flat"]).build();
        let connect = gtk::Button::builder()
            .label("Disconnect")
            .css_classes(["raised"])
            .build();
        let toolbar = gtk::Box::builder().spacing(8).css_classes(["console-toolbar"]).build();
        toolbar.append(&gtk::Label::new(Some("Shell")));
        toolbar.append(&shell);
        toolbar.append(&gtk::Label::new(Some("User")));
        toolbar.append(&user);
        let state_box = gtk::Box::builder().spacing(6).margin_start(6).hexpand(true).build();
        state_box.append(&dot);
        state_box.append(&status);
        toolbar.append(&state_box);
        toolbar.append(&copy);
        toolbar.append(&clear);
        toolbar.append(&connect);

        let terminal = vte4::Terminal::builder()
            .vexpand(true)
            .hexpand(true)
            .scrollback_lines(10_000)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(16)
            .margin_end(16)
            .build();
        // "Monospace" resolves through fontconfig; Adwaita Mono is missing on some distributions.
        terminal.set_font(Some(&pango::FontDescription::from_string("Monospace 10.5")));
        terminal.set_colors(
            Some(&gdk::RGBA::new(0.871, 0.867, 0.855, 1.0)),
            Some(&gdk::RGBA::new(0.118, 0.118, 0.118, 1.0)),
            &[],
        );
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["console"])
            .overflow(gtk::Overflow::Hidden)
            .build();
        root.append(&toolbar);
        root.append(&terminal);

        let view = Self(Rc::new(Inner {
            root,
            terminal,
            shell,
            user,
            users,
            dot,
            status,
            connect,
            state: RefCell::new(State {
                container: container.id.clone(),
                auto: true,
                ..State::default()
            }),
        }));
        let weak = Rc::downgrade(&view.0);
        view.0.connect.connect_clicked(with(&weak, Self::toggle));
        copy.connect_clicked(with(&weak, |view| {
            view.0.terminal.copy_clipboard_format(vte4::Format::Text);
        }));
        clear.connect_clicked(with(&weak, |view| view.0.terminal.reset(true, true)));
        view.0.root.connect_map(with(&weak, |view| {
            let state = view.0.state.borrow();
            let wanted = state.auto && state.running && state.session.is_none();
            drop(state);
            if wanted {
                view.connect();
            }
        }));
        view.0.terminal.connect_commit({
            let weak = weak.clone();
            move |_, text, _| {
                if let Some(inner) = weak.upgrade()
                    && let Some(session) = &inner.state.borrow().session
                {
                    let _ = session.input.send(Input::Bytes(text.as_bytes().to_vec()));
                }
            }
        });
        view.0.terminal.add_tick_callback({
            let weak = weak.clone();
            move |_, _| {
                if let Some(inner) = weak.upgrade() {
                    TerminalView(inner).track_size();
                }
                glib::ControlFlow::Continue
            }
        });
        view.show_status();
        view
    }

    pub(crate) fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    /// Follows the container's state: a stopped container ends the session.
    pub(crate) fn set_container(&self, client: Option<Client>, container: &Container) {
        let running = container.state() == ContainerState::Running;
        {
            let mut state = self.0.state.borrow_mut();
            state.client.clone_from(&client);
            state.container.clone_from(&container.id);
            state.running = running;
        }
        if !running {
            self.disconnect();
        } else if self.0.root.is_mapped() && self.0.state.borrow().auto {
            self.connect();
        }
        if let Some(client) = client {
            self.load_users(client, container.id.clone());
        }
        self.show_status();
    }

    pub(crate) fn stop(&self) {
        self.disconnect();
    }

    fn load_users(&self, client: Client, id: String) {
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            let request = InspectContainerRequest { id };
            let result = relm4::spawn(async move { client.containers().inspect_container(request).await }).await;
            let Ok(Ok(response)) = result else { return };
            let user = response
                .into_inner()
                .details
                .map(|details| details.user)
                .unwrap_or_default();
            let Some(inner) = weak.upgrade() else { return };
            let mut users = vec!["root".to_owned()];
            if !user.is_empty() && user != "root" {
                users.insert(0, user);
            }
            let names: Vec<&str> = users.iter().map(String::as_str).collect();
            inner.users.splice(0, inner.users.n_items(), &names);
            inner.user.set_selected(0);
        });
    }

    fn toggle(&self) {
        if self.0.state.borrow().session.is_some() {
            self.0.state.borrow_mut().auto = false;
            self.disconnect();
        } else {
            self.0.state.borrow_mut().auto = true;
            self.connect();
        }
    }

    fn connect(&self) {
        let (client, container) = {
            let state = self.0.state.borrow();
            if state.session.is_some() || !state.running {
                return;
            }
            let Some(client) = state.client.clone() else { return };
            (client, state.container.clone())
        };
        let shell = SHELLS
            .get(usize::try_from(self.0.shell.selected()).unwrap_or_default())
            .copied()
            .unwrap_or(SHELLS[0]);
        let user = self
            .0
            .users
            .string(self.0.user.selected())
            .map(String::from)
            .unwrap_or_default();
        let (columns, rows) = self.size();
        let start = ExecStart {
            container_id: container,
            command: vec![shell.to_owned()],
            tty: true,
            attach_stdin: true,
            env: vec!["TERM=xterm-256color".to_owned()],
            user,
            terminal_size: Some(TerminalSize {
                rows: rows.into(),
                columns: columns.into(),
            }),
            ..ExecStart::default()
        };

        let (input, mut inputs) = mpsc::unbounded_channel::<Input>();
        let (outputs, mut output) = mpsc::unbounded_channel::<Output>();
        let task = relm4::spawn(async move {
            let mut session = match client.exec(start).await {
                Ok(session) => session,
                Err(error) => {
                    let _ = outputs.send(Output::Failed(describe(&error)));
                    return;
                }
            };
            let writer = session.input();
            let forward = relm4::spawn(async move {
                while let Some(message) = inputs.recv().await {
                    let sent = match message {
                        Input::Bytes(bytes) => writer.write(bytes).await,
                        Input::Resize(columns, rows) => writer.resize(rows, columns).await,
                        Input::Close => {
                            let _ = writer.close().await;
                            break;
                        }
                    };
                    if sent.is_err() {
                        break;
                    }
                }
            });
            loop {
                let event = match session.next_event().await {
                    Ok(Some(ExecEvent::Stdout(bytes) | ExecEvent::Stderr(bytes))) => Output::Data(bytes),
                    Ok(Some(ExecEvent::Exited(code))) => Output::Exited(code),
                    Ok(None) => break,
                    Err(error) => Output::Failed(describe(&error)),
                };
                let last = !matches!(event, Output::Data(_));
                if outputs.send(event).is_err() || last {
                    break;
                }
            }
            forward.abort();
        });
        self.0.state.borrow_mut().session = Some(Session { input, task });
        self.0.state.borrow_mut().size = (columns, rows);
        self.show_status();
        self.0.terminal.grab_focus();

        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            while let Some(event) = output.recv().await {
                let Some(inner) = weak.upgrade() else { return };
                let view = TerminalView(inner);
                match event {
                    Output::Data(bytes) => view.0.terminal.feed(&bytes),
                    Output::Exited(code) => {
                        view.0
                            .terminal
                            .feed(format!("\r\n[exited with code {code}]\r\n").as_bytes());
                        view.0.state.borrow_mut().auto = false;
                        view.disconnect();
                    }
                    Output::Failed(message) => {
                        view.0.terminal.feed(format!("\r\n{message}\r\n").as_bytes());
                        view.0.state.borrow_mut().auto = false;
                        view.disconnect();
                    }
                }
            }
        });
    }

    /// Closes the shell's stdin before dropping the session: the engine keeps an exec running after its client leaves.
    fn disconnect(&self) {
        let session = self.0.state.borrow_mut().session.take();
        if let Some(session) = session {
            let _ = session.input.send(Input::Close);
            glib::timeout_add_local_once(CLOSE_GRACE, move || session.task.abort());
        }
        self.show_status();
    }

    fn size(&self) -> (u16, u16) {
        let columns = u16::try_from(self.0.terminal.column_count()).unwrap_or(80);
        let rows = u16::try_from(self.0.terminal.row_count()).unwrap_or(24);
        (columns, rows)
    }

    fn track_size(&self) {
        let size = self.size();
        let mut state = self.0.state.borrow_mut();
        if state.size == size {
            return;
        }
        state.size = size;
        if let Some(session) = &state.session {
            let _ = session.input.send(Input::Resize(size.0, size.1));
        }
        drop(state);
        self.show_status();
    }

    fn show_status(&self) {
        let state = self.0.state.borrow();
        let connected = state.session.is_some();
        let (columns, rows) = state.size;
        self.0.status.set_label(&if connected {
            format!("Connected · {columns} × {rows}")
        } else if state.running {
            "Disconnected".to_owned()
        } else {
            "The container isn’t running".to_owned()
        });
        self.0.dot.set_visible(connected);
        self.0
            .connect
            .set_label(if connected { "Disconnect" } else { "Connect" });
        self.0.connect.set_sensitive(state.running);
        self.0.shell.set_sensitive(!connected);
        self.0.user.set_sensitive(!connected);
    }
}

fn with<W>(weak: &Weak<Inner>, action: impl Fn(&TerminalView) + 'static) -> impl Fn(&W) + 'static {
    let weak = weak.clone();
    move |_: &W| {
        if let Some(inner) = weak.upgrade() {
            action(&TerminalView(inner));
        }
    }
}
