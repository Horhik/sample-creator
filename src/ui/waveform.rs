//! Waveform view: an overview strip of the whole file plus a zoomable main view
//! with one lane per channel, a time ruler, recorder markers and the playhead.
//!
//! Plain GTK widgets with shared state; the editor component drives it.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use relm4::adw;
use relm4::gtk::{self, cairo, gdk, glib, prelude::*};

use crate::audio::peaks::Peaks;

const RULER_H: f64 = 20.0;
const OVERVIEW_H: i32 = 44;
/// Deepest zoom; below this the fine peak bins just repeat.
const MIN_FRAMES_PER_PX: f64 = 1.0;

pub struct Marker {
    pub frame: u64,
    pub label: String,
}

#[derive(Default)]
struct State {
    peaks: Option<Arc<Peaks>>,
    sample_rate: u32,
    total_frames: u64,
    markers: Vec<Marker>,

    view_start: f64,
    frames_per_px: f64,
    width: f64,

    playhead: u64,
    playing: bool,
    /// Keep the playhead on screen while playing; off after a manual scroll.
    follow: bool,
    pointer_x: f64,
    drawn_done: u64,

    on_seek: Option<Box<dyn Fn(u64)>>,
}

impl State {
    fn visible_frames(&self) -> f64 {
        self.width * self.frames_per_px
    }

    fn max_frames_per_px(&self) -> f64 {
        (self.total_frames as f64 / self.width.max(1.0)).max(MIN_FRAMES_PER_PX)
    }

    fn clamp_view(&mut self) {
        self.frames_per_px = self
            .frames_per_px
            .clamp(MIN_FRAMES_PER_PX, self.max_frames_per_px());
        let max_start = (self.total_frames as f64 - self.visible_frames()).max(0.0);
        self.view_start = self.view_start.clamp(0.0, max_start);
    }

    fn frame_at(&self, x: f64) -> u64 {
        (self.view_start + x * self.frames_per_px).clamp(0.0, self.total_frames as f64) as u64
    }

    fn x_of(&self, frame: u64) -> f64 {
        (frame as f64 - self.view_start) / self.frames_per_px
    }

    fn zoom_at(&mut self, x: f64, factor: f64) {
        let anchor = self.view_start + x * self.frames_per_px;
        self.frames_per_px *= factor;
        self.clamp_view();
        self.view_start = anchor - x * self.frames_per_px;
        self.clamp_view();
    }
}

#[derive(Clone)]
pub struct Waveform {
    root: gtk::Box,
    overview: gtk::DrawingArea,
    main: gtk::DrawingArea,
    state: Rc<RefCell<State>>,
}

impl Default for Waveform {
    fn default() -> Self {
        Self::new()
    }
}

impl Waveform {
    pub fn new() -> Self {
        let overview = gtk::DrawingArea::builder()
            .content_height(OVERVIEW_H)
            .css_classes(["waveform-overview"])
            .build();
        let main = gtk::DrawingArea::builder()
            .vexpand(true)
            .hexpand(true)
            .focusable(true)
            .css_classes(["waveform"])
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.append(&overview);
        root.append(&main);

        let wf = Waveform {
            root,
            overview,
            main,
            state: Rc::new(RefCell::new(State {
                frames_per_px: 1.0,
                width: 1.0,
                ..Default::default()
            })),
        };
        wf.connect_drawing();
        wf.connect_main_input();
        wf.connect_overview_input();
        wf
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.root
    }

    pub fn connect_seek(&self, f: impl Fn(u64) + 'static) {
        self.state.borrow_mut().on_seek = Some(Box::new(f));
    }

    pub fn set_file(&self, peaks: Arc<Peaks>, sample_rate: u32, markers: Vec<Marker>) {
        {
            let mut s = self.state.borrow_mut();
            s.total_frames = peaks.total_frames;
            s.peaks = Some(peaks);
            s.sample_rate = sample_rate;
            s.markers = markers;
            s.playhead = 0;
            s.drawn_done = 0;
            s.view_start = 0.0;
            s.frames_per_px = f64::MAX;
            s.clamp_view();
        }
        self.redraw();
    }

