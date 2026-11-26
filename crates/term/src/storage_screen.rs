//! Owns the on-demand job and the one retained measurement for this terminal session.

use crate::t3_handoff::T3Handoff;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use loupe_core::{
    cleanup_rules::{self, RulesRead, ThreadFact},
    handoff,
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
use loupe_term::{
    platform::Wake,
    storage::{
        DEMO_WORKTREES, Decision, Facts, OwnedInput, ScanState, Scanned, Session, StorageSource,
        StorageWorker, walk_key,
    },
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
    /// T3's cleanup settings, re-read on open, on `r`, and when the file's stat changes.
    rules: Option<RulesRead>,
    rules_sig: Option<(u64, u64, i128, u64)>,
    /// Thread facts for the rows on screen.
    facts: Facts,
    now: f64,
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
            rules: None,
            rules_sig: None,
            facts: Facts::None,
            now: 0.,
        }
    }

    /// Read T3's settings.json (allowlisted keys only). `force` re-reads even when the file's
    /// stat is unchanged. Returns whether the rules changed.
    fn reload_rules(&mut self, force: bool) -> bool {
        let next = match &self.source {
            StorageSource::Demo => {
                if self.rules.is_some() {
                    return false;
                }
                cleanup_rules::demo_settings()
            }
            StorageSource::Unavailable => return false,
            StorageSource::Disk(roots) => {
                let sig = cleanup_rules::fingerprint(roots.data_folder());
                if !force && self.rules.is_some() && sig == self.rules_sig {
                    return false;
                }
                self.rules_sig = sig;
                cleanup_rules::read(roots.data_folder())
            }
        };
        let changed = self.rules.as_ref() != Some(&next);
        self.rules = Some(next);
        changed
    }

    /// Facts with demo times placed on the dashboard clock.
    fn thread_facts(&self) -> Option<std::borrow::Cow<'_, [ThreadFact]>> {
        resolve_facts(&self.facts, self.now)
    }

    fn attach_band(&self, display: &mut data::Inventory) {
        let Some(read) = &self.rules else {
            display.cleanup = None;
            return;
        };
        let home = loupe_core::platform::home_dir().map(std::path::PathBuf::from);
        let (roots, custom_root) = match (&self.source, read.settings()) {
            (StorageSource::Disk(roots), Some(settings)) => (
                cleanup_rules::managed_roots(&settings, &roots.worktrees, home.as_deref()),
                cleanup_rules::custom_root(&settings, &roots.worktrees, home.as_deref()),
            ),
            (StorageSource::Disk(roots), None) => (vec![roots.worktrees.clone()], false),
            _ => (vec![std::path::PathBuf::from(DEMO_WORKTREES)], false),
        };
        let facts = self.thread_facts();
        let entry_facts = facts.as_ref().map(|facts| {
            let idle = |id: &str| self.activity.get(id).is_some_and(|a| a.idle);
            cleanup_rules::entry_facts(display, facts, &idle, &roots)
        });
        if let Some(map) = &entry_facts {
            cleanup_rules::attribute(display, map);
        }
        let mut projects: Vec<String> = self
            .input
            .as_ref()
            .map(|input| input.projects.keys().cloned().collect())
            .unwrap_or_default();
        if let Some(facts) = &facts {
            projects.extend(facts.iter().map(|f| f.project_id.clone()));
        }
        projects.sort();
        projects.dedup();
        projects.retain(|id| !id.is_empty());
        display.cleanup = Some(cleanup_rules::summarize(
            read,
            display,
            entry_facts.as_ref(),
            &projects,
            custom_root,
            self.now,
        ));
    }

    /// Rebuild the band over the rows already on screen.
    fn refresh_band(&mut self) {
        let mut display = std::mem::take(&mut self.display);
        self.attach_band(&mut display);
        self.display = display;
    }

    pub(super) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    pub(super) fn open(&mut self, dashboard: &State) {
        self.view = Some(StorageView::default());
        self.now = dashboard.now;
        self.reload_rules(true);
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
        self.facts = Facts::None;
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
        if explicit {
            self.reload_rules(true);
        }
        self.now = dashboard.now;
        self.agents.clone_from(&dashboard.snap.agents);
        let input = self.source.input(&dashboard.snap);
        let activity = thread_activity(&input, dashboard);
        self.input = Some(input.clone());
        self.activity = activity;
        let activity = std::mem::take(&mut self.activity);
        self.begin_measurement(&input, &activity, explicit);
        self.activity = activity;
        self.refresh_band();
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
        if matches!(decision, Decision::Scan) {
            self.facts = Facts::None;
        }
        let next = match decision {
            Decision::Keep { inventory, shown } | Decision::Refresh { inventory, shown } => {
                let shown = match (shown, self.worker.as_ref().map(StorageWorker::state)) {
                    (Shown::Refreshing { at_ms }, None | Some(ScanState::Failed)) => {
                        Shown::Kept { at_ms }
                    }
                    _ => shown,
                };
                self.facts = inventory.facts.clone();
                display(&inventory, input, activity, shown, self.thread_facts().as_deref())
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
        self.now = dashboard.now;
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
            self.input = Some(input);
            self.activity = activity;
            self.refresh_band();
            return;
        } else if metadata_changed || activity != self.activity {
            let facts = self.thread_facts().map(std::borrow::Cow::into_owned);
            let next = match self.worker.as_ref().map(StorageWorker::state) {
                Some(ScanState::Ready(result)) => Some(display(
                    result,
                    &input,
                    &activity,
                    self.display.shown,
                    facts.as_deref(),
                )),
                _ => self.session.retained(&input).map(|(result, _)| {
                    display(&result, &input, &activity, self.display.shown, facts.as_deref())
                }),
            };
            self.input = Some(input);
            self.activity = activity;
            if let Some(next) = next {
                self.replace_display(next);
            } else {
                self.refresh_band();
            }
            return;
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
        if self.reload_rules(false) {
            self.refresh_band();
        }
        let (Some(worker), Some(input)) = (&mut self.worker, &self.input) else {
            return;
        };
        if !worker.poll() {
            return;
        }
        let shown: Option<(Scanned, Shown)> = match worker.state() {
            ScanState::Ready(result) => {
                self.session
                    .commit(input, result, Instant::now(), unix_ms());
                Some((result.clone(), Shown::Current))
            }
            ScanState::Failed => self.session.retained(input),
            _ => return,
        };
        self.now = dashboard.now;
        let next = match shown {
            Some((result, shown)) => {
                self.facts = result.facts.clone();
                let input = self.input.as_ref().expect("checked above");
                display(&result, input, &self.activity, shown, self.thread_facts().as_deref())
            }
            None => data::Inventory::default(),
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
        self.attach_band(&mut display);
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
        handoff: &mut T3Handoff,
        (w, h): (usize, usize),
    ) -> bool {
        let effect = self
            .view
            .as_mut()
            .and_then(|view| view.key(key, &self.display, w, h));
        match effect {
            Some(Effect::Quit) => return true,
            Some(Effect::Back) => {
                handoff.dismiss();
                self.close();
            }
            Some(Effect::Rescan) => self.request(dashboard, true),
            Some(Effect::OpenInT3(id)) => host::open_local_thread(dashboard, host, &id),
            Some(Effect::CleanInT3) => {
                if let Some((ctx, preferred_project)) = self
                    .view
                    .as_ref()
                    .and_then(|v| v.selected_context(&self.display, w, h))
                {
                    let (title, prompt) = handoff::storage_prompt(&ctx);
                    handoff.begin_start(
                        "clean up storage",
                        title,
                        prompt,
                        preferred_project.as_deref(),
                        None,
                        host.db.as_deref(),
                        true,
                        dashboard,
                    );
                }
            }
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
    /// T3 would call the thread idle: no run in progress and nothing waiting.
    idle: bool,
}

fn resolve_facts(facts: &Facts, now: f64) -> Option<std::borrow::Cow<'_, [ThreadFact]>> {
    match facts {
        Facts::Read(facts) => Some(std::borrow::Cow::Borrowed(facts.as_slice())),
        Facts::Demo => Some(std::borrow::Cow::Owned(cleanup_rules::demo_facts(now, &|name| {
            format!("{DEMO_WORKTREES}/{name}")
        }))),
        Facts::None | Facts::Failed => None,
    }
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
            let idle = matches!(
                state,
                ThreadState::Done | ThreadState::Failed | ThreadState::Interrupted
            ) || (a.status == "idle" && !model::live(a));
            Activity { state, age, idle }
        });
    }
    activity
}

fn display(
    result: &Inventory,
    input: &OwnedInput,
    activity: &BTreeMap<String, Activity>,
    shown: Shown,
    facts: Option<&[ThreadFact]>,
) -> data::Inventory {
    // T3's cleanup counts a thread as settled from `settledAt`, which auto-settle also sets.
    // Storage groups by the same field so the band and the rows agree.
    let settled_at: std::collections::HashSet<&str> = facts
        .unwrap_or_default()
        .iter()
        .filter(|f| f.settled() && !f.archived && !f.deleted)
        .map(|f| f.thread_id.as_str())
        .collect();
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
                state: if current.idle && settled_at.contains(id.as_str()) {
                    ThreadState::Settled
                } else {
                    current.state
                },
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
