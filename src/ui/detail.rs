//! Bottom detail panel: diffs (text and image), commit details, shelf contents.

use super::tab::{Action, CenterTab, DiffData, DiffTarget, RepoTab};
use super::theme::{self, icon, ACCENT, GREEN, MUTED, ORANGE, RED};
use crate::git::diff::LineKind;
use crate::git::history::ChangedFile;
use eframe::egui::{self, Color32, RichText, Ui};

fn status_letter(c: char) -> (&'static str, Color32) {
    match c {
        'A' => (icon::ADD, GREEN),
        'D' => (icon::DELETE, RED),
        'R' | 'C' => (icon::MOVE, theme::PURPLE),
        _ => (icon::EDIT, ACCENT),
    }
}

impl RepoTab {
    pub fn detail_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        match self.center {
            CenterTab::History if self.selected_commit.is_some() => {
                ui.columns(2, |cols| {
                    self.commit_info(&mut cols[0], actions);
                    self.diff_ui(&mut cols[1]);
                });
            }
            CenterTab::Shelves if self.selected_shelf.is_some() => {
                ui.columns(2, |cols| {
                    self.shelf_info(&mut cols[0], actions);
                    self.diff_ui(&mut cols[1]);
                });
            }
            _ => self.diff_ui(ui),
        }
    }

    fn commit_info(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(d) = self.commit_detail.clone() else {
            ui.add(egui::Spinner::new());
            return;
        };
        ui.horizontal(|ui| {
            ui.label(RichText::new(d.commit.cl_label()).strong().size(15.0).color(theme::ACCENT));
            ui.label(RichText::new(&d.commit.subject).strong().size(15.0));
        });
        let body: String = d.message.lines().skip(1).collect::<Vec<_>>().join("\n");
        if !body.trim().is_empty() {
            ui.label(body.trim());
        }
        ui.add_space(4.0);
        egui::Grid::new("commit_meta").num_columns(2).spacing([10.0, 2.0]).show(ui, |ui| {
            ui.label(theme::small("Author"));
            ui.label(format!("{} <{}>", d.commit.author, d.commit.email));
            ui.end_row();
            ui.label(theme::small("Date"));
            ui.label(theme::format_time(d.commit.time));
            ui.end_row();
            if d.committer != d.commit.author {
                ui.label(theme::small("Committed by"));
                ui.label(&d.committer);
                ui.end_row();
            }
            if d.commit.cl > 0 {
                ui.label(theme::small("Changelist"));
                ui.horizontal(|ui| {
                    ui.label(d.commit.cl_label())
                        .on_hover_text("This change's position in its branch history. It's the same for everyone.");
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(d.commit.cl.to_string());
                    }
                });
                ui.end_row();
            }
            ui.label(theme::small("Git commit"));
            ui.horizontal(|ui| {
                ui.label(RichText::new(d.commit.short_id()).monospace().color(theme::MUTED)).on_hover_text(&d.commit.id);
                if ui.small_button("Copy").on_hover_text(&d.commit.id).clicked() {
                    ui.ctx().copy_text(d.commit.id.clone());
                }
            });
            ui.end_row();
        });
        ui.separator();
        ui.label(theme::strong(format!("{} file(s)", d.files.len())));
        let parent = d.commit.parents.first().cloned();
        egui::ScrollArea::vertical().id_salt("commit_files").auto_shrink([false, false]).show_rows(
            ui,
            super::widgets::ROW_H,
            d.files.len(),
            |ui, rows| {
                for f in &d.files[rows] {
                    let target = DiffTarget::Commit { id: d.commit.id.clone(), parent: parent.clone(), file: f.clone() };
                    self.changed_file_row(ui, f, target, actions, Some(&d.commit.id));
                }
            },
        );
    }

    fn changed_file_row(&self, ui: &mut Ui, f: &ChangedFile, target: DiffTarget, actions: &mut Vec<Action>, rev: Option<&str>) {
        let selected = self.diff_target.as_ref() == Some(&target);
        let (resp, mut p) = super::widgets::row(ui, selected, false);
        let (i, c) = status_letter(f.status);
        p.text(i, Some(c));
        let limit = p.right_edge();
        p.text_clipped(&f.path, None, limit);
        if let Some(o) = &f.old_path {
            p.small(&format!("from {o}"), MUTED);
        }
        if resp.clicked() {
            actions.push(Action::ShowDiff(target));
        }
        let path = f.path.clone();
        let deleted = f.status == 'D';
        let rev = rev.map(str::to_string);
        resp.context_menu(|ui| {
            if let Some(rev) = &rev {
                if !deleted && ui.button("Get this revision").on_hover_text("Replace my workspace copy with this version").clicked() {
                    actions.push(Action::GetRevision(rev.clone(), path.clone()));
                    ui.close();
                }
            }
            if ui.button("File history").clicked() {
                actions.push(Action::FileHistory(path.clone()));
                ui.close();
            }
            if ui.button("Copy path").clicked() {
                ui.ctx().copy_text(path.clone());
                ui.close();
            }
        });
    }

    fn shelf_info(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(s) = self.shelves.iter().find(|s| Some(&s.refname) == self.selected_shelf.as_ref()).cloned() else {
            return;
        };
        ui.label(RichText::new(s.display_name()).strong().size(15.0));
        ui.label(theme::small(format!("{} · {}", s.author, theme::format_time(s.time))));
        if s.is_backup() {
            ui.label(theme::small("Automatic backup made by g4v before your local files were replaced."));
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button(RichText::new("Unshelve…").strong()).on_hover_text("Copy these file versions into your workspace").clicked() {
                actions.push(Action::OpenUnshelve(s.clone(), vec![]));
            }
            if s.remote.is_none() && !s.shared && ui.button(format!("{} Share with team", icon::CLOUD)).clicked() {
                actions.push(Action::ShareShelf(s.clone()));
            }
            if s.shared && ui.button("Stop sharing").clicked() {
                actions.push(Action::UnshareShelf(s.clone()));
            }
            if ui.button("Delete…").clicked() {
                actions.push(Action::DeleteShelf(s.clone()));
            }
        });
        ui.separator();
        let Some(files) = self.shelf_files.get(&s.id).cloned() else {
            ui.add(egui::Spinner::new());
            return;
        };
        ui.label(theme::strong(format!("{} file(s)", files.len())));
        egui::ScrollArea::vertical().id_salt("shelf_files").auto_shrink([false, false]).show_rows(
            ui,
            super::widgets::ROW_H,
            files.len(),
            |ui, rows| {
                for f in &files[rows] {
                    let target = DiffTarget::Shelf { id: s.id.clone(), base: s.base.clone(), file: f.clone() };
                    self.changed_file_row(ui, f, target, actions, None);
                }
            },
        );
    }

    pub fn diff_ui(&mut self, ui: &mut Ui) {
        if self.diff_target.is_none() {
            ui.centered_and_justified(|ui| {
                ui.label(theme::small("Select a file to see what changed."));
            });
            return;
        }
        let Self { diff, textures, textures_stale, diff_loading, .. } = self;
        match diff {
            None => {
                ui.add(egui::Spinner::new());
            }
            Some(Err(e)) => {
                ui.colored_label(RED, format!("Could not load the diff: {e}"));
            }
            Some(Ok(d)) => {
                // Upload the images once per loaded diff.
                if *textures_stale {
                    *textures_stale = false;
                    let ctx = ui.ctx();
                    textures.0 = d.old_image.as_ref().map(|i| ctx.load_texture("diff_old", i.clone(), egui::TextureOptions::LINEAR));
                    textures.1 = d.new_image.as_ref().map(|i| ctx.load_texture("diff_new", i.clone(), egui::TextureOptions::LINEAR));
                }
                show_diff(ui, d, textures, *diff_loading);
            }
        }
    }
}

