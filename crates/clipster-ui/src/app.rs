//! The picker window.
//!
//! Deliberately one screen with no chrome: a filter box, a list, a hint bar.
//! Every mutation goes to the daemon over the same socket the CLI uses, so
//! this holds no state the daemon does not already own — only the query, the
//! selection, and the last list it was handed.

use crate::filter;
use clipster_core::client::request;
use clipster_core::ipc::{Request, Response, Summary};
use eframe::egui::{self, text::LayoutJob, Align, Color32, FontId, Key, Modifiers, TextFormat};

const ROW_FONT: f32 = 13.0;
const QUERY_FONT: f32 = 14.0;
const HINTS: &str = "arrows move · enter copy · ctrl+p pin · ctrl+d delete · esc close";

/// What a keypress or a click asked for. Resolved after input is read, so
/// nothing mutates while the list is being borrowed for drawing.
#[derive(Clone, Copy, PartialEq)]
enum Action {
    None,
    Close,
    Copy,
    Down,
    Up,
    TogglePin,
    Delete,
    ClearQuery,
}

pub struct Picker {
    limit: Option<usize>,
    pinned_only: bool,
    close_on_blur: bool,
    query: String,
    /// Newest-first, pinned-first, exactly as the daemon returned it.
    items: Vec<Summary>,
    /// Indices into `items`, in display order after filtering.
    visible: Vec<usize>,
    /// Index into `visible`, not into `items`.
    selected: usize,
    status: Option<String>,
    /// A window that has never been focused must not close on "focus lost" —
    /// under some compositors that is the state it starts in.
    seen_focus: bool,
    scroll_pending: bool,
}

