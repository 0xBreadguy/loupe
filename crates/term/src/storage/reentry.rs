//! One completed measurement for this process.
//!
//! Storage used to cancel its worker on the way out and walk again on every entry.
//! Leaving the screen and coming straight back therefore repeated a cold walk even
//! when the snapshot's scan inputs had not changed. This slot keeps the last walk
//! that finished with complete coverage. It is not a history and it is not written
//! to disk.
//!
//! Fifteen seconds is long enough for a close and an immediate reopen, including a
//! glance at the dashboard, and short enough that a later visit does not present
//! those bytes as a new completion. Folder contents can change without any change
//! to thread or project metadata. After the window, the same rows stay on screen
//! under an explicit previous-measurement label while one walk runs. An explicit
//! rescan always walks. A change of source, ownership, path, or settled state
//! drops the rows immediately, because they would describe a different inventory.
//! A partial or unavailable completion is not retained and clears a matching slot,
//! so a gappy result is never served later as a finished scan. Cancellation does
//! not commit, and it does not erase the previous completion.

use std::time::{Duration, Instant};

use t3_agents_core::storage::{Coverage, Inventory};
use t3_agents_core::storage_view::data::Shown;

use super::{OwnedInput, StorageSource};

/// How long a completed measurement may be shown again without walking.
pub const FRESH_FOR: Duration = Duration::from_secs(15);

