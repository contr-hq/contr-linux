//! The Logs view: the container's output with timestamps, find, and saving to a file.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::time::SystemTime;

use contr_daemon_ui::{describe, system_time};
use contr_sdk::Client;
use contr_sdk::api::containers::v1::{Container, ContainerState, LogStream, StreamLogsRequest};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use tokio::sync::mpsc;

use crate::model;

/// Lines kept in memory; the oldest tenth goes when the log grows past it.
const MAX_LINES: usize = 10_000;
const TAIL_LINES: u32 = 1_000;

struct Line {
    time: Option<SystemTime>,
    text: String,
}

enum Event {
    Chunk {
        stderr: bool,
        data: Vec<u8>,
        time: Option<prost_types::Timestamp>,
    },
    End,
    Failed(String),
}

#[derive(Default)]
struct State {
    lines: Vec<Line>,
    pending: [String; 2],
    last: Option<prost_types::Timestamp>,
    streaming: bool,
    exit_note: Option<String>,
    error: Option<String>,
    matches: Vec<usize>,
    current: usize,
    task: Option<tokio::task::JoinHandle<()>>,
}

struct Inner {
    root: gtk::Box,
    buffer: gtk::TextBuffer,
    view: gtk::TextView,
    entry: gtk::SearchEntry,
    count: gtk::Label,
    timestamps: gtk::Switch,
    name: RefCell<String>,
    state: RefCell<State>,
}

#[derive(Clone)]
pub(crate) struct LogsView(Rc<Inner>);

impl LogsView {
    pub(crate) fn new(container: &Container) -> Self {
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Find in logs")
            .hexpand(true)
            .build();
        let count = gtk::Label::builder()
            .css_classes(["dim-label", "group-description"])
            .build();
        let find = gtk::Box::builder().spacing(8).hexpand(true).build();
        find.append(&entry);
        find.append(&count);
        let timestamps = gtk::Switch::builder().active(true).valign(gtk::Align::Center).build();
        let timestamps_label = gtk::Label::new(Some("Timestamps"));
        let save = gtk::Button::builder().label("Save…").css_classes(["flat"]).build();
        let toolbar = gtk::Box::builder().spacing(12).build();
        toolbar.append(&find);
        toolbar.append(&timestamps_label);
        toolbar.append(&timestamps);
        toolbar.append(&save);

        let buffer = gtk::TextBuffer::new(None);
        buffer.create_tag(Some("time"), &[("foreground", &"#9a9996")]);
        buffer.create_tag(Some("note"), &[("foreground", &"#9a9996")]);
        buffer.create_tag(Some("match"), &[("background", &"#f6d32d"), ("foreground", &"#1e1e1e")]);
        buffer.create_tag(Some("match-line"), &[("paragraph-background", &"#4a1f22")]);
        let view = gtk::TextView::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .css_classes(["console-view"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&view)
            .vexpand(true)
            .css_classes(["console"])
            .build();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(14)
            .build();
        root.append(&toolbar);
        root.append(&scroller);

        let logs = Self(Rc::new(Inner {
            root,
            buffer,
            view,
            entry,
            count,
            timestamps,
            name: RefCell::new(model::name(container)),
            state: RefCell::default(),
        }));
        let weak = Rc::downgrade(&logs.0);
        logs.0.entry.connect_search_changed(with(&weak, LogsView::search));
        logs.0.entry.connect_activate(with(&weak, |logs| logs.step(1)));
        logs.0.entry.connect_next_match(with(&weak, |logs| logs.step(1)));
        logs.0.entry.connect_previous_match(with(&weak, |logs| logs.step(-1)));
        logs.0.timestamps.connect_active_notify(with(&weak, LogsView::render));
        save.connect_clicked(with(&weak, LogsView::save));
        logs
    }

    pub(crate) fn widget(&self) -> &gtk::Box {
        &self.0.root
    }

    /// Streams the log while the container runs, or reads what it wrote before it stopped.
    pub(crate) fn follow(&self, client: Option<Client>, container: &Container) {
        self.0.name.replace(model::name(container));
        let running = container.state() == ContainerState::Running;
        {
            let mut state = self.0.state.borrow_mut();
            state.exit_note = model::exit_code(&container.status)
                .filter(|_| !running)
                .map(|code| format!("— container exited with code {code} —"));
            let loaded = !state.lines.is_empty() || state.streaming || state.error.is_some();
            if loaded && (!running || state.streaming) {
                drop(state);
                self.render();
                return;
            }
        }
        let Some(client) = client else { return };
        let since = self.0.state.borrow().last.map(|last| prost_types::Timestamp {
            seconds: last.seconds,
            nanos: last.nanos + 1,
        });
        let request = StreamLogsRequest {
            id: container.id.clone(),
            follow: true,
            tail_lines: since.is_none().then_some(TAIL_LINES),
            since,
            timestamps: true,
        };
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let task = relm4::spawn(async move {
            let stream = client.containers().stream_logs(request).await;
            let mut stream = match stream {
                Ok(stream) => stream.into_inner(),
                Err(status) => {
                    let _ = sender.send(Event::Failed(describe(&contr_sdk::Error::from(status))));
                    return;
                }
            };
            loop {
                match stream.message().await {
                    Ok(Some(chunk)) => {
                        let event = Event::Chunk {
                            stderr: chunk.stream() == LogStream::Stderr,
                            data: chunk.data,
                            time: chunk.timestamp,
                        };
                        if sender.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(status) => {
                        let _ = sender.send(Event::Failed(describe(&contr_sdk::Error::from(status))));
                        return;
                    }
                }
            }
            let _ = sender.send(Event::End);
        });
        {
            let mut state = self.0.state.borrow_mut();
            if let Some(old) = state.task.replace(task) {
                old.abort();
            }
            state.streaming = true;
            state.error = None;
        }
        let weak = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            while let Some(event) = receiver.recv().await {
                let Some(inner) = weak.upgrade() else { return };
                LogsView(inner).receive(event);
            }
        });
    }

