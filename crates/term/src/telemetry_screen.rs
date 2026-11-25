//! Interactive Telemetry screen inside `loupe` (`e` key).
//!
//! Mirrors `StorageScreen` and `InsightsScreen`: borrows `State.telemetry` (or the zero-I/O
//! demo report in `--demo` mode) and renders through `TelemetryView`'s cached body canvas.
//! Also supports an explicit, user-confirmed handoff (`f`) that starts a T3 Code thread
//! via `t3 thread start` with Loupe's non-secret findings and fix options.

use crate::platform::Wake;
use loupe_core::{
    clock,
    host::{self, Host},
    integration,
    model::State,
    product::{Database, Origin},
    telemetry::{self, Report, Verdict},
    telemetry_view::{HandoffState, Motion, TelemetryView},
    terminal::Canvas,
};
use std::{
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, mpsc},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TelemetrySource {
    Demo,
    Machine,
    Unavailable,
}

impl TelemetrySource {
    pub(super) fn for_database(database: &Database) -> Self {
        match database {
            Database::Demo => Self::Demo,
            Database::Path {
                origin: Origin::Db, ..
            } => Self::Unavailable,
            Database::Path { .. } => Self::Machine,
        }
    }
}

enum HandoffOutcome {
    Started {
        thread_id: String,
        project_name: String,
        opened_app: bool,
    },
    Error(String),
}

pub(super) struct TelemetryScreen {
    source: TelemetrySource,
    wake: Option<Arc<Wake>>,
    view: Option<TelemetryView>,
    demo_report: Option<Report>,
    demo_revision: u64,
    handoff: HandoffState,
    handoff_rx: Option<mpsc::Receiver<HandoffOutcome>>,
    projects: Vec<(String, String)>,
    project_idx: usize,
}

impl TelemetryScreen {
    #[allow(dead_code)]
    pub(super) fn new(source: TelemetrySource) -> Self {
        Self {
            source,
            wake: None,
            view: None,
            demo_report: None,
            demo_revision: 0,
            handoff: HandoffState::Idle,
            handoff_rx: None,
            projects: Vec::new(),
            project_idx: 0,
        }
    }

