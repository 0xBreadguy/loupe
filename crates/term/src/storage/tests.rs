use super::*;
use std::sync::{Arc, mpsc};
use std::time::Duration;
use t3_agents_core::model::{Agent, Shelf, ThreadStorage};

const WAIT: Duration = Duration::from_secs(5);

fn agent(id: &str, project_id: &str, root: &Path, worktree: Option<&Path>) -> Agent {
    Agent {
        id: id.into(),
        title: format!("Thread {id}"),
        thread_id: id.into(),
        project: "Same title".into(),
        cwd: worktree.unwrap_or(root).to_str().unwrap().into(),
        storage: Some(ThreadStorage {
            project_id: project_id.into(),
            project_root: Some(root.to_str().unwrap().into()),
            worktree: worktree.map(|path| path.to_str().unwrap().into()),
        }),
        ..Agent::default()
    }
}

#[test]
fn owned_input_uses_local_thread_identity_and_only_explicit_worktrees() {
    let root = std::env::temp_dir();
    let main = Arc::new(agent("main", "one", &root.join("project"), None));
    let worktree = root.join("worktrees/child");
    let mut child = agent("child", "two", &root.join("other"), Some(&worktree));
    child.kind = "subagent".into();
    child.parent = Some("main".into());
    child.shelf = Some(Shelf {
        settled: true,
        ..Shelf::default()
    });
    let mut remote = agent("remote", "untrusted", &root, Some(&root.join("remote")));
    remote.remote = Some(t3_agents_core::model::AgentRemote::from_value(&serde_json::json!({"environmentId": "another-host", "threadId": "child"})));
    let input = StorageSource::Unavailable.input(&Snapshot {
        agents: vec![Arc::clone(&main), Arc::new(child), Arc::new(remote), main],
        ..Snapshot::default()
    });
    assert_eq!(
        input
            .projects
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert_eq!(
        input.threads.keys().map(String::as_str).collect::<Vec<_>>(),
        ["child", "main"]
    );
    assert_eq!(
        input.projects["one"].root.as_deref(),
        Some(root.join("project").as_path())
    );
    assert_eq!(input.threads["main"].worktree, None);
    assert_eq!(
        input.threads["child"].worktree.as_deref(),
        Some(worktree.as_path())
    );
    assert!(input.threads["child"].settled);
}

#[cfg(target_os = "linux")]
#[test]
fn default_roots_follow_t3_home_and_unix_temp_rules() {
    let scratch = std::env::temp_dir();
    let data = scratch.join(".t3");
    let db = data.join("userdata/statev2.sqlite");
    let temp = scratch.join("temps");
    let source = linux_source(
        &db,
        None,
        Some(scratch.as_os_str()),
        Some(&scratch),
        temp.clone(),
        42,
    );
    assert_eq!(
        source,
        StorageSource::Disk(ScanRoots {
            worktrees: data.join("worktrees"),
            temp: temp.clone(),
            uid: 42
        })
    );

    let db = Path::new("custom/userdata/statev2.sqlite");
    let source = linux_source(
        db,
        Some(OsStr::new("custom")),
        Some(scratch.as_os_str()),
        Some(&scratch),
        temp.clone(),
        42,
    );
    assert_eq!(
        source,
        StorageSource::Disk(ScanRoots {
            worktrees: scratch.join("custom/worktrees"),
            temp: temp.clone(),
            uid: 42
        })
    );
    let mismatched = linux_source(
        db,
        Some(OsStr::new("other")),
        Some(scratch.as_os_str()),
        Some(&scratch),
        temp.clone(),
        42,
    );
    assert_eq!(mismatched, StorageSource::Unavailable);
    assert_eq!(
        unix_temp_dir([Some(temp.as_os_str()), Some(scratch.as_os_str()), None]),
        temp
    );
    assert_eq!(
        unix_temp_dir([
            Some(OsStr::new("")),
            Some(temp.as_os_str()),
            Some(scratch.as_os_str())
        ]),
        temp
    );
    assert_eq!(unix_temp_dir([None, None, Some(temp.as_os_str())]), temp);
}

#[cfg(target_os = "linux")]
#[test]
fn explicit_db_needs_opt_in_to_scan_even_with_a_managed_looking_layout() {
    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let scratch =
        Scratch(std::env::temp_dir().join(format!("storage-adapter-{}", std::process::id())));
    std::fs::create_dir(&scratch.0).unwrap();
    let worktrees = scratch.0.join("worktrees");
    let checkout = worktrees.join("checkout");
    // Also place a linked-worktree marker in a known project root inside the managed
    // area. The fallback cwd must never make that checkout eligible for a scan.
    let project = worktrees.join("project");
    let temp = scratch.0.join("temp");
    for dir in [&checkout, &project, &temp.join("claude-42")] {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(".git"), "synthetic").unwrap();
    }
    let db = Database::Path {
        path: scratch.0.join("userdata/statev2.sqlite"),
        origin: Origin::Db,
    };
    let snapshot = Snapshot {
        agents: vec![
            Arc::new(agent("fallback", "p", &project, None)),
            Arc::new(agent("checkout", "p", &project, Some(&checkout))),
        ],
        ..Snapshot::default()
    };
    let automatic = StorageSource::for_database(&db);
    assert_eq!(automatic, StorageSource::Unavailable);
    assert_eq!(
        automatic.input(&snapshot).scan(|| false).unwrap(),
        Inventory::default()
    );

    let synthetic = StorageSource::Disk(ScanRoots {
        worktrees,
        temp,
        uid: 42,
    });
    let (wake, ready) = mpsc::channel();
    let mut worker = StorageWorker::new(move || {
        let _ = wake.send(());
    })
    .unwrap();
    worker.request(synthetic.input(&snapshot));
    ready.recv_timeout(WAIT).unwrap();
    assert!(worker.poll());
    let ScanState::Ready(result) = worker.state() else {
        panic!("inventory not ready")
    };
    assert_eq!(result.worktrees, Coverage::Complete);
    assert_eq!(result.claude, Coverage::Complete);
    assert_eq!(result.folders.len(), 1);
    assert_eq!(result.folders[0].path, checkout);
    assert_eq!(
        result.folders[0].owner,
        Owner::Thread {
            id: "checkout".into(),
            settled: false
        }
    );
    assert_eq!(result.threads["fallback"], Usage::default());
}

