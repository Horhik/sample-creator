//! "Обзор регионов": every region from every recording, grouped by recording,
//! with a mini waveform, preview, the export flag and a jump back to the source.
//! The bottom bar exports all flagged regions into a folder picked at that moment.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gio, glib};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use super::markup_list::color_swatch;
use super::waveform::{color_rgba, envelope, fill_envelope, format_time};
use crate::audio::peaks::{self, PeakJob};
use crate::audio::player::Player;
use crate::core::history::Store;
use crate::core::{ChannelMode, ColorIdx, Id, PALETTE, Region};
use crate::export::{self, OutputFormat, Source};
use crate::lang::{is_russian, sec, tr};
use crate::store::{FileRow, Library};

const MINI_W: i32 = 220;
const MINI_H: i32 = 34;

struct ExportRun {
    cancel: Arc<AtomicBool>,
    done: u64,
    total: u64,
}

pub struct Review {
    player: Option<Rc<Player>>,
    library: Rc<Library>,
    rows: Vec<(FileRow, Region)>,
    peaks: HashMap<Id, PeakJob>,

    color_filter: Option<ColorIdx>,
    only_export: bool,

    /// Region being previewed, and the file the player has loaded for it.
    previewing: Option<Id>,
    loaded: Option<PathBuf>,
    play_buttons: HashMap<Id, gtk::Button>,

    template: String,
    format: OutputFormat,
    export: Option<ExportRun>,

    groups: gtk::Box,
    toasts: adw::ToastOverlay,
}

#[derive(Debug)]
pub enum ReviewMsg {
    /// The page became visible: reload from the library.
    Refresh,
    /// The page was left: stop previewing.
    Leave,
    Preview(Id),
    PreviewEnded,
    SetExport(Id, bool),
    ColorFilter(Option<ColorIdx>),
    OnlyExport(bool),
    Template(String),
    Format(OutputFormat),
    ChooseFolder,
    ExportTo(PathBuf),
    Progress(u64),
    Finished {
        dir: PathBuf,
        written: usize,
        errors: Vec<String>,
    },
    Cancel,
}

#[derive(Debug)]
pub enum ReviewOutput {
    /// Show this region in the editor.
    Open(PathBuf, Id),
}

#[relm4::component(pub)]
impl SimpleComponent for Review {
    type Init = (Option<Rc<Player>>, Rc<Library>);
    type Input = ReviewMsg;
    type Output = ReviewOutput;

