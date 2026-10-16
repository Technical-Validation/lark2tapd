#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[cfg(not(target_os = "macos"))]
compile_error!("Lark → TAPD currently supports macOS only.");

mod console;
mod i18n;
mod native;
mod startup;
mod ui;
mod worker;

use gpui_kit::*;
use std::path::PathBuf;

fn main() {
    console::install_panic_hook();
    let args: Vec<String> = std::env::args().collect();
    let mock_env = std::env::var_os("LARK2TAPD_MOCK");
    let options = match startup::LaunchOptions::parse(
        &args[1..],
        mock_env
            .as_ref()
            .map(|value| value.to_string_lossy())
            .as_deref(),
    ) {
        Ok(options) => options,
        Err(error) => {
            console::failure("app.startup", &error);
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if options.help {
        println!(
            "lark2tapd [--data-dir PATH] [--help]\nLive connections only. Demo mode removed: unset LARK2TAPD_MOCK and remove --mock/--live. Credentials stay in macOS Keychain."
        );
        return;
    }
    console::event("app.lifecycle", "starting");
    let mode = worker::Mode::Live;
    let root = options.data_dir.unwrap_or_else(|| {
        PathBuf::from(std::env::var_os("HOME").expect("HOME is required"))
            .join("Library/Application Support/Lark2Tapd")
    });
    let worker = worker::start(root, mode);
    let app = application()
        .with_assets(assets::Assets)
        .with_quit_mode(QuitMode::Explicit);
    app.on_reopen(ui::show_window);
    console::event("app.lifecycle", "event_loop_enter");
    app.run(move |cx| {
        gpui_kit::init(cx);
        cx.on_app_quit(|_| {
            console::event("app.lifecycle", "quitting");
            async {}
        })
        .detach();
        ui::start(worker, mode, cx);
    });
    console::event("app.lifecycle", "event_loop_returned");
}
