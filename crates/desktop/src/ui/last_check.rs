//! 检查时间只影响展示，不触发远端轮询。
use super::*;
use chrono::{DateTime, Utc};

fn relative_time(value: Option<&str>, language: &str, now: DateTime<Utc>) -> String {
    let Some(value) = value else {
        return text(language, "尚未检查", "Not checked yet").into();
    };
    let Ok(checked) = DateTime::parse_from_rfc3339(value) else {
        return text(language, "检查时间不可用", "Check time unavailable").into();
    };
    let seconds = now.signed_duration_since(checked).num_seconds().max(0);
    if seconds < 60 {
        return text(language, "几秒前", "A few seconds ago").into();
    }
    // 紧凑时长按 30 天折算月，不混合多个单位；未来时间按刚检查处理。
    let (count, zh, en) = match seconds {
        60..3600 => (seconds / 60, "分钟", "minute"),
        3600..86400 => (seconds / 3600, "小时", "hour"),
        86400..2592000 => (seconds / 86400, "天", "day"),
        _ => (seconds / 2592000, "个月", "month"),
    };
    if language == "zh-CN" {
        format!("{count} {zh}前")
    } else {
        format!("{count} {en}{} ago", if count == 1 { "" } else { "s" })
    }
}

// 时间标签独立刷新，避免定时重绘整个 Markdown 编辑器或触发网络请求。
pub(super) struct LastCheck {
    model: Entity<Model>,
    timer: Option<Task<()>>,
}

impl LastCheck {
    pub(super) fn new(model: Entity<Model>, cx: &mut Context<Self>) -> Self {
        cx.observe(&model, |_, _, cx| cx.notify()).detach();
        Self { model, timer: None }
    }
}

impl Render for LastCheck {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 仅真正显示过的标签启动计时；任务由视图拥有，关闭窗口即取消。
        if self.timer.is_none() {
            self.timer = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(15))
                        .await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        }
        let model = self.model.read(cx);
        let checked = model
            .snapshot
            .as_ref()
            .and_then(|s| s.last_checked.as_deref());
        let label = relative_time(checked, model.language(), Utc::now());
        let full = checked
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S %:z")
                    .to_string()
            });
        div()
            .id("last-check")
            .debug_selector(|| "last-check".into())
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .when_some(full, |d, full| {
                d.tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(full.clone()).build(window, cx)
                })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::prelude::v1::test;

    // 单位临界点、旧数据及机器时钟偏差不能泄露原始时间戳或显示负数。
    #[test]
    fn relative_check_time_uses_one_unit_and_handles_boundaries() {
        let now = "2026-10-13T12:00:00Z".parse::<DateTime<Utc>>().unwrap();
        for (seconds, zh, en) in [
            (-60, "几秒前", "A few seconds ago"),
            (0, "几秒前", "A few seconds ago"),
            (59, "几秒前", "A few seconds ago"),
            (60, "1 分钟前", "1 minute ago"),
            (3599, "59 分钟前", "59 minutes ago"),
            (3600, "1 小时前", "1 hour ago"),
            (86399, "23 小时前", "23 hours ago"),
            (86400, "1 天前", "1 day ago"),
            (2591999, "29 天前", "29 days ago"),
            (2592000, "1 个月前", "1 month ago"),
            (5184000, "2 个月前", "2 months ago"),
            (31536000, "12 个月前", "12 months ago"),
        ] {
            let checked = (now - chrono::Duration::seconds(seconds)).to_rfc3339();
            assert_eq!(relative_time(Some(&checked), "zh-CN", now), zh);
            assert_eq!(relative_time(Some(&checked), "en", now), en);
        }
        assert_eq!(relative_time(None, "zh-CN", now), "尚未检查");
        assert_eq!(
            relative_time(Some("invalid"), "en", now),
            "Check time unavailable"
        );
        assert_eq!(
            relative_time(Some("2026-10-13T19:59:00+08:00"), "zh-CN", now),
            "1 分钟前"
        );
    }
}
