#![forbid(unsafe_code)]

use std::path::PathBuf;

use clap::Parser;
use gpui::{
    prelude::*, px, size, App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};

use rsmpeg_gui::i18n::{strings, Locale};
use rsmpeg_gui::ui::PlayerView;

#[derive(Debug, Parser)]
#[command(name = "rsmpeg-gui", version, about = "rsmpeg GPUI 桌面影音播放器")]
struct Cli {
    /// 介面語言（zh 或 en）
    #[arg(long, default_value = "zh")]
    lang: String,
    /// 啟動時載入並加入播放清單的媒體檔案
    files: Vec<PathBuf>,
}

fn parse_locale(lang: &str) -> Locale {
    if lang.to_ascii_lowercase().starts_with("en") {
        Locale::EnUs
    } else {
        Locale::ZhTw
    }
}

fn main() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    // Register built-in container demuxers and codecs for rsmpeg probe & native pipeline.
    rsmpeg::format::format_registry::register_builtin_formats();
    rsmpeg::codec::codec_registry::register_builtin_codecs();

    let cli = Cli::parse();
    let initial_locale = parse_locale(&cli.lang);
    let app_title = strings(initial_locale).app_title;

    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1120.0), px(680.0)), cx);
        let initial_files = cli.files.clone();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(720.0), px(460.0))),
                titlebar: Some(TitlebarOptions {
                    title: Some(app_title.into()),
                    ..Default::default()
                }),
                focus: true,
                show: true,
                ..Default::default()
            },
            move |window, cx| {
                cx.new(|cx| PlayerView::new(initial_files, initial_locale, window, cx))
            },
        )
        .expect("failed to open rsmpeg-gui window");

        cx.activate(true);
    });
}
