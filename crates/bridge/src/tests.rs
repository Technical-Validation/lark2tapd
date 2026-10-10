use super::*;
use std::{fs, os::unix::fs::PermissionsExt};

// 假可执行文件验证真正的进程边界，不调用用户安装的 CLI。
fn gateway(body: &str) -> (tempfile::TempDir, CliGateway) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake");
    fs::write(
        &path,
        format!("#!/usr/bin/env python3\nimport sys,json,os\na=sys.argv[1:]\n{body}\n"),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let settings = Settings {
        language: "en".into(),
        theme: "light".into(),
        poll_enabled: false,
        poll_minutes: 5,
        workspace_id: "42".into(),
        owner: "owner".into(),
        lark_path: path.to_str().unwrap().into(),
        tapd_path: path.to_str().unwrap().into(),
        tapd_endpoint: "https://api.tapd.cn".into(),
        ..Settings::default()
    };
    (
        dir,
        CliGateway::new(
            settings,
            Credentials {
                lark_app_id: "app".into(),
                lark_app_secret: "secret".into(),
                lark_user_token: "user-token".into(),
                tapd_token: "tapd-token".into(),
            },
        )
        .unwrap(),
    )
}
// OAuth 身份仅由假进程提供；检查每次刷新恰好验证一次。
fn oauth_gateway(body: &str) -> (tempfile::TempDir, CliGateway) {
    let (d, mut g) = gateway(&format!(
        r#"
if a==['auth','status','--json','--verify']:
 with open(__file__+'.auth','a') as f: f.write('verify\n')
 print('{{"identity":"user","verified":true,"identities":{{"user":{{"openId":"self"}}}}}}')
 sys.exit(0)
{body}
"#
    ));
    g.credentials.lark_user_token.clear();
    (d, g)
}
fn source() -> SourceTask {
    SourceTask {
        guid: "guid-1".into(),
        title: "task".into(),
        description: String::new(),
        url: "https://example.feishu.cn/task/1".into(),
        due: None,
        completed: false,
        metadata: SourceMetadata::default(),
    }
}
fn draft() -> Draft {
    Draft {
        title: "Title".into(),
        description: "<script>&\"'".into(),
        workspace_id: "42".into(),
        owner: "owner".into(),
        due: None,
    }
}
#[test]
fn related_union_paginates_filters_and_deduplicates_before_details() {
    let (d, g) = oauth_gateway(
        r#"
if a[:2]==['task','+get-related-tasks']:
 assert a[:8]==['task','+get-related-tasks','--as','user','--format','json','--page-limit','1']
 second='--page-token' in a
 if second: assert a[8:]==['--page-token','next']
 own={'id':'self','name':'快捷姓名'}
 items=([{'guid':'both','creator':own,'members':[dict(own,role='assignee')]},{'guid':'removed','creator':own}] if second else [
  {'guid':'creator','creator':own},
  {'guid':'assignee','creator':{'id':'other'},'members':[dict(own,role='assignee')]},
  {'guid':'both','creator':own,'members':[dict(own,role='assignee')]},
  {'guid':'followed','members':[dict(own,role='follower')]},
  {'guid':'other','creator':{'id':'other'}},
  {'guid':'missing','creator':{'name':'self'},'members':[{'role':'assignee'}]}])
 for t in items: t['created_at']='2025-01-01 08:00:00'
 print(json.dumps({'ok':True,'identity':'user','data':{'items':items,'has_more':not second,'page_token':'next' if not second else ''},'meta':{'count':len(items)}}))
else:
 assert a[:3]==['task','tasks','get']
 p=json.load(sys.stdin); guid=p['task_guid']
 assert guid in ['creator','assignee','both','removed']
 with open(__file__+'.details','a') as f: f.write(guid+'\n')
 t={'guid':guid,'summary':'Full','description':'Details','created_at':'1735689600000','completed_at':'0','due':{'timestamp':'1735689600000','is_all_day':True},'creator':{'id':'other' if guid in ['assignee','removed'] else 'self','name':'原生姓名'},'members':[],'url':'https://evil.example/'}
 if guid in ['assignee','both']: t['members']=[{'id':'self','name':'负责人','role':'assignee','type':'user'}]
 print(json.dumps({'ok':True,'data':{'task':t}}))
"#,
    );
    let tasks = g.fetch_tasks().unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.guid.as_str()).collect::<Vec<_>>(),
        ["creator", "assignee", "both"]
    );
    assert_eq!(
        fs::read_to_string(d.path().join("fake.auth")).unwrap(),
        "verify\n"
    );
    assert_eq!(
        fs::read_to_string(d.path().join("fake.details")).unwrap(),
        "creator\nassignee\nboth\nremoved\n"
    );
    assert_eq!(tasks[0].metadata.creator.as_ref().unwrap().name, "原生姓名");
    assert_eq!(tasks[1].metadata.assignees[0].name, "负责人");
    assert_eq!(
        tasks[0].metadata.created_at.as_deref(),
        Some("1735689600000")
    );
    assert_eq!(tasks[0].metadata.completed_at.as_deref(), Some("0"));
    assert_eq!(tasks[0].metadata.due_at.as_deref(), Some("1735689600000"));
    assert_eq!(tasks[0].description, "Details");
    assert!(tasks[0].due.is_some());
    assert!(tasks[0].url.is_empty());
}
#[test]
fn commit_source_read_verifies_self_and_selected_task_relation() {
    for (creator, accepted) in [("self", true), ("other", false), ("", false)] {
        let (_d, g) = oauth_gateway(&format!(
            r#"
assert a[:3]==['task','tasks','get']
p=json.load(sys.stdin)
assert p['task_guid']=='guid-1'
print(json.dumps({{'ok':True,'data':{{'task':{{'guid':'guid-1','summary':'selected','creator':{{'id':'{creator}'}}}}}}}}))
"#
        ));
        assert_eq!(g.get_source("guid-1").is_ok(), accepted);
        assert!(g.get_source("bad@id").is_err());
    }
}
#[test]
fn tapd_auth_check_without_workspace_does_not_read_a_project() {
    let (_d, mut g) =
        gateway("assert a==['user','info']; print('{\"status\":1,\"data\":{\"id\":\"1\"}}')");
    g.settings.workspace_id.clear();
    g.check_tapd().unwrap();
}
#[test]
fn draft_preflight_rejects_invalid_workspace_without_invoking_cli() {
    let (_d, g) = gateway("raise Exception('no commands allowed')");
    let mut d = draft();
    d.workspace_id = "demo".into();
    assert!(g.validate_draft(&d).is_err());
    d.workspace_id = "42".into();
    g.validate_draft(&d).unwrap();
    d.due = Some("2025-02-30".into());
    assert!(g.validate_draft(&d).is_err());
}
#[test]
fn invalid_source_ids_are_rejected_during_refresh() {
    let (_d, g) = oauth_gateway(
        r#"
if a[1]=='+get-related-tasks': print('{"ok":true,"data":{"items":[{"guid":"bad@id","creator":{"id":"self"}}],"has_more":false}}')
else: print('{"ok":true,"data":{"task":{"guid":"bad@id","creator":{"id":"self"},"summary":"title"}}}')
"#,
    );
    assert!(g.fetch_tasks().is_err());
}
#[test]
fn due_day_uses_local_calendar_not_utc_and_drops_time() {
    // 2025-01-01 00:00 +08:00 对应 UTC 前一天，不修改全局 TZ。
    let east = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    let west = chrono::FixedOffset::west_opt(8 * 3600).unwrap();
    assert_eq!(due_date(1735660800000, east).unwrap(), "2025-01-01");
    assert_eq!(due_date(1735689600000, west).unwrap(), "2024-12-31");
    assert!(due_date(i64::MAX, east).is_err());
}
#[test]
fn refresh_deadline_covers_all_pages_and_details() {
    let (_d, g) = oauth_gateway(
        r#"
import time
time.sleep(0.2)
if a[1]=='+get-related-tasks':
 print('{"ok":true,"data":{"items":[{"guid":"g1","creator":{"id":"self"}},{"guid":"g2","creator":{"id":"self"}},{"guid":"g3","creator":{"id":"self"}}],"has_more":false}}')
else:
 p=json.load(sys.stdin)
 print(json.dumps({'ok':True,'data':{'task':{'guid':p['task_guid'],'summary':'full'}}}))
"#,
    );
    let now = Instant::now();
    let result = g.fetch_tasks_until(now + Duration::from_millis(700));
    assert!(result.is_err());
    assert!(now.elapsed() < Duration::from_millis(1200));
}
#[test]
fn oversized_refresh_is_rejected_before_fetching_details() {
    let (_d, g) = oauth_gateway(
        r#"
if a[1]=='+get-related-tasks':
 print(json.dumps({'ok':True,'data':{'items':[{'guid':str(i),'creator':{'id':'self'}} for i in range(1001)],'has_more':False}}))
else:
 print('{"ok":false}')
"#,
    );
    assert!(g.fetch_tasks().unwrap_err().to_string().contains("1000"));
}
#[test]
fn rejects_partial_pagination_and_business_errors() {
    for body in [
        "print('{\"ok\":true,\"data\":{\"items\":[],\"has_more\":true}}')",
        "print('{\"ok\":false,\"error\":\"secret\"}')",
        "print('not JSON secret')",
    ] {
        let (_d, g) = oauth_gateway(body);
        let e = g.fetch_tasks().unwrap_err();
        assert!(!format!("{e:?}").contains("secret"));
    }
}
#[test]
fn create_detects_story_and_update_dispatches_same_kind() {
    let (_d, g) = gateway(
        r#"
assert os.environ['TAPD_TOKEN']=='tapd-token'
assert not any('token' in x for x in a)
p=dict(x.split('=',1) for x in a[2:])
assert p['workspaceid']=='42'
if a[1] in ['add','update']:
 assert a[:2]==(['task','add'] if a[1]=='add' else ['story','update'])
 assert '&lt;script&gt;&amp;&quot;&#39;' in p['description']
 assert '[lark-task:guid-1]' in p['description']
 assert 'https://example.feishu.cn/task/1' in p['description']
 with open(__file__+'.json','w') as f: json.dump(p,f)
else:
 assert a[:2]==['story','list'] and p['id']=='99'
 with open(__file__+'.json') as f: p=json.load(f)
t=({**p,'id':'99'} if a[1]=='list' else {'id':'99'})
print(json.dumps({'status':1,'data':[{'Story':t}] if a[1]=='list' else {'Story':t}}))
"#,
    );
    let created = g.create_target(&draft(), &source()).unwrap();
    assert_eq!(created.kind, "story");
    assert_eq!(created.title, "Title");
    assert_eq!(
        created.description,
        "&lt;script&gt;&amp;&quot;&#39;<p>[lark-task:guid-1]</p><p>https://example.feishu.cn/task/1</p>"
    );
    let updated = g.update_target(&created, &draft(), &source()).unwrap();
    assert_eq!(updated.kind, "story");
    assert_eq!(g.get_target("42", "99", "story").unwrap().id, "99");
}
#[test]
fn account_namespace_rejects_unverified_identity() {
    for body in [
        "print('{\"verified\":true}')",
        "print('{\"verified\":false,\"identities\":{\"user\":{\"openId\":\"user-a\"}}}')",
    ] {
        let (_d, mut g) = gateway(body);
        g.credentials.lark_user_token.clear();
        assert!(g.account_namespace().is_err());
    }
}
// 官方响应仅含公开标识；不同令牌和安装路径不能改变同一账户的库。
fn identity_gateway(
    lark_user: &str,
    tapd_user: &str,
    brand: &str,
    app: &str,
) -> (tempfile::TempDir, CliGateway) {
    let (d, mut g) = gateway(&format!(
        r#"
if a==['auth','status','--json','--verify']:
 print(json.dumps({{'appId':'{app}','brand':'{brand}','identity':'user','verified':True,'identities':{{'user':{{'openId':'{lark_user}'}}}}}}))
elif a==['user','info']:
 print(json.dumps({{'status':1,'data':{{'id':'{tapd_user}','nick':'public','enabled':'1'}}}}))
else: raise Exception('unexpected command')
"#
    ));
    g.credentials.lark_user_token.clear();
    g.credentials.lark_app_id = app.into();
    (d, g)
}
#[test]
fn namespace_survives_rotation_and_executable_relocation() {
    let (_d, a) = identity_gateway("u1", "1", "feishu", "app");
    let (_e, mut b) = identity_gateway("u1", "1", "feishu", "app");
    b.credentials.tapd_token = "rotated".into();
    b.credentials.lark_app_secret = "rotated-secret".into();
    assert_eq!(
        a.account_namespace().unwrap(),
        b.account_namespace().unwrap()
    );
}
#[test]
fn namespace_distinguishes_all_public_identity_components() {
    let (_d, a) = identity_gateway("u1", "1", "feishu", "app");
    for (u, t, b, app) in [
        ("u2", "1", "feishu", "app"),
        ("u1", "2", "feishu", "app"),
        ("u1", "1", "lark", "app"),
        ("u1", "1", "feishu", "app2"),
    ] {
        let (_e, other) = identity_gateway(u, t, b, app);
        assert_ne!(
            a.account_namespace().unwrap(),
            other.account_namespace().unwrap()
        );
    }
}
#[test]
fn namespace_requires_both_verified_public_subjects_and_app() {
    for patch in [
        "v['identity']='bot'",
        "v['verified']=False",
        "v['appId']='different-app'",
        "v['brand']='unknown'",
        "v['identities']['user']['openId']=''",
        "del v['appId']",
    ] {
        let (_d, mut g) = gateway(&format!(
            r#"
if a==['auth','status','--json','--verify']:
 v={{'appId':'app','brand':'feishu','identity':'user','verified':True,'identities':{{'user':{{'openId':'u1'}}}}}}
 {patch}
 print(json.dumps(v))
else: print('{{"status":1,"data":{{"id":"1"}}}}')
"#
        ));
        g.credentials.lark_user_token.clear();
        assert!(g.account_namespace().is_err(), "{patch}");
    }
    for response in [
        r#"{"status":0,"info":"secret"}"#,
        r#"{"status":1,"data":{}}"#,
        r#"{"status":1,"data":{"id":""}}"#,
    ] {
        let (_d, mut g) = gateway(&format!(
            r#"
if a==['auth','status','--json','--verify']:
 print('{{"appId":"app","brand":"feishu","identity":"user","verified":true,"identities":{{"user":{{"openId":"u1"}}}}}}')
else: print('{response}')
"#
        ));
        g.credentials.lark_user_token.clear();
        let error = g.account_namespace().unwrap_err().to_string();
        assert!(!error.contains("secret"));
    }
}
#[test]
fn manual_token_cannot_borrow_saved_oauth_subject() {
    let (_d, mut g) = identity_gateway("saved-account", "1", "feishu", "app");
    g.credentials.lark_user_token = "unknown-subject-token".into();
    assert!(
        g.account_namespace()
            .unwrap_err()
            .to_string()
            .contains("manual")
    );
}
#[test]
fn tapd_check_supports_story_only_projects() {
    let (_d, g) = gateway(
        r#"
if a==['user','info']: print('{"status":1,"data":{"id":"1"}}')
elif a[:2]==['story','list']: print('{"status":1,"data":[]}')
else: print('{"status":0}')
"#,
    );
    g.check_tapd().unwrap();
}
#[test]
fn ignored_write_fields_never_become_synced() {
    for (field, value) in [
        ("name", "ignored"),
        ("description", "ignored"),
        ("owner", "ignored"),
        ("due", "2027-01-01"),
        ("due", "__missing__"),
    ] {
        let (_d, g) = gateway(&format!(
            r#"
p=dict(x.split('=',1) for x in a[2:])
if a[1]=='update':
 with open(__file__+'.json','w') as f: json.dump(p,f)
 print('{{"status":1,"data":{{"Task":{{"id":"9"}}}}}}')
else:
 with open(__file__+'.json') as f: p=json.load(f)
 if '{value}'=='__missing__': del p['{field}']
 else: p['{field}']='{value}'
 print(json.dumps({{'status':1,'data':[{{'Task':p}}]}}))
"#
        ));
        let old = TargetTask {
            id: "9".into(),
            kind: "task".into(),
            title: "old".into(),
            description: String::new(),
            owner: "owner".into(),
            due: Some("2027-01-01".into()),
            url: String::new(),
        };
        assert!(
            g.update_target(&old, &draft(), &source()).is_err(),
            "ignored {field}"
        );
    }
}
#[test]
fn missing_tapd_token_does_not_fall_back_to_external_cli_account() {
    let (_d, mut g) = gateway("print('{\"status\":1,\"data\":[]}')");
    g.credentials.tapd_token.clear();
    assert!(g.check_tapd().is_err());
}
#[test]
fn rejects_unsafe_urls_and_tapd_business_failure() {
    for raw in [
        "javascript:alert(1)",
        "https://tapd.cn.evil.com/x",
        "https://evil.com@tapd.cn/x",
        "https://tapd.cn:444/x",
        "http://tapd.cn/x",
    ] {
        assert!(safe_url(raw, false).is_empty());
    }
    assert_eq!(
        safe_url("https://www.tapd.cn/x", false),
        "https://www.tapd.cn/x"
    );
    for body in [
        "print('{\"status\":0,\"info\":\"secret\"}')",
        "print('{\"status\":1,\"data\":[]}')",
    ] {
        let (_d, g) = gateway(body);
        assert!(g.get_target("42", "9", "task").is_err());
    }
}
#[test]
fn legacy_task_and_missing_id_are_not_confused() {
    let (_d, g) = gateway(
        "assert a[:2]==['task','list']; print('{\"status\":1,\"data\":[{\"Task\":{\"id\":\"9\",\"name\":\"n\",\"description\":\"d\",\"owner\":\"o\",\"due\":\"\"}}]}')",
    );
    assert_eq!(g.get_target("42", "9", "task").unwrap().kind, "task");
    let (_d, g) = gateway("print('{\"status\":1,\"data\":{\"Task\":{\"name\":\"n\"}}}')");
    assert!(g.create_target(&draft(), &source()).is_err());
}

