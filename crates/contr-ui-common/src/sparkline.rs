//! A small line chart of recent samples, as the Stats cards draw them.

use std::cell::RefCell;
use std::rc::Rc;

use relm4::gtk::{self, cairo, gdk, prelude::*};

/// How one series is stroked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stroke {
    /// Solid line with a light fill below it.
    Filled,
    Dashed,
}

#[derive(Clone, Debug)]
pub struct Series {
    pub values: Vec<f64>,
    pub color: gdk::RGBA,
    pub stroke: Stroke,
}

#[derive(Default)]
struct Data {
    series: Vec<Series>,
    /// Drawn as a dashed line; also the top of the scale when set.
    limit: Option<(f64, gdk::RGBA)>,
    /// Most samples across the width; fewer are stretched over it.
    capacity: usize,
}

/// A drawing area that plots one or more series of the same length, newest on the right.
#[derive(Clone)]
pub struct Sparkline {
    area: gtk::DrawingArea,
    data: Rc<RefCell<Data>>,
}

impl Sparkline {
    pub fn new(capacity: usize, height: i32) -> Self {
        let area = gtk::DrawingArea::builder()
            .content_height(height)
            .hexpand(true)
            .margin_top(8)
            .build();
        let data = Rc::new(RefCell::new(Data {
            capacity,
            ..Data::default()
        }));
        let shared = Rc::clone(&data);
        area.set_draw_func(move |_, cr, width, height| draw(&shared.borrow(), cr, width, height));
        Self { area, data }
    }

    pub fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub fn set(&self, series: Vec<Series>, limit: Option<(f64, gdk::RGBA)>) {
        {
            let mut data = self.data.borrow_mut();
            data.series = series;
            data.limit = limit;
        }
        self.area.queue_draw();
    }
}

fn draw(data: &Data, cr: &cairo::Context, width: i32, height: i32) {
    let (width, height) = (f64::from(width), f64::from(height));
    let top = data
        .series
        .iter()
        .flat_map(|series| series.values.iter().copied())
        .chain(data.limit.map(|(limit, _)| limit))
        .fold(0.0_f64, f64::max);
    let scale = if top > 0.0 { top * 1.1 } else { 1.0 };
    let longest = data.series.iter().map(|series| series.values.len()).max().unwrap_or(0);
    let slots = longest.clamp(2, data.capacity.max(2));
    #[allow(clippy::cast_precision_loss)]
    let step = width / (slots - 1) as f64;
    let y = |value: f64| height - 1.0 - (value / scale) * (height - 2.0);

    if let Some((limit, color)) = data.limit {
        set_color(cr, &color, 0.8);
        cr.set_line_width(1.0);
        cr.set_dash(&[3.0, 3.0], 0.0);
        cr.move_to(0.0, y(limit));
        cr.line_to(width, y(limit));
        let _ = cr.stroke();
    }

    for series in &data.series {
        let count = series.values.len();
        if count < 2 {
            continue;
        }
        let skip = slots.saturating_sub(count);
        #[allow(clippy::cast_precision_loss)]
        let x = |index: usize| (skip + index) as f64 * step;
        cr.new_path();
        for (index, value) in series.values.iter().enumerate() {
            cr.line_to(x(index), y(*value));
        }
        match series.stroke {
            Stroke::Filled => {
                cr.set_dash(&[], 0.0);
                let path = cr.copy_path();
                cr.line_to(x(count - 1), height);
                cr.line_to(x(0), height);
                cr.close_path();
                set_color(cr, &series.color, 0.12);
                let _ = cr.fill();
                if let Ok(path) = path {
                    cr.append_path(&path);
                }
            }
            Stroke::Dashed => cr.set_dash(&[4.0, 3.0], 0.0),
        }
        set_color(cr, &series.color, 1.0);
        cr.set_line_width(1.5);
        cr.set_line_join(cairo::LineJoin::Round);
        let _ = cr.stroke();
    }
}

fn set_color(cr: &cairo::Context, color: &gdk::RGBA, alpha: f32) {
    cr.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        f64::from(color.alpha() * alpha),
    );
}
