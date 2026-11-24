//! Storage view inputs and an on-demand scanner. The adapter only copies projection metadata.
//! The worker owns its input and runs independently of the dashboard's refresh and render paths.
//!
//! Capture the source at launch. While Storage is open, submit the full current snapshot on
//! entry and when it changes. Poll the worker before composing a frame, and render the state
//! with the worker's matching metadata. Cancel the in-flight walk on exit from Storage.
//! The last completed measurement stays in the session until it is stale, rejected, or replaced.

use std::collections::BTreeMap;
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use loupe_core::{
    model::Snapshot,
    product::{Database, Origin},
    storage::{
        self, Cancelled, Coverage, Folder, FolderKind, Inventory, InventoryInput, Owner, Usage,
    },
};

mod reentry;
mod worker;
pub use reentry::{walk_key, Commit, Decision, Session, WalkKey};
pub use worker::{ScanState, StorageWorker};

#[cfg(test)]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanRoots {
    pub worktrees: PathBuf,
    /// Base containing the per-user `claude-<uid>` directory.
    pub temp: PathBuf,
    pub uid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageSource {
    /// Fixed sample data, entirely in memory.
    Demo,
    /// Coverage is unavailable. Used by explicit databases and snapshot/replay fixtures.
    Unavailable,
    /// Roots for discovery. The inventory also checks explicit local worktree paths.
    Disk(ScanRoots),
}

impl StorageSource {
    /// Capture once from the product launch, before it loses the database origin.
    pub fn for_database(database: &Database) -> Self {
        match database {
            Database::Demo => Self::Demo,
            Database::Path {
                origin: Origin::Db, ..
            } => Self::Unavailable,
            Database::Path { path, .. } => live_source(path),
        }
    }

