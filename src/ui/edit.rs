//! The edit panel of the manager: tags, class, rating, streak of one selected mark, with
//! play-at-tick and re-queue. It only produces an [`EditAction`]; the app persists it through
//! `review::edit_event` and reloads.

use eframe::egui::{self, Key, RichText};

use super::library::{EventRow, Library};
use super::theme::Palette;
use super::{pill, section, stars};
use crate::review::EventPatch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditAction {
    None,
    /// Persist this patch on the target mark.
    Save(EventPatch),
    /// The draft does not validate; show this message.
    Invalid(String),
    /// Play the target's demo at its tick.
    Play,
    /// Put the target's demo back into the wizard's queue (`--requeue`).
    Requeue,
}

/// Draft state for the selected mark. `target` changes reset the draft from the row.
#[derive(Debug, Default)]
pub struct EditPanel {
    pub target: Option<(String, i64)>,
    labels: Vec<String>,
    free_text: String,
    class: Option<String>,
    rating: Option<u8>,
    streak: String,
    /// `l` / `t` pressed: focus that box next frame.
    pub focus_streak: bool,
    pub focus_free_text: bool,
}

impl EditPanel {
    /// Point the panel at `row`, resetting the draft when the target changed.
    pub fn select(&mut self, row: &EventRow) {
        let key = row.key();
        if self.target.as_ref() == Some(&key) {
            return;
        }
        self.target = Some(key);
        self.labels = row.labels.clone();
        self.free_text.clear();
        self.class = row.class.clone();
        self.rating = row.rating;
        self.streak = row.streak.map(|s| s.to_string()).unwrap_or_default();
    }

    pub fn clear(&mut self) {
        *self = EditPanel::default();
    }

    /// Toggle a label by its position in the library's list (keys `1`–`9`).
    pub fn toggle_nth(&mut self, lib: &Library, n: usize) {
        if let Some(l) = lib.labels().get(n).cloned() {
            self.toggle(&l);
        }
    }

    fn toggle(&mut self, label: &str) {
        if let Some(i) = self.labels.iter().position(|x| x == label) {
            self.labels.remove(i);
        } else {
            self.labels.push(label.to_string());
        }
    }

    pub fn cycle_class(&mut self, lib: &Library) {
        let classes = lib.classes();
        if classes.is_empty() {
            return;
        }
        let next = match self.class.as_deref() {
            Some(c) => classes
                .iter()
                .position(|x| x == c)
                .map_or(0, |i| (i + 1) % classes.len()),
            None => 0,
        };
        self.class = Some(classes[next].clone());
    }

    pub fn set_rating(&mut self, r: Option<u8>) {
        self.rating = r;
    }

