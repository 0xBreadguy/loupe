//! Pure conversion from a completed storage inventory and already-held metadata.
use super::data::{Bytes, Entry, Inventory, Project, Scan, Sizes, ThreadState, VolumePath};
use crate::storage::{self, Coverage, Folder, FolderKind, Owner, Usage};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

pub struct ProjectMetadata<'a> {
    pub id: &'a str,
    pub title: &'a str,
}

pub struct ThreadMetadata<'a> {
    pub id: &'a str,
    pub title: &'a str,
    pub state: ThreadState,
    pub age: Option<&'a str>,
}

/// All metadata and volume samples are supplied by the caller; this performs no I/O.
pub fn convert(
    source: &storage::Inventory,
    projects: &[ProjectMetadata<'_>],
    threads: &[ThreadMetadata<'_>],
    worktree_volumes: Vec<VolumePath>,
    temporary_volume: Option<VolumePath>,
    scan: Scan,
) -> Inventory {
    let metadata: HashMap<_, _> = threads.iter().map(|t| (t.id, t)).collect();
    let mut result = Inventory {
        worktree_volumes,
        temporary_volume,
        scan,
        ..Inventory::default()
    };
    result.projects = projects
        .iter()
        .map(|p| Project {
            id: p.id.into(),
            title: p.title.into(),
            threads: Vec::new(),
        })
        .collect();

    let worktrees = worktree_namespaces(source);
    let managed_root = source
        .worktree_root
        .as_deref()
        .and_then(storage::encoded_cwd);
    let mut rows: HashMap<String, Row> = HashMap::new();
    // source.other already holds every folder without a project. Promote some of
    // them to named rows and subtract those from the residual; project folders that
    // stay in Other are added back so no measured byte is lost or counted twice.
    let mut residual = source.other;
    let mut other_folders = Vec::new();

    for folder in &source.folders {
        let plan = plan(folder, &worktrees, managed_root.as_deref());
        if matches!(plan, Plan::Other) {
            if folder.project_id.is_some() {
                other_folders.push(folder);
            }
            continue;
        }
        if folder.project_id.is_none() {
            subtract(&mut residual, folder.usage);
        }

        let (key, project, title, state, age, open_target, worktree_path) = match plan {
            Plan::Thread { id, settled } => {
                let meta = metadata.get(id).copied();
                (
                    format!("thread:{id}"),
                    folder.project_id.clone(),
                    meta.map(|m| m.title.to_owned())
                        .unwrap_or_else(|| file_name(&folder.path)),
                    if settled {
                        ThreadState::Settled
                    } else {
                        meta.map(|m| m.state).unwrap_or(ThreadState::Gone)
                    },
                    meta.and_then(|m| m.age.map(str::to_owned)),
                    meta.map(|m| m.id.to_owned()),
                    (folder.kind == FolderKind::Worktree).then(|| folder.path.clone()),
                )
            }
            Plan::Worktree => {
                let name = file_name(&folder.path);
                (
                    format!("folder:Worktree:{:?}", folder.path),
                    folder.project_id.clone(),
                    if folder.project_id.is_none() {
                        format!("unowned {name}")
                    } else {
                        name
                    },
                    ThreadState::Gone,
                    None,
                    None,
                    Some(folder.path.clone()),
                )
            }
            Plan::ScratchTied(worktree) => {
                let name = file_name(&worktree.path);
                (
                    format!("folder:Worktree:{:?}", worktree.path),
                    worktree.project_id.clone(),
                    if worktree.project_id.is_none() {
                        format!("unowned {name}")
                    } else {
                        name
                    },
                    ThreadState::Gone,
                    None,
                    None,
                    Some(worktree.path.clone()),
                )
            }
            Plan::ScratchNamespace(namespace, name) => (
                format!("scratch:{namespace:?}"),
                None,
                format!("unowned {name}"),
                ThreadState::Gone,
                None,
                None,
                None,
            ),
            Plan::Other => unreachable!("handled above"),
        };
        let row = rows.entry(key.clone()).or_insert_with(|| Row {
            project: project.clone(),
            entry: Entry {
                id: key.clone(),
                title,
                worktree_path: None,
                sizes: Sizes {
                    worktree: initial(source.worktrees),
                    build: initial(source.worktrees),
                    scratch: initial(source.claude),
                },
                state,
                age,
                open_target,
            },
        });
        if row.project.is_none() {
            row.project = project;
        }
        if worktree_path.is_some() {
            row.entry.worktree_path = worktree_path;
        }
        add_sizes(&mut row.entry.sizes, folder);
    }

    let mut rows: Vec<_> = rows.into_values().collect();
    rows.sort_by(|a, b| a.entry.id.cmp(&b.entry.id));
    for row in rows {
        if matches!(row.entry.state, ThreadState::Settled | ThreadState::Gone) {
            result.settled_or_gone.push(row.entry);
        } else if let Some(project) = result
            .projects
            .iter_mut()
            .find(|p| Some(p.id.as_str()) == row.project.as_deref())
        {
            project.threads.push(row.entry);
        } else {
            result.settled_or_gone.push(row.entry);
        }
    }

    let had_other_folders = !other_folders.is_empty();
    let mut sizes = Sizes {
        worktree: initial(source.worktrees),
        build: initial(source.worktrees),
        scratch: initial(source.claude),
    };
    add(
        &mut sizes.worktree,
        residual.worktree_bytes,
        source.worktrees,
    );
    add(
        &mut sizes.build,
        residual.dependency_bytes,
        source.worktrees,
    );
    add(
        &mut sizes.scratch,
        residual.scratch_bytes.saturating_add(residual.other_bytes),
        source.claude,
    );
    for folder in other_folders {
        add_sizes(&mut sizes, folder);
    }
    if residual.total_bytes() > 0
        || had_other_folders
        || source.worktrees != Coverage::Complete
        || source.claude != Coverage::Complete
    {
        result.other = Some(sizes);
    }
    result
}

struct Row {
    project: Option<String>,
    entry: Entry,
}

enum Plan<'a> {
    Thread {
        id: &'a str,
        settled: bool,
    },
    Worktree,
    /// Scratch under a managed worktree that is itself unowned.
    ScratchTied(&'a Folder),
    /// Scratch under the managed root whose worktree no longer exists.
    ScratchNamespace(&'a Path, &'a str),
    Other,
}

fn plan<'a>(
    folder: &'a Folder,
    worktrees: &HashMap<String, Vec<&'a Folder>>,
    managed_root: Option<&str>,
) -> Plan<'a> {
    match &folder.owner {
        Owner::Thread { id, settled } => Plan::Thread {
            id,
            settled: *settled,
        },
        // Shared means more than one thread, or a nested candidate. Its owner is
        // uncertain, which is what Other is for.
        Owner::Shared => Plan::Other,
        Owner::Missing if folder.kind == FolderKind::Worktree => Plan::Worktree,
        Owner::Missing => {
            let Some((namespace, name)) = scratch_namespace(folder) else {
                return Plan::Other;
            };
            // An exact namespace match ties scratch to a discovered orphan worktree.
            // A lossy encoding that matches more than one worktree is ambiguous.
            if let Some([worktree]) = worktrees.get(name).map(Vec::as_slice)
                && matches!(worktree.owner, Owner::Missing)
            {
                return Plan::ScratchTied(*worktree);
            }
            // Without a project or a worktree, only the scanned managed root can
            // place an absolute namespace. The encoded remainder must end in a
            // generated worktree ID; other look-alike paths stay in Other.
            let under_managed_root = managed_root.is_some_and(|root| {
                name.strip_prefix(root)
                    .and_then(|rest| rest.strip_prefix('-'))
                    .is_some_and(|tail| {
                        !tail.is_empty() && storage::generated_name(OsStr::new(tail))
                    })
            });
            if folder.project_id.is_none() && under_managed_root {
                Plan::ScratchNamespace(namespace, name)
            } else {
                Plan::Other
            }
        }
    }
}

