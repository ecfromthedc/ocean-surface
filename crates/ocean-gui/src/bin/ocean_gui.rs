use gpui::{
    App, AppContext, Application, Bounds, SharedString, TitlebarOptions, WindowBounds,
    WindowOptions, px, size,
};
use ocean_gui::shell::{OceanGuiShell, ShellAssets};

fn main() {
    Application::new()
        .with_assets(ShellAssets::new())
        .run(|cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1440.0), px(900.0)), cx);
            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(SharedString::new_static("Ocean GUI")),
                    ..Default::default()
                }),
                window_min_size: Some(size(px(1180.0), px(760.0))),
                ..Default::default()
            };

            cx.open_window(window_options, |window, cx| {
                cx.new(|cx| OceanGuiShell::new(window, cx))
            })
            .expect("failed to open Ocean GUI GPUI window");
            cx.activate(true);
        });
}
