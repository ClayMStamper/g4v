#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    // `g4v [folder ...]` opens the given repositories as tabs.
    let open: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("g4v")
            .with_inner_size([1400.0, 880.0])
            .with_min_inner_size([900.0, 560.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/g4v.png")).unwrap_or_default()),
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native("g4v", options, Box::new(move |cc| Ok(Box::new(g4v::ui::G4vApp::new(cc, open)))))
}
