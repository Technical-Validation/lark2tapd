//! 应用调色板统一驱动原生组件、菜单和 Markdown 编辑器。
use gpui_kit::{
    App,
    component::{Theme, ThemeMode, theme::ThemeColor},
    px, rgb,
};

// 每次先加载 Kit 的完整模式，防止暗色覆盖残留在浅色或系统模式中。
pub(super) fn apply_theme(mode: &str, cx: &mut App) {
    match mode {
        "light" => Theme::change(ThemeMode::Light, None, cx),
        "dark" => Theme::change(ThemeMode::Dark, None, cx),
        _ => Theme::sync_system_appearance(None, cx),
    }
    Theme::update(cx, |theme| {
        // 马尔斯绿的深浅变体保证小字号按钮文字对比度，焦点环保留品牌原色。
        let dark = theme.mode == ThemeMode::Dark;
        if dark {
            apply_dark_palette(&mut theme.colors);
        }
        theme.colors.primary = rgb(if dark { 0x59c3bc } else { 0x006764 }).into();
        theme.colors.primary_foreground = rgb(if dark { 0x171b1b } else { 0xffffff }).into();
        theme.colors.ring = rgb(0x008c8c).into();
        // Kit 的按钮采用独立令牌，不能只改 primary；同时覆盖交互状态和次级操作。
        theme.colors.button_primary = theme.colors.primary;
        theme.colors.button_primary_foreground = theme.colors.primary_foreground;
        theme.colors.button_primary_hover = rgb(if dark { 0x71d1ca } else { 0x007873 }).into();
        theme.colors.button_primary_active = rgb(if dark { 0x46aaa4 } else { 0x00534f }).into();
        theme.colors.button_secondary = rgb(if dark { 0x222524 } else { 0xedf7f5 }).into();
        theme.colors.button_secondary_foreground = theme.colors.foreground;
        theme.colors.button_secondary_hover = rgb(if dark { 0x2b302e } else { 0xd6efea }).into();
        theme.colors.button_secondary_active = rgb(if dark { 0x363d39 } else { 0xbfe5de }).into();
        theme.colors.secondary_foreground = theme.colors.foreground;
        theme.colors.accent_foreground = theme.colors.foreground;
        theme.colors.link = theme.colors.foreground;
        theme.colors.link_hover = theme.colors.foreground;
        theme.colors.link_active = theme.colors.foreground;
        theme.font_size = px(14.);
        theme.radius = px(7.);
    });
}

fn apply_dark_palette(c: &mut ThemeColor) {
    // 中性层级用于所有容器和常规交互；品牌色仅强调主要操作与焦点。
    c.background = rgb(0x191b1b).into();
    c.sidebar = rgb(0x191b1b).into();
    c.list = rgb(0x191b1b).into();
    c.table = rgb(0x191b1b).into();
    c.tab_bar = rgb(0x191b1b).into();
    c.title_bar = rgb(0x191b1b).into();
    c.status_bar = rgb(0x191b1b).into();
    c.muted = rgb(0x222524).into();
    c.popover = rgb(0x222524).into();
    c.accordion = rgb(0x222524).into();
    c.group_box = rgb(0x222524).into();
    c.button = rgb(0x222524).into();
    c.secondary = rgb(0x222524).into();
    c.list_even = rgb(0x222524).into();
    c.list_head = rgb(0x222524).into();
    c.table_even = rgb(0x222524).into();
    c.table_head = rgb(0x222524).into();
    c.table_foot = rgb(0x222524).into();
    c.tab = rgb(0x222524).into();
    c.tab_bar_segmented = rgb(0x222524).into();
    c.description_list_label = rgb(0x222524).into();
    c.accent = rgb(0x2b302e).into();
    c.button_hover = rgb(0x2b302e).into();
    c.secondary_hover = rgb(0x2b302e).into();
    c.list_hover = rgb(0x2b302e).into();
    c.table_hover = rgb(0x2b302e).into();
    c.sidebar_accent = rgb(0x2b302e).into();
    c.button_active = rgb(0x363d39).into();
    c.secondary_active = rgb(0x363d39).into();
    c.list_active = rgb(0x363d39).into();
    c.table_active = rgb(0x363d39).into();
    c.tab_active = rgb(0x363d39).into();
    c.skeleton = rgb(0x363d39).into();
    c.switch = rgb(0x363d39).into();
    c.foreground = rgb(0xeceeed).into();
    c.popover_foreground = rgb(0xeceeed).into();
    c.group_box_foreground = rgb(0xeceeed).into();
    c.button_foreground = rgb(0xeceeed).into();
    c.sidebar_foreground = rgb(0xeceeed).into();
    c.sidebar_accent_foreground = rgb(0xeceeed).into();
    c.tab_active_foreground = rgb(0xeceeed).into();
    c.table_head_foreground = rgb(0xeceeed).into();
    c.table_foot_foreground = rgb(0xeceeed).into();
    c.muted_foreground = rgb(0xb0b8b4).into();
    c.tab_foreground = rgb(0xb0b8b4).into();
    c.description_list_label_foreground = rgb(0xb0b8b4).into();
    c.border = rgb(0x454d49).into();
    c.input = rgb(0x454d49).into();
    c.sidebar_border = rgb(0x454d49).into();
    c.title_bar_border = rgb(0x454d49).into();
    c.status_bar_border = rgb(0x454d49).into();
    c.window_border = rgb(0x454d49).into();
    c.table_row_border = rgb(0x454d49).into();
    c.primary = rgb(0x59c3bc).into();
    c.caret = rgb(0x59c3bc).into();
    c.list_active_border = rgb(0x59c3bc).into();
    c.table_active_border = rgb(0x59c3bc).into();
    c.sidebar_primary = rgb(0x59c3bc).into();
    c.slider_bar = rgb(0x59c3bc).into();
    c.progress_bar = rgb(0x59c3bc).into();
    c.drag_border = rgb(0x59c3bc).into();
    c.sidebar_primary_foreground = rgb(0x171b1b).into();
    c.primary_hover = rgb(0x71d1ca).into();
    c.primary_active = rgb(0x46aaa4).into();
    c.selection = rgb(0x497b73).into();
    c.scrollbar_thumb = rgb(0x5d6963).into();
    c.scrollbar_thumb_hover = rgb(0x7a8880).into();
    // 语义色保留绿、琥珀和红的区别；按钮文字使用暗色以满足小字号对比度。
    c.success = rgb(0x79c995).into();
    c.button_success = c.success;
    c.success_hover = rgb(0x91d7a9).into();
    c.button_success_hover = c.success_hover;
    c.success_active = rgb(0x64b780).into();
    c.button_success_active = c.success_active;
    c.success_foreground = rgb(0x191b1b).into();
    c.button_success_foreground = c.success_foreground;
    c.warning = rgb(0xe5b567).into();
    c.button_warning = c.warning;
    c.warning_hover = rgb(0xefc780).into();
    c.button_warning_hover = c.warning_hover;
    c.warning_active = rgb(0xcfa052).into();
    c.button_warning_active = c.warning_active;
    c.warning_foreground = rgb(0x191b1b).into();
    c.button_warning_foreground = c.warning_foreground;
    c.danger = rgb(0xef9292).into();
    c.button_danger = c.danger;
    c.danger_hover = rgb(0xf5aaaa).into();
    c.button_danger_hover = c.danger_hover;
    c.danger_active = rgb(0xdb7c7c).into();
    c.button_danger_active = c.danger_active;
    c.danger_foreground = rgb(0x191b1b).into();
    c.button_danger_foreground = c.danger_foreground;
}

