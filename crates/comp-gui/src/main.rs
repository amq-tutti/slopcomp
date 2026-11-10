//! anime-comp desktop app.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod preview;
mod setup;

/// Window/taskbar icon (macOS uses the bundle's .icns instead).
fn icon() -> Option<eframe::egui::IconData> {
    let bytes = include_bytes!("../../../packaging/icon/icon_256.png");
    let dec = png::Decoder::new(std::io::Cursor::new(&bytes[..]));
    let mut r = dec.read_info().ok()?;
    let mut buf = vec![0; r.output_buffer_size()];
    let info = r.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(eframe::egui::IconData { rgba: buf, width: info.width, height: info.height })
}

fn main() -> eframe::Result {
    let mut viewport = eframe::egui::ViewportBuilder::default();
    if let Some(i) = icon() {
        viewport = viewport.with_icon(i);
    }
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title("anime-comp")
            .with_inner_size([1180.0, 820.0])
            .with_min_inner_size([720.0, 520.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("anime-comp", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
