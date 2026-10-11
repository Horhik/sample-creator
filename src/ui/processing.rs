//! "Обработка": simple per-region editing: speed (with or without keeping
//! pitch) and cutting the region into parts that are spliced back with fades. Settings are saved to the library as they change; the
//! processed sound is rendered on demand for preview and again on export.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, glib};
use relm4::{ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent};

use super::markup_list::color_swatch;
use super::waveform::{envelope, fill_envelope, format_time};
use crate::audio::peaks::{self, PeakJob, Peaks};
use crate::audio::player::Player;
use crate::core::history::RegionHistory;
use crate::core::parts::{output_to_source, part_fades, positions, spliced_len};
use crate::core::{Id, PALETTE, Part, Region, SPEED_MAX, SPEED_MIN, SPEED_NORMAL};
use crate::export;
use crate::lang::{sec, tr};
use crate::store::{FileRow, Library};

const PRESETS: [u16; 7] = [50, 75, 100, 125, 150, 200, 300];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Playing {
    Original,
    Result,
}

pub struct Processing {
    player: Option<Rc<Player>>,
    library: Rc<Library>,
    rows: Vec<(FileRow, Region)>,
    selected: Option<Id>,
    /// Rendering the result for preview.
    busy: bool,
    playing: Option<Playing>,
    /// Bumped on every change so a slow render of old settings is ignored.
    generation: u64,
    /// Undo for edits made here, across recordings.
    history: RegionHistory,

    list: gtk::ListBox,
    /// The selected region's waveform with its parts and the cut cursor.
    wave: gtk::DrawingArea,
    wave_state: Rc<RefCell<WaveState>>,
    /// Summary of the selected region's recording, for the waveform.
    wave_peaks: Option<(Id, PeakJob)>,
    parts_box: gtk::Box,
    parts_sender: Option<relm4::Sender<ProcessingMsg>>,
    speed_scale: gtk::Scale,
    keep_pitch: gtk::Switch,
    /// Set while the controls are being filled from the model.
    updating: Rc<std::cell::Cell<bool>>,
    toasts: adw::ToastOverlay,
}

#[derive(Debug)]
pub enum ProcessingMsg {
    Refresh,
    Leave,
    Select(Option<Id>),
    Speed(u16),
    KeepPitch(bool),
    /// Click on the waveform: where the next cut goes.
    Cursor(u64),
    Cut,
    ResetParts,
    RemovePart(usize),
    MovePart(usize, bool),
    /// Part index, fade-in and fade-out in milliseconds.
    Fades(usize, u32, u32),
    /// A dragged part, as it should be stored.
    ReplacePart(usize, Part),
    /// Part index and its overlap over the previous one in ms (negative: pause).
    Overlap(usize, i32),
    Undo,
    Redo,
    /// A markup file brought regions from a volume: ids changed.
    Synced,
    /// Delete / Backspace: drop the part selected on the timeline.
    DeleteSelectedPart,
    PlayOriginal,
    PlayResult,
    Rendered {
        generation: u64,
        result: Result<(Arc<Vec<f32>>, u16, u32), String>,
    },
    Stopped,
}

#[relm4::component(pub)]
impl SimpleComponent for Processing {
    type Init = (Option<Rc<Player>>, Rc<Library>);
    type Input = ProcessingMsg;
    type Output = ();

