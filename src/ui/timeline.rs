//! Timeline strip of one demo (HANDOFF §4): a bar of the demo's length with a diamond per mark,
//! coloured by its first label. Hover shows time and labels; click selects the mark.

use eframe::egui::{self, Align2, FontId, Pos2, Sense, Shape, Stroke, StrokeKind};

use super::library::label_color;
use super::theme::Palette;
use crate::index::DemoEntry;
use crate::review::format_offset;

const HEIGHT: f32 = 56.0;
const BAR_H: f32 = 10.0;
const DIAMOND: f32 = 7.0;
const HIT_PX: f32 = 10.0;

/// Draw the strip; returns the tick of a mark that was clicked.
pub fn show(
    ui: &mut egui::Ui,
    p: &Palette,
    demo: &DemoEntry,
    selected: Option<i64>,
) -> Option<i64> {
    let width = ui.available_width().max(120.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, HEIGHT), Sense::click());
    let painter = ui.painter_at(rect);
    let total = f64::from(demo.ticks.max(1));
    let x_of = |tick: i64| {
        let frac = (tick.max(0) as f64 / total).clamp(0.0, 1.0) as f32;
        rect.left() + 8.0 + frac * (rect.width() - 16.0)
    };
    let bar_y = rect.top() + 22.0;
    let bar = egui::Rect::from_min_max(
        Pos2::new(rect.left() + 8.0, bar_y),
        Pos2::new(rect.right() - 8.0, bar_y + BAR_H),
    );
    painter.rect_filled(bar, 0.0, p.panel);
    painter.rect_stroke(bar, 0.0, Stroke::new(1.0, p.dim), StrokeKind::Inside);
    let font = FontId::monospace(11.0);
    painter.text(
        Pos2::new(bar.left(), rect.top() + 4.0),
        Align2::LEFT_TOP,
        "00:00",
        font.clone(),
        p.dim,
    );
    painter.text(
        Pos2::new(bar.right(), rect.top() + 4.0),
        Align2::RIGHT_TOP,
        format_offset(i64::from(demo.ticks)),
        font.clone(),
        p.dim,
    );

    let hover_x = resp.hover_pos().map(|pos| pos.x);
    let nearest = |x: f32| {
        demo.events
            .iter()
            .map(|e| (e.tick, (x_of(e.tick) - x).abs()))
            .filter(|(_, d)| *d <= HIT_PX)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(t, _)| t)
    };
    let hovered = hover_x.and_then(nearest);
    for e in &demo.events {
        let color = e.labels.first().map_or(p.dim, |l| label_color(l, p));
        let c = Pos2::new(x_of(e.tick), bar.center().y);
        let r = if Some(e.tick) == selected || Some(e.tick) == hovered {
            DIAMOND + 2.0
        } else {
            DIAMOND
        };
        let pts = vec![
            Pos2::new(c.x, c.y - r),
            Pos2::new(c.x + r, c.y),
            Pos2::new(c.x, c.y + r),
            Pos2::new(c.x - r, c.y),
        ];
        let stroke = if Some(e.tick) == selected {
            Stroke::new(2.0, p.text)
        } else {
            Stroke::new(1.0, p.bg)
        };
        painter.add(Shape::convex_polygon(pts, color, stroke));
    }
    let caption = hovered
        .or(selected)
        .and_then(|t| demo.events.iter().find(|e| e.tick == t));
    if let Some(e) = caption {
        let text = format!(
            "{}  tick {}  {}{}",
            format_offset(e.tick),
            e.tick,
            e.labels_text(", "),
            if e.presses > 1 {
                format!("  ({} presses)", e.presses)
            } else {
                String::new()
            }
        );
        let x = x_of(e.tick).clamp(bar.left() + 60.0, bar.right() - 60.0);
        painter.text(
            Pos2::new(x, bar.bottom() + 6.0),
            Align2::CENTER_TOP,
            text,
            font,
            p.text,
        );
    }
    if resp.clicked() {
        return resp.interact_pointer_pos().and_then(|pos| nearest(pos.x));
    }
    None
}