fn show_diff(ui: &mut Ui, d: &DiffData, textures: &(Option<egui::TextureHandle>, Option<egui::TextureHandle>), loading: bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(&d.title).strong());
        if d.diff.adds + d.diff.dels > 0 && !d.diff.binary && d.diff.lfs.is_none() {
            ui.label(RichText::new(format!("+{}", d.diff.adds)).color(GREEN));
            ui.label(RichText::new(format!("−{}", d.diff.dels)).color(RED));
        }
        if loading {
            ui.add(egui::Spinner::new());
        }
    });
    let size_line = |ui: &mut Ui| {
        let fmt = |s: Option<u64>| s.map(theme::format_size).unwrap_or_else(|| "—".into());
        ui.label(format!("Size: {} ➡ {}", fmt(d.old_size), fmt(d.new_size)));
    };
    if textures.0.is_some() || textures.1.is_some() {
        if d.diff.lfs.is_some() {
            ui.label(theme::small("Stored with Git LFS"));
        }
        size_line(ui);
        image_compare(ui, textures);
        return;
    }
    if let Some((old, new)) = &d.diff.lfs {
        ui.label(format!("{} Large file stored with Git LFS.", icon::INFO));
        let fmt = |p: &Option<crate::git::diff::LfsPointer>| p.as_ref().map(|p| theme::format_size(p.size)).unwrap_or_else(|| "—".into());
        ui.label(format!("Size: {} ➡ {}", fmt(old), fmt(new)));
        return;
    }
    if d.diff.binary {
        ui.label(format!("{} Binary file — contents can't be shown as text.", icon::INFO));
        size_line(ui);
        return;
    }
    if d.diff.lines.is_empty() {
        ui.label(theme::small("No content changes (only file mode / permissions or line endings)."));
        return;
    }
    if d.diff.truncated {
        ui.colored_label(ORANGE, "Very large diff — only the beginning is shown.");
    }
    let font = egui::FontId::monospace(13.0);
    let row_h = ui.text_style_height(&egui::TextStyle::Monospace) + 3.0;
    let lines = &d.diff.lines;
    let dark = ui.visuals().dark_mode;
    let text_color = ui.visuals().text_color();
    let max_chars = lines.iter().map(|l| l.text.chars().count()).max().unwrap_or(0).min(2000) + 24;
    let char_w = ui.painter().layout_no_wrap("MMMMMMMMMM".into(), font.clone(), text_color).size().x / 10.0;
    let content_w = max_chars as f32 * char_w;
    egui::ScrollArea::both().id_salt("diff_scroll").auto_shrink([false, false]).show_rows(ui, row_h, lines.len(), |ui, range| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let width = content_w.max(ui.available_width());
        for l in &lines[range] {
            let (bg, fg) = match l.kind {
                LineKind::Add => (Some(if dark { Color32::from_rgb(30, 64, 40) } else { Color32::from_rgb(220, 250, 225) }), text_color),
                LineKind::Del => (Some(if dark { Color32::from_rgb(74, 32, 34) } else { Color32::from_rgb(255, 225, 225) }), text_color),
                LineKind::Hunk => (None, ACCENT),
                LineKind::Meta => (None, MUTED),
                LineKind::Context => (None, text_color),
            };
            let num = |n: Option<u32>| n.map(|n| format!("{n:>5}")).unwrap_or_else(|| "     ".into());
            let sign = match l.kind {
                LineKind::Add => "+",
                LineKind::Del => "-",
                _ => " ",
            };
            let text = match l.kind {
                LineKind::Hunk | LineKind::Meta => format!("              {}", l.text),
                _ => format!("{} {} {sign} {}", num(l.old_no), num(l.new_no), l.text.replace('\t', "    ")),
            };
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, row_h), egui::Sense::hover());
            if let Some(bg) = bg {
                ui.painter().rect_filled(rect, 0.0, bg);
            }
            ui.painter().text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, text, font.clone(), fg);
        }
    });
}

