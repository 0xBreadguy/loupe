//! Explicit, synchronous inventory of worktrees and Claude's per-user temp area.
//! A caller can run this on its own worker and publish only completed refreshes.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub struct InventoryInput<'a> {
    /// Effective T3 managed worktree directory. Paths used for traversal must be
    /// absolute and have no symlink or parent components.
    pub worktree_root: &'a Path,
    pub projects: &'a [Project<'a>],
    /// One record per local T3 thread, including settled threads.
    pub threads: &'a [Thread<'a>],
    /// Claude's effective temp base, before the claude-<uid> component.
    pub temp_dir: &'a Path,
    pub uid: u32,
}

pub struct Project<'a> {
    pub id: &'a str,
    pub root: &'a Path,
}

pub struct Thread<'a> {
    pub id: &'a str,
    pub project_id: &'a str,
    /// The recorded worktree path. Pass an empty path when the thread has no
    /// explicit worktree.
    pub cwd: &'a Path,
    pub settled: bool,
}

/// Claude Code's on-disk directory name for a working directory: every character
/// outside ASCII letters and digits becomes `-`. The mapping is lossy, so equality
/// identifies a namespace but the name cannot be decoded back into a path.
pub(crate) fn encoded_cwd(path: &Path) -> Option<String> {
    Some(
        path.to_str()?
            .encode_utf16()
            .map(|unit| match unit {
                48..=57 | 65..=90 | 97..=122 => char::from_u32(u32::from(unit)).expect("ASCII"),
                _ => '-',
            })
            .collect(),
    )
}

/// A worktree name T3 generates: `<branch prefix>-<16 hex>[-N]`
/// (GitVcsDriverCore.ts uses `<root>/<repo basename>/<branch prefix>-<16 hex>[-N]`).
pub(crate) fn generated_name(name: &OsStr) -> bool {
    fn hash(part: &str) -> bool {
        part.len() == 16 && part.bytes().all(|byte| byte.is_ascii_hexdigit())
    }
    let Some((prefix, tail)) = name.to_str().and_then(|name| name.rsplit_once('-')) else {
        return false;
    };
    (!prefix.is_empty() && hash(tail))
        || (tail.parse::<u64>().is_ok_and(|n| n >= 2)
            && prefix
                .rsplit_once('-')
                .is_some_and(|(prefix, part)| !prefix.is_empty() && hash(part)))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Coverage {
    #[default]
    Unavailable,
    Complete,
    Partial,
    /// A referenced folder was absent.
    Missing,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub worktree_bytes: u64,
    /// A subset of worktree_bytes, covering directories named target or node_modules.
    pub dependency_bytes: u64,
    pub scratch_bytes: u64,
    /// Unclassified entries in Claude's per-UID temporary area.
    pub other_bytes: u64,
}

impl Usage {
    pub fn total_bytes(self) -> u64 {
        self.worktree_bytes
            .saturating_add(self.scratch_bytes)
            .saturating_add(self.other_bytes)
    }

    #[cfg(target_os = "linux")]
    fn add(&mut self, usage: Self) {
        self.worktree_bytes = self.worktree_bytes.saturating_add(usage.worktree_bytes);
        self.dependency_bytes = self.dependency_bytes.saturating_add(usage.dependency_bytes);
        self.scratch_bytes = self.scratch_bytes.saturating_add(usage.scratch_bytes);
        self.other_bytes = self.other_bytes.saturating_add(usage.other_bytes);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    Thread {
        id: String,
        settled: bool,
    },
    Shared,
    /// No unique thread was found. The folder can still have a project.
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FolderKind {
    Worktree,
    Scratch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    pub path: PathBuf,
    pub kind: FolderKind,
    pub owner: Owner,
    pub project_id: Option<String>,
    pub coverage: Coverage,
    /// Bytes exclusive to this folder. Cross-folder hard links are included in Other.
    pub usage: Usage,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub worktrees: Coverage,
    pub claude: Coverage,
    /// The absolute managed worktree directory this scan traversed, when it was usable.
    /// Claude's scratch directory names encode a working path lossily, so a view that
    /// wants to place an unowned namespace needs the root that was actually scanned.
    pub worktree_root: Option<PathBuf>,
    pub folders: Vec<Folder>,
    /// Keyed by the supplied project IDs. Includes any project-attributable orphans.
    pub projects: BTreeMap<String, Usage>,
    /// Thread totals are already included in their project totals.
    pub threads: BTreeMap<String, Usage>,
    /// Shared and unassigned bytes. Includes shared hard links across rows.
    pub other: Usage,
    /// Sum of project totals and Other.
    pub total: Usage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cancelled;

/// Uses allocated blocks, including directory metadata. Does no file-content I/O.
/// Missing/partial data must be interpreted alongside the coverage fields.
/// On platforms without the Linux metadata traversal, returns unavailable coverage.
pub fn inventory(
    input: &InventoryInput<'_>,
    cancelled: impl FnMut() -> bool,
) -> Result<Inventory, Cancelled> {
    #[cfg(target_os = "linux")]
    return linux::scan(input, cancelled);

    #[cfg(not(target_os = "linux"))]
    {
        let _ = input;
        let mut cancelled = cancelled;
        if cancelled() {
            return Err(Cancelled);
        }
        Ok(Inventory::default())
    }
}

#[cfg(target_os = "linux")]
mod linux;
