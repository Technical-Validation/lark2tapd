//! 仅在焦点切换时露出设置控件；用户手动滚动不会被当前焦点拉回。
use super::*;

struct State {
    scope: FocusHandle,
    last_focus: Option<FocusHandle>,
}

#[derive(IntoElement)]
struct FocusRegion {
    id: ElementId,
    scroll: ScrollHandle,
    content: AnyElement,
}
impl RenderOnce for FocusRegion {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state((self.id, "focus-scroll"), cx, |_, cx| State {
            scope: cx.focus_handle().tab_stop(false),
            last_focus: None,
        });
        let scope = state.read(cx).scope.clone();
        let observed_scope = scope.clone();
        div()
            .on_children_prepainted(move |bounds, window, cx| {
                let focused = window.focused(cx);
                let inside = observed_scope.contains_focused(window, cx);
                let changed = state.update(cx, |state, _| {
                    let next = if inside { focused } else { None };
                    let changed = state.last_focus != next;
                    state.last_focus = next;
                    changed && inside
                });
                if !changed {
                    return;
                }
                let Some(bounds) = bounds.first() else {
                    return;
                };
                let viewport = self.scroll.bounds();
                if viewport.size.height <= px(0.) {
                    return;
                }
                let delta = if bounds.top() < viewport.top() {
                    viewport.top() - bounds.top()
                } else if bounds.bottom() > viewport.bottom() {
                    viewport.bottom() - bounds.bottom()
                } else {
                    px(0.)
                };
                if delta != px(0.) {
                    let scroll = self.scroll.clone();
                    let offset = scroll.offset() + point(px(0.), delta);
                    window.on_next_frame(move |window, _| {
                        scroll.set_offset(offset);
                        window.refresh();
                    });
                }
            })
            .track_focus(&scope)
            .child(self.content)
    }
}

impl Workspace {
    pub(super) fn focus_region(
        &self,
        id: impl Into<ElementId>,
        content: impl IntoElement,
    ) -> impl IntoElement {
        FocusRegion {
            id: id.into(),
            scroll: self.settings_scroll.clone(),
            content: content.into_any_element(),
        }
    }
}
