//! Right-hand pane: metadata, waveform, transport and the markup list for the
//! selected file. Owns the file's markup and undo history; every edit goes
//! through [`History`] and is in the library before the next event is handled.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use relm4::adw::{self, prelude::*};
use relm4::gtk;
use relm4::{ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent};

use super::context_menu;
use super::markup_list::{ListEvent, MarkupList};
use super::waveform::{WaveEvent, Waveform, format_time};
use crate::audio::decode::AudioInfo;
use crate::audio::detail::Detail;
use crate::audio::peaks::{self, PeakJob};
use crate::audio::player::{MONITOR_MAX_DB, MONITOR_MIN_DB, Player};
use crate::core::history::{Change, History};
use crate::core::{ChannelMode, ColorIdx, Id, Item, Marker, Markup, PALETTE, Region};
use crate::lang::{sec, tr};
use crate::recording;
use crate::store::{FileRecord, Library};

struct Loaded {
    path: PathBuf,
    info: AudioInfo,
    record: FileRecord,
    markup: Markup,
    _peaks: PeakJob,
}

pub struct Editor {
    player: Option<Rc<Player>>,
    /// Listening volume in dB (see [`Player::set_monitor_db`]).
    listening_db: f64,
    library: Rc<Library>,
    file: Option<Loaded>,
    error: Option<String>,
    /// Per file, so undo survives switching between takes within a session.
    histories: HashMap<Id, History>,

    selected: Option<Item>,
    /// Range picked with the mouse or I/O, not yet a region.
    selection: Option<(u64, u64)>,
    /// Playback loops over the selection, or else the selected region. On by
    /// default so a picked range never runs away while you listen; L toggles it.
    auto_loop: bool,
    /// Loop range last sent to the player, to avoid resending it on every message.
    applied_loop: std::cell::Cell<Option<(u64, u64)>>,
    /// Colour for the next new region or marker. Advances after each region so
    /// neighbouring regions differ; picking a colour with nothing selected sets it.
    color: ColorIdx,

    /// Frame under the last right click, for "marker here".
    context_frame: Rc<std::cell::Cell<u64>>,

    waveform: Waveform,
    list: MarkupList,
    toasts: adw::ToastOverlay,
    /// Take name: the alias if set, else the file name. Click to edit.
    title: gtk::EditableLabel,
    /// "?" button with the full list of keys.
    help: gtk::MenuButton,
}

#[derive(Debug)]
pub enum EditorMsg {
    Open(PathBuf),
    TogglePlay,
    ToStart,
    /// Relative seek in seconds.
    Nudge(f64),
    /// Detailed (true) or smooth waveform; a setting.
    WaveDetailed(bool),
    /// Listening volume in dB, for everything played (not exported).
    ListeningDb(f64),
    ZoomIn,
    ZoomOut,
    ZoomFit,

    Wave(WaveEvent),
    List(ListEvent),

    AddMarker,
    MarkerAt(u64),
    SetIn,
    SetOut,
    MakeRegion,
    RenameSelected,
    SetColor(ColorIdx),
    ToggleExport,
    ToggleLoop,
    DeleteSelected,
    /// Select the next (`true`) or previous item of `StepKind` and jump to it.
    Step(bool, StepKind),
    Escape,
    Undo,
    Redo,
    SetReviewed(bool),
    /// Alias typed into the title; empty or the file name clears it.
    SetAlias(String),
    ShowHelp,
    Pause,
    /// Back from the review page: reload the player and pick up markup edited there.
    Reattach,
    /// Open `path` (if needed) and select `item`.
    OpenAt(PathBuf, Item),
}

/// What `EditorMsg::Step` walks over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// Markers and regions together (`Tab`).
    Both,
    Marker,
    Region,
}

#[derive(Debug)]
pub enum EditorOutput {
    /// Markup or review state of this file changed (for the file tree).
    Changed(PathBuf),
}

#[relm4::component(pub)]
impl SimpleComponent for Editor {
    type Init = (Option<Rc<Player>>, Rc<Library>);
    type Input = EditorMsg;
    type Output = EditorOutput;

