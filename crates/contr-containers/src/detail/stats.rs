//! The Stats view: CPU, memory, network and disk use, plotted from the samples received since the view opened.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::time::SystemTime;

use contr_daemon_ui::{describe, system_time};
use contr_sdk::Client;
use contr_sdk::api::containers::v1::{ContainerStats, StreamStatsRequest};
use contr_sdk::api::engine::v1::StreamUsageRequest;
use contr_ui_common::format;
use contr_ui_common::sparkline::{Series, Sparkline, Stroke};
use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gdk, glib};
use tokio::sync::mpsc;

/// Five minutes of the engine's one-per-second samples.
const CAPACITY: usize = 300;

#[derive(Clone, Copy, Default)]
struct Sample {
    cpu: f64,
    memory: u64,
    limit: u64,
    memory_percent: f64,
    received: f64,
    sent: f64,
    read: f64,
    written: f64,
}

#[derive(Default)]
struct State {
    samples: VecDeque<Sample>,
    last: Option<(SystemTime, ContainerStats)>,
    cpus: Option<u32>,
    paused: bool,
    task: Option<tokio::task::JoinHandle<()>>,
}

struct Card {
    value: gtk::Label,
    detail: gtk::Label,
    chart: Sparkline,
}

struct Inner {
    root: gtk::Stack,
    cpu: Card,
    memory: Card,
    network: Card,
    disk: Card,
    pause: gtk::ToggleButton,
    error: adw::StatusPage,
    state: RefCell<State>,
}

#[derive(Clone)]
pub(crate) struct StatsView(Rc<Inner>);

enum Event {
    Sample(ContainerStats),
    Cpus(u32),
    Failed(String),
}

impl StatsView {
    pub(crate) fn new() -> Self {
        let (cpu_box, cpu) = card("CPU", "stat-value", None);
        let (memory_box, memory) = card("Memory", "stat-value", None);
        let (network_box, network) = card("Network", "stat-rate", Some(legend()));
        let (disk_box, disk) = card("Disk", "stat-rate", None);
        let cards = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .min_children_per_line(1)
            .max_children_per_line(3)
            .column_spacing(12)
            .row_spacing(12)
            .valign(gtk::Align::Start)
            .build();
        for card in [&cpu_box, &memory_box, &network_box, &disk_box] {
            cards.append(card);
        }
        let pause = gtk::ToggleButton::builder()
            .child(
                &adw::ButtonContent::builder()
                    .icon_name("media-playback-pause-symbolic")
                    .label("Pause")
                    .build(),
            )
            .css_classes(["flat"])
            .halign(gtk::Align::End)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .build();
        content.append(&pause);
        content.append(&cards);
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .vexpand(true)
            .build();
        let stopped = adw::StatusPage::builder()
            .icon_name("contr-stats-symbolic")
            .title("The Container Isn’t Running")
            .description("Start it to see its resource use.")
            .build();
        let error = adw::StatusPage::builder()
            .icon_name("contr-stats-symbolic")
            .title("Can’t Read the Container’s Stats")
            .build();
        let root = gtk::Stack::new();
        root.add_named(&scroller, Some("stats"));
        root.add_named(&stopped, Some("stopped"));
        root.add_named(&error, Some("error"));

        let view = Self(Rc::new(Inner {
            root,
            cpu,
            memory,
            network,
            disk,
            pause,
            error,
            state: RefCell::default(),
        }));
        let weak = Rc::downgrade(&view.0);
        view.0.pause.connect_toggled(move |pause| {
            if let Some(inner) = weak.upgrade() {
                inner.state.borrow_mut().paused = pause.is_active();
                if !pause.is_active() {
                    StatsView(inner).render();
                }
            }
        });
        view
    }

    pub(crate) fn widget(&self) -> &gtk::Stack {
        &self.0.root
    }

    pub(crate) fn follow(&self, client: Option<Client>, id: &str, running: bool) {
        self.stop();
        if !running {
            self.0.root.set_visible_child_name("stopped");
            return;
        }
        let Some(client) = client else { return };
        {
            let mut state = self.0.state.borrow_mut();
            state.samples.clear();
            state.last = None;
        }
        self.0.root.set_visible_child_name("stats");
        self.render();

        let (sender, mut receiver) = mpsc::unbounded_channel();
        let request = StreamStatsRequest {
            id: id.to_owned(),
            one_sample: false,
        };
        let task = relm4::spawn(async move {
            if let Ok(usage) = client.engine().stream_usage(StreamUsageRequest {}).await
                && let Ok(Some(first)) = usage.into_inner().message().await
                && let Some(usage) = first.usage
            {
                let _ = sender.send(Event::Cpus(usage.cpus));
            }
            let mut stream = match client.containers().stream_stats(request).await {
                Ok(stream) => stream.into_inner(),
                Err(status) => {
                    let _ = sender.send(Event::Failed(describe(&contr_sdk::Error::from(status))));
                    return;
                }
            };
            while let Ok(Some(response)) = stream.message().await {
                if let Some(stats) = response.stats
                    && sender.send(Event::Sample(stats)).is_err()
                {
                    return;
                }
            }
        });
        self.0.state.borrow_mut().task = Some(task);

        let weak: Weak<Inner> = Rc::downgrade(&self.0);
        glib::spawn_future_local(async move {
            while let Some(event) = receiver.recv().await {
                let Some(inner) = weak.upgrade() else { return };
                let view = StatsView(inner);
                match event {
                    Event::Sample(stats) => view.add(stats),
                    Event::Cpus(cpus) => view.0.state.borrow_mut().cpus = Some(cpus),
                    Event::Failed(message) => {
                        view.0.error.set_description(Some(&glib::markup_escape_text(&message)));
                        view.0.root.set_visible_child_name("error");
                    }
                }
            }
        });
    }

