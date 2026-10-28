//! 批量详情仅按任务标识补查；选中任务的受限原文匹配由 source_conversation 单独执行。
use super::*;
use lark2tapd_core::SourceMessageDetails;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::Ordering;

// 数字消息 ID 不是 OpenAPI 的 om_ ID；不能猜测转换或反复请求一个已知无效的标识。
pub(super) fn open_id(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

impl CliGateway {
    pub(super) fn enrich_sources(
        &self,
        tasks: &mut [SourceTask],
        deadline: Instant,
        cancelled: &AtomicBool,
    ) {
        // 可选展示信息最多共用五秒，给原刷新预算留一秒余量，不挤占必需详情读取。
        let now = Instant::now();
        if cancelled.load(Ordering::SeqCst)
            || deadline.saturating_duration_since(now) < Duration::from_secs(6)
        {
            return;
        }
        let deadline = now + Duration::from_secs(5);
        let message_deadline = now + Duration::from_secs(2);
        let ids: BTreeSet<_> = tasks
            .iter()
            .flat_map(|task| &task.metadata.source_messages)
            .map(|message| message.message_id.clone())
            .filter(|id| open_id(id, "om_"))
            .collect();
        let mut messages = BTreeMap::new();
        for id in ids {
            let args = vec![
                "api".into(),
                "GET".into(),
                format!("/open-apis/im/v1/messages/{id}"),
                "--as".into(),
                "user".into(),
                "--format".into(),
                "json".into(),
                "--params".into(),
                "-".into(),
            ];
            let Some(data) = self.optional_detail_read(
                args,
                json!({"user_id_type":"open_id"}),
                message_deadline,
                cancelled,
            ) else {
                break;
            };
            let matches: Vec<_> = data
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|message| text(message, "message_id") == id)
                .collect();
            // 不接收旁边的话题回复，也不把相同正文当作任务引用的消息。
            if let [message] = matches.as_slice() {
                let sender = message.get("sender").and_then(|sender| {
                    let id = text(sender, "id");
                    (!id.is_empty()).then(|| SourcePerson {
                        id,
                        name: text(sender, "sender_name"),
                    })
                });
                let sent_at = message
                    .get("create_time")
                    .and_then(Value::as_str)
                    .filter(|time| time.bytes().all(|byte| byte.is_ascii_digit()))
                    .and_then(|time| time.parse::<i64>().ok())
                    .filter(|time| *time > 0 && DateTime::from_timestamp_millis(*time).is_some())
                    .map(|time| time.to_string());
                messages.insert(id, SourceMessageDetails { sender, sent_at });
            }
        }
        let mut names = BTreeMap::new();
        let mut missing = BTreeSet::new();
        // 同一人的任务内姓名也可复用；只补缺失姓名，已存在的任务姓名始终优先。
        for person in tasks
            .iter()
            .flat_map(|task| {
                task.metadata
                    .creator
                    .iter()
                    .chain(&task.metadata.assignees)
                    .chain(&task.metadata.followers)
            })
            .chain(
                messages
                    .values()
                    .filter_map(|message| message.sender.as_ref()),
            )
        {
            if !person.name.trim().is_empty() && person.name != person.id {
                names
                    .entry(person.id.clone())
                    .or_insert_with(|| person.name.clone());
            } else if open_id(&person.id, "ou_") {
                missing.insert(person.id.clone());
            }
        }
        let missing: Vec<_> = missing
            .into_iter()
            .filter(|id| !names.contains_key(id))
            .collect();
        // 单页最多30人，无关键词、无枚举；仅请求当前任务真正引用的用户。
        for chunk in missing.chunks(30) {
            let args = [
                "contact",
                "+search-user",
                "--as",
                "user",
                "--format",
                "json",
                "--user-ids",
                &chunk.join(","),
                "--page-size",
                "30",
            ]
            .map(String::from)
            .to_vec();
            let Some(data) = self.optional_detail_read(args, Value::Null, deadline, cancelled)
            else {
                break;
            };
            for user in data
                .get("users")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let id = text(user, "open_id");
                let name = text(user, "localized_name");
                if chunk.contains(&id) && !name.trim().is_empty() && name != id {
                    names.insert(id, name);
                }
            }
        }
        for task in tasks {
            let metadata = &mut task.metadata;
            // 每个任务只持久化自己的关联信息，不把整轮通讯录结果复制给每条任务。
            metadata.message_details = metadata
                .source_messages
                .iter()
                .filter_map(|message| {
                    messages
                        .get(&message.message_id)
                        .map(|details| (message.message_id.clone(), details.clone()))
                })
                .collect();
            metadata.resolved_names = metadata
                .creator
                .iter()
                .chain(&metadata.assignees)
                .chain(&metadata.followers)
                .chain(
                    metadata
                        .message_details
                        .values()
                        .filter_map(|message| message.sender.as_ref()),
                )
                .filter(|person| person.name.trim().is_empty() || person.name == person.id)
                .filter_map(|person| {
                    names
                        .get(&person.id)
                        .map(|name| (person.id.clone(), name.clone()))
                })
                .collect();
        }
    }

    fn optional_detail_read(
        &self,
        args: Vec<String>,
        input: Value,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Option<Value> {
        if cancelled.load(Ordering::SeqCst) {
            return None;
        }
        let timeout = deadline.checked_duration_since(Instant::now())?;
        let response = self
            .invoke_cancellable(true, args, input, timeout, cancelled)
            .ok()?;
        (response.get("ok").and_then(Value::as_bool) == Some(true))
            .then(|| response.get("data").cloned())
            .flatten()
    }
}
