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
fn source() -> SourceTask {
    SourceTask {
        guid: "guid-1".into(),
        title: "task".into(),
        description: String::new(),
        url: "https://example.feishu.cn/task/1".into(),
        due: None,
        completed: false,
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
fn paginates_and_fetches_full_details() {
    let (_d, g) = gateway(
        r#"
assert a[:2]==['task','tasks'] and '--params' in a and '--as' in a
assert os.environ['LARKSUITE_CLI_USER_ACCESS_TOKEN']=='user-token'
p=json.load(sys.stdin)
if a[2]=='list':
 assert p['type']=='my_tasks'
 second=p.get('page_token')=='next'
 print(json.dumps({'ok':True,'data':{'items':[{'guid':'b' if second else 'a'}],'has_more':not second,'page_token':'' if second else 'next'}}))
else:
 print(json.dumps({'ok':True,'data':{'task':{'guid':p['task_guid'],'summary':'Full','description':'Details','completed_at':'0','due':{'timestamp':'1735689600000','is_all_day':True},'url':'https://evil.example/'}}}))
"#,
    );
    let tasks = g.fetch_tasks().unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[1].guid, "b");
    assert_eq!(tasks[0].description, "Details");
    // 分页测试不假设执行机器的时区；日期语义由固定偏移回归单独验证。
    assert!(tasks[0].due.is_some());
    assert!(tasks[0].url.is_empty());
}
#[test]
fn commit_source_read_fetches_only_selected_task() {
    let (_d, g) = gateway(
        r#"
assert a[:3]==['task','tasks','get']
p=json.load(sys.stdin)
assert p['task_guid']=='guid-1'
print('{"ok":true,"data":{"task":{"guid":"guid-1","summary":"selected"}}}')
"#,
    );
    assert_eq!(g.get_source("guid-1").unwrap().title, "selected");
    assert!(g.get_source("bad@id").is_err());
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
    let (_d, g) = gateway(
        r#"
if a[2]=='list': print('{"ok":true,"data":{"items":[{"guid":"bad@id"}],"has_more":false}}')
else: print('{"ok":true,"data":{"task":{"guid":"bad@id","summary":"title"}}}')
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
    let (_d, g) = gateway(
        r#"
import time
time.sleep(0.2)
if a[2]=='list':
 print('{"ok":true,"data":{"items":[{"guid":"g1"},{"guid":"g2"},{"guid":"g3"}],"has_more":false}}')
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
    let (_d, g) = gateway(
        r#"
if a[2]=='list':
 print(json.dumps({'ok':True,'data':{'items':[{'guid':str(i)} for i in range(1001)],'has_more':False}}))
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
        let (_d, g) = gateway(body);
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
