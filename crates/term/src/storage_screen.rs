//! Owns the on-demand job and the one retained measurement for this terminal session.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use t3_agents_core::{
    headroom::{Volume, WarningVolume},
    host::{self, Host},
    model::{self, Agent, State},
    storage::{Coverage, Inventory},
    storage_view::{
        Effect, StorageView,
        adapter::{self, ProjectMetadata, ThreadMetadata},
        data::{self, Scan, Shown, ThreadState, VolumePath},
    },
    terminal::{Canvas, short_duration},
};
use t3_agents_term::{
    platform::Wake,
    storage::{Decision, OwnedInput, ScanState, Session, StorageSource, StorageWorker, walk_key},
};

pub(super) struct StorageScreen {
    source: StorageSource,
    wake: Arc<Wake>,
    worker: Option<StorageWorker>,
    view: Option<StorageView>,
    agents: Vec<Arc<Agent>>,
    activity: BTreeMap<String, Activity>,
    headroom: Vec<WarningVolume>,
    display: data::Inventory,
    session: Session,
    input: Option<OwnedInput>,
}

impl StorageScreen {
    pub(super) fn new(source: StorageSource, wake: Arc<Wake>) -> Self {
        Self {
            source,
            wake,
            worker: None,
            view: None,
            agents: Vec::new(),
            activity: BTreeMap::new(),
            headroom: Vec::new(),
            display: data::Inventory::default(),
            session: Session::default(),
            input: None,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    pub(super) fn open(&mut self, dashboard: &State) {
        self.view = Some(StorageView::default());
        self.request(dashboard, false);
        self.update_headroom(dashboard);
    }

    fn close(&mut self) {
        // Stop the walk. The retained completion stays in the session.
        if let Some(worker) = &mut self.worker {
            worker.cancel();
        }
        self.view = None;
        self.input = None;
        self.agents.clear();
        self.activity.clear();
        self.headroom.clear();
        self.display = data::Inventory::default();
    }

    fn ensure_worker(&mut self) {
        if self
            .worker
            .as_ref()
            .is_none_or(|worker| matches!(worker.state(), ScanState::Failed))
        {
            let wake = Arc::clone(&self.wake);
            self.worker = StorageWorker::new(move || wake.waker().wake()).ok();
        }
    }

    fn request(&mut self, dashboard: &State, explicit: bool) {
        if self.source == StorageSource::Unavailable {
            return;
        }
        self.agents.clone_from(&dashboard.snap.agents);
        let input = self.source.input(&dashboard.snap);
        let activity = thread_activity(&input, dashboard);
        self.begin_measurement(&input, &activity, explicit);
        self.input = Some(input);
        self.activity = activity;
    }

    fn begin_measurement(
        &mut self,
        input: &OwnedInput,
        activity: &BTreeMap<String, Activity>,
        explicit: bool,
    ) {
        let decision = self.session.decide(input, Instant::now(), explicit);
        if !matches!(decision, Decision::Keep { .. }) {
            self.ensure_worker();
            if let Some(worker) = &mut self.worker {
                worker.rescan(input.clone());
            }
        }
        let next = match decision {
            Decision::Keep { inventory, shown } | Decision::Refresh { inventory, shown } => {
                let shown = match (shown, self.worker.as_ref().map(StorageWorker::state)) {
                    (Shown::Refreshing { at_ms }, None | Some(ScanState::Failed)) => {
                        Shown::Kept { at_ms }
                    }
                    _ => shown,
                };
                display(&inventory, input, activity, shown)
            }
            Decision::Scan => data::Inventory {
                scan: match self.worker.as_ref().map(StorageWorker::state) {
                    Some(ScanState::Scanning) => Scan::Scanning(Default::default()),
                    _ => Scan::Unavailable,
                },
                ..data::Inventory::default()
            },
        };
        self.replace_display(next);
    }

    /// A new snapshot can change a title or activity while the same walk is in
    /// flight. Keep that job, including any completion waiting in the mailbox.
    fn update_snapshot(&mut self, dashboard: &State) {
        self.agents.clone_from(&dashboard.snap.agents);
        let input = self.source.input(&dashboard.snap);
        let activity = thread_activity(&input, dashboard);
        let metadata_changed = self.input.as_ref() != Some(&input);
        if metadata_changed
            && self
                .input
                .as_ref()
                .is_none_or(|old| walk_key(old) != walk_key(&input))
        {
            self.begin_measurement(&input, &activity, true);
        } else if metadata_changed || activity != self.activity {
            let next = match self.worker.as_ref().map(StorageWorker::state) {
                Some(ScanState::Ready(result)) => {
                    Some(display(result, &input, &activity, self.display.shown))
                }
                _ => self
                    .session
                    .retained(&input)
                    .map(|(result, _)| display(&result, &input, &activity, self.display.shown)),
            };
            if let Some(next) = next {
                self.replace_display(next);
            }
        }
        self.input = Some(input);
        self.activity = activity;
    }

    pub(super) fn poll(&mut self, dashboard: &State) {
        if !self.is_open() || self.source == StorageSource::Unavailable {
            return;
        }
        let current = &dashboard.snap.agents;
        if current.len() != self.agents.len()
            || current
                .iter()
                .zip(&self.agents)
                .any(|(a, b)| !Arc::ptr_eq(a, b))
        {
            self.update_snapshot(dashboard);
        }
        self.update_headroom(dashboard);
        let (Some(worker), Some(input)) = (&mut self.worker, &self.input) else {
            return;
        };
        if !worker.poll() {
            return;
        }
        let next = match worker.state() {
            ScanState::Ready(result) => {
                self.session
                    .commit(input, result, Instant::now(), unix_ms());
                display(result, input, &self.activity, Shown::Current)
            }
            ScanState::Failed => self
                .session
                .retained(input)
                .map(|(result, shown)| display(&result, input, &self.activity, shown))
                .unwrap_or_default(),
            _ => return,
        };
        self.replace_display(next);
    }

    fn replace_display(&mut self, mut display: data::Inventory) {
        // A rescan changes folder measurements without invalidating the latest volume sample.
        if self.source == StorageSource::Demo && self.display.worktree_volumes.is_empty() {
            self.display.worktree_volumes.push(VolumePath {
                label: "sample".into(),
                volume: Some(Volume {
                    device: 0,
                    available_bytes: 120_000_000_000,
                    total_bytes: 500_000_000_000,
                    is_tmpfs: Some(false),
                }),
            });
        }
        display.worktree_volumes = std::mem::take(&mut self.display.worktree_volumes);
        display.temporary_volume = self.display.temporary_volume.take();
        display.set_temp_volume_used(self.display.temp_volume_used);
        self.display = display;
    }

    fn update_headroom(&mut self, dashboard: &State) {
        if !matches!(self.source, StorageSource::Disk(_)) || self.headroom == dashboard.headroom {
            return;
        }
        self.headroom.clone_from(&dashboard.headroom);
        self.display.worktree_volumes = self
            .headroom
            .iter()
            .filter(|sample| sample.worktrees)
            .map(|sample| VolumePath {
                label: sample.worktree_mount.clone(),
                volume: Some(sample.volume.clone()),
            })
            .collect();
        let temp = self.headroom.iter().find(|sample| sample.temporary);
        self.display.temporary_volume = temp.map(|sample| VolumePath {
            label: sample.temp_mount.clone(),
            volume: Some(sample.volume.clone()),
        });
        self.display.set_temp_volume_used(
            temp.and_then(|sample| sample.dedicated_temp.as_ref())
                .and_then(|usage| usage.used_bytes),
        );
    }

    pub(super) fn render(&mut self, dashboard: &State, (w, h): (usize, usize)) -> Option<Canvas> {
        let view = self.view.as_mut()?;
        Some(view.render(&self.display, dashboard, w, h, &dashboard.palette))
    }

    pub(super) fn press(&mut self, y: i32, (w, h): (usize, usize)) {
        if let Some(view) = &mut self.view
            && let Ok(y) = usize::try_from(y)
        {
            view.select_at(&self.display, w, h, y);
        }
    }

    pub(super) fn key(
        &mut self,
        key: &str,
        dashboard: &mut State,
        host: &Host,
        (w, h): (usize, usize),
    ) -> bool {
        let effect = self
            .view
            .as_mut()
            .and_then(|view| view.key(key, &self.display, w, h));
        match effect {
            Some(Effect::Quit) => return true,
            Some(Effect::Back) => self.close(),
            Some(Effect::Rescan) => self.request(dashboard, true),
            Some(Effect::OpenInT3(id)) => host::open_local_thread(dashboard, host, &id),
            None => {}
        }
        false
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[derive(PartialEq, Eq)]
struct Activity {
    state: ThreadState,
    age: Option<String>,
}

fn thread_activity(input: &OwnedInput, dashboard: &State) -> BTreeMap<String, Activity> {
    let paces = dashboard.paces();
    let mut activity = BTreeMap::new();
    for a in &dashboard.snap.agents {
        if !input.threads.contains_key(&a.thread_id) || a.remote.is_some() || a.storage.is_none() {
            continue;
        }
        activity.entry(a.thread_id.clone()).or_insert_with(|| {
            let pace = paces.get(&a.id);
            let state = match a.status.as_str() {
                _ if model::needs_you(a) => ThreadState::NeedsYou,
                "queued" | "pending" => ThreadState::Queued,
                "idle" => ThreadState::Quiet,
                _ if pace.is_some_and(|p| p.quiet) => ThreadState::Quiet,
                _ if model::live(a) => ThreadState::Running,
                "failed" | "error" => ThreadState::Failed,
                "interrupted" | "aborted" | "canceled" | "cancelled" => ThreadState::Interrupted,
                _ => ThreadState::Done,
            };
            let age = if state == ThreadState::Quiet {
                pace.map(|p| short_duration(p.quiet_ms / 1000.))
            } else {
                model::ms(a.ended.as_deref().or(a.started.as_deref()))
                    .map(|at| short_duration(((dashboard.now - at) / 1000.).max(0.)))
            };
            Activity { state, age }
        });
    }
    activity
}

fn display(
    result: &Inventory,
    input: &OwnedInput,
    activity: &BTreeMap<String, Activity>,
    shown: Shown,
) -> data::Inventory {
    if result.worktrees == Coverage::Unavailable && result.claude == Coverage::Unavailable {
        return data::Inventory::default();
    }
    let folders = result.folders.len();
    let incomplete = result
        .folders
        .iter()
        .filter(|folder| folder.coverage != Coverage::Complete)
        .count();
    let projects: Vec<_> = input
        .projects
        .iter()
        .map(|(id, p)| ProjectMetadata {
            id,
            title: &p.title,
        })
        .collect();
    let threads: Vec<_> = input
        .threads
        .iter()
        .filter_map(|(id, t)| {
            let current = activity.get(id)?;
            Some(ThreadMetadata {
                id,
                title: &t.title,
                state: current.state,
                age: current.age.as_deref(),
            })
        })
        .collect();
    let mut display = adapter::convert(
        result,
        &projects,
        &threads,
        Vec::new(),
        None,
        Scan::Finished {
            folders,
            incomplete,
            elapsed: None,
            finished_at: None,
        },
    );
    display.incomplete_coverage =
        result.worktrees != Coverage::Complete || result.claude != Coverage::Complete;
    display.shown = shown;
    display
}