    /// Called every frame by the editor; redraws only when something changed.
    pub fn update(&self, playhead: u64, playing: bool) {
        let mut s = self.state.borrow_mut();
        let done = s.peaks.as_ref().map_or(0, |p| p.done_frames());
        if s.playhead == playhead && s.playing == playing && s.drawn_done == done {
            return;
        }
        if playing && !s.playing {
            s.follow = true;
        }
        s.playhead = playhead;
        s.playing = playing;
        s.drawn_done = done;

        if s.follow && playing {
            let x = s.x_of(playhead);
            if x < 0.0 || x > s.width * 0.95 {
                s.view_start = playhead as f64 - s.visible_frames() * 0.05;
                s.clamp_view();
            }
        }
        drop(s);
        self.redraw();
    }

    pub fn zoom(&self, factor: f64) {
        let mut s = self.state.borrow_mut();
        // Zoom around the playhead when it is visible, otherwise around the centre.
        let x = s.x_of(s.playhead);
        let x = if (0.0..=s.width).contains(&x) {
            x
        } else {
            s.width / 2.0
        };
        s.zoom_at(x, factor);
        drop(s);
        self.redraw();
    }

    pub fn zoom_fit(&self) {
        let mut s = self.state.borrow_mut();
        s.frames_per_px = f64::MAX;
        s.clamp_view();
        drop(s);
        self.redraw();
    }

    /// Scroll the main view so `frame` is visible (used after keyboard seeks).
    pub fn reveal(&self, frame: u64) {
        let mut s = self.state.borrow_mut();
        let x = s.x_of(frame);
        if x < 0.0 || x > s.width {
            s.view_start = frame as f64 - s.visible_frames() / 2.0;
            s.clamp_view();
        }
        s.follow = true;
        drop(s);
        self.redraw();
    }

    /// Share of the file analysed so far, `None` when no file is loaded.
    pub fn analysis_progress(&self) -> Option<f64> {
        let s = self.state.borrow();
        let peaks = s.peaks.as_ref()?;
        Some(peaks.done_frames() as f64 / peaks.total_frames.max(1) as f64)
    }

    /// Sample rate and length of the loaded file.
    pub fn timing(&self) -> (f64, u64) {
        let s = self.state.borrow();
        (f64::from(s.sample_rate.max(1)), s.total_frames)
    }

    fn redraw(&self) {
        self.main.queue_draw();
        self.overview.queue_draw();
    }

    fn connect_drawing(&self) {
        let state = self.state.clone();
        self.main.connect_resize(move |_, w, _| {
            let mut s = state.borrow_mut();
            let fit = s.frames_per_px >= s.max_frames_per_px();
            s.width = f64::from(w.max(1));
            if fit {
                s.frames_per_px = f64::MAX;
            }
            s.clamp_view();
        });

        let state = self.state.clone();
        self.main.set_draw_func(move |area, cr, w, h| {
            draw_main(area, cr, f64::from(w), f64::from(h), &state.borrow()).ok();
        });

        let state = self.state.clone();
        self.overview.set_draw_func(move |area, cr, w, h| {
            draw_overview(area, cr, f64::from(w), f64::from(h), &state.borrow()).ok();
        });
    }

    fn connect_main_input(&self) {
        let click = gtk::GestureClick::new();
        let (state, main) = (self.state.clone(), self.main.clone());
        click.connect_pressed(move |_, _, x, _| {
            main.grab_focus();
            let s = state.borrow();
            let frame = s.frame_at(x);
            if let Some(seek) = &s.on_seek {
                seek(frame);
            }
        });
        self.main.add_controller(click);

        let motion = gtk::EventControllerMotion::new();
        let state = self.state.clone();
        motion.connect_motion(move |_, x, _| state.borrow_mut().pointer_x = x);
        self.main.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let wf = self.clone();
        scroll.connect_scroll(move |ctl, dx, dy| {
            let ctrl = ctl
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK);
            let mut s = wf.state.borrow_mut();
            if ctrl {
                let x = s.pointer_x;
                s.zoom_at(x, 1.25f64.powf(dy));
            } else {
                // Vertical wheel pans too: most mice have no horizontal wheel.
                let delta = if dx != 0.0 { dx } else { dy };
                s.view_start += delta * 40.0 * s.frames_per_px;
                s.clamp_view();
                s.follow = false;
            }
            drop(s);
            wf.redraw();
            glib::Propagation::Stop
        });
        self.main.add_controller(scroll);
    }

    fn connect_overview_input(&self) {
        let drag = gtk::GestureDrag::new();
        let wf = self.clone();
        let center_on = move |x: f64| {
            let mut s = wf.state.borrow_mut();
            let w = wf.overview.width().max(1) as f64;
            let frame = x.clamp(0.0, w) / w * s.total_frames as f64;
            s.view_start = frame - s.visible_frames() / 2.0;
            s.clamp_view();
            s.follow = false;
            drop(s);
            wf.redraw();
        };
        let on_begin = center_on.clone();
        drag.connect_drag_begin(move |_, x, _| on_begin(x));
        drag.connect_drag_update(move |g, dx, _| {
            if let Some((x, _)) = g.start_point() {
                center_on(x + dx);
            }
        });
        self.overview.add_controller(drag);
    }
}

