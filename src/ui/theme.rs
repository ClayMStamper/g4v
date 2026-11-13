//! Colours, icons and style tweaks shared by all views.

use eframe::egui::{self, Color32, CornerRadius, FontId, RichText, Stroke, TextStyle, Visuals};

pub const ACCENT: Color32 = Color32::from_rgb(64, 156, 255);
pub const GREEN: Color32 = Color32::from_rgb(80, 190, 110);
pub const RED: Color32 = Color32::from_rgb(235, 87, 87);
pub const ORANGE: Color32 = Color32::from_rgb(242, 153, 74);
pub const YELLOW: Color32 = Color32::from_rgb(235, 200, 70);
pub const PURPLE: Color32 = Color32::from_rgb(170, 120, 240);
pub const MUTED: Color32 = Color32::from_rgb(140, 140, 150);

pub const LANE_COLORS: [Color32; 8] = [
    Color32::from_rgb(64, 156, 255),
    Color32::from_rgb(80, 190, 110),
    Color32::from_rgb(242, 153, 74),
    Color32::from_rgb(170, 120, 240),
    Color32::from_rgb(235, 87, 87),
    Color32::from_rgb(60, 200, 200),
    Color32::from_rgb(235, 200, 70),
    Color32::from_rgb(230, 110, 180),
];

pub mod icon {
    pub const REFRESH: &str = "⟳";
    pub const GET_LATEST: &str = "⬇";
    pub const SUBMIT: &str = "⬆";
    pub const SHELVE: &str = "📦";
    pub const LOCK: &str = "🔒";
    pub const UNLOCK: &str = "🔓";
    pub const REVERT: &str = "⟲";
    pub const BRANCH: &str = "🔀";
    pub const FOLDER: &str = "🗀";
    pub const FILE: &str = "🗋";
    pub const WARN: &str = "⚠";
    pub const OK: &str = "✔";
    pub const ADD: &str = "✚";
    pub const EDIT: &str = "✏";
    pub const DELETE: &str = "✖";
    pub const MOVE: &str = "➡";
    pub const CONFLICT: &str = "⚡";
    pub const INCOMING: &str = "⬇";
    pub const TAG: &str = "🏷";
    pub const CLOUD: &str = "☁";
    pub const SUBMODULE: &str = "🗐";
    pub const HISTORY: &str = "🕘";
    pub const INFO: &str = "ℹ";
}

pub fn apply(ctx: &egui::Context, dark: bool, scale: f32) {
    let mut visuals = if dark { Visuals::dark() } else { Visuals::light() };
    visuals.selection.bg_fill = if dark { Color32::from_rgb(38, 79, 120) } else { Color32::from_rgb(190, 220, 255) };
    visuals.selection.stroke = Stroke::new(1.0, if dark { Color32::WHITE } else { Color32::BLACK });
    visuals.hyperlink_color = ACCENT;
    if dark {
        visuals.panel_fill = Color32::from_rgb(30, 31, 34);
        visuals.window_fill = Color32::from_rgb(37, 38, 42);
        visuals.extreme_bg_color = Color32::from_rgb(22, 23, 25);
        visuals.faint_bg_color = Color32::from_rgb(36, 37, 41);
    }
    visuals.window_corner_radius = CornerRadius::same(8);
    ctx.set_visuals(visuals);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 5.0);
        style.spacing.button_padding = egui::vec2(8.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
        style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
        style.text_styles.insert(TextStyle::Small, FontId::proportional(11.5));
        style.text_styles.insert(TextStyle::Heading, FontId::proportional(19.0));
    });
    ctx.set_zoom_factor(scale.clamp(0.6, 2.5));
}

pub fn lane_color(i: usize) -> Color32 {
    LANE_COLORS[i % LANE_COLORS.len()]
}

pub fn small(t: impl Into<String>) -> RichText {
    RichText::new(t).small().color(MUTED)
}

pub fn strong(t: impl Into<String>) -> RichText {
    RichText::new(t).strong()
}

/// Large toolbar button with icon over label.
pub fn tool_button(ui: &mut egui::Ui, icon: &str, label: &str, enabled: bool, badge: Option<String>, tip: &str) -> egui::Response {
    let text = match badge {
        Some(b) => format!("{icon}  {label}  ({b})"),
        None => format!("{icon}  {label}"),
    };
    let btn = egui::Button::new(RichText::new(text).size(14.5)).min_size(egui::vec2(0.0, 30.0));
    ui.add_enabled(enabled, btn).on_hover_text(tip).on_disabled_hover_text(tip)
}

pub fn format_time(ts: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts, 0) {
        chrono::LocalResult::Single(t) => {
            let now = chrono::Local::now();
            let d = now.signed_duration_since(t);
            if d.num_seconds() < 60 {
                "just now".into()
            } else if d.num_minutes() < 60 {
                format!("{} min ago", d.num_minutes())
            } else if d.num_hours() < 24 && t.date_naive() == now.date_naive() {
                t.format("today %H:%M").to_string()
            } else if t.format("%Y").to_string() == now.format("%Y").to_string() {
                t.format("%b %d %H:%M").to_string()
            } else {
                t.format("%Y-%m-%d").to_string()
            }
        }
        _ => String::new(),
    }
}

pub fn format_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} B")
    } else if b < K * K {
        format!("{:.1} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / K / K)
    } else {
        format!("{:.2} GB", b / K / K / K)
    }
}

/// Coloured banner frame used for warnings and status messages.
pub fn banner(ui: &mut egui::Ui, color: Color32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.7)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(add);
        });
}