    pub(crate) fn stop(&self) {
        if let Some(task) = self.0.state.borrow_mut().task.take() {
            task.abort();
        }
    }

    fn add(&self, stats: ContainerStats) {
        let now = system_time(stats.read_at.as_ref()).unwrap_or_else(SystemTime::now);
        {
            let mut state = self.0.state.borrow_mut();
            let rate = |current: u64, previous: u64, seconds: f64| {
                #[allow(clippy::cast_precision_loss)]
                let delta = current.saturating_sub(previous) as f64;
                if seconds > 0.0 { delta / seconds } else { 0.0 }
            };
            let (received, sent, read, written) = match &state.last {
                Some((at, previous)) => {
                    let seconds = now.duration_since(*at).map_or(0.0, |elapsed| elapsed.as_secs_f64());
                    (
                        rate(stats.network_rx_bytes, previous.network_rx_bytes, seconds),
                        rate(stats.network_tx_bytes, previous.network_tx_bytes, seconds),
                        rate(stats.block_read_bytes, previous.block_read_bytes, seconds),
                        rate(stats.block_write_bytes, previous.block_write_bytes, seconds),
                    )
                }
                None => (0.0, 0.0, 0.0, 0.0),
            };
            state.samples.push_back(Sample {
                cpu: stats.cpu_percent,
                memory: stats.memory_usage_bytes,
                limit: stats.memory_limit_bytes,
                memory_percent: stats.memory_percent,
                received,
                sent,
                read,
                written,
            });
            while state.samples.len() > CAPACITY {
                state.samples.pop_front();
            }
            state.last = Some((now, stats));
        }
        if !self.0.state.borrow().paused {
            self.render();
        }
    }

    fn render(&self) {
        let state = self.0.state.borrow();
        let accent = adw::StyleManager::default().accent_color_rgba();
        let sent_color = gdk::RGBA::new(0.569, 0.255, 0.675, 1.0);
        let limit_color = gdk::RGBA::new(0.878, 0.106, 0.141, 1.0);
        let latest = state.samples.back().copied().unwrap_or_default();
        let series = |values: Vec<f64>, color: gdk::RGBA, stroke: Stroke| Series { values, color, stroke };

        let peak = state.samples.iter().map(|sample| sample.cpu).fold(0.0, f64::max);
        self.0.cpu.value.set_label(&format::percent(latest.cpu));
        self.0.cpu.detail.set_label(&match state.cpus {
            Some(cpus) => format!("of {cpus} cores · peak {}", format::percent(peak)),
            None => format!("peak {}", format::percent(peak)),
        });
        self.0.cpu.chart.set(
            vec![series(
                state.samples.iter().map(|sample| sample.cpu).collect(),
                accent,
                Stroke::Filled,
            )],
            None,
        );

        self.0.memory.value.set_label(&format::bytes(latest.memory));
        self.0.memory.detail.set_label(&if latest.limit > 0 {
            format!(
                "of {} limit · {:.0}%",
                format::bytes(latest.limit),
                latest.memory_percent
            )
        } else {
            String::new()
        });
        #[allow(clippy::cast_precision_loss)]
        self.0.memory.chart.set(
            vec![series(
                state.samples.iter().map(|sample| sample.memory as f64).collect(),
                accent,
                Stroke::Filled,
            )],
            (latest.limit > 0).then_some((latest.limit as f64, limit_color)),
        );

        self.0.network.value.set_label(&format!(
            "↓ {} · ↑ {}",
            format::rate(latest.received),
            format::rate(latest.sent)
        ));
        self.0.network.chart.set(
            vec![
                series(
                    state.samples.iter().map(|sample| sample.received).collect(),
                    accent,
                    Stroke::Filled,
                ),
                series(
                    state.samples.iter().map(|sample| sample.sent).collect(),
                    sent_color,
                    Stroke::Dashed,
                ),
            ],
            None,
        );

        self.0.disk.value.set_label(&format!(
            "Read {} · Write {}",
            format::rate(latest.read),
            format::rate(latest.written)
        ));
        self.0.disk.chart.set(
            vec![
                series(
                    state.samples.iter().map(|sample| sample.read).collect(),
                    accent,
                    Stroke::Filled,
                ),
                series(
                    state.samples.iter().map(|sample| sample.written).collect(),
                    sent_color,
                    Stroke::Dashed,
                ),
            ],
            None,
        );
    }
}

fn card(title: &str, value_class: &str, legend: Option<gtk::Box>) -> (gtk::Box, Card) {
    let heading = gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["group-title"])
        .build();
    let value = gtk::Label::builder().css_classes([value_class]).build();
    let top = gtk::Box::builder().spacing(8).build();
    top.append(&heading);
    top.append(&value);
    let detail = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["dim-label", "group-description"])
        .build();
    let chart = Sparkline::new(CAPACITY, 72);
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["card", "stat-card"])
        .build();
    root.append(&top);
    match legend {
        Some(legend) => root.append(&legend),
        None => root.append(&detail),
    }
    root.append(chart.widget());
    (root, Card { value, detail, chart })
}

fn legend() -> gtk::Box {
    let item = |class: &str, text: &str| {
        let line = gtk::Box::builder()
            .css_classes([class])
            .valign(gtk::Align::Center)
            .build();
        let label = gtk::Label::builder()
            .label(text)
            .css_classes(["dim-label", "group-description"])
            .build();
        let item = gtk::Box::builder().spacing(6).build();
        item.append(&line);
        item.append(&label);
        item
    };
    let legend = gtk::Box::builder().spacing(14).build();
    legend.append(&item("legend-received", "Received"));
    legend.append(&item("legend-sent", "Sent"));
    legend
}