    /// The patch the Save button would apply, or an error message for the status bar.
    pub fn patch(&self) -> Result<EventPatch, String> {
        let streak = match self.streak.trim() {
            "" => None,
            s => match s.parse::<u32>() {
                Ok(n) if n >= 1 => Some(n),
                _ => return Err("streak must be a whole number ≥ 1 (blank = 1)".into()),
            },
        };
        Ok(EventPatch {
            labels: Some(self.labels.clone()),
            class: Some(self.class.clone()),
            rating: Some(self.rating),
            streak: streak.map(Some),
            ..EventPatch::default()
        })
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        lib: &Library,
        row: &EventRow,
        p: &Palette,
    ) -> EditAction {
        let mut action = EditAction::None;
        ui.label(RichText::new(&row.map).heading().color(p.cyan));
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "{} into demo",
                    crate::review::format_offset(row.tick)
                ))
                .strong()
                .color(p.yellow),
            );
            if row.presses > 1 {
                ui.label(RichText::new(format!("· {} presses", row.presses)).color(p.purple));
            }
            ui.label(
                RichText::new(format!("· {}", row.recorded_at.format("%Y-%m-%d %H:%M")))
                    .color(p.dim),
            );
        });
        ui.label(
            RichText::new(format!("{} · tick {}", row.demo_id, row.tick))
                .small()
                .color(p.dim),
        );
        ui.label(RichText::new(row.state_text()).small().color(p.dim));
        ui.separator();

        section(ui, p, "labels", "1–9 toggle · t type");
        let labels: Vec<String> = lib.labels().to_vec();
        ui.horizontal_wrapped(|ui| {
            for (i, l) in labels.iter().enumerate() {
                let selected = self.labels.iter().any(|x| x == l);
                let text = if i < 9 {
                    format!("{} {l}", i + 1)
                } else {
                    l.clone()
                };
                if pill(ui, p, &text, selected).clicked() {
                    self.toggle(l);
                }
            }
            // Tags on this mark that are not in the list any more still show, so they can be removed.
            for l in self.labels.clone() {
                if !labels.contains(&l) && pill(ui, p, &l, true).clicked() {
                    self.toggle(&l);
                }
            }
        });
        ui.horizontal(|ui| {
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.free_text)
                    .hint_text("other label…")
                    .desired_width(180.0),
            );
            if self.focus_free_text {
                resp.request_focus();
                self.focus_free_text = false;
            }
            let committed = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            if (committed || ui.button("add").clicked()) && !self.free_text.trim().is_empty() {
                let l = self.free_text.trim().to_string();
                if !self.labels.contains(&l) {
                    self.labels.push(l);
                }
                self.free_text.clear();
            }
        });
        ui.label(
            RichText::new(if self.labels.is_empty() {
                "→ (no label)".to_string()
            } else {
                format!("→ {}", self.labels.join(", "))
            })
            .color(if self.labels.is_empty() {
                p.dim
            } else {
                p.cyan
            })
            .strong(),
        );

        section(ui, p, "class", "c cycles");
        let classes: Vec<String> = lib.classes().to_vec();
        ui.horizontal_wrapped(|ui| {
            for c in &classes {
                let selected = self.class.as_deref() == Some(c.as_str());
                if pill(ui, p, c, selected).clicked() {
                    self.class = if selected { None } else { Some(c.clone()) };
                }
            }
        });

        section(ui, p, "rating", "r then 1–5");
        ui.horizontal(|ui| {
            if let Some(r) = stars(ui, p, self.rating) {
                self.rating = r;
            }
            ui.add_space(12.0);
            ui.label(RichText::new("streak").strong().color(p.green));
            ui.label(RichText::new("l").small().color(p.dim));
            let resp = ui.add(
                egui::TextEdit::singleline(&mut self.streak)
                    .hint_text("1")
                    .desired_width(48.0),
            );
            if self.focus_streak {
                resp.request_focus();
                self.focus_streak = false;
            }
        });

        ui.add_space(12.0);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("▶ play  [Enter]").clicked() {
                action = EditAction::Play;
            }
            let save =
                egui::Button::new(RichText::new("save  [w]").color(p.bg).strong()).fill(p.green);
            if ui.add(save).clicked() {
                action = match self.patch() {
                    Ok(patch) => EditAction::Save(patch),
                    Err(msg) => EditAction::Invalid(msg),
                };
            }
            if row.reviewed && ui.button("re-queue").clicked() {
                action = EditAction::Requeue;
            }
        });
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn row() -> EventRow {
        EventRow {
            demo_id: "d".into(),
            tick: 10,
            presses: 1,
            map: "pl_x".into(),
            recorded_at: NaiveDate::from_ymd_opt(2026, 9, 21)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            labels: vec!["matador".into()],
            class: Some("spy".into()),
            rating: Some(3),
            streak: Some(2),
            archived: false,
            reviewed: true,
            file: "demos/d.dem".into(),
        }
    }

    #[test]
    fn select_resets_draft_only_on_target_change() {
        let mut e = EditPanel::default();
        e.select(&row());
        assert_eq!(e.labels, ["matador"]);
        assert_eq!(e.streak, "2");
        e.toggle("c-tap");
        e.select(&row());
        assert_eq!(e.labels, ["matador", "c-tap"], "same target keeps edits");
        let mut other = row();
        other.tick = 20;
        e.select(&other);
        assert_eq!(e.labels, ["matador"], "new target resets");
        e.clear();
        assert!(e.target.is_none());
    }

    #[test]
    fn patch_reflects_draft_and_validates_streak() {
        let mut e = EditPanel::default();
        e.select(&row());
        e.toggle("matador");
        e.set_rating(None);
        e.streak = "x".into();
        assert!(e.patch().is_err());
        e.streak = "".into();
        let p = e.patch().unwrap();
        assert_eq!(p.labels, Some(vec![]));
        assert_eq!(p.class, Some(Some("spy".into())));
        assert_eq!(p.rating, Some(None));
        assert_eq!(p.streak, None);
        e.streak = "5".into();
        assert_eq!(e.patch().unwrap().streak, Some(Some(5)));
    }
}