    view! {
        #[root]
        adw::ToastOverlay {
            #[wrap(Some)]
            set_child = &gtk::Paned {
                set_position: 320,
                set_shrink_start_child: false,
                set_resize_start_child: false,

                #[wrap(Some)]
                set_start_child = &gtk::ScrolledWindow {
                    set_hscrollbar_policy: gtk::PolicyType::Never,
                    set_width_request: 220,
                    set_child: Some(&model.list),
                },

                #[wrap(Some)]
                set_end_child = &gtk::Stack {
                    add_named[Some("empty")] = &adw::StatusPage {
                        set_icon_name: Some("preferences-system-symbolic"),
                        #[watch]
                        set_title: if model.rows.is_empty() { tr("Регионов пока нет", "No regions yet") } else { tr("Выберите регион", "Pick a region") },
                        set_description: Some(tr("Здесь меняется скорость региона. Изменения сразу сохраняются и применяются при экспорте.", "Speed and cutting of a region. Changes are saved right away and applied on export.")),
                    },

                    add_named[Some("edit")] = &gtk::ScrolledWindow {
                      set_hscrollbar_policy: gtk::PolicyType::Never,
                      #[wrap(Some)]
                      set_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 18,
                        set_margin_all: 24,

                        gtk::Box {
                            set_spacing: 10,
                            gtk::Label {
                                add_css_class: "title-3",
                                set_xalign: 0.0,
                                set_ellipsize: gtk::pango::EllipsizeMode::End,
                                #[watch]
                                set_label: &model.title(),
                            },
                            gtk::Label {
                                add_css_class: "dim-label",
                                set_xalign: 0.0,
                                set_hexpand: true,
                                set_ellipsize: gtk::pango::EllipsizeMode::Start,
                                #[watch]
                                set_label: &model.source_title(),
                            },
                            gtk::Button {
                                set_icon_name: "edit-undo-symbolic",
                                set_tooltip_text: Some(tr("Отменить (Ctrl+Z)", "Undo (Ctrl+Z)")),
                                add_css_class: "flat",
                                #[watch]
                                set_sensitive: model.history.can_undo(),
                                connect_clicked => ProcessingMsg::Undo,
                            },
                            gtk::Button {
                                set_icon_name: "edit-redo-symbolic",
                                set_tooltip_text: Some(tr("Повторить (Ctrl+Shift+Z)", "Redo (Ctrl+Shift+Z)")),
                                add_css_class: "flat",
                                #[watch]
                                set_sensitive: model.history.can_redo(),
                                connect_clicked => ProcessingMsg::Redo,
                            },
                        },

                        adw::PreferencesGroup {
                            set_title: tr("Скорость", "Speed"),

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 12,
                                set_margin_top: 6,

                                gtk::Box {
                                    set_spacing: 12,
                                    append: &model.speed_scale,
                                    gtk::Label {
                                        add_css_class: "title-4",
                                        add_css_class: "numeric",
                                        set_width_chars: 6,
                                        #[watch]
                                        set_label: &model.speed_label(),
                                    },
                                },

                                #[name = "presets"]
                                gtk::Box {
                                    set_spacing: 6,
                                    set_halign: gtk::Align::Start,
                                },

                                gtk::Box {
                                    set_spacing: 12,
                                    append: &model.keep_pitch,
                                    gtk::Label {
                                        set_label: tr("Сохранять тон", "Keep pitch"),
                                        set_xalign: 0.0,
                                    },
                                    gtk::Label {
                                        add_css_class: "dim-label",
                                        add_css_class: "caption",
                                        set_hexpand: true,
                                        set_xalign: 0.0,
                                        set_wrap: true,
                                        #[watch]
                                        set_label: model.mode_hint(),
                                    },
                                },
                            },
                        },

                        adw::PreferencesGroup {
                            set_title: tr("Нарезка и склейка", "Cut and splice"),
                            set_description: Some(tr("Лента результата: части подряд, по очереди сверху и снизу. Щелчок — курсор и выбор части; выбранную тяните за середину (наложение на предыдущую или пауза), за края (подрезка), за кружки сверху (фейды), Delete — убрать. Ctrl+колесо — масштаб, колесо или перетаскивание пустого места — прокрутка, двойной щелчок — всё.", "The result timeline: parts in a row, alternating top and bottom. Click — cursor and pick a part; drag the picked part by its middle (overlap the previous one or leave a pause), by its edges (trim), by the top circles (fades); Delete removes it. Ctrl+wheel — zoom, wheel or dragging empty space — scroll, double click — everything.")),

                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 8,
                                set_margin_top: 6,

                                append: &model.wave,

                                gtk::Box {
                                    set_spacing: 8,
                                    gtk::Button {
                                        set_label: tr("Разрезать в курсоре", "Cut at cursor"),
                                        set_icon_name: "edit-cut-symbolic",
                                        set_tooltip_text: Some(tr("Разрезать часть в позиции курсора", "Cut the part at the cursor")),
                                        connect_clicked => ProcessingMsg::Cut,
                                    },
                                    gtk::Button {
                                        set_label: tr("Сбросить", "Reset"),
                                        #[watch]
                                        set_sensitive: model.region().is_some_and(|r| r.is_edited()),
                                        set_tooltip_text: Some(tr("Снова весь регион, без фейдов", "The whole region again, no fades")),
                                        connect_clicked => ProcessingMsg::ResetParts,
                                    },
                                },

                                append: &model.parts_box,
                            },
                        },

                        gtk::Label {
                            add_css_class: "title-4",
                            add_css_class: "numeric",
                            set_xalign: 0.0,
                            #[watch]
                            set_label: &model.duration_label(),
                        },

                        gtk::Box {
                            set_spacing: 8,
                            gtk::Button {
                                #[watch]
                                set_label: if model.playing == Some(Playing::Original) { tr("■ Оригинал", "■ Original") } else { tr("▶ Оригинал", "▶ Original") },
                                #[watch]
                                set_sensitive: model.player.is_some(),
                                connect_clicked => ProcessingMsg::PlayOriginal,
                            },
                            gtk::Button {
                                add_css_class: "suggested-action",
                                #[watch]
                                set_label: if model.playing == Some(Playing::Result) { tr("■ Результат", "■ Result") } else { tr("▶ Результат", "▶ Result") },
                                #[watch]
                                set_sensitive: model.player.is_some() && !model.busy,
                                connect_clicked => ProcessingMsg::PlayResult,
                            },
                            gtk::Spinner {
                                #[watch]
                                set_spinning: model.busy,
                                #[watch]
                                set_visible: model.busy,
                            },
                        },
                      },
                    },

                    #[watch]
                    set_visible_child_name: if model.region().is_some() { "edit" } else { "empty" },
                },
            },
        }
    }

    fn init(
        (player, library): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let speed_scale = gtk::Scale::with_range(
            gtk::Orientation::Horizontal,
            f64::from(SPEED_MIN),
            f64::from(SPEED_MAX),
            1.0,
        );
        speed_scale.set_hexpand(true);
        speed_scale.set_draw_value(false);
        for (mark, label) in [(50, "×0.5"), (100, "×1"), (200, "×2"), (400, "×4")] {
            speed_scale.add_mark(f64::from(mark), gtk::PositionType::Bottom, Some(label));
        }

        let wave_state = Rc::new(RefCell::new(WaveState::default()));
        let wave = gtk::DrawingArea::builder()
            .height_request(110)
            .hexpand(true)
            .build();
        {
            let st = wave_state.clone();
            wave.set_draw_func(move |_, cr, w, h| draw_wave(&st.borrow(), cr, w, h));
            // Press: cursor and the part under the pointer; double click: all.
            let (st, s) = (wave_state.clone(), sender.clone());
            let click = gtk::GestureClick::new();
            click.connect_pressed(move |g, n_press, x, y| {
                let Some(widget) = g.widget() else { return };
                let size = (
                    f64::from(widget.width().max(1)),
                    f64::from(widget.height().max(1)),
                );
                let mut st = st.borrow_mut();
                if st.region.is_none() {
                    return;
                }
                if n_press == 2 {
                    st.view = None;
                    drop(st);
                    widget.queue_draw();
                    return;
                }
                let t = st.frame_at(x, size.0);
                st.selected = st.clip_at(t, y, size.1);
                drop(st);
                s.input(ProcessingMsg::Cursor(t));
            });
            wave.add_controller(click);
            // Drag the selected part: body slides it (overlap or pause), edges
            // trim, top circles fade; anywhere else scrolls the view.
            let (st, s) = (wave_state.clone(), sender.clone());
            let drag = gtk::GestureDrag::new();
            let st2 = st.clone();
            drag.connect_drag_begin(move |g, x, y| {
                let Some(widget) = g.widget() else { return };
                let size = (
                    f64::from(widget.width().max(1)),
                    f64::from(widget.height().max(1)),
                );
                let mut st = st2.borrow_mut();
                st.drag = st.grab(x, y, size);
            });
            let st2 = st.clone();
            drag.connect_drag_update(move |g, dx, _| {
                let Some(widget) = g.widget() else { return };
                let w = f64::from(widget.width().max(1));
                let mut st = st2.borrow_mut();
                st.drag_to(dx, w);
                drop(st);
                widget.queue_draw();
            });
            drag.connect_drag_end(move |_, _, _| {
                let mut st = st.borrow_mut();
                if let Some(d) = st.drag.take()
                    && let (Some(i), Some(part)) = (d.index, d.now)
                    && Some(part) != d.from
                {
                    s.input(ProcessingMsg::ReplacePart(i, part));
                }
            });
            wave.add_controller(drag);
            // Ctrl+wheel zooms around the pointer, the wheel scrolls.
            let pointer = Rc::new(std::cell::Cell::new(0.5));
            let motion = gtk::EventControllerMotion::new();
            let p = pointer.clone();
            motion.connect_motion(move |m, x, _| {
                let w = m.widget().map_or(1, |w| w.width()).max(1);
                p.set((x / f64::from(w)).clamp(0.0, 1.0));
            });
            wave.add_controller(motion);
            let st = wave_state.clone();
            let scroll =
                gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
            scroll.connect_scroll(move |c, dx, dy| {
                let mut st = st.borrow_mut();
                let ctrl = c
                    .current_event_state()
                    .contains(gtk::gdk::ModifierType::CONTROL_MASK);
                if ctrl {
                    st.zoom(if dy < 0.0 { 1.25 } else { 0.8 }, pointer.get());
                } else {
                    st.scroll((dx + dy) * 0.1);
                }
                drop(st);
                if let Some(w) = c.widget() {
                    w.queue_draw();
                }
                glib::Propagation::Stop
            });
            wave.add_controller(scroll);
            // Follow the result on the timeline (undoing the speed stretch).
            let (st, player) = (wave_state.clone(), player.clone());
            wave.add_tick_callback(move |w, _| {
                let mut st = st.borrow_mut();
                let speed = st.region.as_ref().map_or(100, |r| u64::from(r.speed));
                let pos = player
                    .as_ref()
                    .filter(|p| st.following && p.is_playing())
                    .map(|p| p.position() * speed / 100);
                if pos != st.playhead || st.peaks.as_ref().is_some_and(|p| !p.is_complete()) {
                    st.playhead = pos;
                    w.queue_draw();
                }
                glib::ControlFlow::Continue
            });
        }

        let model = Processing {
            player,
            library,
            rows: Vec::new(),
            selected: None,
            busy: false,
            playing: None,
            generation: 0,
            history: RegionHistory::default(),
            list: gtk::ListBox::builder()
                .selection_mode(gtk::SelectionMode::Single)
                .css_classes(["navigation-sidebar"])
                .build(),
            wave,
            wave_state,
            wave_peaks: None,
            parts_box: gtk::Box::new(gtk::Orientation::Vertical, 6),
            parts_sender: Some(sender.input_sender().clone()),
            speed_scale,
            keep_pitch: gtk::Switch::builder().valign(gtk::Align::Center).build(),
            updating: Rc::default(),
            toasts: root.clone(),
        };
        let widgets = view_output!();

        for preset in PRESETS {
            let b = gtk::Button::builder()
                .label(speed_text(preset))
                .css_classes(["pill"])
                .build();
            let s = sender.clone();
            b.connect_clicked(move |_| s.input(ProcessingMsg::Speed(preset)));
            widgets.presets.append(&b);
        }

        let (s, updating) = (sender.clone(), model.updating.clone());
        model.speed_scale.connect_value_changed(move |scale| {
            if !updating.get() {
                s.input(ProcessingMsg::Speed(scale.value().round() as u16));
            }
        });
        let (s, updating) = (sender.clone(), model.updating.clone());
        model.keep_pitch.connect_active_notify(move |sw| {
            if !updating.get() {
                s.input(ProcessingMsg::KeepPitch(sw.is_active()));
            }
        });
        let (s, updating) = (sender.clone(), model.updating.clone());
        model.list.connect_row_selected(move |_, row| {
            if !updating.get() {
                let id = row.and_then(|r| r.widget_name().parse().ok());
                s.input(ProcessingMsg::Select(id));
            }
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ProcessingMsg::Refresh => {
                match self.library.all_regions() {
                    Ok(rows) => self.rows = rows,
                    Err(err) => self.toast(&format!(
                        "{}: {err:#}",
                        tr("Не удалось прочитать регионы", "Could not read the regions")
                    )),
                }
                if self.region().is_none() {
                    self.selected = None;
                }
                self.rebuild_list();
                self.fill_controls();
            }
            ProcessingMsg::Leave => self.stop(),
            ProcessingMsg::DeleteSelectedPart => {
                let selected = self.wave_state.borrow().selected;
                match selected {
                    Some(i) => {
                        self.wave_state.borrow_mut().selected = None;
                        sender.input(ProcessingMsg::RemovePart(i));
                    }
                    None => self.toast(tr(
                        "Выберите часть на ленте щелчком",
                        "Click a part on the timeline first",
                    )),
                }
            }
            ProcessingMsg::Synced => {
                self.history = RegionHistory::default();
                sender.input(ProcessingMsg::Refresh);
            }
            ProcessingMsg::Select(id) => {
                self.stop();
                self.selected = id;
                self.fill_controls();
            }
            ProcessingMsg::Speed(speed) => {
                let speed = speed.clamp(SPEED_MIN, SPEED_MAX);
                self.edit(|r| r.speed = speed);
            }
            ProcessingMsg::KeepPitch(keep) => self.edit(|r| r.keep_pitch = keep),
            ProcessingMsg::Cursor(at) => {
                self.wave_state.borrow_mut().cursor = Some(at);
                self.wave.queue_draw();
            }
            ProcessingMsg::Cut => {
                let Some(at) = self.wave_state.borrow().cursor else {
                    self.toast(tr(
                        "Сначала щёлкните по ленте, где резать",
                        "Click on the timeline where to cut first",
                    ));
                    return;
                };
                let mut done = false;
                self.edit(|r| {
                    if let Some((i, src)) = output_to_source(&r.effective_parts(), at) {
                        done = r.split_part(i, src);
                    }
                });
                if !done {
                    self.toast(tr(
                        "Под курсором нет части или это её край",
                        "No part under the cursor, or it is an edge",
                    ));
                }
            }
            ProcessingMsg::ReplacePart(i, part) => self.edit(|r| {
                let mut parts = r.effective_parts();
                if let Some(p) = parts.get_mut(i) {
                    *p = part;
                    r.parts = parts;
                }
            }),
            ProcessingMsg::Overlap(i, ms) => {
                let rate = self.row().map_or(48_000, |(f, _)| i64::from(f.sample_rate));
                self.edit_with(
                    |r| {
                        r.set_overlap(i, i64::from(ms) * rate / 1000);
                    },
                    false,
                );
            }
            ProcessingMsg::ResetParts => self.edit(Region::reset_parts),
            ProcessingMsg::Undo | ProcessingMsg::Redo => {
                self.stop();
                let result = if matches!(msg, ProcessingMsg::Undo) {
                    self.history.undo(&*self.library)
                } else {
                    self.history.redo(&*self.library)
                };
                match result {
                    Ok(Some(region)) => {
                        self.generation += 1;
                        if let Some((_, r)) = self.rows.iter_mut().find(|(_, r)| r.id == region.id)
                        {
                            *r = region.clone();
                        }
                        self.selected = Some(region.id);
                        self.fill_controls();
                        self.rebuild_list_badges();
                    }
                    Ok(None) => {}
                    Err(err) => self.toast(&format!("{}: {err:#}", tr("Не удалось", "Failed"))),
                }
            }
            ProcessingMsg::RemovePart(i) => {
                if self.region().is_some_and(|r| r.effective_parts().len() < 2) {
                    self.toast(tr(
                        "Последнюю часть удалить нельзя",
                        "The last part cannot be removed",
                    ));
                    return;
                }
                self.edit(|r| {
                    r.remove_part(i);
                });
            }
            ProcessingMsg::MovePart(i, up) => self.edit(|r| {
                r.move_part(i, up);
            }),
            ProcessingMsg::Fades(i, fade_in, fade_out) => {
                let rate = self.row().map_or(48_000, |(f, _)| u64::from(f.sample_rate));
                let to_frames = |ms: u32| u64::from(ms) * rate / 1000;
                self.edit_with(
                    |r| {
                        r.set_fades(i, to_frames(fade_in), to_frames(fade_out));
                    },
                    false,
                );
            }
            ProcessingMsg::PlayOriginal => self.play_original(&sender),
            ProcessingMsg::PlayResult => self.play_result(&sender),
            ProcessingMsg::Rendered { generation, result } => {
                self.busy = false;
                if generation != self.generation {
                    return;
                }
                match result {
                    Ok((samples, channels, rate)) => {
                        if let Some(p) = &self.player {
                            p.load_memory(samples, channels, rate);
                            // From the cursor on the timeline, in rendered frames.
                            let speed = self.region().map_or(100, |r| u64::from(r.speed.max(1)));
                            let at = self.wave_state.borrow().cursor.unwrap_or(0) * 100 / speed;
                            p.set_range(at, None, false);
                            p.seek(at);
                            p.play();
                            self.wave_state.borrow_mut().following = true;
                            self.playing = Some(Playing::Result);
                            self.watch_end(&sender);
                        }
                    }
                    Err(err) => self.toast(&format!(
                        "{}: {err}",
                        tr("Не удалось обработать", "Could not process")
                    )),
                }
            }
            ProcessingMsg::Stopped => self.playing = None,
        }
    }
}

