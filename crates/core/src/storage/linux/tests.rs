use super::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, symlink};
use std::sync::atomic::{AtomicUsize, Ordering};

const SESSION: &str = "b1536030-b1e4-402f-bf15-05cf9c43ed67";

struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "storage-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn mkdir(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn scratch(&self, cwd: &Path) -> PathBuf {
        let encoded: String = cwd
            .to_str()
            .unwrap()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        self.mkdir(&format!("temp/claude-42/{encoded}/{SESSION}"))
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn write(path: &Path) -> u64 {
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&[0x6a; 32768]).unwrap();
    file.sync_all().unwrap();
    bytes(path)
}

fn bytes(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

fn row<'a>(result: &'a Inventory, path: &Path) -> &'a Folder {
    result
        .folders
        .iter()
        .find(|folder| folder.path == path)
        .unwrap()
}

#[test]
fn allocates_each_inode_once_and_never_walks_project_checkout() {
    let tree = Tree::new();
    let managed = tree.mkdir("worktrees");
    let project = tree.mkdir("checkout/repo");
    let named = tree.mkdir("worktrees/repo/owned");
    let second = tree.mkdir("worktrees/repo/second");
    let target = tree.mkdir("worktrees/repo/owned/nested/target");
    let modules = tree.mkdir("worktrees/repo/owned/node_modules");
    let sparse = named.join("sparse");
    fs::File::create(&sparse).unwrap().set_len(1 << 30).unwrap();
    let cache = write(&target.join("data"));
    fs::hard_link(target.join("data"), modules.join("same")).unwrap();
    let shared = write(&named.join("linked"));
    fs::hard_link(named.join("linked"), second.join("linked")).unwrap();
    write(&project.join("ignored"));
    symlink(&project, named.join("points-outside")).unwrap();
    let temp = tree.mkdir("temp");
    write(&temp.join("unrelated-to-claude"));
    let session = tree.scratch(&named);
    let scratch = write(&session.join("notes"));
    let fallback_scratch = tree.scratch(&project);
    let project_scratch = write(&fallback_scratch.join("notes"));

    let projects = [Project {
        id: "repo",
        root: &project,
    }];
    let threads = [
        Thread {
            id: "first",
            project_id: "repo",
            cwd: &named,
            settled: true,
        },
        Thread {
            id: "second",
            project_id: "repo",
            cwd: &second,
            settled: false,
        },
        Thread {
            id: "checkout",
            project_id: "repo",
            cwd: &project,
            settled: false,
        },
    ];
    let result = inventory(
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
        (result.worktrees, result.claude),
        (Coverage::Partial, Coverage::Complete)
    );
    assert_eq!(
        row(&result, &named).owner,
        Owner::Thread {
            id: "first".into(),
            settled: true
        }
    );
    assert_eq!(
        result.threads["first"],
        Usage {
            worktree_bytes: cache
                + [
                    &named,
                    &target,
                    &modules,
                    &named.join("nested"),
                    &sparse,
                    &named.join("points-outside")
                ]
                .into_iter()
                .map(|path| bytes(path))
                .sum::<u64>(),
            dependency_bytes: cache + bytes(&target) + bytes(&modules),
            scratch_bytes: scratch + bytes(&session),
            other_bytes: 0,
        }
    );
    assert_eq!(result.threads["second"].worktree_bytes, bytes(&second));
    assert_eq!(result.threads["checkout"], Usage::default());
    assert_eq!(
        result.projects["repo"].scratch_bytes,
        scratch + project_scratch + bytes(&session) + bytes(&fallback_scratch)
    );
    assert_eq!(result.other.worktree_bytes, shared);
    let overhead = bytes(&temp.join("claude-42"))
        + bytes(session.parent().unwrap())
        + bytes(fallback_scratch.parent().unwrap());
    assert_eq!(result.other.other_bytes, overhead);
    assert_eq!(
        result.total.total_bytes(),
        result.projects["repo"].total_bytes() + result.other.total_bytes()
    );
}