/// Scan identity. Titles and activity are display-only and stay out of the key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalkKey {
    source: SourceKey,
    projects: Vec<(String, Option<std::path::PathBuf>)>,
    threads: Vec<ThreadKey>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SourceKey {
    Demo,
    Unavailable,
    Disk {
        worktrees: std::path::PathBuf,
        temp: std::path::PathBuf,
        uid: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ThreadKey {
    id: String,
    project_id: String,
    worktree: Option<std::path::PathBuf>,
    settled: bool,
}

pub fn walk_key(input: &OwnedInput) -> WalkKey {
    WalkKey {
        source: match &input.source {
            StorageSource::Demo => SourceKey::Demo,
            StorageSource::Unavailable => SourceKey::Unavailable,
            StorageSource::Disk(roots) => SourceKey::Disk {
                worktrees: roots.worktrees.clone(),
                temp: roots.temp.clone(),
                uid: roots.uid,
            },
        },
        projects: input
            .projects
            .iter()
            .map(|(id, project)| (id.clone(), project.root.clone()))
            .collect(),
        threads: input
            .threads
            .iter()
            .map(|(id, thread)| ThreadKey {
                id: id.clone(),
                project_id: thread.project_id.clone(),
                worktree: thread.worktree.clone(),
                settled: thread.settled,
            })
            .collect(),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Paint the retained rows. Do not walk.
    Keep { inventory: Inventory, shown: Shown },
    /// Paint the retained rows as a previous measurement and walk.
    Refresh { inventory: Inventory, shown: Shown },
    /// Nothing retained matches this input.
    Scan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Commit {
    /// Complete coverage replaced the slot and may be labeled as just scanned.
    Fresh,
    /// The result is partial, unavailable, or otherwise not a completed scan.
    /// A matching slot was dropped. Do not label the result as freshly completed.
    Rejected,
}

struct Slot {
    key: WalkKey,
    inventory: Inventory,
    at: Instant,
    at_ms: u64,
}

pub struct Session {
    slot: Option<Slot>,
    fresh_for: Duration,
}

impl Session {
    pub fn new(fresh_for: Duration) -> Self {
        Self {
            slot: None,
            fresh_for,
        }
    }

    pub fn decide(&self, input: &OwnedInput, now: Instant, explicit: bool) -> Decision {
        let Some(slot) = &self.slot else {
            return Decision::Scan;
        };
        if slot.key != walk_key(input) {
            return Decision::Scan;
        }
        let age = now.saturating_duration_since(slot.at);
        let fresh = self.fresh_for > Duration::ZERO && age <= self.fresh_for;
        let shown = if explicit || !fresh {
            Shown::Refreshing { at_ms: slot.at_ms }
        } else {
            Shown::Kept { at_ms: slot.at_ms }
        };
        let inventory = slot.inventory.clone();
        if explicit || !fresh {
            Decision::Refresh { inventory, shown }
        } else {
            Decision::Keep { inventory, shown }
        }
    }

    /// Remember a walk that finished with complete coverage for every root.
    /// Anything else is not a completed measurement. If it was for the retained
    /// key, the slot is dropped so the next entry walks again.
    pub fn commit(
        &mut self,
        input: &OwnedInput,
        inventory: &Inventory,
        at: Instant,
        at_ms: u64,
    ) -> Commit {
        let key = walk_key(input);
        if inventory.worktrees == Coverage::Complete && inventory.claude == Coverage::Complete {
            self.slot = Some(Slot {
                key,
                inventory: inventory.clone(),
                at,
                at_ms,
            });
            Commit::Fresh
        } else {
            if self.slot.as_ref().is_some_and(|slot| slot.key == key) {
                self.slot = None;
            }
            Commit::Rejected
        }
    }

    /// The last completion for this input, labeled as previous. Used when a walk
    /// dies without a result. A failure is not a new completion and does not
    /// erase the slot.
    pub fn retained(&self, input: &OwnedInput) -> Option<(Inventory, Shown)> {
        let slot = self.slot.as_ref()?;
        (slot.key == walk_key(input))
            .then(|| (slot.inventory.clone(), Shown::Kept { at_ms: slot.at_ms }))
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new(FRESH_FOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;
    use t3_agents_core::model::{Agent, Shelf, Snapshot, ThreadStorage};
    use t3_agents_core::storage::{Coverage, Inventory, Usage};

    use crate::storage::{ScanRoots, StorageSource};

    fn measured(bytes: u64, worktrees: Coverage, claude: Coverage) -> Inventory {
        Inventory {
            worktrees,
            claude,
            total: Usage {
                worktree_bytes: bytes,
                ..Usage::default()
            },
            ..Inventory::default()
        }
    }

    fn input(path: &str, settled: bool, title: &str) -> crate::storage::OwnedInput {
        let root = Path::new("/synthetic");
        let mut agent = Agent {
            id: "t".into(),
            title: title.into(),
            thread_id: "t".into(),
            project: "Proj".into(),
            storage: Some(ThreadStorage {
                project_id: "p".into(),
                project_root: Some("/synthetic/project".into()),
                worktree: Some(root.join(path).to_str().unwrap().into()),
            }),
            ..Agent::default()
        };
        if settled {
            agent.shelf = Some(Shelf {
                settled: true,
                ..Shelf::default()
            });
        }
        StorageSource::Disk(ScanRoots {
            worktrees: root.join("managed"),
            temp: root.join("tmp"),
            uid: 7,
        })
        .input(&Snapshot {
            agents: vec![Arc::new(agent)],
            ..Snapshot::default()
        })
    }

    fn at() -> Instant {
        Instant::now()
    }

    #[test]
    fn identical_inputs_reuse_the_completion_until_the_window_ends() {
        let mut session = Session::new(FRESH_FOR);
        let now = at();
        let current = input("tree", false, "Alpha");
        assert!(matches!(
            session.decide(&current, now, false),
            Decision::Scan
        ));
        assert_eq!(
            session.commit(
                &current,
                &measured(10, Coverage::Complete, Coverage::Complete),
                now,
                50
            ),
            Commit::Fresh
        );
        match session.decide(&input("tree", false, "Renamed"), now, false) {
            Decision::Keep { inventory, shown } => {
                assert_eq!(inventory.total.worktree_bytes, 10);
                assert_eq!(shown, Shown::Kept { at_ms: 50 });
            }
            other => panic!("expected keep, got {other:?}"),
        }
        match session.decide(&current, now + FRESH_FOR, false) {
            Decision::Keep { .. } => {}
            other => panic!("the window is inclusive, got {other:?}"),
        }
        match session.decide(&current, now + FRESH_FOR + Duration::from_millis(1), false) {
            Decision::Refresh { inventory, shown } => {
                assert_eq!(inventory.total.worktree_bytes, 10);
                assert!(matches!(shown, Shown::Refreshing { at_ms: 50 }));
            }
            other => panic!("expected refresh, got {other:?}"),
        }
    }

    #[test]
    fn explicit_rescan_and_a_zero_window_walk_without_hiding_the_rows() {
        let mut session = Session::new(FRESH_FOR);
        let now = at();
        let current = input("tree", false, "Alpha");
        session.commit(
            &current,
            &measured(10, Coverage::Complete, Coverage::Complete),
            now,
            50,
        );
        assert!(matches!(
            session.decide(&current, now, true),
            Decision::Refresh { .. }
        ));
        let mut stale = Session::new(Duration::ZERO);
        stale.commit(
            &current,
            &measured(10, Coverage::Complete, Coverage::Complete),
            now,
            50,
        );
        assert!(matches!(
            stale.decide(&current, now, false),
            Decision::Refresh { .. }
        ));
    }

    #[test]
    fn changed_scan_inputs_are_not_shown_from_the_previous_slot() {
        let mut session = Session::new(FRESH_FOR);
        let now = at();
        let current = input("tree", false, "Alpha");
        session.commit(
            &current,
            &measured(10, Coverage::Complete, Coverage::Complete),
            now,
            50,
        );
        for changed in [input("other", false, "Alpha"), input("tree", true, "Alpha")] {
            assert!(
                matches!(session.decide(&changed, now, false), Decision::Scan),
                "changed inputs must not reuse the slot"
            );
        }
        let mut other_source = current.clone();
        other_source.source = StorageSource::Demo;
        assert!(matches!(
            session.decide(&other_source, now, false),
            Decision::Scan
        ));
        let mut extra = current.clone();
        extra.threads.insert(
            "second".into(),
            super::super::ThreadMetadata {
                title: "Second".into(),
                project_id: "p".into(),
                worktree: Some(Path::new("/synthetic/second").into()),
                settled: false,
            },
        );
        assert!(matches!(session.decide(&extra, now, false), Decision::Scan));
        match session.retained(&current) {
            Some((inventory, Shown::Kept { at_ms: 50 })) => {
                assert_eq!(inventory.total.worktree_bytes, 10);
            }
            other => panic!("expected the retained completion, got {other:?}"),
        }
        assert!(session.retained(&input("other", false, "Alpha")).is_none());
    }

    #[test]
    fn a_partial_or_unavailable_result_drops_only_the_matching_slot() {
        let mut session = Session::new(FRESH_FOR);
        let now = at();
        let current = input("tree", false, "Alpha");
        session.commit(
            &current,
            &measured(10, Coverage::Complete, Coverage::Complete),
            now,
            50,
        );
        let other = input("other", false, "Alpha");
        assert_eq!(
            session.commit(
                &other,
                &measured(4, Coverage::Partial, Coverage::Complete),
                now,
                60
            ),
            Commit::Rejected
        );
        assert!(matches!(
            session.decide(&current, now, false),
            Decision::Keep { .. }
        ));
        assert_eq!(
            session.commit(
                &current,
                &measured(4, Coverage::Complete, Coverage::Unavailable),
                now,
                70
            ),
            Commit::Rejected
        );
        assert!(matches!(
            session.decide(&current, now, false),
            Decision::Scan
        ));
        assert!(session.retained(&current).is_none());
        session.commit(
            &current,
            &measured(12, Coverage::Complete, Coverage::Complete),
            now,
            80,
        );
        match session.decide(&current, now, false) {
            Decision::Keep { inventory, shown } => {
                assert_eq!(inventory.total.worktree_bytes, 12);
                assert_eq!(shown, Shown::Kept { at_ms: 80 });
            }
            other => panic!("expected the replacement, got {other:?}"),
        }
    }

    #[test]
    fn one_slot_replaces_the_previous_completion() {
        let mut session = Session::new(FRESH_FOR);
        let now = at();
        let first = input("tree", false, "Alpha");
        let second = input("other", false, "Beta");
        session.commit(
            &first,
            &measured(10, Coverage::Complete, Coverage::Complete),
            now,
            50,
        );
        session.commit(
            &second,
            &measured(20, Coverage::Complete, Coverage::Complete),
            now,
            60,
        );
        assert!(matches!(session.decide(&first, now, false), Decision::Scan));
        match session.decide(&second, now, false) {
            Decision::Keep { inventory, .. } => assert_eq!(inventory.total.worktree_bytes, 20),
            other => panic!("expected the only slot, got {other:?}"),
        }
    }
}
