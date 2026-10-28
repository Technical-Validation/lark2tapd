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

// UI 选择键包含种类，不把空类型 ID 或跨种类同 ID 当作同一选项。
pub(super) fn type_key(kind: &str, id: &str) -> String {
    if kind == "story" && id.is_empty() {
        String::new()
    } else {
        format!("{kind}:{id}")
    }
}

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
    pub(super) fn fill_workitem_types(
        &self,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = selected(&self.workspace, cx);
        let options: Vec<Choice> = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|s| s.directory.workitem_types.get(&workspace))
            .map(|items| {
                items
                    .iter()
                    .map(|item| Choice {
                        id: type_key(&item.kind, &item.id),
                        label: item.name.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        // 目录已缓存时清除失效选择；绑定目标保留其身份，不让刷新改写历史映射。
        let model = self.model.read(cx);
        let snapshot = model.snapshot.as_ref();
        let bound = snapshot.is_some_and(|s| {
            s.records
                .iter()
                .any(|r| Some(&r.source.guid) == self.selected.as_ref() && r.target.is_some())
        });
        let value = if !bound
            && snapshot.is_some_and(|s| s.directory.workitem_types.contains_key(&workspace))
            && !options.iter().any(|option: &Choice| option.id == value)
        {
            ""
        } else {
            value
        };
        replace(
            &self.workitem_type,
            options,
            value,
            self.tr(cx, "类型未加载或已不可用", "Type not loaded or unavailable")
                .into(),
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
                a.workitem_types == b.workitem_types
                    && a.workspaces == b.workspaces
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
        let type_id = selected(&self.workitem_type, cx);
        self.fill_workitem_types(&type_id, window, cx);
        let id = selected(&self.prefix_workspace, cx);
        self.fill_prefix_workspace(&id, window, cx);
    }
    pub(super) fn save_destination(&self, cx: &mut Context<Self>) {
        self.preference(
            Preference::DefaultDestination {
                workspace: selected(&self.settings_workspace, cx),
                owner: selected(&self.settings_owner, cx),
            },
            cx,
        );
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
            self.fill_workitem_types("", window, cx);
        } else if self.settings_loaded {
            // 切换空间与清空旧处理人一起保存，避免跨空间沿用默认处理人。
            self.save_destination(cx);
        }
        if !id.is_empty() {
            if !settings {
                self.send(Job::LoadWorkitemTypes(id.clone()), cx);
            }
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
                    label: id.clone(),
                    id,
                })
                .collect(),
            value,
            value.to_owned(),
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
    pub(super) fn load_prefix_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // 切换项目或确认保存后重新开始；已有前缀只从持久化列表展示。
        self.pending_prefix = None;
        self.new_prefix_open = false;
        self.prefix_options
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }
}