// 分页控制必须拒绝重复游标、缺游标和超页数，不能用半份列表覆盖缓存。
#[test]
fn related_pagination_never_returns_partial_results() {
    for response in [
        r#"{'items':[],'has_more':True}"#,
        r#"{'items':[],'has_more':True,'page_token':'repeat'}"#,
        r#"{'items':[],'has_more':True,'page_token':str(int(a[-1])+1) if '--page-token' in a else '1'}"#,
        r#"{'items':[]}"#,
        r#"{'items':{},'has_more':False}"#,
    ] {
        let (_d, g) = oauth_gateway(&format!(
            "assert a[1]=='+get-related-tasks'; print(json.dumps({{'ok':True,'data':{response}}}))"
        ));
        assert!(g.fetch_tasks().is_err(), "{response}");
    }
}
#[test]
fn lark_connection_verifies_oauth_and_related_permission_without_tapd() {
    let (_d, mut g) = oauth_gateway(
        "assert a==['task','+get-related-tasks','--as','user','--format','json','--page-limit','1']; print('{\"ok\":true,\"data\":{\"items\":[],\"has_more\":false}}')",
    );
    g.settings.tapd_path = "/does/not/exist/tapd".into();
    g.check_lark().unwrap();
    g.credentials.lark_user_token = "manual-token".into();
    assert!(g.check_lark().is_err());
    assert!(g.fetch_tasks().is_err());
    assert!(g.get_source("guid-1").is_err());
}

