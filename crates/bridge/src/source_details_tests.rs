//! 假 CLI 只替换网络边界；任务读取、关联和补查走生产代码。
use super::*;

#[test]
fn refresh_resolves_missing_names_once_by_exact_user_ids() {
    let (dir, gateway) = oauth_gateway(
        r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'first','creator':{'id':'self'}},{'id':'second','creator':{'id':'self'}}],'has_more':False}}))
elif a[:3]==['task','tasks','get']:
 p=json.load(sys.stdin)
 print(json.dumps({'ok':True,'data':{'task':{'guid':p['task_guid'],'summary':'任务','creator':{'id':'self','name':'创建人'},'members':[
  {'id':'ou_owner','role':'assignee','type':'user'},
  {'id':'ou_owner','role':'follower','type':'user'},
  {'id':'ou_missing','role':'follower','type':'user'},
  {'id':'ou_named','name':'任务内已有姓名','role':'follower','type':'user'}]}}}))
elif a[:2]==['contact','+search-user']:
 assert a[2:6]==['--as','user','--format','json']
 assert a[a.index('--user-ids')+1]=='ou_missing,ou_owner'
 assert a[a.index('--page-size')+1]=='30'
 assert '--query' not in a
 assert os.environ.get('LARKSUITE_CLI_PROFILE')=='app'
 with open(__file__+'.names','a') as f: f.write('lookup\n')
 print(json.dumps({'ok':True,'data':{'users':[
  {'open_id':'ou_owner','localized_name':'负责人姓名'},
  {'open_id':'ou_missing','localized_name':'ou_missing'},
  {'open_id':'ou_unrequested','localized_name':'无关人员'}],'has_more':False}}))
else: sys.exit(99)
"#,
    );
    let tasks = gateway.fetch_tasks().unwrap();
    assert_eq!(tasks.len(), 2);
    for task in tasks {
        let metadata = serde_json::to_value(&task.metadata).unwrap();
        assert_eq!(metadata["resolved_names"], json!({"ou_owner":"负责人姓名"}));
        assert_eq!(task.metadata.followers[2].name, "任务内已有姓名");
        assert!(!task.is_related_to("ou_missing"));
    }
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.names")).unwrap(),
        "lookup\n"
    );
}

#[test]
fn task_id_drives_message_and_sender_lookups_without_replacing_task_content() {
    let (dir, gateway) = oauth_gateway(
        r#"
if a[:3]==['task','tasks','get']:
 p=json.load(sys.stdin)
 print(json.dumps({'ok':True,'data':{'task':{'guid':p['task_guid'],'summary':'消息任务','creator':{'id':'self','name':'创建人'},'origin':{'refer_resources':[
  {'type':'message','source_message':{'message_id':'om_source','content':'**任务中的消息快照**'}},
  {'type':'message','source_message':{'message_id':'7684199632590769140','content':'数字消息 ID 的正文'}}]}}}}))
elif a[:2]==['api','GET']:
 assert a[2]=='/open-apis/im/v1/messages/om_source'
 assert a[3:]==['--as','user','--format','json','--params','-']
 assert json.load(sys.stdin)=={'user_id_type':'open_id'}
 with open(__file__+'.messages','a') as f: f.write('lookup\n')
 print(json.dumps({'ok':True,'data':{'items':[
  {'message_id':'om_other','create_time':'1','sender':{'id':'ou_other','sender_name':'错误关联'}},
  {'message_id':'om_source','msg_type':'text','create_time':'1735689600123','sender':{'id':'ou_sender','sender_type':'user'},'body':{'content':'{"text":"后来编辑的消息"}'}}]}}))
elif a[:2]==['contact','+search-user']:
 assert a[a.index('--user-ids')+1]=='ou_sender'
 print(json.dumps({'ok':True,'data':{'users':[{'open_id':'ou_sender','localized_name':'发送者姓名'}],'has_more':False}}))
else: sys.exit(99)
"#,
    );
    let task = gateway.get_source("source-task").unwrap();
    let metadata = serde_json::to_value(&task.metadata).unwrap();
    assert_eq!(metadata["source_messages"][0]["message_id"], "om_source");
    assert_eq!(
        metadata["source_messages"][0]["content"],
        "**任务中的消息快照**"
    );
    assert_eq!(
        metadata["source_messages"][1]["message_id"],
        "7684199632590769140"
    );
    assert_eq!(
        metadata["message_details"]["om_source"]["sent_at"],
        "1735689600123"
    );
    assert_eq!(
        metadata["message_details"]["om_source"]["sender"]["id"],
        "ou_sender"
    );
    assert_eq!(metadata["resolved_names"]["ou_sender"], "发送者姓名");
    assert!(metadata["message_details"].get("om_other").is_none());
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.messages")).unwrap(),
        "lookup\n"
    );
}