#[cfg(test)]
mod tests {
    use super::apply_theme;
    use gpui_kit::{component::Theme, *};
    use std::prelude::v1::test;

    #[gpui_kit::test]
    fn dark_surfaces_and_controls_share_the_neutral_palette(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            apply_theme("dark", cx);
            let t = Theme::global(cx);
            for (actual, expected) in [
                (t.background, 0x191b1b),
                (t.foreground, 0xeceeed),
                (t.muted_foreground, 0xb0b8b4),
                (t.muted, 0x222524),
                (t.popover, 0x222524),
                (t.button_secondary, 0x222524),
                (t.button_secondary_hover, 0x2b302e),
                (t.button_secondary_active, 0x363d39),
                (t.accent, 0x2b302e),
                (t.primary, 0x59c3bc),
            ] {
                assert_eq!(actual, Hsla::from(rgb(expected)));
            }
            assert_eq!(t.tokens.background.color, t.background);
            assert_eq!(t.tokens.popover.color, t.popover);
            assert_eq!(t.tokens.button_secondary.color, t.button_secondary);
        });
    }

    // 使用 WCAG 相对亮度校验实际令牌，避免中性色文字和语义按钮在暗色下失去可读性。
    fn contrast(a: Hsla, b: Hsla) -> f32 {
        fn luminance(color: Hsla) -> f32 {
            let color = Rgba::from(color);
            let linear = |v: f32| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[gpui_kit::test]
    fn dark_text_and_semantic_buttons_have_readable_contrast(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            apply_theme("dark", cx);
            let t = Theme::global(cx);
            for background in [
                t.background,
                t.muted,
                t.popover,
                t.accent,
                t.button_secondary_active,
            ] {
                assert!(contrast(t.foreground, background) >= 4.5);
                assert!(contrast(t.muted_foreground, background) >= 4.5);
            }
            for (foreground, backgrounds) in [
                (
                    t.button_primary_foreground,
                    [
                        t.button_primary,
                        t.button_primary_hover,
                        t.button_primary_active,
                    ],
                ),
                (
                    t.button_success_foreground,
                    [
                        t.button_success,
                        t.button_success_hover,
                        t.button_success_active,
                    ],
                ),
                (
                    t.button_warning_foreground,
                    [
                        t.button_warning,
                        t.button_warning_hover,
                        t.button_warning_active,
                    ],
                ),
                (
                    t.button_danger_foreground,
                    [
                        t.button_danger,
                        t.button_danger_hover,
                        t.button_danger_active,
                    ],
                ),
            ] {
                for background in backgrounds {
                    assert!(
                        contrast(foreground, background) >= 4.5,
                        "contrast {}",
                        contrast(foreground, background)
                    );
                }
            }
            assert_ne!(t.success, t.primary);
            assert_ne!(t.warning, t.primary);
            assert_ne!(t.danger, t.primary);
        });
    }

    #[gpui_kit::test]
    fn switching_modes_restores_every_color_and_resolved_token(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        cx.update(|cx| {
            apply_theme("light", cx);
            let light = Theme::global(cx).clone();
            apply_theme("dark", cx);
            let dark = Theme::global(cx).clone();
            apply_theme("light", cx);
            assert_eq!(Theme::global(cx).colors, light.colors);
            assert_eq!(Theme::global(cx).tokens, light.tokens);
            assert_eq!(Theme::global(cx).font_size, px(14.));
            assert_eq!(Theme::global(cx).radius, px(7.));
            apply_theme("dark", cx);
            assert_eq!(Theme::global(cx).colors, dark.colors);
            assert_eq!(Theme::global(cx).tokens, dark.tokens);
        });
    }
}