impl Picker {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        items: Vec<Summary>,
        limit: Option<usize>,
        pinned_only: bool,
        close_on_blur: bool,
    ) -> Self {
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(6.0, 1.0);
        style.spacing.button_padding = egui::vec2(6.0, 3.0);
        cc.egui_ctx.set_style(style);

        let mut picker = Self {
            limit,
            pinned_only,
            close_on_blur,
            query: String::new(),
            items,
            visible: Vec::new(),
            selected: 0,
            status: None,
            seen_focus: false,
            scroll_pending: false,
        };
        picker.refilter();
        picker
    }

    /// Re-run the query over `items`. Order is the daemon's when the query is
    /// empty — that ordering is the product's core promise and a fuzzy score
    /// has no business overriding it for free.
    fn refilter(&mut self) {
        // Owned, so the filtered indices can be written back without the
        // borrow checker taking an interest.
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            self.visible = (0..self.items.len()).collect();
        } else {
            let mut scored: Vec<(i32, usize)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(i, item)| filter::score(&haystack(item), &query).map(|s| (s, i)))
                .collect();
            // Ties fall back to the daemon's order, which keeps the list from
            // reshuffling under the cursor as the query grows.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.visible = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
        self.scroll_pending = true;
    }

    fn reload(&mut self) {
        match request(Request::List { limit: self.limit, pinned_only: self.pinned_only }) {
            Ok(Response::Items(items)) => {
                self.items = items;
                self.refilter();
            }
            Ok(other) => self.status = Some(describe(other)),
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    fn selected_item(&self) -> Option<&Summary> {
        self.visible.get(self.selected).map(|&i| &self.items[i])
    }

    fn read_keys(&self, ctx: &egui::Context) -> Action {
        // Consumed rather than peeked: the filter box has keyboard focus for
        // the window's whole life, and would otherwise eat the arrows.
        ctx.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::Escape) {
                Action::Close
            } else if i.consume_key(Modifiers::NONE, Key::Enter) {
                Action::Copy
            } else if i.consume_key(Modifiers::NONE, Key::ArrowDown)
                || i.consume_key(Modifiers::CTRL, Key::N)
                || i.consume_key(Modifiers::CTRL, Key::J)
            {
                Action::Down
            } else if i.consume_key(Modifiers::NONE, Key::ArrowUp)
                || i.consume_key(Modifiers::CTRL, Key::K)
            {
                Action::Up
            } else if i.consume_key(Modifiers::CTRL, Key::P) {
                Action::TogglePin
            } else if i.consume_key(Modifiers::CTRL, Key::D) {
                Action::Delete
            } else if i.consume_key(Modifiers::CTRL, Key::U) {
                Action::ClearQuery
            } else {
                Action::None
            }
        })
    }

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::None => {}
            Action::Close => close(ctx),
            Action::Copy => self.copy_selected(ctx),
            Action::Down => self.move_selection(1),
            Action::Up => self.move_selection(-1),
            Action::TogglePin => self.toggle_pin(),
            Action::Delete => self.delete_selected(),
            Action::ClearQuery => {
                self.query.clear();
                self.refilter();
            }
        }
    }

    fn move_selection(&mut self, delta: isize) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.checked_sub(1).unwrap_or(last),
            _ if self.selected >= last => 0,
            _ => self.selected + 1,
        };
        self.scroll_pending = true;
    }

    fn copy_selected(&mut self, ctx: &egui::Context) {
        let Some(id) = self.selected_item().map(|item| item.id) else { return };

        // The list only carries previews, so the content is fetched now
        // rather than held for every row.
        let item = match request(Request::Get { id }) {
            Ok(Response::Item(item)) => item,
            Ok(other) => return self.status = Some(describe(other)),
            Err(e) => return self.status = Some(format!("{e:#}")),
        };

        match clipster_core::clipboard::copy(&item.content) {
            // Staying open after a successful copy would mean the next window
            // you type into is this one.
            Ok(_) => close(ctx),
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    fn toggle_pin(&mut self) {
        let Some(item) = self.selected_item() else { return };
        let (id, pinned) = (item.id, !item.pinned);
        match request(Request::SetPinned { id, pinned }) {
            // Pinning moves the entry to the top of the list, so follow it
            // rather than leaving the cursor on whatever slid into its place.
            Ok(Response::Affected(_)) => {
                self.reload();
                self.select_id(id);
            }
            Ok(other) => self.status = Some(describe(other)),
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    fn delete_selected(&mut self) {
        let Some(id) = self.selected_item().map(|item| item.id) else { return };
        match request(Request::Remove { id }) {
            // No confirmation: one row of history, and the daemon is the only
            // thing that could have restored it anyway.
            Ok(Response::Affected(_)) => self.reload(),
            Ok(other) => self.status = Some(describe(other)),
            Err(e) => self.status = Some(format!("{e:#}")),
        }
    }

    fn select_id(&mut self, id: i64) {
        if let Some(row) = self.visible.iter().position(|&i| self.items[i].id == id) {
            self.selected = row;
            self.scroll_pending = true;
        }
    }

    /// Close when focus goes elsewhere, the way a menu does. A picker left
    /// behind on another workspace is worse than one that has to be
    /// re-summoned, which is a keystroke.
    fn lost_focus(&mut self, ctx: &egui::Context) -> bool {
        match ctx.input(|i| i.viewport().focused) {
            Some(true) => {
                self.seen_focus = true;
                false
            }
            Some(false) => self.close_on_blur && self.seen_focus,
            None => false,
        }
    }

    fn query_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(5.0);
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("type to filter")
                .desired_width(f32::INFINITY)
                .font(FontId::monospace(QUERY_FONT))
                .frame(false),
        );
        ui.add_space(5.0);

        // Nothing else in the window is focusable, and the box is the whole
        // interaction, so it takes focus back whenever it loses it.
        if !response.has_focus() {
            response.request_focus();
        }
        if response.changed() {
            self.selected = 0;
            self.refilter();
        }
    }

    fn list_ui(&mut self, ui: &mut egui::Ui) {
        if self.items.is_empty() {
            return empty_note(ui, "history is empty");
        }
        if self.visible.is_empty() {
            return empty_note(ui, "no match");
        }

        let now = clipster_core::now_millis();
        let mut clicked = None;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.with_layout(egui::Layout::top_down_justified(Align::LEFT), |ui| {
                for row in 0..self.visible.len() {
                    let item = &self.items[self.visible[row]];
                    let selected = row == self.selected;
                    let job = row_job(ui, item, now);
                    let response = ui.selectable_label(selected, job);
                    if response.clicked() {
                        clicked = Some(row);
                    }
                    if selected && self.scroll_pending {
                        response.scroll_to_me(Some(Align::Center));
                    }
                }
            });
        });
        self.scroll_pending = false;

        if let Some(row) = clicked {
            self.selected = row;
            self.copy_selected(ui.ctx());
        }
    }

    fn hints_ui(&mut self, ui: &mut egui::Ui) {
        ui.add_space(3.0);
        ui.horizontal(|ui| {
            match &self.status {
                Some(message) => {
                    let error = Color32::from_rgb(214, 92, 92);
                    ui.label(egui::RichText::new(message.as_str()).color(error).size(11.0));
                }
                None => {
                    ui.label(egui::RichText::new(HINTS).weak().size(11.0));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let count = format!("{}/{}", self.visible.len(), self.items.len());
                ui.label(egui::RichText::new(count).weak().size(11.0).monospace());
            });
        });
        ui.add_space(3.0);
    }
}