    pub(super) fn with_wake(source: TelemetrySource, wake: Arc<Wake>) -> Self {
        Self {
            wake: Some(wake),
            ..Self::new(source)
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.view.is_some()
    }

    pub(super) fn open(&mut self, state: &State, host: &mut Host) {
        self.view = Some(TelemetryView::new());
        if !matches!(self.handoff, HandoffState::Starting { .. }) {
            self.handoff = HandoffState::Idle;
        }
        match self.source {
            TelemetrySource::Demo => {
                self.demo_report = Some(telemetry::demo::report(state.now));
                self.demo_revision = self.demo_revision.wrapping_add(1);
            }
            TelemetrySource::Machine => {
                if state.telemetry.is_none() {
                    host::force_telemetry_refresh(host);
                }
            }
            TelemetrySource::Unavailable => {}
        }
    }

    pub(super) fn close(&mut self) {
        self.view = None;
        if !matches!(self.handoff, HandoffState::Starting { .. }) {
            self.handoff = HandoffState::Idle;
        }
    }

    /// Drain completed background `t3 thread start` handoffs.
    pub(super) fn poll(&mut self) -> bool {
        let Some(rx) = &self.handoff_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(HandoffOutcome::Started {
                thread_id,
                project_name,
                opened_app,
            }) => {
                self.handoff_rx = None;
                self.handoff = HandoffState::Started {
                    thread_id,
                    project_name,
                    opened_app,
                };
                true
            }
            Ok(HandoffOutcome::Error(err)) => {
                self.handoff_rx = None;
                self.handoff = HandoffState::Error(err);
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.handoff_rx = None;
                if matches!(self.handoff, HandoffState::Starting { .. }) {
                    self.handoff = HandoffState::Error("worker stopped unexpectedly".into());
                }
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }

    fn active_report<'a>(&'a self, state: &'a State) -> (Option<&'a Report>, u64) {
        match self.source {
            TelemetrySource::Demo => (self.demo_report.as_ref(), self.demo_revision),
            TelemetrySource::Machine => (state.telemetry.as_deref(), state.telemetry_revision),
            TelemetrySource::Unavailable => (None, 0),
        }
    }

    pub(super) fn render(
        &mut self,
        state: &State,
        host: &Host,
        (cols, rows): (usize, usize),
    ) -> Option<Canvas> {
        if self.view.is_none() {
            return None;
        }
        let unavailable = self.source == TelemetrySource::Unavailable;
        let checking = self.source == TelemetrySource::Machine && host::telemetry_inflight(host);
        let (report_ref, rev) = match self.source {
            TelemetrySource::Demo => (self.demo_report.as_ref(), self.demo_revision),
            TelemetrySource::Machine => (state.telemetry.as_deref(), state.telemetry_revision),
            TelemetrySource::Unavailable => (None, 0),
        };
        let handoff = self.handoff.clone();
        let view = self.view.as_mut()?;
        Some(view.render_with_handoff(
            state,
            report_ref,
            rev,
            checking,
            unavailable,
            &handoff,
            cols.max(1),
            rows.max(1),
        ))
    }

    /// Returns `true` when the user pressed `q` to quit Loupe.
    pub(super) fn key(&mut self, name: &str, state: &State, host: &mut Host) -> bool {
        if self.view.is_none() {
            return false;
        }

        // Inline confirmation modal for `f` ("have a T3 thread turn this off")
        if matches!(self.handoff, HandoffState::Confirm { .. }) {
            match name {
                "q" => return true,
                "escape" => {
                    self.handoff = HandoffState::Idle;
                    return false;
                }
                "left" => {
                    if !self.projects.is_empty() {
                        self.project_idx = if self.project_idx == 0 {
                            self.projects.len() - 1
                        } else {
                            self.project_idx - 1
                        };
                        let project_name = self.projects[self.project_idx].1.clone();
                        self.handoff = HandoffState::Confirm {
                            project_name,
                            project_count: self.projects.len(),
                        };
                    }
                    return false;
                }
                "right" | "tab" => {
                    if !self.projects.is_empty() {
                        self.project_idx = (self.project_idx + 1) % self.projects.len();
                        let project_name = self.projects[self.project_idx].1.clone();
                        self.handoff = HandoffState::Confirm {
                            project_name,
                            project_count: self.projects.len(),
                        };
                    }
                    return false;
                }
                "enter" => {
                    self.launch_handoff(state, host);
                    return false;
                }
                _ => return false,
            }
        }

        let Some(view) = self.view.as_mut() else {
            return false;
        };
        match name {
            "q" => return true,
            "?" | "h" => {
                view.help = !view.help;
                view.scroll = 0;
                return false;
            }
            "escape" => {
                if view.help {
                    view.help = false;
                    view.scroll = 0;
                } else if !matches!(
                    self.handoff,
                    HandoffState::Idle | HandoffState::Starting { .. }
                ) {
                    self.handoff = HandoffState::Idle;
                } else {
                    self.close();
                }
                return false;
            }
            "e" => {
                self.close();
                return false;
            }
            _ if view.help => {
                match name {
                    "down" | "j" => view.scroll_motion(Motion::Rows(1)),
                    "up" | "k" => view.scroll_motion(Motion::Rows(-1)),
                    "pagedown" => view.scroll_motion(Motion::Page(1)),
                    "pageup" => view.scroll_motion(Motion::Page(-1)),
                    "home" | "g" => view.scroll_motion(Motion::Start),
                    "end" | "G" => view.scroll_motion(Motion::End),
                    _ => {}
                }
                return false;
            }
            "f" => {
                let can_fix = self
                    .active_report(state)
                    .0
                    .is_some_and(|r| r.verdict != Verdict::Off);
                if !can_fix || matches!(self.handoff, HandoffState::Starting { .. }) {
                    return false;
                }
                match self.source {
                    TelemetrySource::Demo => {
                        self.handoff = HandoffState::DemoPreview;
                    }
                    TelemetrySource::Unavailable => {
                        self.handoff = HandoffState::Error("unavailable with --db".into());
                    }
                    TelemetrySource::Machine => {
                        self.projects = collect_projects(state);
                        self.project_idx = 0;
                        let project_name = self.projects[0].1.clone();
                        self.handoff = HandoffState::Confirm {
                            project_name,
                            project_count: self.projects.len(),
                        };
                    }
                }
            }
            "tab" | "shift-tab" => {
                view.toggle_tab();
            }
            "r" => match self.source {
                TelemetrySource::Demo => {
                    self.demo_report = Some(telemetry::demo::report(clock::wall_now()));
                    self.demo_revision = self.demo_revision.wrapping_add(1);
                }
                TelemetrySource::Machine => {
                    host::force_telemetry_refresh(host);
                }
                TelemetrySource::Unavailable => {}
            },
            "down" | "j" => {
                let total = self
                    .active_report(state)
                    .0
                    .map(TelemetryView::selectable_count)
                    .unwrap_or(0);
                if let Some(v) = self.view.as_mut() {
                    v.move_selection(1, total);
                }
            }
            "up" | "k" => {
                let total = self
                    .active_report(state)
                    .0
                    .map(TelemetryView::selectable_count)
                    .unwrap_or(0);
                if let Some(v) = self.view.as_mut() {
                    v.move_selection(-1, total);
                }
            }
            "pagedown" => view.scroll_motion(Motion::Page(1)),
            "pageup" => view.scroll_motion(Motion::Page(-1)),
            "home" | "g" => {
                if view.tab == loupe_core::telemetry_view::Tab::Telemetry {
                    view.selected = 0;
                }
                view.scroll_motion(Motion::Start);
            }
            "end" | "G" => {
                let total = self
                    .active_report(state)
                    .0
                    .map(TelemetryView::selectable_count)
                    .unwrap_or(0);
                if let Some(v) = self.view.as_mut() {
                    if v.tab == loupe_core::telemetry_view::Tab::Telemetry && total > 0 {
                        v.selected = total - 1;
                    }
                    v.scroll_motion(Motion::End);
                }
            }
            _ => {}
        }
        false
    }

    fn launch_handoff(&mut self, state: &State, host: &Host) {
        let Some(report) = self.active_report(state).0.cloned() else {
            self.handoff = HandoffState::Error("no telemetry report yet".into());
            return;
        };
        let (project_id, project_name) = self
            .projects
            .get(self.project_idx)
            .cloned()
            .unwrap_or_else(|| (".".into(), "current folder".into()));
        let (title, prompt) = telemetry::t3_handoff_prompt(&report);
        let data_folder = report.inputs.data_folder.clone();
        let env_id = host.db.as_deref().and_then(integration::environment_id);
        let wake = self.wake.clone();

        let (tx, rx) = mpsc::channel();
        self.handoff_rx = Some(rx);
        self.handoff = HandoffState::Starting {
            project_name: project_name.clone(),
        };

        let _ = std::thread::Builder::new()
            .name("loupe-t3-handoff".into())
            .spawn(move || {
                let outcome =
                    run_t3_thread_start(&project_id, &project_name, &title, &prompt, &data_folder, env_id.as_deref());
                let _ = tx.send(outcome);
                if let Some(w) = wake {
                    w.waker().wake();
                }
            });
    }

    pub(super) fn press(&mut self, y: i32) {
        if let Some(view) = self.view.as_mut() {
            view.press(y);
        }
    }

    pub(super) fn wheel(&mut self, down: bool, state: &State, host: &mut Host) {
        self.key(if down { "down" } else { "up" }, state, host);
    }
}

fn collect_projects(state: &State) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push_unique = |id: &str, name: &str| {
        if id.is_empty() || out.iter().any(|(existing, _)| existing == id) {
            return;
        }
        let label = if name.is_empty() { id } else { name };
        out.push((id.to_owned(), label.to_owned()));
    };

