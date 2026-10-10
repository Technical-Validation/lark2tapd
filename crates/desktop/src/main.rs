#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[cfg(not(target_os = "macos"))]
compile_error!("Lark → TAPD currently supports macOS only.");

mod i18n;
mod native;
mod startup;
mod ui;
mod worker;

use gpui_kit::*;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help") {
        println!(
            "lark2tapd [--mock | --live] [--data-dir PATH]\nLARK2TAPD_MOCK=1 enables isolated demo mode. Default: live; CLI flags override environment. Credentials stay in macOS Keychain."
        );
        return;
    }
    let options = match startup::LaunchOptions::parse(
        &args[1..],
        std::env::var("LARK2TAPD_MOCK").ok().as_deref(),
    ) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let mode = options.mode;
    let root = options.data_dir.unwrap_or_else(|| {
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
