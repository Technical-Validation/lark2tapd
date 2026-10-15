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
// 非零退出和 API 信封只保留结构化安全诊断，不能泄露消息、令牌或任务内容。
#[test]
fn safe_failures_preserve_exit_category_code_and_required_scope() {
    for exit in [0, 7] {
        let (_d, g) = gateway(&format!(
            r#"
print(json.dumps({{'status':0,'error':{{'type':'authorization','code':99991672,'missing_scopes':['task:task:read','secret-token'],'message':'secret-token task-content','hint':'secret-token'}}}}))
print('secret-token',file=sys.stderr)
sys.exit({exit})
"#
        ));
        let error = g.check_tapd().unwrap_err().to_string();
        assert!(error.contains("authorization"), "{error}");
        assert!(error.contains("99991672"), "{error}");
        assert!(error.contains("task:task:read"), "{error}");
        if exit != 0 {
            assert!(error.contains("exit 7"), "{error}");
        }
        assert!(!error.contains("secret-token") && !error.contains("task-content"));
    }
}

// 原生 OAuth 必须使用相同 profile；环境 App Secret 会激活不支持 auth 的 provider。
#[test]
fn native_oauth_profile_is_used_for_auth_and_business_reads() {
    let (_d, mut g) = gateway(
        r#"
assert not any(k in os.environ for k in ['LARKSUITE_CLI_APP_ID','LARKSUITE_CLI_APP_SECRET','LARKSUITE_CLI_USER_ACCESS_TOKEN'])
assert os.environ.get('LARKSUITE_CLI_PROFILE')=='app'
if a==['auth','status','--json','--verify']:
 print(json.dumps({'appId':'app','identity':'user','verified':True,'identities':{'user':{'openId':'self'}}}))
else:
 assert a==['task','+get-related-tasks','--as','user','--format','json','--page-limit','1']
 print(json.dumps({'ok':True,'data':{'items':[],'has_more':False}}))
"#,
    );
    g.credentials.lark_user_token.clear();
    g.check_lark().unwrap();
}

#[test]
fn device_login_is_split_and_rejects_partial_consent_and_untrusted_urls() {
    for (url, missing) in [
        (
            "https://accounts.feishu.cn/oauth/verify?user_code=public",
            false,
        ),
        ("https://accounts.larksuite.com/oauth/verify", true),
        ("https://accounts.feishu.cn.evil.test/", false),
        ("http://accounts.feishu.cn/", false),
        ("https://user@accounts.feishu.cn/", false),
    ] {
        let (_d, mut g) = gateway(&format!(
            r#"
assert 'LARKSUITE_CLI_APP_SECRET' not in os.environ
if a==['--version']: print('lark-cli version 1.0.96')
elif a==['auth','status','--json']:
 print(json.dumps({{'appId':'app','brand':'feishu','identity':'none','identities':{{}}}}))
elif a==['auth','login','--scope','task:task:read','--no-wait','--json']:
 print(json.dumps({{'verification_url':'{url}','device_code':'one-time-secret','expires_in':600}}))
elif a==['auth','login','--device-code','one-time-secret','--json']:
 print(json.dumps({{'event':'authorization_complete','user_open_id':'self','scope':'task:task:read','granted':['task:task:read'],'missing':{missing_json},'warning':{{'hint':'one-time-secret'}}}}))
 sys.exit({exit})
else: sys.exit(99)
"#,
            missing_json = if missing { "['task:task:read']" } else { "[]" },
            exit = if missing { 1 } else { 0 }
        ));
        g.credentials.lark_user_token.clear();
        let start = g.begin_lark_login();
        if url.starts_with("https://accounts.feishu.cn/")
            || url.starts_with("https://accounts.larksuite.com/")
        {
            let authorization = start.unwrap();
            assert_eq!(authorization.expires_in, 600);
            assert_eq!(authorization.verification_url, url);
            let result = g.finish_lark_login(&authorization);
            assert_eq!(result.is_err(), missing);
            if let Err(error) = result {
                assert!(error.to_string().contains("task:task:read"));
                assert!(!format!("{error:?}").contains("one-time-secret"));
            }
        } else {
            assert!(start.is_err());
        }
    }
}

