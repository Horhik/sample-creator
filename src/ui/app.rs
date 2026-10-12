use std::path::PathBuf;
use std::rc::Rc;

use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gdk, gio, glib};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, RelmWidgetExt,
    SimpleComponent,
};

use super::editor::{Editor, EditorMsg, EditorOutput};
use super::file_tree::{FileTree, FileTreeInput, FileTreeOutput};
use super::processing::{Processing, ProcessingMsg};
use super::review::{Review, ReviewMsg, ReviewOutput};
use crate::audio::player::Player;
use crate::core::{Id, Item};
use crate::lang::{self, tr};
use crate::store::Library;
use crate::sync;

pub struct App {
    window: adw::ApplicationWindow,
    library: Rc<Library>,
    /// What the last sync of the root folder did, for the markup-file menu.
    sync_status: String,
    file_tree: Controller<FileTree>,
    editor: Controller<Editor>,
    review: Controller<Review>,
    processing: Controller<Processing>,
    root: PathBuf,
    view_stack: adw::ViewStack,
    /// Window-wide notifications, e.g. "takes effect after a restart".
    toasts: adw::ToastOverlay,
}

#[derive(Debug)]
pub enum AppMsg {
    ChooseRoot,
    SetRoot(PathBuf),
    FileSelected(PathBuf),
    FileChanged(PathBuf),
    /// From the review page: show this region in the editor.
    OpenRegion(PathBuf, Id),
    /// Markup file on the volume (the tree root): create it at the root, or in
    /// a folder the user picks.
    CreateStateFile {
        here: bool,
    },
    /// Point at an existing markup file (its folder).
    FindStateFile,
    StateFileFolder {
        location: PathBuf,
        must_exist: bool,
    },
    SyncNow,
    DisableSync,
    /// 0 = the system's language, 1 = English, 2 = Russian.
    Language(u32),
}

#[relm4::component(pub)]
impl SimpleComponent for App {
    /// Tree root and an optional file to open right away.
    type Init = (PathBuf, Option<PathBuf>);
    type Input = AppMsg;
    type Output = ();

