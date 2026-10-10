#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[cfg(not(target_os = "macos"))]
compile_error!("Lark → TAPD currently supports macOS only.");

mod i18n;
mod native;
mod ui;
mod worker;

use gpui_kit::*;
use std::path::PathBuf;
use worker::Mode;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help") {
        println!(
            "lark2tapd [--mock | --live] [--data-dir PATH]\nDefault: isolated mock data. Credentials are stored in macOS Keychain."
        );
        return;
    }
    let mode = if args.iter().any(|a| a == "--live") {
        Mode::Live
    } else {
        Mode::Mock
    };
    let root = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is required"))
                .join("Library/Application Support/Lark2Tapd")
        });
    let worker = worker::start(root, mode);
    let app = application()
        .with_assets(assets::Assets)
        .with_quit_mode(QuitMode::Explicit);
    app.on_reopen(ui::show_window);
    app.run(move |cx| {
        gpui_kit::init(cx);
        ui::start(worker, mode, cx);
    });
}