fn image_compare(ui: &mut Ui, textures: &(Option<egui::TextureHandle>, Option<egui::TextureHandle>)) {
    // The images are fitted to the space the panel has *right now*. The content must never be taller or wider than
    // that space: egui persists a resizable panel's size from its content, so an overflowing preview would make the
    // panel grow back to fit it the moment the user releases the resize handle (the "snaps back to full size" bug).
    // Reserve the real height of the label above and the dimensions line below each image (plus spacing), and wrap
    // everything in a scroll area so an unusually small panel scrolls instead of growing.
    let avail = ui.available_size();
    let spacing = ui.spacing().item_spacing;
    let gap = 12.0;
    let label_h = ui.text_style_height(&egui::TextStyle::Body).max(ui.text_style_height(&egui::TextStyle::Small));
    let overhead = label_h * 2.0 + spacing.y * 3.0 + 6.0;
    let w = ((avail.x - 2.0 * (gap + spacing.x) - 4.0) / 2.0).max(40.0);
    let h = (avail.y - overhead).max(40.0);
    egui::ScrollArea::both().id_salt("image_compare_scroll").auto_shrink([false, false]).show(ui, |ui| {
        ui.horizontal_top(|ui| {
            for (label, tex) in [("Before", &textures.0), ("After", &textures.1)] {
                ui.vertical(|ui| {
                    ui.label(theme::strong(label));
                    match tex {
                        Some(t) => {
                            let size = t.size_vec2();
                            let scale = (w / size.x).min(h / size.y).min(4.0);
                            let disp = size * scale;
                            let (rect, _) = ui.allocate_exact_size(disp, egui::Sense::hover());
                            checkerboard(ui, rect);
                            ui.painter().image(
                                t.id(),
                                rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                            ui.label(theme::small(format!("{} × {}", size.x as u32, size.y as u32)));
                        }
                        None => {
                            let (rect, _) = ui.allocate_exact_size(egui::vec2(w.min(200.0), 60.0), egui::Sense::hover());
                            ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0, MUTED), egui::StrokeKind::Inside);
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "(none)", egui::FontId::proportional(13.0), MUTED);
                        }
                    }
                });
                ui.add_space(gap);
            }
        });
    });
}