#[test]
fn labels_evidenced_orphans_and_sends_collision_and_unrelated_usage_to_other() {
    let tree = Tree::new();
    let managed = tree.mkdir("worktrees");
    let project = tree.mkdir("checkout/repo");
    let known = tree.mkdir("worktrees/repo/work.foo");
    let collision = tree.mkdir("worktrees/repo/work-foo");
    let orphan = tree.mkdir("worktrees/repo/cleanup-955ec46f9bec0a4c-2");
    let flat = tree.mkdir("worktrees/custom");
    write(&flat.join(".git"));
    let ignored = tree.mkdir("worktrees/repo/other");
    write(&ignored.join("ignore"));
    let temp = tree.mkdir("temp");
    let colliding_scratch = tree.scratch(&known);
    write(&colliding_scratch.join("data"));
    assert_eq!(colliding_scratch, tree.scratch(&collision));
    let orphan_scratch = tree.scratch(&orphan);
    write(&orphan_scratch.join("data"));
    let unknown = tree.mkdir(&format!("temp/claude-42/-unknown/{SESSION}"));
    fs::create_dir(unknown.join("scratchpad")).unwrap();
    let unrelated = tree.mkdir("temp/claude-42/logs/extra");
    let unrelated_data = write(&unrelated.join("log"));

    let result = inventory(
        &InventoryInput {
            worktree_root: &managed,
            projects: &[Project {
                id: "repo",
                root: &project,
            }],
            threads: &[
                Thread {
                    id: "known",
                    project_id: "repo",
                    cwd: &known,
                    settled: false,
                },
                Thread {
                    id: "collision",
                    project_id: "repo",
                    cwd: &collision,
                    settled: false,
                },
            ],
            temp_dir: &temp,
            uid: 42,
        },
        || false,
    )
    .unwrap();
    assert_eq!(row(&result, &orphan).owner, Owner::Missing);
    assert_eq!(row(&result, &flat).owner, Owner::Missing);
    assert_eq!(
        row(&result, &orphan_scratch).project_id.as_deref(),
        Some("repo")
    );
    assert_eq!(row(&result, &colliding_scratch).owner, Owner::Shared);
    assert_eq!(row(&result, &unknown).owner, Owner::Missing);
    assert_eq!(row(&result, &unknown).kind, FolderKind::Scratch);
    assert_eq!(row(&result, &unknown).project_id, None);
    assert!(
        !result
            .folders
            .iter()
            .any(|folder| folder.path == unrelated || folder.path == ignored)
    );
    assert_eq!(
        result.other.scratch_bytes,
        row(&result, &unknown).usage.scratch_bytes
            + row(&result, &colliding_scratch).usage.scratch_bytes
    );
    assert!(result.other.other_bytes >= unrelated_data);
}

#[test]
fn shared_nested_and_missing_references_preserve_coverage() {
    let tree = Tree::new();
    let managed = tree.mkdir("worktrees");
    let project = tree.mkdir("checkout/repo");
    let shared = tree.mkdir("worktrees/repo/shared");
    let nested = tree.mkdir("worktrees/repo/shared/nested");
    let nested_data = write(&nested.join("data"));
    let missing = managed.join("missing");
    let outside = tree.mkdir("outside");
    symlink(&outside, managed.join("symlink")).unwrap();
    let escaped = managed.join("symlink/nested");
    let temp = tree.mkdir("temp");
    let projects = [Project {
        id: "repo",
        root: &project,
    }];
    let threads: Vec<_> = [
        ("one", &shared),
        ("two", &shared),
        ("nested", &nested),
        ("missing", &missing),
        ("escape", &escaped),
    ]
    .into_iter()
    .map(|(id, cwd)| Thread {
        id,
        project_id: "repo",
        cwd,
        settled: false,
    })
    .collect();
    let mut input = InventoryInput {
        worktree_root: &managed,
        projects: &projects,
        threads: &threads,
        temp_dir: &temp,
        uid: 42,
    };
    let result = inventory(&input, || false).unwrap();
    assert_eq!(
        (result.worktrees, result.claude),
        (Coverage::Partial, Coverage::Complete)
    );
    assert_eq!(row(&result, &missing).coverage, Coverage::Missing);
    assert_eq!(row(&result, &escaped).coverage, Coverage::Unavailable);
    assert_eq!(row(&result, &shared).owner, Owner::Shared);
    assert_eq!(row(&result, &nested).owner, Owner::Shared);
    assert_eq!(
        result.other.worktree_bytes,
        bytes(&shared) + bytes(&nested) + nested_data
    );
    assert!(
        result
            .threads
            .values()
            .all(|usage| usage.total_bytes() == 0)
    );

    let mut checks = 0;
    assert_eq!(
        inventory(&input, || {
            checks += 1;
            checks > 10
        }),
        Err(Cancelled)
    );
    input.temp_dir = &managed;
    let result = inventory(&input, || false).unwrap();
    assert_eq!(
        (result.worktrees, result.claude),
        (Coverage::Unavailable, Coverage::Unavailable)
    );
    assert_eq!(result.total.total_bytes(), 0);
}