    view! {
        #[root]
        adw::ApplicationWindow {
            set_title: Some("Sample Creator"),
            set_icon_name: Some(crate::APP_ID),
            set_default_size: (1200, 760),

            #[name = "toasts"]
            adw::ToastOverlay {
            #[wrap(Some)]
            set_child = &adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    pack_start = &gtk::Button {
                        set_icon_name: "folder-open-symbolic",
                        set_tooltip_text: Some(tr("Открыть папку", "Open folder")),
                        connect_clicked => AppMsg::ChooseRoot,
                    },

                    pack_end = &gtk::MenuButton {
                        set_icon_name: "open-menu-symbolic",
                        set_tooltip_text: Some(tr("Настройки", "Settings")),
                        #[wrap(Some)]
                        #[name = "settings_popover"]
                        set_popover = &gtk::Popover {
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 8,
                                set_margin_all: 8,

                                gtk::Label {
                                    set_label: tr("Язык интерфейса", "Interface language"),
                                    add_css_class: "heading",
                                    set_xalign: 0.0,
                                },
                                #[name = "language"]
                                gtk::DropDown::from_strings(&[
                                    tr("Как в системе", "System"),
                                    "English",
                                    "Русский",
                                ]) {
                                    set_selected: language_index(&model.library),
                                },
                                gtk::Label {
                                    set_label: tr(
                                        "Применится после перезапуска",
                                        "Takes effect after a restart",
                                    ),
                                    add_css_class: "dim-label",
                                    add_css_class: "caption",
                                    set_xalign: 0.0,
                                },
                            },
                        },
                    },

                    pack_end = &gtk::MenuButton {
                        set_icon_name: "emblem-synchronizing-symbolic",
                        set_tooltip_text: Some(tr("Разметка на носителе", "Markup on the volume")),
                        #[wrap(Some)]
                        set_popover = &gtk::Popover {
                            gtk::Box {
                                set_orientation: gtk::Orientation::Vertical,
                                set_spacing: 8,
                                set_margin_all: 8,
                                set_width_request: 320,

                                gtk::Label {
                                    set_label: tr("Файл разметки на носителе", "Markup file on the volume"),
                                    add_css_class: "heading",
                                    set_xalign: 0.0,
                                },
                                gtk::Label {
                                    #[watch]
                                    set_label: &model.sync_text(),
                                    set_wrap: true,
                                    set_xalign: 0.0,
                                    set_max_width_chars: 40,
                                    add_css_class: "dim-label",
                                },
                                gtk::Button {
                                    set_label: tr("Создать в корне папки", "Create in the folder root"),
                                    #[watch]
                                    set_visible: !model.sync_enabled(),
                                    connect_clicked => AppMsg::CreateStateFile { here: true },
                                },
                                gtk::Button {
                                    set_label: tr("Создать в папке…", "Create in a folder…"),
                                    #[watch]
                                    set_visible: !model.sync_enabled(),
                                    connect_clicked => AppMsg::CreateStateFile { here: false },
                                },
                                gtk::Button {
                                    set_label: tr("Указать существующий…", "Use an existing one…"),
                                    #[watch]
                                    set_visible: !model.sync_enabled(),
                                    connect_clicked => AppMsg::FindStateFile,
                                },
                                gtk::Button {
                                    set_label: tr("Синхронизировать сейчас", "Sync now"),
                                    add_css_class: "suggested-action",
                                    #[watch]
                                    set_visible: model.sync_enabled(),
                                    connect_clicked => AppMsg::SyncNow,
                                },
                                gtk::Button {
                                    set_label: tr("Отключить", "Turn off"),
                                    add_css_class: "flat",
                                    #[watch]
                                    set_visible: model.sync_enabled(),
                                    connect_clicked => AppMsg::DisableSync,
                                },
                            },
                        },
                    },

                    #[wrap(Some)]
                    set_title_widget = &adw::ViewSwitcher {
                        set_stack: Some(&view_stack),
                        set_policy: adw::ViewSwitcherPolicy::Wide,
                    },
                },

                #[name = "view_stack"]
                adw::ViewStack {
                    add_titled_with_icon[Some("markup"), tr("Разметка", "Markup"), "document-edit-symbolic"] = &gtk::Paned {
                        set_orientation: gtk::Orientation::Horizontal,
                        set_position: 280,
                        set_resize_start_child: false,
                        set_shrink_start_child: false,
                        set_shrink_end_child: false,

                        #[wrap(Some)]
                        set_start_child = &gtk::Box {
                            set_orientation: gtk::Orientation::Vertical,
                            set_width_request: 180,

                            gtk::Label {
                                #[watch]
                                set_label: &dir_title(&model.root),
                                #[watch]
                                set_tooltip_text: Some(&model.root.to_string_lossy()),
                                set_xalign: 0.0,
                                set_margin_top: 8,
                                set_margin_bottom: 4,
                                set_margin_start: 12,
                                set_margin_end: 12,
                                set_ellipsize: gtk::pango::EllipsizeMode::Start,
                                add_css_class: "heading",
                            },

                            append: model.file_tree.widget(),
                        },

                        set_end_child: Some(model.editor.widget()),
                    },

                    add_titled_with_icon[Some("review"), tr("Обзор регионов", "Region review"), "view-list-symbolic"] = model.review.widget(),
                    add_titled_with_icon[Some("processing"), tr("Обработка", "Processing"), "preferences-system-symbolic"] = model.processing.widget(),
                },
            },
            },
        }
    }

    fn init(
        (root_dir, open_file): Self::Init,
        window: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);
        // Installed builds find the icon in share/icons; `cargo run` in the tree.
        if let Some(display) = gdk::Display::default() {
            gtk::IconTheme::for_display(&display)
                .add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons"));
        }
        gtk::Window::set_default_icon_name(crate::APP_ID);

        let library = Rc::new(Library::open_default().unwrap_or_else(|err| {
            // Still usable for listening; markup just won't outlive the session.
            eprintln!("cannot open the markup library, keeping it in memory: {err:#}");
            Library::open_in_memory().expect("in-memory SQLite")
        }));
        // Before any widget is built: every label reads the language once.
        lang::set_russian(match library.setting("language").as_deref() {
            Some("ru") => true,
            Some("en") => false,
            _ => lang::system_is_russian(),
        });

        let file_tree = FileTree::builder()
            .launch((root_dir.clone(), library.clone()))
            .forward(sender.input_sender(), |out| match out {
                FileTreeOutput::FileSelected(path) => AppMsg::FileSelected(path),
            });

        let player = match Player::new() {
            Ok(p) => Some(Rc::new(p)),
            Err(err) => {
                eprintln!("audio output unavailable: {err:#}");
                None
            }
        };
        let processing = Processing::builder()
            .launch((player.clone(), library.clone()))
            .detach();
        let review = Review::builder()
            .launch((player.clone(), library.clone()))
            .forward(sender.input_sender(), |out| match out {
                ReviewOutput::Open(path, id) => AppMsg::OpenRegion(path, id),
            });
        let library_for_app = library.clone();
        let editor =
            Editor::builder()
                .launch((player, library))
                .forward(sender.input_sender(), |out| match out {
                    EditorOutput::Changed(path) => AppMsg::FileChanged(path),
                });

        let mut model = App {
            window: window.clone(),
            library: library_for_app,
            sync_status: String::new(),
            file_tree,
            editor,
            review,
            processing,
            root: root_dir,
            view_stack: adw::ViewStack::new(),
            toasts: adw::ToastOverlay::new(),
        };
        let widgets = view_output!();
        model.view_stack = widgets.view_stack.clone();
        model.toasts = widgets.toasts.clone();

        // The player is shared: whichever page becomes visible takes it over.
        let (editor_tx, review_tx, processing_tx) = (
            model.editor.sender().clone(),
            model.review.sender().clone(),
            model.processing.sender().clone(),
        );
        widgets
            .view_stack
            .connect_visible_child_name_notify(move |stack| {
                let page = stack.visible_child_name();
                let page = page.as_deref().unwrap_or("markup");
                // Everyone else lets go first, then the new page takes over.
                if page != "markup" {
                    editor_tx.emit(EditorMsg::Pause);
                }
                if page != "review" {
                    review_tx.emit(ReviewMsg::Leave);
                }
                if page != "processing" {
                    processing_tx.emit(ProcessingMsg::Leave);
                }
                match page {
                    "review" => review_tx.emit(ReviewMsg::Refresh),
                    "processing" => processing_tx.emit(ProcessingMsg::Refresh),
                    _ => editor_tx.emit(EditorMsg::Reattach),
                }
            });

        window.add_controller(editor_keys(
            &window,
            &widgets.view_stack,
            model.editor.sender().clone(),
            model.review.sender().clone(),
            model.processing.sender().clone(),
        ));
        let (s, popover) = (sender.clone(), widgets.settings_popover.clone());
        widgets.language.connect_selected_notify(move |d| {
            s.input(AppMsg::Language(d.selected()));
            // After the drop-down's own popup, GTK leaves this popover deaf to
            // clicks outside it; close it, the choice is made.
            popover.popdown();
        });
        // Closing the window writes the markup out to volumes that keep it.
        let lib = model.library.clone();
        window.connect_close_request(move |_| {
            for (root, r) in sync::sync_all(&lib) {
                if let Err(err) = r {
                    eprintln!("sync {}: {err:#}", root.display());
                }
            }
            glib::Propagation::Proceed
        });
        model.sync_root(&sender);
        if let Some(path) = open_file {
            model.file_tree.emit(FileTreeInput::Reveal(path.clone()));
            model.editor.emit(EditorMsg::Open(path));
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ChooseRoot => {
                let dialog = gtk::FileDialog::builder()
                    .title(tr("Папка с записями", "Folder with recordings"))
                    .initial_folder(&gio::File::for_path(&self.root))
                    .build();
                dialog.select_folder(Some(&self.window), gio::Cancellable::NONE, move |result| {
                    if let Some(path) = result.ok().and_then(|f| f.path()) {
                        sender.input(AppMsg::SetRoot(path));
                    }
                });
            }
            AppMsg::SetRoot(dir) => {
                self.root = dir.clone();
                self.file_tree.emit(FileTreeInput::SetRoot(dir));
                self.sync_root(&sender);
            }
            AppMsg::FileSelected(path) => {
                // The previous recording's edits go out, the volume's news come in.
                self.sync_root(&sender);
                self.editor.emit(EditorMsg::Open(path));
            }
            AppMsg::CreateStateFile { here: true } => {
                let location = self.root.clone();
                sender.input(AppMsg::StateFileFolder {
                    location,
                    must_exist: false,
                });
            }
            AppMsg::CreateStateFile { here: false } => {
                self.pick_folder(
                    tr("Где хранить файл разметки", "Where to keep the markup file"),
                    false,
                    &sender,
                );
            }
            AppMsg::FindStateFile => {
                self.pick_folder(
                    &format!("{} {}", tr("Папка с", "Folder with"), sync::FILE_NAME),
                    true,
                    &sender,
                );
            }
            AppMsg::StateFileFolder {
                location,
                must_exist,
            } => {
                let result = sync::enable(&self.library, &self.root, &location, must_exist);
                self.after_sync(result, &sender);
            }
            AppMsg::SyncNow => {
                let result = sync::sync_target(&self.library, &self.root);
                self.after_sync(result, &sender);
            }
            AppMsg::Language(i) => {
                let value = match i {
                    1 => "en",
                    2 => "ru",
                    _ => "system",
                };
                if let Err(err) = self.library.set_setting("language", value) {
                    eprintln!("language: {err:#}");
                    return;
                }
                // Said in the chosen language: the labels only change on restart.
                let russian = match value {
                    "ru" => true,
                    "en" => false,
                    _ => lang::system_is_russian(),
                };
                let text = if russian {
                    "Язык сменится после перезапуска"
                } else {
                    "Language changes after a restart"
                };
                self.toasts
                    .add_toast(adw::Toast::builder().title(text).timeout(4).build());
            }
            AppMsg::DisableSync => {
                if let Err(err) = sync::disable(&self.library, &self.root) {
                    self.sync_status = format!("{}: {err:#}", tr("Не удалось", "Failed"));
                } else {
                    self.sync_status = tr(
                        "Отключено; файл остался на носителе",
                        "Turned off; the file stays on the volume",
                    )
                    .into();
                }
            }
            AppMsg::FileChanged(path) => self.file_tree.emit(FileTreeInput::Refresh(path)),
            AppMsg::OpenRegion(path, id) => {
                self.file_tree.emit(FileTreeInput::Reveal(path.clone()));
                self.view_stack.set_visible_child_name("markup");
                self.editor.emit(EditorMsg::OpenAt(path, Item::Region(id)));
            }
        }
    }
}