#[test]
fn report_keeps_scope_evidence_and_ancillary_failure_does_not_disconnect() {
    let (_d, mut g) = gateway(
        r#"
if a==['--version']: print('lark-cli version 1.0.96')
elif a==['auth','status','--json','--verify']:
 print(json.dumps({'appId':'app','brand':'feishu','identity':'user','verified':True,'identities':{'user':{'openId':'self','scope':'task:task:read offline_access'}}}))
elif a==['auth','check','--scope','task:task:read','--json']:
 print(json.dumps({'ok':True,'granted':['task:task:read'],'missing':None}))
elif a==['auth','scopes','--json']:
 print(json.dumps({'ok':False,'error':{'type':'network','code':503,'message':'secret-content'}})); sys.exit(2)
else:
 assert a==['task','+get-related-tasks','--as','user','--format','json','--page-limit','1']
 print(json.dumps({'ok':True,'data':{'items':[],'has_more':False}}))
"#,
    );
    g.credentials.lark_user_token.clear();
    let report = g.connection_report(installer::CliTool::Lark);
    assert!(report.connected, "{report:?}");
    assert_eq!(
        report.user_scopes,
        Some(vec!["offline_access".into(), "task:task:read".into()])
    );
    assert!(report.app_scopes.is_none());
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.state == PermissionState::Unknown && c.detail.contains("503"))
    );
    assert!(!format!("{report:?}").contains("secret-content"));
}

#[test]
fn report_preserves_missing_scope_from_nonzero_check_and_partial_identity() {
    let (_d, mut g) = gateway(
        r#"
if a==['--version']: print('lark-cli version 1.0.96')
elif a==['auth','status','--json','--verify']:
 print(json.dumps({'appId':'app','identity':'user','verified':True,'identities':{'user':{'openId':'self'}}}))
elif a==['auth','check','--scope','task:task:read','--json']:
 print(json.dumps({'ok':False,'granted':None,'missing':['task:task:read']})); sys.exit(1)
elif a==['auth','scopes','--json']: print(json.dumps({'userScopes':['task:task:read']}))
else:
 print(json.dumps({'ok':False,'error':{'type':'authorization','code':99991672,'missing_scopes':['task:task:read']}})); sys.exit(1)
"#,
    );
    g.credentials.lark_user_token.clear();
    let report = g.connection_report(installer::CliTool::Lark);
    assert!(!report.connected);
    assert!(report.failure.is_some());
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "User identity" && c.state == PermissionState::Granted)
    );
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "task:task:read" && c.state == PermissionState::Missing)
    );
    assert_eq!(report.app_scopes, Some(vec!["task:task:read".into()]));
}

// profile add 在 CLI 内部拒绝碰撞和坏配置；只在显式保存后通过 stdin 提交凭据。
#[test]
fn explicit_profile_preparation_uses_stdin_and_never_switches_or_replaces_profiles() {
    for existing in [false, true] {
        let (dir, mut g) = gateway(&format!(
            r#"
assert 'LARKSUITE_CLI_APP_SECRET' not in os.environ
assert 'secret' not in a
if a==['--version']: print('lark-cli version 1.0.96')
elif a==['profile','list']:
 print(json.dumps([{{'name':'other','appId':'other-app','brand':'lark','active':True}}]{existing}))
else:
 assert a==['profile','add','--name','lark2tapd-app','--app-id','app','--app-secret-stdin','--brand','feishu']
 assert sys.stdin.read()=='secret\n'
 with open(__file__+'.added','w') as f: f.write('added')
"#,
            existing = if existing {
                "+[{'name':'existing','appId':'app','brand':'lark','active':False}]"
            } else {
                ""
            }
        ));
        g.credentials.lark_user_token.clear();
        g.prepare_lark_profile().unwrap();
        assert_eq!(dir.path().join("fake.added").exists(), !existing);
    }
}