    view! {
        #[root]
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,

            add_named[Some("empty")] = &adw::StatusPage {
                set_icon_name: Some("audio-x-generic-symbolic"),
                set_title: tr("Файл не выбран", "No file selected"),
                set_description: Some(tr("Выберите запись слева", "Pick a recording on the left")),
            },

            add_named[Some("error")] = &adw::StatusPage {
                set_icon_name: Some("dialog-warning-symbolic"),
                set_title: tr("Не удалось открыть файл", "Cannot open the file"),
                #[watch]
                set_description: model.error.as_deref(),
            },

            add_named[Some("editor")] = &model.toasts.clone() -> adw::ToastOverlay {
                #[wrap(Some)]
                set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Paned {
                    set_vexpand: true,
                    set_orientation: gtk::Orientation::Vertical,
                    set_position: 420,
                    set_shrink_start_child: false,
                    set_shrink_end_child: false,

                    #[wrap(Some)]
                    set_start_child = &gtk::Box {
                        set_orientation: gtk::Orientation::Vertical,
                        set_spacing: 8,
                        set_margin_all: 12,

                        gtk::Box {
                            set_spacing: 12,
                            append: &model.title,
                            gtk::Label {
                                add_css_class: "dim-label",
                                set_xalign: 0.0,
                                set_hexpand: true,
                                set_ellipsize: gtk::pango::EllipsizeMode::End,
                                #[watch]
                                set_label: &model.file_details(),
                            },
                            gtk::ToggleButton {
                                set_label: tr("Просмотрен", "Reviewed"),
                                set_tooltip_text: Some(tr("Отметить запись как разобранную", "Mark the recording as reviewed")),
                                add_css_class: "flat",
                                #[watch]
                                #[block_signal(reviewed_toggled)]
                                set_active: model.file.as_ref().is_some_and(|f| f.record.reviewed),
                                connect_toggled[sender] => move |b| {
                                    sender.input(EditorMsg::SetReviewed(b.is_active()));
                                } @reviewed_toggled,
                            },
                        },

                        append: model.waveform.widget(),

                        gtk::Box {
                            set_spacing: 6,

                            gtk::Button {
                                set_icon_name: "media-skip-backward-symbolic",
                                set_tooltip_text: Some(tr("В начало (Home)", "To start (Home)")),
                                add_css_class: "flat",
                                connect_clicked => EditorMsg::ToStart,
                            },
                            #[name = "play_button"]
                            gtk::Button {
                                set_icon_name: "media-playback-start-symbolic",
                                set_tooltip_text: Some(tr("Воспроизвести / пауза (Space)", "Play / pause (Space)")),
                                add_css_class: "circular",
                                add_css_class: "suggested-action",
                                #[watch]
                                set_sensitive: model.player.is_some(),
                                connect_clicked => EditorMsg::TogglePlay,
                            },
                            #[name = "time_label"]
                            gtk::Label {
                                add_css_class: "numeric",
                                set_margin_start: 8,
                                set_xalign: 0.0,
                            },
                            #[name = "status_label"]
                            gtk::Label {
                                add_css_class: "dim-label",
                                add_css_class: "caption",
                                set_hexpand: true,
                                set_xalign: 0.0,
                                set_margin_start: 12,
                            },
                            gtk::Label {
                                add_css_class: "dim-label",
                                add_css_class: "caption",
                                #[watch]
                                set_label: &model.selection_label(),
                            },
                            gtk::MenuButton {
                                add_css_class: "flat",
                                set_tooltip_text: Some(tr("Громкость прослушивания", "Listening volume")),
                                #[wrap(Some)]
                                set_child = &gtk::Box {
                                    set_spacing: 4,
                                    gtk::Image {
                                        #[watch]
                                        set_icon_name: Some(if model.listening_db < 0.0 {
                                            "audio-volume-low-symbolic"
                                        } else {
                                            "audio-volume-high-symbolic"
                                        }),
                                    },
                                    gtk::Label {
                                        add_css_class: "numeric",
                                        #[watch]
                                        set_visible: model.listening_db != 0.0,
                                        #[watch]
                                        set_label: &listening_label(model.listening_db),
                                    },
                                },
                                #[wrap(Some)]
                                set_popover = &gtk::Popover {
                                    gtk::Box {
                                        set_orientation: gtk::Orientation::Vertical,
                                        set_spacing: 6,
                                        set_margin_all: 6,
                                        gtk::Label {
                                            set_label: tr("Громкость прослушивания", "Listening volume"),
                                            add_css_class: "heading",
                                            set_xalign: 0.0,
                                        },
                                        gtk::Scale::with_range(
                                            gtk::Orientation::Horizontal,
                                            f64::from(MONITOR_MIN_DB),
                                            f64::from(MONITOR_MAX_DB),
                                            1.0,
                                        ) {
                                            set_width_request: 260,
                                            set_draw_value: true,
                                            set_digits: 0,
                                            set_value: model.listening_db,
                                            add_mark: (0.0, gtk::PositionType::Bottom, Some("0")),
                                            connect_value_changed[sender] => move |scale| {
                                                sender.input(EditorMsg::ListeningDb(scale.value().round()));
                                            },
                                        },
                                        gtk::Label {
                                            set_label: tr(
                                                "Для всего, что играет; в экспорт не попадает.\nВыше 0 дБ громкие места перегружаются.",
                                                "For everything that plays; export is not affected.\nAbove 0 dB loud parts clip.",
                                            ),
                                            add_css_class: "dim-label",
                                            add_css_class: "caption",
                                            set_xalign: 0.0,
                                        },
                                    },
                                },
                            },
                            gtk::Button {
                                set_icon_name: "zoom-out-symbolic",
                                set_tooltip_text: Some(tr("Отдалить (−)", "Zoom out (−)")),
                                add_css_class: "flat",
                                connect_clicked => EditorMsg::ZoomOut,
                            },
                            gtk::Button {
                                set_icon_name: "zoom-fit-best-symbolic",
                                set_tooltip_text: Some(tr("Весь файл (0)", "Whole file (0)")),
                                add_css_class: "flat",
                                connect_clicked => EditorMsg::ZoomFit,
                            },
                            gtk::Button {
                                set_icon_name: "zoom-in-symbolic",
                                set_tooltip_text: Some(tr("Приблизить (+)", "Zoom in (+)")),
                                add_css_class: "flat",
                                connect_clicked => EditorMsg::ZoomIn,
                            },
                        },
                    },

                    #[wrap(Some)]
                    set_end_child = &gtk::Box {
                        set_margin_start: 12,
                        set_margin_end: 12,
                        set_margin_bottom: 6,
                        append: model.list.widget(),
                    },
                },

                // Always-visible cheat line; changes with what is selected.
                gtk::Box {
                    add_css_class: "toolbar",
                    set_spacing: 6,
                    set_margin_start: 6,
                    set_margin_end: 6,

                    gtk::Label {
                        set_hexpand: true,
                        set_xalign: 0.0,
                        set_wrap: true,
                        add_css_class: "caption",
                        #[watch]
                        set_markup: &model.hints(),
                    },
                    append: &model.help.clone(),
                },
                },
            },

