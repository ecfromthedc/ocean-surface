mod app;

use app::OceanGuiApp;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Ocean GUI")
            .with_inner_size([1400.0, 900.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Ocean GUI",
        options,
        Box::new(|_cc| Ok(Box::new(OceanGuiApp::new()))),
    )
}