    /// Use the entire snapshot, so shelf, selection, and tree folding do not change ownership.
    pub fn input(&self, snapshot: &Snapshot) -> OwnedInput {
        let mut projects = BTreeMap::new();
        let mut threads = BTreeMap::new();
        for agent in &snapshot.agents {
            let Some(storage) = &agent.storage else {
                continue;
            };
            if agent.remote.is_some() || agent.thread_id.is_empty() {
                continue;
            }
            if !storage.project_id.is_empty() {
                projects
                    .entry(storage.project_id.clone())
                    .or_insert_with(|| ProjectMetadata {
                        title: agent.project.clone(),
                        root: storage.project_root.as_deref().map(PathBuf::from),
                    });
            }
            threads
                .entry(agent.thread_id.clone())
                .or_insert_with(|| ThreadMetadata {
                    title: agent.title.clone(),
                    project_id: storage.project_id.clone(),
                    worktree: storage.worktree.as_deref().map(PathBuf::from),
                    settled: agent.shelf.as_ref().is_some_and(|shelf| shelf.settled),
                });
        }
        OwnedInput {
            source: self.clone(),
            projects,
            threads,
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn live_source(_database: &Path) -> StorageSource {
    StorageSource::Unavailable
}

#[cfg(target_os = "linux")]
fn live_source(database: &Path) -> StorageSource {
    use std::env::{current_dir, var_os};
    let cwd = current_dir().ok();
    let temp = unix_temp_dir(
        ["TMPDIR", "TMP", "TEMP"]
            .map(var_os)
            .each_ref()
            .map(|v| v.as_deref()),
    );
    linux_source(
        database,
        var_os("T3CODE_HOME").as_deref(),
        loupe_core::platform::home_dir().as_deref(),
        cwd.as_deref(),
        temp,
        crate::platform::user_id(),
    )
}

#[cfg(target_os = "linux")]
fn unix_temp_dir(variables: [Option<&OsStr>; 3]) -> PathBuf {
    // Node's lib/os.js and src/node_credentials.cc try TMPDIR, TMP, TEMP, then /tmp.
    variables
        .into_iter()
        .flatten()
        .find(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from)
}

#[cfg(target_os = "linux")]
fn linux_source(
    database: &Path,
    t3code_home: Option<&OsStr>,
    home: Option<&OsStr>,
    cwd: Option<&Path>,
    temp: PathBuf,
    uid: u32,
) -> StorageSource {
    use loupe_core::product::{data_folder, HomeRules};

    // T3's deriveServerPaths (apps/server/src/config.ts) places worktrees beside userdata.
    let base = data_folder(t3code_home, home, cwd, HomeRules::T3);
    let expected = base.join("userdata").join("statev2.sqlite");
    // The dashboard keeps the original Linux DB lookup rules, which can disagree with T3's.
    // Only enable the live scan when the launch uses the matching database path.
    let launched = match cwd {
        Some(cwd) if database.is_relative() => cwd.join(database),
        _ => database.to_owned(),
    };
    if !base.is_absolute() || launched != expected {
        return StorageSource::Unavailable;
    }
    StorageSource::Disk(ScanRoots {
        worktrees: base.join("worktrees"),
        temp,
        uid,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectMetadata {
    pub title: String,
    pub root: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadMetadata {
    pub title: String,
    pub project_id: String,
    pub worktree: Option<PathBuf>,
    pub settled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnedInput {
    pub source: StorageSource,
    /// Keyed by T3 project ID.
    pub projects: BTreeMap<String, ProjectMetadata>,
    /// Keyed by local T3 thread ID.
    pub threads: BTreeMap<String, ThreadMetadata>,
}

impl OwnedInput {
    fn scan(&self, mut cancelled: impl FnMut() -> bool) -> Result<Inventory, Cancelled> {
        if cancelled() {
            return Err(Cancelled);
        }
        let roots = match &self.source {
            StorageSource::Demo => return self.demo_inventory(cancelled),
            StorageSource::Unavailable => return Ok(Inventory::default()),
            StorageSource::Disk(roots) => roots,
        };
        let mut projects = Vec::with_capacity(self.projects.len());
        for (id, project) in &self.projects {
            if cancelled() {
                return Err(Cancelled);
            }
            projects.push(storage::Project {
                id,
                root: project.root.as_deref().unwrap_or_else(|| Path::new("")),
            });
        }
        let mut threads = Vec::with_capacity(self.threads.len());
        for (id, thread) in &self.threads {
            if cancelled() {
                return Err(Cancelled);
            }
            threads.push(storage::Thread {
                id,
                project_id: &thread.project_id,
                cwd: thread.worktree.as_deref().unwrap_or_else(|| Path::new("")),
                settled: thread.settled,
            });
        }
        storage::inventory(
            &InventoryInput {
                worktree_root: &roots.worktrees,
                projects: &projects,
                threads: &threads,
                temp_dir: &roots.temp,
                uid: roots.uid,
            },
            cancelled,
        )
    }

    fn demo_inventory(&self, mut cancelled: impl FnMut() -> bool) -> Result<Inventory, Cancelled> {
        const MIB: u64 = 1024 * 1024;
        let mut result = Inventory {
            worktrees: Coverage::Complete,
            claude: Coverage::Complete,
            ..Inventory::default()
        };
        for id in self.projects.keys() {
            result.projects.insert(id.clone(), Usage::default());
        }
        for (id, thread) in &self.threads {
            if cancelled() {
                return Err(Cancelled);
            }
            let (worktree, dependency, scratch) = match id.as_str() {
                "demo-checkout" => (840, 620, 28),
                "demo-billing" => (380, 190, 14),
                _ if thread.settled => (120, 80, 0),
                _ => (0, 0, 0),
            };
            let usage = Usage {
                worktree_bytes: worktree * MIB,
                dependency_bytes: dependency * MIB,
                scratch_bytes: scratch * MIB,
                ..Usage::default()
            };
            result.threads.insert(id.clone(), usage);
            let project = result.projects.get_mut(&thread.project_id);
            let project_id = project.as_ref().map(|_| thread.project_id.clone());
            for total in [&mut result.total, project.unwrap_or(&mut result.other)] {
                total.worktree_bytes += usage.worktree_bytes;
                total.dependency_bytes += usage.dependency_bytes;
                total.scratch_bytes += usage.scratch_bytes;
            }
            let owner = Owner::Thread {
                id: id.clone(),
                settled: thread.settled,
            };
            if worktree > 0 {
                result.folders.push(Folder {
                    path: Path::new("demo/worktrees").join(id),
                    kind: FolderKind::Worktree,
                    owner: owner.clone(),
                    project_id: project_id.clone(),
                    coverage: Coverage::Complete,
                    usage: Usage {
                        scratch_bytes: 0,
                        ..usage
                    },
                });
            }
            if scratch > 0 {
                result.folders.push(Folder {
                    path: Path::new("demo/scratch").join(id),
                    kind: FolderKind::Scratch,
                    owner,
                    project_id,
                    coverage: Coverage::Complete,
                    usage: Usage {
                        scratch_bytes: usage.scratch_bytes,
                        ..Usage::default()
                    },
                });
            }
        }
        Ok(result)
    }
}
