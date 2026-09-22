//! egui/eframe front end: the review wizard (`tf2demos review`) and the manager window
//! (`tf2demos gui`: Events / Demos / Review tabs). Shared drawing helpers live here.

pub mod theme;

mod edit;
mod gui;
mod library;
mod timeline;
mod wizard;

use eframe::egui::{self, RichText};

pub use gui::{Tab, run_gui};
pub use wizard::run_review;

use theme::Palette;

/// A toggle-style button: filled cyan when selected.
pub(crate) fn pill(ui: &mut egui::Ui, p: &Palette, text: &str, selected: bool) -> egui::Response {
    let button = if selected {
        egui::Button::new(RichText::new(text).color(p.bg).strong()).fill(p.cyan)
    } else {
        egui::Button::new(RichText::new(text))
    };
    ui.add(button)
}

/// Green section title with a dim key hint.
pub(crate) fn section(ui: &mut egui::Ui, p: &Palette, title: &str, hint: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong().color(p.green));
        ui.label(RichText::new(hint).small().color(p.dim));
    });
}

/// Five stars; returns the new rating when one was clicked (clicking the current one clears).
pub(crate) fn stars(ui: &mut egui::Ui, p: &Palette, rating: Option<u8>) -> Option<Option<u8>> {
    let mut out = None;
    for n in 1..=5u8 {
        let lit = rating.is_some_and(|r| r >= n);
        let text = RichText::new(if lit { "★" } else { "☆" })
            .size(22.0)
            .color(if lit { p.yellow } else { p.dim });
        if ui.add(egui::Button::new(text).frame(false)).clicked() {
            out = Some(if rating == Some(n) { None } else { Some(n) });
        }
    }
    out
}