struct Palette {
    fg: gdk::RGBA,
    accent: gdk::RGBA,
    marker: gdk::RGBA,
}

fn palette(widget: &impl IsA<gtk::Widget>) -> Palette {
    Palette {
        fg: widget.color(),
        accent: adw::StyleManager::default().accent_color_rgba(),
        marker: gdk::RGBA::new(0.96, 0.76, 0.07, 1.0),
    }
}

fn set_color(cr: &cairo::Context, c: &gdk::RGBA, alpha: f32) {
    cr.set_source_rgba(
        f64::from(c.red()),
        f64::from(c.green()),
        f64::from(c.blue()),
        f64::from(c.alpha() * alpha),
    );
}

fn draw_main(
    area: &gtk::DrawingArea,
    cr: &cairo::Context,
    w: f64,
    h: f64,
    s: &State,
) -> Result<(), cairo::Error> {
    let Some(peaks) = &s.peaks else { return Ok(()) };
    let pal = palette(area);
    let channels = peaks.channels.max(1);
    let lane_h = (h - RULER_H) / channels as f64;

    draw_ruler(cr, w, s, &pal)?;

    let reader = peaks.read();
    for ch in 0..channels {
        let top = RULER_H + lane_h * ch as f64;
        let mid = top + lane_h / 2.0;
        let half = lane_h / 2.0 - 2.0;

        set_color(cr, &pal.fg, 0.08);
        cr.rectangle(0.0, mid.floor(), w, 1.0);
        cr.fill()?;
        if ch > 0 {
            set_color(cr, &pal.fg, 0.12);
            cr.rectangle(0.0, top.floor(), w, 1.0);
            cr.fill()?;
        }

        set_color(cr, &pal.fg, 0.75);
        for x in 0..w as usize {
            let a = s.view_start + x as f64 * s.frames_per_px;
            let Some((min, max)) = reader.min_max(ch, a, a + s.frames_per_px) else {
                break;
            };
            let y0 = mid - f64::from(max) * half;
            let y1 = mid - f64::from(min) * half;
            cr.rectangle(x as f64, y0, 1.0, (y1 - y0).max(1.0));
        }
        cr.fill()?;
    }
    drop(reader);

    // Part of the file not analysed yet.
    let done_x = s.x_of(peaks.done_frames());
    if done_x < w {
        set_color(cr, &pal.fg, 0.04);
        cr.rectangle(done_x.max(0.0), RULER_H, w, h);
        cr.fill()?;
    }

    cr.set_font_size(11.0);
    for m in &s.markers {
        let x = s.x_of(m.frame).floor() + 0.5;
        if !(0.0..=w).contains(&x) {
            continue;
        }
        set_color(cr, &pal.marker, 0.9);
        cr.set_dash(&[3.0, 3.0], 0.0);
        cr.set_line_width(1.0);
        cr.move_to(x, RULER_H);
        cr.line_to(x, h);
        cr.stroke()?;
        cr.set_dash(&[], 0.0);
        cr.move_to(x + 4.0, RULER_H + 14.0);
        cr.show_text(&m.label)?;
    }

    let x = s.x_of(s.playhead).floor();
    if (0.0..=w).contains(&x) {
        set_color(cr, &pal.accent, 1.0);
        cr.rectangle(x - 0.5, 0.0, 2.0, h);
        cr.fill()?;
    }
    Ok(())
}