            // After the pages exist, or GTK warns about an unknown child.
            #[watch]
            set_visible_child_name: if model.file.is_some() {
                "editor"
            } else if model.error.is_some() {
                "error"
            } else {
                "empty"
            },
        }
    }

    fn init(
        (player, library): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let waveform = Waveform::new();
        waveform.set_detailed(library.setting("waveform").as_deref() == Some("detailed"));
        let s = sender.input_sender().clone();
        waveform.connect_event(move |e| s.emit(EditorMsg::Wave(e)));
        let list = MarkupList::new();
        let s = sender.input_sender().clone();
        list.connect_event(move |e| s.emit(EditorMsg::List(e)));
        list.widget().set_hexpand(true);

        let listening_db = library
            .setting("listening_db")
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            .clamp(f64::from(MONITOR_MIN_DB), f64::from(MONITOR_MAX_DB));
        if let Some(p) = &player {
            p.set_monitor_db(listening_db as f32);
        }
        let model = Editor {
            player,
            listening_db,
            library,
            file: None,
            error: None,
            histories: HashMap::new(),
            selected: None,
            selection: None,
            auto_loop: true,
            applied_loop: Default::default(),
            color: 0,
            context_frame: Rc::default(),
            waveform,
            list,
            toasts: adw::ToastOverlay::new(),
            help: help_button(),
            title: gtk::EditableLabel::builder()
                .css_classes(["title-4"])
                .tooltip_text(tr(
                    "Нажмите, чтобы дать записи своё имя",
                    "Click to give the recording a name of your own",
                ))
                .build(),
        };
        let widgets = view_output!();
        model.start_ticking(&widgets);
        let input = sender.input_sender().clone();
        model.title.connect_editing_notify(move |label| {
            if !label.is_editing() {
                input.emit(EditorMsg::SetAlias(label.text().into()));
            }
        });
        root.insert_action_group("ed", Some(&actions(&sender, model.context_frame.clone())));

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        let edited = match msg {
            EditorMsg::Open(path) => {
                self.open(path);
                false
            }
            EditorMsg::TogglePlay => {
                if let Some(p) = self.player()
                    && self.file.is_some()
                {
                    p.toggle(self.loop_range().map_or(0, |(a, _)| a));
                    if !p.is_playing() {
                        self.remember_position();
                    }
                }
                false
            }
            EditorMsg::ToStart => {
                self.seek(0);
                false
            }
            EditorMsg::Nudge(secs) => {
                if let (Some(p), Some(f)) = (self.player(), &self.file) {
                    let delta = secs * f64::from(f.info.sample_rate);
                    let target = (p.position() as f64 + delta).max(0.0) as u64;
                    self.seek(target.min(f.info.total_frames));
                }
                false
            }
            EditorMsg::ZoomIn => {
                self.waveform.zoom(0.5);
                false
            }
            EditorMsg::ZoomOut => {
                self.waveform.zoom(2.0);
                false
            }
            EditorMsg::WaveDetailed(on) => {
                self.waveform.set_detailed(on);
                false
            }
            EditorMsg::ListeningDb(db) => {
                if db != self.listening_db {
                    self.listening_db = db;
                    if let Some(p) = self.player() {
                        p.set_monitor_db(db as f32);
                    }
                    self.library
                        .set_setting("listening_db", &db.to_string())
                        .ok();
                }
                false
            }
            EditorMsg::ZoomFit => {
                self.waveform.zoom_fit();
                false
            }
            EditorMsg::Wave(e) => self.on_wave(e, &sender),
            EditorMsg::List(e) => self.on_list(e),
            EditorMsg::AddMarker => self.add_marker(self.playhead()),
            EditorMsg::MarkerAt(frame) => self.add_marker(frame),
            EditorMsg::SetIn => self.set_edge(true),
            EditorMsg::SetOut => self.set_edge(false),
            EditorMsg::MakeRegion => self.make_region(),
            EditorMsg::RenameSelected => {
                if let Some(item) = self.selected {
                    self.list.focus_name(item);
                }
                false
            }
            EditorMsg::SetColor(color) => {
                self.color = color;
                match self.selected {
                    Some(item) => self.recolor(item, color),
                    None => false,
                }
            }
            EditorMsg::ToggleExport => match self.selected_region() {
                Some(r) => {
                    let export = !r.export;
                    self.edit_region(r.id, |r| r.export = export)
                }
                None => false,
            },
            EditorMsg::ToggleLoop => {
                self.toggle_loop();
                false
            }
            EditorMsg::DeleteSelected => match self.selected {
                Some(item) => self.delete(item),
                None => false,
            },
            EditorMsg::Step(forward, kind) => {
                self.step(forward, kind);
                false
            }
            EditorMsg::Escape => {
                self.set_selection(None);
                self.selected = None;
                self.sync_selection();
                false
            }
            EditorMsg::SetAlias(text) => {
                self.set_alias(text.trim(), &sender);
                false
            }
            EditorMsg::Pause => {
                // Leaving the page: the player goes to another one, so where
                // listening stopped is written down before it is lost.
                self.remember_position();
                if let Some(p) = self.player() {
                    p.pause();
                }
                false
            }
            EditorMsg::Reattach => {
                self.reattach();
                false
            }
            EditorMsg::OpenAt(path, item) => {
                self.open(path);
                self.select_and_show(item);
                false
            }
            EditorMsg::ShowHelp => {
                self.help.popup();
                false
            }
            EditorMsg::Undo | EditorMsg::Redo => self.undo_redo(matches!(msg, EditorMsg::Redo)),
            EditorMsg::SetReviewed(reviewed) => {
                if let Some(f) = &mut self.file
                    && f.record.reviewed != reviewed
                {
                    match self.library.set_reviewed(f.record.id, reviewed) {
                        Ok(()) => {
                            f.record.reviewed = reviewed;
                            sender.output(EditorOutput::Changed(f.path.clone())).ok();
                        }
                        Err(err) => self.toast(&format!(
                            "{}: {err:#}",
                            tr("Не удалось сохранить", "Could not save")
                        )),
                    }
                }
                false
            }
        };

        self.apply_loop();
        if edited {
            self.sync_markup();
            if let Some(f) = &self.file {
                sender.output(EditorOutput::Changed(f.path.clone())).ok();
            }
        }
    }
}

impl Editor {
    fn player(&self) -> Option<&Player> {
        self.player.as_deref()
    }

    fn markup(&self) -> Option<&Markup> {
        self.file.as_ref().map(|f| &f.markup)
    }

    fn playhead(&self) -> u64 {
        self.player().map_or(0, Player::position)
    }

