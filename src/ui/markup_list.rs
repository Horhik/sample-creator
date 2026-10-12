//! List of markers and regions under the waveform: inline rename, colour,
//! export flag, channel mode, delete. Rebuilt from the markup after each edit.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use relm4::gtk::{self, gdk, glib, prelude::*};

use super::waveform::{color_rgba, format_time};
use crate::core::{ChannelMode, ColorIdx, GAIN_MAX, GAIN_MIN, Id, Item, Markup, PALETTE};
use crate::lang::{sec, tr};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListEvent {
    Select(Item),
    Rename(Item, String),
    Color(Item, ColorIdx),
    Export(Id, bool),
    Channels(Id, ChannelMode),
    /// Region volume in tenths of a dB.
    Gain(Id, i16),
    Delete(Item),
    /// Editing finished (Enter / Escape): give focus back to the waveform.
    Done,
}

/// "+3.5 dB", "−2 dB", "0 dB".
pub fn gain_label(tenths: i16) -> String {
    let db = f32::from(tenths) / 10.0;
    let unit = tr("дБ", "dB");
    if tenths == 0 {
        format!("0 {unit}")
    } else if db.fract() == 0.0 {
        format!("{:+} {unit}", db as i32)
    } else {
        format!("{db:+.1} {unit}")
    }
}