#[test]
fn demo_inventory_is_repeatable_and_has_no_disk_roots() {
    let base = 1_794_484_800_000.;
    let source = StorageSource::for_database(&Database::Demo);
    assert_eq!(source, StorageSource::Demo);
    let a = t3_agents_core::demo::snapshot(base, base).unwrap();
    let b = t3_agents_core::demo::snapshot(base + 5_000., base).unwrap();
    let first = source.input(&a);
    assert_eq!(first, source.input(&b));
    let result = first.scan(|| false).unwrap();
    assert_eq!(result, source.input(&b).scan(|| false).unwrap());
    assert!(result.threads["demo-checkout"].total_bytes() > 0);
    assert_eq!(
        result.total.total_bytes(),
        result
            .projects
            .values()
            .map(|u| u.total_bytes())
            .sum::<u64>()
            + result.other.total_bytes()
    );
    assert_eq!(first.scan(|| true), Err(Cancelled));
}

#[test]
fn worker_replaces_pending_input_and_never_publishes_a_stale_completion() {
    let (wake, ready) = mpsc::channel();
    let (started, starts) = mpsc::channel();
    let (release, unblocked) = mpsc::channel();
    let mut worker = StorageWorker::start(
        move || {
            let _ = wake.send(());
        },
        move |input, cancelled| {
            let path = input.threads["t"].worktree.clone().unwrap();
            started.send(path.clone()).unwrap();
            unblocked.recv_timeout(WAIT).unwrap();
            // An old syscall can return success even after cancellation.
            Ok(Inventory {
                total: Usage {
                    worktree_bytes: if cancelled() { 91 } else { 42 },
                    ..Usage::default()
                },
                ..Inventory::default()
            })
        },
    )
    .unwrap();
    let base = std::env::temp_dir();
    let input = |name: &str| {
        StorageSource::Unavailable.input(&Snapshot {
            agents: vec![Arc::new(agent("t", "p", &base, Some(&base.join(name))))],
            ..Snapshot::default()
        })
    };
    worker.request(input("old"));
    assert_eq!(starts.recv_timeout(WAIT).unwrap(), base.join("old"));
    worker.request(input("same-view-new-worktree"));
    worker.rescan(input("same-view-new-worktree"));
    worker.request(input("latest"));
    worker.request(input("latest"));
    assert!(matches!(worker.state(), ScanState::Scanning));
    release.send(()).unwrap();
    assert_eq!(starts.recv_timeout(WAIT).unwrap(), base.join("latest"));
    worker.request(input("latest"));
    assert!(!worker.poll());
    assert!(ready.try_recv().is_err());
    release.send(()).unwrap();
    ready.recv_timeout(WAIT).unwrap();
    assert!(worker.poll());
    let ScanState::Ready(result) = worker.state() else {
        panic!("inventory not ready")
    };
    assert_eq!(result.total.worktree_bytes, 42);
    assert_eq!(worker.input(), Some(&input("latest")));
    worker.request(input("latest"));
    assert!(matches!(worker.state(), ScanState::Ready(_)));
    worker.rescan(input("latest"));
    assert_eq!(starts.recv_timeout(WAIT).unwrap(), base.join("latest"));
    release.send(()).unwrap();
    ready.recv_timeout(WAIT).unwrap();
    worker.cancel(); // Cancel even a completed but unpolled result.
    assert!(!worker.poll());
    assert!(matches!(worker.state(), ScanState::Idle));
    assert!(worker.input().is_none());
}

