//! 原文匹配必须覆盖整个来源，不能把整段会话归给第一个候选发送人。
use super::*;

fn matching_gateway(mode: &str) -> (tempfile::TempDir, CliGateway) {
    oauth_gateway(&format!(
        r#"
mode={mode:?}
messages=[
 {{'message_id':'om_first','chat_id':'oc_chat','message_position':'10','msg_type':'text','body':{{'content':json.dumps({{'text':'第一段\n\n仍属于第一条消息'}})}},'sender':{{'id':'ou_alice','sender_name':'Alice'}},'create_time':'1735689600000'}},
 {{'message_id':'om_second','chat_id':'oc_chat','message_position':12,'msg_type':'text','body':{{'content':json.dumps({{'text':'第二条消息'}})}},'sender':{{'id':'ou_bob','sender_name':'Bob'}},'create_time':'1735689601000'}},
 {{'message_id':'om_third','chat_id':'oc_chat','message_position':13,'msg_type':'text','body':{{'content':json.dumps({{'text':'第三条消息'}})}},'sender':{{'id':'ou_alice','sender_name':'Alice'}},'create_time':'1735689602000'}}]
if mode=='missing-names': messages[1]['sender']['sender_name']=''
if mode=='too-long':
 for i in range(6): messages.append(dict(messages[-1], message_id='om_extra'+str(i), message_position=14+i, body={{'content':json.dumps({{'text':'Extra '+str(i)}})}}))
content='\n\n'.join(json.loads(m['body']['content'])['text'] for m in messages)
if mode=='partial': content+='\n\n无法找到的结尾'
if a[:3]==['task','tasks','get']:
 p=json.load(sys.stdin)
 print(json.dumps({{'ok':True,'data':{{'task':{{'guid':p['task_guid'],'summary':'会话任务','description':'不覆盖描述','creator':{{'id':'self','name':'任务创建者'}},'origin':{{'refer_resources':[{{'type':'message','source_message':{{'message_id':'1234567890','content':content}}}}]}}}}}}}}))
elif a[:3]==['api','POST','/open-apis/im/v1/messages/search']:
 body=json.load(sys.stdin);params=json.loads(a[a.index('--params')+1])
 assert 1 <= params['page_size'] <= 20 and 'page_token' not in params
 assert 0 < len(body['query']) <= 50
 assert '--as' in a and a[a.index('--as')+1]=='user'
 idx=next((i for i,m in enumerate(messages) if json.loads(m['body']['content'])['text'].startswith(body['query'])),None)
 if idx is not None and idx>0: assert body['filter']=={{'chat_ids':['oc_chat']}}
 hits=[] if idx is None else [{{'meta_data':{{'message_id':messages[idx]['message_id']}}}}]
 if mode=='ambiguous' and idx==0: hits.append({{'meta_data':{{'message_id':'om_duplicate'}}}})
 with open(__file__+'.queries','a') as f:f.write('search\n')
 print(json.dumps({{'ok':True,'data':{{'items':hits,'has_more':mode=='truncated','notice':''}}}}))
elif a[:3]==['api','GET','/open-apis/im/v1/messages/mget']:
 params=json.load(sys.stdin)
 assert params['with_sender_name'] is True
 assert params['user_id_type']=='open_id'
 ids=params['message_ids'];items=[m for m in messages if m['message_id'] in ids]
 if 'om_duplicate' in ids:
  other=dict(messages[0],message_id='om_duplicate',chat_id='oc_other');items.append(other)
 if mode=='wrong-chat':
  for m in items:
   if m['message_id']=='om_second':m['chat_id']='oc_other'
 if mode=='wrong-order':
  for m in items:
   if m['message_id']=='om_second':m['message_position']=9
 print(json.dumps({{'ok':True,'data':{{'items':items}}}}))
elif a[:2]==['contact','+search-user']:
 assert a[a.index('--user-ids')+1]=='ou_bob'
 print(json.dumps({{'ok':True,'data':{{'users':[{{'open_id':'ou_bob','localized_name':'Bob'}}]}}}}))
else: sys.exit(99)
"#
    ))
}

#[test]
fn exact_complete_conversation_resolves_each_sender_without_splitting_paragraphs() {
    let (dir, gateway) = matching_gateway("complete");
    let source = gateway.get_source("source-task").unwrap();
    let enriched = gateway
        .resolve_source_conversation(&source, &AtomicBool::new(false))
        .unwrap();
    let metadata = serde_json::to_value(&enriched.metadata).unwrap();
    assert_eq!(
        metadata["matched_messages"]["1234567890"],
        json!([
            {"message_id":"om_first","content":"第一段\n\n仍属于第一条消息"},
            {"message_id":"om_second","content":"第二条消息"},
            {"message_id":"om_third","content":"第三条消息"}
        ])
    );
    assert_eq!(
        metadata["message_details"]["om_first"]["sender"]["name"],
        "Alice"
    );
    assert_eq!(
        metadata["message_details"]["om_second"]["sender"]["name"],
        "Bob"
    );
    assert_eq!(
        metadata["message_details"]["om_third"]["sender"]["name"],
        "Alice"
    );
    assert_eq!(enriched.description, "不覆盖描述");
    assert_eq!(
        enriched, source,
        "display matching must not change the task version"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.queries")).unwrap(),
        "search\nsearch\nsearch\n"
    );
}

#[test]
fn matched_sender_ids_are_resolved_to_names_without_exposing_ids() {
    let (_dir, gateway) = matching_gateway("missing-names");
    let source = gateway.get_source("source-task").unwrap();
    let enriched = gateway
        .resolve_source_conversation(&source, &AtomicBool::new(false))
        .unwrap();
    let sender = enriched.metadata.message_details["om_second"]
        .sender
        .as_ref()
        .unwrap();
    assert_eq!(enriched.metadata.person_name(sender), Some("Bob"));
    assert!(!enriched.needs_conversation_resolution());
}

#[test]
fn cancelled_stale_and_over_budget_matching_never_publishes_partial_authors() {
    let (dir, gateway) = matching_gateway("too-long");
    let source = gateway.get_source("source-task").unwrap();
    assert!(
        gateway
            .resolve_source_conversation(&source, &AtomicBool::new(true))
            .is_err()
    );
    assert!(!dir.path().join("fake.queries").exists());
    let mut stale = source.clone();
    stale.description.push_str("stale");
    assert!(
        gateway
            .resolve_source_conversation(&stale, &AtomicBool::new(false))
            .is_err()
    );
    assert!(!dir.path().join("fake.queries").exists());
    assert!(
        gateway
            .resolve_source_conversation(&source, &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.queries"))
            .unwrap()
            .lines()
            .count(),
        8
    );
}

#[test]
fn ambiguous_partial_cross_chat_and_reordered_candidates_are_not_published() {
    for mode in [
        "ambiguous",
        "partial",
        "truncated",
        "wrong-chat",
        "wrong-order",
    ] {
        let (_dir, gateway) = matching_gateway(mode);
        let source = gateway.get_source("source-task").unwrap();
        assert!(
            gateway
                .resolve_source_conversation(&source, &AtomicBool::new(false))
                .is_err(),
            "{mode}"
        );
    }
}