    view! {
        #[root]
        adw::ToastOverlay {
            #[wrap(Some)]
            set_child = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,

                gtk::Box {
                    add_css_class: "toolbar",
                    set_spacing: 6,
                    set_margin_start: 6,
                    set_margin_end: 6,

                    gtk::Label {
                        set_hexpand: true,
                        set_xalign: 0.0,
                        set_margin_start: 6,
                        #[watch]
                        set_label: &model.summary(),
                    },
                    append: &color_filter_bar(&sender),
                    gtk::ToggleButton {
                        set_label: tr("Только к экспорту", "Export only"),
                        add_css_class: "flat",
                        connect_toggled[sender] => move |b| {
                            sender.input(ReviewMsg::OnlyExport(b.is_active()));
                        },
                    },
                },

                gtk::Stack {
                    set_vexpand: true,
                    add_named[Some("empty")] = &adw::StatusPage {
                        set_icon_name: Some("view-list-symbolic"),
                        set_title: tr("Регионов пока нет", "No regions yet"),
                        set_description: Some(tr("Выделите участок на вкладке «Разметка» и нажмите R", "Select a stretch on the Markup page and press R")),
                    },
                    add_named[Some("list")] = &gtk::ScrolledWindow {
                        set_hscrollbar_policy: gtk::PolicyType::Never,
                        set_child: Some(&model.groups),
                    },
                    #[watch]
                    set_visible_child_name: if model.rows.is_empty() { "empty" } else { "list" },
                },

                gtk::Box {
                    add_css_class: "toolbar",
                    set_spacing: 8,
                    set_margin_start: 6,
                    set_margin_end: 6,

                    gtk::Label {
                        set_label: tr("Имя файла", "File name"),
                        set_margin_start: 6,
                    },
                    #[name = "template_entry"]
                    gtk::Entry {
                        set_width_chars: 22,
                        set_tooltip_text: Some(export::template_help()),
                        connect_changed[sender] => move |e| {
                            sender.input(ReviewMsg::Template(e.text().into()));
                        },
                    },
                    #[name = "format_dropdown"]
                    gtk::DropDown {
                        set_tooltip_text: Some(tr("Формат файлов. Каналы задаются у каждого региона", "File format. Channels are set per region")),
                    },
                    gtk::ProgressBar {
                        set_hexpand: true,
                        set_valign: gtk::Align::Center,
                        set_show_text: true,
                        #[watch]
                        set_visible: model.export.is_some(),
                        #[watch]
                        set_fraction: model.export.as_ref().map_or(0.0, |e| e.done as f64 / e.total.max(1) as f64),
                    },
                    gtk::Box {
                        set_hexpand: true,
                        #[watch]
                        set_visible: model.export.is_none(),
                    },
                    gtk::Button {
                        set_label: tr("Отмена", "Cancel"),
                        #[watch]
                        set_visible: model.export.is_some(),
                        connect_clicked => ReviewMsg::Cancel,
                    },
                    gtk::Button {
                        add_css_class: "suggested-action",
                        #[watch]
                        set_label: &format!("{} {}…", tr("Экспортировать", "Export"), model.to_export().count()),
                        #[watch]
                        set_sensitive: model.export.is_none() && model.to_export().count() > 0,
                        connect_clicked => ReviewMsg::ChooseFolder,
                    },
                },
            },
        }
    }

    fn init(
        (player, library): Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let template = library
            .setting("export.template")
            .unwrap_or_else(|| export::DEFAULT_TEMPLATE.into());
        let format = library
            .setting("export.format")
            .map_or(OutputFormat::AsSource, |k| OutputFormat::from_key(&k));
        let model = Review {
            player,
            library,
            rows: Vec::new(),
            peaks: HashMap::new(),
            color_filter: None,
            only_export: false,
            previewing: None,
            loaded: None,
            play_buttons: HashMap::new(),
            template: template.clone(),
            format,
            export: None,
            groups: gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(18)
                .margin_start(12)
                .margin_end(12)
                .margin_top(12)
                .margin_bottom(12)
                .build(),
            toasts: root.clone(),
        };
        let widgets = view_output!();

        widgets.template_entry.set_text(&template);
        let names: Vec<&str> = OutputFormat::ALL.iter().map(|f| f.label()).collect();
        widgets
            .format_dropdown
            .set_model(Some(&gtk::StringList::new(&names)));
        let pos = OutputFormat::ALL
            .iter()
            .position(|f| *f == format)
            .unwrap_or(0);
        widgets.format_dropdown.set_selected(pos as u32);
        let s = sender.clone();
        widgets.format_dropdown.connect_selected_notify(move |d| {
            if let Some(f) = OutputFormat::ALL.get(d.selected() as usize) {
                s.input(ReviewMsg::Format(*f));
            }
        });

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            ReviewMsg::Refresh => {
                // The editor may have loaded another file into the shared player.
                self.loaded = None;
                self.previewing = None;
                match self.library.all_regions() {
                    Ok(rows) => self.rows = rows,
                    Err(err) => self.toast(&format!(
                        "{}: {err:#}",
                        tr("Не удалось прочитать регионы", "Could not read the regions")
                    )),
                }
                self.start_peaks();
                self.rebuild(&sender);
            }
            ReviewMsg::Leave => self.stop_preview(),
            ReviewMsg::Preview(id) => self.toggle_preview(id, &sender),
            ReviewMsg::PreviewEnded => {
                self.previewing = None;
                self.update_play_icons();
            }
            ReviewMsg::SetExport(id, export) => self.set_export(id, export),
            ReviewMsg::ColorFilter(color) => {
                self.color_filter = color;
                self.rebuild(&sender);
            }
            ReviewMsg::OnlyExport(only) => {
                self.only_export = only;
                self.rebuild(&sender);
            }
            ReviewMsg::Template(t) => {
                self.template = t;
                self.library
                    .set_setting("export.template", &self.template)
                    .ok();
            }
            ReviewMsg::Format(f) => {
                self.format = f;
                self.library.set_setting("export.format", f.key()).ok();
            }
            ReviewMsg::ChooseFolder => self.choose_folder(&sender),
            ReviewMsg::ExportTo(dir) => self.start_export(dir, &sender),
            ReviewMsg::Progress(done) => {
                if let Some(run) = &mut self.export {
                    run.done = done;
                }
            }
            ReviewMsg::Finished {
                dir,
                written,
                errors,
            } => {
                self.export = None;
                self.finished(dir, written, errors);
            }
            ReviewMsg::Cancel => {
                if let Some(run) = &self.export {
                    run.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
    }
}

impl Review {
    fn toast(&self, text: &str) {
        self.toasts.add_toast(adw::Toast::new(text));
    }

    fn to_export(&self) -> impl Iterator<Item = &(FileRow, Region)> {
        self.rows.iter().filter(|(_, r)| r.export)
    }

    fn visible(&self) -> impl Iterator<Item = &(FileRow, Region)> {
        self.rows.iter().filter(|(_, r)| {
            self.color_filter.is_none_or(|c| r.color == c) && (!self.only_export || r.export)
        })
    }

    fn summary(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }
        let mut files: Vec<Id> = self.rows.iter().map(|(f, _)| f.id).collect();
        files.dedup();
        if is_russian() {
            format!(
                "Регионов: {} в {} записях · к экспорту {}",
                self.rows.len(),
                files.len(),
                self.to_export().count()
            )
        } else {
            format!(
                "{} regions in {} recordings · {} to export",
                self.rows.len(),
                files.len(),
                self.to_export().count()
            )
        }
    }

    /// Mini waveforms come from the peak cache, which exists for every file opened
    /// in the editor; others are analysed in the background.
    fn start_peaks(&mut self) {
        for (file, _) in &self.rows {
            self.peaks.entry(file.id).or_insert_with(|| {
                peaks::start(&file.path, usize::from(file.channels), file.total_frames)
            });
        }
    }

    fn rebuild(&mut self, sender: &ComponentSender<Self>) {
        while let Some(child) = self.groups.first_child() {
            self.groups.remove(&child);
        }
        self.play_buttons.clear();

        let visible: Vec<(FileRow, Region)> = self.visible().cloned().collect();
        let mut buttons = Vec::new();
        let mut i = 0;
        while i < visible.len() {
            let file = &visible[i].0;
            let end = visible[i..]
                .iter()
                .position(|(f, _)| f.id != file.id)
                .map_or(visible.len(), |n| i + n);
            self.groups
                .append(&self.group(file, &visible[i..end], sender, &mut buttons));
            i = end;
        }
        self.play_buttons.extend(buttons);
        if visible.is_empty() && !self.rows.is_empty() {
            self.groups.append(
                &gtk::Label::builder()
                    .label(tr(
                        "Под фильтр ничего не попадает",
                        "Nothing matches the filter",
                    ))
                    .css_classes(["dim-label"])
                    .margin_top(24)
                    .build(),
            );
        }
        self.update_play_icons();

        let any_running = self.peaks.values().any(|j| !j.peaks.is_complete());
        if any_running {
            // Redraw mini waveforms while their peaks are still being computed.
            let groups = self.groups.clone();
            let jobs: Vec<_> = self.peaks.values().map(|j| j.peaks.clone()).collect();
            glib::timeout_add_local(Duration::from_millis(300), move || {
                queue_draw_all(groups.upcast_ref());
                if jobs.iter().all(|p| p.is_complete()) {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        }
    }

    fn group(
        &self,
        file: &FileRow,
        regions: &[(FileRow, Region)],
        sender: &ComponentSender<Self>,
        buttons: &mut Vec<(Id, gtk::Button)>,
    ) -> gtk::Box {
        let group = gtk::Box::new(gtk::Orientation::Vertical, 6);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.append(
            &gtk::Label::builder()
                .label(file.title())
                .css_classes(["heading"])
                .xalign(0.0)
                .build(),
        );
        let missing = !file.path.exists();
        let path_label = if missing {
            format!(
                "{} — {}",
                file.path.display(),
                tr(
                    "нет доступа (рекордер отключён?)",
                    "not available (recorder unplugged?)"
                )
            )
        } else {
            file.path.display().to_string()
        };
        header.append(
            &gtk::Label::builder()
                .label(path_label)
                .css_classes(if missing {
                    vec!["error", "caption"]
                } else {
                    vec!["dim-label", "caption"]
                })
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk::pango::EllipsizeMode::Start)
                .build(),
        );
        group.append(&header);

        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        for (_, region) in regions {
            let (row, play) = self.row(file, region, sender);
            list.append(&row);
            buttons.push((region.id, play));
        }
        group.append(&list);
        group
    }

    /// The row and its play button (kept to flip its icon during preview).
    fn row(
        &self,
        file: &FileRow,
        r: &Region,
        sender: &ComponentSender<Self>,
    ) -> (gtk::ListBoxRow, gtk::Button) {
        let rate = f64::from(file.sample_rate.max(1));
        let row = gtk::Box::builder()
            .spacing(10)
            .margin_start(8)
            .margin_end(8)
            .margin_top(6)
            .margin_bottom(6)
            .build();

        row.append(&color_swatch(r.color, 14));

        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        let name = if r.name.is_empty() {
            tr("без имени", "untitled")
        } else {
            &r.name
        };
        text.append(
            &gtk::Label::builder()
                .label(name)
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(if r.name.is_empty() {
                    vec!["dim-label"]
                } else {
                    vec![]
                })
                .build(),
        );
        let channels = match (r.channels, file.channels) {
            (_, 1) | (ChannelMode::AsSource, _) => "",
            (ChannelMode::Left, _) => tr(" · только L", " · L only"),
            (ChannelMode::Right, _) => tr(" · только R", " · R only"),
            (ChannelMode::SplitMono, _) => tr(" · L и R отдельно", " · L and R apart"),
        };
        text.append(
            &gtk::Label::builder()
                .label(format!(
                    "{} – {} · {:.2} {}{speed}{channels}",
                    format_time(r.start as f64 / rate, true),
                    format_time(r.end as f64 / rate, true),
                    r.len() as f64 / rate,
                    sec(),
                    speed = speed_note(r, rate),
                ))
                .xalign(0.0)
                .css_classes(["dim-label", "caption", "numeric"])
                .build(),
        );
        row.append(&text);

        if let Some(job) = self.peaks.get(&file.id) {
            row.append(&mini_wave(job.peaks.clone(), r.start, r.end, r.color));
        }

        let play = gtk::Button::builder()
            .icon_name("media-playback-start-symbolic")
            .tooltip_text(tr("Прослушать", "Listen"))
            .css_classes(["circular", "flat"])
            .valign(gtk::Align::Center)
            .sensitive(self.player.is_some())
            .build();
        let (s, id) = (sender.clone(), r.id);
        play.connect_clicked(move |_| s.input(ReviewMsg::Preview(id)));
        row.append(&play.clone());

        let open = gtk::Button::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text(tr("Открыть в разметке", "Open in markup"))
            .css_classes(["circular", "flat"])
            .valign(gtk::Align::Center)
            .build();
        let (s, path, id) = (sender.clone(), file.path.clone(), r.id);
        open.connect_clicked(move |_| {
            s.output(ReviewOutput::Open(path.clone(), id)).ok();
        });
        row.append(&open);

        let export = gtk::CheckButton::builder()
            .active(r.export)
            .tooltip_text(tr("Экспортировать", "Export"))
            .valign(gtk::Align::Center)
            .build();
        let (s, id) = (sender.clone(), r.id);
        export.connect_toggled(move |b| s.input(ReviewMsg::SetExport(id, b.is_active())));
        row.append(&export);

        let row = gtk::ListBoxRow::builder()
            .child(&row)
            .activatable(false)
            .build();
        (row, play)
    }

    fn update_play_icons(&self) {
        for (id, button) in &self.play_buttons {
            let playing = self.previewing == Some(*id);
            button.set_icon_name(if playing {
                "media-playback-stop-symbolic"
            } else {
                "media-playback-start-symbolic"
            });
        }
    }

    fn toggle_preview(&mut self, id: Id, sender: &ComponentSender<Self>) {
        let Some(player) = self.player.clone() else {
            return;
        };
        if self.previewing == Some(id) {
            self.stop_preview();
            return;
        }
        let Some((file, region)) = self.rows.iter().find(|(_, r)| r.id == id).cloned() else {
            return;
        };
        if self.loaded.as_ref() != Some(&file.path) {
            player.load(file.path.clone());
            self.loaded = Some(file.path.clone());
        }
        player.set_range(region.start, Some(region.end), false);
        player.seek(region.start);
        player.play();
        self.previewing = Some(id);
        self.update_play_icons();

        // Flip the icon back when the region has played out.
        let s = sender.clone();
        let started = Instant::now();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            // Give the decoder a moment to queue audio before trusting is_playing.
            if started.elapsed() > Duration::from_millis(300) && !player.is_playing() {
                s.input(ReviewMsg::PreviewEnded);
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }

    fn stop_preview(&mut self) {
        if self.previewing.take().is_some()
            && let Some(p) = &self.player
        {
            p.pause();
        }
        self.update_play_icons();
    }

    fn set_export(&mut self, id: Id, export: bool) {
        let Some((_, region)) = self.rows.iter_mut().find(|(_, r)| r.id == id) else {
            return;
        };
        if region.export == export {
            return;
        }
        region.export = export;
        if let Err(err) = self.library.save_region(region) {
            self.toast(&format!(
                "{}: {err:#}",
                tr("Не удалось сохранить", "Could not save")
            ));
        }
    }

    fn choose_folder(&self, sender: &ComponentSender<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title(tr("Куда сохранить сэмплы", "Where to save the samples"))
            .accept_label(tr("Экспортировать сюда", "Export here"))
            .build();
        if let Some(dir) = self.library.setting("export.dir") {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let window = self.toasts.root().and_downcast::<gtk::Window>();
        let s = sender.clone();
        dialog.select_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
            if let Some(path) = result.ok().and_then(|f| f.path()) {
                s.input(ReviewMsg::ExportTo(path));
            }
        });
    }

    fn start_export(&mut self, dir: PathBuf, sender: &ComponentSender<Self>) {
        self.stop_preview();
        self.library
            .set_setting("export.dir", &dir.to_string_lossy())
            .ok();

        let regions: Vec<(Source, Region)> = self
            .to_export()
            .map(|(f, r)| {
                let source = Source {
                    path: f.path.clone(),
                    title: f.title(),
                    channels: f.channels,
                };
                (source, r.clone())
            })
            .collect();
        let missing = export::missing_sources(&regions);
        if !missing.is_empty() {
            self.toast(&format!(
                "{} ({}). {}",
                tr("Нет доступа к записям", "Recordings not available"),
                missing.len(),
                tr(
                    "Подключите рекордер и попробуйте снова",
                    "Connect the recorder and try again"
                )
            ));
            return;
        }

        let planned = export::plan(&regions, &self.template, &dir);
        let total = planned.iter().map(|p| p.frames()).sum();
        let cancel = Arc::new(AtomicBool::new(false));
        self.export = Some(ExportRun {
            cancel: cancel.clone(),
            done: 0,
            total,
        });

        let format = self.format;
        let input = sender.input_sender().clone();
        std::thread::Builder::new()
            .name("export".into())
            .spawn(move || {
                let mut last_report = Instant::now();
                let report = export::render_all(&planned, format, &cancel, &mut |done| {
                    if last_report.elapsed() > Duration::from_millis(100) {
                        input.emit(ReviewMsg::Progress(done));
                        last_report = Instant::now();
                    }
                });
                input.emit(ReviewMsg::Finished {
                    dir,
                    written: report.written,
                    errors: report.errors,
                });
            })
            .expect("spawn export thread");
    }

    fn finished(&self, dir: PathBuf, written: usize, errors: Vec<String>) {
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("export: {e}");
            }
            self.toast(&format!(
                "{}: {written}, {}: {}. {}: {}",
                tr("Сохранено файлов", "Files saved"),
                tr("ошибок", "errors"),
                errors.len(),
                tr("Первая", "First"),
                errors[0]
            ));
            return;
        }
        let toast = adw::Toast::builder()
            .title(format!(
                "{}: {written}",
                tr("Сохранено файлов", "Files saved")
            ))
            .button_label(tr("Открыть папку", "Open folder"))
            .timeout(8)
            .build();
        let window = self.toasts.root().and_downcast::<gtk::Window>();
        toast.connect_button_clicked(move |_| {
            let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(&dir)));
            launcher.launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
        });
        self.toasts.add_toast(toast);
    }
}

