#![cfg(target_os = "linux")]

//! One end-to-end Storage accounting contract, on a small made-up tree.
//!
//! The real filesystem inventory feeds the view adapter. Every measured byte must
//! appear once: either on a named row (active, settled or unowned) or in Other.
//! Ownership is only claimed when the inventory recorded evidence for it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use t3_agents_core::storage::{
    self, Coverage, Folder, FolderKind, InventoryInput, Owner, Project, Thread,
};
use t3_agents_core::storage_view::adapter::{ProjectMetadata, ThreadMetadata, convert};
use t3_agents_core::storage_view::data::{
    Bytes, Entry, Inventory as ViewInventory, Scan, Sizes, ThreadState,
};

const LIVE: &str = "11111111-1111-4111-8111-111111111111";
const GONE_A: &str = "22222222-2222-4222-8222-222222222222";
const GONE_B: &str = "33333333-3333-4333-8333-333333333333";
const PROJECT_SCRATCH: &str = "44444444-4444-4444-8444-444444444444";
const UNRELATED: &str = "55555555-5555-4555-8555-555555555555";
const NO_MARKER: &str = "66666666-6666-4666-8666-666666666666";
const LOOKALIKE: &str = "77777777-7777-4777-8777-777777777777";

struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "t3-storage-accounting-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn dir(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn file(&self, relative: &str, size: usize) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&vec![0x6a; size]).unwrap();
        file.sync_all().unwrap();
        path
    }

    fn empty(&self, relative: &str) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::File::create(&path).unwrap();
        path
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

// The same lossy mapping Claude Code uses for its scratch directory names.
fn encoded(path: &Path) -> String {
    path.to_str()
        .unwrap()
        .encode_utf16()
        .map(|unit| match unit {
            48..=57 | 65..=90 | 97..=122 => char::from_u32(u32::from(unit)).unwrap(),
            _ => '-',
        })
        .collect()
}

fn exact(bytes: Bytes) -> u64 {
    match bytes {
        Bytes::Absent => 0,
        Bytes::Exact(n) => n,
        other => panic!("expected measured bytes, got {other:?}"),
    }
}

fn footprint(sizes: Sizes) -> u64 {
    exact(sizes.worktree) + exact(sizes.scratch)
}

fn folder<'a>(inventory: &'a storage::Inventory, path: &Path) -> &'a Folder {
    inventory
        .folders
        .iter()
        .find(|folder| folder.path == path)
        .unwrap_or_else(|| panic!("no inventory folder at {}", path.display()))
}

fn entries(view: &ViewInventory) -> impl Iterator<Item = &Entry> {
    view.projects
        .iter()
        .flat_map(|project| project.threads.iter())
        .chain(view.settled_or_gone.iter())
}