fn channel_modes() -> [(ChannelMode, &'static str); 4] {
    [
        (ChannelMode::AsSource, tr("как есть", "as is")),
        (ChannelMode::Left, tr("только L", "L only")),
        (ChannelMode::Right, tr("только R", "R only")),
        (
            ChannelMode::SplitMono,
            tr("L и R отдельно", "L and R apart"),
        ),
    ]
}

type Handler = Rc<RefCell<Option<Rc<dyn Fn(ListEvent)>>>>;

#[derive(Clone)]
pub struct MarkupList {
    root: gtk::ScrolledWindow,
    list: gtk::ListBox,
    placeholder: gtk::Label,
    rows: Rc<RefCell<Vec<(Item, gtk::ListBoxRow, gtk::Entry)>>>,
    /// Set while rebuilding, so programmatic selection does not echo back.
    updating: Rc<Cell<bool>>,
    on_event: Handler,
}

impl Default for MarkupList {
    fn default() -> Self {
        Self::new()
    }
}

impl MarkupList {
    /// The region's volume: a button with the current gain that opens a
    /// slider. The library is written when the slider is put away.
    fn gain_button(&self, id: Id, gain: i16) -> gtk::MenuButton {
        let button = gtk::MenuButton::builder()
            .label(gain_label(gain))
            .tooltip_text(tr(
                "Громкость региона: выше исходной — громче в разметке и при экспорте",
                "Region volume: above the original it plays and exports louder",
            ))
            .css_classes(["flat"])
            .build();
        let scale = gtk::Scale::with_range(
            gtk::Orientation::Horizontal,
            f64::from(GAIN_MIN) / 10.0,
            f64::from(GAIN_MAX) / 10.0,
            0.5,
        );
        scale.set_value(f64::from(gain) / 10.0);
        scale.set_width_request(260);
        scale.set_draw_value(false);
        scale.add_mark(0.0, gtk::PositionType::Bottom, Some("0"));
        let b = button.clone();
        scale.connect_value_changed(move |s| b.set_label(&gain_label(tenths(s.value()))));
        let popover = gtk::Popover::builder().child(&scale).build();
        let (this, s) = (self.clone(), scale.clone());
        popover.connect_closed(move |_| {
            let now = tenths(s.value());
            if now != gain {
                this.emit(ListEvent::Gain(id, now));
            }
        });
        button.set_popover(Some(&popover));
        button
    }

    pub fn new() -> Self {
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .css_classes(["boxed-list-separate", "markup-list"])
            .build();
        let placeholder = gtk::Label::builder()
            .label(tr(
                "Нет маркеров и регионов. M — маркер, выделите участок и нажмите R — регион.",
                "No markers or regions. M — marker; select a stretch and press R — region.",
            ))
            .wrap(true)
            .css_classes(["dim-label"])
            .margin_top(18)
            .margin_bottom(18)
            .build();
        list.set_placeholder(Some(&placeholder));
        let root = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(120)
            .child(&list)
            .build();

        let me = MarkupList {
            root,
            list,
            placeholder,
            rows: Rc::default(),
            updating: Rc::default(),
            on_event: Rc::default(),
        };

        let this = me.clone();
        me.list.connect_row_selected(move |_, row| {
            if this.updating.get() {
                return;
            }
            let item = row.and_then(|row| {
                this.rows
                    .borrow()
                    .iter()
                    .find(|(_, r, _)| r == row)
                    .map(|(item, _, _)| *item)
            });
            if let Some(item) = item {
                this.emit(ListEvent::Select(item));
            }
        });
        me
    }

    pub fn widget(&self) -> &gtk::ScrolledWindow {
        &self.root
    }

    pub fn connect_event(&self, f: impl Fn(ListEvent) + 'static) {
        *self.on_event.borrow_mut() = Some(Rc::new(f));
    }

    fn emit(&self, e: ListEvent) {
        let handler = self.on_event.borrow().clone();
        if let Some(handler) = handler {
            handler(e);
        }
    }

    /// Highlights `item` without rebuilding (rebuilding would drop an entry being edited).
    pub fn set_selected(&self, item: Option<Item>) {
        self.updating.set(true);
        let row = self
            .rows
            .borrow()
            .iter()
            .find(|(i, _, _)| Some(*i) == item)
            .map(|(_, row, _)| row.clone());
        match row {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
        self.updating.set(false);
    }

    pub fn set(&self, markup: &Markup, selected: Option<Item>, rate: u32, channels: u16) {
        self.updating.set(true);
        let scroll = self.root.vadjustment().value();
        let editing = self
            .rows
            .borrow()
            .iter()
            .find(|(_, _, e)| e.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN))
            .map(|(item, _, _)| *item);
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.rows.borrow_mut().clear();

        // Markers and regions interleaved by time, as they appear on the waveform.
        let mut items: Vec<(u64, Item)> = markup
            .markers
            .iter()
            .map(|m| (m.pos, Item::Marker(m.id)))
            .chain(markup.regions.iter().map(|r| (r.start, Item::Region(r.id))))
            .collect();
        items.sort_by_key(|(pos, item)| (*pos, matches!(item, Item::Region(_))));

        for (_, item) in items {
            let (row, entry) = self.build_row(markup, item, rate, channels);
            self.list.append(&row);
            if Some(item) == selected {
                self.list.select_row(Some(&row));
            }
            self.rows.borrow_mut().push((item, row, entry));
        }
        self.placeholder
            .set_visible(markup.markers.is_empty() && markup.regions.is_empty());

        // Keep typing where the user was, e.g. after clicking from one name to another.
        if let Some((_, _, entry)) = self
            .rows
            .borrow()
            .iter()
            .find(|(i, _, _)| Some(*i) == editing)
        {
            entry.grab_focus();
            entry.set_position(-1);
        }
        let adj = self.root.vadjustment();
        glib::idle_add_local_once(move || adj.set_value(scroll));
        self.updating.set(false);
    }

    /// Puts the cursor into the name field of `item`.
    pub fn focus_name(&self, item: Item) {
        if let Some((_, row, entry)) = self.rows.borrow().iter().find(|(i, _, _)| *i == item) {
            row.grab_focus();
            entry.grab_focus();
            entry.select_region(0, -1);
        }
    }

    fn build_row(
        &self,
        markup: &Markup,
        item: Item,
        rate: u32,
        channels: u16,
    ) -> (gtk::ListBoxRow, gtk::Entry) {
        let secs = |frame: u64| frame as f64 / f64::from(rate.max(1));
        let (name, color, time, region) = match item {
            Item::Marker(id) => {
                let m = markup.marker(id).expect("item comes from this markup");
                (
                    m.name.clone(),
                    m.color,
                    format_time(secs(m.pos), true),
                    None,
                )
            }
            Item::Region(id) => {
                let r = markup.region(id).expect("item comes from this markup");
                let time = format!(
                    "{} – {}  ({:.2} {})",
                    format_time(secs(r.start), true),
                    format_time(secs(r.end), true),
                    secs(r.len()),
                    sec()
                );
                (r.name.clone(), r.color, time, Some(r))
            }
        };

        let row_box = gtk::Box::builder()
            .spacing(8)
            .margin_start(6)
            .margin_end(6)
            .margin_top(3)
            .margin_bottom(3)
            .build();

        row_box.append(&self.color_button(item, color));

        let kind = gtk::Image::from_icon_name(match item {
            Item::Marker(_) => "mark-location-symbolic",
            Item::Region(_) => "object-select-symbolic",
        });
        kind.set_tooltip_text(Some(match item {
            Item::Marker(_) => tr("маркер", "marker"),
            Item::Region(_) => tr("регион", "region"),
        }));
        kind.add_css_class("dim-label");
        row_box.append(&kind);

        let entry = gtk::Entry::builder()
            .text(&name)
            .placeholder_text(tr("без имени", "untitled"))
            .hexpand(true)
            .has_frame(false)
            .build();
        self.connect_rename(&entry, item, name);
        row_box.append(&entry);

        let time_label = gtk::Label::builder()
            .label(&time)
            .css_classes(["dim-label", "numeric", "caption"])
            .build();
        row_box.append(&time_label);

        if let Some(r) = region {
            row_box.append(&self.gain_button(r.id, r.fx.gain));
            if channels == 2 {
                let names: Vec<&str> = channel_modes().iter().map(|(_, n)| *n).collect();
                let dropdown = gtk::DropDown::from_strings(&names);
                dropdown.set_tooltip_text(Some(tr("Каналы при экспорте", "Channels on export")));
                dropdown.add_css_class("flat");
                let pos = channel_modes()
                    .iter()
                    .position(|(m, _)| *m == r.channels)
                    .unwrap_or(0);
                dropdown.set_selected(pos as u32);
                let (this, id) = (self.clone(), r.id);
                dropdown.connect_selected_notify(move |d| {
                    if let Some((mode, _)) = channel_modes().get(d.selected() as usize) {
                        this.emit(ListEvent::Channels(id, *mode));
                    }
                });
                row_box.append(&dropdown);
            }

            let export = gtk::CheckButton::builder()
                .active(r.export)
                .tooltip_text(tr("В экспорт (E)", "Export (E)"))
                .build();
            let (this, id) = (self.clone(), r.id);
            export.connect_toggled(move |b| this.emit(ListEvent::Export(id, b.is_active())));
            row_box.append(&export);
        }

        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text(tr("Удалить (Del)", "Delete (Del)"))
            .css_classes(["flat", "circular"])
            .build();
        let this = self.clone();
        delete.connect_clicked(move |_| this.emit(ListEvent::Delete(item)));
        row_box.append(&delete);

        let row = gtk::ListBoxRow::builder().child(&row_box).build();
        (row, entry)
    }

    fn connect_rename(&self, entry: &gtk::Entry, item: Item, original: String) {
        let original = Rc::new(original);

        let (this, orig) = (self.clone(), original.clone());
        entry.connect_activate(move |e| {
            let text = e.text().trim().to_string();
            if text != *orig {
                this.emit(ListEvent::Rename(item, text));
            }
            this.emit(ListEvent::Done);
        });

        let focus = gtk::EventControllerFocus::new();
        let (this, orig, e) = (self.clone(), original.clone(), entry.clone());
        focus.connect_enter(move |_| {
            if !this.updating.get() {
                this.emit(ListEvent::Select(item));
            }
        });
        let this2 = self.clone();
        focus.connect_leave(move |_| {
            let text = e.text().trim().to_string();
            if !this2.updating.get() && text != *orig {
                this2.emit(ListEvent::Rename(item, text));
            }
        });
        entry.add_controller(focus);

        let keys = gtk::EventControllerKey::new();
        let (this, e) = (self.clone(), entry.clone());
        keys.connect_key_pressed(move |_, key, _, _| {
            if key == gdk::Key::Escape {
                e.set_text(&original);
                this.emit(ListEvent::Done);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(keys);
    }

    fn color_button(&self, item: Item, color: ColorIdx) -> gtk::MenuButton {
        let swatch = color_swatch(color, 14);
        let button = gtk::MenuButton::builder()
            .child(&swatch)
            .tooltip_text(tr("Цвет (1–8)", "Colour (1–8)"))
            .css_classes(["flat", "circular"])
            .build();

        let grid = gtk::Grid::builder()
            .row_spacing(4)
            .column_spacing(4)
            .build();
        let popover = gtk::Popover::builder().child(&grid).build();
        for (i, (name, _)) in PALETTE.iter().enumerate() {
            let pick = gtk::Button::builder()
                .child(&color_swatch(i as ColorIdx, 18))
                .tooltip_text(format!("{} — {name}", i + 1))
                .css_classes(["flat"])
                .build();
            let (this, pop) = (self.clone(), popover.clone());
            pick.connect_clicked(move |_| {
                pop.popdown();
                this.emit(ListEvent::Color(item, i as ColorIdx));
            });
            grid.attach(&pick, (i % 4) as i32, (i / 4) as i32, 1, 1);
        }
        button.set_popover(Some(&popover));
        button
    }
}

/// A filled circle in palette colour `idx`.
pub fn color_swatch(idx: ColorIdx, size: i32) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(size)
        .content_height(size)
        .valign(gtk::Align::Center)
        .build();
    let color = color_rgba(idx);
    area.set_draw_func(move |_, cr, w, h| {
        let r = f64::from(w.min(h)) / 2.0;
        cr.arc(
            f64::from(w) / 2.0,
            f64::from(h) / 2.0,
            r - 1.0,
            0.0,
            std::f64::consts::TAU,
        );
        cr.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            1.0,
        );
        cr.fill().ok();
    });
    area
}

/// Slider value in dB → tenths, on half-dB steps.
fn tenths(db: f64) -> i16 {
    ((db * 2.0).round() * 5.0) as i16
}