#[test]
fn protects_project_roots_inside_managed_tree_and_buckets_shared_scratch() {
    let tree = Tree::new();
    let managed = tree.mkdir("worktrees");
    let project = tree.mkdir("worktrees/repo/checkout");
    write(&project.join(".git"));
    write(&project.join("source"));
    let temp = tree.mkdir("temp");
    let session = tree.scratch(&project);
    write(&session.join("data"));

    let projects = [Project {
        id: "repo",
        root: &project,
    }];
    let threads = ["first", "second"].map(|id| Thread {
        id,
        project_id: "repo",
        cwd: &project,
        settled: false,
    });
    let result = inventory(
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
    assert_eq!(result.worktrees, Coverage::Partial);
    assert_eq!(row(&result, &project).coverage, Coverage::Unavailable);
    assert_eq!(result.total.worktree_bytes, 0);
    assert_eq!(row(&result, &session).owner, Owner::Shared);
    assert_eq!(result.projects["repo"], Usage::default());
    assert_eq!(
        result.other.scratch_bytes,
        row(&result, &session).usage.scratch_bytes
    );
}

#[test]
fn measures_an_exact_external_worktree_and_attributes_its_scratch() {
    let tree = Tree::new();
    let managed = tree.0.join("worktrees");
    let project = tree.mkdir("checkout");
    let external = tree.mkdir("custom/branch");
    let target = tree.mkdir("custom/branch/target");
    let modules = tree.mkdir("custom/branch/lib/node_modules");
    let unrelated = tree.mkdir("custom/neighbor");
    let temp = tree.mkdir("temp");
    let session = tree.scratch(&external);
    let marker = write(&external.join(".git"));
    let build = write(&target.join("artifact"));
    let package = write(&modules.join("package"));
    let source = write(&external.join("source"));
    let scratch = write(&session.join("notes"));
    write(&unrelated.join(".git"));
    write(&unrelated.join("ignored"));
    let link = external.join("points-outside");
    symlink(&unrelated, &link).unwrap();

    let result = inventory(
        &InventoryInput {
            worktree_root: &managed,
            projects: &[Project {
                id: "repo",
                root: &project,
            }],
            threads: &[Thread {
                id: "external",
                project_id: "repo",
                cwd: &external,
                settled: true,
            }],
            temp_dir: &temp,
            uid: 42,
        },
        || false,
    )
    .unwrap();
    let owner = Owner::Thread {
        id: "external".into(),
        settled: true,
    };
    assert_eq!(result.worktrees, Coverage::Complete);
    assert_eq!(result.folders.len(), 2);
    assert_eq!(row(&result, &external).coverage, Coverage::Complete);
    assert_eq!(row(&result, &external).owner, owner);
    assert_eq!(row(&result, &session).owner, owner);
    let dependency_bytes = build + package + bytes(&target) + bytes(&modules);
    assert_eq!(
        result.threads["external"],
        Usage {
            worktree_bytes: dependency_bytes
                + bytes(&external)
                + bytes(&external.join("lib"))
                + bytes(&link)
                + marker
                + source,
            dependency_bytes,
            scratch_bytes: scratch + bytes(&session),
            other_bytes: 0,
        }
    );
    assert_eq!(
        result.total.worktree_bytes,
        result.threads["external"].worktree_bytes
    );
}

#[test]
fn reports_missing_and_refuses_unsafe_external_references() {
    let tree = Tree::new();
    let managed = tree.mkdir("worktrees");
    let project = tree.mkdir("project-parent/checkout");
    let project_parent = project.parent().unwrap();
    let temp = tree.mkdir("temp");
    let scratch_root = tree.mkdir("temp/claude-42");
    let missing = tree.0.join("missing");
    let marked = tree.mkdir("custom/marked");
    let unmarked = tree.mkdir("custom/unmarked");
    let marker_link = tree.mkdir("custom/marker-link");
    let ordinary_repo = tree.mkdir("custom/repo");
    fs::create_dir(ordinary_repo.join(".git")).unwrap();
    let root_link = tree.0.join("custom/root-link");
    let parent_link = tree.0.join("alias");
    let through_link = parent_link.join("marked");
    symlink(&marked, &root_link).unwrap();
    symlink(marked.parent().unwrap(), &parent_link).unwrap();
    symlink(marked.join(".git"), marker_link.join(".git")).unwrap();
    for dir in [&project, project_parent, &temp, &scratch_root, &marked] {
        write(&dir.join(".git"));
        write(&dir.join("data"));
    }
    let unsafe_paths = [
        ("fallback", project.as_path()),
        ("project-parent", project_parent),
        ("managed", managed.as_path()),
        ("common-ancestor", tree.0.as_path()),
        ("temp", temp.as_path()),
        ("scratch", scratch_root.as_path()),
        ("root-link", root_link.as_path()),
        ("parent-link", through_link.as_path()),
        ("marker-link", marker_link.as_path()),
        ("ordinary-repo", ordinary_repo.as_path()),
        ("unmarked", unmarked.as_path()),
    ];
    let threads: Vec<_> = unsafe_paths
        .into_iter()
        .chain([("missing", missing.as_path())])
        .map(|(id, cwd)| Thread {
            id,
            project_id: "repo",
            cwd,
            settled: false,
        })
        .collect();
    let result = inventory(
        &InventoryInput {
            worktree_root: &managed,
            projects: &[Project {
                id: "repo",
                root: &project,
            }],
            threads: &threads,
            temp_dir: &temp,
            uid: 42,
        },
        || false,
    )
    .unwrap();
    assert_eq!(result.worktrees, Coverage::Partial);
    assert_eq!(result.total.worktree_bytes, 0);
    assert_eq!(row(&result, &missing).coverage, Coverage::Missing);
    for (_, path) in unsafe_paths {
        assert_eq!(
            row(&result, path).coverage,
            Coverage::Unavailable,
            "{path:?}"
        );
    }
}

#[test]
fn counts_nested_and_duplicate_external_paths_and_cross_root_links_once() {
    let tree = Tree::new();
    let managed = tree.mkdir("worktrees");
    let inside = tree.mkdir("worktrees/owned");
    let external = tree.mkdir("outside");
    let nested = tree.mkdir("outside/inner");
    let shared = tree.mkdir("custom/shared");
    let same_shared = shared.join(".");
    let unmarked = tree.mkdir("outside/unmarked");
    let project = tree.mkdir("checkout");
    let temp = tree.mkdir("temp");
    let session = tree.scratch(&nested);
    let marker = write(&external.join(".git"));
    let nested_marker = write(&nested.join(".git"));
    let shared_marker = write(&shared.join(".git"));
    let data = write(&nested.join("data"));
    let unmarked_data = write(&unmarked.join("data"));
    let linked = write(&external.join("linked"));
    fs::hard_link(external.join("linked"), inside.join("linked")).unwrap();
    fs::hard_link(external.join("linked"), session.join("linked")).unwrap();
    let paths = [
        ("managed", &inside),
        ("external", &external),
        ("nested", &nested),
        ("shared", &shared),
        ("duplicate", &same_shared),
        ("unmarked", &unmarked),
    ];
    let threads = paths.map(|(id, cwd)| Thread {
        id,
        project_id: "repo",
        cwd,
        settled: false,
    });
    let result = inventory(
        &InventoryInput {
            worktree_root: &managed,
            projects: &[Project {
                id: "repo",
                root: &project,
            }],
            threads: &threads,
            temp_dir: &temp,
            uid: 42,
        },
        || false,
    )
    .unwrap();
    assert_eq!(result.worktrees, Coverage::Partial);
    assert_eq!(row(&result, &external).coverage, Coverage::Complete);
    assert_eq!(row(&result, &nested).owner, Owner::Shared);
    assert_eq!(row(&result, &shared).owner, Owner::Shared);
    assert_eq!(row(&result, &session).owner, Owner::Shared);
    assert_eq!(row(&result, &unmarked).coverage, Coverage::Unavailable);
    assert_eq!(
        result.threads["external"].worktree_bytes,
        bytes(&external) + marker + bytes(&unmarked) + unmarked_data
    );
    assert_eq!(result.threads["managed"].worktree_bytes, bytes(&inside));
    assert_eq!(
        result.other.worktree_bytes,
        bytes(&nested) + nested_marker + data + bytes(&shared) + shared_marker + linked
    );
    assert_eq!(result.other.scratch_bytes, bytes(&session));
    assert_eq!(
        result.total.worktree_bytes,
        result.projects["repo"].worktree_bytes + result.other.worktree_bytes
    );
}
