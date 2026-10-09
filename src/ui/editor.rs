//! Right-hand pane: file metadata, waveform and transport for the selected file.

use std::path::PathBuf;
use std::rc::Rc;

use relm4::adw::{self, prelude::*};
use relm4::gtk;
use relm4::{ComponentParts, ComponentSender, RelmWidgetExt, SimpleComponent};

use super::waveform::{Marker, Waveform, format_time};
use crate::audio::decode::{self, AudioInfo};
use crate::audio::peaks::{self, PeakJob};
use crate::audio::player::Player;

struct Loaded {
    path: PathBuf,
    info: AudioInfo,
    _peaks: PeakJob,
}

pub struct Editor {
    player: Option<Rc<Player>>,
    file: Option<Loaded>,
    error: Option<String>,
    waveform: Waveform,
}

#[derive(Debug)]
pub enum EditorMsg {
    Open(PathBuf),
    TogglePlay,
    ToStart,
    /// Relative seek in seconds.
    Nudge(f64),
    Seek(u64),
    ZoomIn,
    ZoomOut,
    ZoomFit,
}

#[relm4::component(pub)]
impl SimpleComponent for Editor {
    type Init = Option<Rc<Player>>;
    type Input = EditorMsg;
    type Output = ();

    view! {
        #[root]
        gtk::Stack {
            set_transition_type: gtk::StackTransitionType::Crossfade,

            add_named[Some("empty")] = &adw::StatusPage {
                set_icon_name: Some("audio-x-generic-symbolic"),
                set_title: "Файл не выбран",
                set_description: Some("Выберите запись слева"),
            },

            add_named[Some("error")] = &adw::StatusPage {
                set_icon_name: Some("dialog-warning-symbolic"),
                set_title: "Не удалось открыть файл",
                #[watch]
                set_description: model.error.as_deref(),
            },

            add_named[Some("editor")] = &gtk::Box {
                set_orientation: gtk::Orientation::Vertical,
                set_spacing: 8,
                set_margin_all: 12,

                gtk::Box {
                    set_spacing: 12,
                    gtk::Label {
                        add_css_class: "title-4",
                        set_xalign: 0.0,
                        set_ellipsize: gtk::pango::EllipsizeMode::Middle,
                        #[watch]
                        set_label: &model.file_name(),
                    },
                    gtk::Label {
                        add_css_class: "dim-label",
                        set_xalign: 0.0,
                        set_hexpand: true,
                        set_ellipsize: gtk::pango::EllipsizeMode::End,
                        #[watch]
                        set_label: &model.file_details(),
                    },
                },

                append: model.waveform.widget(),

                gtk::Box {
                    set_spacing: 6,

                    gtk::Button {
                        set_icon_name: "media-skip-backward-symbolic",
                        set_tooltip_text: Some("В начало (Home)"),
                        add_css_class: "flat",
                        connect_clicked => EditorMsg::ToStart,
                    },
                    #[name = "play_button"]
                    gtk::Button {
                        set_icon_name: "media-playback-start-symbolic",
                        set_tooltip_text: Some("Воспроизвести / пауза (Space)"),
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
                        #[watch]
                        set_label: if model.player.is_none() { "нет аудиоустройства" } else { "" },
                    },
                    gtk::Button {
                        set_icon_name: "zoom-out-symbolic",
                        set_tooltip_text: Some("Отдалить (−)"),
                        add_css_class: "flat",
                        connect_clicked => EditorMsg::ZoomOut,
                    },
                    gtk::Button {
                        set_icon_name: "zoom-fit-best-symbolic",
                        set_tooltip_text: Some("Весь файл (0)"),
                        add_css_class: "flat",
                        connect_clicked => EditorMsg::ZoomFit,
                    },
                    gtk::Button {
                        set_icon_name: "zoom-in-symbolic",
                        set_tooltip_text: Some("Приблизить (+)"),
                        add_css_class: "flat",
                        connect_clicked => EditorMsg::ZoomIn,
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
        player: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let waveform = Waveform::new();
        let seek_sender = sender.input_sender().clone();
        waveform.connect_seek(move |frame| seek_sender.emit(EditorMsg::Seek(frame)));

        let model = Editor {
            player,
            file: None,
            error: None,
            waveform,
        };
        let widgets = view_output!();

        // Playhead, time and analysis progress are polled every frame instead of
        // going through messages.
        let (waveform, player) = (model.waveform.clone(), model.player.clone());
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

            let progress = waveform.analysis_progress();
            let status = match progress {
                Some(p) if p < 1.0 => format!("анализ волны {:.0}%", p * 100.0),
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

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            EditorMsg::Open(path) => self.open(path),
            EditorMsg::TogglePlay => {
                if let Some(p) = &self.player
                    && self.file.is_some()
                {
                    p.toggle(0);
                }
            }
            EditorMsg::ToStart => self.seek(0),
            EditorMsg::Nudge(secs) => {
                if let (Some(p), Some(f)) = (&self.player, &self.file) {
                    let delta = secs * f64::from(f.info.sample_rate);
                    let target = (p.position() as f64 + delta).max(0.0) as u64;
                    self.seek(target.min(f.info.total_frames));
                }
            }
            EditorMsg::Seek(frame) => self.seek(frame),
            EditorMsg::ZoomIn => self.waveform.zoom(0.5),
            EditorMsg::ZoomOut => self.waveform.zoom(2.0),
            EditorMsg::ZoomFit => self.waveform.zoom_fit(),
        }
    }
}

impl Editor {
    fn open(&mut self, path: PathBuf) {
        if self.file.as_ref().is_some_and(|f| f.path == path) {
            return;
        }
        self.file = None;
        if let Some(p) = &self.player {
            p.unload();
        }

        let info = match decode::probe(&path) {
            Ok(info) => info,
            Err(err) => {
                self.error = Some(format!("{err:#}"));
                return;
            }
        };
        self.error = None;

        let job = peaks::start(&path, usize::from(info.channels), info.total_frames);
        let markers = info
            .cues
            .iter()
            .map(|c| Marker {
                frame: c.frame,
                label: c.label.clone().unwrap_or_else(|| format!("#{}", c.id)),
            })
            .collect();
        self.waveform
            .set_file(job.peaks.clone(), info.sample_rate, markers);
        if let Some(p) = &self.player {
            p.load(path.clone());
        }
        self.file = Some(Loaded {
            path,
            info,
            _peaks: job,
        });
    }

    fn seek(&self, frame: u64) {
        if let Some(p) = &self.player
            && self.file.is_some()
        {
            p.seek(frame);
            self.waveform.reveal(frame);
        }
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
            1 => "моно".to_string(),
            2 => "стерео".to_string(),
            n => format!("{n} кан."),
        };
        let mut parts = vec![
            format!("{:.1} kHz", f64::from(i.sample_rate) / 1000.0).replace(".0 kHz", " kHz"),
            i.encoding.clone(),
            channels,
            format_time(i.duration_secs(), false),
        ];
        if let Some(bext) = &i.bext {
            // TimeReference marks the first sample; the `time` field is when the file was closed.
            let start = bext.time_reference as f64 / f64::from(i.sample_rate);
            parts.push(format!(
                "{} {}",
                bext.origination_date,
                format_time(start % 86_400.0, false)
            ));
        }
        if !i.cues.is_empty() {
            parts.push(format!("меток: {}", i.cues.len()));
        }
        parts.join(" · ")
    }
}