impl App {
    fn sync_enabled(&self) -> bool {
        sync::target(&self.library, &self.root).is_some()
    }

    fn sync_text(&self) -> String {
        let state = match sync::target(&self.library, &self.root) {
            None => format!(
                "Разметка этой папки хранится только в приложении. Файл {} на \
                 носителе возит её вместе с записями на другое устройство; \
                 синхронизация сама при выборе папки, смене записи и закрытии \
                 окна. Побеждает более поздняя правка каждой записи.",
                sync::FILE_NAME
            ),
            Some(t) if !t.available() => format!(
                "{}\n{}",
                t.file().display(),
                tr("носитель не подключён", "volume not connected")
            ),
            Some(t) => format!("{}", t.file().display()),
        };
        if self.sync_status.is_empty() {
            state
        } else {
            format!("{state}\n\n{}", self.sync_status)
        }
    }

    fn pick_folder(&self, title: &str, must_exist: bool, sender: &ComponentSender<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .initial_folder(&gio::File::for_path(&self.root))
            .build();
        let sender = sender.clone();
        dialog.select_folder(Some(&self.window), gio::Cancellable::NONE, move |result| {
            if let Some(location) = result.ok().and_then(|f| f.path()) {
                sender.input(AppMsg::StateFileFolder {
                    location,
                    must_exist,
                });
            }
        });
    }