    pub(crate) fn stop(&self) {
        if let Some(task) = self.0.state.borrow_mut().task.take() {
            task.abort();
        }
    }

    fn receive(&self, event: Event) {
        match event {
            Event::Chunk { stderr, data, time } => {
                let added = {
                    let mut state = self.0.state.borrow_mut();
                    if time.is_some() {
                        state.last = time;
                    }
                    let slot = usize::from(stderr);
                    let mut text = std::mem::take(&mut state.pending[slot]);
                    text.push_str(&strip_ansi(&String::from_utf8_lossy(&data)));
                    let mut parts: Vec<&str> = text.split('\n').collect();
                    let rest = parts.pop().unwrap_or_default().to_owned();
                    let time = system_time(time.as_ref());
                    let added: Vec<Line> = parts
                        .into_iter()
                        .map(|line| Line {
                            time,
                            text: line.trim_end_matches('\r').to_owned(),
                        })
                        .collect();
                    state.pending[slot] = rest;
                    added
                };
                let trimmed = {
                    let mut state = self.0.state.borrow_mut();
                    state.lines.extend(added.iter().map(|line| Line {
                        time: line.time,
                        text: line.text.clone(),
                    }));
                    if state.lines.len() > MAX_LINES {
                        state.lines.drain(..MAX_LINES / 10);
                        true
                    } else {
                        false
                    }
                };
                if trimmed || !self.query().is_empty() {
                    self.render();
                } else {
                    let at_bottom = self.at_bottom();
                    for line in &added {
                        self.insert_line(line, false);
                    }
                    if at_bottom {
                        self.scroll_to_end();
                    }
                }
            }
            Event::End => {
                self.0.state.borrow_mut().streaming = false;
                self.render();
            }
            Event::Failed(message) => {
                let mut state = self.0.state.borrow_mut();
                state.streaming = false;
                state.error = Some(format!("Can’t read the logs: {message}"));
                drop(state);
                self.render();
            }
        }
    }

    fn query(&self) -> String {
        self.0.entry.text().trim().to_lowercase()
    }