#[test]
fn preparation_refuses_bad_config_collision_and_multiline_secret_without_writing() {
    for response in [
        "print(json.dumps({'ok':False,'error':{'type':'config','message':'secret'}})); sys.exit(2)",
        "print(json.dumps([{'name':'lark2tapd-app','appId':'unrelated','brand':'feishu'}]))",
        "print('not-json')",
    ] {
        let (_dir, mut g) = gateway(&format!(
            "if a==['--version']: print('lark-cli version 1.0.96')\nelif a==['profile','list']: {response}\nelse: sys.exit(99)"
        ));
        g.credentials.lark_user_token.clear();
        let error = g.prepare_lark_profile().unwrap_err().to_string();
        assert!(!error.contains("exit 99"));
        assert!(!error.contains("\"secret\""));
    }
    let (_d, mut g) = gateway("sys.exit(99)");
    g.credentials.lark_user_token.clear();
    g.credentials.lark_app_secret = "first\nsecond".into();
    assert!(
        !g.prepare_lark_profile()
            .unwrap_err()
            .to_string()
            .contains("exit 99")
    );
}

#[test]
fn mismatched_profile_cannot_authorize_or_read_tasks() {
    let (_dir, mut g) = gateway(
        r#"
if a==['--version']: print('lark-cli version 1.0.96')
elif a[:2]==['auth','status']:
 print(json.dumps({'appId':'other','identity':'user','verified':True,'identities':{'user':{'openId':'self'}}}))
else: sys.exit(99)
"#,
    );
    g.credentials.lark_user_token.clear();
    assert!(
        g.check_lark()
            .unwrap_err()
            .to_string()
            .contains("does not match")
    );
    assert!(
        g.begin_lark_login()
            .err()
            .unwrap()
            .to_string()
            .contains("does not match")
    );
}

#[test]
fn tapd_report_reads_selected_workspace_and_never_claims_write_permissions() {
    let (_d, g) = gateway(
        r#"
if a==['--version']: print('1.15.0')
elif a==['--help']: print('Usage: tapd-cli [options] [command]')
elif a==['user','info']: print(json.dumps({'status':1,'data':{'id':'123'}}))
else:
 assert a==['story','list','workspaceid=42','limit=1','page=1']
 print(json.dumps({'status':1,'data':[]}))
"#,
    );
    let report = g.connection_report(installer::CliTool::Tapd);
    assert!(report.connected, "{report:?}");
    for name in [
        "Token permission enumeration",
        "Task write",
        "Attachment upload",
    ] {
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.name == name && c.state == PermissionState::Unknown)
        );
    }
}

#[test]
fn tapd_initial_setup_accepts_known_versions_without_requiring_a_workspace() {
    for version in ["1.15.0", "1.8.1"] {
        let (_d, mut g) = gateway(&format!(
            r#"
if a==['--version']: print('{version}')
elif a==['--help']: print('Usage: tapd-cli [options] [command]')
else:
 assert a==['user','info']
 print(json.dumps({{'status':1,'data':{{'id':'123'}}}}))
"#
        ));
        g.settings.workspace_id.clear();
        let report = g.connection_report(installer::CliTool::Tapd);
        assert!(report.connected, "{version}: {report:?}");
        assert!(report.failure.is_none());
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.name == "Workspace read" && c.state == PermissionState::Unknown)
        );
    }
}