// 管理员项目列表只认实际观察的 Workspace 包装；字段不全必须报错而非空下拉。
#[test]
fn workspace_directory_requires_observed_named_workspace_shape() {
    let (_d, g) = gateway(
        "assert a==['user','workspaces']; print('{\"status\":1,\"data\":[{\"Workspace\":{\"id\":\"42\",\"name\":\"项目甲\",\"pretty_name\":\"甲\"}}],\"info\":\"success\"}')",
    );
    let items = g.list_workspaces().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "42");
    assert_eq!(items[0].name, "项目甲");
    for data in [
        r#"{}"#,
        r#"[{"id":"42","name":"项目"}]"#,
        r#"[{"Workspace":{"id":"42"}}]"#,
        r#"[{"Workspace":{"id":"42","name":" "}}]"#,
        r#"[{"Workspace":{"id":"bad","name":"项目"}}]"#,
        r#"null"#,
    ] {
        let (_d, g) = gateway(&format!("print('{{\"status\":1,\"data\":{data}}}')"));
        assert!(g.list_workspaces().is_err(), "{data}");
    }
    let (_d, g) = gateway("print('{\"status\":1,\"data\":[]}')");
    assert!(g.list_workspaces().unwrap().is_empty());
}
#[test]
fn member_directory_uses_nickname_account_and_requires_meaningful_names() {
    let (_d, g) = gateway(
        "assert a==['user','members','workspaceid=42','fields=user,name']; print('{\"status\":1,\"data\":[{\"UserWorkspace\":{\"user\":\"nickname\",\"name\":\"显示名\",\"id\":\"999\"}}]}')",
    );
    let items = g.list_members("42").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].account, "nickname");
    assert_eq!(items[0].name, "显示名");
    g.verify_workspace("42").unwrap();
    assert!(g.verify_workspace("not-an-id").is_err());
    for data in [
        r#"{}"#,
        r#"[{"user":"nickname","name":"显示名"}]"#,
        r#"[{"UserWorkspace":{"user":"nickname"}}]"#,
        r#"[{"UserWorkspace":{"id":"999","name":"显示名"}}]"#,
        r#"[{"UserWorkspace":{"user":"nickname","name":" "}}]"#,
        r#"null"#,
    ] {
        let (_d, g) = gateway(&format!("print('{{\"status\":1,\"data\":{data}}}')"));
        assert!(g.list_members("42").is_err(), "{data}");
        assert!(g.verify_workspace("42").is_err(), "{data}");
    }
}
#[test]
fn native_metadata_rejects_formatted_time_and_preserves_unknown_names() {
    for (timestamp, accepted) in [
        ("1735689600000", true),
        ("2025-01-01 08:00:00", false),
        ("NaN", false),
    ] {
        let (_d, g) = oauth_gateway(&format!(
            r#"
assert a[:3]==['task','tasks','get']
print(json.dumps({{'ok':True,'data':{{'task':{{'guid':'guid-1','summary':'task','creator':{{'id':'self'}},'created_at':'{timestamp}'}}}}}}))
"#
        ));
        let result = g.get_source("guid-1");
        assert_eq!(result.is_ok(), accepted);
        if let Ok(task) = result {
            assert_eq!(task.metadata.creator.unwrap().name, "");
            assert_eq!(task.metadata.completed_at, None);
        }
    }
}