    fn render(&self) {
        let query = self.query();
        let buffer = &self.0.buffer;
        buffer.set_text("");
        let mut state = self.0.state.borrow_mut();
        state.matches = state
            .lines
            .iter()
            .enumerate()
            .filter(|(_, line)| !query.is_empty() && line.text.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        state.current = state.current.min(state.matches.len().saturating_sub(1));
        for (index, line) in state.lines.iter().enumerate() {
            self.insert_line(line, state.matches.binary_search(&index).is_ok());
        }
        let note = state
            .error
            .clone()
            .or_else(|| state.exit_note.clone().filter(|_| !state.streaming));
        if let Some(note) = note {
            let mut end = buffer.end_iter();
            buffer.insert_with_tags_by_name(&mut end, &format!("{note}\n"), &["note"]);
        }
        let total = state.matches.len();
        self.0.count.set_label(&match (query.is_empty(), total) {
            (true, _) => String::new(),
            (false, 0) => "No matches".to_owned(),
            (false, total) => format!("{} of {total}", state.current + 1),
        });
        let target = state.matches.get(state.current).copied();
        drop(state);
        match target {
            Some(line) => self.scroll_to_line(line),
            None => self.scroll_to_end(),
        }
    }

    fn insert_line(&self, line: &Line, matched: bool) {
        let buffer = &self.0.buffer;
        let start = buffer.end_iter().offset();
        let mut end = buffer.end_iter();
        if self.0.timestamps.is_active()
            && let Some(time) = line.time
        {
            buffer.insert_with_tags_by_name(&mut end, &format!("{}  ", clock(time)), &["time"]);
        }
        let text_start = end.offset();
        buffer.insert(&mut end, &line.text);
        buffer.insert(&mut end, "\n");
        if !matched {
            return;
        }
        buffer.apply_tag_by_name("match-line", &buffer.iter_at_offset(start), &buffer.end_iter());
        let query = self.query();
        let lower = line.text.to_lowercase();
        for (byte, found) in lower.match_indices(&query) {
            let from = text_start + count_chars(&lower[..byte]);
            let to = from + count_chars(found);
            buffer.apply_tag_by_name("match", &buffer.iter_at_offset(from), &buffer.iter_at_offset(to));
        }
    }

    fn search(&self) {
        self.0.state.borrow_mut().current = 0;
        self.render();
    }

    fn step(&self, by: isize) {
        let total = self.0.state.borrow().matches.len();
        if total == 0 {
            return;
        }
        {
            let mut state = self.0.state.borrow_mut();
            let total = isize::try_from(total).unwrap_or(1);
            let current = isize::try_from(state.current).unwrap_or(0);
            state.current = usize::try_from((current + by).rem_euclid(total)).unwrap_or(0);
        }
        self.render();
    }

    fn at_bottom(&self) -> bool {
        self.0
            .view
            .vadjustment()
            .is_none_or(|adjustment| adjustment.value() + adjustment.page_size() >= adjustment.upper() - 4.0)
    }

    fn scroll_to_end(&self) {
        let mark = self.0.buffer.create_mark(None, &self.0.buffer.end_iter(), false);
        self.0.view.scroll_mark_onscreen(&mark);
        self.0.buffer.delete_mark(&mark);
    }

    fn scroll_to_line(&self, line: usize) {
        if let Some(mut iter) = self.0.buffer.iter_at_line(i32::try_from(line).unwrap_or(i32::MAX)) {
            self.0.view.scroll_to_iter(&mut iter, 0.1, true, 0.0, 0.3);
        }
    }

    fn save(&self) {
        let text = {
            let buffer = &self.0.buffer;
            buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string()
        };
        let dialog = gtk::FileDialog::builder()
            .title("Save Logs")
            .initial_name(format!("{}.log", self.0.name.borrow()))
            .build();
        let window = self.0.root.root().and_downcast::<gtk::Window>();
        let toast_target = self.0.root.clone();
        dialog.save(window.as_ref(), gio::Cancellable::NONE, move |result| {
            let Ok(file) = result else { return };
            let saved = file.replace_contents(
                text.as_bytes(),
                None,
                false,
                gio::FileCreateFlags::REPLACE_DESTINATION,
                gio::Cancellable::NONE,
            );
            if let Err(error) = saved
                && let Some(overlay) = toast_target.ancestor(adw::ToastOverlay::static_type())
                && let Ok(overlay) = overlay.downcast::<adw::ToastOverlay>()
            {
                overlay.add_toast(adw::Toast::new(&format!("Couldn’t save the logs: {error}")));
            }
        });
    }
}

fn with<W>(weak: &Weak<Inner>, action: impl Fn(&LogsView) + 'static) -> impl Fn(&W) + 'static {
    let weak = weak.clone();
    move |_: &W| {
        if let Some(inner) = weak.upgrade() {
            action(&LogsView(inner));
        }
    }
}

fn count_chars(text: &str) -> i32 {
    i32::try_from(text.chars().count()).unwrap_or(i32::MAX)
}

/// Local time with milliseconds, such as `10:41:02.118`.
fn clock(time: SystemTime) -> String {
    let micros = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_micros()).ok())
        .unwrap_or_default();
    glib::DateTime::from_unix_local(micros / 1_000_000)
        .and_then(|date| date.format("%H:%M:%S"))
        .map(|text| format!("{text}.{:03}", (micros % 1_000_000) / 1_000))
        .unwrap_or_default()
}

/// Drops ANSI escape sequences, which the text view would show as raw characters.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                for c in chars.by_ref() {
                    if c == '\u{7}' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strips_colour_and_title_sequences() {
        assert_eq!(strip_ansi("\u{1b}[31merror\u{1b}[0m: x"), "error: x");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}ok"), "ok");
    }
}