    fn selected_region(&self) -> Option<&Region> {
        match self.selected? {
            Item::Region(id) => self.markup()?.region(id),
            Item::Marker(_) => None,
        }
    }

    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    /// Plays from the playhead and the clock / waveform follow on every frame.
    fn start_ticking(&self, widgets: &EditorWidgets) {
        let (waveform, player) = (self.waveform.clone(), self.player.clone());
        let (time_label, play_button, status_label) = (
            widgets.time_label.clone(),
            widgets.play_button.clone(),
            widgets.status_label.clone(),
        );
        let last = std::cell::RefCell::new((u64::MAX, false, String::new(), 0u64));
        widgets.time_label.add_tick_callback(move |_, _| {
            let Some(player) = &player else {
                return gtk::glib::ControlFlow::Continue;
            };
            let (pos, playing) = (player.position(), player.is_playing());
            waveform.update(pos, playing);

            let status = match waveform.analysis_progress() {
                Some(p) if p < 1.0 => format!(
                    "{} {:.0}%",
                    tr("анализ волны", "analysing the waveform"),
                    p * 100.0
                ),
                _ => String::new(),
            };
            let (rate, total) = waveform.timing();
            let mut last = last.borrow_mut();
            if (pos, playing, &status, total) != (last.0, last.1, &last.2, last.3) {
                time_label.set_label(&format!(
                    "{} / {}",
                    format_time(pos as f64 / rate, true),
                    format_time(total as f64 / rate, false)
                ));
                play_button.set_icon_name(if playing {
                    "media-playback-pause-symbolic"
                } else {
                    "media-playback-start-symbolic"
                });
                status_label.set_label(&status);
                *last = (pos, playing, status, total);
            }
            gtk::glib::ControlFlow::Continue
        });
    }

    fn open(&mut self, path: PathBuf) {
        if self.file.as_ref().is_some_and(|f| f.path == path) {
            return;
        }
        self.remember_position();
        self.file = None;
        self.selected = None;
        self.selection = None;
        self.applied_loop.set(None);
        if let Some(p) = self.player() {
            p.unload();
        }

        match self.load(&path) {
            Ok(loaded) => {
                self.error = None;
                let job = &loaded._peaks;
                let detail = Detail::start(
                    &loaded.path,
                    usize::from(loaded.info.channels),
                    loaded.info.total_frames,
                );
                self.waveform.set_file(
                    job.peaks.clone(),
                    std::sync::Arc::new(detail),
                    loaded.info.sample_rate,
                );
                if let Some(p) = self.player() {
                    p.load(path);
                }
                self.file = Some(loaded);
                self.resume();
                self.title.set_text(&self.display_name());
                self.sync_markup();
                self.waveform.focus();
            }
            Err(err) => self.error = Some(format!("{err:#}")),
        }
    }

    fn load(&mut self, path: &std::path::Path) -> anyhow::Result<Loaded> {
        let rec = recording::open(&self.library, path)?;
        let job = peaks::start(path, usize::from(rec.info.channels), rec.info.total_frames);
        Ok(Loaded {
            path: rec.path,
            info: rec.info,
            record: rec.record,
            markup: rec.markup,
            _peaks: job,
        })
    }

    fn reattach(&mut self) {
        let Some(f) = &mut self.file else { return };
        if let Ok(markup) = self.library.markup(f.record.id)
            && markup != f.markup
        {
            // Edited elsewhere: our undo steps no longer describe the stored state.
            f.markup = markup;
            self.histories.remove(&f.record.id);
        }
        if let Some(p) = self.player.as_deref() {
            p.load(f.path.clone());
        }
        self.applied_loop.set(None);
        let markup = &f.markup;
        let exists = match self.selected {
            Some(Item::Marker(id)) => markup.marker(id).is_some(),
            Some(Item::Region(id)) => markup.region(id).is_some(),
            None => true,
        };
        if !exists {
            self.selected = None;
        }
        // Back from another page: listening goes on where it stopped.
        self.resume();
        self.sync_markup();
    }

    fn select_and_show(&mut self, item: Item) {
        let pos = match item {
            Item::Marker(id) => self.markup().and_then(|m| m.marker(id)).map(|m| m.pos),
            Item::Region(id) => self.markup().and_then(|m| m.region(id)).map(|r| r.start),
        };
        let Some(pos) = pos else { return };
        self.set_selection(None);
        self.selected = Some(item);
        self.seek(pos);
        self.sync_markup();
    }

    /// Writes down where listening is right now, to pick it up later.
    fn remember_position(&self) {
        let (Some(f), Some(p)) = (&self.file, self.player()) else {
            return;
        };
        let pos = p.position();
        // At the very end it would only replay the last moment: start over.
        let pos = if pos + 1 >= f.info.total_frames {
            0
        } else {
            pos
        };
        self.library.set_position(f.record.id, pos).ok();
    }

    /// Puts the playhead back where listening stopped last time.
    fn resume(&self) {
        let Some(f) = &self.file else { return };
        let at = self.library.position(f.record.id).unwrap_or(0);
        if at > 0 && at < f.info.total_frames {
            self.seek(at);
        }
    }

    fn seek(&self, frame: u64) {
        if let Some(p) = self.player()
            && self.file.is_some()
        {
            p.seek(frame);
            self.waveform.reveal(frame);
        }
    }

    /// Applies one user action. Returns whether anything changed.
    fn commit(&mut self, changes: Vec<Change>) -> bool {
        let Some(f) = &mut self.file else {
            return false;
        };
        if changes.is_empty() {
            return false;
        }
        let history = self.histories.entry(f.record.id).or_default();
        match history.apply(&mut f.markup, &*self.library, changes) {
            Ok(applied) => {
                // Select what was just created.
                match applied.last() {
                    Some(Change::Marker {
                        before: None,
                        after: Some(m),
                    }) => self.selected = Some(Item::Marker(m.id)),
                    Some(Change::Region {
                        before: None,
                        after: Some(r),
                    }) => self.selected = Some(Item::Region(r.id)),
                    _ => {}
                }
                true
            }
            Err(err) => {
                self.toast(&format!(
                    "{}: {err:#}",
                    tr("Не удалось сохранить", "Could not save")
                ));
                false
            }
        }
    }

