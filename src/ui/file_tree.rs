//! Sidebar with a lazily loaded directory tree that only shows folders and audio files.
//!
//! Built on GIO's `DirectoryList`, so folders are read asynchronously on expand and
//! the tree follows changes on disk (e.g. a recorder being plugged in). Nothing is
//! ever written to the browsed directories.
//!
//! Each audio file shows its alias if it has one, and a badge with its region
//! count and a check mark once it is marked as reviewed, from the library.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use relm4::gtk::{self, gio, glib, prelude::*};
use relm4::{ComponentParts, ComponentSender, SimpleComponent};

use crate::audio;
use crate::lang::tr;
use crate::store::{FileSummary, Library};

const ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
                          standard::is-hidden,standard::symbolic-icon";

pub struct FileTree {
    root: PathBuf,
    badges: Badges,
    /// A file to select once the view has caught up with the root.
    reveal: RefCell<Option<PathBuf>>,
    /// Set while selecting a file the editor already has open, so the
    /// selection does not open it again.
    revealing: Rc<std::cell::Cell<bool>>,
}

#[derive(Debug)]
pub enum FileTreeInput {
    SetRoot(PathBuf),
    /// Markup of this file changed: update its badge.
    Refresh(PathBuf),
    /// Highlight the open file: expand its folders and select its row.
    Reveal(PathBuf),
}

/// Name and badge label of a bound row.
type RowLabels = (glib::WeakRef<gtk::Label>, glib::WeakRef<gtk::Label>);

/// Labels of the rows currently bound, by file path.
#[derive(Clone)]
struct Badges {
    library: Rc<Library>,
    labels: Rc<RefCell<HashMap<PathBuf, RowLabels>>>,
}

impl Badges {
    fn show(&self, path: &Path, name: &gtk::Label, label: &gtk::Label) {
        let summary = self
            .library
            .summary_at(path)
            .ok()
            .flatten()
            .unwrap_or_default();
        let FileSummary {
            alias,
            regions,
            reviewed,
            ..
        } = summary;
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match alias {
            Some(alias) => {
                name.set_label(&alias);
                name.set_tooltip_text(Some(&file_name));
            }
            None => {
                name.set_label(&file_name);
                name.set_tooltip_text(None);
            }
        }
        let mut text = Vec::new();
        let mut tip = Vec::new();
        if regions > 0 {
            text.push(regions.to_string());
            tip.push(format!("{}: {regions}", tr("регионов", "regions")));
        }
        if reviewed {
            text.push("✓".to_string());
            tip.push(tr("просмотрен", "reviewed").to_string());
        }
        label.set_label(&text.join(" "));
        label.set_tooltip_text((!tip.is_empty()).then(|| tip.join(", ")).as_deref());
        label.set_visible(!text.is_empty());
    }

    fn refresh(&self, path: &Path) {
        let labels = self
            .labels
            .borrow()
            .get(path)
            .and_then(|(n, b)| Some((n.upgrade()?, b.upgrade()?)));
        if let Some((name, badge)) = labels {
            self.show(path, &name, &badge);
        }
    }
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
    type Init = (PathBuf, Rc<Library>);
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
        (root_dir, library): Self::Init,
        scrolled: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let badges = Badges {
            library,
            labels: Rc::default(),
        };
        let list_view = gtk::ListView::builder()
            .factory(&row_factory(badges.clone()))
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

        let revealing = Rc::new(std::cell::Cell::new(false));
        set_tree_root(&list_view, &root_dir, &sender, revealing.clone());
        let widgets = FileTreeWidgets {
            list_view,
            shown_root: root_dir.clone(),
        };

