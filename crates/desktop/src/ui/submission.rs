//! 仅提交发生冲突时询问覆盖；确认针对这一次草稿和读到的远端版本，不修改同步基线。
use super::*;

impl Workspace {
    pub(super) fn begin_submission(
        &mut self,
        guid: &str,
        source: SourceTask,
        cx: &mut Context<Self>,
    ) {
        if self.pending_submission.is_some() || !self.can_edit_description(cx) {
            return;
        }
        let draft = self.draft(cx);
        let imported = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| {
                snapshot
                    .records
                    .iter()
                    .any(|record| record.source.guid == guid && record.target.is_some())
            });
        if imported {
            self.pending_submission = Some((draft, source));
            self.send_editor(
                Job::PrepareSubmit {
                    guid: guid.into(),
                    session: self.editor_session,
                },
                cx,
            );
        } else {
            self.send_editor(Job::Submit(guid.into(), draft, source, None), cx);
        }
        cx.notify();
    }

    pub(super) fn confirm_submission(
        &mut self,
        remote: &RemoteContext,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.as_ref() != Some(&remote.guid)
            || self.edit_profile != remote.profile
            || self.editor_session != remote.session
        {
            return;
        }
        let Some((draft, source)) = self.pending_submission.clone() else {
            return;
        };
        if !self.submission_is_current(&draft, &source, cx) {
            self.pending_submission = None;
            self.form_error(
                "submit",
                self.tr(
                    cx,
                    "草稿或来源已变化，请重新提交。",
                    "Draft or source changed. Submit again.",
                )
                .into(),
                cx,
            );
            cx.notify();
            return;
        }
        let old = self
            .model
            .read(cx)
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .records
                    .iter()
                    .find(|record| record.source.guid == remote.guid)
            })
            .and_then(|record| record.target.as_ref());
        // URL 是导航信息，不是远端正文版本；旧缓存补齐 URL 不应触发覆盖确认。
        let unchanged = old.is_some_and(|old| {
            let mut old = old.clone();
            old.url = remote.target.url.clone();
            if old.workitem_type_id.is_empty() {
                old.workitem_type_id = remote.target.workitem_type_id.clone();
            }
            old == remote.target
        });
        if unchanged {
            self.pending_submission = None;
            self.send_editor(
                Job::Submit(
                    remote.guid.clone(),
                    draft,
                    source,
                    Some(Box::new(remote.target.clone())),
                ),
                cx,
            );
            cx.notify();
            return;
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            self.tr(cx, "TAPD 内容已被修改，覆盖同步字段？", "TAPD contents changed. Overwrite synchronized fields?"),
            Some(self.tr(cx, "继续将用当前草稿覆盖标题、正文、处理人和截止日期。取消不会写入 TAPD。", "Continuing replaces the title, description, assignee and due date with this draft. Cancel leaves TAPD unchanged.")),
            &[self.tr(cx, "取消", "Cancel"), self.tr(cx, "覆盖并更新", "Overwrite and update")],
            cx,
        );
        let remote = remote.clone();
        cx.spawn_in(window, async move |this, cx| {
            let overwrite = answer.await == Ok(1);
            let _ = this.update_in(cx, |this, _, cx| {
                if this.editor_session != remote.session
                    || this.edit_profile != remote.profile
                    || this.selected.as_ref() != Some(&remote.guid)
                {
                    return;
                }
                let current = this.pending_submission.as_ref()
                    == Some(&(draft.clone(), source.clone()))
                    && this.submission_is_current(&draft, &source, cx);
                this.pending_submission = None;
                if overwrite && current {
                    // Engine 在写入前再次读取并比较 expected；确认后远端再改动仍会被拒绝。
                    this.send_editor(
                        Job::Submit(remote.guid, draft, source, Some(Box::new(remote.target))),
                        cx,
                    );
                } else if overwrite {
                    this.form_error(
                        "submit",
                        this.tr(
                            cx,
                            "草稿或来源已变化，请重新提交。",
                            "Draft or source changed. Submit again.",
                        )
                        .into(),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn submission_is_current(&self, draft: &Draft, source: &SourceTask, cx: &App) -> bool {
        self.can_edit_description(cx)
            && self.draft(cx) == *draft
            && self.edit_source.as_ref() == Some(source)
            && self.pending_pastes.is_empty()
            && self.pending_files == 0
    }
}