    fn edit_marker(&mut self, id: Id, f: impl FnOnce(&mut Marker)) -> bool {
        let Some(before) = self.markup().and_then(|m| m.marker(id)).cloned() else {
            return false;
        };
        let mut after = before.clone();
        f(&mut after);
        if after == before {
            return false;
        }
        self.commit(vec![Change::Marker {
            before: Some(before),
            after: Some(after),
        }])
    }

    fn edit_region(&mut self, id: Id, f: impl FnOnce(&mut Region)) -> bool {
        let Some(before) = self.markup().and_then(|m| m.region(id)).cloned() else {
            return false;
        };
        let mut after = before.clone();
        f(&mut after);
        if after == before {
            return false;
        }
        self.commit(vec![Change::Region {
            before: Some(before),
            after: Some(after),
        }])
    }

    fn on_wave(&mut self, e: WaveEvent, sender: &ComponentSender<Self>) -> bool {
        match e {
            WaveEvent::Seek(frame) => {
                if let Some(p) = self.player() {
                    p.seek(frame);
                }
                false
            }
            WaveEvent::Select(item) => {
                self.selected = item;
                self.sync_selection();
                false
            }
            WaveEvent::Selection(sel) => {
                self.set_selection(sel);
                false
            }
            WaveEvent::Trim(frame) => self.trim(frame),
            WaveEvent::Context { x, y, frame } => {
                self.show_context_menu(x, y, frame, sender);
                false
            }
            WaveEvent::MarkerMoved { id, pos } => self.edit_marker(id, |m| m.pos = pos),
            WaveEvent::RegionChanged { id, start, end } => self.edit_region(id, |r| {
                r.start = start;
                r.end = end;
            }),
            WaveEvent::Activate(item) => {
                self.selected = Some(item);
                self.sync_selection();
                self.list.focus_name(item);
                false
            }
        }
    }

    fn show_context_menu(&self, x: f64, y: f64, frame: u64, sender: &ComponentSender<Self>) {
        if self.file.is_none() {
            return;
        }
        self.context_frame.set(frame);
        let history = self
            .file
            .as_ref()
            .and_then(|f| self.histories.get(&f.record.id));
        let region = self.selected_region();
        let ctx = context_menu::Context {
            has_selection: self.selection.is_some_and(|(a, b)| b > a),
            marker_selected: matches!(self.selected, Some(Item::Marker(_))),
            region_selected: region.is_some(),
            region_exported: region.is_some_and(|r| r.export),
            auto_loop: self.auto_loop,
            can_undo: history.is_some_and(History::can_undo),
            can_redo: history.is_some_and(History::can_redo),
        };
        let input = sender.input_sender().clone();
        context_menu::show(self.waveform.canvas(), x, y, &ctx, move |color| {
            input.emit(EditorMsg::SetColor(color));
        });
    }

    fn on_list(&mut self, e: ListEvent) -> bool {
        match e {
            ListEvent::Select(item) => {
                self.selected = Some(item);
                let pos = match item {
                    Item::Marker(id) => self.markup().and_then(|m| m.marker(id)).map(|m| m.pos),
                    Item::Region(id) => self.markup().and_then(|m| m.region(id)).map(|r| r.start),
                };
                if let Some(pos) = pos {
                    self.seek(pos);
                }
                self.sync_selection();
                false
            }
            ListEvent::Rename(item, name) => match item {
                Item::Marker(id) => self.edit_marker(id, |m| m.name = name),
                Item::Region(id) => self.edit_region(id, |r| r.name = name),
            },
            ListEvent::Color(item, color) => {
                self.color = color;
                self.recolor(item, color)
            }
            ListEvent::Export(id, export) => self.edit_region(id, |r| r.export = export),
            ListEvent::Channels(id, mode) => self.edit_region(id, |r| r.channels = mode),
            ListEvent::Gain(id, gain) => self.edit_region(id, |r| r.fx.gain = gain),
            ListEvent::Delete(item) => self.delete(item),
            ListEvent::Done => {
                self.waveform.focus();
                false
            }
        }
    }

    fn recolor(&mut self, item: Item, color: ColorIdx) -> bool {
        match item {
            Item::Marker(id) => self.edit_marker(id, |m| m.color = color),
            Item::Region(id) => self.edit_region(id, |r| r.color = color),
        }
    }

    fn add_marker(&mut self, pos: u64) -> bool {
        let Some(f) = &self.file else { return false };
        let marker = Marker {
            id: 0,
            file: f.record.id,
            pos,
            name: String::new(),
            color: self.color,
            from_recorder: false,
        };
        self.commit(vec![Change::add_marker(marker)])
    }

    /// I / O: trims the selected region, or sets the edges of the temporary selection.
    fn set_edge(&mut self, start: bool) -> bool {
        let at = self.playhead();
        if let Some(r) = self.selected_region().cloned() {
            return self.edit_region(r.id, |r| {
                if start {
                    r.start = at.min(r.end.saturating_sub(1));
                } else {
                    r.end = at.max(r.start + 1);
                }
            });
        }
        let (a, b) = self.selection.unwrap_or((at, at));
        self.set_selection(Some(if start {
            (at, b.max(at))
        } else {
            (a.min(at), at)
        }));
        false
    }

    /// Shift+click at `frame`: the edge on that side of the centre moves there.
    /// Applies to the temporary selection, else to the selected region, else
    /// selects from the playhead to `frame`.
    fn trim(&mut self, frame: u64) -> bool {
        let pick = |a, b| crate::core::trim_range(a, b, frame);
        if let Some((a, b)) = self.selection.filter(|(a, b)| b > a) {
            self.set_selection(Some(pick(a, b)));
            return false;
        }
        if let Some(r) = self.selected_region().cloned() {
            let (start, end) = pick(r.start, r.end);
            return self.edit_region(r.id, |r| {
                r.start = start;
                r.end = end;
            });
        }
        let at = self.playhead();
        self.set_selection(Some((at.min(frame), at.max(frame))));
        false
    }

    fn set_selection(&mut self, selection: Option<(u64, u64)>) {
        self.selection = selection;
        self.waveform.set_selection(selection);
    }