fn queue_draw_all(widget: &gtk::Widget) {
    if widget.is::<gtk::DrawingArea>() {
        widget.queue_draw();
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        queue_draw_all(&c);
        child = c.next_sibling();
    }
}

fn mini_wave(peaks: Arc<peaks::Peaks>, start: u64, end: u64, color: ColorIdx) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(MINI_W)
        .content_height(MINI_H)
        .valign(gtk::Align::Center)
        .build();
    let rgba = color_rgba(color);
    area.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        let mid = h / 2.0;
        let per_px = (end - start) as f64 / w.max(1.0);
        cr.set_source_rgba(
            f64::from(rgba.red()),
            f64::from(rgba.green()),
            f64::from(rgba.blue()),
            0.12,
        );
        cr.rectangle(0.0, 0.0, w, h);
        cr.fill().ok();
        cr.set_source_rgba(
            f64::from(rgba.red()),
            f64::from(rgba.green()),
            f64::from(rgba.blue()),
            0.9,
        );
        let reader = peaks.read();
        let points = envelope(&reader, 0..peaks.channels, start as f64, per_px, w);
        fill_envelope(cr, &points, mid, mid - 1.0).ok();
    });
    area
}

fn color_filter_bar(sender: &ComponentSender<Review>) -> gtk::Box {
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("linked");
    let all = gtk::ToggleButton::builder()
        .label(tr("Все", "All"))
        .active(true)
        .tooltip_text(tr("Все цвета", "All colours"))
        .build();
    let s = sender.clone();
    all.connect_toggled(move |b| {
        if b.is_active() {
            s.input(ReviewMsg::ColorFilter(None));
        }
    });
    bar.append(&all);
    for (i, (name, _)) in PALETTE.iter().enumerate() {
        let b = gtk::ToggleButton::builder()
            .child(&color_swatch(i as ColorIdx, 12))
            .tooltip_text(format!("{} {name}", tr("Только", "Only")))
            .group(&all)
            .build();
        let s = sender.clone();
        b.connect_toggled(move |b| {
            if b.is_active() {
                s.input(ReviewMsg::ColorFilter(Some(i as ColorIdx)));
            }
        });
        bar.append(&b);
    }
    bar
}

/// " → 16.50 с (×2, тон сохранён, склейка)" for processed regions.
fn speed_note(r: &Region, rate: f64) -> String {
    if !r.is_processed() {
        return String::new();
    }
    let mut notes = Vec::new();
    if r.speed != crate::core::SPEED_NORMAL {
        let factor = format!("×{:.2}", f64::from(r.speed) / 100.0);
        notes.push(
            factor
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned(),
        );
        if r.keep_pitch {
            notes.push(tr("тон сохранён", "pitch kept").into());
        }
    }
    if r.is_edited() {
        notes.push(tr("склейка", "spliced").into());
    }
    format!(
        " → {:.2} {} ({})",
        r.output_len() as f64 / rate,
        sec(),
        notes.join(", ")
    )
}
