//! `tf2demos gui` — the manager window: Events / Demos / Review tabs (HANDOFF §3 Layout).
//!
//! Events: every mark as a sortable, filterable table; select a row to edit it on the right.
//! Demos: master list on the left, the selected demo with its timeline and marks in the middle.
//! Review: the wizard, with the pending count as a badge. Keys when no text box has focus:
//! `Tab` next tab · `/` search · `j`/`k` move · `Enter` play · `1`–`9` toggle a label on the
//! selected mark · `c` class · `r` then `1`–`5` rating · `l` streak · `t` type a label · `w`
//! save · `Esc` clear selection / leave the wizard.

use anyhow::{Result, anyhow};
use eframe::egui::{self, Key, RichText};

use super::edit::{EditAction, EditPanel};
use super::library::{EventRow, Filter, Library, SortKey, filter, label_color, sort};
use super::theme::{Palette, Theme};
use super::timeline;
use super::wizard::ReviewApp;
use crate::config::Config;
use crate::review::{self, EventPatch, Session};
use crate::tf2;

pub const WINDOW_TITLE: &str = "tf2demos";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Events,
    Demos,
    Review,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::Events, Tab::Demos, Tab::Review];

    fn next(self) -> Tab {
        match self {
            Tab::Events => Tab::Demos,
            Tab::Demos => Tab::Review,
            Tab::Review => Tab::Events,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Tab::Events => "events",
            Tab::Demos => "demos",
            Tab::Review => "review",
        }
    }
}

/// Open the manager window on `tab`.
pub fn run_gui(cfg: &Config, theme: &Theme, tab: Tab) -> Result<()> {
    let lib = Library::load(cfg)?;
    println!(
        "tf2demos gui: {} demos, {} marks, {} to review",
        lib.index.demos.len(),
        lib.rows().len(),
        lib.pending()
    );
    let palette = theme.palette();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(WINDOW_TITLE)
            .with_app_id("tf2demos")
            .with_inner_size([1180.0, 720.0])
            .with_min_inner_size([820.0, 520.0]),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |cc| {
            palette.apply(&cc.egui_ctx);
            Ok(Box::new(GuiApp::new(lib, palette, tab)))
        }),
    )
    .map_err(|e| anyhow!("eframe: {e}"))
}

struct GuiApp {
    lib: Library,
    p: Palette,
    tab: Tab,
    filter: Filter,
    sort: (SortKey, bool),
    /// Selected mark `(demo id, tick)` on the Events and Demos tabs.
    selected: Option<(String, i64)>,
    selected_demo: Option<String>,
    demo_filter: String,
    edit: EditPanel,
    wizard: Option<ReviewApp>,
    awaiting_rating: bool,
    focus_search: bool,
    status: Option<(String, bool)>,
}

impl GuiApp {
    fn new(lib: Library, p: Palette, tab: Tab) -> Self {
        let selected_demo = lib.index.demos.last().map(|d| d.id.clone());
        GuiApp {
            lib,
            p,
            tab,
            filter: Filter::default(),
            sort: (SortKey::Date, true),
            selected: None,
            selected_demo,
            demo_filter: String::new(),
            edit: EditPanel::default(),
            wizard: None,
            awaiting_rating: false,
            focus_search: false,
            status: None,
        }
    }