        ComponentParts {
            model: FileTree {
                root: root_dir,
                badges,
                reveal: RefCell::new(None),
                revealing,
            },
            widgets,
        }
    }

    fn update(&mut self, msg: Self::Input, _sender: ComponentSender<Self>) {
        match msg {
            FileTreeInput::SetRoot(dir) => self.root = dir,
            FileTreeInput::Refresh(path) => self.badges.refresh(&path),
            FileTreeInput::Reveal(path) => *self.reveal.borrow_mut() = Some(path),
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, sender: ComponentSender<Self>) {
        if widgets.shown_root != self.root {
            set_tree_root(
                &widgets.list_view,
                &self.root,
                &sender,
                self.revealing.clone(),
            );
            widgets.shown_root = self.root.clone();
        }
        // Taken once: later updates must not scroll the tree back to it.
        if let Some(path) = self.reveal.borrow_mut().take()
            && path.starts_with(&self.root)
        {
            reveal(&widgets.list_view, path, self.revealing.clone());
        }
    }
}

/// Expands the folders on the way to `path` and selects its row. Folders
/// load asynchronously, so this keeps looking for a couple of seconds.
fn reveal(list_view: &gtk::ListView, path: PathBuf, revealing: Rc<std::cell::Cell<bool>>) {
    let view = list_view.clone();
    let mut tries = 0;
    let step = move || -> bool {
        let Some(selection) = view.model().and_downcast::<gtk::SingleSelection>() else {
            return true;
        };
        for i in 0..selection.n_items() {
            let Some(row) = selection.item(i).and_downcast::<gtk::TreeListRow>() else {
                continue;
            };
            let Some(p) = row
                .item()
                .and_downcast::<gio::FileInfo>()
                .and_then(|info| file_of(&info))
                .and_then(|f| f.path())
            else {
                continue;
            };
            if p == path {
                if selection.selected() != i {
                    revealing.set(true);
                    selection.set_selected(i);
                    revealing.set(false);
                }
                view.scroll_to(i, gtk::ListScrollFlags::NONE, None);
                return true;
            }
            if path.starts_with(&p) && row.is_expandable() && !row.is_expanded() {
                row.set_expanded(true);
            }
        }
        false
    };
    let step = Rc::new(RefCell::new(step));
    if (step.borrow_mut())() {
        return;
    }
    glib::timeout_add_local(std::time::Duration::from_millis(60), move || {
        tries += 1;
        if (step.borrow_mut())() || tries > 40 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn set_tree_root(
    list_view: &gtk::ListView,
    dir: &Path,
    sender: &ComponentSender<FileTree>,
    revealing: Rc<std::cell::Cell<bool>>,
) {
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
        if revealing.get() {
            return;
        }
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

fn row_factory(badges: Badges) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();

    factory.connect_setup(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let icon = gtk::Image::new();
        let label = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        let badge = gtk::Label::builder()
            .css_classes(["caption", "dim-label", "numeric"])
            .visible(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        content.append(&icon);
        content.append(&label);
        content.append(&badge);
        let expander = gtk::TreeExpander::builder()
            .child(&content)
            .indent_for_icon(true)
            .build();
        item.set_child(Some(&expander));
    });

    let bind_badges = badges.clone();
    factory.connect_bind(move |_, item| {
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
        let label = icon.next_sibling().and_downcast::<gtk::Label>().unwrap();
        let badge = content.last_child().and_downcast::<gtk::Label>().unwrap();

        let is_dir = info.file_type() == gio::FileType::Directory;
        icon.set_icon_name(Some(if is_dir {
            "folder-symbolic"
        } else {
            "audio-x-generic-symbolic"
        }));
        label.set_label(&info.display_name());
        label.set_tooltip_text(None);
        item.set_activatable(is_dir);

        let path = (!is_dir)
            .then(|| file_of(&info).and_then(|f| f.path()))
            .flatten();
        match path {
            Some(path) => {
                bind_badges.show(&path, &label, &badge);
                bind_badges
                    .labels
                    .borrow_mut()
                    .insert(path, (label.downgrade(), badge.downgrade()));
            }
            None => badge.set_visible(false),
        }
    });

    // Recycled rows must stop answering for the file they showed before.
    factory.connect_unbind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let badge = item
            .child()
            .and_downcast::<gtk::TreeExpander>()
            .and_then(|e| e.child())
            .and_then(|c| c.last_child())
            .and_downcast::<gtk::Label>();
        if let Some(badge) = badge {
            badges
                .labels
                .borrow_mut()
                .retain(|_, (_, b)| b.upgrade().as_ref() != Some(&badge));
        }
    });

    factory
}