fn draw_ruler(cr: &cairo::Context, w: f64, s: &State, pal: &Palette) -> Result<(), cairo::Error> {
    let rate = f64::from(s.sample_rate.max(1));
    let px_per_sec = rate / s.frames_per_px;
    const STEPS: &[f64] = &[
        0.001, 0.002, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5, 1.0, 2.0, 5.0, 10.0, 15.0, 30.0,
        60.0, 120.0, 300.0, 600.0, 900.0, 1800.0, 3600.0,
    ];
    let step = STEPS
        .iter()
        .copied()
        .find(|st| st * px_per_sec >= 90.0)
        .unwrap_or(7200.0);

    set_color(cr, &pal.fg, 0.12);
    cr.rectangle(0.0, RULER_H - 1.0, w, 1.0);
    cr.fill()?;

    cr.set_font_size(10.0);
    let first = (s.view_start / rate / step).floor() as i64;
    let mut i = first;
    loop {
        let t = i as f64 * step;
        let x = (t * rate - s.view_start) / s.frames_per_px;
        if x > w {
            break;
        }
        if x >= 0.0 {
            set_color(cr, &pal.fg, 0.3);
            cr.rectangle(x.floor(), RULER_H - 6.0, 1.0, 6.0);
            cr.fill()?;
            set_color(cr, &pal.fg, 0.55);
            cr.move_to(x + 3.0, RULER_H - 7.0);
            cr.show_text(&format_time(t, step < 1.0))?;
        }
        i += 1;
    }
    Ok(())
}

fn draw_overview(
    area: &gtk::DrawingArea,
    cr: &cairo::Context,
    w: f64,
    h: f64,
    s: &State,
) -> Result<(), cairo::Error> {
    let Some(peaks) = &s.peaks else { return Ok(()) };
    let pal = palette(area);
    let per_px = s.total_frames as f64 / w.max(1.0);
    let mid = h / 2.0;

    set_color(cr, &pal.fg, 0.05);
    cr.rectangle(0.0, 0.0, w, h);
    cr.fill()?;

    let reader = peaks.read();
    set_color(cr, &pal.fg, 0.45);
    for x in 0..w as usize {
        let a = x as f64 * per_px;
        let mut lo = 0f32;
        let mut hi = 0f32;
        let mut any = false;
        for ch in 0..peaks.channels {
            if let Some((min, max)) = reader.min_max(ch, a, a + per_px) {
                lo = lo.min(min);
                hi = hi.max(max);
                any = true;
            }
        }
        if !any {
            break;
        }
        let y0 = mid - f64::from(hi) * (mid - 2.0);
        let y1 = mid - f64::from(lo) * (mid - 2.0);
        cr.rectangle(x as f64, y0, 1.0, (y1 - y0).max(1.0));
    }
    cr.fill()?;
    drop(reader);

    // Visible window of the main view.
    let x0 = s.view_start / per_px;
    let x1 = (s.view_start + s.visible_frames()) / per_px;
    set_color(cr, &pal.accent, 0.18);
    cr.rectangle(x0, 0.0, (x1 - x0).max(2.0), h);
    cr.fill()?;
    set_color(cr, &pal.accent, 0.8);
    cr.set_line_width(1.0);
    cr.rectangle(x0 + 0.5, 0.5, (x1 - x0).max(2.0) - 1.0, h - 1.0);
    cr.stroke()?;

    for m in &s.markers {
        set_color(cr, &pal.marker, 0.8);
        cr.rectangle((m.frame as f64 / per_px).floor(), 0.0, 1.0, h);
        cr.fill()?;
    }

    set_color(cr, &pal.accent, 1.0);
    cr.rectangle((s.playhead as f64 / per_px).floor(), 0.0, 2.0, h);
    cr.fill()?;
    Ok(())
}

/// `h:mm:ss`, `m:ss`, with milliseconds when `fine`.
pub fn format_time(secs: f64, fine: bool) -> String {
    let total_ms = (secs * 1000.0).round() as u64;
    let (h, m, s, ms) = (
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1000 % 60,
        total_ms % 1000,
    );
    let base = if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    };
    if fine {
        format!("{base}.{ms:03}")
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_format() {
        assert_eq!(format_time(0.0, false), "0:00");
        assert_eq!(format_time(75.5, true), "1:15.500");
        assert_eq!(
            format_time(2.0 * 3600.0 + 41.0 * 60.0 + 33.0, false),
            "2:41:33"
        );
    }

    #[test]
    fn zoom_keeps_anchor_frame_under_pointer() {
        let mut s = State {
            total_frames: 1_000_000,
            width: 1000.0,
            frames_per_px: 100.0,
            view_start: 200_000.0,
            ..Default::default()
        };
        let before = s.frame_at(250.0);
        s.zoom_at(250.0, 0.5);
        assert_eq!(s.frames_per_px, 50.0);
        assert_eq!(s.frame_at(250.0), before);

        s.zoom_at(0.0, 1e9);
        assert_eq!((s.frames_per_px, s.view_start), (1000.0, 0.0));
    }
}