    fn make_region(&mut self) -> bool {
        let Some(f) = &self.file else { return false };
        let Some((start, end)) = self.selection.filter(|(a, b)| b > a) else {
            self.toast(tr(
                "Сначала выделите участок мышью или клавишами I и O",
                "Select a stretch first: drag with the mouse or use I and O",
            ));
            return false;
        };
        // I and O pressed at almost the same moment make a useless sliver.
        let min_len = u64::from(f.info.sample_rate) / 100;
        if end - start < min_len {
            self.toast(tr(
                "Участок короче 10 мс — растяните выделение",
                "Shorter than 10 ms — make the selection longer",
            ));
            return false;
        }
        let region = Region {
            id: 0,
            file: f.record.id,
            start,
            end,
            name: String::new(),
            color: self.color,
            export: true,
            channels: ChannelMode::AsSource,
            speed: 100,
            keep_pitch: false,
            fx: Default::default(),
            parts: Vec::new(),
        };
        let created = self.commit(vec![Change::add_region(region)]);
        if created {
            self.color = (self.color + 1) % PALETTE.len() as ColorIdx;
            // The new region is selected, so the loop carries over to it.
            self.set_selection(None);
            // Name it right away: that is what the region is for.
            self.sync_markup();
            if let Some(item) = self.selected {
                self.list.focus_name(item);
            }
        }
        created
    }

    fn delete(&mut self, item: Item) -> bool {
        let change = match item {
            Item::Marker(id) => self
                .markup()
                .and_then(|m| m.marker(id))
                .map(|m| Change::Marker {
                    before: Some(m.clone()),
                    after: None,
                }),
            Item::Region(id) => self
                .markup()
                .and_then(|m| m.region(id))
                .map(|r| Change::Region {
                    before: Some(r.clone()),
                    after: None,
                }),
        };
        let Some(change) = change else { return false };
        if self.selected == Some(item) {
            self.selected = None;
        }
        self.commit(vec![change])
    }

    fn step(&mut self, forward: bool, kind: StepKind) {
        let Some(markup) = self.markup() else { return };
        let mut items: Vec<(u64, Item)> = Vec::new();
        if kind != StepKind::Region {
            items.extend(markup.markers.iter().map(|m| (m.pos, Item::Marker(m.id))));
        }
        if kind != StepKind::Marker {
            items.extend(markup.regions.iter().map(|r| (r.start, Item::Region(r.id))));
        }
        items.sort_by_key(|(pos, item)| (*pos, matches!(item, Item::Region(_))));
        if items.is_empty() {
            return;
        }

        let current = self
            .selected
            .and_then(|sel| items.iter().position(|(_, i)| *i == sel));
        let next = match (current, forward) {
            (Some(i), true) => (i + 1).min(items.len() - 1),
            (Some(i), false) => i.saturating_sub(1),
            (None, true) => {
                let at = self.playhead();
                items
                    .iter()
                    .position(|(p, _)| *p > at)
                    .unwrap_or(items.len() - 1)
            }
            (None, false) => {
                let at = self.playhead();
                items.iter().rposition(|(p, _)| *p < at).unwrap_or(0)
            }
        };
        let (pos, item) = items[next];
        self.selected = Some(item);
        self.seek(pos);
        self.sync_selection();
    }

    /// What playback is confined to right now, if anything.
    fn loop_range(&self) -> Option<(u64, u64)> {
        if !self.auto_loop {
            return None;
        }
        self.selection
            .filter(|(a, b)| b > a)
            .or_else(|| self.selected_region().map(|r| (r.start, r.end)))
    }

    /// Sends the loop range to the player when it changed. Called after every
    /// message, so selection, region edits, undo and deletes all stay in sync.
    fn apply_loop(&self) {
        // A selected region that was made louder is heard louder.
        if let Some(p) = self.player() {
            let gain = match self.selection {
                None => self.selected_region().map_or(1.0, |r| r.fx.gain_factor()),
                Some(_) => 1.0,
            };
            p.set_gain(gain);
        }
        let range = self.loop_range();
        if range == self.applied_loop.get() {
            return;
        }
        self.applied_loop.set(range);
        if let Some(p) = self.player() {
            match range {
                Some((a, b)) => p.set_range(a, Some(b), true),
                None => p.set_range(0, None, false),
            }
        }
        // Mark the region on the waveform only when it, not a selection, is looping.
        let looping_region = match (range, self.selection) {
            (Some(_), None) => self.selected_region().map(|r| r.id),
            _ => None,
        };
        self.waveform.set_looping(looping_region);
    }

    fn toggle_loop(&mut self) {
        self.auto_loop = !self.auto_loop;
        self.toast(if self.auto_loop {
            tr(
                "Цикл включён: выделение и выбранный регион повторяются",
                "Loop on: the selection and the selected region repeat",
            )
        } else {
            tr(
                "Цикл выключен: воспроизведение идёт дальше",
                "Loop off: playback goes on",
            )
        });
    }

    fn undo_redo(&mut self, redo: bool) -> bool {
        let Some(f) = &mut self.file else {
            return false;
        };
        let history = self.histories.entry(f.record.id).or_default();
        let result = if redo {
            history.redo(&mut f.markup, &*self.library)
        } else {
            history.undo(&mut f.markup, &*self.library)
        };
        match result {
            Ok(changed) => {
                // The selection may point at something that no longer exists.
                let markup = &f.markup;
                let exists = |item: &Item| match *item {
                    Item::Marker(id) => markup.marker(id).is_some(),
                    Item::Region(id) => markup.region(id).is_some(),
                };
                if !self.selected.as_ref().is_some_and(exists) {
                    self.selected = None;
                }
                changed
            }
            Err(err) => {
                self.toast(&format!(
                    "{}: {err:#}",
                    tr("Не удалось отменить", "Could not undo")
                ));
                false
            }
        }
    }

    /// Markup changed: redraw the waveform and rebuild the list.
    fn sync_markup(&self) {
        let Some(f) = &self.file else { return };
        self.waveform.set_markup(&f.markup, self.selected);
        self.list.set(
            &f.markup,
            self.selected,
            f.info.sample_rate,
            f.info.channels,
        );
    }

