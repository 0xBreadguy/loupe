//! Read-only inputs for the Storage view. The presenter supplies measurements and thread
//! metadata. Rendering and navigation never query the database or the filesystem.

use crate::headroom::Volume;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// Allocated bytes. Use `Absent` only for a component known not to apply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Bytes {
    Absent,
    Exact(u64),
    AtLeast(u64),
    #[default]
    Unknown,
}

impl Bytes {
    pub(crate) fn measured(self) -> Option<u64> {
        match self {
            Self::Absent => Some(0),
            Self::Exact(n) | Self::AtLeast(n) => Some(n),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sizes {
    pub worktree: Bytes,
    /// A subset of worktree bytes, already included in `worktree`.
    pub build: Bytes,
    pub scratch: Bytes,
}

impl Sizes {
    pub(crate) fn footprint(self) -> Option<u128> {
        let worktree = self.worktree.measured().map(u128::from);
        let scratch = self.scratch.measured().map(u128::from);
        (worktree.is_some() || scratch.is_some())
            .then(|| worktree.unwrap_or(0) + scratch.unwrap_or(0))
    }

    pub(crate) fn group(entries: &[&Entry], scanning: bool) -> Self {
        fn sum(entries: &[&Entry], pick: impl Fn(Sizes) -> Bytes, scanning: bool) -> Bytes {
            let mut count = None::<u64>;
            let mut partial = false;
            for entry in entries {
                let bytes = pick(entry.sizes);
                match bytes {
                    Bytes::Unknown if scanning => return Bytes::Unknown,
                    Bytes::Unknown => partial = true,
                    Bytes::Absent => {}
                    Bytes::Exact(n) | Bytes::AtLeast(n) => {
                        let before = count.unwrap_or_default();
                        count = Some(before.saturating_add(n));
                        partial |=
                            matches!(bytes, Bytes::AtLeast(_)) || before.checked_add(n).is_none();
                    }
                }
            }
            match (count, partial) {
                (Some(n), true) => Bytes::AtLeast(n),
                (Some(n), false) => Bytes::Exact(n),
                (None, true) => Bytes::Unknown,
                (None, false) => Bytes::Absent,
            }
        }

        Self {
            worktree: sum(entries, |s| s.worktree, scanning),
            build: sum(entries, |s| s.build, scanning),
            scratch: sum(entries, |s| s.scratch, scanning),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Running,
    Quiet,
    NeedsYou,
    Done,
    Settled,
    Gone,
    Queued,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Stable identity unique across entries, including folders without a thread.
    pub id: String,
    pub title: String,
    pub worktree_path: Option<PathBuf>,
    pub sizes: Sizes,
    pub state: ThreadState,
    /// For example, an already formatted age from the thread model.
    pub age: Option<String>,
    /// Target the host can open in T3. Leave unset when opening is unavailable.
    pub open_target: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Project {
    /// Stable identity unique across projects.
    pub id: String,
    pub title: String,
    pub threads: Vec<Entry>,
}

#[derive(Debug, Clone)]
pub struct VolumePath {
    /// Display path or mount label. The caller retains the actual path for probing.
    pub label: String,
    pub volume: Option<Volume>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct FolderProgress {
    pub done: usize,
    /// Supply only when the scanner has enumerated the full list of folders.
    pub total: Option<usize>,
}

/// Whether the rows on screen are the walk named by [`Scan`] or an earlier completion.
///
/// `Current` is the walk in progress or the one that just finished. `Kept` and
/// `Refreshing` are the last completed measurement for the same inputs. They must
/// not use the freshly scanned label.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Shown {
    #[default]
    Current,
    Kept {
        /// Unix milliseconds when that measurement completed.
        at_ms: u64,
    },
    Refreshing {
        at_ms: u64,
    },
}

#[derive(Debug, Clone, Copy, Default)]
pub enum Scan {
    #[default]
    Unavailable,
    Scanning(FolderProgress),
    Finished {
        folders: usize,
        /// Folders counted above whose measurements are incomplete.
        incomplete: usize,
        elapsed: Option<Duration>,
        finished_at: Option<SystemTime>,
    },
}

impl Scan {
    pub fn scanning(self) -> bool {
        matches!(self, Self::Scanning(_))
    }
}

#[derive(Debug, Clone, Default)]
pub struct Inventory {
    pub worktree_volumes: Vec<VolumePath>,
    pub temporary_volume: Option<VolumePath>,
    pub scan: Scan,
    /// Defaults to the walk named by `scan`. A retained measurement sets this explicitly.
    pub shown: Shown,
    /// One or more roots could not be fully inspected, even if every listed folder was read.
    pub incomplete_coverage: bool,
    /// In walk order while scanning. The view sorts measured results after the scan.
    pub projects: Vec<Project>,
    pub settled_or_gone: Vec<Entry>,
    /// Unattributed allocations. Supply a residual only if the scanner can establish it
    /// without double counting; a total supplied by a volume probe does not establish ownership.
    pub other: Option<Sizes>,
    /// Used bytes only when the caller verified temp is a separate tmpfs root.
    #[doc(hidden)]
    pub temp_volume_used: Option<u64>,
    /// Owning project ID for entries placed in `settled_or_gone`, keyed by `Entry::id`.
    #[doc(hidden)]
    pub settled_projects: std::collections::BTreeMap<String, String>,
}

impl Inventory {
    /// Supply the latest verified dedicated temp volume usage, or None when unavailable/shared.
    pub fn set_temp_volume_used(&mut self, used: Option<u64>) {
        self.temp_volume_used = used;
    }

    pub(crate) fn displayed_other(&self) -> (Option<Sizes>, bool) {
        let measured = self.other;
        let derive = || {
            let used = self.temp_volume_used?;
            if self.incomplete_coverage
                || !matches!(self.scan, Scan::Finished { incomplete: 0, .. })
            {
                return None;
            }
            let mut named = 0u64;
            for entry in self
                .projects
                .iter()
                .flat_map(|p| &p.threads)
                .chain(&self.settled_or_gone)
            {
                let n = match entry.sizes.scratch {
                    Bytes::Absent => 0,
                    Bytes::Exact(n) => n,
                    _ => return None,
                };
                named = named.checked_add(n)?;
            }
            let remainder = used.checked_sub(named)?;
            if remainder == 0 && measured.is_none() {
                return None;
            }
            let mut sizes = measured.unwrap_or(Sizes {
                worktree: Bytes::Absent,
                build: Bytes::Absent,
                scratch: Bytes::Absent,
            });
            let minimum = match sizes.scratch {
                Bytes::Absent => 0,
                Bytes::Exact(n) => n,
                _ => return None,
            };
            if remainder < minimum {
                return None;
            }
            sizes.scratch = Bytes::Exact(remainder);
            Some(sizes)
        };
        match derive() {
            Some(sizes) => (Some(sizes), true),
            None => (measured, false),
        }
    }
}

#[cfg(test)]
mod remainder_tests {
    use super::*;

    #[test]
    fn dedicated_volume_remainder_is_reversible_and_bounded() {
        let mut inventory = Inventory {
            scan: Scan::Finished {
                folders: 1,
                incomplete: 0,
                elapsed: None,
                finished_at: None,
            },
            projects: vec![Project {
                id: "p".into(),
                title: "p".into(),
                threads: vec![Entry {
                    id: "t".into(),
                    title: "t".into(),
                    worktree_path: None,
                    sizes: Sizes {
                        scratch: Bytes::Exact(3_900),
                        ..Sizes::default()
                    },
                    state: ThreadState::Running,
                    age: None,
                    open_target: None,
                }],
            }],
            other: Some(Sizes {
                worktree: Bytes::Exact(42),
                build: Bytes::Exact(7),
                scratch: Bytes::Exact(300),
            }),
            ..Inventory::default()
        };
        let measured = inventory.other;
        inventory.set_temp_volume_used(Some(9_600));
        assert_eq!(
            inventory.displayed_other().0.unwrap().scratch,
            Bytes::Exact(5_700)
        );
        assert_eq!(
            inventory.displayed_other().0.unwrap().worktree,
            Bytes::Exact(42)
        );
        assert_eq!(
            inventory.displayed_other().0.unwrap().build,
            Bytes::Exact(7)
        );
        assert_eq!(
            inventory.displayed_other().0.unwrap().scratch,
            Bytes::Exact(5_700)
        );
        for sample in [None, Some(0), Some(3_899), Some(4_000)] {
            inventory.set_temp_volume_used(sample);
            assert_eq!(inventory.displayed_other(), (measured, false));
        }
        inventory.set_temp_volume_used(Some(9_600));
        inventory.incomplete_coverage = true;
        assert_eq!(inventory.displayed_other(), (measured, false));
        inventory.incomplete_coverage = false;
        inventory.scan = Scan::Scanning(FolderProgress::default());
        assert_eq!(inventory.displayed_other(), (measured, false));
        inventory.scan = Scan::Finished {
            folders: 1,
            incomplete: 1,
            elapsed: None,
            finished_at: None,
        };
        assert_eq!(inventory.displayed_other(), (measured, false));
        inventory.scan = Scan::Finished {
            folders: 1,
            incomplete: 0,
            elapsed: None,
            finished_at: None,
        };
        inventory.other.as_mut().unwrap().scratch = Bytes::Exact(0);
        inventory.set_temp_volume_used(Some(3_900));
        assert_eq!(
            inventory.displayed_other().0.unwrap().scratch,
            Bytes::Exact(0)
        );
        inventory.other = None;
        assert_eq!(inventory.displayed_other(), (None, false));
        inventory.set_temp_volume_used(Some(9_600));
        assert_eq!(inventory.displayed_other().0.unwrap().scratch, Bytes::Exact(5_700));
    }
}