#[test]
fn app_scope_list_is_compared_to_required_permission_not_just_fetched() {
    for (scopes, expected) in [
        ("[]", PermissionState::Missing),
        ("['contact:user.base:readonly']", PermissionState::Missing),
        ("['task:task:read']", PermissionState::Granted),
        ("None", PermissionState::Unknown),
    ] {
        let (_d, mut g) = gateway(&format!(
            r#"
if a==['--version']: print('lark-cli version 1.0.96')
elif a==['auth','status','--json','--verify']:
 print(json.dumps({{'appId':'app','identity':'user','verified':True,'identities':{{'user':{{'openId':'self'}}}}}}))
elif a==['auth','check','--scope','task:task:read','--json']:
 print(json.dumps({{'ok':True,'granted':['task:task:read'],'missing':None}}))
elif a==['auth','scopes','--json']: print(json.dumps({{'userScopes':{scopes}}}))
else:
 assert a[:2]==['task','+get-related-tasks']
 print(json.dumps({{'ok':True,'data':{{'items':[],'has_more':False}}}}))
"#
        ));
        g.credentials.lark_user_token.clear();
        let report = g.connection_report(installer::CliTool::Lark);
        assert!(
            report.connected,
            "a successful read remains evidence even if ancillary metadata differs"
        );
        let check = report
            .checks
            .iter()
            .find(|c| c.name == "App task:task:read")
            .expect("required app permission comparison");
        assert_eq!(check.state, expected);
    }
}

#[test]
fn missing_scope_metadata_is_unknown_not_missing_when_read_succeeds() {
    let (_d, mut g) = gateway(
        r#"
if a==['--version']: print('lark-cli version 1.0.96')
elif a==['auth','status','--json','--verify']:
 print(json.dumps({'appId':'native','identity':'user','verified':True,'identities':{'user':{'openId':'self'}}}))
elif a==['auth','check','--scope','task:task:read','--json']:
 print(json.dumps({'ok':True,'granted':None,'missing':None}))
elif a==['auth','scopes','--json']: print(json.dumps({'userScopes':None}))
else:
 assert a[:2]==['task','+get-related-tasks']
 print(json.dumps({'ok':True,'data':{'items':[],'has_more':False}}))
"#,
    );
    g.credentials = Credentials::default();
    let report = g.connection_report(installer::CliTool::Lark);
    assert!(report.connected, "{report:?}");
    assert!(report.user_scopes.is_none() && report.app_scopes.is_none());
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.name == "task:task:read" && c.state == PermissionState::Unknown)
    );
}

#[test]
fn failed_identity_transport_does_not_probe_a_different_account() {
    let (dir, mut g) = gateway(
        r#"
if a==['--version']: print('lark-cli version 1.0.96')
elif a[:2]==['auth','status']:
 print(json.dumps({'appId':'different','identity':'user','verified':True,'identities':{'user':{'openId':'other'}}}))
else:
 with open(__file__+'.unexpected','w') as f: f.write('unexpected')
 sys.exit(99)
"#,
    );
    g.credentials.lark_user_token.clear();
    let report = g.connection_report(installer::CliTool::Lark);
    assert!(!report.connected);
    assert!(!dir.path().join("fake.unexpected").exists());
}