    /// Quiet sync of the tree root, if it keeps a markup file and is plugged in.
    fn sync_root(&mut self, sender: &ComponentSender<Self>) {
        if sync::target(&self.library, &self.root).is_some_and(|t| t.available()) {
            let result = sync::sync_target(&self.library, &self.root);
            self.after_sync(result, sender);
        }
    }

    fn after_sync(
        &mut self,
        result: anyhow::Result<sync::Report>,
        _sender: &ComponentSender<Self>,
    ) {
        match result {
            Ok(r) => {
                self.sync_status = format!(
                    "{} {}, {} {}",
                    tr(
                        "Синхронизировано: с носителя записей —",
                        "Synced: recordings from the volume —"
                    ),
                    r.imported,
                    tr("на носитель —", "to the volume —"),
                    r.exported
                );
                if r.imported > 0 {
                    // Regions were replaced: show what is stored now.
                    self.file_tree
                        .emit(FileTreeInput::SetRoot(self.root.clone()));
                    self.processing.emit(ProcessingMsg::Synced);
                    self.review.emit(ReviewMsg::Refresh);
                    if self.view_stack.visible_child_name().as_deref() == Some("markup") {
                        self.editor.emit(EditorMsg::Reattach);
                    }
                }
            }
            Err(err) => {
                eprintln!("sync: {err:#}");
                self.sync_status = format!("{}: {err:#}", tr("Не удалось", "Failed"));
            }
        }
    }
}

/// Position of the saved language choice in the settings drop-down.
fn language_index(library: &Library) -> u32 {
    match library.setting("language").as_deref() {
        Some("en") => 1,
        Some("ru") => 2,
        _ => 0,
    }
}

fn dir_title(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned())
}