    /// Only the selection changed: cheaper, and keeps list entries alive while typing.
    fn sync_selection(&self) {
        let Some(f) = &self.file else { return };
        self.waveform.set_markup(&f.markup, self.selected);
        self.list.set_selected(self.selected);
    }

    fn set_alias(&mut self, text: &str, sender: &ComponentSender<Self>) {
        let file_name = self.file_name();
        let Some(f) = &mut self.file else { return };
        let alias = (!text.is_empty() && text != file_name).then(|| text.to_string());
        if alias != f.record.alias {
            match self.library.set_alias(f.record.id, alias.as_deref()) {
                Ok(()) => {
                    f.record.alias = alias;
                    sender.output(EditorOutput::Changed(f.path.clone())).ok();
                }
                Err(err) => self.toast(&format!(
                    "{}: {err:#}",
                    tr("Не удалось сохранить имя", "Could not save the name")
                )),
            }
        }
        // Clearing the alias shows the file name again.
        self.title.set_text(&self.display_name());
    }

    fn display_name(&self) -> String {
        self.file
            .as_ref()
            .and_then(|f| f.record.alias.clone())
            .unwrap_or_else(|| self.file_name())
    }

    fn file_name(&self) -> String {
        self.file
            .as_ref()
            .and_then(|f| f.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    fn file_details(&self) -> String {
        let Some(f) = &self.file else {
            return String::new();
        };
        let i = &f.info;
        let channels = match i.channels {
            1 => tr("моно", "mono").to_string(),
            2 => tr("стерео", "stereo").to_string(),
            n => format!("{n} {}", tr("кан.", "ch")),
        };
        let mut parts = Vec::new();
        if f.record.alias.is_some() {
            parts.push(self.file_name());
        }
        parts.extend([
            format!("{:.1} kHz", f64::from(i.sample_rate) / 1000.0).replace(".0 kHz", " kHz"),
            i.encoding.clone(),
            channels,
            format_time(i.duration_secs(), false),
        ]);
        if let Some(bext) = &i.bext {
            // TimeReference marks the first sample; the `time` field is when the file was closed.
            let start = bext.time_reference as f64 / f64::from(i.sample_rate);
            parts.push(format!(
                "{} {}",
                bext.origination_date,
                format_time(start % 86_400.0, false)
            ));
        }
        parts.join(" · ")
    }

    /// Pango markup for the hint line: the keys that make sense right now.
    fn hints(&self) -> String {
        if self.file.is_none() {
            return String::new();
        }
        let hints: &[(&str, &str)] = if self.selection.is_some_and(|(a, b)| b > a) {
            &[
                ("R", tr("регион из выделения", "region from selection")),
                (
                    tr("Shift+клик", "Shift+click"),
                    tr("подрезать край", "trim an edge"),
                ),
                ("I / O", tr("край по плейхеду", "edge at playhead")),
                ("1–8", tr("цвет нового региона", "colour of the new region")),
                ("Esc", tr("снять выделение", "clear selection")),
            ]
        } else if self.selected_region().is_some() {
            &[
                ("Enter", tr("имя", "name")),
                ("1–8", tr("цвет", "colour")),
                ("E", tr("экспорт", "export")),
                (
                    tr("Shift+клик", "Shift+click"),
                    tr("подрезать край", "trim an edge"),
                ),
                ("I / O", tr("край по плейхеду", "edge at playhead")),
                ("Del", tr("удалить", "delete")),
                ("Esc", tr("снять выбор", "deselect")),
            ]
        } else if matches!(self.selected, Some(Item::Marker(_))) {
            &[
                ("Enter", tr("имя", "name")),
                ("1–8", tr("цвет", "colour")),
                (tr("тащите", "drag"), tr("сдвинуть", "move")),
                ("Del", tr("удалить", "delete")),
                ("Esc", tr("снять выбор", "deselect")),
            ]
        } else {
            &[
                ("Space", tr("играть", "play")),
                (tr("тащите мышью", "drag"), tr("выделить", "select")),
                ("M", tr("маркер", "marker")),
                (
                    "I / O",
                    tr("выделение по плейхеду", "selection at playhead"),
                ),
                ("Tab", tr("следующий", "next")),
                (
                    tr("правая кнопка", "right click"),
                    tr("все действия", "all actions"),
                ),
            ]
        };
        let mut parts: Vec<String> = hints
            .iter()
            .map(|(key, what)| format!("{}  {what}", kbd(key)))
            .collect();
        if !self.auto_loop {
            parts.push(format!("{}  {}", kbd("C"), tr("цикл выключен", "loop off")));
        }
        parts.join("     ")
    }

    fn selection_label(&self) -> String {
        let (Some((a, b)), Some(f)) = (self.selection, &self.file) else {
            return String::new();
        };
        if b <= a {
            return String::new();
        }
        let rate = f64::from(f.info.sample_rate);
        format!(
            "{} {:.2} {} — {}",
            tr("выделено", "selected"),
            (b - a) as f64 / rate,
            sec(),
            tr("R: создать регион", "R: make a region")
        )
    }
}

/// `ed.*` actions used by the context menu. Each one just sends the message the
/// matching key would send.
fn actions(
    sender: &ComponentSender<Editor>,
    context_frame: Rc<std::cell::Cell<u64>>,
) -> gtk::gio::SimpleActionGroup {
    let group = gtk::gio::SimpleActionGroup::new();
    let add = |name: &str, msg: fn() -> EditorMsg| {
        let action = gtk::gio::SimpleAction::new(name, None);
        let input = sender.input_sender().clone();
        action.connect_activate(move |_, _| input.emit(msg()));
        group.add_action(&action);
    };
    add("play", || EditorMsg::TogglePlay);
    add("loop", || EditorMsg::ToggleLoop);
    add("region", || EditorMsg::MakeRegion);
    add("marker", || EditorMsg::AddMarker);
    add("set-in", || EditorMsg::SetIn);
    add("set-out", || EditorMsg::SetOut);
    add("rename", || EditorMsg::RenameSelected);
    add("export", || EditorMsg::ToggleExport);
    add("delete", || EditorMsg::DeleteSelected);
    add("undo", || EditorMsg::Undo);
    add("redo", || EditorMsg::Redo);
    add("zoom-fit", || EditorMsg::ZoomFit);

    let here = gtk::gio::SimpleAction::new("marker-here", None);
    let input = sender.input_sender().clone();
    here.connect_activate(move |_, _| input.emit(EditorMsg::MarkerAt(context_frame.get())));
    group.add_action(&here);
    group
}

/// A key cap in Pango markup.
fn kbd(key: &str) -> String {
    format!(
        "<span weight=\"bold\" bgcolor=\"#808080\" bgalpha=\"22%\"> {} </span>",
        gtk::glib::markup_escape_text(key)
    )
}

/// Every key, by section, in the interface language.
fn all_keys() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        (
            tr("Прослушивание", "Listening"),
            vec![
                ("Space", tr("воспроизвести / пауза", "play / pause")),
                ("Home", tr("в начало", "to start")),
                (
                    "← / →",
                    tr(
                        "назад / вперёд на 1 с (Shift — 10 с)",
                        "back / forward 1 s (Shift — 10 s)",
                    ),
                ),
                (
                    "C",
                    tr(
                        "зацикливать выделение и выбранный регион (вкл/выкл)",
                        "loop the selection and the selected region (on/off)",
                    ),
                ),
                (
                    tr("колесо", "wheel"),
                    tr(
                        "прокрутка; Ctrl+колесо или щипок на тачпаде — зум",
                        "scroll; Ctrl+wheel or a touchpad pinch — zoom",
                    ),
                ),
                (
                    "+ / − / 0",
                    tr(
                        "приблизить / отдалить / весь файл",
                        "zoom in / out / whole file",
                    ),
                ),
            ],
        ),
        (
            tr("Разметка", "Markup"),
            vec![
                (
                    tr("тащите мышью", "drag"),
                    tr("выделить участок", "select a stretch"),
                ),
                (
                    tr("Shift+клик", "Shift+click"),
                    tr(
                        "подрезать ближний к клику край выделения или региона",
                        "trim the edge of the selection or region nearest the click",
                    ),
                ),
                (
                    "I / O",
                    tr("начало / конец по плейхеду", "start / end at playhead"),
                ),
                ("R", tr("регион из выделения", "region from selection")),
                ("M", tr("маркер у плейхеда", "marker at playhead")),
                (
                    tr("правая кнопка", "right click"),
                    tr(
                        "меню: маркер здесь, цвет и всё остальное",
                        "menu: marker here, colour and the rest",
                    ),
                ),
            ],
        ),
        (
            tr("Выбранный маркер или регион", "Selected marker or region"),
            vec![
                (
                    tr("клик", "click"),
                    tr(
                        "выбрать; двойной клик — переименовать",
                        "select; double click — rename",
                    ),
                ),
                ("Enter / F2", tr("переименовать", "rename")),
                ("1–8", tr("цвет", "colour")),
                ("E", tr("экспорт вкл/выкл", "export on/off")),
                ("Del", tr("удалить", "delete")),
                (
                    "Tab / Shift+Tab",
                    tr("следующий / предыдущий", "next / previous"),
                ),
                (
                    "Esc",
                    tr(
                        "снять выделение и выбор",
                        "clear selection and selection of items",
                    ),
                ),
            ],
        ),
        (
            tr("Переходы", "Moving around"),
            vec![
                (
                    "J / K",
                    tr(
                        "следующая / предыдущая запись в папке",
                        "next / previous recording in the folder",
                    ),
                ),
                ("Ctrl+Tab / Ctrl+Shift+Tab", tr("то же самое", "the same")),
                (
                    "L / H",
                    tr(
                        "следующая / предыдущая папка (открывает первую запись)",
                        "next / previous folder (opens the first recording)",
                    ),
                ),
                (
                    "Ctrl+L / Ctrl+H",
                    tr("следующий / предыдущий регион", "next / previous region"),
                ),
                (
                    "Ctrl+J / Ctrl+K",
                    tr("следующий / предыдущий маркер", "next / previous marker"),
                ),
            ],
        ),
        (
            tr("Правка", "Edit"),
            vec![
                ("Ctrl+Z", tr("отменить", "undo")),
                ("Ctrl+Shift+Z / Ctrl+Y", tr("повторить", "redo")),
                ("? / F1", tr("эта памятка", "this sheet")),
            ],
        ),
    ]
}