#[test]
fn a_cancelled_rescan_does_not_replace_or_publish_the_last_completion() {
    use super::{Commit, Decision, Session};
    use std::time::Instant;
    use t3_agents_core::storage::Coverage;

    let base = std::env::temp_dir();
    let input = StorageSource::Unavailable.input(&Snapshot {
        agents: vec![Arc::new(agent("t", "p", &base, Some(&base.join("tree"))))],
        ..Snapshot::default()
    });
    let now = Instant::now();
    let mut session = Session::new(super::reentry::FRESH_FOR);
    let finished = Inventory {
        worktrees: Coverage::Complete,
        claude: Coverage::Complete,
        total: Usage {
            worktree_bytes: 10,
            ..Usage::default()
        },
        ..Inventory::default()
    };
    assert_eq!(session.commit(&input, &finished, now, 50), Commit::Fresh);

    let (release, unblocked) = mpsc::channel();
    let mut worker = StorageWorker::start(
        || {},
        move |_, cancelled| {
            unblocked.recv_timeout(WAIT).unwrap();
            if cancelled() {
                return Err(Cancelled);
            }
            Ok(Inventory {
                worktrees: Coverage::Complete,
                claude: Coverage::Complete,
                total: Usage {
                    worktree_bytes: 99,
                    ..Usage::default()
                },
                ..Inventory::default()
            })
        },
    )
    .unwrap();
    worker.rescan(input.clone());
    worker.cancel();
    release.send(()).unwrap();
    assert!(!worker.poll());
    assert!(matches!(worker.state(), ScanState::Idle));
    match session.decide(&input, now, false) {
        Decision::Keep { inventory, .. } => assert_eq!(inventory.total.worktree_bytes, 10),
        other => panic!("cancellation must keep the last completion, got {other:?}"),
    }
}

#[test]
fn dropping_worker_cancels_without_waiting_for_the_active_scan() {
    let (started, starts) = mpsc::channel();
    let (release, unblocked) = mpsc::channel();
    let (cancelled, stopped) = mpsc::channel();
    let mut worker = StorageWorker::start(
        || {},
        move |_, is_cancelled| {
            started.send(()).unwrap();
            unblocked.recv_timeout(WAIT).unwrap();
            cancelled.send(is_cancelled()).unwrap();
            Err(Cancelled)
        },
    )
    .unwrap();
    worker.request(StorageSource::Unavailable.input(&Snapshot::default()));
    starts.recv_timeout(WAIT).unwrap();
    drop(worker);
    release.send(()).unwrap();
    assert!(stopped.recv_timeout(WAIT).unwrap());
    assert_eq!(
        stopped.recv_timeout(WAIT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}