fn worktree_namespaces(source: &storage::Inventory) -> HashMap<String, Vec<&Folder>> {
    let mut namespaces: HashMap<String, Vec<&Folder>> = HashMap::new();
    for folder in &source.folders {
        if folder.kind == FolderKind::Worktree
            && let Some(name) = storage::encoded_cwd(&folder.path)
        {
            namespaces.entry(name).or_default().push(folder);
        }
    }
    namespaces
}

fn scratch_namespace(folder: &Folder) -> Option<(&Path, &str)> {
    let namespace = folder.path.parent()?;
    Some((namespace, namespace.file_name()?.to_str()?))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

fn add_sizes(sizes: &mut Sizes, folder: &Folder) {
    match folder.kind {
        FolderKind::Worktree => {
            add(
                &mut sizes.worktree,
                folder.usage.worktree_bytes,
                folder.coverage,
            );
            add(
                &mut sizes.build,
                folder.usage.dependency_bytes,
                folder.coverage,
            );
        }
        FolderKind::Scratch => add(
            &mut sizes.scratch,
            folder
                .usage
                .scratch_bytes
                .saturating_add(folder.usage.other_bytes),
            folder.coverage,
        ),
    }
}

fn subtract(dst: &mut Usage, src: Usage) {
    dst.worktree_bytes = dst.worktree_bytes.saturating_sub(src.worktree_bytes);
    dst.dependency_bytes = dst.dependency_bytes.saturating_sub(src.dependency_bytes);
    dst.scratch_bytes = dst.scratch_bytes.saturating_sub(src.scratch_bytes);
    dst.other_bytes = dst.other_bytes.saturating_sub(src.other_bytes);
}

fn initial(coverage: Coverage) -> Bytes {
    match coverage {
        Coverage::Complete => Bytes::Absent,
        _ => Bytes::Unknown,
    }
}
fn add(dst: &mut Bytes, n: u64, coverage: Coverage) {
    let old = match *dst {
        Bytes::Exact(v) | Bytes::AtLeast(v) => v,
        _ => 0,
    };
    let sum = old.saturating_add(n);
    *dst = if coverage == Coverage::Unavailable || coverage == Coverage::Missing {
        if sum > 0 {
            Bytes::AtLeast(sum)
        } else {
            Bytes::Unknown
        }
    } else if coverage == Coverage::Partial
        || matches!(*dst, Bytes::AtLeast(_) | Bytes::Unknown)
        || old.checked_add(n).is_none()
    {
        Bytes::AtLeast(sum)
    } else {
        Bytes::Exact(sum)
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{Folder, Usage};
    use std::path::PathBuf;

    #[test]
    fn keeps_identity_coverage_and_sends_unowned_scratch_to_other() {
        let folder = |path: &str, project: &str, owner: Owner, kind, coverage, usage| Folder {
            path: PathBuf::from(path),
            project_id: Some(project.into()),
            owner,
            kind,
            coverage,
            usage,
        };
        let source = storage::Inventory {
            worktrees: Coverage::Complete,
            claude: Coverage::Partial,
            folders: vec![
                folder(
                    "/a",
                    "a",
                    Owner::Thread {
                        id: "t".into(),
                        settled: false,
                    },
                    FolderKind::Worktree,
                    Coverage::Complete,
                    Usage {
                        worktree_bytes: 10,
                        dependency_bytes: 4,
                        ..Usage::default()
                    },
                ),
                folder(
                    "/b",
                    "b",
                    Owner::Missing,
                    FolderKind::Scratch,
                    Coverage::Partial,
                    Usage {
                        scratch_bytes: 3,
                        ..Usage::default()
                    },
                ),
            ],
            other: Usage {
                other_bytes: 7,
                ..Usage::default()
            },
            ..storage::Inventory::default()
        };
        let result = convert(
            &source,
            &[
                ProjectMetadata {
                    id: "a",
                    title: "same",
                },
                ProjectMetadata {
                    id: "b",
                    title: "same",
                },
            ],
            &[ThreadMetadata {
                id: "t",
                title: "thread",
                state: ThreadState::Quiet,
                age: None,
            }],
            vec![],
            None,
            Scan::Unavailable,
        );
        assert_eq!(
            result.projects[0].threads[0].sizes.worktree,
            Bytes::Exact(10)
        );
        assert_eq!(result.projects[0].threads[0].sizes.build, Bytes::Exact(4));
        assert_eq!(
            result.projects[0].threads[0].open_target.as_deref(),
            Some("t")
        );
        // Scratch without a thread is not evidence of a gone thread.
        assert!(result.settled_or_gone.is_empty());
        assert!(result.projects[1].threads.is_empty());
        assert_eq!(result.other.unwrap().scratch, Bytes::AtLeast(10));
        assert_ne!(result.projects[0].id, result.projects[1].id);
    }
}
