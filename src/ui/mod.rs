//! `tf2demos review` — the egui/eframe labelling wizard (HANDOFF §4 "Review wizard").
//!
//! One card per unlabelled event. All state lives in [`Session`] (display-free, tested); this
//! module only draws it and maps keys. Keys when no text box has focus:
//! `1`–`9` label · `t` type a label · `c` cycle class · `r` then `1`–`5` rating · `l` streak ·
//! `p` play at tick · `s` skip · `Enter` save (or play when no label yet) · `Esc` quit.
//! Repaints happen only on input (T2 iGPU: no `request_repaint` loops).

pub mod theme;

use anyhow::{Result, anyhow};
use eframe::egui::{self, Color32, Key, RichText};

use crate::config::Config;
use crate::review::{Answer, Card, Session};
use crate::tf2;
use theme::{Palette, Theme};

/// Window title; `niri msg --json windows` finds it by this.
pub const WINDOW_TITLE: &str = "tf2demos review";

/// Open the wizard. Returns without a window when the queue is empty.
pub fn run_review(cfg: &Config, theme: &Theme) -> Result<()> {
    let session = Session::open(cfg)?;
    if session.is_done() {
        println!("tf2demos review: nothing to review");
        return Ok(());
    }
    let (_, total) = session.progress();
    println!("tf2demos review: {total} marks to label");
    let palette = theme.palette();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(WINDOW_TITLE)
            .with_app_id("tf2demos")
            .with_inner_size([760.0, 600.0])
            .with_min_inner_size([600.0, 460.0]),
        centered: true,
        ..Default::default()
    };
    eframe::run_native(
        WINDOW_TITLE,
        options,
        Box::new(move |cc| {
            palette.apply(&cc.egui_ctx);
            Ok(Box::new(ReviewApp::new(session, palette)))
        }),
    )
    .map_err(|e| anyhow!("eframe: {e}"))
}

/// What the user has entered for the current card.
#[derive(Debug, Clone, Default)]
struct Draft {
    label: Option<String>,
    free_text: String,
    class: Option<String>,
    rating: Option<u8>,
    streak: String,
}

impl Draft {
    fn for_card(last_class: Option<&str>) -> Draft {
        Draft {
            class: last_class.map(str::to_string),
            ..Draft::default()
        }
    }
}

struct ReviewApp {
    session: Session,
    p: Palette,
    draft: Draft,
    /// `r` was pressed: the next `1`–`5` sets the rating.
    awaiting_rating: bool,
    /// `t` / `l` were pressed: focus that box on the next frame.
    focus_free_text: bool,
    focus_streak: bool,
    /// `(message, is_error)` shown in the bottom bar.
    status: Option<(String, bool)>,
}

impl ReviewApp {
    fn new(session: Session, p: Palette) -> Self {
        let draft = Draft::for_card(session.last_class());
        ReviewApp {
            session,
            p,
            draft,
            awaiting_rating: false,
            focus_free_text: false,
            focus_streak: false,
            status: None,
        }
    }