#[test]
fn enrichment_batches_users_and_skips_cancelled_or_exhausted_reads() {
    let (dir, gateway) = oauth_gateway(
        r#"
assert a[:2]==['contact','+search-user']
ids=a[a.index('--user-ids')+1].split(',')
assert 1 <= len(ids) <= 30
with open(__file__+'.batches','a') as f: f.write(str(len(ids))+'\n')
print(json.dumps({'ok':True,'data':{'users':[{'open_id':id,'localized_name':'姓名 '+id} for id in ids],'has_more':False}}))
"#,
    );
    let mut task = source();
    task.metadata.assignees = (0..31)
        .map(|index| SourcePerson {
            id: format!("ou_person{index:02}"),
            name: String::new(),
        })
        .collect();
    let tasks = std::slice::from_mut(&mut task);
    gateway.enrich_sources(
        tasks,
        Instant::now() + Duration::from_secs(5),
        &AtomicBool::new(false),
    );
    gateway.enrich_sources(
        tasks,
        Instant::now() + Duration::from_secs(120),
        &AtomicBool::new(true),
    );
    assert!(!dir.path().join("fake.batches").exists());
    gateway.enrich_sources(
        tasks,
        Instant::now() + Duration::from_secs(120),
        &AtomicBool::new(false),
    );
    assert_eq!(task.metadata.resolved_names.len(), 31);
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.batches")).unwrap(),
        "30\n1\n"
    );
}

// 手动验收入口只读：显式提供 CLI 和任务 GUID，默认测试绝不访问真实账号。
#[test]
#[ignore = "Requires an explicitly selected live CLI and task GUID"]
fn live_task_id_resolves_display_names_read_only() {
    let path = std::env::var("LARK2TAPD_LIVE_CLI").expect("explicit CLI path");
    let guid = std::env::var("LARK2TAPD_LIVE_TASK_GUID").expect("explicit task GUID");
    let gateway = CliGateway::new(
        Settings {
            lark_path: path,
            ..Default::default()
        },
        Credentials::default(),
    )
    .unwrap();
    let task = gateway.get_source(&guid).expect("read selected live task");
    assert_eq!(task.guid, guid);
    let people: Vec<_> = task
        .metadata
        .creator
        .iter()
        .chain(&task.metadata.assignees)
        .chain(&task.metadata.followers)
        .collect();
    assert!(!people.is_empty());
    assert!(
        people
            .iter()
            .all(|person| task.metadata.person_name(person).is_some()),
        "a selected task member name is unresolved"
    );
    if task.needs_conversation_resolution() {
        let enriched = gateway
            .resolve_source_conversation(&task, &AtomicBool::new(false))
            .expect("resolve the selected source conversation");
        let messages = enriched
            .metadata
            .source_messages
            .iter()
            .flat_map(|source| {
                enriched
                    .metadata
                    .matched_messages_for(source)
                    .unwrap_or(&[])
            })
            .collect::<Vec<_>>();
        assert!(!messages.is_empty(), "no per-message matches were resolved");
        assert!(
            messages.iter().all(|message| enriched
                .metadata
                .message_details
                .get(&message.message_id)
                .and_then(|details| details.sender.as_ref())
                .and_then(|person| enriched.metadata.person_name(person))
                .is_some()),
            "some message senders remain unresolved"
        );
        eprintln!(
            "Read-only conversation: {} individually attributed messages (text-matched)",
            messages.len()
        );
    }
    // 同时验证生产刷新入口，而非仅调用底层的单任务读取。
    let started = Instant::now();
    let tasks = gateway
        .fetch_tasks()
        .expect("read filtered unfinished tasks");
    assert!(
        tasks.iter().any(|task| task.guid == guid),
        "sample task is missing from the filtered refresh"
    );
    assert!(tasks.iter().all(|task| !task.completed));
    eprintln!(
        "Read-only refresh: {} unfinished tasks in {:.2}s",
        tasks.len(),
        started.elapsed().as_secs_f64()
    );
    eprintln!(
        "Read-only sample: {} members resolved; {} source message blocks; {} message metadata records",
        people.len(),
        task.metadata.source_messages.len(),
        task.metadata.message_details.len()
    );
}

#[test]
fn optional_lookup_failure_preserves_native_task_and_message_ids() {
    for response in [
        "print(json.dumps({'ok':False,'error':{'type':'authorization','code':99991679}})); sys.exit(1)",
        "print('invalid JSON')",
    ] {
        let (_dir, gateway) = oauth_gateway(&format!(
            r#"
if a[:3]==['task','tasks','get']:
 p=json.load(sys.stdin)
 print(json.dumps({{'ok':True,'data':{{'task':{{'guid':p['task_guid'],'summary':'任务','description':'原始正文','creator':{{'id':'self'}},'members':[{{'id':'ou_owner','role':'assignee','type':'user'}}],'origin':{{'refer_resources':[{{'type':'message','source_message':{{'message_id':'om_source','content':'保留来源正文'}}}}]}}}}}}}}))
else:
 {response}
"#
        ));
        let task = gateway.get_source("source-task").unwrap();
        let metadata = serde_json::to_value(&task.metadata).unwrap();
        assert_eq!(task.description, "原始正文");
        assert_eq!(metadata["source_messages"][0]["message_id"], "om_source");
        assert_eq!(metadata["source_messages"][0]["content"], "保留来源正文");
        assert_eq!(metadata["message_details"], json!({}));
        assert_eq!(metadata["resolved_names"], json!({}));
    }
}
