//! anime-comp desktop app.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod preview;
mod setup;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("anime-comp")
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([720.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("anime-comp", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