    fn info(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), false));
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.status = Some((msg.into(), true));
    }

    fn next_card(&mut self) {
        self.draft = Draft::for_card(self.session.last_class());
        self.awaiting_rating = false;
    }

    fn cycle_class(&mut self) {
        let classes = self.session.classes();
        if classes.is_empty() {
            return;
        }
        let next = match self.draft.class.as_deref() {
            Some(c) => classes
                .iter()
                .position(|x| x == c)
                .map_or(0, |i| (i + 1) % classes.len()),
            None => 0,
        };
        self.draft.class = Some(classes[next].clone());
    }

    fn pick_label(&mut self, n: usize) {
        if let Some(l) = self.session.labels().get(n) {
            self.draft.label = Some(l.clone());
            self.info(format!("label: {l}"));
        }
    }

    fn play(&mut self) {
        let Some(card) = self.session.current() else {
            return;
        };
        let tick = card.tick;
        let Some(file) = self.session.current_file() else {
            self.error("demo is not in the index any more");
            return;
        };
        match tf2::play(&file, tick) {
            Ok(msg) => self.info(msg),
            Err(err) => self.error(format!("{err:#}")),
        }
    }

    fn save(&mut self) {
        let Some(label) = self.draft.label.clone() else {
            self.error("pick a label first (1–9, or t to type one)");
            return;
        };
        let streak = match self.draft.streak.trim() {
            "" => 1,
            s => match s.parse::<u32>() {
                Ok(n) if n >= 1 => n,
                _ => {
                    self.error("streak must be a whole number ≥ 1 (blank = 1)");
                    return;
                }
            },
        };
        let answer = Answer {
            label,
            class: self.draft.class.clone(),
            rating: self.draft.rating,
            streak,
        };
        match self.session.save(answer) {
            Ok(()) => {
                self.info("saved");
                self.next_card();
            }
            Err(err) => self.error(format!("{err:#}")),
        }
    }

    fn skip(&mut self) {
        match self.session.skip() {
            Ok(()) => {
                self.info("skipped");
                self.next_card();
            }
            Err(err) => self.error(format!("{err:#}")),
        }
    }

    /// Global hotkeys; only when no text box has keyboard focus.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        let focused = ctx.memory(|m| m.focused().is_some());
        if ctx.input(|i| i.key_pressed(Key::Escape)) {
            if focused {
                ctx.memory_mut(|m| {
                    if let Some(id) = m.focused() {
                        m.surrender_focus(id);
                    }
                });
            } else if self.awaiting_rating {
                self.awaiting_rating = false;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        if focused {
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
            if self.awaiting_rating {
                if d < 5 {
                    self.draft.rating = Some(d as u8 + 1);
                    self.awaiting_rating = false;
                }
            } else if self.session.is_done() {
                // nothing
            } else {
                self.pick_label(d);
            }
            return;
        }
        if self.session.is_done() {
            if ctx.input(|i| i.key_pressed(Key::Enter)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            return;
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if pressed(Key::C) {
            self.cycle_class();
        } else if pressed(Key::R) {
            self.awaiting_rating = true;
        } else if pressed(Key::L) {
            self.focus_streak = true;
        } else if pressed(Key::T) {
            self.focus_free_text = true;
        } else if pressed(Key::P) {
            self.play();
        } else if pressed(Key::S) {
            self.skip();
        } else if pressed(Key::Enter) {
            if self.draft.label.is_some() {
                self.save();
            } else {
                self.play();
            }
        }
    }

    fn card_header(&self, ui: &mut egui::Ui, card: &Card) {
        let (pos, total) = self.session.progress();
        ui.horizontal(|ui| {
            ui.label(RichText::new(&card.map).heading().color(self.p.cyan));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{pos} / {total}"))
                        .heading()
                        .color(self.p.pink),
                );
            });
        });
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} into demo", card.offset()))
                    .strong()
                    .color(self.p.yellow),
            );
            if card.presses > 1 {
                ui.label(RichText::new(format!("· {} presses", card.presses)).color(self.p.purple));
            }
            ui.label(
                RichText::new(format!("· {}", card.recorded_at.format("%Y-%m-%d %H:%M")))
                    .color(self.p.dim),
            );
            ui.label(
                RichText::new(format!(
                    "· demo {}",
                    crate::review::format_offset(
                        (f64::from(card.demo_seconds) * crate::demo::TICKS_PER_SEC) as i64
                    )
                ))
                .color(self.p.dim),
            );
        });
        if let Some(file) = self.session.current_file() {
            ui.label(RichText::new(file).small().color(self.p.dim));
        }
    }

    fn pill(&self, ui: &mut egui::Ui, text: String, selected: bool) -> egui::Response {
        let button = if selected {
            egui::Button::new(RichText::new(text).color(self.p.bg).strong()).fill(self.p.cyan)
        } else {
            egui::Button::new(RichText::new(text))
        };
        ui.add(button)
    }

    fn section(&self, ui: &mut egui::Ui, title: &str, hint: &str) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).strong().color(self.p.green));
            ui.label(RichText::new(hint).small().color(self.p.dim));
        });
    }

    fn card_body(&mut self, ui: &mut egui::Ui) {
        // Label.
        self.section(ui, "label", "1–9, or t to type");
        let labels: Vec<String> = self.session.labels().to_vec();
        ui.horizontal_wrapped(|ui| {
            for (i, l) in labels.iter().enumerate() {
                let selected = self.draft.label.as_deref() == Some(l.as_str());
                let text = if i < 9 {
                    format!("{} {l}", i + 1)
                } else {
                    l.clone()
                };
                if self.pill(ui, text, selected).clicked() {
                    self.draft.label = Some(l.clone());
                }
            }
        });
        ui.horizontal(|ui| {
            let edit = egui::TextEdit::singleline(&mut self.draft.free_text)
                .hint_text("other label…")
                .desired_width(280.0);
            let resp = ui.add(edit);
            if self.focus_free_text {
                resp.request_focus();
                self.focus_free_text = false;
            }
            let committed = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (committed || ui.button("use").clicked()) && !self.draft.free_text.trim().is_empty()
            {
                self.draft.label = Some(self.draft.free_text.trim().to_string());
            }
            if let Some(l) = &self.draft.label {
                ui.label(RichText::new(format!("→ {l}")).color(self.p.cyan).strong());
            }
        });

        // Class.
        self.section(ui, "class", "c cycles");
        let classes: Vec<String> = self.session.classes().to_vec();
        ui.horizontal_wrapped(|ui| {
            for c in &classes {
                let selected = self.draft.class.as_deref() == Some(c.as_str());
                if self.pill(ui, c.clone(), selected).clicked() {
                    self.draft.class = if selected { None } else { Some(c.clone()) };
                }
            }
        });

        // Rating + streak on one row.
        ui.horizontal(|ui| {
            let hint = if self.awaiting_rating {
                "press 1–5"
            } else {
                "r then 1–5"
            };
            ui.label(RichText::new("rating").strong().color(self.p.green));
            ui.label(RichText::new(hint).small().color(if self.awaiting_rating {
                self.p.yellow
            } else {
                self.p.dim
            }));
            for n in 1..=5u8 {
                let lit = self.draft.rating.is_some_and(|r| r >= n);
                let star = if lit { "★" } else { "☆" };
                let text = RichText::new(star).size(22.0).color(if lit {
                    self.p.yellow
                } else {
                    self.p.dim
                });
                if ui.add(egui::Button::new(text).frame(false)).clicked() {
                    self.draft.rating = if self.draft.rating == Some(n) {
                        None
                    } else {
                        Some(n)
                    };
                }
            }
            ui.add_space(16.0);
            ui.label(RichText::new("streak").strong().color(self.p.green));
            ui.label(RichText::new("l").small().color(self.p.dim));
            let edit = egui::TextEdit::singleline(&mut self.draft.streak)
                .hint_text("1")
                .desired_width(48.0);
            let resp = ui.add(edit);
            if self.focus_streak {
                resp.request_focus();
                self.focus_streak = false;
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button("▶ play at tick  [p]").clicked() {
                self.play();
            }
            if ui.button("skip  [s]").clicked() {
                self.skip();
            }
            let save = egui::Button::new(
                RichText::new("save & next  [Enter]")
                    .color(self.p.bg)
                    .strong(),
            )
            .fill(if self.draft.label.is_some() {
                self.p.green
            } else {
                self.p.dim
            });
            if ui.add(save).clicked() {
                self.save();
            }
        });
    }

    fn done_body(&self, ui: &mut egui::Ui) {
        let (_, total) = self.session.progress();
        ui.add_space(40.0);
        ui.vertical_centered(|ui| {
            ui.label(
                RichText::new("all marks reviewed")
                    .heading()
                    .color(self.p.green),
            );
            ui.label(RichText::new(format!("{total} cards this session")).color(self.p.dim));
            ui.add_space(12.0);
            if ui.button("close  [Enter / Esc]").clicked() {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}

impl eframe::App for ReviewApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_keys(&ctx);
        let p = self.p;
        let frame = egui::Frame::new()
            .fill(p.bg)
            .inner_margin(egui::Margin::same(16))
            .stroke(egui::Stroke::new(1.0, p.pink));

        egui::Panel::bottom("status")
            .frame(egui::Frame::new().fill(p.bg).inner_margin(egui::Margin::symmetric(16, 8)))
            .show_separator_line(false)
            .show(ui, |ui| {
                if let Some((msg, is_err)) = &self.status {
                    let color: Color32 = if *is_err { p.pink } else { p.blue };
                    ui.label(RichText::new(msg).color(color));
                }
                ui.label(
                    RichText::new(
                        "1–9 label · t type · c class · r rating · l streak · p play · s skip · Enter save · Esc quit",
                    )
                    .small()
                    .color(p.dim),
                );
            });

        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if let Some(card) = self.session.current().cloned() {
                self.card_header(ui, &card);
                ui.separator();
                self.card_body(ui);
            } else {
                self.done_body(ui);
            }
        });
    }
}
