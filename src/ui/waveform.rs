//! Waveform view: an overview strip of the whole file plus a zoomable main view
//! with a time ruler, a label strip for markers and regions, and one lane per
//! channel.
//!
//! Plain GTK widgets with shared state. The editor owns the markup; this view
//! keeps a copy to draw, previews drags on it and reports finished gestures
//! through [`WaveEvent`].

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use relm4::adw;
use relm4::gtk::{self, cairo, gdk, glib, prelude::*};

use crate::audio::detail::{self, Detail, DetailReader};
use crate::audio::peaks::{self as peaks_mod, Peaks, PeaksReader};
use crate::core::{ColorIdx, Id, Item, Markup, PALETTE};
use crate::lang::tr;

const RULER_H: f64 = 20.0;
const LABEL_H: f64 = 20.0;
const LANES_TOP: f64 = RULER_H + LABEL_H;
const OVERVIEW_H: i32 = 44;
/// Deepest zoom of the smooth waveform; below this the fine peak bins just repeat.
const MIN_FRAMES_PER_PX: f64 = peaks_mod::MIN_FRAMES_PER_PX;
/// Deepest zoom of the detailed waveform, which reads the samples themselves.
const DETAIL_MIN_FRAMES_PER_PX: f64 = detail::MIN_FRAMES_PER_PX;
/// How close (px) the pointer must be to grab a marker or a region edge.
const GRAB_PX: f64 = 5.0;
/// Pointer travel (px) that turns a click into a drag.
const DRAG_THRESHOLD: f64 = 3.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WaveEvent {
    Seek(u64),
    Select(Option<Item>),
    /// Temporary selection (not a region yet) changed.
    Selection(Option<(u64, u64)>),
    MarkerMoved {
        id: Id,
        pos: u64,
    },
    RegionChanged {
        id: Id,
        start: u64,
        end: u64,
    },
    /// Double click: rename.
    Activate(Item),
    /// Shift+click: move the nearer-to-centre edge of the selection or region here.
    Trim(u64),
    /// Right click at widget coordinates, over `frame`.
    Context {
        x: f64,
        y: f64,
        frame: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Start,
    End,
}

/// What is under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Nothing,
    Marker(Id),
    RegionEdge(Id, Edge),
    /// Region label in the label strip: dragging moves the whole region.
    RegionLabel(Id),
    RegionBody(Id),
}

#[derive(Debug, Clone, Copy)]
enum Drag {
    /// Pressed, not moved far enough to decide yet.
    Pending {
        hit: Hit,
        x: f64,
        shift: bool,
    },
    Select {
        anchor: u64,
    },
    Marker {
        id: Id,
    },
    Edge {
        id: Id,
        edge: Edge,
    },
    Move {
        id: Id,
        grab: i64,
    },
}

#[derive(Default)]
struct State {
    peaks: Option<Arc<Peaks>>,
    /// Samples of the visible stretch, for the detailed waveform.
    detail: Option<Arc<Detail>>,
    /// Detailed waveform: a column per pixel, straight edges, zoom down to
    /// single samples. Otherwise smooth curves through the summary.
    detailed: bool,
    drawn_detail: u64,
    sample_rate: u32,
    total_frames: u64,

    markup: Markup,
    selected: Option<Item>,
    selection: Option<(u64, u64)>,
    looping: Option<Id>,

    view_start: f64,
    frames_per_px: f64,
    width: f64,

    playhead: u64,
    playing: bool,
    /// Keep the playhead on screen while playing; off after a manual scroll.
    follow: bool,
    pointer_x: f64,
    drawn_done: u64,
    drag: Option<Drag>,

    on_event: Option<Rc<dyn Fn(WaveEvent)>>,
}

impl State {
    fn visible_frames(&self) -> f64 {
        self.width * self.frames_per_px
    }

    fn min_frames_per_px(&self) -> f64 {
        if self.detailed {
            DETAIL_MIN_FRAMES_PER_PX
        } else {
            MIN_FRAMES_PER_PX
        }
    }

    fn max_frames_per_px(&self) -> f64 {
        (self.total_frames as f64 / self.width.max(1.0)).max(self.min_frames_per_px())
    }

