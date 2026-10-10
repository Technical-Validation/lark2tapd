//! 列表只显示摘要；编辑、Markdown 和附件都留在单一任务详情中。
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
            Filter::Changed => self.tr(cx, "待处理任务", "Needs attention"),
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
                                .child(self.tr(cx, "暂无符合条件的任务。", "No matching tasks.")),
                        )
                    })
                    .children(records.into_iter().map(|record| {
                        let id = record.source.guid.clone();
                        let selected = self.selected.as_ref() == Some(&id);
                        let summary = task_summary(&record.source);
                        div()
                            .id(SharedString::from(format!("task-{id}")))
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
                            .child(
                                Button::new(SharedString::from(format!("select-{id}")))
                                    .label(record.source.title.clone())
                                    .ghost()
                                    .w_full()
                                    .text_color(cx.theme().foreground)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select(record.clone(), window, cx)
                                    })),
                            )
                            .children(summary.into_iter().map(|value| {
                                div()
                                    .mt_1()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(value)
                            }))
                    })),
            )
    }
    pub(super) fn detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let model = self.model.read(cx);
        let record = self
            .selected
            .as_ref()
            .and_then(|id| {
                model
                    .snapshot
                    .as_ref()?
                    .records
                    .iter()
                    .find(|r| &r.source.guid == id && r.in_scope)
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
                    "选择任务即可编辑和导入。",
                    "Select a task to edit and import.",
                )))
                .into_any_element();
        };
        let busy = model.busy;
        let uncertain = record.uncertain;
        let stale = self.edit_source.as_ref() != Some(&record.source);
        let guid = record.source.guid.clone();
        let owner = cx.entity().downgrade();
        let partial = record.has_pending_attachments();
        div().id("detail-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().p_6().flex().flex_col().gap_4()
            .child(div().flex().justify_between().child(div().text_xl().font_semibold().child(self.tr(cx,"任务详情","Task details"))).child(div().text_sm().text_color(cx.theme().muted_foreground).child(if partial{self.tr(cx,"附件待处理","Attachments pending")}else{status_text(model.language(),record.status())})))
            .child(div().flex().gap_2().child(Button::new("open-source").label(self.tr(cx,"在飞书中打开","Open in Lark")).ghost().disabled(record.source.url.is_empty()).on_click({let url=record.source.url.clone();move |_,_,cx|open_safe_url(&url,cx)}))
                .when_some(record.target.as_ref(),|d,t|d.child(Button::new("open-target").label(self.tr(cx,"在 TAPD 中打开","Open in TAPD")).ghost().disabled(t.url.is_empty()).on_click({let url=t.url.clone();move |_,_,cx|open_safe_url(&url,cx)}))))
            .child(Collapsible::new().open(self.source_open).motion_id("source-details")
                .child(Button::new("toggle-source-details").label(if self.source_open{self.tr(cx,"收起飞书任务详情","Hide Lark task details")}else{self.tr(cx,"展开飞书任务详情","Show Lark task details")}).ghost().on_click(cx.listener(|this,_,_,cx|{this.source_open = !this.source_open;cx.notify();})))
                .content(self.source_details(&record.source,cx)))
            .when(stale,|d|d.child(div().flex().flex_col().gap_2().child(self.tr(cx,"来源已变化或草稿尚未加载。","Source changed or the draft is not loaded."))
                .child(Button::new("reload-source").label(self.tr(cx,"丢弃草稿并重新读取","Discard draft and reload")).disabled(busy||uncertain).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|this.reload_source(guid.clone(),cx)})))))
            .child(field(self.tr(cx,"TAPD 标题","TAPD title"),div().flex().gap_2().child(Select::new(&self.prefix).id("title-prefix").placeholder(self.tr(cx,"无前缀","No prefix")).cleanable(true).w(px(160.)).disabled(busy||uncertain||stale)).child(Input::new(&self.title).id("draft-title").aria_label(self.tr(cx,"标题","Title")).disabled(busy||uncertain||stale))))
            .child(field(self.tr(cx,"内容 · Markdown","Description · Markdown"),Textarea::new(&self.description).accessibility_id("draft-description").aria_label(self.tr(cx,"Markdown 内容","Markdown description")).h(px(180.)).disabled(busy||uncertain||stale).on_paste(move |item,window,cx|owner.update(cx,|this,cx|this.paste_image(item,window,cx)).unwrap_or(false))))
            .child(div().id("markdown-content").p_3().rounded_md().border_1().border_color(cx.theme().border).child(self.markdown("rendered-description",self.description.read(cx).value().to_string())))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"支持粘贴图片。仅显示本任务的本地图片，外部图片不自动加载。","Paste images here. Only this task’s local images are displayed; external images are not loaded automatically.")))
            .child(Button::new("add-attachments").label(self.tr(cx,"添加附件","Add attachments")).disabled(busy||uncertain||stale).on_click(cx.listener(|this,_,window,cx|this.add_files(window,cx))))
            .children(self.attachments.iter().map(|a|{
                let uploaded=a.remote_id.as_ref().is_some_and(|v|!v.is_empty())||a.remote_url.as_ref().is_some_and(|v|!v.is_empty());let id=a.id.clone();
                div().flex().items_center().gap_2().child(div().flex_1().min_w_0().child(a.name.clone())).child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} KB · {}",a.size.div_ceil(1024),if a.uncertain{self.tr(cx,"需核对上传","Verify upload")}else if uploaded{self.tr(cx,"已上传","Uploaded")}else{self.tr(cx,"本地附件","Local attachment")})))
                    .child(Button::new(SharedString::from(format!("remove-{id}"))).label(self.tr(cx,"移除","Remove")).ghost().disabled(busy||uploaded||a.uncertain).on_click(cx.listener(move |this,_,_,cx|{this.attachments.retain(|v|v.id!=id);this.images.remove(&format!("attachment:{id}"));cx.notify();})))
            }))
            .when(self.pending_files>0||!self.pending_pastes.is_empty(),|d|d.child(div().text_sm().child(self.tr(cx,"正在读取附件…","Reading attachments…"))))
            .child(self.inline_error("attachments",cx))
            .child(div().flex().gap_3().child(field(self.tr(cx,"目标项目空间","TAPD workspace"),Select::new(&self.workspace).id("draft-workspace").placeholder(self.tr(cx,"选择项目空间","Select workspace")).search_placeholder(self.tr(cx,"搜索项目空间","Search workspaces")).disabled(busy||record.target.is_some()||stale)))
                .child(field(self.tr(cx,"处理人","Assignee"),Select::new(&self.owner).id("draft-owner").placeholder(self.tr(cx,"选择处理人","Select assignee")).cleanable(true).disabled(busy||uncertain||stale))))
            .child(field(self.tr(cx,"截止日期","Due date"),DatePicker::new(&self.due).cleanable(true).disabled(busy||uncertain||stale)))
            .when_some(record.last_error.clone(),|d,error|d.child(div().text_sm().text_color(cx.theme().danger).child(error)))
            .when(uncertain,|d|d.child(div().p_3().bg(cx.theme().muted).flex().flex_col().gap_2()
                .child(self.tr(cx,"上次写入结果不确定，请先在 TAPD 核对已有条目，不会自动重试创建。","The last write is uncertain. Verify the existing TAPD item; creation is not automatically retried."))
                .child(format!("[lark-task:{}]",guid)).child(Input::new(&self.recover_id).id("recovery-id"))
                .child(div().flex().gap_2().children([("story","Story"),("task","Task")].into_iter().map(|(kind,label)|Button::new(kind).label(label).ghost().when(self.recover_kind==kind,|b|b.primary()).on_click(cx.listener(move |this,_,_,cx|{this.recover_kind=kind.into();cx.notify();})))))
                .child(Button::new("recover").label(self.tr(cx,"验证已有条目","Verify existing item")).disabled(busy).on_click(cx.listener({let guid=guid.clone();let workspace=record.target_workspace.clone().or_else(||record.draft.as_ref().map(|d|d.workspace_id.clone())).unwrap_or_default();move |this,_,_,cx|this.send_editor(Job::Recover{guid:guid.clone(),workspace:workspace.clone(),id:this.recover_id.read(cx).value().to_string(),kind:this.recover_kind.clone()},cx)})))))
            .child(div().flex().gap_3().child(Button::new("save-draft").label(self.tr(cx,"保存草稿","Save draft")).disabled(busy||uncertain||stale||!self.pending_pastes.is_empty()||self.pending_files>0).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|{if let Some(source)=this.edit_source.clone(){this.send_editor(Job::SaveDraft(guid.clone(),this.draft(cx),source),cx);}}})))
                .child(Button::new("submit").label(if record.target.is_some(){self.tr(cx,"更新到 TAPD","Update in TAPD")}else{self.tr(cx,"导入 TAPD","Import to TAPD")}).primary().disabled(busy||uncertain||stale||!self.pending_pastes.is_empty()||self.pending_files>0).on_click(cx.listener(move |this,_,_,cx|{if let Some(source)=this.edit_source.clone(){this.send_editor(Job::Submit(guid.clone(),this.draft(cx),source),cx);}}))))
            .child(self.inline_error("submit",cx))
            .when(record.target.is_some(),|d|d.child(Button::new("check-remote-target").label(self.tr(cx,"核对 TAPD 最新内容","Review current TAPD contents")).ghost().on_click(cx.listener({let guid=record.source.guid.clone();move |this,_,_,cx|this.send_editor(Job::CheckTarget{guid:guid.clone(),session:this.editor_session},cx)}))))
            .when_some(self.remote_target.clone(),|d,target|d.child(div().p_3().border_1().border_color(cx.theme().border).rounded_md().flex().flex_col().gap_2()
                .child(self.tr(cx,"远端当前版本；接受只更新本地基线，不会立即覆盖 TAPD。","Current remote version. Accepting updates the local baseline only; it does not write to TAPD."))
                .child(target.title.clone()).child(self.markdown("remote-description",target.description.clone()))
                .child(Button::new("accept-remote-target").label(self.tr(cx,"接受当前版本并继续编辑","Accept version and continue editing")).disabled(busy).on_click(cx.listener({let guid=record.source.guid.clone();move |this,_,_,cx|this.send_editor(Job::AcceptTarget{guid:guid.clone(),target:target.clone()},cx)})))))
            .into_any_element()
    }
    fn source_details(&self, source: &SourceTask, cx: &App) -> impl IntoElement {
        let creator = source
            .metadata
            .creator
            .as_ref()
            .and_then(|p| nonblank(&p.name));
        let assignees = source
            .metadata
            .assignees
            .iter()
            .filter_map(|p| nonblank(&p.name))
            .collect::<Vec<_>>()
            .join(", ");
        div()
            .p_3()
            .bg(cx.theme().muted)
            .flex()
            .flex_col()
            .gap_2()
            .child(source.title.clone())
            .when_some(creator, |d, v| {
                d.child(format!("{}: {v}", self.tr(cx, "创建人", "Created by")))
            })
            .when(!assignees.is_empty(), |d| {
                d.child(format!(
                    "{}: {assignees}",
                    self.tr(cx, "负责人", "Assignees")
                ))
            })
            .when_some(local_time(source.metadata.created_at.as_deref()), |d, v| {
                d.child(format!("{}: {v}", self.tr(cx, "创建时间", "Created")))
            })
            .when_some(local_time(source.metadata.due_at.as_deref()), |d, v| {
                d.child(format!("{}: {v}", self.tr(cx, "截止时间", "Due")))
            })
            .when_some(
                local_time(source.metadata.completed_at.as_deref()),
                |d, v| d.child(format!("{}: {v}", self.tr(cx, "结束时间", "Completed"))),
            )
            .child(self.tr(
                cx,
                if source.completed {
                    "已完成"
                } else {
                    "未完成"
                },
                if source.completed {
                    "Completed"
                } else {
                    "Incomplete"
                },
            ))
            .when(!source.description.is_empty(), |d| {
                d.child(self.markdown("source-description", source.description.clone()))
            })
    }
}
fn nonblank(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}
fn local_time(value: Option<&str>) -> Option<String> {
    value
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M")
                .to_string()
        })
}
fn task_summary(source: &SourceTask) -> Vec<String> {
    let names = source
        .metadata
        .assignees
        .iter()
        .filter_map(|p| nonblank(&p.name))
        .collect::<Vec<_>>()
        .join(", ");
    let mut values = Vec::new();
    if !names.is_empty() {
        values.push(names);
    }
    if let Some(time) = local_time(source.metadata.created_at.as_deref()) {
        values.push(time);
    }
    values
}
