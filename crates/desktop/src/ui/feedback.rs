//! 反馈不占固定底栏：成功短暂显示，最新未确认错误跨窗口保留到用户关闭提示。
use super::*;
use gpui_kit::component::{
    WindowExt,
    notification::{Notification, NotificationDelivery},
};

pub(super) fn present(feedback: Feedback, cx: &mut App) {
    let Some(shell) = cx.try_global::<Shell>() else {
        return;
    };
    let model = shell.model.downgrade();
    let language = shell.model.read(cx).language().to_owned();
    let setup = if feedback.error {
        let message = feedback.message.to_lowercase();
        if message.contains("tapd") {
            Some(CliTool::Tapd)
        } else if message.contains("lark") || message.contains("飞书") {
            Some(CliTool::Lark)
        } else {
            None
        }
    } else {
        None
    };
    let handles = [shell.window, shell.settings_window];
    // 优先活动窗口，其次主窗口。应用不在前台时仍将提示留在已有窗口里。
    let live: Vec<_> = handles
        .into_iter()
        .flatten()
        .filter_map(|handle| {
            handle
                .update(cx, |_, window, _| window.is_window_active())
                .ok()
                .map(|active| (handle, active))
        })
        .collect();
    let Some((handle, _)) = live.iter().find(|(_, active)| *active).or(live.first()) else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        let note = if feedback.error {
            let message = feedback.message.clone();
            Notification::error(feedback.message)
                .id1::<Feedback>("error")
                .autohide(false)
                .on_close(move |_, cx| {
                    let _ = model.update(cx, |model, cx| {
                        if model.pending_error.as_ref() == Some(&message) {
                            model.pending_error = None;
                            cx.notify();
                        }
                    });
                })
        } else {
            // 成功与错误分开标识，主题/语言保存等成功不能抹掉未确认的失败。
            Notification::success(feedback.message).id1::<Feedback>("success")
        };
        // 留出顶栏齿轮与刷新入口，不让操作提示挡住全局导航。
        let note = if let Some(tool) = setup {
            note.action(move |_, _, cx| {
                Button::new("configure-error-service")
                    .ghost()
                    .text_color(cx.theme().primary)
                    .label(if tool == CliTool::Lark {
                        text(&language, "配置飞书", "Configure Lark")
                    } else {
                        text(&language, "配置 TAPD", "Configure TAPD")
                    })
                    .on_click(move |_, _, cx| settings_navigation::open(tool, cx))
            })
        } else {
            note
        };
        window.push_notification(
            note.placement(Anchor::BottomRight)
                .delivery(NotificationDelivery::InApp),
            cx,
        );
    });
}

pub(super) fn replay(cx: &mut App) {
    let pending = cx
        .try_global::<Shell>()
        .and_then(|shell| shell.model.read(cx).pending_error.clone());
    if let Some(message) = pending {
        // 入口可能正在借用窗口；延迟到释放后选定唯一投递窗口。
        cx.defer(move |cx| {
            present(
                Feedback {
                    message,
                    error: true,
                },
                cx,
            )
        });
    }
}
