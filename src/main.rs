#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]
#![cfg_attr(mobile_ui, allow(dead_code, unused_mut))]

mod app;
mod audio;
mod config;
mod cover;
mod dsp;
mod meta;
mod platform;
mod playlist;
mod remote;
mod plugins;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Meu Player")
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([760.0, 460.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native("Meu Player", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))))
}