/// Window-wide editor keys. Captured before the focused widget sees them, except
/// while typing into a text field.
fn editor_keys(
    window: &adw::ApplicationWindow,
    stack: &adw::ViewStack,
    editor: relm4::Sender<EditorMsg>,
    review: relm4::Sender<ReviewMsg>,
    processing: relm4::Sender<ProcessingMsg>,
) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let (window, stack) = (window.clone(), stack.clone());
    keys.connect_key_pressed(move |_, key, keycode, mods| {
        // Off the markup page the editor keys do not apply; Space stops a preview.
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let on_processing = stack.visible_child_name().as_deref() == Some("processing");
        // Delete / Backspace on the processing page drops the selected part.
        if on_processing
            && !ctrl
            && matches!(key, gdk::Key::Delete | gdk::Key::BackSpace)
            && !gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>())
        {
            processing.emit(ProcessingMsg::DeleteSelectedPart);
            return glib::Propagation::Stop;
        }
        if on_processing && ctrl {
            let typing =
                gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>());
            // Undo on the processing page; letters by physical key, as below.
            let display = gtk::prelude::WidgetExt::display(&window);
            let key = (0..4)
                .filter_map(|group| display.translate_key(keycode, mods, group))
                .map(|(k, ..)| k)
                .find(|k| k.to_unicode().is_some_and(|c| c.is_ascii_graphic()))
                .unwrap_or(key);
            match key.to_lower() {
                gdk::Key::z if !typing && shift => processing.emit(ProcessingMsg::Redo),
                gdk::Key::z if !typing => processing.emit(ProcessingMsg::Undo),
                gdk::Key::y if !typing => processing.emit(ProcessingMsg::Redo),
                _ => return glib::Propagation::Proceed,
            }
            return glib::Propagation::Stop;
        }
        match stack.visible_child_name().as_deref() {
            Some("review") | Some("processing") => {
                if key != gdk::Key::space {
                    return glib::Propagation::Proceed;
                }
                review.emit(ReviewMsg::Leave);
                processing.emit(ProcessingMsg::Leave);
                return glib::Propagation::Stop;
            }
            _ => {}
        }
        // Letters by physical key, so M/I/O/R work with a Cyrillic layout too:
        // take the first layout group where the key produces a Latin character.
        let display = gtk::prelude::WidgetExt::display(&window);
        let key = (0..4)
            .filter_map(|group| display.translate_key(keycode, mods, group))
            .map(|(k, ..)| k)
            .find(|k| k.to_unicode().is_some_and(|c| c.is_ascii_graphic()))
            .unwrap_or(key);
        let typing =
            gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>());
        if typing || mods.contains(gdk::ModifierType::ALT_MASK) {
            return glib::Propagation::Proceed;
        }
        let msg = match (ctrl, key.to_lower()) {
            (true, gdk::Key::z) if shift => EditorMsg::Redo,
            (true, gdk::Key::z) => EditorMsg::Undo,
            (true, gdk::Key::y) => EditorMsg::Redo,
            (true, _) => return glib::Propagation::Proceed,

            (_, gdk::Key::space) => EditorMsg::TogglePlay,
            (_, gdk::Key::Home) => EditorMsg::ToStart,
            (_, gdk::Key::Left) => EditorMsg::Nudge(if shift { -10.0 } else { -1.0 }),
            (_, gdk::Key::Right) => EditorMsg::Nudge(if shift { 10.0 } else { 1.0 }),
            (_, gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add) => EditorMsg::ZoomIn,
            (_, gdk::Key::minus | gdk::Key::KP_Subtract) => EditorMsg::ZoomOut,
            (_, gdk::Key::_0 | gdk::Key::KP_0) => EditorMsg::ZoomFit,

            (_, gdk::Key::m) => EditorMsg::AddMarker,
            (_, gdk::Key::i) => EditorMsg::SetIn,
            (_, gdk::Key::o) => EditorMsg::SetOut,
            (_, gdk::Key::r) => EditorMsg::MakeRegion,
            (_, gdk::Key::e) => EditorMsg::ToggleExport,
            (_, gdk::Key::l) => EditorMsg::ToggleLoop,
            (_, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::F2) => EditorMsg::RenameSelected,
            (_, gdk::Key::Delete | gdk::Key::BackSpace) => EditorMsg::DeleteSelected,
            (_, gdk::Key::Tab) => EditorMsg::Step(true),
            (_, gdk::Key::ISO_Left_Tab) => EditorMsg::Step(false),
            (_, gdk::Key::Escape) => EditorMsg::Escape,
            (_, gdk::Key::question | gdk::Key::F1) => EditorMsg::ShowHelp,
            (_, k) => match k.to_unicode().and_then(|c| c.to_digit(10)) {
                Some(d @ 1..=8) => EditorMsg::SetColor(d as u8 - 1),
                _ => return glib::Propagation::Proceed,
            },
        };
        editor.emit(msg);
        glib::Propagation::Stop
    });
    keys
}