// OAuth 身份仅由假进程提供；检查每次刷新恰好验证一次。
fn oauth_gateway(body: &str) -> (tempfile::TempDir, CliGateway) {
    let (d, mut g) = gateway(&format!(
        r#"
if a==['auth','status','--json','--verify']:
 with open(__file__+'.auth','a') as f: f.write('verify\n')
 print('{{"appId":"app","identity":"user","verified":true,"identities":{{"user":{{"openId":"self"}}}}}}')
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
        title_prefix: String::new(),
        attachments: Vec::new(),
    }
}
// 读回筛选不是忽略响应身份的许可；空、重复和跨项目结果都不能标成功。
#[test]
fn attachment_readback_rejects_missing_ambiguous_and_conflicting_rows() {
    for rows in [
        "[]",
        "[{'Attachment':{'id':'77','type':'story','workspace_id':'42','entry_id':'8'}}]",
        "[{'Attachment':{'id':'78','type':'story','workspace_id':'42','entry_id':'9'}}]",
        "[{'Attachment':{'id':'77','type':'story','workspace_id':'43','entry_id':'9'}}]",
        "[{'Attachment':{'id':'77','type':'task','workspace_id':'42','entry_id':'9'}}]",
        "[{'Attachment':{'id':'77','type':'story','workspace_id':'42','entry_id':'9'}}]*2",
    ] {
        let (dir, g) = gateway(&format!(
            r#"
if a[1]=='upload':
 with open(__file__+'.calls','a') as f: f.write('upload\n')
 result={{'Attachment':{{'id':'77'}}}}
else:
 assert a[:2]==['attachment','list']
 result={rows}
print(json.dumps({{'status':1,'data':result}}))
"#
        ));
        let file = dir.path().join("document.txt");
        fs::write(&file, b"file content").unwrap();
        let a = lark2tapd_core::copy_attachment(dir.path(), &file).unwrap();
        assert!(
            g.upload_draft_attachment(dir.path(), "42", &upload_target(), &a)
                .is_err(),
            "{rows}"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("fake.calls")).unwrap(),
            "upload\n"
        );
    }
}
// CLI 的 JS 数字可能丢精度；精确筛选的附件列表必须再次证明大 ID 归属。
#[test]
fn large_numeric_parent_receipt_requires_filtered_readback() {
    let (dir, g) = gateway(
        r#"
p=dict(x.split('=',1) for x in a[2:])
if a[1]=='upload':
 assert p['entry-id']=='112233445566778899'
 result={'Attachment':{'id':'77','type':'story','entry_id':112233445566778896,'workspace_id':42}}
else:
 assert a==['attachment','list','workspaceid=42','entry_id=112233445566778899','id=77','type=story','limit=2','page=1']
 result=[{'Attachment':{'id':'77','type':'story','entry_id':112233445566778896,'workspace_id':'42'}}]
print(json.dumps({'status':1,'data':result}))
"#,
    );
    let file = dir.path().join("document.txt");
    fs::write(&file, b"file content").unwrap();
    let a = lark2tapd_core::copy_attachment(dir.path(), &file).unwrap();
    let mut target = upload_target();
    target.id = "112233445566778899".into();
    let receipt = g
        .upload_draft_attachment(dir.path(), "42", &target, &a)
        .unwrap();
    assert_eq!(receipt.remote_id.as_deref(), Some("77"));
}
// 本地限制在任何任务/附件写入前拒绝，错误不能泄露缓存路径。
#[test]
fn upload_preflight_rejects_changed_cache_webp_and_image_limit() {
    let dir = tempfile::tempdir().unwrap();
    let webp = lark2tapd_core::save_pasted_image(dir.path(), b"RIFF0000WEBPdata", "webp").unwrap();
    assert!(validate_attachment_for_upload(&webp).is_err());
    let mut bytes = vec![0; 5 * 1024 * 1024];
    bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
    let large = lark2tapd_core::save_pasted_image(dir.path(), &bytes, "png").unwrap();
    assert!(validate_attachment_for_upload(&large).is_err());
    let a =
        lark2tapd_core::save_pasted_image(dir.path(), b"\x89PNG\r\n\x1a\nvalid", "png").unwrap();
    fs::write(&a.path, b"tampered").unwrap();
    let error = validate_attachment_for_upload(&a).unwrap_err().to_string();
    assert!(!error.contains(&a.path));
}
#[test]
fn image_receipts_reject_traversal_and_untrusted_origins() {
    for src in [
        "/tfl/pictures/../x.png",
        "/tfl/pictures/%2e%2e/x.png",
        "/tfl/pictures/x.png?secret",
        "/tfl/pictures//x.png",
        "https://evil.example/x.png",
        "https://tapd.cn.evil.example/x.png",
        "javascript:x",
    ] {
        let (dir, g) = gateway(&format!(
            "print(json.dumps({{'status':1,'data':{{'image_src':{src:?}}}}}))"
        ));
        let a = lark2tapd_core::save_pasted_image(dir.path(), b"\x89PNG\r\n\x1a\nvalid", "png")
            .unwrap();
        assert!(
            g.upload_draft_attachment(dir.path(), "42", &upload_target(), &a)
                .is_err(),
            "{src}"
        );
    }
}
#[test]
fn upload_stages_image_extension_from_verified_content() {
    let (dir, g) = gateway(
        r#"
p=dict(x.split('=',1) for x in a[2:])
assert p['file'].endswith('.png')
print('{"status":1,"data":{"image_src":"/tfl/pictures/image.png"}}')
"#,
    );
    let source = dir.path().join("misnamed.txt");
    fs::write(&source, b"\x89PNG\r\n\x1a\nvalid").unwrap();
    let a = lark2tapd_core::copy_attachment(dir.path(), &source).unwrap();
    g.upload_draft_attachment(dir.path(), "42", &upload_target(), &a)
        .unwrap();
}
// 上传测试仅运行临时假进程；校验实际 argv、文件字节和失败时不重试。
#[test]
fn attachment_upload_uses_verified_bytes_and_checks_receipts() {
    for image in [false, true] {
        let (dir, g) = gateway(
            r#"
assert a[0]=='attachment'
p=dict(x.split('=',1) for x in a[2:])
assert p['workspaceid']=='42'
with open(p['file'],'rb') as f: data=f.read()
if a[1]=='upload-image':
 assert set(p)=={'workspaceid','file'} and data==b'\x89PNG\r\n\x1a\ncontent'
 result={'image_src':'/tfl/pictures/202608/image.png','html_code':'<img src="/tfl/pictures/202608/image.png"/>'}
else:
 assert a[1]=='upload' and p['type']=='story' and p['entry-id']=='9'
 assert data==b'file content'
 result={'Attachment':{'id':'77','type':'story','entry_id':9,'workspace_id':42}}
with open(__file__+'.calls','a') as f: f.write('upload\n')
print(json.dumps({'status':1,'data':result}))
"#,
        );
        let attachment = if image {
            lark2tapd_core::save_pasted_image(dir.path(), b"\x89PNG\r\n\x1a\ncontent", "png")
                .unwrap()
        } else {
            let file = dir.path().join("document.txt");
            fs::write(&file, b"file content").unwrap();
            lark2tapd_core::copy_attachment(dir.path(), &file).unwrap()
        };
        validate_attachment_for_upload(&attachment).unwrap();
        let receipt = g
            .upload_draft_attachment(dir.path(), "42", &upload_target(), &attachment)
            .unwrap();
        assert_eq!(
            receipt.remote_id.as_deref(),
            if image { None } else { Some("77") }
        );
        assert_eq!(
            receipt.remote_url.as_deref(),
            if image {
                Some("/tfl/pictures/202608/image.png")
            } else {
                None
            }
        );
        assert!(!receipt.uncertain);
        assert_eq!(
            fs::read_to_string(dir.path().join("fake.calls")).unwrap(),
            "upload\n"
        );
    }
}
fn upload_target() -> TargetTask {
    TargetTask {
        id: "9".into(),
        kind: "story".into(),
        title: "t".into(),
        description: String::new(),
        owner: String::new(),
        due: None,
        url: String::new(),
    }
}
#[test]
fn attachment_missing_or_conflicting_proof_is_uncertain_without_retry() {
    for response in [
        r#"{}"#,
        r#"{"Attachment":{"id":"77","type":"story","entry_id":8,"workspace_id":42}}"#,
        r#"{"Attachment":{"id":"77","type":"story","entry_id":9,"workspace_id":43}}"#,
    ] {
        let (dir, g) = gateway(&format!(
            "with open(__file__+'.calls','a') as f: f.write('1')\nprint('{{\"status\":1,\"data\":{response}}}')"
        ));
        let file = dir.path().join("doc");
        fs::write(&file, b"data").unwrap();
        let a = lark2tapd_core::copy_attachment(dir.path(), &file).unwrap();
        let error = g
            .upload_draft_attachment(dir.path(), "42", &upload_target(), &a)
            .unwrap_err()
            .to_string();
        assert!(error.contains("uncertain"), "{error}");
        assert_eq!(
            fs::read_to_string(dir.path().join("fake.calls")).unwrap(),
            "1"
        );
    }
}
#[test]
fn markdown_images_resolve_only_current_confirmed_attachment_ids() {
    let dir = tempfile::tempdir().unwrap();
    let mut a =
        lark2tapd_core::save_pasted_image(dir.path(), b"\x89PNG\r\n\x1a\ncontent", "png").unwrap();
    let mut d = draft();
    d.description = format!(
        "![inline](attachment:{}) ![reference][img]\n\n[img]: attachment:{}",
        a.id, a.id
    );
    d.attachments = vec![a.clone()];
    assert!(!render_description(&d, &source()).unwrap().contains("<img"));
    a.remote_url = Some("/tfl/pictures/202608/image.png".into());
    d.attachments = vec![a.clone()];
    let html = render_description(&d, &source()).unwrap();
    assert_eq!(
        html.matches("<img src=\"/tfl/pictures/202608/image.png\"")
            .count(),
        2,
        "{html}"
    );
    d.attachments[0].uncertain = true;
    assert!(!render_description(&d, &source()).unwrap().contains("<img"));
    d.attachments.clear();
    assert!(!render_description(&d, &source()).unwrap().contains("<img"));
}
// 只容忍已知 CLI 的单层段落包装，不能靠去标签掩盖数据丢失。
#[test]
fn readback_accepts_only_exact_generated_html_wrapper() {
    for (transform, accepted) in [
        ("'<p>'+p['description']+'</p>'", true),
        ("'<p>changed</p>'", false),
    ] {
        let (_dir, g) = gateway(&format!(
            r#"
p=dict(x.split('=',1) for x in a[2:])
if a[1]=='add':
 with open(__file__+'.json','w') as f: json.dump(p,f)
 print('{{"status":1,"data":{{"Task":{{"id":"9"}}}}}}')
else:
 with open(__file__+'.json') as f: p=json.load(f)
 p['id']='9'
 p['description']={transform}
 print(json.dumps({{'status':1,'data':[{{'Task':p}}]}}))
"#
        ));
        let mut d = draft();
        d.description = "**strong**".into();
        assert_eq!(g.create_target(&d, &source()).is_ok(), accepted);
    }
}
// 不可信 Markdown 不能加载跟踪图、本地路径，也不能创建非 HTTP(S) 链接。
#[test]
fn description_blocks_external_images_and_unsafe_links() {
    let mut d = draft();
    d.description = "![tracking](https://evil.example/pixel) ![disk](file:///tmp/secret) ![ref][img]\n\n[img]: https://evil.example/second\n\n[x](file:///tmp/secret) [mail](mailto:x@y.com) [ok](https://example.com)\n\n<script>alert(1)</script>".into();
    let html = render_description(&d, &source()).unwrap();
    assert!(!html.contains("<img"), "{html}");
    assert!(
        !html.contains("href=\"file:") && !html.contains("href=\"mailto:"),
        "{html}"
    );
    assert!(html.contains("href=\"https://example.com\""));
    assert!(!html.contains("<script>"));
    assert!(html.contains("[lark-task:guid-1]"));
}
// 回归：描述必须保留 Markdown 结构，而不是把所有内容当作普通文本。
#[test]
fn write_renders_markdown_instead_of_literal_syntax() {
    let (_dir, g) = gateway(
        r#"
p=dict(x.split('=',1) for x in a[2:])
if a[1]=='add':
 assert p['name']=='【需求】Title'
 assert '<h1>Heading</h1>' in p['description']
 assert '<strong>bold</strong>' in p['description']
 with open(__file__+'.json','w') as f: json.dump(p,f)
 print('{"status":1,"data":{"Task":{"id":"9"}}}')
else:
 with open(__file__+'.json') as f: p=json.load(f)
 p['id']='9'
 print(json.dumps({'status':1,'data':[{'Task':p}]}))
"#,
    );
    let mut d = draft();
    d.description = "# Heading\n\n**bold**".into();
    d.title_prefix = "【需求】".into();
    g.create_target(&d, &source()).unwrap();
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
 assert '&lt;script&gt;&amp;' in p['description'] and '<script>' not in p['description']
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
        "&lt;script&gt;&amp;&quot;'<p>[lark-task:guid-1]</p><p>https://example.feishu.cn/task/1</p>"
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