impl Processing {
    fn sender_for_parts(&self) -> relm4::Sender<ProcessingMsg> {
        self.parts_sender.clone().expect("set in init")
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn row(&self) -> Option<&(FileRow, Region)> {
        let id = self.selected?;
        self.rows.iter().find(|(_, r)| r.id == id)
    }

    fn region(&self) -> Option<&Region> {
        self.row().map(|(_, r)| r)
    }

    fn title(&self) -> String {
        match self.region() {
            Some(r) if !r.name.is_empty() => r.name.clone(),
            Some(_) => tr("без имени", "untitled").into(),
            None => String::new(),
        }
    }

    fn source_title(&self) -> String {
        self.row().map(|(f, _)| f.title()).unwrap_or_default()
    }

    fn speed_label(&self) -> String {
        self.region()
            .map(|r| speed_text(r.speed))
            .unwrap_or_default()
    }

    fn mode_hint(&self) -> &'static str {
        match self.region() {
            Some(r) if r.keep_pitch => tr(
                "меняется только темп; на сильных значениях возможен «эффект эха»",
                "only the tempo changes; strong values may sound a little echoey",
            ),
            Some(_) => tr(
                "как пластинка: быстрее — выше, медленнее — ниже",
                "like a record: faster is higher, slower is lower",
            ),
            None => "",
        }
    }

