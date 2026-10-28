//! 替身 CLI 验证实际并发和完整性，不访问用户的凭证或服务。
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

// 两路服务端条件必须是并集；分页游标独立，重复任务只读一次详情。
#[test]
fn filtered_search_unions_creator_and_assignee_with_independent_pagination() {
    let (dir, gateway) = oauth_gateway(
        r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 assert a[3:7]==['--as','user','--format','json']
 params=json.loads(a[a.index('--params')+1]); body=json.load(sys.stdin)
 assert body['query']=='' and body['filter']['is_completed'] is False
 role='creator_ids' if 'creator_ids' in body['filter'] else 'assignee_ids'
 assert body['filter']=={role:['self'],'is_completed':False}
 second=params.get('page_token')=='next'
 with open(__file__+'.queries','a') as f: f.write(role+':'+str(second)+'\n')
 ids=(['both','removed','completed'] if second else ['creator','both']) if role=='creator_ids' else (['assignee'] if second else ['both'])
 print(json.dumps({'ok':True,'data':{'items':[{'id':id} for id in ids],'has_more':not second,'page_token':'next' if not second else ''}}))
elif a[:3]==['task','tasks','get']:
 p=json.load(sys.stdin);id=p['task_guid']
 with open(__file__+'.details','a') as f: f.write(id+'\n')
 t={'guid':id,'summary':'Title','creator':{'id':'self' if id in ['creator','both','completed'] else 'other'}}
 if id=='assignee': t['members']=[{'id':'self','role':'assignee'}]
 if id=='completed': t['completed_at']='1735689600000'
 print(json.dumps({'ok':True,'data':{'task':t}}))
else: sys.exit(99)
"#,
    );
    let tasks = gateway.fetch_tasks().unwrap();
    assert_eq!(
        tasks
            .iter()
            .map(|task| task.guid.as_str())
            .collect::<Vec<_>>(),
        ["creator", "both", "assignee"]
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.queries")).unwrap(),
        "creator_ids:False\ncreator_ids:True\nassignee_ids:False\nassignee_ids:True\n"
    );
    let details = fs::read_to_string(dir.path().join("fake.details")).unwrap();
    let mut ids = details.lines().collect::<Vec<_>>();
    ids.sort();
    assert_eq!(ids, ["assignee", "both", "completed", "creator", "removed"]);
}

#[test]
fn second_search_failure_or_incomplete_notice_never_returns_creator_only_tasks() {
    for result in [
        "{'ok':False,'error':{'type':'authorization','code':99991672}}",
        "{'ok':True,'data':{'items':[],'has_more':False,'notice':'incomplete'}}",
    ] {
        let (_dir, gateway) = oauth_gateway(&format!(
            r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 body=json.load(sys.stdin)
 if 'assignee_ids' in body['filter']: print(json.dumps({result}))
 else: print(json.dumps({{'ok':True,'data':{{'items':[{{'id':'created'}}],'has_more':False}}}}))
elif a[:3]==['task','tasks','get']:
 print(json.dumps({{'ok':True,'data':{{'task':{{'guid':'created','summary':'Task','creator':{{'id':'self'}}}}}}}}))
else: sys.exit(99)
"#
        ));
        assert!(gateway.fetch_tasks().is_err());
    }
}

#[test]
fn incomplete_filter_is_sent_on_every_page_including_after_an_empty_page() {
    let (dir, g) = creator_search_gateway(
        r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 second='page_token' in params
 if second: assert params['page_token']=='next'
 with open(__file__+'.pages','a') as f: f.write(('second' if second else 'first')+'\n')
 items=[{'id':'open'}] if second else []
 print(json.dumps({'ok':True,'data':{'items':items,'has_more':not second,'page_token':'next' if not second else ''}}))
else:
 p=json.load(sys.stdin);assert p['task_guid']=='open'
 print(json.dumps({'ok':True,'data':{'task':{'guid':'open','summary':'Open','completed_at':'0','creator':{'id':'self'}}}}))
"#,
    );
    let tasks = g.fetch_tasks().unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.guid.as_str()).collect::<Vec<_>>(),
        ["open"]
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.pages")).unwrap(),
        "first\nsecond\n"
    );
}

#[test]
fn tasks_completed_after_listing_are_excluded_but_overdue_open_tasks_remain() {
    let (_dir, g) = creator_search_gateway(
        r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':g,'creator':{'id':'self'},'completed_at':'0'} for g in ['overdue','just-completed','done-status']],'has_more':False}}))
else:
 p=json.load(sys.stdin);guid=p['task_guid']
 task={'guid':guid,'summary':'Task','creator':{'id':'self'},'completed_at':'0','due':{'timestamp':'1577836800000'}}
 if guid=='just-completed': task['completed_at']='1735689600000'
 if guid=='done-status': task['status']='done'
 print(json.dumps({'ok':True,'data':{'task':task}}))
"#,
    );
    let tasks = g.fetch_tasks().unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.guid.as_str()).collect::<Vec<_>>(),
        ["overdue"]
    );
    assert!(!tasks[0].completed);
    assert!(tasks[0].due.is_some());
}

