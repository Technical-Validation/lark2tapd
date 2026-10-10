//! AppKit 菜单栏复用 GPUI 的事件循环，回调只投递消息，不在原生回调中访问应用状态。
use crate::i18n::text;
use anyhow::Result;
use tray_icon::{
    TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem},
};

#[derive(Clone, Copy)]
pub enum NativeAction {
    Open,
    Pending,
    Refresh,
    TogglePolling,
    Quit,
}

pub struct StatusMenu {
    tray: TrayIcon,
    open: MenuItem,
    pending: MenuItem,
    refresh: MenuItem,
    polling: MenuItem,
    quit: MenuItem,
}

impl StatusMenu {
    pub fn new(sender: async_channel::Sender<NativeAction>) -> Result<Self> {
        let menu = Menu::new();
        let open = MenuItem::new("打开 Lark → TAPD", true, None);
        let pending = MenuItem::new("未同步任务", true, None);
        let refresh = MenuItem::new("立即检查", true, None);
        let polling = MenuItem::new("开启自动检查", true, None);
        let quit = MenuItem::new("退出", true, None);
        for item in [&open, &pending, &refresh, &polling, &quit] {
            menu.append(item)?;
        }
        let ids = [
            open.id().clone(),
            pending.id().clone(),
            refresh.id().clone(),
            polling.id().clone(),
            quit.id().clone(),
        ];
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(index) = ids.iter().position(|id| *id == event.id) {
                let action = [
                    NativeAction::Open,
                    NativeAction::Pending,
                    NativeAction::Refresh,
                    NativeAction::TogglePolling,
                    NativeAction::Quit,
                ][index];
                let _ = sender.try_send(action);
            }
        }));
        let tray = TrayIconBuilder::new()
            .with_title("L2T")
            .with_tooltip("Lark → TAPD")
            .with_menu(Box::new(menu))
            .build()?;
        Ok(Self {
            tray,
            open,
            pending,
            refresh,
            polling,
            quit,
        })
    }

    pub fn update(&self, language: &str, count: usize, polling: bool) {
        self.tray.set_title(Some(&format!("L2T {count}")));
        self.open
            .set_text(text(language, "打开 Lark → TAPD", "Open Lark → TAPD"));
        self.pending.set_text(format!(
            "{} ({count})",
            text(language, "未同步任务", "Unimported tasks")
        ));
        self.refresh
            .set_text(text(language, "立即检查", "Check now"));
        self.polling.set_text(if polling {
            text(language, "暂停自动检查", "Pause auto-check")
        } else {
            text(language, "开启自动检查", "Enable auto-check")
        });
        self.quit.set_text(text(language, "退出", "Quit"));
    }
}

pub fn notify_new(language: &str, count: usize) {
    let title = text(language, "发现新的飞书任务", "New Lark tasks").to_owned();
    let body = if language == "en" {
        format!("{count} new tasks. Open Unimported from the menu bar to review.")
    } else {
        format!("发现 {count} 个新任务，可从菜单栏打开未同步列表。")
    };
    std::thread::spawn(move || {
        // 只使用本应用标识，不冒充 Finder；未打包启动时系统通知可能不可用。
        if mac_notification_sys::set_application("com.roxi.lark2tapd").is_ok() {
            let _ = mac_notification_sys::send_notification(&title, None, &body, None);
        }
    });
}
