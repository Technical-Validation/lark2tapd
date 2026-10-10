//! 下拉框按稳定值恢复选中项，标签只负责展示；切换项目必须清空旧处理人。
use super::*;
use gpui_kit::component::select::{SearchableVec, SelectItem, SelectState};

#[derive(Clone)]
pub(super) struct Choice {
    pub id: String,
    pub label: String,
}
impl SelectItem for Choice {
    type Value = String;
    fn title(&self) -> SharedString {
        self.label.clone().into()
    }
    fn value(&self) -> &String {
        &self.id
    }
}
pub(super) type ChoiceList = SearchableVec<Choice>;
pub(super) type ChoiceState = SelectState<ChoiceList>;

pub(super) fn selected(input: &Entity<ChoiceState>, cx: &App) -> String {
    input.read(cx).selected_value().cloned().unwrap_or_default()
}
fn replace(
    input: &Entity<ChoiceState>,
    mut options: Vec<Choice>,
    value: &str,
    fallback: String,
    window: &mut Window,
    cx: &mut App,
) {
    // 已有映射不因目录暂未加载而变成空值；此占位明确标明未缓存，提交仍校验真实成员缓存。
    if !value.is_empty() && !options.iter().any(|o| o.id == value) {
        options.push(Choice {
            id: value.into(),
            label: fallback,
        });
    }
    input.update(cx, |state, cx| {
        state.set_items(SearchableVec::new(options), window, cx);
        state.set_selected_value(&value.to_owned(), window, cx);
        cx.notify();
    });
}
impl Workspace {
    pub(super) fn fill_projects(
        &self,
        settings: bool,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| {
                s.directory
                    .workspaces
                    .iter()
                    .map(|p| Choice {
                        id: p.id.clone(),
                        label: p.name.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let input = if settings {
            &self.settings_workspace
        } else {
            &self.workspace
        };
        replace(
            input,
            options,
            value,
            format!(
                "{} ({value})",
                self.tr(cx, "未缓存项目空间", "Uncached workspace")
            ),
            window,
            cx,
        );
    }
    pub(super) fn fill_members(
        &self,
        settings: bool,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = selected(
            if settings {
                &self.settings_workspace
            } else {
                &self.workspace
            },
            cx,
        );
        let options = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|s| s.directory.members.get(&workspace))
            .map(|cache| {
                cache
                    .items
                    .iter()
                    .map(|m| Choice {
                        id: m.account.clone(),
                        label: format!("{} ({})", m.name, m.account),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let input = if settings {
            &self.settings_owner
        } else {
            &self.owner
        };
        replace(
            input,
            options,
            value,
            format!(
                "{} ({value})",
                self.tr(cx, "未缓存处理人", "Uncached owner")
            ),
            window,
            cx,
        );
    }
    pub(super) fn sync_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| s.directory.clone());
        let same = self
            .last_directory
            .as_ref()
            .zip(directory.as_ref())
            .is_some_and(|(a, b)| {
                a.workspaces == b.workspaces
                    && a.members.len() == b.members.len()
                    && a.members.iter().all(|(id, c)| {
                        b.members
                            .get(id)
                            .is_some_and(|other| other.items == c.items)
                    })
            });
        if same {
            self.last_directory = directory;
            return;
        }
        self.last_directory = directory;
        for settings in [false, true] {
            let project = selected(
                if settings {
                    &self.settings_workspace
                } else {
                    &self.workspace
                },
                cx,
            );
            let owner = selected(
                if settings {
                    &self.settings_owner
                } else {
                    &self.owner
                },
                cx,
            );
            self.fill_projects(settings, &project, window, cx);
            self.fill_members(settings, &owner, window, cx);
        }
        let id = selected(&self.prefix_workspace, cx);
        self.fill_prefix_workspace(&id, window, cx);
    }
    pub(super) fn project_changed(
        &mut self,
        settings: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.fill_members(settings, "", window, cx);
        let id = selected(
            if settings {
                &self.settings_workspace
            } else {
                &self.workspace
            },
            cx,
        );
        if !settings {
            self.fill_prefixes("", window, cx);
        }
        if !id.is_empty() {
            self.send(Job::LoadMembers(id), cx);
        }
        cx.notify();
    }
}

impl Workspace {
    pub(super) fn fill_prefixes(&self, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = selected(&self.workspace, cx);
        let values = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|s| s.settings.workspace_prefixes.get(&workspace))
            .cloned()
            .unwrap_or_default();
        replace(
            &self.prefix,
            values
                .into_iter()
                .map(|id| Choice {
                    label: format!("【{id}】"),
                    id,
                })
                .collect(),
            value,
            format!("【{value}】"),
            window,
            cx,
        );
    }
    pub(super) fn fill_prefix_workspace(
        &self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .map(|s| {
                s.directory
                    .workspaces
                    .iter()
                    .map(|w| Choice {
                        id: w.id.clone(),
                        label: w.name.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        replace(
            &self.prefix_workspace,
            options,
            value,
            format!(
                "{} ({value})",
                self.tr(cx, "未缓存项目空间", "Uncached workspace")
            ),
            window,
            cx,
        );
    }
    pub(super) fn load_prefix_form(&self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = selected(&self.prefix_workspace, cx);
        let text = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|s| s.settings.workspace_prefixes.get(&workspace))
            .map(|v| v.join("\n"))
            .unwrap_or_default();
        self.prefix_options
            .update(cx, |s, cx| s.set_value(text, window, cx));
        cx.notify();
    }
}
