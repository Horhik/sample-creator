//! Sidebar with a lazily loaded directory tree that only shows folders and audio files.
//!
//! Built on GIO's `DirectoryList`, so folders are read asynchronously on expand and
//! the tree follows changes on disk (e.g. a recorder being plugged in). Nothing is
//! ever written to the browsed directories.

use std::path::{Path, PathBuf};

use relm4::gtk::{self, gio, prelude::*};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use crate::audio;

const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
                          standard::is-hidden,standard::symbolic-icon";

pub struct FileTree {
    root: PathBuf,
}

#[derive(Debug)]
pub enum FileTreeInput {
    SetRoot(PathBuf),
}

#[derive(Debug)]
pub enum FileTreeOutput {
    FileSelected(PathBuf),
}

pub struct FileTreeWidgets {
    list_view: gtk::ListView,
    shown_root: PathBuf,
}

impl SimpleComponent for FileTree {
    type Init = PathBuf;
    type Input = FileTreeInput;
    type Output = FileTreeOutput;
    type Widgets = FileTreeWidgets;
    type Root = gtk::ScrolledWindow;

    fn init_root() -> Self::Root {
        gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build()
    }

    fn init(
        root_dir: Self::Init,
        scrolled: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let list_view = gtk::ListView::builder()
            .factory(&row_factory())
            .css_classes(["navigation-sidebar"])
            .build();
        scrolled.set_child(Some(&list_view));

        // Enter / double click on a folder toggles it.
        list_view.connect_activate(|view, position| {
            let row = view
                .model()
                .and_then(|m| m.item(position))
                .and_downcast::<gtk::TreeListRow>();
            if let Some(row) = row
                && row.is_expandable()
            {
                row.set_expanded(!row.is_expanded());
            }
        });

        set_tree_root(&list_view, &root_dir, &sender);
        let widgets = FileTreeWidgets {
            list_view,
            shown_root: root_dir.clone(),
        };

        ComponentParts {
            model: FileTree { root: root_dir },
            widgets,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            FileTreeInput::SetRoot(dir) => self.root = dir,
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, sender: ComponentSender<Self>) {
        if widgets.shown_root != self.root {
            set_tree_root(&widgets.list_view, &self.root, &sender);
            widgets.shown_root = self.root.clone();
        }
    }
}

fn set_tree_root(list_view: &gtk::ListView, dir: &Path, sender: &ComponentSender<FileTree>) {
    let tree = gtk::TreeListModel::new(
        directory_model(&gio::File::for_path(dir)),
        false,
        false,
        |item| {
            let info = item.downcast_ref::<gio::FileInfo>()?;
            if info.file_type() != gio::FileType::Directory {
                return None;
            }
            Some(directory_model(&file_of(info)?))
        },
    );

    let selection = gtk::SingleSelection::builder()
        .model(&tree)
        .autoselect(false)
        .can_unselect(true)
        .build();

    let sender = sender.clone();
    selection.connect_selected_item_notify(move |selection| {
        let info = selection
            .selected_item()
            .and_downcast::<gtk::TreeListRow>()
            .and_then(|row| row.item())
            .and_downcast::<gio::FileInfo>();
        if let Some(info) = info
            && info.file_type() != gio::FileType::Directory
            && let Some(path) = file_of(&info).and_then(|f| f.path())
        {
            sender.output(FileTreeOutput::FileSelected(path)).ok();
        }
    });

    list_view.set_model(Some(&selection));
}

/// Children of `dir`: visible folders and supported audio files, folders first.
fn directory_model(dir: &gio::File) -> gio::ListModel {
    let list = gtk::DirectoryList::new(Some(ATTRIBUTES), Some(dir));

    let filter = gtk::CustomFilter::new(|item| {
        let Some(info) = item.downcast_ref::<gio::FileInfo>() else {
            return false;
        };
        !info.is_hidden()
            && (info.file_type() == gio::FileType::Directory || audio::is_supported(info.name()))
    });

    let sorter = gtk::CustomSorter::new(|a, b| {
        let (a, b) = (
            a.downcast_ref::<gio::FileInfo>().unwrap(),
            b.downcast_ref::<gio::FileInfo>().unwrap(),
        );
        let a_dir = a.file_type() == gio::FileType::Directory;
        let b_dir = b.file_type() == gio::FileType::Directory;
        b_dir
            .cmp(&a_dir)
            .then_with(|| {
                a.display_name()
                    .to_lowercase()
                    .cmp(&b.display_name().to_lowercase())
            })
            .into()
    });

    let filtered = gtk::FilterListModel::new(Some(list), Some(filter));
    gtk::SortListModel::new(Some(filtered), Some(sorter)).upcast()
}

fn file_of(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast()
}

fn row_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let icon = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&icon);
        content.append(&label);
        let expander = gtk::TreeExpander::builder()
            .child(&content)
            .indent_for_icon(true)
            .build();
        item.set_child(Some(&expander));
    });

    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(row) = item.item().and_downcast::<gtk::TreeListRow>() else {
            return;
        };
        let Some(info) = row.item().and_downcast::<gio::FileInfo>() else {
            return;
        };
        let expander = item.child().and_downcast::<gtk::TreeExpander>().unwrap();
        expander.set_list_row(Some(&row));

        let content = expander.child().and_downcast::<gtk::Box>().unwrap();
        let icon = content.first_child().and_downcast::<gtk::Image>().unwrap();
        let label = content.last_child().and_downcast::<gtk::Label>().unwrap();

        let is_dir = info.file_type() == gio::FileType::Directory;
        icon.set_icon_name(Some(if is_dir {
            "folder-symbolic"
        } else {
            "audio-x-generic-symbolic"
        }));
        label.set_label(&info.display_name());
        item.set_activatable(is_dir);
    });

    factory
}