fn help_button() -> gtk::MenuButton {
    let grid = gtk::Grid::builder()
        .row_spacing(4)
        .column_spacing(16)
        .margin_start(12)
        .margin_end(12)
        .margin_top(12)
        .margin_bottom(12)
        .build();
    let mut row = 0;
    for (section, keys) in all_keys() {
        let title = gtk::Label::builder()
            .label(section)
            .xalign(0.0)
            .css_classes(["heading"])
            .margin_top(if row == 0 { 0 } else { 8 })
            .build();
        grid.attach(&title, 0, row, 2, 1);
        row += 1;
        for (key, what) in keys {
            let cap = gtk::Label::builder().xalign(1.0).build();
            cap.set_markup(&kbd(key));
            grid.attach(&cap, 0, row, 1, 1);
            grid.attach(
                &gtk::Label::builder().label(what).xalign(0.0).build(),
                1,
                row,
                1,
                1,
            );
            row += 1;
        }
    }
    let popover = gtk::Popover::builder().child(&grid).build();
    gtk::MenuButton::builder()
        .icon_name("help-about-symbolic")
        .tooltip_text(tr("Все клавиши (? или F1)", "All keys (? or F1)"))
        .css_classes(["flat", "circular"])
        .popover(&popover)
        .build()
}

fn listening_label(db: f64) -> String {
    format!(
        "{}{db:.0} {}",
        if db > 0.0 { "+" } else { "" },
        tr("дБ", "dB")
    )
}