#[test]
fn measured_rows_partition_into_named_and_other_bytes_once() {
    let tree = Tree::new();
    let project_root = tree.dir("checkout/repo");
    let managed = tree.dir("worktrees");
    let live = tree.dir("worktrees/repo/active");
    let settled = tree.dir("worktrees/repo/settled");
    let cleanup = tree.dir("worktrees/repo/cleanup-955ec46f9bec0a4c-2");
    let detached = tree.dir("worktrees/detached");
    let temp = tree.dir("temp");

    tree.file("worktrees/repo/active/source", 8 * 1024);
    tree.file("worktrees/repo/active/target/artifact", 32 * 1024);
    tree.file("worktrees/repo/settled/source", 8 * 1024);
    tree.file("worktrees/repo/cleanup-955ec46f9bec0a4c-2/source", 8 * 1024);
    tree.file("worktrees/detached/source", 8 * 1024);
    tree.empty("worktrees/detached/.git");
    tree.file("checkout/repo/source", 8 * 1024);

    let live_scratch_rel = format!("temp/claude-42/{}/{}", encoded(&live), LIVE);
    let live_scratch = tree.dir(&live_scratch_rel);
    tree.file(&format!("{live_scratch_rel}/scratchpad/notes"), 4 * 1024);

    // A removed managed worktree leaves only Claude's namespace behind. Two sessions
    // under that namespace must become one row, not two.
    let gone_worktree = managed.join("repo/cleanup-0123456789abcdef");
    assert!(!gone_worktree.exists());
    let gone_namespace = format!("temp/claude-42/{}", encoded(&gone_worktree));
    let gone_a_rel = format!("{gone_namespace}/{GONE_A}");
    let gone_b_rel = format!("{gone_namespace}/{GONE_B}");
    let gone_a = tree.dir(&gone_a_rel);
    let gone_b = tree.dir(&gone_b_rel);
    tree.file(&format!("{gone_a_rel}/scratchpad/notes"), 4 * 1024);
    tree.file(&format!("{gone_b_rel}/tasks/one"), 8 * 1024);

    // Scratch on the project checkout is not evidence that a thread is gone.
    let project_scratch_rel = format!(
        "temp/claude-42/{}/{}",
        encoded(&project_root),
        PROJECT_SCRATCH
    );
    let project_scratch = tree.dir(&project_scratch_rel);
    tree.file(
        &format!("{project_scratch_rel}/scratchpad/notes"),
        96 * 1024,
    );

    // An absolute path outside the managed root is unrelated.
    let unrelated_cwd = tree.0.join("elsewhere/project");
    let unrelated_rel = format!("temp/claude-42/{}/{}", encoded(&unrelated_cwd), UNRELATED);
    let unrelated = tree.dir(&unrelated_rel);
    tree.file(&format!("{unrelated_rel}/scratchpad/notes"), 40 * 1024);

    // A sibling that only shares the managed root's encoded prefix is a look-alike,
    // not a namespace under the root.
    let lookalike_cwd = PathBuf::from(format!("{}-archive/project", managed.display()));
    let lookalike_rel = format!("temp/claude-42/{}/{}", encoded(&lookalike_cwd), LOOKALIKE);
    let lookalike = tree.dir(&lookalike_rel);
    tree.file(&format!("{lookalike_rel}/scratchpad/notes"), 28 * 1024);

    // Under the managed root, but without Claude's task/scratchpad evidence.
    let no_marker_cwd = managed.join("repo/cleanup-fedcba9876543210");
    let no_marker_rel = format!("temp/claude-42/{}/{}", encoded(&no_marker_cwd), NO_MARKER);
    let no_marker = tree.dir(&no_marker_rel);
    tree.file(&format!("{no_marker_rel}/notes"), 20 * 1024);

    let projects = [Project {
        id: "repo",
        root: &project_root,
    }];
    let threads = [
        Thread {
            id: "live",
            project_id: "repo",
            cwd: &live,
            settled: false,
        },
        Thread {
            id: "old",
            project_id: "repo",
            cwd: &settled,
            settled: true,
        },
    ];
    let source = storage::inventory(
        &InventoryInput {
            worktree_root: &managed,
            projects: &projects,
            threads: &threads,
            temp_dir: &temp,
            uid: 42,
        },
        || false,
    )
    .unwrap();

    assert_eq!(
        (source.worktrees, source.claude),
        (Coverage::Complete, Coverage::Complete)
    );
    assert_eq!(source.worktree_root.as_deref(), Some(managed.as_path()));

    assert_eq!(
        folder(&source, &live).owner,
        Owner::Thread {
            id: "live".into(),
            settled: false
        }
    );
    assert_eq!(
        folder(&source, &settled).owner,
        Owner::Thread {
            id: "old".into(),
            settled: true
        }
    );
    assert_eq!(folder(&source, &cleanup).owner, Owner::Missing);
    assert_eq!(
        folder(&source, &cleanup).project_id.as_deref(),
        Some("repo")
    );
    assert_eq!(folder(&source, &detached).owner, Owner::Missing);
    assert_eq!(folder(&source, &detached).project_id, None);
    assert_eq!(folder(&source, &project_scratch).owner, Owner::Missing);
    assert_eq!(
        folder(&source, &project_scratch).project_id.as_deref(),
        Some("repo")
    );
    for (path, kind) in [
        (&live_scratch, FolderKind::Scratch),
        (&gone_a, FolderKind::Scratch),
        (&gone_b, FolderKind::Scratch),
        (&unrelated, FolderKind::Scratch),
        (&lookalike, FolderKind::Scratch),
    ] {
        assert_eq!(folder(&source, path).kind, kind);
    }
    for path in [&gone_a, &gone_b, &unrelated, &lookalike] {
        assert_eq!(folder(&source, path).owner, Owner::Missing);
        assert_eq!(folder(&source, path).project_id, None);
    }
    assert!(
        !source.folders.iter().any(|folder| folder.path == no_marker),
        "a namespace without Claude's session evidence is not a folder row"
    );

    let view = convert(
        &source,
        &[ProjectMetadata {
            id: "repo",
            title: "Repo",
        }],
        &[
            ThreadMetadata {
                id: "live",
                title: "Live thread",
                state: ThreadState::Quiet,
                age: None,
            },
            ThreadMetadata {
                id: "old",
                title: "Old thread",
                state: ThreadState::Settled,
                age: None,
            },
        ],
        Vec::new(),
        None,
        Scan::Finished {
            folders: source.folders.len(),
            incomplete: 0,
            elapsed: None,
            finished_at: None,
        },
    );

    let rows: Vec<&Entry> = entries(&view).collect();
    let row = |title: &str| {
        rows.iter()
            .copied()
            .find(|entry| entry.title == title)
            .unwrap_or_else(|| {
                panic!(
                    "missing row {title:?}; displayed: {:?}",
                    rows.iter()
                        .map(|entry| (&entry.title, entry.state))
                        .collect::<Vec<_>>()
                )
            })
    };

    let live_row = row("Live thread");
    assert_eq!(live_row.state, ThreadState::Quiet);
    assert_eq!(live_row.open_target.as_deref(), Some("live"));
    assert_eq!(
        live_row.sizes.worktree,
        Bytes::Exact(folder(&source, &live).usage.worktree_bytes)
    );
    assert_eq!(
        live_row.sizes.build,
        Bytes::Exact(folder(&source, &live).usage.dependency_bytes)
    );
    assert_eq!(
        live_row.sizes.scratch,
        Bytes::Exact(folder(&source, &live_scratch).usage.scratch_bytes)
    );

    let old_row = row("Old thread");
    assert_eq!(old_row.state, ThreadState::Settled);
    assert_eq!(
        old_row.sizes.worktree,
        Bytes::Exact(folder(&source, &settled).usage.worktree_bytes)
    );

    let cleanup_row = row("cleanup-955ec46f9bec0a4c-2");
    assert_eq!(cleanup_row.state, ThreadState::Gone);
    assert_eq!(cleanup_row.open_target, None);
    assert_eq!(
        cleanup_row.sizes.worktree,
        Bytes::Exact(folder(&source, &cleanup).usage.worktree_bytes)
    );
    assert_eq!(
        cleanup_row.sizes.build,
        Bytes::Exact(folder(&source, &cleanup).usage.dependency_bytes)
    );

    // Without a project, an orphaned managed worktree is still named, but it is
    // labelled unowned rather than tied to a project that the inventory never saw.
    let detached_row = row("unowned detached");
    assert_eq!(detached_row.state, ThreadState::Gone);
    assert_eq!(
        detached_row.sizes.worktree,
        Bytes::Exact(folder(&source, &detached).usage.worktree_bytes)
    );

    let gone_title = format!("unowned {}", encoded(&gone_worktree));
    let gone_rows: Vec<&Entry> = rows
        .iter()
        .copied()
        .filter(|entry| entry.title == gone_title)
        .collect();
    assert_eq!(
        gone_rows.len(),
        1,
        "one row per namespace, not per UUID session: {gone_title}"
    );
    assert_eq!(gone_rows[0].state, ThreadState::Gone);
    assert_eq!(
        gone_rows[0].sizes.scratch,
        Bytes::Exact(
            folder(&source, &gone_a).usage.scratch_bytes
                + folder(&source, &gone_b).usage.scratch_bytes
        )
    );

    // Neither project-checkout scratch nor unrelated scratch may be claimed gone.
    for (title, folder_path) in [
        (PROJECT_SCRATCH, &project_scratch),
        (UNRELATED, &unrelated),
        (LOOKALIKE, &lookalike),
    ] {
        assert!(
            rows.iter().all(|entry| entry.title != title),
            "{title} must stay in Other, not become a named row"
        );
        assert!(
            rows.iter().all(|entry| entry.state != ThreadState::Gone
                || entry.sizes.scratch
                    != Bytes::Exact(folder(&source, folder_path).usage.scratch_bytes)),
            "{title} must not be claimed gone"
        );
    }
    for namespace in [
        encoded(&project_root),
        encoded(&unrelated_cwd),
        encoded(&lookalike_cwd),
        encoded(&no_marker_cwd),
    ] {
        assert!(
            rows.iter().all(|entry| !entry.title.contains(&namespace)),
            "{namespace} must stay in Other, not become a named row"
        );
    }
    assert!(
        rows.iter().all(|entry| !entry.title.contains(NO_MARKER)),
        "a namespace without Claude's session evidence must stay in Other"
    );

    // Every measured byte appears exactly once, and BUILD stays a subset of WORKTREE.
    let other = view.other.as_ref().expect("unattributed bytes remain");
    for entry in &rows {
        assert!(
            exact(entry.sizes.build) <= exact(entry.sizes.worktree),
            "BUILD is a subset of WORKTREE for {:?}",
            entry.title
        );
    }
    assert!(exact(other.build) <= exact(other.worktree));

    let entry_worktree: u64 = rows.iter().map(|entry| exact(entry.sizes.worktree)).sum();
    let entry_build: u64 = rows.iter().map(|entry| exact(entry.sizes.build)).sum();
    let entry_scratch: u64 = rows.iter().map(|entry| exact(entry.sizes.scratch)).sum();
    let shown_worktree = entry_worktree + exact(other.worktree);
    let shown_scratch = entry_scratch + exact(other.scratch);
    let shown_build = entry_build + exact(other.build);
    assert_eq!(shown_worktree, source.total.worktree_bytes);
    assert_eq!(
        shown_scratch,
        source.total.scratch_bytes + source.total.other_bytes
    );
    assert_eq!(shown_build, source.total.dependency_bytes);
    assert_eq!(shown_worktree + shown_scratch, source.total.total_bytes());
    assert_eq!(
        rows.iter().map(|entry| footprint(entry.sizes)).sum::<u64>()
            + exact(other.worktree)
            + exact(other.scratch),
        source.total.total_bytes()
    );
}
