//! 列表只显示摘要；编辑、Markdown 和附件都留在单一任务详情中。
use super::*;
use gpui_kit::base::Selectable;
use gpui_kit::component::input::{InputGroup, InputGroupAddon, InputGroupInput};
use gpui_kit::component::{Icon, IconName, button::ButtonCustomVariant};

impl Workspace {
    pub(super) fn task_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).value().to_lowercase();
        let model = self.model.read(cx);
        let mut records: Vec<_> = model
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
        // 仅改变视图顺序，不重排缓存或切换当前任务；相同时间沿用原有顺序。
        records.sort_by_key(|record| {
            std::cmp::Reverse(timestamp(record.source.metadata.created_at.as_deref()))
        });
        div()
            .flex()
            .flex_col()
            .w(px(380.))
            .flex_shrink_0()
            .h_full()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(self.sidebar(cx))
            .child(
                div().px_4().pb_4().flex().flex_col().gap_3().child(
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
                        // 原生 Button 包住整张卡片，鼠标和键盘共享同一个选择行为。
                        Button::new(SharedString::from(format!("select-{id}")))
                            .accessibility_label(record.source.title.clone())
                            // selected 使用组件的 active 样式，不让普通 hover 覆盖选中背景。
                            .custom(
                                ButtonCustomVariant::new(cx)
                                    .hover(cx.theme().secondary_hover)
                                    .active(cx.theme().secondary_active),
                            )
                            .selected(selected)
                            .w_full()
                            .h_auto()
                            .p_3()
                            .mb_2()
                            .text_color(cx.theme().foreground)
                            .border_0()
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .whitespace_normal()
                                    .flex()
                                    .flex_col()
                                    .items_start()
                                    .gap_2()
                                    .child(
                                        div()
                                            .w_full()
                                            .text_left()
                                            .line_clamp(2)
                                            .font_semibold()
                                            .child(summary.title),
                                    )
                                    .when_some(summary.description, |d, value| {
                                        d.child(
                                            div()
                                                .w_full()
                                                .text_left()
                                                .line_clamp(2)
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(value),
                                        )
                                    })
                                    // 缺失字段不占位；仅有元数据时才创建底部一排。
                                    .when(
                                        summary.assignees.is_some() || summary.created.is_some(),
                                        |d| {
                                            d.child(
                                                div()
                                                    .w_full()
                                                    .flex()
                                                    .items_center()
                                                    .justify_between()
                                                    .gap_2()
                                                    .text_xs()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .when_some(summary.assignees, |d, names| {
                                                        d.child(
                                                            div()
                                                                .min_w_0()
                                                                .flex_1()
                                                                .text_left()
                                                                .truncate()
                                                                .child(names),
                                                        )
                                                    })
                                                    .when_some(summary.created, |d, created| {
                                                        d.child(
                                                            div().flex_shrink_0().child(created),
                                                        )
                                                    }),
                                            )
                                        },
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select(record.clone(), window, cx)
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
        let partial = record.has_pending_attachments();
        let type_workspace = selected(&self.workspace, cx);
        let selected_type = selected(&self.workitem_type, cx);
        let types_loading = model
            .activities
            .contains(&format!("workitem-types:{type_workspace}"));
        let type_valid = record.target.is_some()
            || model
                .snapshot
                .as_ref()
                .and_then(|s| s.directory.workitem_types.get(&type_workspace))
                .is_some_and(|items| {
                    items
                        .iter()
                        .any(|item| type_key(&item.kind, &item.id) == selected_type)
                });
        let submitting = model.activities.contains("submit");
        let preparing = self.pending_submission.is_some();
        let target_url = record.target.as_ref().map(|target| {
            lark2tapd_bridge::target_url(target, record.target_workspace.as_deref().unwrap_or(""))
        });
        let recovery_id = self.recover_id.read(cx).value();
        let recovery_valid = !recovery_id.trim().is_empty()
            && recovery_id.trim().bytes().all(|byte| byte.is_ascii_digit());
        div().id("detail-scroll").flex_1().min_w_0().h_full().overflow_y_scroll().track_scroll(&self.description_scroll).p_6().flex().flex_col().gap_4()
            // 状态与外部入口保持在标题行，飞书图标位于最右侧。
            .child(div().flex().items_center().gap_2()
                .child(div().debug_selector(|| "source-summary".into()).flex_1().min_w_0().whitespace_normal().text_xl().font_semibold().child(record.source.title.clone()))
                .child(div().id("detail-sync-status").flex().items_center().gap_1().text_sm().text_color(cx.theme().muted_foreground)
                    .when(Filter::Synced.matches(&record), |d| d.child(synced_icon(cx)))
                    .child(if partial { self.tr(cx,"附件待处理","Attachments pending") } else { status_text(model.language(),record.status()) })
                    .tooltip({let label = if partial { self.tr(cx,"附件待处理","Attachments pending") } else { status_text(model.language(),record.status()) }; move |window,cx| gpui_kit::component::tooltip::Tooltip::new(label).build(window,cx) }))
                .when_some(target_url,|d,url|d.child(Button::new("open-target").child(Icon::new(IconName::ExternalLink).size_4()).accessibility_label(self.tr(cx,"在 TAPD 中打开","Open in TAPD")).tooltip(self.tr(cx,"在 TAPD 中打开","Open in TAPD")).ghost().disabled(url.is_empty()).on_click(move |_,_,cx|open_safe_url(&url,cx))))
                .child(Button::new("open-source").child(img(lark_icon(cx.theme().is_dark())).size(px(20.))).accessibility_label(self.tr(cx,"在飞书中打开","Open in Lark")).tooltip(self.tr(cx,"在飞书中打开","Open in Lark")).ghost().disabled(record.source.url.is_empty()).on_click({let url=record.source.url.clone();move |_,_,cx|open_safe_url(&url,cx)})))
            .child(self.source_metadata_row(&record.source,cx))
            .when(!record.source.metadata.source_messages.is_empty(), |d| d.child(self.source_conversation(&record.source, cx)))
            .when(stale,|d|d.child(div().flex().flex_col().gap_2().child(self.tr(cx,"来源已变化或草稿尚未加载。","Source changed or the draft is not loaded."))
                .child(Button::new("reload-source").label(self.tr(cx,"丢弃草稿并重新读取","Discard draft and reload")).disabled(busy||uncertain).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|this.reload_source(guid.clone(),cx)})))))
            .child(field(self.tr(cx,"TAPD 标题","TAPD title"),InputGroup::new("tapd-title-group").input(InputGroupInput::new(&self.title).id("draft-title").aria_label(self.tr(cx,"标题","Title")).disabled(busy||uncertain||stale)).addon(InputGroupAddon::new("title-prefix-addon").child(Select::new(&self.prefix).id("title-prefix").appearance(false).placeholder(self.tr(cx,"无前缀","No prefix")).cleanable(true).w(px(160.)).disabled(busy||uncertain||stale)))))
            .child(field(self.tr(cx,"内容 · Markdown","Description · Markdown"),self.description_editor(busy||uncertain||stale,cx)))
            .when(record.source.description.contains("[Image]") && self.description.read(cx).text().contains("[Image]"),|d| d.child(div().debug_selector(|| "source-image-unavailable".into()).text_sm().text_color(cx.theme().muted_foreground).child(self.tr(cx,"飞书接口仅返回 [Image] 占位文本，未提供对应图片。请在飞书查看原图，或粘贴图片替换占位符。","Lark returned only an [Image] placeholder, without image data. View the original in Lark or paste the image to replace it."))))
            .child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"支持粘贴图片。仅显示本任务的本地图片，外部图片不自动加载。","Paste images here. Only this task’s local images are displayed; external images are not loaded automatically.")))
            .child(Switch::new("include-source-link").label(self.tr(cx,"附加飞书任务链接","Include Lark task link")).checked(self.include_source_link).disabled(busy||uncertain||stale).on_click(cx.listener(|this,enabled,_,cx| {this.include_source_link = *enabled;cx.notify();})))
            .child(Button::new("add-attachments").label(self.tr(cx,"添加附件","Add attachments")).disabled(busy||uncertain||stale).on_click(cx.listener(|this,_,window,cx|this.add_files(window,cx))))
            .children(self.attachments.iter().map(|a|{
                let uploaded=a.remote_id.as_ref().is_some_and(|v|!v.is_empty())||a.remote_url.as_ref().is_some_and(|v|!v.is_empty());let id=a.id.clone();
                div().flex().items_center().gap_2().child(div().flex_1().min_w_0().child(a.name.clone())).child(div().text_xs().text_color(cx.theme().muted_foreground).child(format!("{} KB · {}",a.size.div_ceil(1024),if a.uncertain{self.tr(cx,"需核对上传","Verify upload")}else if uploaded{self.tr(cx,"已上传","Uploaded")}else{self.tr(cx,"本地附件","Local attachment")})))
                    .child(Button::new(SharedString::from(format!("remove-{id}"))).label(self.tr(cx,"移除","Remove")).ghost().disabled(busy||uploaded||a.uncertain).on_click(cx.listener(move |this,_,_,cx|{this.attachments.retain(|v|v.id!=id);this.images.remove(&format!("attachment:{id}"));this.refresh_editor_images(cx);cx.notify();})))
            }))
            .when(self.pending_files>0||!self.pending_pastes.is_empty(),|d|d.child(div().text_sm().child(self.tr(cx,"正在读取附件…","Reading attachments…"))))
            .child(self.field_feedback("attachments",cx))
            .child(div().flex().gap_3().child(field(self.tr(cx,"目标项目空间","TAPD workspace"),Select::new(&self.workspace).id("draft-workspace").placeholder(self.tr(cx,"选择项目空间","Select workspace")).search_placeholder(self.tr(cx,"搜索项目空间","Search workspaces")).disabled(busy||record.target.is_some()||stale)))
                .child(field(self.tr(cx,"处理人","Assignee"),Select::new(&self.owner).id("draft-owner").placeholder(self.tr(cx,"选择处理人","Select assignee")).cleanable(true).disabled(busy||uncertain||stale))))
            .child(field(self.tr(cx,"TAPD 类型","TAPD work item type"),Select::new(&self.workitem_type).id("draft-workitem-type").placeholder(if types_loading {self.tr(cx,"读取类型中…","Loading types…")}else if record.target.is_some(){self.tr(cx,"已有条目类型不变","Existing type is unchanged")}else{self.tr(cx,"选择创建类型","Select a type")}).disabled(busy||uncertain||stale||record.target.is_some())))
            .child(self.field_feedback("workitem-types",cx))
            .child(field(self.tr(cx,"截止日期","Due date"),DatePicker::new(&self.due).cleanable(true).disabled(busy||uncertain||stale)))
            .when_some(record.last_error.clone(),|d,error|d.child(div().text_sm().text_color(cx.theme().danger).child(error)))
            .when(uncertain,|d|d.child(div().p_3().bg(cx.theme().muted).flex().flex_col().gap_2()
                .child(self.tr(cx,"上次写入结果不确定，请先在 TAPD 核对已有条目，不会自动重试创建。","The last write is uncertain. Verify the existing TAPD item; creation is not automatically retried."))
                .child(format!("[lark-task:{}]",guid)).child(Input::new(&self.recover_id).id("recovery-id").aria_label(self.tr(cx,"TAPD 条目的完整数字 ID（不是链接）","Full numeric TAPD item ID, not a URL")))
                .when(!recovery_valid, |d| d.child(div().text_xs().text_color(cx.theme().muted_foreground).child(self.tr(cx,"请填写已有条目的完整数字 ID，核对后才能继续。","Enter the existing item's full numeric ID to verify it."))))
                .child(div().flex().gap_2().children([("story","Story"),("bug","Bug"),("task","Task")].into_iter().map(|(kind,label)|Button::new(kind).label(label).ghost().when(self.recover_kind==kind,|b|b.primary()).on_click(cx.listener(move |this,_,_,cx|{this.recover_kind=kind.into();cx.notify();})))))
                .child(Button::new("recover").label(self.tr(cx,"验证已有条目","Verify existing item")).disabled(busy || !recovery_valid).on_click(cx.listener({let guid=guid.clone();let workspace=record.target_workspace.clone().or_else(||record.draft.as_ref().map(|d|d.workspace_id.clone())).unwrap_or_default();move |this,_,_,cx|this.send_editor(Job::Recover{guid:guid.clone(),workspace:workspace.clone(),id:this.recover_id.read(cx).value().trim().to_owned(),kind:this.recover_kind.clone()},cx)})))))
            .child(div().flex().flex_wrap().items_center().gap_3().child(Button::new("save-draft").label(self.tr(cx,"保存草稿","Save draft")).disabled(busy||uncertain||stale||!self.pending_pastes.is_empty()||self.pending_files>0).on_click(cx.listener({let guid=guid.clone();move |this,_,_,cx|{if let Some(source)=this.edit_source.clone(){this.send_editor(Job::SaveDraft(guid.clone(),this.draft(cx),source),cx);}}})))
                .child(Button::new("submit").loading(submitting||preparing).label(if preparing {self.tr(cx,"检查中…","Checking…")}else if submitting {if record.target.is_some(){self.tr(cx,"更新中…","Updating…")}else{self.tr(cx,"导入中…","Importing…")}}else if record.target.is_some(){self.tr(cx,"更新到 TAPD","Update in TAPD")}else{self.tr(cx,"导入 TAPD","Import to TAPD")}).primary().disabled(!type_valid||busy||self.pending_submission.is_some()||uncertain||stale||!self.pending_pastes.is_empty()||self.pending_files>0).on_click(cx.listener(move |this,_,_,cx|{if let Some(source)=this.edit_source.clone(){this.begin_submission(&guid,source,cx);}})))
                .when_some(record.last_synced_at.as_deref().and_then(|time| local_time(Some(time))), |d,time| d.child(div().id("last-sync-time").debug_selector(|| "last-sync-time".into()).text_xs().text_color(cx.theme().muted_foreground).child(format!("{} {time}",self.tr(cx,"上次成功更新：","Last successful update:"))))))

            .child(self.field_feedback("submit",cx))
            .into_any_element()
    }
    fn source_metadata_row(&self, source: &SourceTask, cx: &App) -> impl IntoElement {
        let missing = self.tr(cx, "未提供", "Not provided");
        let unnamed = self.tr(cx, "名称未提供", "Name unavailable");
        let creator = source
            .metadata
            .creator
            .as_ref()
            .map(|person| source.metadata.person_name(person).unwrap_or(unnamed))
            .unwrap_or(missing)
            .to_owned();
        let names = source
            .metadata
            .assignees
            .iter()
            .map(|person| source.metadata.person_name(person).unwrap_or(unnamed))
            .collect::<Vec<_>>()
            .join(", ");
        let assignees = nonblank(&names).unwrap_or_else(|| missing.to_owned());
        let created =
            local_time(source.metadata.created_at.as_deref()).unwrap_or_else(|| missing.to_owned());
        // 只展示三项基本信息；不再引入折叠状态，也不重复显示编辑区已有的正文。
        div()
            .debug_selector(|| "source-detail-panel".into())
            .w_full()
            .min_w_0()
            .flex_shrink_0()
            .flex()
            .flex_wrap()
            .gap_x_4()
            .gap_y_1()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .children(
                [
                    (
                        "source-creator",
                        self.tr(cx, "创建人", "Created by"),
                        creator,
                    ),
                    (
                        "source-assignees",
                        self.tr(cx, "负责人", "Assignees"),
                        assignees,
                    ),
                    (
                        "source-created",
                        self.tr(cx, "创建时间", "Created"),
                        created,
                    ),
                ]
                .into_iter()
                .map(|(id, label, value)| {
                    div()
                        .debug_selector(move || id.into())
                        .min_w_0()
                        .whitespace_normal()
                        .child(format!("{label}: {value}"))
                }),
            )
    }
}
// 实心圆勾只用于详情状态；是否完成沿用 Filter 的正文及附件判定。
pub(super) fn synced_icon(cx: &App) -> impl IntoElement {
    div()
        .debug_selector(|| "synced-icon".into())
        .size(px(16.))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .bg(cx.theme().success)
        .text_color(cx.theme().success_foreground)
        .child(Icon::new(IconName::Check).size(px(12.)))
}

