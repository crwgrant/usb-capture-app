mod app;
mod capture;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::app::Viewer;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    application().with_assets(assets::Assets).run(|cx| {
        init(cx);
        Theme::change(ThemeMode::Dark, None, cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
            window_min_size: Some(size(px(720.), px(480.))),
            titlebar: Some(TitlebarOptions {
                title: Some("ShadowCast".into()),
                ..Default::default()
            }),
            app_id: Some("shadowcast-app".into()),
            focus: true,
            show: true,
            inactive_frame_interval: None,
            low_power_frame_interval: None,
            ..Default::default()
        };

        open_window(options, cx, |window, cx| {
            cx.new(|cx| Viewer::new(window, cx))
        })
        .expect("Failed to open window");
    });
}