impl eframe::App for Picker {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.lost_focus(ctx) {
            return close(ctx);
        }

        let action = self.read_keys(ctx);
        if action != Action::None {
            // Any deliberate keypress clears a stale error off the hint bar.
            self.status = None;
        }
        self.apply(action, ctx);

        egui::TopBottomPanel::top("query").show(ctx, |ui| self.query_ui(ui));
        egui::TopBottomPanel::bottom("hints").show(ctx, |ui| self.hints_ui(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.list_ui(ui));
    }
}

fn close(ctx: &egui::Context) {
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

fn empty_note(ui: &mut egui::Ui, text: &str) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new(text).weak());
}

fn haystack(item: &Summary) -> String {
    match &item.label {
        Some(label) => format!("{label} {}", item.preview),
        None => item.preview.clone(),
    }
}

/// One row: age, pin marker, label, preview. Monospaced throughout so the
/// age and marker columns line up without a grid layout.
fn row_job(ui: &egui::Ui, item: &Summary, now: i64) -> LayoutJob {
    let font = FontId::monospace(ROW_FONT);
    let dim = ui.visuals().weak_text_color();
    let text = ui.visuals().text_color();

    let mut job = LayoutJob::default();
    job.append(
        &format!("{:>4} ", age(now - item.updated_at)),
        0.0,
        TextFormat::simple(font.clone(), dim),
    );
    job.append(
        if item.pinned { "* " } else { "  " },
        0.0,
        TextFormat::simple(font.clone(), Color32::from_rgb(226, 178, 74)),
    );
    if let Some(label) = &item.label {
        job.append(
            &format!("[{label}] "),
            0.0,
            TextFormat::simple(font.clone(), Color32::from_rgb(110, 168, 224)),
        );
    }
    job.append(&item.preview, 0.0, TextFormat::simple(font, text));
    job
}

/// Compact age for the left column: two digits and a unit at most, so the
/// column never shifts. Precision past the unit is noise here.
fn age(millis: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;

    let seconds = millis.max(0) / 1000;
    match seconds {
        s if s < MINUTE => format!("{s}s"),
        s if s < HOUR => format!("{}m", s / MINUTE),
        s if s < DAY => format!("{}h", s / HOUR),
        s if s < WEEK => format!("{}d", s / DAY),
        s => format!("{}w", (s / WEEK).min(99)),
    }
}

fn describe(response: Response) -> String {
    match response {
        Response::Error { message } => message,
        Response::NotFound { id } => format!("entry {id} is gone"),
        other => format!("unexpected response from daemon: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_picks_a_unit_per_magnitude() {
        assert_eq!(age(0), "0s");
        assert_eq!(age(45_000), "45s");
        assert_eq!(age(90_000), "1m");
        assert_eq!(age(3 * 3_600_000), "3h");
        assert_eq!(age(50 * 3_600_000), "2d");
        assert_eq!(age(30 * 86_400_000), "4w");
    }

    #[test]
    fn age_of_a_future_timestamp_does_not_go_negative() {
        // Clock adjustments can put updated_at ahead of now.
        assert_eq!(age(-5_000), "0s");
    }

    #[test]
    fn age_never_outgrows_its_column() {
        // Including a timestamp old enough to overflow the week count, which
        // is why weeks saturate rather than run to four digits.
        for millis in [0i64, 59_000, 3_599_000, 86_399_000, 6 * 86_400_000, 9000 * 86_400_000] {
            assert!(age(millis).len() <= 3, "{}", age(millis));
        }
    }
}
