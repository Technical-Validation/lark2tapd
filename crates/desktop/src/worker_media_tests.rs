//! 真实 CLI 子进程边界 + SQLite + 提交流程，远端仅使用隔离替身。
use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
fn cli_image_submission_uploads_and_rewrites_body_after_normalized_readback() {
    cli_media_submission("story", None);
}

#[test]
fn bug_media_submission_recovers_readback_failures_without_reuploading() {
    for failure in [None, Some("create"), Some("final"), Some("upload")] {
        cli_media_submission("bug", failure);
    }
}

fn cli_media_submission(kind: &str, failure: Option<&str>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("cli");
    std::fs::write(&path,r#"#!/usr/bin/env python3
import sys,json,os
a=sys.argv[1:]
kind='__KIND__'
wrapper=kind.capitalize()
owner='current_owner' if kind=='bug' else 'owner'
if a==['auth','status','--json','--verify']:
 print(json.dumps({'appId':'app','brand':'feishu','identity':'user','verified':True,'identities':{'user':{'openId':'self'}}}))
elif a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'source'}],'has_more':False}}))
elif a[:3]==['task','tasks','get']:
 print(json.dumps({'ok':True,'data':{'task':{'guid':'source','summary':'Task','description':'source','creator':{'id':'self','name':'Creator'}}}}))
elif a[:2]==['user','workspaces']:
 print(json.dumps({'status':1,'data':[{'Workspace':{'id':'42','name':'Workspace'}}]}))
elif a[:2]==['story','fields']:
 print(json.dumps({'status':1,'data':{'workitem_type_id':{'options':{'7':'Development'}}}}))
elif a[:2]==['bug','fields']:
 print(json.dumps({'status':1,'data':{'title':{'name':'标题'}}}))
elif a[:2]==['user','members']:
 print(json.dumps({'status':1,'data':[{'UserWorkspace':{'user':'alice','name':'Alice'}}]}))
elif a[:2]==['attachment','upload-image']:
 p=dict(x.split('=',1) for x in a[2:])
 assert p['workspaceid']=='42'
 assert open(p['file'],'rb').read().startswith(b'\x89PNG\r\n\x1a\n')
 with open(__file__+'.uploads','a') as f:f.write('upload\n')
 print(json.dumps({'status':1,'data':{'image_src':'/tfl/pictures/source.png'}}))
elif a[:2]==['attachment','upload']:
 p=dict(x.split('=',1) for x in a[2:])
 assert p['type']==kind and p['entry-id']=='9' and p['workspaceid']=='42'
 assert open(p['file'],'rb').read()==b'ordinary attachment'
 with open(__file__+'.files','a') as f:f.write('upload\n')
 if os.path.exists(__file__+'.fail') and open(__file__+'.fail').read()=='upload':
  os.rename(__file__+'.fail',__file__+'.failed');sys.exit(2)
 print(json.dumps({'status':1,'data':{'Attachment':{'id':'77','type':kind,'entry_id':'9','workspace_id':'42'}}}))
elif a[0]==kind and a[1] in ['add','update']:
 p=dict(x.split('=',1) for x in a[2:]);p['id']='9'
 if kind=='bug':
  assert 'workitem_type_id' not in p and 'name' not in p and 'owner' not in p and 'due' not in p
  assert 'title' in p and 'current_owner' in p and 'deadline' in p
 else:
  if a[1]=='update': assert 'workitem_type_id' not in p
  p['workitem_type_id']='7'
 with open(__file__+'.json','w') as f:json.dump(p,f)
 with open(__file__+'.writes','a') as f:f.write(a[1]+'\n')
 print(json.dumps({'status':1,'data':{wrapper:{'id':'9'}}}))
elif a[:2]==[kind,'list']:
 writes=open(__file__+'.writes').read().splitlines()
 if os.path.exists(__file__+'.fail'):
  stage=open(__file__+'.fail').read()
  if (stage=='create' and len(writes)==1) or (stage=='final' and len(writes)==2):
   os.rename(__file__+'.fail',__file__+'.failed');sys.exit(2)
 p=json.load(open(__file__+'.json'));p[owner]+=';'
 p['description']='<p>'+p['description'].replace('&amp;','&#38;').replace('>\n<','><')+'</p>'
 print(json.dumps({'status':1,'data':[{wrapper:p}]}))