#[test]
fn cancelling_during_rate_limit_cooldown_stops_without_retrying() {
    let (dir, g) = creator_search_gateway(
        r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'g0','creator':{'id':'self'}}],'has_more':False}}))
else:
 with open(__file__+'.attempts','a') as f: f.write('attempt\n')
 print(json.dumps({'ok':False,'error':{'type':'api','code':99991400,'retry_after_seconds':10}}));sys.exit(1)
"#,
    );
    let cancelled = AtomicBool::new(false);
    let mut saw_cooldown = false;
    let now = Instant::now();
    let error = crate::refresh::read(
        &g,
        now + Duration::from_secs(30),
        &cancelled,
        false,
        &mut |e| {
            if matches!(e.outcome, RefreshOutcome::RateLimited { .. }) {
                saw_cooldown = true;
                cancelled.store(true, Ordering::SeqCst);
            }
        },
    )
    .unwrap_err()
    .to_string();
    assert!(saw_cooldown && error.contains("cancelled"), "{error}");
    assert!(now.elapsed() < Duration::from_secs(3));
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.attempts")).unwrap(),
        "attempt\n"
    );
}

#[test]
fn other_api_errors_are_not_retried_even_when_marked_retryable() {
    let (dir, g) = creator_search_gateway(
        r#"
with open(__file__+'.attempts','a') as f: f.write('attempt\n')
print(json.dumps({'ok':False,'error':{'type':'authorization','code':99991672,'retryable':True,'retry_after_seconds':1,'message':'99991400'}}));sys.exit(1)
"#,
    );
    assert!(
        g.fetch_tasks()
            .unwrap_err()
            .to_string()
            .contains("99991672")
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.attempts")).unwrap(),
        "attempt\n"
    );
}

#[test]
fn tapd_writes_are_never_retried_by_read_rate_limiting() {
    let (dir, g) = gateway(
        r#"
assert a[:2]==['story','add']
with open(__file__+'.attempts','a') as f: f.write('attempt\n')
print(json.dumps({'status':0,'error':{'type':'api','code':99991400,'retry_after_seconds':1}}));sys.exit(1)
"#,
    );
    assert!(g.create_target(&draft(), &source()).is_err());
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.attempts")).unwrap(),
        "attempt\n"
    );
}

#[test]
fn rate_limited_detail_recovers_without_restarting_successful_reads() {
    let (dir, g) = creator_search_gateway(
        r#"
import time
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 with open(__file__+'.pages','a') as f: f.write('page\n')
 print(json.dumps({'ok':True,'data':{'items':[{'id':g,'creator':{'id':'self'}} for g in ['g0','g1']],'has_more':False}}))
else:
 p=json.load(sys.stdin);guid=p['task_guid'];path=__file__+'.'+guid
 previous=open(path).readlines() if os.path.exists(path) else []
 with open(path,'a') as f: f.write(str(time.time())+'\n')
 if guid=='g0' and not previous:
  print(json.dumps({'ok':False,'error':{'type':'api','code':99991400,'retry_after_seconds':2,'message':'private-secret'}}));sys.exit(1)
 print(json.dumps({'ok':True,'data':{'task':{'guid':guid,'summary':'private-title','creator':{'id':'self'}}}}))
"#,
    );
    let mut events = Vec::new();
    let (_, tasks) = crate::refresh::read(
        &g,
        Instant::now() + Duration::from_secs(10),
        &AtomicBool::new(false),
        false,
        &mut |e| events.push(e),
    )
    .unwrap();
    assert_eq!(
        tasks.iter().map(|t| t.guid.as_str()).collect::<Vec<_>>(),
        ["g0", "g1"]
    );
    let attempts: Vec<f64> = fs::read_to_string(dir.path().join("fake.g0"))
        .unwrap()
        .lines()
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(attempts.len(), 2);
    assert!(attempts[1] - attempts[0] >= 2.0, "respect server cooldown");
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.g1"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.pages")).unwrap(),
        "page\n"
    );
    assert!(format!("{events:?}").contains("RateLimited"));
    assert!(!format!("{events:?}").contains("private"));
}

