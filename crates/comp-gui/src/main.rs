//! SlopComp desktop app.
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

mod app;
mod preview;
mod setup;
mod theme;
mod viewer;

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
    // Developer flags: --dev-storage keeps test runs away from the real saved setup;
    // --size=WxH opens the window at that size (and doesn't restore or save the window size).
    let args: Vec<String> = std::env::args().collect();
    let dev_storage = args.iter().any(|a| a == "--dev-storage");
    let size = args.iter().find_map(|a| a.strip_prefix("--size=")).and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some([w.parse::<f32>().ok()?, h.parse::<f32>().ok()?])
    });
    let options = eframe::NativeOptions {
        viewport: viewport
            .with_title("SlopComp")
            .with_inner_size(size.unwrap_or([1180.0, 820.0]))
            .with_min_inner_size([720.0, 520.0])
            .with_drag_and_drop(true),
        persist_window: size.is_none(),
        ..Default::default()
    };
    // The app id stays "anime-comp" so the saved setup (eframe storage) carries over.
    let app_id = if dev_storage { "anime-comp-dev" } else { "anime-comp" };
    eframe::run_native(app_id, options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