// 编译进程序，运行时不请求远端图标或依赖工作目录。
fn lark_icon(dark: bool) -> Arc<Image> {
    static ICONS: std::sync::OnceLock<[Arc<Image>; 2]> = std::sync::OnceLock::new();
    let icons = ICONS.get_or_init(|| {
        ["#000", "#fff"].map(|color| {
            let svg = include_str!("../../assets/lark.svg").replace("#000", color);
            Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes()))
        })
    });
    icons[usize::from(dark)].clone()
}

fn nonblank(value: &str) -> Option<String> {
    if value.trim().is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}
pub(super) fn timestamp(value: Option<&str>) -> Option<chrono::DateTime<chrono::Utc>> {
    let value = value?;
    // 飞书返回毫秒时间戳，本地成功记录使用 RFC 3339；共享同一显示格式。
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|time| time.to_utc())
        .ok()
        .or_else(|| {
            value
                .parse::<i64>()
                .ok()
                .filter(|v| *v > 0)
                .and_then(chrono::DateTime::from_timestamp_millis)
        })
}
pub(super) fn local_time(value: Option<&str>) -> Option<String> {
    timestamp(value).map(|t| {
        t.with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}
// 明确字段，渲染时不会把缺失值误当成需要占位的一行。
struct TaskSummary {
    title: String,
    description: Option<String>,
    assignees: Option<String>,
    created: Option<String>,
}

fn task_summary(source: &SourceTask) -> TaskSummary {
    let names = source
        .metadata
        .assignees
        .iter()
        .filter_map(|person| source.metadata.person_name(person))
        .collect::<Vec<_>>()
        .join(", ");
    // 摘要按字符截断，不会切断 UTF-8；折叠 Markdown 常见标记与换行。
    let plain = source
        .description
        .split_whitespace()
        .map(|word| word.trim_matches(['#', '*', '_', '`', '>']))
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = plain.chars();
    let mut description: String = chars.by_ref().take(120).collect();
    if chars.next().is_some() {
        description.push('…');
    }
    TaskSummary {
        title: source.title.clone(),
        description: nonblank(&description),
        assignees: nonblank(&names),
        created: local_time(source.metadata.created_at.as_deref()),
    }
}

#[cfg(test)]
mod summary_tests {
    use super::*;
    use std::prelude::v1::test;
    #[test]
    fn missing_metadata_does_not_create_placeholder_rows() {
        let source = SourceTask {
            guid: "empty".into(),
            title: "保留标题".into(),
            description: "  \n ** **".into(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        let summary = task_summary(&source);
        assert_eq!(summary.title, "保留标题");
        assert_eq!(summary.description, None);
        assert_eq!(summary.assignees, None);
        assert_eq!(summary.created, None);
    }

    #[test]
    fn partial_metadata_keeps_only_available_fields() {
        let mut source = SourceTask {
            guid: "partial".into(),
            title: "Title".into(),
            description: String::new(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        source.metadata.assignees = vec![
            lark2tapd_core::SourcePerson {
                id: "1".into(),
                name: "林".into(),
            },
            lark2tapd_core::SourcePerson {
                id: "2".into(),
                name: " \n".into(),
            },
            lark2tapd_core::SourcePerson {
                id: "3".into(),
                name: "Alex".into(),
            },
        ];
        source.metadata.created_at = Some("invalid".into());
        let summary = task_summary(&source);
        assert_eq!(summary.assignees.as_deref(), Some("林, Alex"));
        assert_eq!(summary.created, None);
        assert_eq!(summary.description, None);

        source.metadata.assignees.clear();
        source.metadata.created_at = Some("1710000000000".into());
        let summary = task_summary(&source);
        assert_eq!(summary.assignees, None);
        assert!(summary.created.is_some());
        for invalid in ["", "0", "-1", "9223372036854775807"] {
            source.metadata.created_at = Some(invalid.into());
            assert_eq!(task_summary(&source).created, None);
        }
    }

    #[test]
    fn summary_uses_resolved_names_without_showing_user_ids() {
        let mut source = SourceTask {
            guid: "names".into(),
            title: "Title".into(),
            description: String::new(),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        source.metadata.assignees = vec![
            lark2tapd_core::SourcePerson {
                id: "ou_found".into(),
                name: String::new(),
            },
            lark2tapd_core::SourcePerson {
                id: "ou_missing".into(),
                name: "ou_missing".into(),
            },
        ];
        source
            .metadata
            .resolved_names
            .insert("ou_found".into(), "已查询姓名".into());
        assert_eq!(
            task_summary(&source).assignees.as_deref(),
            Some("已查询姓名")
        );
    }

    #[test]
    fn summary_includes_a_bounded_unicode_description() {
        let source = SourceTask {
            guid: "summary".into(),
            title: "Title".into(),
            description: format!("# **你好**\n\n{}", "界".repeat(200)),
            url: String::new(),
            due: None,
            completed: false,
            metadata: Default::default(),
        };
        let summary = task_summary(&source);
        let description = summary.description.unwrap();
        assert!(description.starts_with("你好 界"));
        assert!(description.ends_with('…'));
        assert!(description.chars().count() <= 121);
        assert!(!description.contains('\n'));
    }
}