    fn duration_label(&self) -> String {
        let Some((file, r)) = self.row() else {
            return String::new();
        };
        let rate = f64::from(file.sample_rate.max(1));
        let before = r.len() as f64 / rate;
        let after = r.output_len() as f64 / rate;
        if r.is_processed() {
            format!("{:.2} {s} → {:.2} {s}", before, after, s = sec())
        } else {
            format!("{:.2} {}", before, sec())
        }
    }

    fn rebuild_list(&self) {
        self.updating.set(true);
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        for (file, r) in &self.rows {
            let row_box = gtk::Box::builder()
                .spacing(8)
                .margin_top(4)
                .margin_bottom(4)
                .build();
            row_box.append(&color_swatch(r.color, 12));
            let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
            text.set_hexpand(true);
            text.append(
                &gtk::Label::builder()
                    .label(if r.name.is_empty() {
                        tr("без имени", "untitled")
                    } else {
                        &r.name
                    })
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .build(),
            );
            let rate = f64::from(file.sample_rate.max(1));
            text.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{} · {}",
                        file.title(),
                        format_time(r.start as f64 / rate, false)
                    ))
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["dim-label", "caption"])
                    .build(),
            );
            row_box.append(&text);
            if r.is_processed() {
                let mut badge = Vec::new();
                if r.speed != SPEED_NORMAL {
                    badge.push(speed_text(r.speed));
                }
                if r.is_edited() {
                    badge.push("✂".into());
                }
                row_box.append(
                    &gtk::Label::builder()
                        .label(badge.join(" "))
                        .css_classes(["caption", "numeric", "accent"])
                        .build(),
                );
            }
            let row = gtk::ListBoxRow::builder().child(&row_box).build();
            // The row remembers its region; ids are unique across the library.
            row.set_widget_name(&r.id.to_string());
            self.list.append(&row);
            if Some(r.id) == self.selected {
                self.list.select_row(Some(&row));
            }
        }
        self.updating.set(false);
    }

    /// Puts the selected region's settings into the controls without echoing back.
    fn fill_controls(&mut self) {
        self.sync_wave();
        let Some(r) = self.region().cloned() else {
            return;
        };
        self.updating.set(true);
        self.speed_scale.set_value(f64::from(r.speed));
        self.keep_pitch.set_active(r.keep_pitch);
        self.updating.set(false);
        self.rebuild_parts();
    }

    /// Points the waveform at the selected region, starting its summary.
    fn sync_wave(&mut self) {
        let row = self.row().cloned();
        if let Some((file, _)) = &row
            && self
                .wave_peaks
                .as_ref()
                .is_none_or(|(id, _)| *id != file.id)
        {
            let job = peaks::start(&file.path, usize::from(file.channels), file.total_frames);
            self.wave_peaks = Some((file.id, job));
        }
        let mut st = self.wave_state.borrow_mut();
        let same = st.region.as_ref().map(|r| r.id) == row.as_ref().map(|(_, r)| r.id);
        if !same {
            st.cursor = None;
            st.view = None;
            st.selected = None;
        }
        st.region = row.as_ref().map(|(_, r)| r.clone());
        st.channels = row.as_ref().map_or(1, |(f, _)| usize::from(f.channels));
        st.peaks = self.wave_peaks.as_ref().map(|(_, j)| j.peaks.clone());
        drop(st);
        self.wave.queue_draw();
    }

    /// One row per part, in splice order: times, fades, reorder, remove.
    fn rebuild_parts(&self) {
        while let Some(child) = self.parts_box.first_child() {
            self.parts_box.remove(&child);
        }
        let Some((file, r)) = self.row() else { return };
        let rate = f64::from(file.sample_rate.max(1));
        let parts = r.effective_parts();
        if parts.len() == 1 && !r.is_edited() {
            return;
        }
        let count = parts.len();
        let starts = positions(&parts);
        for (i, p) in parts.iter().enumerate() {
            let row = gtk::Box::builder().spacing(8).build();
            row.append(
                &gtk::Label::builder()
                    .label(format!("{}", i + 1))
                    .css_classes(["title-4", "numeric"])
                    .width_chars(2)
                    .build(),
            );
            row.append(
                &gtk::Label::builder()
                    .label(format!(
                        "{} {}  ·  {:.2} {}",
                        tr("в", "at"),
                        format_time(starts[i] as f64 / rate, true),
                        p.len() as f64 / rate,
                        sec()
                    ))
                    .tooltip_text(format!(
                        "{}: {}–{}",
                        tr("из записи", "from the recording"),
                        format_time((p.start - r.start) as f64 / rate, true),
                        format_time((p.end - r.start) as f64 / rate, true),
                    ))
                    .css_classes(["numeric"])
                    .xalign(0.0)
                    .hexpand(true)
                    .build(),
            );
            let ms = |frames: u64| (frames as f64 * 1000.0 / rate).round();
            let spin = |value: f64| {
                let s = gtk::SpinButton::with_range(0.0, 5000.0, 10.0);
                s.set_value(value);
                s.set_width_chars(5);
                s
            };
            let fade_in = spin(ms(p.fade_in));
            let fade_out = spin(ms(p.fade_out));
            row.append(&gtk::Label::new(Some("fade in")));
            row.append(&fade_in);
            row.append(&gtk::Label::new(Some("out")));
            row.append(&fade_out);
            if i > 0 {
                // Over the previous part; negative is a pause before this one.
                let overlap = gtk::SpinButton::with_range(-5000.0, 5000.0, 10.0);
                overlap.set_value((p.overlap as f64 * 1000.0 / rate).round());
                overlap.set_width_chars(5);
                overlap.set_tooltip_text(Some(tr(
                    "Наложение на предыдущую часть, мс; меньше нуля — пауза",
                    "Overlap with the previous part, ms; below zero is a pause",
                )));
                row.append(&gtk::Label::new(Some(tr("наложение", "overlap"))));
                row.append(&overlap);
                let s = self.sender_for_parts();
                overlap.connect_value_changed(move |o| {
                    s.emit(ProcessingMsg::Overlap(i, o.value() as i32));
                });
            }
            row.append(
                &gtk::Label::builder()
                    .label(tr("мс", "ms"))
                    .css_classes(["dim-label"])
                    .build(),
            );
            for spin in [&fade_in, &fade_out] {
                let (fi, fo, s) = (fade_in.clone(), fade_out.clone(), self.sender_for_parts());
                spin.connect_value_changed(move |_| {
                    s.emit(ProcessingMsg::Fades(
                        i,
                        fi.value() as u32,
                        fo.value() as u32,
                    ));
                });
            }
            let button = |icon: &str, tip: &str, enabled: bool, msg: fn(usize) -> ProcessingMsg| {
                let b = gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(tip)
                    .css_classes(["flat"])
                    .sensitive(enabled)
                    .build();
                let s = self.sender_for_parts();
                b.connect_clicked(move |_| s.emit(msg(i)));
                b
            };
            row.append(&button(
                "go-up-symbolic",
                tr("Раньше", "Earlier"),
                i > 0,
                |i| ProcessingMsg::MovePart(i, true),
            ));
            row.append(&button(
                "go-down-symbolic",
                tr("Позже", "Later"),
                i + 1 < count,
                |i| ProcessingMsg::MovePart(i, false),
            ));
            row.append(&button(
                "user-trash-symbolic",
                tr("Убрать часть", "Remove part"),
                count > 1,
                ProcessingMsg::RemovePart,
            ));
            self.parts_box.append(&row);
        }
    }

    fn edit(&mut self, f: impl FnOnce(&mut Region)) {
        self.edit_with(f, true);
    }

    /// `rebuild` is false for fades: the spin buttons being turned must survive.
    fn edit_with(&mut self, f: impl FnOnce(&mut Region), rebuild: bool) {
        let Some(id) = self.selected else { return };
        let Some((_, region)) = self.rows.iter_mut().find(|(_, r)| r.id == id) else {
            return;
        };
        let before = region.clone();
        f(region);
        if *region == before {
            return;
        }
        let saved = self.history.apply(&*self.library, before, region.clone());
        self.generation += 1;
        if let Err(err) = saved {
            self.toast(&format!(
                "{}: {err:#}",
                tr("Не удалось сохранить", "Could not save")
            ));
        }
        // The old preview no longer matches the settings.
        if self.playing == Some(Playing::Result) {
            self.stop();
        }
        if rebuild {
            self.fill_controls();
        } else {
            self.sync_wave();
        }
        self.rebuild_list_badges();
    }

    /// Cheap refresh of the list after a speed change: rebuild keeps selection.
    fn rebuild_list_badges(&self) {
        let scroll = self
            .list
            .parent()
            .and_downcast::<gtk::Viewport>()
            .and_then(|v| v.parent())
            .and_downcast::<gtk::ScrolledWindow>()
            .map(|s| (s.vadjustment(), s.vadjustment().value()));
        self.rebuild_list();
        if let Some((adj, value)) = scroll {
            glib::idle_add_local_once(move || adj.set_value(value));
        }
    }

    fn play_original(&mut self, sender: &ComponentSender<Self>) {
        if self.playing == Some(Playing::Original) {
            self.stop();
            return;
        }
        let (Some(p), Some((file, r))) = (self.player.clone(), self.row().cloned()) else {
            return;
        };
        p.load(file.path.clone());
        p.set_range(r.start, Some(r.end), false);
        p.seek(r.start);
        p.play();
        self.playing = Some(Playing::Original);
        self.watch_end(sender);
    }

    fn play_result(&mut self, sender: &ComponentSender<Self>) {
        if self.playing == Some(Playing::Result) {
            self.stop();
            return;
        }
        self.stop();
        let Some((file, region)) = self.row().cloned() else {
            return;
        };
        if !file.path.exists() {
            self.toast(tr(
                "Нет доступа к записи. Подключите рекордер",
                "The recording is not available. Connect the recorder",
            ));
            return;
        }
        self.busy = true;
        let generation = self.generation;
        let input = sender.input_sender().clone();
        let path: PathBuf = file.path.clone();
        std::thread::spawn(move || {
            let result = export::render_to_memory(&path, &region)
                .map(|(info, samples)| (Arc::new(samples), info.channels, info.sample_rate))
                .map_err(|e| format!("{e:#}"));
            input.emit(ProcessingMsg::Rendered { generation, result });
        });
    }

    /// Flips the play buttons back when the sound has run out.
    fn watch_end(&self, sender: &ComponentSender<Self>) {
        let Some(player) = self.player.clone() else {
            return;
        };
        let s = sender.clone();
        let started = std::time::Instant::now();
        glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            if started.elapsed() > std::time::Duration::from_millis(300) && !player.is_playing() {
                s.input(ProcessingMsg::Stopped);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    fn stop(&mut self) {
        self.wave_state.borrow_mut().following = false;
        if self.playing.take().is_some()
            && let Some(p) = &self.player
        {
            p.pause();
        }
    }
}

#[derive(Default)]
struct WaveState {
    region: Option<Region>,
    channels: usize,
    peaks: Option<Arc<Peaks>>,
    /// Output-timeline frame where the next cut goes and the result plays from.
    cursor: Option<u64>,
    playhead: Option<u64>,
    /// The result is playing, so the playhead is shown.
    following: bool,
    /// Visible output-timeline frames; `None` shows all of it.
    view: Option<(f64, f64)>,
    selected: Option<usize>,
    drag: Option<Drag>,
    /// Width of the drawing area at the last draw, for the zoom limit.
    width: std::cell::Cell<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DragKind {
    Pan,
    Move,
    TrimStart,
    TrimEnd,
    FadeIn,
    FadeOut,
}

#[derive(Clone, Copy)]
struct Drag {
    kind: DragKind,
    index: Option<usize>,
    from: Option<Part>,
    now: Option<Part>,
    /// View when the drag began (for panning).
    view: (f64, f64),
}

/// Height of the strip at the top of a lane where the fade circles sit.
const FADE_STRIP: f64 = 22.0;
const GRAB: f64 = 10.0;

impl WaveState {
    /// Parts as shown, with a dragged one in its dragged state.
    fn parts(&self) -> Vec<Part> {
        let Some(r) = &self.region else {
            return Vec::new();
        };
        let mut parts = r.effective_parts();
        if let Some(Drag {
            index: Some(i),
            now: Some(p),
            ..
        }) = self.drag
            && let Some(slot) = parts.get_mut(i)
        {
            *slot = p;
        }
        parts
    }

    fn total(&self) -> f64 {
        spliced_len(&self.parts()).max(1) as f64
    }

    fn view(&self) -> (f64, f64) {
        self.view.unwrap_or((0.0, self.total()))
    }

    fn frame_at(&self, x: f64, w: f64) -> u64 {
        let (a, b) = self.view();
        (a + x / w * (b - a)).max(0.0) as u64
    }

    /// Lanes alternate, so `y` picks even or odd parts.
    fn clip_at(&self, t: u64, y: f64, h: f64) -> Option<usize> {
        let parts = self.parts();
        let pos = positions(&parts);
        let lane = usize::from(y >= h / 2.0);
        let hits: Vec<usize> = (0..parts.len())
            .rev()
            .filter(|&i| pos[i] <= t && t < pos[i] + parts[i].len())
            .collect();
        hits.iter()
            .copied()
            .find(|i| i % 2 == lane)
            .or(hits.first().copied())
    }

    /// What a press at (x, y) grabs: a handle or the body of the selected part,
    /// else the view.
    fn grab(&self, x: f64, y: f64, (w, h): (f64, f64)) -> Option<Drag> {
        let view = self.view();
        let pan = Drag {
            kind: DragKind::Pan,
            index: None,
            from: None,
            now: None,
            view,
        };
        let Some(i) = self.selected else {
            return Some(pan);
        };
        let parts = self.parts();
        let Some(&p) = parts.get(i) else {
            return Some(pan);
        };
        let pos = positions(&parts)[i];
        let (lane_top, lane_bottom) = lane(i, h);
        if y < lane_top - 4.0 || y > lane_bottom + 4.0 {
            return Some(pan);
        }
        let x_of = |f: u64| (f as f64 - view.0) / (view.1 - view.0) * w;
        let (fi, fo) = part_fades(&p);
        let near = |f: u64| (x_of(f) - x).abs() < GRAB;
        let top = y < lane_top + FADE_STRIP + 6.0;
        let kind = if top && near(pos + fi) {
            DragKind::FadeIn
        } else if top && near(pos + p.len() - fo) {
            DragKind::FadeOut
        } else if near(pos) {
            DragKind::TrimStart
        } else if near(pos + p.len()) {
            DragKind::TrimEnd
        } else if x > x_of(pos) && x < x_of(pos + p.len()) && i > 0 {
            DragKind::Move
        } else {
            return Some(pan);
        };
        Some(Drag {
            kind,
            index: Some(i),
            from: Some(p),
            now: Some(p),
            view,
        })
    }

    fn drag_to(&mut self, dx: f64, w: f64) {
        let Some(mut d) = self.drag else { return };
        let (Some(r), view) = (&self.region, d.view) else {
            return;
        };
        let frames = (dx / w * (view.1 - view.0)).round() as i64;
        let shift =
            |v: u64, lo: u64, hi: u64| (v as i64 + frames).clamp(lo as i64, hi as i64) as u64;
        if let Some(p) = d.from {
            let len = p.len();
            let (fi, fo) = part_fades(&p);
            d.now = Some(match d.kind {
                DragKind::Pan => p,
                // Left slides over the previous part, right leaves a pause.
                DragKind::Move => Part {
                    overlap: p.overlap - frames,
                    ..p
                },
                // The sound stays put: a later start also starts later.
                DragKind::TrimStart => {
                    let start = shift(p.start, r.start, p.end - 1);
                    Part {
                        start,
                        overlap: p.overlap - (start as i64 - p.start as i64),
                        ..p
                    }
                }
                DragKind::TrimEnd => Part {
                    end: shift(p.end, p.start + 1, r.end),
                    ..p
                },
                DragKind::FadeIn => Part {
                    fade_in: shift(fi, 0, len),
                    ..p
                },
                DragKind::FadeOut => Part {
                    fade_out: (fo as i64 - frames).clamp(0, len as i64) as u64,
                    ..p
                },
            });
        }
        if d.kind == DragKind::Pan {
            let span = view.1 - view.0;
            self.set_view(view.0 - dx / w * span, span);
        }
        self.drag = Some(d);
    }

    /// Zooms by `factor` (>1 closer) keeping `anchor` (0..1 of the width) in place.
    fn zoom(&mut self, factor: f64, anchor: f64) {
        let (a, b) = self.view();
        let at = a + (b - a) * anchor;
        let closest = peaks::MIN_FRAMES_PER_PX * self.width.get().max(1.0);
        let span = ((b - a) / factor).clamp(closest.min(self.total()), self.total() * 1.1);
        self.set_view(at - span * anchor, span);
    }

    fn scroll(&mut self, by: f64) {
        let (a, b) = self.view();
        self.set_view(a + (b - a) * by, b - a);
    }

    fn set_view(&mut self, start: f64, span: f64) {
        let max = (self.total() * 1.1 - span).max(0.0);
        let start = start.clamp(0.0, max);
        self.view = Some((start, start + span));
    }
}

/// Vertical extent of the lane for part `index`.
fn lane(index: usize, h: f64) -> (f64, f64) {
    let lane_h = h / 2.0;
    let top = (index % 2) as f64 * lane_h;
    (top + 2.0, top + lane_h - 2.0)
}

/// The output timeline: parts end to end on two alternating lanes so overlaps
/// show, each with its own waveform and fade ramps; the selected part has
/// handles for its edges and fades.
fn draw_wave(st: &WaveState, cr: &gtk::cairo::Context, w: i32, h: i32) {
    let Some(r) = &st.region else { return };
    let (w, h) = (f64::from(w), f64::from(h));
    st.width.set(w);
    let (view_start, view_end) = st.view();
    let span = (view_end - view_start).max(1.0);
    let x_of = |frame: u64| (frame as f64 - view_start) / span * w;
    let [cr_, cg, cb] = PALETTE[usize::from(r.color) % PALETTE.len()].1;
    let (red, green, blue) = (
        f64::from(cr_) / 255.0,
        f64::from(cg) / 255.0,
        f64::from(cb) / 255.0,
    );

    cr.set_source_rgba(0.5, 0.5, 0.5, 0.12);
    cr.rectangle(0.0, 0.0, w, h);
    let _ = cr.fill();
    cr.set_source_rgba(0.5, 0.5, 0.5, 0.3);
    cr.set_line_width(1.0);
    cr.move_to(0.0, h / 2.0);
    cr.line_to(w, h / 2.0);
    let _ = cr.stroke();

    let parts = st.parts();
    let pos = positions(&parts);
    for (i, p) in parts.iter().enumerate() {
        let (top, bottom) = lane(i, h);
        let (x0, x1) = (x_of(pos[i]), x_of(pos[i] + p.len()));
        if x1 < 0.0 || x0 > w {
            continue;
        }
        let selected = st.selected == Some(i);
        cr.set_source_rgba(red, green, blue, if selected { 0.42 } else { 0.24 });
        cr.rectangle(x0, top, x1 - x0, bottom - top);
        let _ = cr.fill();
        if selected {
            cr.set_source_rgba(red, green, blue, 1.0);
            cr.set_line_width(2.0);
            cr.rectangle(x0, top, x1 - x0, bottom - top);
            let _ = cr.stroke();
        }

        // The part's own waveform, visible columns only.
        if let Some(peaks) = &st.peaks {
            let reader = peaks.read();
            let mid = (top + bottom) / 2.0;
            let half = (bottom - top) / 2.0 - 2.0;
            let per_px = span / w;
            let first = x0.max(0.0);
            let last = x1.min(w);
            // Frame of the source under the clip's first visible pixel.
            let src = p.start as f64 + (view_start + first * per_px - pos[i] as f64);
            let points: Vec<(f64, f32, f32)> =
                envelope(&reader, 0..st.channels.max(1), src, per_px, last - first)
                    .into_iter()
                    .map(|(x, lo, hi)| (x + first, lo, hi))
                    .filter(|&(x, ..)| x <= last + 1.0)
                    .collect();
            cr.set_source_rgba(0.8, 0.8, 0.85, 0.9);
            fill_envelope(cr, &points, mid, half).ok();
        }

        let (fi, fo) = part_fades(p);
        cr.set_source_rgba(red, green, blue, 1.0);
        cr.set_line_width(if selected { 2.5 } else { 1.5 });
        if fi > 0 {
            cr.move_to(x0, bottom);
            cr.line_to(x_of(pos[i] + fi), top);
            let _ = cr.stroke();
        }
        if fo > 0 {
            cr.move_to(x_of(pos[i] + p.len() - fo), top);
            cr.line_to(x1, bottom);
            let _ = cr.stroke();
        }
        cr.move_to(x0.max(0.0) + 5.0, bottom - 6.0);
        cr.set_font_size(12.0);
        let _ = cr.show_text(&format!("{}", i + 1));

        if selected {
            for x in [x0, x1] {
                cr.rectangle(x - 4.0, (top + bottom) / 2.0 - 14.0, 8.0, 28.0);
            }
            let _ = cr.fill();
            for x in [x_of(pos[i] + fi), x_of(pos[i] + p.len() - fo)] {
                cr.arc(x, top + FADE_STRIP / 2.0, 6.0, 0.0, std::f64::consts::TAU);
                let _ = cr.fill();
            }
        }
    }

    if let Some(c) = st.cursor {
        cr.set_source_rgba(1.0, 1.0, 1.0, 0.9);
        cr.set_line_width(2.0);
        cr.move_to(x_of(c), 0.0);
        cr.line_to(x_of(c), h);
        let _ = cr.stroke();
    }
    if let Some(p) = st.playhead {
        cr.set_source_rgba(1.0, 0.85, 0.3, 1.0);
        cr.set_line_width(1.5);
        cr.move_to(x_of(p), 0.0);
        cr.line_to(x_of(p), h);
        let _ = cr.stroke();
    }
}

fn speed_text(speed: u16) -> String {
    if speed == SPEED_NORMAL {
        "×1".into()
    } else {
        format!("×{:.2}", f64::from(speed) / 100.0)
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::speed_text;

    #[test]
    fn speed_labels() {
        assert_eq!(speed_text(100), "×1");
        assert_eq!(speed_text(200), "×2");
        assert_eq!(speed_text(75), "×0.75");
        assert_eq!(speed_text(150), "×1.5");
    }
}
