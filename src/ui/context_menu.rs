//! Right-click menu on the waveform. Lists what can be done right here, with the
//! keyboard shortcut next to each entry so the menu doubles as a cheat sheet.
//!
//! Entries fire `ed.*` actions registered by the editor; shortcuts are shown via
//! the `accel` attribute only, the keys themselves are handled by the window.

use relm4::gtk::{self, gio, glib, prelude::*};

use super::markup_list::color_swatch;
use crate::core::{ColorIdx, PALETTE};
use crate::lang::tr;

/// What the menu needs to know about the editor's state.
pub struct Context {
    pub has_selection: bool,
    pub marker_selected: bool,
    pub region_selected: bool,
    pub region_exported: bool,
    pub auto_loop: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

fn entry(menu: &gio::Menu, label: &str, action: &str, accel: Option<&str>) {
    let item = gio::MenuItem::new(Some(label), Some(&format!("ed.{action}")));
    if let Some(accel) = accel {
        item.set_attribute_value("accel", Some(&accel.to_variant()));
    }
    menu.append_item(&item);
}

pub fn show(
    parent: &impl IsA<gtk::Widget>,
    x: f64,
    y: f64,
    ctx: &Context,
    on_color: impl Fn(ColorIdx) + 'static,
) {
    let menu = gio::Menu::new();

    let play = gio::Menu::new();
    entry(
        &play,
        tr("Воспроизвести / пауза", "Play / pause"),
        "play",
        Some("space"),
    );
    let label = if ctx.auto_loop {
        tr("Не зацикливать выделение", "Don't loop the selection")
    } else {
        tr("Зацикливать выделение", "Loop the selection")
    };
    entry(&play, label, "loop", Some("l"));
    menu.append_section(None, &play);

    let create = gio::Menu::new();
    if ctx.has_selection {
        entry(
            &create,
            tr("Создать регион из выделения", "Region from selection"),
            "region",
            Some("r"),
        );
    }
    entry(
        &create,
        tr("Маркер здесь", "Marker here"),
        "marker-here",
        None,
    );
    entry(
        &create,
        tr("Маркер у плейхеда", "Marker at playhead"),
        "marker",
        Some("m"),
    );
    entry(
        &create,
        tr("Начало выделения у плейхеда", "Selection start at playhead"),
        "set-in",
        Some("i"),
    );
    entry(
        &create,
        tr("Конец выделения у плейхеда", "Selection end at playhead"),
        "set-out",
        Some("o"),
    );
    menu.append_section(None, &create);

    if ctx.marker_selected || ctx.region_selected {
        let item = gio::Menu::new();
        entry(
            &item,
            tr("Переименовать", "Rename"),
            "rename",
            Some("Return"),
        );
        if ctx.region_selected {
            let label = if ctx.region_exported {
                tr("Не экспортировать", "Don't export")
            } else {
                tr("Экспортировать", "Export")
            };
            entry(&item, label, "export", Some("e"));
        }
        entry(&item, tr("Удалить", "Delete"), "delete", Some("Delete"));
        menu.append_section(None, &item);
    }

    // Colour row: a custom widget, see below.
    let colors = gio::Menu::new();
    let row = gio::MenuItem::new(None, None);
    row.set_attribute_value("custom", Some(&"colors".to_variant()));
    colors.append_item(&row);
    let title = if ctx.marker_selected || ctx.region_selected {
        tr("Цвет (1–8)", "Colour (1–8)")
    } else {
        tr(
            "Цвет следующего региона (1–8)",
            "Colour of the next region (1–8)",
        )
    };
    menu.append_section(Some(title), &colors);

    let edit = gio::Menu::new();
    if ctx.can_undo {
        entry(&edit, tr("Отменить", "Undo"), "undo", Some("<Control>z"));
    }
    if ctx.can_redo {
        entry(
            &edit,
            tr("Повторить", "Redo"),
            "redo",
            Some("<Control><Shift>z"),
        );
    }
    entry(
        &edit,
        tr("Показать весь файл", "Show the whole file"),
        "zoom-fit",
        Some("0"),
    );
    menu.append_section(None, &edit);

    let popover = gtk::PopoverMenu::from_model(Some(&menu));
    popover.set_has_arrow(false);
    popover.set_halign(gtk::Align::Start);
    popover.set_parent(parent);
    popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

    let swatches = gtk::Box::builder()
        .spacing(2)
        .margin_start(6)
        .margin_end(6)
        .build();
    let on_color = std::rc::Rc::new(on_color);
    for (i, (name, _)) in PALETTE.iter().enumerate() {
        let button = gtk::Button::builder()
            .child(&color_swatch(i as ColorIdx, 18))
            .tooltip_text(format!("{} — {name}", i + 1))
            .css_classes(["flat", "circular"])
            .build();
        let (pop, on_color) = (popover.clone(), on_color.clone());
        button.connect_clicked(move |_| {
            pop.popdown();
            on_color(i as ColorIdx);
        });
        swatches.append(&button);
    }
    popover.add_child(&swatches, "colors");

    popover.connect_closed(|p| {
        // Unparent after the action of a clicked entry has run.
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    popover.popup();
}