    fn clamp_view(&mut self) {
        self.frames_per_px = self
            .frames_per_px
            .clamp(self.min_frames_per_px(), self.max_frames_per_px());
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

    fn hit(&self, x: f64, y: f64) -> Hit {
        if y < RULER_H {
            return Hit::Nothing;
        }
        let in_labels = y < LANES_TOP;
        let near = |frame: u64| (self.x_of(frame) - x).abs() <= GRAB_PX;

        // Selected items first, so overlapping things stay reachable.
        let marker_order = self
            .markup
            .markers
            .iter()
            .filter(|m| near(m.pos))
            .min_by_key(|m| {
                (
                    self.selected != Some(Item::Marker(m.id)),
                    (self.x_of(m.pos) - x).abs() as i64,
                )
            });
        if let Some(m) = marker_order {
            return Hit::Marker(m.id);
        }
        for r in self.regions_by_priority() {
            if near(r.start) {
                return Hit::RegionEdge(r.id, Edge::Start);
            }
            if near(r.end) {
                return Hit::RegionEdge(r.id, Edge::End);
            }
        }
        let frame = self.frame_at(x);
        let inside = self
            .regions_by_priority()
            .find(|r| (r.start..r.end).contains(&frame));
        match inside {
            Some(r) if in_labels => Hit::RegionLabel(r.id),
            Some(r) => Hit::RegionBody(r.id),
            None => Hit::Nothing,
        }
    }

    /// Selected region first, then shorter ones (they sit "on top" of longer ones).
    fn regions_by_priority(&self) -> impl Iterator<Item = &crate::core::Region> {
        let mut regions: Vec<_> = self.markup.regions.iter().collect();
        regions.sort_by_key(|r| (self.selected != Some(Item::Region(r.id)), r.len()));
        regions.into_iter()
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

    /// The main drawing area, e.g. to anchor a popover.
    pub fn canvas(&self) -> &gtk::DrawingArea {
        &self.main
    }

    pub fn focus(&self) {
        self.main.grab_focus();
    }

    pub fn connect_event(&self, f: impl Fn(WaveEvent) + 'static) {
        self.state.borrow_mut().on_event = Some(Rc::new(f));
    }

    pub fn set_file(&self, peaks: Arc<Peaks>, detail: Arc<Detail>, sample_rate: u32) {
        {
            let mut s = self.state.borrow_mut();
            s.total_frames = peaks.total_frames;
            s.peaks = Some(peaks);
            s.detail = Some(detail);
            s.drawn_detail = 0;
            s.sample_rate = sample_rate;
            s.markup = Markup::default();
            s.selected = None;
            s.selection = None;
            s.looping = None;
            s.drag = None;
            s.playhead = 0;
            s.drawn_done = 0;
            s.view_start = 0.0;
            s.frames_per_px = f64::MAX;
            s.clamp_view();
        }
        self.redraw();
    }

    /// Detailed or smooth waveform (a setting).
    pub fn set_detailed(&self, detailed: bool) {
        let mut s = self.state.borrow_mut();
        s.detailed = detailed;
        s.clamp_view();
        drop(s);
        self.redraw();
    }

    pub fn set_markup(&self, markup: &Markup, selected: Option<Item>) {
        let mut s = self.state.borrow_mut();
        s.markup = markup.clone();
        s.selected = selected;
        // A drag in progress refers to the old copy; drop it rather than fight the editor.
        s.drag = None;
        drop(s);
        self.redraw();
    }

    pub fn set_selection(&self, selection: Option<(u64, u64)>) {
        self.state.borrow_mut().selection = selection;
        self.redraw();
    }

    pub fn set_looping(&self, region: Option<Id>) {
        self.state.borrow_mut().looping = region;
        self.redraw();
    }

    /// Called every frame by the editor; redraws only when something changed.
    pub fn update(&self, playhead: u64, playing: bool) {
        let mut s = self.state.borrow_mut();
        let done = s.peaks.as_ref().map_or(0, |p| p.done_frames());
        let detail = s.detail.as_ref().map_or(0, |d| d.generation());
        if s.playhead == playhead
            && s.playing == playing
            && s.drawn_done == done
            && s.drawn_detail == detail
        {
            return;
        }
        s.drawn_detail = detail;
        if playing && !s.playing {
            s.follow = true;
        }
        s.playhead = playhead;
        s.playing = playing;
        s.drawn_done = done;

        if s.follow && playing && s.drag.is_none() {
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

    /// Calls the editor without holding the state borrow (it may call back into us).
    fn emit(&self, events: &[WaveEvent]) {
        let handler = self.state.borrow().on_event.clone();
        if let Some(handler) = handler {
            for e in events {
                handler(*e);
            }
        }
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
        let drag = gtk::GestureDrag::new();
        drag.set_button(gdk::BUTTON_PRIMARY);
        let wf = self.clone();
        drag.connect_drag_begin(move |g, x, y| {
            wf.main.grab_focus();
            let shift = g
                .current_event_state()
                .contains(gdk::ModifierType::SHIFT_MASK);
            let mut s = wf.state.borrow_mut();
            let hit = s.hit(x, y);
            s.drag = Some(Drag::Pending { hit, x, shift });
        });
        let wf = self.clone();
        drag.connect_drag_update(move |g, dx, _| {
            if let Some((x0, _)) = g.start_point() {
                wf.drag_to(x0 + dx);
            }
        });
        let wf = self.clone();
        drag.connect_drag_end(move |g, dx, _| {
            if let Some((x0, _)) = g.start_point() {
                wf.drag_to(x0 + dx);
            }
            wf.finish_drag();
        });
        self.main.add_controller(drag);

        let double = gtk::GestureClick::new();
        double.set_button(gdk::BUTTON_PRIMARY);
        let wf = self.clone();
        double.connect_pressed(move |_, n, x, y| {
            if n != 2 {
                return;
            }
            let hit = wf.state.borrow().hit(x, y);
            let item = match hit {
                Hit::Marker(id) => Some(Item::Marker(id)),
                Hit::RegionEdge(id, _) | Hit::RegionLabel(id) | Hit::RegionBody(id) => {
                    Some(Item::Region(id))
                }
                Hit::Nothing => None,
            };
            if let Some(item) = item {
                wf.emit(&[WaveEvent::Activate(item)]);
            }
        });
        self.main.add_controller(double);

        let context = gtk::GestureClick::new();
        context.set_button(gdk::BUTTON_SECONDARY);
        let wf = self.clone();
        context.connect_pressed(move |_, _, x, y| {
            wf.main.grab_focus();
            let (hit, frame) = {
                let s = wf.state.borrow();
                (s.hit(x, y), s.frame_at(x))
            };
            let item = match hit {
                Hit::Marker(id) => Some(Item::Marker(id)),
                Hit::RegionEdge(id, _) | Hit::RegionLabel(id) | Hit::RegionBody(id) => {
                    Some(Item::Region(id))
                }
                Hit::Nothing => None,
            };
            // Right click on an item acts on it; on empty space keeps the selection.
            let mut events = Vec::new();
            if item.is_some() {
                events.push(WaveEvent::Select(item));
            }
            events.push(WaveEvent::Context { x, y, frame });
            wf.emit(&events);
        });
        self.main.add_controller(context);

        let motion = gtk::EventControllerMotion::new();
        let wf = self.clone();
        motion.connect_motion(move |_, x, y| {
            let mut s = wf.state.borrow_mut();
            s.pointer_x = x;
            if s.drag.is_none() {
                let cursor = match s.hit(x, y) {
                    Hit::Marker(_) | Hit::RegionEdge(..) => "col-resize",
                    Hit::RegionLabel(_) => "grab",
                    _ => "default",
                };
                wf.main.set_cursor_from_name(Some(cursor));
            }
        });
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

        // Pinch on a touchpad (or a touchscreen) zooms around the fingers,
        // the same way Ctrl+wheel zooms around the pointer.
        let zoom = gtk::GestureZoom::new();
        let started = Rc::new(std::cell::Cell::new(1.0));
        let s = started.clone();
        zoom.connect_begin(move |_, _| s.set(1.0));
        let wf = self.clone();
        zoom.connect_scale_changed(move |gesture, scale| {
            if scale <= 0.0 {
                return;
            }
            // Spreading the fingers (a growing scale) shows fewer frames.
            let factor = started.replace(scale) / scale;
            let mut s = wf.state.borrow_mut();
            let x = gesture
                .bounding_box_center()
                .map_or(s.pointer_x, |(x, _)| x);
            s.zoom_at(x, factor);
            drop(s);
            wf.redraw();
        });
        self.main.add_controller(zoom);
    }

    fn drag_to(&self, x: f64) {
        let mut s = self.state.borrow_mut();
        let frame = s.frame_at(x);
        let Some(drag) = s.drag else { return };

        let drag = match drag {
            Drag::Pending { hit, x: x0, shift } => {
                // Shift+press is always a click (trim), never a drag.
                if shift || (x - x0).abs() < DRAG_THRESHOLD {
                    return;
                }
                let anchor = s.frame_at(x0);
                match hit {
                    Hit::Marker(id) => Drag::Marker { id },
                    Hit::RegionEdge(id, edge) => Drag::Edge { id, edge },
                    Hit::RegionLabel(id) => {
                        let start = s.markup.region(id).map_or(0, |r| r.start);
                        Drag::Move {
                            id,
                            grab: anchor as i64 - start as i64,
                        }
                    }
                    Hit::RegionBody(_) | Hit::Nothing => Drag::Select { anchor },
                }
            }
            other => other,
        };
        s.drag = Some(drag);

        let total = s.total_frames;
        match drag {
            Drag::Pending { .. } => unreachable!(),
            Drag::Select { anchor } => {
                s.selection = Some((anchor.min(frame), anchor.max(frame)));
            }
            Drag::Marker { id } => {
                if let Some(m) = s.markup.markers.iter_mut().find(|m| m.id == id) {
                    m.pos = frame;
                }
            }
            Drag::Edge { id, edge } => {
                if let Some(r) = s.markup.regions.iter_mut().find(|r| r.id == id) {
                    match edge {
                        Edge::Start => r.start = frame.min(r.end.saturating_sub(1)),
                        Edge::End => r.end = frame.max(r.start + 1),
                    }
                }
            }
            Drag::Move { id, grab } => {
                if let Some(r) = s.markup.regions.iter_mut().find(|r| r.id == id) {
                    let len = r.len();
                    let start = (frame as i64 - grab).clamp(0, total.saturating_sub(len) as i64);
                    r.start = start as u64;
                    r.end = r.start + len;
                }
            }
        }
        drop(s);
        self.redraw();
    }

    fn finish_drag(&self) {
        let mut s = self.state.borrow_mut();
        let Some(drag) = s.drag.take() else { return };

        let events = match drag {
            Drag::Pending { x, shift: true, .. } => vec![WaveEvent::Trim(s.frame_at(x))],
            // A plain click: select what was under the pointer and move the playhead.
            Drag::Pending { hit, x, .. } => {
                let frame = s.frame_at(x);
                s.selection = None;
                let (item, seek) = match hit {
                    Hit::Marker(id) => (
                        Some(Item::Marker(id)),
                        s.markup.marker(id).map_or(frame, |m| m.pos),
                    ),
                    Hit::RegionLabel(id) => (
                        Some(Item::Region(id)),
                        s.markup.region(id).map_or(frame, |r| r.start),
                    ),
                    Hit::RegionEdge(id, _) | Hit::RegionBody(id) => (Some(Item::Region(id)), frame),
                    Hit::Nothing => (None, frame),
                };
                vec![
                    WaveEvent::Selection(None),
                    WaveEvent::Select(item),
                    WaveEvent::Seek(seek),
                ]
            }
            Drag::Select { .. } => {
                let selection = s.selection.filter(|(a, b)| b > a);
                s.selection = selection;
                let mut events = vec![WaveEvent::Select(None), WaveEvent::Selection(selection)];
                if let Some((start, _)) = selection {
                    events.push(WaveEvent::Seek(start));
                }
                events
            }
            Drag::Marker { id } => s
                .markup
                .marker(id)
                .map(|m| WaveEvent::MarkerMoved { id, pos: m.pos })
                .into_iter()
                .chain([WaveEvent::Select(Some(Item::Marker(id)))])
                .collect(),
            Drag::Edge { id, .. } | Drag::Move { id, .. } => s
                .markup
                .region(id)
                .map(|r| WaveEvent::RegionChanged {
                    id,
                    start: r.start,
                    end: r.end,
                })
                .into_iter()
                .chain([WaveEvent::Select(Some(Item::Region(id)))])
                .collect(),
        };
        drop(s);
        self.redraw();
        self.emit(&events);
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
}

fn palette(widget: &impl IsA<gtk::Widget>) -> Palette {
    Palette {
        fg: widget.color(),
        accent: adw::StyleManager::default().accent_color_rgba(),
    }
}

pub fn color_rgba(idx: ColorIdx) -> gdk::RGBA {
    let [r, g, b] = PALETTE[usize::from(idx) % PALETTE.len()].1;
    gdk::RGBA::new(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
        1.0,
    )
}

fn set_color(cr: &cairo::Context, c: &gdk::RGBA, alpha: f32) {
    cr.set_source_rgba(
        f64::from(c.red()),
        f64::from(c.green()),
        f64::from(c.blue()),
        f64::from(c.alpha() * alpha),
    );
}

/// Black or white, whichever reads better on `bg`.
fn text_on(bg: &gdk::RGBA) -> gdk::RGBA {
    let luma = 0.299 * bg.red() + 0.587 * bg.green() + 0.114 * bg.blue();
    if luma > 0.6 {
        gdk::RGBA::new(0.1, 0.1, 0.1, 1.0)
    } else {
        gdk::RGBA::WHITE
    }
}

/// Draws `text` clipped to `[x0, x1)` inside the label strip.
fn label_text(cr: &cairo::Context, text: &str, x0: f64, x1: f64) -> Result<(), cairo::Error> {
    if x1 - x0 < 12.0 || text.is_empty() {
        return Ok(());
    }
    cr.save()?;
    cr.rectangle(x0, RULER_H, x1 - x0, LABEL_H);
    cr.clip();
    cr.move_to(x0 + 5.0, RULER_H + 14.0);
    cr.show_text(text)?;
    cr.restore()
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
    let lane_h = (h - LANES_TOP) / channels as f64;

    draw_ruler(cr, w, s, &pal)?;
    draw_regions_under(cr, w, h, s)?;

    let reader = peaks.read();
    // Closer than the summary goes: the samples of the view, once loaded.
    let detail = s
        .detail
        .as_ref()
        .filter(|_| s.detailed && s.frames_per_px < peaks_mod::FINE as f64);
    if let Some(d) = detail {
        d.want(s.view_start, s.view_start + s.visible_frames());
    }
    let detail = detail.map(|d| d.read());
    for ch in 0..channels {
        let top = LANES_TOP + lane_h * ch as f64;
        let mid = top + lane_h / 2.0;
        let half = lane_h / 2.0 - 2.0;

        set_color(cr, &pal.fg, 0.08);
        cr.rectangle(0.0, mid.floor(), w, 1.0);
        cr.fill()?;
        set_color(cr, &pal.fg, 0.12);
        cr.rectangle(0.0, top.floor(), w, 1.0);
        cr.fill()?;

        set_color(cr, &pal.fg, 0.75);
        if s.detailed {
            let points = columns(&reader, detail.as_ref(), ch, s.view_start, s.frames_per_px, w);
            fill_columns(cr, &points, mid, half)?;
        } else {
            let points = envelope(&reader, ch..ch + 1, s.view_start, s.frames_per_px, w);
            fill_envelope(cr, &points, mid, half)?;
        }
    }
    drop(detail);
    drop(reader);

    // Part of the file not analysed yet.
    let done_x = s.x_of(peaks.done_frames());
    if done_x < w {
        set_color(cr, &pal.fg, 0.04);
        cr.rectangle(done_x.max(0.0), LANES_TOP, w, h);
        cr.fill()?;
    }

    if let Some((a, b)) = s.selection {
        let (x0, x1) = (s.x_of(a), s.x_of(b));
        set_color(cr, &pal.accent, 0.22);
        cr.rectangle(x0, LANES_TOP, (x1 - x0).max(1.0), h - LANES_TOP);
        cr.fill()?;
        set_color(cr, &pal.accent, 0.9);
        cr.rectangle(x0.floor(), RULER_H, 1.0, h - RULER_H);
        cr.rectangle(x1.floor(), RULER_H, 1.0, h - RULER_H);
        cr.fill()?;
    }

    draw_region_labels(cr, w, s)?;
    draw_markers(cr, w, h, s)?;

    let x = s.x_of(s.playhead).floor();
    if (0.0..=w).contains(&x) {
        set_color(cr, &pal.accent, 1.0);
        cr.rectangle(x - 0.5, 0.0, 2.0, h);
        cr.fill()?;
    }
    Ok(())
}

fn draw_regions_under(cr: &cairo::Context, w: f64, h: f64, s: &State) -> Result<(), cairo::Error> {
    for r in &s.markup.regions {
        let (x0, x1) = (s.x_of(r.start), s.x_of(r.end));
        if x1 < 0.0 || x0 > w {
            continue;
        }
        let color = color_rgba(r.color);
        let selected = s.selected == Some(Item::Region(r.id));
        let fill = match (selected, r.export) {
            (true, _) => 0.26,
            (false, true) => 0.14,
            (false, false) => 0.06,
        };
        set_color(cr, &color, fill);
        cr.rectangle(x0, LANES_TOP, x1 - x0, h - LANES_TOP);
        cr.fill()?;
        set_color(cr, &color, if selected { 1.0 } else { 0.6 });
        cr.rectangle(x0.floor(), LANES_TOP, 1.0, h - LANES_TOP);
        cr.rectangle(x1.floor(), LANES_TOP, 1.0, h - LANES_TOP);
        cr.fill()?;
    }
    Ok(())
}

fn draw_region_labels(cr: &cairo::Context, w: f64, s: &State) -> Result<(), cairo::Error> {
    cr.set_font_size(11.0);
    // Longer first so shorter regions' labels end up on top.
    let mut regions: Vec<_> = s.markup.regions.iter().collect();
    regions.sort_by_key(|r| std::cmp::Reverse(r.len()));
    for r in regions {
        let (x0, x1) = (s.x_of(r.start).max(-2.0), s.x_of(r.end).min(w + 2.0));
        if x1 < 0.0 || x0 > w {
            continue;
        }
        let color = color_rgba(r.color);
        let selected = s.selected == Some(Item::Region(r.id));
        set_color(cr, &color, if r.export { 0.9 } else { 0.4 });
        cr.rectangle(x0, RULER_H + 1.0, (x1 - x0).max(1.0), LABEL_H - 2.0);
        cr.fill()?;
        if selected {
            set_color(cr, &text_on(&color), 0.9);
            cr.set_line_width(1.5);
            cr.rectangle(
                x0 + 0.75,
                RULER_H + 1.75,
                (x1 - x0 - 1.5).max(1.0),
                LABEL_H - 3.5,
            );
            cr.stroke()?;
        }
        set_color(cr, &text_on(&color), if r.export { 1.0 } else { 0.6 });
        let looping = if s.looping == Some(r.id) { "⟳ " } else { "" };
        let name = if r.name.is_empty() {
            tr("без имени", "untitled")
        } else {
            &r.name
        };
        label_text(cr, &format!("{looping}{name}"), x0, x1)?;
    }
    Ok(())
}

fn draw_markers(cr: &cairo::Context, w: f64, h: f64, s: &State) -> Result<(), cairo::Error> {
    cr.set_font_size(11.0);
    for m in &s.markup.markers {
        let x = s.x_of(m.pos).floor() + 0.5;
        if !(-1.0..=w + 1.0).contains(&x) {
            continue;
        }
        let color = color_rgba(m.color);
        let selected = s.selected == Some(Item::Marker(m.id));
        set_color(cr, &color, 0.95);
        cr.set_line_width(if selected { 2.0 } else { 1.0 });
        if m.from_recorder {
            cr.set_dash(&[3.0, 3.0], 0.0);
        }
        cr.move_to(x, RULER_H);
        cr.line_to(x, h);
        cr.stroke()?;
        cr.set_dash(&[], 0.0);

        // Flag in the label strip, right of the line.
        let text = if m.name.is_empty() { "▾" } else { &m.name };
        let width = cr.text_extents(text).map(|e| e.x_advance()).unwrap_or(30.0) + 10.0;
        set_color(cr, &color, 0.95);
        cr.rectangle(x, RULER_H + 1.0, width, LABEL_H - 2.0);
        cr.fill()?;
        if selected {
            set_color(cr, &text_on(&color), 0.9);
            cr.set_line_width(1.5);
            cr.rectangle(x + 0.75, RULER_H + 1.75, width - 1.5, LABEL_H - 3.5);
            cr.stroke()?;
        }
        set_color(cr, &text_on(&color), 1.0);
        label_text(cr, text, x, x + width)?;
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

    for r in &s.markup.regions {
        set_color(cr, &color_rgba(r.color), if r.export { 0.35 } else { 0.15 });
        let x0 = r.start as f64 / per_px;
        cr.rectangle(x0, 0.0, (r.len() as f64 / per_px).max(1.0), h);
        cr.fill()?;
    }

    let reader = peaks.read();
    set_color(cr, &pal.fg, 0.45);
    let points = envelope(&reader, 0..peaks.channels, 0.0, per_px, w);
    fill_envelope(cr, &points, mid, mid - 2.0)?;
    drop(reader);

    for m in &s.markup.markers {
        set_color(cr, &color_rgba(m.color), 0.9);
        cr.rectangle((m.pos as f64 / per_px).floor(), 0.0, 1.0, h);
        cr.fill()?;
    }

    // Visible window of the main view.
    let x0 = s.view_start / per_px;
    let x1 = (s.view_start + s.visible_frames()) / per_px;
    set_color(cr, &pal.accent, 0.12);
    cr.rectangle(x0, 0.0, (x1 - x0).max(2.0), h);
    cr.fill()?;
    set_color(cr, &pal.accent, 0.8);
    cr.set_line_width(1.0);
    cr.rectangle(x0 + 0.5, 0.5, (x1 - x0).max(2.0) - 1.0, h - 1.0);
    cr.stroke()?;

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

/// Min/max columns of `channels` (merged) across `[0, w)` px from frame
/// `start` at `fpp` frames per pixel: one per pixel, or one per summary bin
/// once a bin spans several pixels, so deep zoom gets a smooth curve between
/// bins instead of steps. Stops where the summary is not computed yet.
pub(crate) fn envelope(
    reader: &PeaksReader,
    channels: std::ops::Range<usize>,
    start: f64,
    fpp: f64,
    w: f64,
) -> Vec<(f64, f32, f32)> {
    let step = (peaks_mod::FINE as f64 / fpp.max(1e-9)).max(1.0);
    let mut out = Vec::with_capacity((w / step) as usize + 2);
    let mut x = 0.0;
    while x < w + step {
        let a = start + x * fpp;
        let (mut lo, mut hi, mut any) = (0f32, 0f32, false);
        for ch in channels.clone() {
            if let Some((min, max)) = reader.min_max(ch, a, a + step * fpp) {
                lo = lo.min(min);
                hi = hi.max(max);
                any = true;
            }
        }
        if !any {
            break;
        }
        out.push((x + step / 2.0, lo, hi));
        x += step;
    }
    out
}

/// Min/max of `channel` for every pixel across `[0, w)` from frame `start`:
/// from the samples where `detail` holds them, else from the summary.
fn columns(
    reader: &PeaksReader,
    detail: Option<&DetailReader>,
    channel: usize,
    start: f64,
    fpp: f64,
    w: f64,
) -> Vec<(f64, f32, f32)> {
    let mut out = Vec::with_capacity(w as usize + 1);
    let mut x = 0.0;
    while x < w {
        let (a, b) = (start + x * fpp, start + (x + 1.0) * fpp);
        let m = detail
            .and_then(|d| d.min_max(channel, a, b))
            .or_else(|| reader.min_max(channel, a, b));
        let Some((lo, hi)) = m else { break };
        out.push((x + 0.5, lo, hi));
        x += 1.0;
    }
    out
}

/// Fills between the max and min of `points` (one per pixel, from
/// [`columns`]) around `mid`, edges straight from pixel to pixel.
fn fill_columns(
    cr: &cairo::Context,
    points: &[(f64, f32, f32)],
    mid: f64,
    half: f64,
) -> Result<(), cairo::Error> {
    let Some(first) = points.first() else {
        return Ok(());
    };
    cr.move_to(first.0, mid - f64::from(first.2) * half - 0.5);
    for &(x, _, hi) in points {
        cr.line_to(x, mid - f64::from(hi) * half - 0.5);
    }
    for &(x, lo, _) in points.iter().rev() {
        cr.line_to(x, mid - f64::from(lo) * half + 0.5);
    }
    cr.close_path();
    cr.fill()
}

/// Fills the area between the max and min curves of `points` (from
/// [`envelope`]) around `mid`, with smooth (Catmull-Rom) edges.
pub(crate) fn fill_envelope(
    cr: &cairo::Context,
    points: &[(f64, f32, f32)],
    mid: f64,
    half: f64,
) -> Result<(), cairo::Error> {
    if points.is_empty() {
        return Ok(());
    }
    let top: Vec<(f64, f64)> = points
        .iter()
        .map(|&(x, _, hi)| (x, mid - f64::from(hi) * half - 0.5))
        .collect();
    let bottom: Vec<(f64, f64)> = points
        .iter()
        .rev()
        .map(|&(x, lo, _)| (x, mid - f64::from(lo) * half + 0.5))
        .collect();
    cr.move_to(top[0].0, top[0].1);
    smooth_through(cr, &top);
    cr.line_to(bottom[0].0, bottom[0].1);
    smooth_through(cr, &bottom);
    cr.close_path();
    cr.fill()
}

/// Cubic segments through `p[1..]` from the current point `p[0]`.
fn smooth_through(cr: &cairo::Context, p: &[(f64, f64)]) {
    let at = |i: isize| p[i.clamp(0, p.len() as isize - 1) as usize];
    for i in 0..p.len().saturating_sub(1) as isize {
        let (p0, p1, p2, p3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
        cr.curve_to(
            p1.0 + (p2.0 - p0.0) / 6.0,
            p1.1 + (p2.1 - p0.1) / 6.0,
            p2.0 - (p3.0 - p1.0) / 6.0,
            p2.1 - (p3.1 - p1.1) / 6.0,
            p2.0,
            p2.1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ChannelMode, Marker, Region};

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
            frames_per_px: 400.0,
            view_start: 200_000.0,
            ..Default::default()
        };
        let before = s.frame_at(250.0);
        s.zoom_at(250.0, 0.5);
        assert_eq!(s.frames_per_px, 200.0);
        assert_eq!(s.frame_at(250.0), before);
        // Never closer than the shared limit.
        s.zoom_at(250.0, 1e-9);
        assert_eq!(s.frames_per_px, MIN_FRAMES_PER_PX);

        s.zoom_at(0.0, 1e9);
        assert_eq!((s.frames_per_px, s.view_start), (1000.0, 0.0));
    }

    #[test]
    fn hit_testing() {
        let region = |id, start, end| Region {
            id,
            file: 1,
            start,
            end,
            name: String::new(),
            color: 0,
            export: true,
            channels: ChannelMode::AsSource,
            speed: 100,
            keep_pitch: false,
            fx: Default::default(),
            parts: Vec::new(),
        };
        // 1 frame per px, so x == frame.
        let s = State {
            total_frames: 1000,
            width: 1000.0,
            frames_per_px: 1.0,
            markup: Markup {
                file: 1,
                markers: vec![Marker {
                    id: 9,
                    file: 1,
                    pos: 500,
                    name: String::new(),
                    color: 0,
                    from_recorder: false,
                }],
                regions: vec![region(1, 100, 400), region(2, 200, 300)],
            },
            ..Default::default()
        };
        let lane = LANES_TOP + 10.0;
        assert_eq!(s.hit(10.0, 5.0), Hit::Nothing); // ruler
        assert_eq!(s.hit(502.0, lane), Hit::Marker(9));
        assert_eq!(s.hit(102.0, lane), Hit::RegionEdge(1, Edge::Start));
        assert_eq!(s.hit(298.0, lane), Hit::RegionEdge(2, Edge::End));
        // Inside both: the shorter one wins.
        assert_eq!(s.hit(250.0, lane), Hit::RegionBody(2));
        assert_eq!(s.hit(150.0, RULER_H + 5.0), Hit::RegionLabel(1));
        assert_eq!(s.hit(700.0, lane), Hit::Nothing);
    }
}
