//! 任务列表、来源编辑和确认预览，保持写入入口显式可见。
use super::*;

impl Workspace {
    pub(super) fn task_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().to_lowercase();
        let model = self.model.read(cx);
        let records: Vec<_> = model
            .snapshot
            .as_ref()
            .map(|s| {
                s.records
                    .iter()
                    .filter(|r| {
                        self.filter.matches(r)
                            && (r.source.title.to_lowercase().contains(&query)
                                || r.source.description.to_lowercase().contains(&query))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let title = match self.filter {
            Filter::Pending => self.tr(cx, "未同步任务", "Unimported tasks"),
            Filter::Changed => self.tr(cx, "需要你确认", "Needs your attention"),
            Filter::Synced => self.tr(cx, "已同步任务", "Synced tasks"),
            _ => self.tr(cx, "全部任务", "All tasks"),
        };
        div()
            .flex()
            .flex_col()
            .w(px(332.))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(div().text_xl().font_semibold().child(title))
                    .child(
                        Input::new(&self.search)
                            .id("task-search")
                            .aria_label(self.tr(cx, "搜索任务", "Search tasks")),
                    ),
            )
            .child(
                div()
                    .id("task-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .pb_3()
                    .when(records.is_empty(), |d| {
                        d.child(
                            div()
                                .p_4()
                                .text_color(cx.theme().muted_foreground)
                                .child(self.tr(
                                    cx,
                                    "暂无任务。点击“立即检查”，或更换筛选条件。",
                                    "No tasks here. Check now or change the filter.",
                                )),
                        )
                    })
                    .children(records.into_iter().map(|record| {
                        let id = record.source.guid.clone();
                        let selected = self.selected.as_ref() == Some(&id);
                        div()
                            .id(SharedString::from(format!("task-{id}")))
                            .cursor_pointer()
                            .rounded_md()
                            .p_3()
                            .mb_2()
                            .border_1()
                            .border_color(if selected {
                                cx.theme().primary
                            } else {
                                transparent_black()
                            })
                            .when(selected, |d| d.bg(cx.theme().accent))
                            .hover(|d| d.bg(cx.theme().muted))
                            .child(
                                Button::new(SharedString::from(format!("select-{id}")))
                                    .label(record.source.title.clone())
                                    .ghost()
                                    .w_full()
                                    .on_click(cx.listener({
                                        let r = record.clone();
                                        move |this, _, window, cx| {
                                            this.select(r.clone(), window, cx)
                                        }
                                    })),
                            )
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(status_text(model.language(), record.status()))
                                    .child(record.source.due.clone().unwrap_or_else(|| {
                                        self.tr(cx, "无截止日期", "No due date").into()
                                    })),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {}",
                                        self.tr(cx, "创建人", "Created by"),
                                        person_name(
                                            record.source.metadata.creator.as_ref(),
                                            model.language()
                                        )
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {}",
                                        self.tr(cx, "负责人", "Assignees"),
                                        assignee_names(&record.source, model.language())
                                    )),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {}",
                                        self.tr(cx, "创建时间", "Created"),
                                        format_time(
                                            record.source.metadata.created_at.as_deref(),
                                            model.language()
                                        )
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {}",
                                        self.tr(cx, "截止时间", "Due"),
                                        format_time(
                                            record.source.metadata.due_at.as_deref(),
                                            model.language()
                                        )
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!(
                                        "{}: {}",
                                        self.tr(cx, "结束时间", "Completed"),
                                        format_time(
                                            record.source.metadata.completed_at.as_deref(),
                                            model.language()
                                        )
                                    )),
                            )
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.select(record.clone(), window, cx)
                                }),
                            )
                    })),
            )
    }
    pub(super) fn detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        if let Some(preview) = model.preview.clone() {
            return self.preview(preview, cx).into_any_element();
        }
        let record = self
            .selected
            .as_ref()
            .and_then(|id| {
                model
                    .snapshot
                    .as_ref()?
                    .records
                    .iter()
                    .find(|r| &r.source.guid == id)
            })
            .cloned();
        let Some(record) = record else {
            return div()
                .flex_1()
                .h_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .child(div().text_2xl().font_semibold().child(self.tr(
                    cx,
                    "从飞书开始，在 TAPD 跟进",
                    "Start in Lark. Follow through in TAPD.",
                )))
                .child(div().text_color(cx.theme().muted_foreground).child(self.tr(
                    cx,
                    "选择左侧任务，检查内容后再同步。",
                    "Select a task, review its contents, then import.",
                )))
                .into_any_element();
        };
        let busy = model.busy;
        let status = record.status();
        let guid = record.source.guid.clone();
        let is_uncertain = status == SyncStatus::Uncertain;
        let stale = self.edit_source.as_ref() != Some(&record.source);
        div().id("detail-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().p_6().flex().flex_col().gap_4()
            .child(div().flex().justify_between().items_center().child(div().text_xl().font_semibold().child(self.tr(cx,"任务详情","Task details"))).child(div().text_sm().text_color(cx.theme().primary).child(status_text(model.language(),status))))
            .child(div().flex().gap_2().child(Button::new("open-source").label(self.tr(cx,"在飞书中打开","Open in Lark")).ghost().disabled(record.source.url.is_empty()).on_click({let url=record.source.url.clone();move |_,_,cx|open_safe_url(&url,cx)}))
                .when_some(record.target.as_ref(),|d,t|d.child(Button::new("open-target").label(self.tr(cx,"在 TAPD 中打开","Open in TAPD")).ghost().disabled(t.url.is_empty()).on_click({let url=t.url.clone();move |_,_,cx|open_safe_url(&url,cx)}))))
            .when(record.source.completed,|d|d.child(div().text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"飞书任务已完成。TAPD 工作流状态不会被自动覆盖。","Completed in Lark. TAPD workflow status is not changed automatically."))))
            .when(stale,|d|d.child(div().flex().flex_col().gap_2().child(self.tr(cx,"来源已变化或草稿未加载。重新读取来源将丢弃当前编辑及已保存草稿。","Source changed or draft not loaded. Reload discards current edits and the saved draft.")).child(Button::new("reload-source").label(self.tr(cx,"丢弃草稿并重新读取","Discard draft and reload")).disabled(busy||is_uncertain).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|this.send(Job::OpenDraft(guid.clone(),true),cx)})))))
            .child(field(self.tr(cx,"标题","Title"),Input::new(&self.title).id("draft-title").aria_label(self.tr(cx,"标题","Title")).disabled(busy||is_uncertain)))
            .child(field(self.tr(cx,"内容","Description"),Textarea::new(&self.description).accessibility_id("draft-description").aria_label(self.tr(cx,"内容","Description")).h(px(210.)).disabled(busy||is_uncertain)))
            .child(div().flex().gap_3().child(field(self.tr(cx,"目标项目","Target project"),Select::new(&self.workspace).id("draft-workspace").placeholder(self.tr(cx,"选择项目","Select project")).search_placeholder(self.tr(cx,"搜索项目","Search projects")).disabled(busy||record.target.is_some()))).child(field(self.tr(cx,"TAPD 处理人","TAPD assignee"),Select::new(&self.owner).id("draft-owner").placeholder(self.tr(cx,"选择处理人","Select assignee")).search_placeholder(self.tr(cx,"搜索处理人","Search assignees")).cleanable(true).disabled(busy||is_uncertain))))
            .child(field(self.tr(cx,"截止日期（本机时区，仅日期）","Due date (local timezone, date only)"),DatePicker::new(&self.due).placeholder(self.tr(cx,"选择日期","Select date")).cleanable(true).disabled(busy||is_uncertain)))
            .child(div().flex().gap_2().child(Button::new("reload-projects").label(self.tr(cx,"刷新项目列表","Refresh projects")).ghost().disabled(busy).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadProjects,cx)))).child(Button::new("reload-members").label(self.tr(cx,"刷新处理人","Refresh assignees")).ghost().disabled(busy||selected(&self.workspace,cx).is_empty()).on_click(cx.listener(|this,_,_,cx|this.send(Job::LoadMembers(selected(&this.workspace,cx)),cx)))))
            .when_some(record.last_error.clone(),|d,error|d.child(div().text_color(cx.theme().danger).text_sm().child(error)))
            .when(is_uncertain,|d|d.child(div().p_4().bg(cx.theme().muted).rounded_md().flex().flex_col().gap_3()
                .child(self.tr(cx,"上次写入结果不确定。请先在 TAPD 查找来源标记，填写已有条目 ID 验证关联。不会盲目重试创建。","The last write is uncertain. Find its source marker in TAPD and verify the existing item ID. Creation will not be blindly retried."))
                .child(format!("[lark-task:{}]",guid))
                .child(Input::new(&self.recover_id).id("recovery-id").aria_label(self.tr(cx,"已有条目 ID","Existing item ID")))
                .child(div().flex().gap_2().children([("story","新任务 / Story"),("task","旧任务 / Task")].into_iter().map(|(kind,label)|Button::new(kind).label(label).ghost().when(self.recover_kind==kind,|b|b.primary()).on_click(cx.listener(move |this,_,_,cx|{this.recover_kind=kind.into();cx.notify();})))))
                .child(Button::new("recover").label(self.tr(cx,"验证已有条目","Verify existing item")).disabled(busy).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|{let workspace=record.draft.as_ref().map(|d|d.workspace_id.clone()).or_else(||record.target_workspace.clone()).unwrap_or_default();this.send_editor(Job::Recover{guid:guid.clone(),workspace,id:this.recover_id.read(cx).value().to_string(),kind:this.recover_kind.clone()},cx);}})))))
            .child(div().flex().gap_2().child(Button::new("save-draft").label(self.tr(cx,"保存草稿","Save draft")).disabled(busy||is_uncertain||stale).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|this.send_editor(Job::SaveDraft(guid.clone(),this.draft(cx),this.edit_source.clone().expect("loaded editor")),cx)})))
                .child(Button::new("preview").label(if record.target.is_some(){self.tr(cx,"预览更新","Preview update")}else{self.tr(cx,"预览导入","Preview import")}).primary().disabled(busy||is_uncertain||stale||status==SyncStatus::Synced).on_click(cx.listener(move |this,_,_,cx|this.send_editor(Job::Preview(guid.clone(),this.draft(cx),this.edit_source.clone().expect("loaded editor")),cx)))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"以飞书任务为来源。只有你确认后才会写入 TAPD。","Lark is the source. Nothing is written to TAPD until you confirm.")))
            .into_any_element()
    }
    pub(super) fn preview(&self, preview: Preview, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.read(cx);
        let busy = model.busy;
        div().id("preview-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().p_6().flex().flex_col().gap_4()
            .child(div().text_xl().font_semibold().child(self.tr(cx,"确认同步内容","Review before syncing")))
            .child(div().text_sm().text_color(cx.theme().muted_foreground).child(if preview.target.is_some(){self.tr(cx,"将更新已有条目。下方展示当前 TAPD 内容与本次提交；确认意味着同意覆盖所列字段。","Updates an existing item. Review current TAPD contents and the proposed changes; confirmation allows replacement of these fields.")}else{self.tr(cx,"将在目标项目新建一个任务，并记录来源关联。","Creates one task in the selected workspace and records its source mapping.")}))
            .when_some(preview.target.as_ref(),|d,target|d.child(div().rounded_md().p_4().bg(cx.theme().muted).flex().flex_col().gap_2().child(div().font_semibold().child(self.tr(cx,"当前 TAPD 内容","Current TAPD contents"))).child(target.title.clone()).child(target.description.clone()).child(format!("{} · {}",target.owner,target.due.clone().unwrap_or_default()))))
            .child(div().rounded_md().p_4().bg(cx.theme().accent).flex().flex_col().gap_3().child(div().font_semibold().child(self.tr(cx,"将要提交","Proposed contents"))).child(preview.draft.title.clone()).child(preview.draft.description.clone()).child(format!("{}: {}",self.tr(cx,"项目","Workspace"),preview.draft.workspace_id)).child(format!("{}: {}",self.tr(cx,"负责人","Owner"),preview.draft.owner)).child(format!("{}: {}",self.tr(cx,"截止日期","Due date"),preview.draft.due.clone().unwrap_or_default())))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"提交时附加飞书来源链接与稳定标记。提交前会再次检查源任务与目标内容，发现变化则取消写入。","A source link and stable marker are appended. Source and target are rechecked before writing; concurrent changes cancel the write.")))
            .child(div().flex().gap_3().child(Button::new("back-to-edit").label(self.tr(cx,"返回编辑","Back to editing")).disabled(busy).on_click(cx.listener(|this,_,_,cx|this.model.update(cx,|m,cx|{if let Some(p)=m.preview.take(){cx.emit(Editor{source:p.source,draft:p.draft,profile:p.profile});}cx.notify();}))))
                .child(Button::new("confirm-import").label(self.tr(cx,"确认写入 TAPD","Confirm write to TAPD")).primary().disabled(busy).on_click(cx.listener(move |this,_,_,cx|this.model.update(cx,|m,cx|m.send_at(Job::Commit(preview.guid.clone(),preview.draft.clone(),preview.target.clone()),preview.profile.clone(),cx))))))
    }
}

// 人员和时间来自源任务；缺失时明确显示不可用，不把不透明 ID 当成人名。
fn person_name(person: Option<&SourcePerson>, language: &str) -> String {
    person
        .filter(|p| !p.name.trim().is_empty())
        .map(|p| p.name.clone())
        .unwrap_or_else(|| text(language, "姓名未提供", "Name unavailable").into())
}
fn assignee_names(source: &SourceTask, language: &str) -> String {
    if source.metadata.assignees.is_empty() {
        return text(language, "未分配", "Unassigned").into();
    }
    source
        .metadata
        .assignees
        .iter()
        .map(|p| person_name(Some(p), language))
        .collect::<Vec<_>>()
        .join(", ")
}
fn format_time(value: Option<&str>, language: &str) -> String {
    value
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
        .unwrap_or_else(|| text(language, "未提供", "Not available").into())
}
