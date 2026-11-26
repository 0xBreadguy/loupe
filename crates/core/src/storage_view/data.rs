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
    /// T3's worktree cleanup rules. `None` hides the band: explicit `--db`, snapshot replay,
    /// no matching T3 home, or a platform without Storage.
    pub cleanup: Option<Cleanup>,
}

pub use crate::cleanup_rules::{KeepWhen, Rules};

/// The T3 CLEANUP band, already computed by `cleanup_rules::summarize`.
#[derive(Debug, Clone, PartialEq)]
pub struct Cleanup {
    pub state: CleanupState,
    /// The rules for projects without their own rule. `None` while unknown.
    pub global: Option<Rules>,
    /// Projects Storage knows that have their own rule.
    pub own_rules: usize,
    /// Those projects' rules, keyed by project ID.
    pub project_rules: std::collections::BTreeMap<String, ProjectRule>,
    /// Shown while the rules are off, or loose.
    pub suggestion: Option<Suggestion>,
    pub estimate: Estimate,
    /// The global rules are on and keep any local files, and a candidate that follows them
    /// has build output (T3 keeps a worktree for any ignored entry but `node_modules/`).
    pub build_kept: bool,
    /// T3 creates new worktrees outside the folder Storage scans.
    pub custom_root: bool,
    pub auto_settle_days: Option<f64>,
    /// Keyed by `Entry::id`.
    pub entries: std::collections::BTreeMap<String, EntryCleanup>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CleanupState {
    /// No rule removes worktrees for projects that follow the global rule.
    /// `defaults` when there is no settings file.
    Off { defaults: bool },
    On,
    /// On, but the suggestion would free at least 1 GB more.
    Loose,
    Unknown(CleanupProblem),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CleanupProblem {
    /// An error category, never file content.
    Unreadable(String),
    Invalid {
        key: &'static str,
        constraint: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectRule {
    /// `{mode: "off"}`. The rules keep `storageCleanup.worktreeKeepWhen`, as T3 resolves them.
    Off(Rules),
    Custom(Rules),
    /// An explicit `null` override: the `storageCleanup` fields, whatever the top level says.
    Storage(Rules),
}

impl ProjectRule {
    /// The project's resolved rules.
    pub fn rules(self) -> Rules {
        match self {
            Self::Off(rules) | Self::Custom(rules) | Self::Storage(rules) => rules,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suggestion {
    Settled(u16),
    Inactive(u16),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estimate {
    /// The first walk is still running.
    Estimating,
    /// Folder sizes are unavailable.
    NoSizes,
    /// Thread timestamps could not be read.
    NoTimes,
    Ready(Tally),
}

/// Off: what the suggestion would make eligible. On: what the rules make eligible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub now: Count,
    /// The soonest batch that is not due yet.
    pub next: Option<Next>,
    /// Loose only: what the suggestion would add to the current rules.
    pub extra: Option<Count>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Next {
    pub days: u32,
    pub count: Count,
}

/// WORKTREE bytes only: T3 does not remove scratch.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count {
    pub worktrees: usize,
    pub bytes: u64,
    /// A size was a lower bound or unknown.
    pub partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryCleanup {
    pub ownership: Ownership,
    /// Only while the rules for the entry's project are on.
    pub due: Option<Due>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    Active,
    Settled,
    /// The thread is archived. T3's rules still apply.
    Archived,
    /// The thread was deleted. Only "on delete" removes it.
    Deleted,
    /// No thread row records this folder. T3 never removes it.
    NoThread,
}

impl Ownership {
    pub fn word(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Settled => "settled",
            Self::Archived => "archived",
            Self::Deleted => "deleted",
            Self::NoThread => "no thread",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Eligible now, if T3 finds it clean.
    Now,
    InDays(u32),
    /// No timed rule covers it.
    NotCovered,
    /// T3 keeps it while the thread is busy.
    Busy,
    /// Outside T3's managed worktree folders.
    Outside,
    /// No thread: T3 only reports it.
    Never,
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