#[test]
fn rate_limited_page_retries_are_bounded_and_never_return_partial_tasks() {
    let (dir, g) = creator_search_gateway(
        r#"
assert a[:3]==['api','POST','/open-apis/task/v2/tasks/search']
with open(__file__+'.attempts','a') as f: f.write('attempt\n')
print(json.dumps({'ok':False,'error':{'type':'api','code':99991400}}))
"#,
    );
    let mut events = Vec::new();
    let error = crate::refresh::read(
        &g,
        Instant::now() + Duration::from_secs(20),
        &AtomicBool::new(false),
        false,
        &mut |e| events.push(e),
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("99991400") && error.contains("Cached tasks unchanged"));
    let waits = events
        .iter()
        .filter_map(|e| match e.outcome {
            RefreshOutcome::RateLimited { attempt, wait_ms } => Some((attempt, wait_ms)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(waits, [(1, 1000), (2, 2000), (3, 4000)]);
    assert_eq!(
        events.last().unwrap().outcome,
        RefreshOutcome::Failed(RefreshFailure::RateLimited)
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.attempts"))
            .unwrap()
            .lines()
            .count(),
        4,
        "initial attempt and at most three retries"
    );
}

#[test]
fn rate_limit_wait_respects_refresh_budget() {
    let (dir, g) = creator_search_gateway(
        r#"
with open(__file__+'.attempts','a') as f: f.write('attempt\n')
print(json.dumps({'ok':False,'error':{'type':'api','code':99991400,'retry_after_seconds':120}}));sys.exit(1)
"#,
    );
    let now = Instant::now();
    let error = g
        .fetch_tasks_until(now + Duration::from_secs(2))
        .unwrap_err()
        .to_string();
    assert!(error.contains("refresh_budget_timeout"), "{error}");
    assert!(now.elapsed() < Duration::from_secs(3));
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.attempts")).unwrap(),
        "attempt\n"
    );
}

#[test]
fn detail_reads_across_pages_are_complete_and_keep_list_order() {
    let (dir, g) = creator_search_gateway(
        r#"
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 page=1 if 'page_token' not in params else 2
 if page==2: assert params['page_token']=='next'
 with open(__file__+'.pages','a') as f: f.write(str(page)+'\n')
 print(json.dumps({'ok':True,'data':{'items':[{'id':'g'+str(i),'creator':{'id':'self'}} for i in range((page-1)*4,page*4)],'has_more':page==1,'page_token':'next'}}))
else:
 p=json.load(sys.stdin)
 with open(__file__+'.details','a') as f: f.write(p['task_guid']+'\n')
 print(json.dumps({'ok':True,'data':{'task':{'guid':p['task_guid'],'summary':'title','creator':{'id':'self'}}}}))
"#,
    );
    let tasks = g.fetch_tasks().unwrap();
    let expected = ["g0", "g1", "g2", "g3", "g4", "g5", "g6", "g7"];
    assert_eq!(
        tasks.iter().map(|t| t.guid.as_str()).collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("fake.pages")).unwrap(),
        "1\n2\n"
    );
    // 子进程入口时刻不是 Rust 许可时刻；严格限速由 read_pacing 模块测试验证。
    let details = fs::read_to_string(dir.path().join("fake.details")).unwrap();
    let mut guids = details.lines().collect::<Vec<_>>();
    guids.sort_unstable();
    assert_eq!(guids, expected);
}

#[test]
fn account_refresh_reports_stages_and_reuses_verified_identity() {
    let (dir, mut g) = gateway(
        r#"
if a==['auth','status','--json','--verify']:
 with open(__file__+'.auth','a') as f: f.write('verified\n')
 print(json.dumps({'appId':'app','brand':'feishu','verified':True,'identity':'user','identities':{'user':{'openId':'private-user'}}}))
elif a==['user','info']: print(json.dumps({'status':1,'data':{'id':'123'}}))
elif a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'private-guid','creator':{'id':'private-user'}}],'has_more':False}}))
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
            RefreshPhase::TaskDetails,
            RefreshPhase::ListPage
        ]
    );
    assert_eq!(events.last().unwrap().completed, 1);
    assert!(!format!("{events:?}").contains("private"));
}

#[test]
fn cancelled_parallel_reads_stop_all_children_before_returning() {
    let (dir, g) = creator_search_gateway(
        r#"
import time,glob
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'g'+str(i),'creator':{'id':'self'}} for i in range(8)],'has_more':False}}))
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
    let (dir, g) = creator_search_gateway(
        r#"
import time,glob
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'g'+str(i),'creator':{'id':'self'}} for i in range(4)],'has_more':False}}))
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
    let (dir, g) = creator_search_gateway(
        r#"
import time,fcntl
if a[:3]==['api','POST','/open-apis/task/v2/tasks/search']:
 print(json.dumps({'ok':True,'data':{'items':[{'id':'g'+str(i),'creator':{'id':'self'}} for i in range(8)],'has_more':False}}))
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