fn checkerboard(ui: &Ui, rect: egui::Rect) {
    let p = ui.painter().with_clip_rect(rect);
    let s = 10.0;
    let a = Color32::from_gray(90);
    let b = Color32::from_gray(120);
    p.rect_filled(rect, 0.0, a);
    let mut y = rect.top();
    let mut row = 0;
    while y < rect.bottom() {
        let mut x = rect.left() + if row % 2 == 0 { 0.0 } else { s };
        while x < rect.right() {
            p.rect_filled(egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(s, s)), 0.0, b);
            x += 2.0 * s;
        }
        y += s;
        row += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a PNG preview used to be sized to the whole panel, so once the user released the resize handle the
    /// panel grew to fit the image and the image scaled back up. The panel must keep the size it was given.
    #[test]
    fn image_preview_does_not_grow_resizable_panel() {
        let ctx = egui::Context::default();
        let mut textures: (Option<egui::TextureHandle>, Option<egui::TextureHandle>) = (None, None);
        let mut heights = Vec::new();
        for frame in 0..8 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))),
                ..Default::default()
            };
            let mut h = 0.0;
            let mut out = ctx.run_ui(raw, |ui| {
                if textures.0.is_none() {
                    let img = egui::ColorImage::filled([800, 600], Color32::RED);
                    let tex = ui.ctx().load_texture("t", img, egui::TextureOptions::LINEAR);
                    textures = (Some(tex.clone()), Some(tex));
                }
                let r = egui::Panel::bottom("detail_test").resizable(true).default_size(240.0).min_size(120.0).show_inside(ui, |ui| {
                    ui.label("title");
                    ui.label("Size: 1 KB ➡ 2 KB");
                    image_compare(ui, &textures);
                });
                h = r.response.rect.height();
            });
            out.textures_delta.clear();
            heights.push((frame, h));
        }
        let first = heights[0].1;
        for (frame, h) in &heights {
            assert!((h - first).abs() < 0.5, "panel height changed on frame {frame}: {h} vs {first} ({heights:?})");
        }
        assert!(first < 260.0, "panel grew past its default size: {first}");
    }
}
