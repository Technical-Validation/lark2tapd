//! 仅在选中任务时解析来源会话；原文匹配不是数字消息 ID 的精确转换。
use super::*;
use crate::source_details::open_id;
use lark2tapd_core::{SourceMessage, SourceMessageDetails};
use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

impl CliGateway {
    pub fn resolve_source_conversation(
        &self,
        source: &SourceTask,
        cancelled: &AtomicBool,
    ) -> Result<SourceTask> {
        if !source.needs_conversation_resolution() {
            return Ok(source.clone());
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        ensure!(
            !cancelled.load(Ordering::SeqCst),
            "Conversation lookup cancelled"
        );
        let user = self.verified_self_until(deadline)?;
        let current = self.source_until_cancellable(&source.guid, deadline, cancelled)?;
        ensure!(
            current.is_related_to(&user) && current == *source,
            "Source changed; refresh this task before matching its conversation"
        );
        let mut enriched = source.clone();
        let mut requests = 0;
        // 整组成功才写入局部结果；任何歧义、超时或不完整查询都不发布部分发送人。
        for original in &source.metadata.source_messages {
            if original.message_id.is_empty()
                || !original
                    .message_id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit())
                || original.content.trim().is_empty()
                || source.metadata.matched_messages_for(original).is_some()
            {
                continue;
            }
            let matches = self.match_conversation_text(
                &original.content,
                deadline,
                cancelled,
                &mut requests,
            )?;
            for (message, details) in &matches {
                enriched
                    .metadata
                    .message_details
                    .insert(message.message_id.clone(), details.clone());
            }
            enriched.metadata.matched_messages.insert(
                original.message_id.clone(),
                matches.into_iter().map(|(message, _)| message).collect(),
            );
        }
        // 某些发送人只返回 open_id；仅补查已经匹配到的用户，不用任务创建人代替。
        let missing: BTreeSet<_> = enriched
            .metadata
            .matched_messages
            .values()
            .flatten()
            .filter_map(|message| {
                enriched
                    .metadata
                    .message_details
                    .get(&message.message_id)?
                    .sender
                    .as_ref()
            })
            .filter(|person| {
                open_id(&person.id, "ou_") && enriched.metadata.person_name(person).is_none()
            })
            .map(|person| person.id.clone())
            .collect();
        if !missing.is_empty() && missing.len() <= 30 {
            let args = [
                "contact",
                "+search-user",
                "--as",
                "user",
                "--format",
                "json",
                "--user-ids",
                &missing.iter().cloned().collect::<Vec<_>>().join(","),
                "--page-size",
                "30",
            ]
            .map(String::from)
            .to_vec();
            if let Ok(data) =
                self.conversation_read(args, Value::Null, deadline, cancelled, &mut requests)
            {
                for user in data
                    .get("users")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let id = text(user, "open_id");
                    let name = text(user, "localized_name");
                    if missing.contains(&id) && !name.trim().is_empty() && name != id {
                        enriched.metadata.resolved_names.insert(id, name);
                    }
                }
            }
        }
        ensure!(
            !cancelled.load(Ordering::SeqCst),
            "Conversation lookup cancelled"
        );
        Ok(enriched)
    }

    fn conversation_read(
        &self,
        args: Vec<String>,
        input: Value,
        deadline: Instant,
        cancelled: &AtomicBool,
        requests: &mut usize,
    ) -> Result<Value> {
        ensure!(
            !cancelled.load(Ordering::SeqCst),
            "Conversation lookup cancelled"
        );
        ensure!(
            *requests < 16,
            "Conversation lookup limit reached; original content retained"
        );
        *requests += 1;
        let response =
            self.invoke_cancellable(true, args, input, remaining(deadline)?, cancelled)?;
        if response.get("ok").and_then(Value::as_bool) != Some(true) {
            return Err(diagnostics::api_failure("Lark", &response, None));
        }
        response
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Lark conversation response missing data"))
    }

    fn match_conversation_text(
        &self,
        content: &str,
        deadline: Instant,
        cancelled: &AtomicBool,
        requests: &mut usize,
    ) -> Result<Vec<(SourceMessage, SourceMessageDetails)>> {
        let mut remaining_text = content;
        let mut chat: Option<String> = None;
        let mut position = None;
        let mut matched = Vec::new();
        // shortcut: 最多八条纯文本消息；超过预算或遇到富文本仍保留原文，待有结构化来源接口再扩展。
        for _ in 0..8 {
            let query: String = remaining_text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(50)
                .collect();
            ensure!(!query.is_empty(), "No searchable conversation content");
            let mut body = json!({"query":query});
            if let Some(chat) = &chat {
                body["filter"] = json!({"chat_ids":[chat]});
            }
            let data = self.conversation_read(
                vec![
                    "api".into(),
                    "POST".into(),
                    "/open-apis/im/v1/messages/search".into(),
                    "--as".into(),
                    "user".into(),
                    "--format".into(),
                    "json".into(),
                    "--params".into(),
                    json!({"page_size":20}).to_string(),
                    "--data".into(),
                    "-".into(),
                ],
                body,
                deadline,
                cancelled,
                requests,
            )?;
            ensure!(
                data.get("has_more").and_then(Value::as_bool) == Some(false)
                    && data
                        .get("notice")
                        .is_none_or(|notice| notice.is_null() || notice.as_str() == Some("")),
                "Conversation search is incomplete; original content retained"
            );
            let items = data
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("Lark response missing conversation candidates"))?;
            ensure!(
                !items.is_empty() && items.len() <= 20,
                "No unique complete conversation match"
            );
            let mut ids = BTreeSet::new();
            for item in items {
                let id = item
                    .pointer("/meta_data/message_id")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                ensure!(
                    open_id(id, "om_"),
                    "Conversation candidate has no usable message identity"
                );
                ids.insert(id.to_owned());
            }
            let data = self.conversation_read(
                vec![
                    "api".into(),
                    "GET".into(),
                    "/open-apis/im/v1/messages/mget".into(),
                    "--as".into(),
                    "user".into(),
                    "--format".into(),
                    "json".into(),
                    "--params".into(),
                    "-".into(),
                ],
                json!({"message_ids":ids,"user_id_type":"open_id","with_sender_name":true}),
                deadline,
                cancelled,
                requests,
            )?;
            let items = data
                .get("items")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("Lark response missing candidate messages"))?;
            let mut candidates = Vec::new();
            for id in &ids {
                let matches: Vec<_> = items
                    .iter()
                    .filter(|item| text(item, "message_id") == *id)
                    .collect();
                ensure!(
                    matches.len() == 1,
                    "Conversation candidates could not be fully verified"
                );
                let item = matches[0];
                ensure!(
                    text(item, "msg_type") == "text"
                        && item.get("deleted").and_then(Value::as_bool) != Some(true),
                    "Conversation contains an unavailable or unsupported message; original content retained"
                );
                let body: Value = serde_json::from_str(
                    item.pointer("/body/content")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )
                .map_err(|_| anyhow::anyhow!("Invalid conversation message content"))?;
                let body = body
                    .get("text")
                    .and_then(Value::as_str)
                    .filter(|body| !body.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("Conversation message has no text"))?;
                let same_prefix = remaining_text == body
                    || remaining_text
                        .strip_prefix(body)
                        .is_some_and(|rest| rest.starts_with("\n\n"));
                if !same_prefix {
                    continue;
                }
                let candidate_chat = required(item, "chat_id")?;
                let candidate_position = item
                    .get("message_position")
                    .and_then(|value| {
                        value
                            .as_i64()
                            .or_else(|| value.as_str()?.parse::<i64>().ok())
                    })
                    .filter(|position| *position >= 0)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Conversation message has no verifiable order")
                    })?;
                if chat.as_ref().is_some_and(|chat| chat != &candidate_chat)
                    || position.is_some_and(|last| candidate_position <= last)
                {
                    continue;
                }
                let sender = item.get("sender").map(|sender| SourcePerson {
                    id: text(sender, "id"),
                    name: text(sender, "sender_name"),
                });
                let sent_at = item
                    .get("create_time")
                    .and_then(Value::as_str)
                    .filter(|value| value.parse::<i64>().ok().is_some_and(|value| value > 0))
                    .map(str::to_owned);
                candidates.push((
                    SourceMessage {
                        message_id: id.clone(),
                        content: body.to_owned(),
                    },
                    SourceMessageDetails { sender, sent_at },
                    candidate_chat,
                    candidate_position,
                ));
            }
            // 同文重复、不同会话和多种拆分均宁可不匹配，不挑第一项。
            ensure!(
                candidates.len() == 1,
                "No unique complete conversation match"
            );
            let (message, details, next_chat, next_position) = candidates.pop().unwrap();
            let consumed = message.content.len();
            matched.push((message, details));
            if consumed == remaining_text.len() {
                return Ok(matched);
            }
            remaining_text = &remaining_text[consumed + 2..];
            chat = Some(next_chat);
            position = Some(next_position);
        }
        bail!("Conversation exceeds the bounded matching limit; original content retained")
    }
}