else:sys.exit(99)
"#.replace("__KIND__", kind)).unwrap();
    if let Some(failure) = failure {
        std::fs::write(root.path().join("cli.fail"), failure).unwrap();
    }
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let settings = Settings {
        lark_path: path.display().to_string(),
        tapd_path: path.display().to_string(),
        workspace_id: "42".into(),
        owner: "alice".into(),
        ..Default::default()
    };
    let mut credentials = Credentials::default();
    credentials.lark_app_id = "app".into();
    credentials.tapd_token = "test-token".into();
    let gateway = Connection::Live(Box::new(
        CliGateway::new(settings.clone(), credentials).unwrap(),
    ));
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut engine = Engine::open(&root.path().join("state.db")).await.unwrap();
        engine.save_settings(settings).await.unwrap();
        engine.refresh(&gateway).await.unwrap();
        engine.refresh_workspaces(&gateway).await.unwrap();
        engine.refresh_members(&gateway, "42").await.unwrap();
        engine.refresh_workitem_types(&gateway, "42").await.unwrap();
        let source = engine.snapshot().records[0].source.clone();
        let media = root.path().join("media");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(2, 2)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let attachment = lark2tapd_core::save_pasted_image(&media, png.get_ref(), "png").unwrap();
        let mut draft = engine.prepare(&source.guid).unwrap();
        draft.workitem_kind = kind.into();
        draft.workitem_type_id = if kind == "story" { "7" } else { "" }.into();
        draft.description = format!(
            "> Alice\n>\n> body & text\n>\n> ![图片](attachment:{})",
            attachment.id
        );
        let file = root.path().join("notes.txt");
        std::fs::write(&file, "ordinary attachment").unwrap();
        let ordinary = lark2tapd_core::media::copy_attachment(&media, &file).unwrap();
        draft.attachments = vec![attachment, ordinary];
        let result = submit(
            &mut engine,
            &gateway,
            &media,
            &source.guid,
            draft.clone(),
            source.clone(),
            None,
        )
        .await;
        if failure.is_some() {
            assert!(result.is_err());
            let record = &engine.snapshot().records[0];
            assert_eq!(record.uncertain, failure != Some("upload"));
            assert!(record.last_synced_at.is_none());
            assert_eq!(record.target.as_ref().unwrap().kind, kind);
            drop(engine);
            engine = Engine::open(&root.path().join("state.db")).await.unwrap();
            let writes = std::fs::read(root.path().join("cli.writes")).unwrap();
            assert!(
                submit(
                    &mut engine,
                    &gateway,
                    &media,
                    &source.guid,
                    draft.clone(),
                    source.clone(),
                    None
                )
                .await
                .is_err()
            );
            assert_eq!(
                std::fs::read(root.path().join("cli.writes")).unwrap(),
                writes
            );
            if failure == Some("upload") {
                // 服务端可能已收到普通附件；重启后不能重传它或已确认的图片。
                let saved = engine.snapshot().records[0].last_draft.as_ref().unwrap();
                assert!(saved.attachments[0].remote_url.is_some());
                assert!(saved.attachments[1].uncertain);
                assert_eq!(
                    std::fs::read_to_string(root.path().join("cli.files")).unwrap(),
                    "upload\n"
                );
                assert_eq!(
                    std::fs::read_to_string(root.path().join("cli.uploads")).unwrap(),
                    "upload\n"
                );
                return;
            }
            if failure == Some("final") {
                let saved = engine.snapshot().records[0].last_draft.as_ref().unwrap();
                assert!(saved.attachments[0].remote_url.is_some());
                assert_eq!(saved.attachments[1].remote_id.as_deref(), Some("77"));
            }
            engine
                .bind_existing(&gateway, &source.guid, "42", "9", kind)
                .await
                .unwrap();
            submit(
                &mut engine,
                &gateway,
                &media,
                &source.guid,
                draft.clone(),
                source.clone(),
                None,
            )
            .await
            .unwrap();
        } else {
            result.unwrap();
        }
        let record = &engine.snapshot().records[0];
        assert_eq!(record.target.as_ref().unwrap().kind, kind);
        assert_eq!(
            record.last_draft.as_ref().unwrap().attachments[1]
                .remote_id
                .as_deref(),
            Some("77")
        );
        assert!(!record.uncertain && !record.media_body_pending);
        let completed = record.last_synced_at.clone();
        assert!(
            completed
                .as_deref()
                .is_some_and(|time| chrono::DateTime::parse_from_rfc3339(time).is_ok())
        );
        let html = &record.target.as_ref().unwrap().description;
        assert!(html.contains("<img src=\"/tfl/pictures/source.png\""));
        assert!(!html.contains("图片未上传"));
        assert!(
            record.last_draft.as_ref().unwrap().attachments[0]
                .remote_url
                .is_some()
        );
        // 旧版草稿没有类型字段，更新时采用已绑定目标类型，不改变或重建目标。
        draft.workitem_type_id.clear();
        draft.workitem_kind = "story".into();
        submit(
            &mut engine,
            &gateway,
            &media,
            &source.guid,
            draft,
            source.clone(),
            None,
        )
        .await
        .unwrap();
        // 改标题和重启不能破坏 GUID → TAPD ID 的映射，也不能重复创建。
        drop(engine);
        let mut engine = Engine::open(&root.path().join("state.db")).await.unwrap();
        let mut renamed = engine.prepare(&source.guid).unwrap();
        renamed.title = "A completely different title".into();
        submit(
            &mut engine,
            &gateway,
            &media,
            &source.guid,
            renamed.clone(),
            source.clone(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            engine.snapshot().records[0].target.as_ref().unwrap().id,
            "9"
        );
        assert_eq!(
            engine.snapshot().records[0].target.as_ref().unwrap().title,
            renamed.title
        );
        let completed = engine.snapshot().records[0].last_synced_at.clone();
        renamed.workspace_id.clear();
        assert!(
            submit(
                &mut engine,
                &gateway,
                &media,
                &source.guid,
                renamed,
                source.clone(),
                None
            )
            .await
            .is_err()
        );
        assert_eq!(engine.snapshot().records[0].last_synced_at, completed);
        assert_eq!(
            std::fs::read_to_string(root.path().join("cli.files")).unwrap(),
            "upload\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("cli.uploads")).unwrap(),
            "upload\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("cli.writes"))
                .unwrap()
                .matches("add\n")
                .count(),
            1
        );
    });
}