    fn info(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false));
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), true));
    }

    fn reload(&mut self) {
        if let Err(err) = self.lib.reload() {
            self.error(format!("{err:#}"));
        }
    }

    /// Rows of the Events tab after filter and sort.
    fn table_rows(&self) -> Vec<EventRow> {
        let mut rows = filter(&self.lib.rows(), &self.filter);
        sort(&mut rows, self.sort.0, self.sort.1);
        rows
    }

    /// Rows of the selected demo (Demos tab), ticks ascending.
    fn demo_rows(&self) -> Vec<EventRow> {
        let Some(id) = &self.selected_demo else {
            return Vec::new();
        };
        let mut rows: Vec<EventRow> = self
            .lib
            .rows()
            .into_iter()
            .filter(|r| &r.demo_id == id)
            .collect();
        sort(&mut rows, SortKey::Tick, false);
        rows
    }

    fn visible_rows(&self) -> Vec<EventRow> {
        match self.tab {
            Tab::Events => self.table_rows(),
            Tab::Demos => self.demo_rows(),
            Tab::Review => Vec::new(),
        }
    }

    fn selected_row(&self) -> Option<EventRow> {
        let key = self.selected.as_ref()?;
        self.lib.rows().into_iter().find(|r| &r.key() == key)
    }

    fn select(&mut self, row: &EventRow) {
        self.selected = Some(row.key());
        self.selected_demo = Some(row.demo_id.clone());
        self.edit.select(row);
        self.awaiting_rating = false;
    }

    fn move_selection(&mut self, delta: i32) {
        let rows = self.visible_rows();
        if rows.is_empty() {
            return;
        }
        let cur = self
            .selected
            .as_ref()
            .and_then(|k| rows.iter().position(|r| &r.key() == k));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize,
            None => {
                if delta < 0 {
                    rows.len() - 1
                } else {
                    0
                }
            }
        };
        let row = rows[next].clone();
        self.select(&row);
    }

    fn move_demo_selection(&mut self, delta: i32) {
        let ids = self.demo_list_ids();
        if ids.is_empty() {
            return;
        }
        let cur = self
            .selected_demo
            .as_ref()
            .and_then(|id| ids.iter().position(|x| x == id));
        let next = match cur {
            Some(i) => (i as i32 + delta).clamp(0, ids.len() as i32 - 1) as usize,
            None => 0,
        };
        self.selected_demo = Some(ids[next].clone());
        self.selected = None;
        self.edit.clear();
    }

    /// Demo ids shown in the Demos tab list, newest first, filtered by `demo_filter`.
    fn demo_list_ids(&self) -> Vec<String> {
        let needle = self.demo_filter.to_lowercase();
        self.lib
            .index
            .demos
            .iter()
            .rev()
            .filter(|d| {
                needle.split_whitespace().all(|t| {
                    let hay = format!(
                        "{} {} {} {}",
                        d.id,
                        d.map,
                        d.recorded_at.format("%Y-%m-%d"),
                        d.events
                            .iter()
                            .flat_map(|e| e.labels.iter())
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                    .to_lowercase();
                    hay.contains(t)
                })
            })
            .map(|d| d.id.clone())
            .collect()
    }

    fn play_selected(&mut self) {
        let Some(row) = self.selected_row() else {
            self.error("select a mark first");
            return;
        };
        match tf2::play(&row.file, row.tick) {
            Ok(msg) => self.info(msg),
            Err(err) => self.error(format!("{err:#}")),
        }
    }

    fn apply(&mut self, action: EditAction) {
        let Some((id, tick)) = self.selected.clone() else {
            return;
        };
        match action {
            EditAction::None => {}
            EditAction::Invalid(msg) => self.error(msg),
            EditAction::Play => self.play_selected(),
            EditAction::Save(patch) => {
                match review::edit_event(&self.lib.cfg, &id, Some(tick), &patch) {
                    Ok(_) => {
                        self.info("saved");
                        self.reload();
                        self.wizard = None;
                    }
                    Err(err) => self.error(format!("{err:#}")),
                }
            }
            EditAction::Requeue => {
                let patch = EventPatch {
                    reviewed: Some(false),
                    ..EventPatch::default()
                };
                match review::edit_event(&self.lib.cfg, &id, Some(tick), &patch) {
                    Ok(_) => {
                        self.info("demo re-queued for review");
                        self.reload();
                        self.wizard = None;
                    }
                    Err(err) => self.error(format!("{err:#}")),
                }
            }
        }
    }

    fn save_selected(&mut self) {
        match self.edit.patch() {
            Ok(patch) => self.apply(EditAction::Save(patch)),
            Err(msg) => self.error(msg),
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        let focused = ctx.memory(|m| m.focused().is_some());
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if pressed(Key::Escape) {
            if focused {
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            } else if self.awaiting_rating {
                self.awaiting_rating = false;
            } else if self.tab != Tab::Review {
                self.selected = None;
                self.edit.clear();
                self.filter.text.clear();
            }
            return;
        }
        if focused || self.tab == Tab::Review {
            // The wizard handles its own keys.
            if !focused && pressed(Key::Tab) {
                self.tab = self.tab.next();
            }
            return;
        }
        if pressed(Key::Tab) {
            self.tab = self.tab.next();
            return;
        }
        if pressed(Key::Slash) {
            self.focus_search = true;
            return;
        }
        let digit = ctx.input(|i| {
            [
                Key::Num1,
                Key::Num2,
                Key::Num3,
                Key::Num4,
                Key::Num5,
                Key::Num6,
                Key::Num7,
                Key::Num8,
                Key::Num9,
            ]
            .iter()
            .position(|k| i.key_pressed(*k))
        });
        if let Some(d) = digit {
            if self.selected.is_none() {
                return;
            }
            if self.awaiting_rating {
                if d < 5 {
                    self.edit.set_rating(Some(d as u8 + 1));
                    self.awaiting_rating = false;
                }
            } else {
                self.edit.toggle_nth(&self.lib, d);
            }
            return;
        }
        let shift = ctx.input(|i| i.modifiers.shift);
        if pressed(Key::J) || pressed(Key::ArrowDown) {
            if self.tab == Tab::Demos && shift {
                self.move_demo_selection(1);
            } else {
                self.move_selection(1);
            }
        } else if pressed(Key::K) || pressed(Key::ArrowUp) {
            if self.tab == Tab::Demos && shift {
                self.move_demo_selection(-1);
            } else {
                self.move_selection(-1);
            }
        } else if pressed(Key::Enter) || pressed(Key::P) {
            self.play_selected();
        } else if self.selected.is_some() {
            if pressed(Key::C) {
                self.edit.cycle_class(&self.lib);
            } else if pressed(Key::R) {
                self.awaiting_rating = true;
            } else if pressed(Key::L) {
                self.edit.focus_streak = true;
            } else if pressed(Key::T) {
                self.edit.focus_free_text = true;
            } else if pressed(Key::W) {
                self.save_selected();
            }
        }
    }

    // ---- drawing ------------------------------------------------------------------------

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let pending = self.lib.pending();
        ui.horizontal(|ui| {
            ui.label(RichText::new("tf2demos").heading().color(p.pink));
            ui.add_space(16.0);
            for tab in Tab::ALL {
                let title = if tab == Tab::Review && pending > 0 {
                    format!("{}  [{pending}]", tab.title())
                } else {
                    tab.title().to_string()
                };
                let selected = self.tab == tab;
                let text = RichText::new(title)
                    .size(18.0)
                    .color(if selected { p.cyan } else { p.dim })
                    .strong();
                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                    self.tab = tab;
                }
                if selected {
                    let r = ui.min_rect();
                    ui.painter().line_segment(
                        [
                            egui::pos2(r.right() - 90.0, r.bottom() + 2.0),
                            egui::pos2(r.right(), r.bottom() + 2.0),
                        ],
                        egui::Stroke::new(2.0, p.cyan),
                    );
                }
                ui.add_space(8.0);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} demos · {} marks",
                        self.lib.index.demos.len(),
                        self.lib.rows().len()
                    ))
                    .small()
                    .color(p.dim),
                );
            });
        });
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        let p = self.p;
        if let Some((msg, is_err)) = &self.status {
            ui.label(RichText::new(msg).color(if *is_err { p.pink } else { p.blue }));
        }
        let keys = match self.tab {
            Tab::Events => {
                "Tab tabs · / search · j/k move · Enter play · 1–9 labels · c class · r rating · l streak · t type · w save · Esc clear"
            }
            Tab::Demos => {
                "Tab tabs · J/K demo · j/k mark · Enter play · 1–9 labels · c class · r rating · w save · Esc clear"
            }
            Tab::Review => {
                "1–9 labels · t type · c class · r rating · l streak · p play · s skip · Enter save · Esc back"
            }
        };
        ui.label(RichText::new(keys).small().color(p.dim));
    }

    fn edit_panel(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        let Some(row) = self.selected_row() else {
            ui.add_space(24.0);
            ui.label(RichText::new("select a mark").color(p.dim));
            ui.label(RichText::new("j/k or click a row").small().color(p.dim));
            return;
        };
        self.edit.select(&row);
        if self.awaiting_rating {
            ui.label(RichText::new("rating: press 1–5").color(p.yellow));
        }
        let action = self.edit.show(ui, &self.lib, &row, &p);
        self.apply(action);
    }

    fn events_tab(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.filter.text)
                    .hint_text("search: map, label, class, date, 'review'…  [/]")
                    .desired_width(380.0),
            );
            if self.focus_search {
                resp.request_focus();
                self.focus_search = false;
            }
            ui.checkbox(&mut self.filter.unlabelled_only, "unlabelled only");
            if !self.filter.text.is_empty() && ui.button("clear").clicked() {
                self.filter.text.clear();
            }
        });
        let rows = self.table_rows();
        ui.label(
            RichText::new(format!("{} marks", rows.len()))
                .small()
                .color(p.dim),
        );
        egui::ScrollArea::both()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut clicked: Option<EventRow> = None;
                egui::Grid::new("events")
                    .num_columns(9)
                    .striped(true)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for key in SortKey::ALL {
                            let (cur, desc) = self.sort;
                            let marker = if cur == key {
                                if desc { " ▼" } else { " ▲" }
                            } else {
                                ""
                            };
                            let text = RichText::new(format!("{}{marker}", key.title()))
                                .strong()
                                .color(if cur == key { p.cyan } else { p.green });
                            if ui.add(egui::Button::new(text).frame(false)).clicked() {
                                self.sort = if cur == key {
                                    (key, !desc)
                                } else {
                                    (key, false)
                                };
                            }
                            if key == SortKey::Tick {
                                ui.label(RichText::new("into").strong().color(p.green));
                            }
                        }
                        ui.label(RichText::new("state").strong().color(p.green));
                        ui.end_row();
                        for row in &rows {
                            let selected = self.selected.as_ref() == Some(&row.key());
                            let cell = |ui: &mut egui::Ui, text: RichText| -> bool {
                                ui.selectable_label(selected, text).clicked()
                            };
                            let mut hit = false;
                            hit |= cell(
                                ui,
                                RichText::new(row.recorded_at.format("%Y-%m-%d %H:%M").to_string()),
                            );
                            hit |= cell(ui, RichText::new(&row.map).color(p.cyan));
                            hit |= cell(ui, RichText::new(row.tick.to_string()));
                            hit |= cell(
                                ui,
                                RichText::new(review::format_offset(row.tick)).color(p.yellow),
                            );
                            let label_color =
                                row.labels.first().map_or(p.dim, |l| label_color(l, &p));
                            hit |= cell(ui, RichText::new(row.labels_text()).color(label_color));
                            hit |= cell(ui, RichText::new(row.class.as_deref().unwrap_or("-")));
                            hit |= cell(
                                ui,
                                RichText::new(
                                    row.rating
                                        .map_or("-".to_string(), |r| "★".repeat(usize::from(r))),
                                )
                                .color(p.yellow),
                            );
                            hit |= cell(
                                ui,
                                RichText::new(
                                    row.streak.map_or("-".to_string(), |s| s.to_string()),
                                ),
                            );
                            hit |= cell(
                                ui,
                                RichText::new(row.state_text()).color(if row.reviewed {
                                    p.dim
                                } else {
                                    p.pink
                                }),
                            );
                            ui.end_row();
                            if hit {
                                clicked = Some(row.clone());
                            }
                        }
                    });
                if let Some(row) = clicked {
                    self.select(&row);
                }
            });
    }

    fn demos_tab(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        egui::Panel::left("demo-list")
            .default_size(300.0)
            .resizable(true)
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| {
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.demo_filter)
                        .hint_text("filter demos  [/]")
                        .desired_width(f32::INFINITY),
                );
                if self.focus_search {
                    resp.request_focus();
                    self.focus_search = false;
                }
                let ids = self.demo_list_ids();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut pick: Option<String> = None;
                        for id in &ids {
                            let Some(d) = self.lib.demo(id) else { continue };
                            let selected = self.selected_demo.as_deref() == Some(id.as_str());
                            let marks = d.events.len();
                            let unl = d.events.iter().filter(|e| !e.is_labelled()).count();
                            let text = format!(
                                "{} {} · {}{}",
                                d.recorded_at.format("%m-%d %H:%M"),
                                d.map,
                                marks,
                                if unl > 0 && !d.reviewed {
                                    format!(" ({unl} new)")
                                } else {
                                    String::new()
                                }
                            );
                            let color = if unl > 0 && !d.reviewed {
                                p.pink
                            } else {
                                p.text
                            };
                            if ui
                                .selectable_label(selected, RichText::new(text).color(color))
                                .clicked()
                            {
                                pick = Some(id.clone());
                            }
                        }
                        if let Some(id) = pick {
                            if self.selected_demo.as_ref() != Some(&id) {
                                self.selected = None;
                                self.edit.clear();
                            }
                            self.selected_demo = Some(id);
                        }
                    });
            });

        let Some(id) = self.selected_demo.clone() else {
            ui.label(RichText::new("no demos").color(p.dim));
            return;
        };
        let Some(demo) = self.lib.demo(&id).cloned() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(&demo.map).heading().color(p.cyan));
            ui.label(
                RichText::new(format!(
                    "{} · {} · {}",
                    demo.recorded_at.format("%Y-%m-%d %H:%M"),
                    review::format_offset(i64::from(demo.ticks)),
                    if demo.is_archived(&self.lib.cfg.archive_dir) {
                        "archived"
                    } else {
                        "hot"
                    }
                ))
                .color(p.dim),
            );
        });
        ui.label(
            RichText::new(format!("{} · {}", demo.file, demo.server))
                .small()
                .color(p.dim),
        );
        ui.add_space(6.0);
        let selected_tick = self
            .selected
            .as_ref()
            .filter(|(d, _)| d == &id)
            .map(|(_, t)| *t);
        if let Some(tick) = timeline::show(ui, &p, &demo, selected_tick)
            && let Some(row) = self.demo_rows().into_iter().find(|r| r.tick == tick)
        {
            self.select(&row);
        }
        ui.add_space(6.0);
        ui.separator();
        let rows = self.demo_rows();
        if rows.is_empty() {
            ui.label(RichText::new("no marks in this demo").color(p.dim));
        }
        let mut clicked: Option<EventRow> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for row in &rows {
                    let selected = self.selected.as_ref() == Some(&row.key());
                    let color = row.labels.first().map_or(p.dim, |l| label_color(l, &p));
                    let text = format!(
                        "{}  tick {:<7} {:<28} {:<9} {}  {}",
                        review::format_offset(row.tick),
                        row.tick,
                        row.labels_text(),
                        row.class.as_deref().unwrap_or("-"),
                        row.rating
                            .map_or("-".to_string(), |r| "★".repeat(usize::from(r))),
                        if row.presses > 1 {
                            format!("({} presses)", row.presses)
                        } else {
                            String::new()
                        }
                    );
                    if ui
                        .selectable_label(selected, RichText::new(text).color(color))
                        .clicked()
                    {
                        clicked = Some(row.clone());
                    }
                }
            });
        if let Some(row) = clicked {
            self.select(&row);
        }
    }

    fn review_tab(&mut self, ui: &mut egui::Ui) {
        let p = self.p;
        if self.wizard.is_none() {
            match Session::open(&self.lib.cfg) {
                Ok(session) => self.wizard = Some(ReviewApp::new(session, p, true)),
                Err(err) => {
                    ui.label(RichText::new(format!("{err:#}")).color(p.pink));
                    return;
                }
            }
        }
        let Some(wizard) = self.wizard.as_mut() else {
            return;
        };
        let mut go_back = false;
        if wizard.is_done() {
            let (_, total) = wizard.progress();
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("nothing to review").heading().color(p.green));
                ui.label(RichText::new(format!("{total} cards this session")).color(p.dim));
                ui.add_space(8.0);
                if ui.button("back to events  [Esc]").clicked()
                    || ui.input(|i| i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter))
                {
                    go_back = true;
                }
            });
        } else {
            wizard.show(ui);
        }
        let dirty = wizard.take_dirty();
        let close = wizard.take_close();
        if dirty {
            self.reload();
        }
        if close || go_back {
            self.tab = Tab::Events;
        }
    }
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        let p = self.p;
        let frame = egui::Frame::new()
            .fill(p.bg)
            .inner_margin(egui::Margin::symmetric(16, 8));

        egui::Panel::top("tabs")
            .frame(frame)
            .show_separator_line(true)
            .show(ui, |ui| self.tab_bar(ui));
        egui::Panel::bottom("status")
            .frame(frame)
            .show_separator_line(false)
            .show(ui, |ui| self.status_bar(ui));
        if self.tab != Tab::Review && self.selected.is_some() {
            egui::Panel::right("edit")
                .default_size(360.0)
                .resizable(true)
                .frame(
                    egui::Frame::new()
                        .fill(p.bg)
                        .inner_margin(egui::Margin::same(12)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.edit_panel(ui));
                });
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(p.bg)
                    .inner_margin(egui::Margin::same(12)),
            )
            .show(ui, |ui| match self.tab {
                Tab::Events => self.events_tab(ui),
                Tab::Demos => self.demos_tab(ui),
                Tab::Review => self.review_tab(ui),
            });
    }
}