// 即使任务接口可读，也不能用机器人、未验证身份或空主体建立本人范围。
#[test]
fn task_reads_fail_closed_without_verified_user_identity() {
    for patch in [
        "v['verified']=False",
        "v['identity']='bot'",
        "v['identities']['user']['openId']=''",
        "v['identities']['user']['openId']=' '",
        "del v['identities']",
    ] {
        let (_d, mut g) = gateway(&format!(
            r#"
if a==['auth','status','--json','--verify']:
 v={{'identity':'user','verified':True,'identities':{{'user':{{'openId':'self'}}}}}}
 {patch}
 print(json.dumps(v))
elif a[1]=='+get-related-tasks':
 print('{{"ok":true,"data":{{"items":[{{"guid":"guid-1","creator":{{"id":"self"}}}}],"has_more":false}}}}')
else:
 print('{{"ok":true,"data":{{"task":{{"guid":"guid-1","summary":"readable","creator":{{"id":"self"}}}}}}}}')
"#
        ));
        g.credentials.lark_user_token.clear();
        assert!(g.fetch_tasks().is_err(), "{patch}");
        assert!(g.get_source("guid-1").is_err(), "{patch}");
        assert!(g.check_lark().is_err(), "{patch}");
    }
}

#[test]
fn page_limit_refuses_the_forty_first_page() {
    let (d, g) = oauth_gateway(
        r#"
assert a[1]=='+get-related-tasks'
n=int(a[-1]) if '--page-token' in a else 1
with open(__file__+'.pages','a') as f: f.write(str(n)+'\n')
print(json.dumps({'ok':True,'data':{'items':[],'has_more':True,'page_token':str(n+1)}}))
"#,
    );
    assert!(g.fetch_tasks().is_err());
    let calls = fs::read_to_string(d.path().join("fake.pages")).unwrap();
    assert_eq!(calls.lines().count(), 40);
    assert!(calls.ends_with("40\n"));
}

#[test]
fn connection_checks_ignore_other_provider_configuration() {
    let (_d, mut lark) = oauth_gateway(
        "assert a[:2]==['task','+get-related-tasks'];print('{\"ok\":true,\"data\":{\"items\":[],\"has_more\":false,\"page_token\":\"\"}}')",
    );
    lark.settings.tapd_path = "not-installed".into();
    lark.settings.tapd_endpoint = "not-configured".into();
    let lark = CliGateway::new(lark.settings, lark.credentials).unwrap();
    lark.check_lark().unwrap();
    let (_d, mut tapd) =
        gateway("assert a==['user','info'];print('{\"status\":1,\"data\":{\"id\":\"123\"}}')");
    tapd.settings.lark_path = "not-installed".into();
    tapd.settings.workspace_id.clear();
    let tapd = CliGateway::new(tapd.settings, tapd.credentials).unwrap();
    tapd.check_tapd().unwrap();
}
