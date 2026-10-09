use std::path::PathBuf;
use std::rc::Rc;

use relm4::adw::{self, prelude::*};
use relm4::gtk::{self, gdk, gio, glib};
use relm4::{
    Component, ComponentController, ComponentParts, ComponentSender, Controller, SimpleComponent,
};

use super::editor::{Editor, EditorMsg};
use super::file_tree::{FileTree, FileTreeInput, FileTreeOutput};
use crate::audio::player::Player;

pub struct App {
    window: adw::ApplicationWindow,
    file_tree: Controller<FileTree>,
    editor: Controller<Editor>,
    root: PathBuf,
}

#[derive(Debug)]
pub enum AppMsg {
    ChooseRoot,
    SetRoot(PathBuf),
    FileSelected(PathBuf),
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
            set_default_size: (1200, 760),

            adw::ToolbarView {
                add_top_bar = &adw::HeaderBar {
                    pack_start = &gtk::Button {
                        set_icon_name: "folder-open-symbolic",
                        set_tooltip_text: Some("Открыть папку"),
                        connect_clicked => AppMsg::ChooseRoot,
                    },

                    #[wrap(Some)]
                    set_title_widget = &adw::ViewSwitcher {
                        set_stack: Some(&view_stack),
                        set_policy: adw::ViewSwitcherPolicy::Wide,
                    },
                },

                #[name = "view_stack"]
                adw::ViewStack {
                    add_titled_with_icon[Some("markup"), "Разметка", "document-edit-symbolic"] = &gtk::Paned {
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

                    add_titled_with_icon[Some("review"), "Обзор регионов", "view-list-symbolic"] = &adw::StatusPage {
                        set_icon_name: Some("view-list-symbolic"),
                        set_title: "Регионов пока нет",
                        set_description: Some("Выделенные регионы из всех файлов появятся здесь"),
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

        let file_tree =
            FileTree::builder()
                .launch(root_dir.clone())
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
        let editor = Editor::builder().launch(player).detach();

        let model = App {
            window: window.clone(),
            file_tree,
            editor,
            root: root_dir,
        };
        let widgets = view_output!();

        window.add_controller(transport_keys(&window, model.editor.sender().clone()));
        if let Some(path) = open_file {
            model.editor.emit(EditorMsg::Open(path));
        }

        ComponentParts { model, widgets }
    }

    fn update(&mut self, msg: Self::Input, sender: ComponentSender<Self>) {
        match msg {
            AppMsg::ChooseRoot => {
                let dialog = gtk::FileDialog::builder()
                    .title("Папка с записями")
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
            }
            AppMsg::FileSelected(path) => self.editor.emit(EditorMsg::Open(path)),
        }
    }
}

fn dir_title(dir: &std::path::Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned())
}

/// Window-wide transport keys. Captured before the focused widget sees them, except
/// while typing into a text field.
fn transport_keys(
    window: &adw::ApplicationWindow,
    editor: relm4::Sender<EditorMsg>,
) -> gtk::EventControllerKey {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let window = window.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let typing =
            gtk::prelude::GtkWindowExt::focus(&window).is_some_and(|w| w.is::<gtk::Text>());
        if typing || mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let big = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let msg = match key {
            gdk::Key::space => EditorMsg::TogglePlay,
            gdk::Key::Home => EditorMsg::ToStart,
            gdk::Key::Left => EditorMsg::Nudge(if big { -10.0 } else { -1.0 }),
            gdk::Key::Right => EditorMsg::Nudge(if big { 10.0 } else { 1.0 }),
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => EditorMsg::ZoomIn,
            gdk::Key::minus | gdk::Key::KP_Subtract => EditorMsg::ZoomOut,
            gdk::Key::_0 | gdk::Key::KP_0 => EditorMsg::ZoomFit,
            _ => return glib::Propagation::Proceed,
        };
        editor.emit(msg);
        glib::Propagation::Stop
    });
    keys
}