    fn agent_project_key(a: &loupe_core::model::Agent) -> Option<&str> {
        a.storage
            .as_ref()
            .map(|s| s.project_id.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| (!a.cwd.is_empty()).then_some(a.cwd.as_str()))
    }

    // Prefer the selected or currently focused T3 thread's project first.
    for preferred in [state.selected.as_deref(), state.linked.as_deref()]
        .into_iter()
        .flatten()
    {
        if let Some(a) = state
            .snap
            .agents
            .iter()
            .find(|a| a.id == preferred && a.remote.is_none())
        {
            if let Some(key) = agent_project_key(a) {
                push_unique(key, &a.project);
            }
        }
    }

    for a in &state.snap.agents {
        if a.remote.is_none() {
            if let Some(key) = agent_project_key(a) {
                push_unique(key, &a.project);
            }
        }
    }

    if out.is_empty() {
        out.push((".".into(), "current folder".into()));
    }
    out
}

fn find_t3_binary() -> PathBuf {
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_env) {
            let cand = dir.join("t3");
            if cand.is_file() {
                return cand;
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
        let local = PathBuf::from(home).join(".local/bin/t3");
        if local.is_file() {
            return local;
        }
    }
    PathBuf::from("t3")
}

fn has_gui_display() -> bool {
    if cfg!(target_vendor = "apple") {
        std::env::var_os("SSH_CONNECTION").is_none()
    } else {
        std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
            || std::env::var_os("DISPLAY").is_some_and(|v| !v.is_empty())
    }
}

fn run_t3_thread_start(
    project_id: &str,
    project_name: &str,
    title: &str,
    prompt: &str,
    data_folder: &std::path::Path,
    env_id: Option<&str>,
) -> HandoffOutcome {
    let bin = find_t3_binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("thread")
        .arg("start")
        .arg("--title")
        .arg(title)
        .arg("--json");
    if data_folder.is_absolute() {
        cmd.arg("--base-dir").arg(data_folder);
    }
    cmd.arg(project_id)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return HandoffOutcome::Error(
                "t3 CLI not found on PATH (copy the command above instead)".into(),
            );
        }
        Err(e) => {
            return HandoffOutcome::Error(format!("failed to run t3: {e}"));
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return HandoffOutcome::Error(format!("t3 CLI failed: {e}")),
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let msg = stderr
            .lines()
            .chain(stdout.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("t3 thread start failed");
        let clean: String = msg.chars().take(90).collect();
        return HandoffOutcome::Error(clean);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let thread_id = telemetry::parse_t3_thread_id(&stdout).unwrap_or_else(|| "started".into());

    let mut opened_app = false;
    if has_gui_display() {
        if let Some(env) = env_id {
            if let Some(link) = integration::thread_link(env, &thread_id) {
                opened_app = integration::open_link(&link).is_ok();
            }
        }
    }

    HandoffOutcome::Started {
        thread_id,
        project_name: project_name.to_owned(),
        opened_app,
    }
}
