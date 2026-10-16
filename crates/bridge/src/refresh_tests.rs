//! 替身 CLI 验证实际并发和完整性，不访问用户的凭证或服务。
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

#[test]
fn account_refresh_reports_stages_and_reuses_verified_identity() {
    let (dir, mut g) = gateway(
        r#"
if a==['auth','status','--json','--verify']:
 with open(__file__+'.auth','a') as f: f.write('verified\n')
 print(json.dumps({'appId':'app','brand':'feishu','verified':True,'identity':'user','identities':{'user':{'openId':'private-user'}}}))
elif a==['user','info']: print(json.dumps({'status':1,'data':{'id':'123'}}))
elif a[:2]==['task','+get-related-tasks']:
 print(json.dumps({'ok':True,'data':{'items':[{'guid':'private-guid','creator':{'id':'private-user'}}],'has_more':False}}))
else:
 p=json.load(sys.stdin)
 print(json.dumps({'ok':True,'data':{'task':{'guid':p['task_guid'],'summary':'private-title','creator':{'id':'private-user'}}}}))
"#,
    );
    g.credentials.lark_user_token.clear();
    let mut events = Vec::new();
    let (namespace, tasks) = g
        .fetch_account_tasks(&AtomicBool::new(false), |e| events.push(e))
        .unwrap();
    assert_eq!(namespace.len(), 64);
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.auth")).unwrap(),
        "verified\n"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.outcome == RefreshOutcome::Started)
            .map(|e| e.phase)
            .collect::<Vec<_>>(),
        [
            RefreshPhase::LarkIdentity,
            RefreshPhase::TapdIdentity,
            RefreshPhase::ListPage,
            RefreshPhase::TaskDetails
        ]
    );
    assert_eq!(events.last().unwrap().completed, 1);
    assert!(!format!("{events:?}").contains("private"));
}

#[test]
fn cancelled_parallel_reads_stop_all_children_before_returning() {
    let (dir, g) = oauth_gateway(
        r#"
import time,glob
if a[:2]==['task','+get-related-tasks']:
 print(json.dumps({'ok':True,'data':{'items':[{'guid':'g'+str(i),'creator':{'id':'self'}} for i in range(8)],'has_more':False}}))
else:
 p=json.load(sys.stdin);guid=p['task_guid']
 open(__file__+'.started-'+guid,'w').close()
 while not os.path.exists(__file__+'.release'): time.sleep(0.005)
 open(__file__+'.escaped-'+guid,'w').close()
 print(json.dumps({'ok':True,'data':{'task':{'guid':guid,'summary':'should not return'}}}))
"#,
    );
    let cancelled = AtomicBool::new(false);
    let mut events = Vec::new();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let started = fs::read_dir(dir.path())
                    .unwrap()
                    .filter(|e| {
                        e.as_ref()
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with("fake.started-")
                    })
                    .count();
                if started == 4 {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "four details must start before cancellation"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            cancelled.store(true, Ordering::SeqCst);
        });
        let result = crate::refresh::read(
            &g,
            Instant::now() + Duration::from_secs(5),
            &cancelled,
            false,
            &mut |e| events.push(e),
        );
        assert!(result.is_err());
    });
    assert_eq!(
        events.last().unwrap().outcome,
        RefreshOutcome::Failed(RefreshFailure::Cancelled)
    );
    fs::write(dir.path().join("fake.release"), "").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("fake.escaped-")
    }));
    assert_eq!(
        fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| e
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("fake.started-"))
            .count(),
        4
    );
}

#[test]
fn one_detail_failure_cancels_siblings_and_never_returns_partial_tasks() {
    let (dir, g) = oauth_gateway(
        r#"
import time,glob
if a[:2]==['task','+get-related-tasks']:
 print(json.dumps({'ok':True,'data':{'items':[{'guid':'g'+str(i),'creator':{'id':'self'}} for i in range(4)],'has_more':False}}))
else:
 p=json.load(sys.stdin);guid=p['task_guid']
 open(__file__+'.started-'+guid,'w').close()
 while len(glob.glob(__file__+'.started-*'))<4: time.sleep(0.005)
 if guid=='g0':
  print(json.dumps({'ok':False,'error':{'type':'authorization','code':99991672,'message':'private-secret'}}));sys.exit(1)
 elif guid=='g1':
  print(json.dumps({'ok':True,'data':{'task':{'guid':guid,'summary':'private-partial','creator':{'id':'self'}}}}))
 else:
  while not os.path.exists(__file__+'.release'): time.sleep(0.005)
  open(__file__+'.escaped-'+guid,'w').close()
"#,
    );
    let cancelled = AtomicBool::new(false);
    let mut events = Vec::new();
    let error = crate::refresh::read(
        &g,
        Instant::now() + Duration::from_secs(5),
        &cancelled,
        false,
        &mut |e| events.push(e),
    )
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("99991672") && error.contains("task_details"),
        "{error}"
    );
    assert!(!error.contains("private"));
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(
        events.last().unwrap().outcome,
        RefreshOutcome::Failed(RefreshFailure::Other)
    );
    fs::write(dir.path().join("fake.release"), "").unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(!fs::read_dir(dir.path()).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("fake.escaped-")
    }));
}

#[test]
fn detail_reads_are_parallel_but_bounded_and_keep_list_order() {
    let (dir, g) = oauth_gateway(
        r#"
import time,fcntl
if a[:2]==['task','+get-related-tasks']:
 print(json.dumps({'ok':True,'data':{'items':[{'guid':'g'+str(i),'creator':{'id':'self'}} for i in range(8)],'has_more':False}}))
else:
 p=json.load(sys.stdin); guid=p['task_guid']
 def update(delta):
  with open(__file__+'.concurrency','a+') as f:
   fcntl.flock(f,fcntl.LOCK_EX);f.seek(0);s=f.read()
   active,peak=json.loads(s) if s else [0,0]
   active+=delta;peak=max(active,peak)
   f.seek(0);f.truncate();json.dump([active,peak],f);f.flush()
 update(1)
 time.sleep(0.15 if guid in ['g0','g4'] else 0.08)
 update(-1)
 print(json.dumps({'ok':True,'data':{'task':{'guid':guid,'summary':'private title','creator':{'id':'self'}}}}))
"#,
    );
    let tasks = g.fetch_tasks().unwrap();
    let peak: Vec<usize> =
        serde_json::from_str(&fs::read_to_string(dir.path().join("fake.concurrency")).unwrap())
            .unwrap();
    assert!(
        peak[1] > 1,
        "serial detail calls exhaust the fixed refresh budget"
    );
    assert!(peak[1] <= 4, "parallelism must stay bounded");
    assert_eq!(peak[0], 0);
    assert_eq!(
        tasks.iter().map(|t| t.guid.as_str()).collect::<Vec<_>>(),
        ["g0", "g1", "g2", "g3", "g4", "g5", "g6", "g7"]
    );
}
