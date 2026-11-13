//! Small custom widgets: fast full-width list rows painted directly.

use eframe::egui::{self, Align2, Color32, FontId, Rect, Response, Sense, Ui};

pub const ROW_H: f32 = 22.0;

/// Allocates a full-width row, paints hover/selection and returns a painter cursor.
pub fn row(ui: &mut Ui, selected: bool, drag: bool) -> (Response, RowPainter) {
    let sense = if drag { Sense::click_and_drag() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), sense);
    let visuals = ui.visuals();
    if selected {
        ui.painter().rect_filled(rect, 3.0, visuals.selection.bg_fill);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill.gamma_multiply(0.6));
    }
    let text_color = if selected { visuals.strong_text_color() } else { visuals.text_color() };
    (
        resp,
        RowPainter {
            painter: ui.painter().clone(),
            rect,
            x: rect.left() + 6.0,
            font: FontId::proportional(13.5),
            text_color,
            right: rect.right() - 6.0,
        },
    )
}

pub struct RowPainter {
    pub painter: egui::Painter,
    pub rect: Rect,
    pub x: f32,
    pub font: FontId,
    pub text_color: Color32,
    right: f32,
}

impl RowPainter {
    pub fn indent(&mut self, px: f32) {
        self.x += px;
    }

    /// Draws text left-to-right and returns its rect.
    pub fn text(&mut self, s: &str, color: Option<Color32>) -> Rect {
        let r = self.painter.text(
            egui::pos2(self.x, self.rect.center().y),
            Align2::LEFT_CENTER,
            s,
            self.font.clone(),
            color.unwrap_or(self.text_color),
        );
        self.x = r.right() + 6.0;
        r
    }

    pub fn italic(&mut self, s: &str, color: Color32) -> Rect {
        let galley = self.painter.layout_no_wrap(s.to_string(), self.font.clone(), color);
        let pos = egui::pos2(self.x, self.rect.center().y - galley.size().y / 2.0);
        let r = Rect::from_min_size(pos, galley.size());
        self.painter.galley(pos, galley, color);
        // underline-free "ghost" marker: a thin strike to the left
        self.x = r.right() + 6.0;
        r
    }

    pub fn small(&mut self, s: &str, color: Color32) -> Rect {
        let r = self.painter.text(egui::pos2(self.x, self.rect.center().y), Align2::LEFT_CENTER, s, FontId::proportional(11.5), color);
        self.x = r.right() + 6.0;
        r
    }

    /// Draws text right-aligned, moving the right edge leftwards.
    pub fn right_text(&mut self, s: &str, color: Color32) -> Rect {
        let r = self.painter.text(egui::pos2(self.right, self.rect.center().y), Align2::RIGHT_CENTER, s, FontId::proportional(12.0), color);
        self.right = r.left() - 10.0;
        r
    }

    /// Text clipped to end before `limit_x`.
    pub fn text_clipped(&mut self, s: &str, color: Option<Color32>, limit_x: f32) -> Rect {
        let clip = Rect::from_min_max(egui::pos2(self.x, self.rect.top()), egui::pos2(limit_x.max(self.x), self.rect.bottom()));
        let p = self.painter.with_clip_rect(clip);
        let r =
            p.text(egui::pos2(self.x, self.rect.center().y), Align2::LEFT_CENTER, s, self.font.clone(), color.unwrap_or(self.text_color));
        self.x = r.right().min(limit_x) + 6.0;
        r
    }

    pub fn right_edge(&self) -> f32 {
        self.right
    }
}

/// Shift/ctrl aware selection update for list views.
pub fn update_selection(
    selection: &mut std::collections::BTreeSet<String>,
    anchor: &mut Option<String>,
    order: &[String],
    clicked: &str,
    modifiers: egui::Modifiers,
) {
    if modifiers.shift {
        if let Some(a) = anchor.clone() {
            let ia = order.iter().position(|p| *p == a);
            let ib = order.iter().position(|p| p == clicked);
            if let (Some(ia), Some(ib)) = (ia, ib) {
                let (lo, hi) = if ia <= ib { (ia, ib) } else { (ib, ia) };
                if !modifiers.command {
                    selection.clear();
                }
                for p in &order[lo..=hi] {
                    selection.insert(p.clone());
                }
                return;
            }
        }
    }
    if modifiers.command {
        if !selection.remove(clicked) {
            selection.insert(clicked.to_string());
        }
    } else {
        selection.clear();
        selection.insert(clicked.to_string());
    }
    *anchor = Some(clicked.to_string());
}

/// Dragged list of file paths (for moving between changelists).
#[derive(Clone, Debug)]
pub struct DraggedFiles(pub Vec<String>);
