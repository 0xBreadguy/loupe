//! Unified async T3 Code handoff controller (`t3 thread start` and `t3 thread send`).
//!
//! Shared across Telemetry, Storage, Trace, Insights, and Live so every screen uses the same
//! confirmation flow, exact payload preview, project/model picker, quick-reply composer,
//! SSH-safe launch behavior, and `--demo` / `--db` safety guards.

use crate::platform::Wake;
use loupe_core::{
    handoff::{self, HandoffState, ModelChoice, SendPreset},
    host::Host,
    integration,
    model::{Agent, Pace, State},
    terminal::{self, Canvas, Palette},
};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

const MAX_DRAFT_CHARS: usize = 400;
const MAX_CLI_OUTPUT_BYTES: usize = 64 * 1024;
const CLI_TIMEOUT: Duration = Duration::from_secs(30);
const PIPE_JOIN_TIMEOUT: Duration = Duration::from_millis(250);
const DROP_WAIT_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandoffSource {
    Demo,
    ExplicitDb,
    Machine,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HandoffKey {
    /// Key did not belong to the handoff bar.
    NotHandled,
    /// Key was consumed by the handoff bar; repaint.
    Consumed,
    /// User pressed `q` while a non-editing confirmation bar was open.
    Quit,
}

enum PendingAction {
    Start {
        action_label: String,
        /// Exact sanitized `--title=` value (same string previewed and passed to `t3`).
        title: String,
        prompt: String,
        projects: Vec<(String, String)>,
        project_idx: usize,
        models: Vec<ModelChoice>,
        model_idx: usize,
        destructive: bool,
        armed: bool,
        preview: bool,
        preview_scroll: usize,
        db_path: Option<PathBuf>,
        base_dir: Option<PathBuf>,
    },
    Send {
        thread_id: String,
        thread_title: String,
        presets: Vec<SendPreset>,
        preset_idx: usize,
        editing: bool,
        draft: String,
    },
}

enum WorkerOutcome {
    Done(String),
    Error(String),
}

pub(super) struct T3Handoff {
    source: HandoffSource,
    wake: Option<Arc<Wake>>,
    state: HandoffState,
    pending: Option<PendingAction>,
    rx: Option<mpsc::Receiver<WorkerOutcome>>,
    cancel: Option<Arc<AtomicBool>>,
    done_rx: Option<mpsc::Receiver<()>>,
}

impl Drop for T3Handoff {
    fn drop(&mut self) {
        if let Some(flag) = self.cancel.take() {
            flag.store(true, Ordering::Relaxed);
        }
        // Bounded wait so exiting Loupe reaps the child process group promptly without ever hanging.
        if let Some(done) = self.done_rx.take() {
            let _ = done.recv_timeout(DROP_WAIT_TIMEOUT);
        }
    }
}

impl T3Handoff {
    pub(super) fn new(source: HandoffSource, wake: Option<Arc<Wake>>) -> Self {
        Self {
            source,
            wake,
            state: HandoffState::Idle,
            pending: None,
            rx: None,
            cancel: None,
            done_rx: None,
        }
    }

    pub(super) fn state(&self) -> &HandoffState {
        &self.state
    }

    pub(super) fn is_active(&self) -> bool {
        !self.state.is_idle()
    }

    pub(super) fn captures_keys(&self) -> bool {
        matches!(
            self.state,
            HandoffState::ConfirmStart { .. } | HandoffState::ComposeSend { .. }
        )
    }

    pub(super) fn wants_text_input(&self) -> bool {
        matches!(
            self.state,
            HandoffState::ComposeSend { editing: true, .. }
        )
    }

    /// Clear any non-running banner (e.g. on screen switch or Esc).
    pub(super) fn dismiss(&mut self) {
        if !self.state.is_running() {
            self.state = HandoffState::Idle;
            self.pending = None;
        }
    }

    /// Start the confirmation flow for `t3 thread start`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn begin_start(
        &mut self,
        action_label: impl Into<String>,
        title: String,
        prompt: String,
        preferred_project_id: Option<&str>,
        base_dir: Option<PathBuf>,
        db_path: Option<&Path>,
        destructive: bool,
        dashboard: &State,
    ) {
        if self.state.is_running() {
            return;
        }
        let action_label = action_label.into();
        let exact_title = handoff::sanitize_title(&title);
        match self.source {
            HandoffSource::Demo => {
                self.pending = None;
                self.state = HandoffState::DemoPreview(format!(
                    "Demo: would start a T3 thread ({action_label})"
                ));
            }
            HandoffSource::ExplicitDb => {
                self.pending = None;
                self.state = HandoffState::Error("T3 handoff is unavailable with --db".into());
            }
            HandoffSource::Machine => {
                let resolved_base_dir = base_dir
                    .filter(|p| validate_base_dir(p).is_ok())
                    .or_else(|| {
                        db_path
                            .and_then(base_dir_from_db)
                            .filter(|p| validate_base_dir(p).is_ok())
                    });
                let scratch_project = db_path
                    .and_then(|p| handoff::read_scratch_project(p, resolved_base_dir.as_deref()));
                let projects = collect_projects(dashboard, preferred_project_id, scratch_project);
                let project_id = projects[0].0.clone();
                let models = db_path
                    .map(|p| handoff::read_model_choices(p, &project_id))
                    .unwrap_or_else(|| vec![ModelChoice::Unselected]);
                let pending = PendingAction::Start {
                    action_label,
                    title: exact_title,
                    prompt,
                    projects,
                    project_idx: 0,
                    models,
                    model_idx: 0,
                    destructive,
                    armed: false,
                    preview: true,
                    preview_scroll: 0,
                    db_path: db_path.map(Path::to_path_buf),
                    base_dir: resolved_base_dir,
                };
                self.state = state_for_pending(&pending);
                self.pending = Some(pending);
            }
        }
    }

    /// Start the preset / quick-reply flow for `t3 thread send` on the selected Live agent.
    pub(super) fn begin_send(&mut self, agent: &Agent, pace: Option<&Pace>) {
        if self.state.is_running() {
            return;
        }
        if agent.remote.is_some() {
            self.pending = None;
            self.state = HandoffState::Error(
                "Remote thread: open in T3 Code to send a message".into(),
            );
            return;
        }
        if agent.reference_only == Some(true) || agent.thread_id.is_empty() {
            self.pending = None;
            self.state = HandoffState::Error("No local T3 thread ID for this row".into());
            return;
        }
        match self.source {
            HandoffSource::Demo => {
                self.pending = None;
                self.state = HandoffState::DemoPreview(format!(
                    "Demo: would queue a message on \"{}\"",
                    agent.title
                ));
            }
            HandoffSource::ExplicitDb => {
                self.pending = None;
                self.state = HandoffState::Error("T3 handoff is unavailable with --db".into());
            }
            HandoffSource::Machine => {
                let presets = handoff::live_send_presets(agent, pace);
                let pending = PendingAction::Send {
                    thread_id: agent.thread_id.clone(),
                    thread_title: agent.title.clone(),
                    presets,
                    preset_idx: 0,
                    editing: false,
                    draft: String::new(),
                };
                self.state = state_for_pending(&pending);
                self.pending = Some(pending);
            }
        }
    }

    /// Drain completed background CLI calls.
    pub(super) fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(WorkerOutcome::Done(msg)) => {
                self.finish_worker();
                self.state = HandoffState::Done { message: msg };
                true
            }
            Ok(WorkerOutcome::Error(err)) => {
                self.finish_worker();
                self.state = HandoffState::Error(err);
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.finish_worker();
                if self.state.is_running() {
                    self.state = HandoffState::Error("T3 handoff worker stopped unexpectedly".into());
                }
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }

    fn finish_worker(&mut self) {
        self.rx = None;
        self.cancel = None;
        self.done_rx = None;
    }

    /// Handle a typed character while in free-text compose mode.
    /// Rejects control and hidden/BiDi Unicode so previewed and sent text match 1:1.
    pub(super) fn char(&mut self, ch: char) -> bool {
        let Some(PendingAction::Send {
            editing: true,
            draft,
            ..
        }) = &mut self.pending
        else {
            return false;
        };
        if ch.is_control() || terminal::is_hidden(ch) {
            return true;
        }
        if draft.chars().count() < MAX_DRAFT_CHARS {
            draft.push(ch);
            if let Some(pending) = &self.pending {
                self.state = state_for_pending(pending);
            }
        }
        true
    }

    /// Route a key event to the handoff bar when active.
    pub(super) fn key(&mut self, name: &str, host: &Host, size: (usize, usize)) -> HandoffKey {
        if self.state.is_idle() {
            return HandoffKey::NotHandled;
        }

        // Done / DemoPreview / Error banners dismiss cleanly on Esc (letting all other keys pass through).
        if !self.captures_keys() {
            if name == "escape" && !self.state.is_running() {
                self.dismiss();
                return HandoffKey::Consumed;
            }
            return HandoffKey::NotHandled;
        }

        let mut should_launch = false;
        let mut custom_error: Option<String> = None;
        let can_preview = size.0 >= handoff::MIN_PREVIEW_COLS && size.1 >= handoff::MIN_PREVIEW_ROWS;

        match &mut self.pending {
            Some(PendingAction::Start {
                projects,
                project_idx,
                models,
                model_idx,
                destructive,
                armed,
                preview,
                preview_scroll,
                db_path,
                ..
            }) => match name {
                "q" => return HandoffKey::Quit,
                "escape" => {
                    if *armed {
                        *armed = false;
                    } else {
                        self.dismiss();
                        return HandoffKey::Consumed;
                    }
                }
                "p" => {
                    *preview = !*preview;
                }
                "pagedown" => {
                    if *preview {
                        *preview_scroll = preview_scroll.saturating_add(6);
                    }
                }
                "pageup" => {
                    if *preview {
                        *preview_scroll = preview_scroll.saturating_sub(6);
                    }
                }
                "j" if *preview => {
                    *preview_scroll = preview_scroll.saturating_add(1);
                }
                "k" if *preview => {
                    *preview_scroll = preview_scroll.saturating_sub(1);
                }
                "left" | "right" | "tab" => {
                    if !projects.is_empty() {
                        *project_idx = if name == "left" {
                            if *project_idx == 0 {
                                projects.len() - 1
                            } else {
                                *project_idx - 1
                            }
                        } else {
                            (*project_idx + 1) % projects.len()
                        };
                        let project_id = &projects[*project_idx].0;
                        *models = db_path
                            .as_deref()
                            .map(|p| handoff::read_model_choices(p, project_id))
                            .unwrap_or_else(|| vec![ModelChoice::Unselected]);
                        *model_idx = 0;
                        *armed = false;
                    }
                }
                "up" | "down" | "k" | "j" => {
                    if !models.is_empty() {
                        *model_idx = if matches!(name, "up" | "k") {
                            if *model_idx == 0 {
                                models.len() - 1
                            } else {
                                *model_idx - 1
                            }
                        } else {
                            (*model_idx + 1) % models.len()
                        };
                        *armed = false;
                    }
                }
                "enter" => {
                    // Block confirmation whenever the terminal is too small to display the exact payload preview.
                    if !can_preview {
                        return HandoffKey::Consumed;
                    }
                    let chosen = models.get(*model_idx);
                    if chosen.is_none_or(ModelChoice::is_unselected) {
                        if models.len() <= 1 {
                            custom_error = Some(
                                "No local model found — start one thread in T3 or set a project default"
                                    .into(),
                            );
                        }
                    } else if *destructive && !*armed {
                        *armed = true;
                    } else {
                        should_launch = true;
                    }
                }
                _ => {}
            },
            Some(PendingAction::Send {
                presets,
                preset_idx,
                editing,
                draft,
                ..
            }) => {
                if *editing {
                    match name {
                        "escape" => {
                            *editing = false;
                        }
                        "backspace" => {
                            draft.pop();
                        }
                        "clear-line" => {
                            draft.clear();
                        }
                        "enter" => {
                            if !draft.trim().is_empty() {
                                should_launch = true;
                            }
                        }
                        _ => {}
                    }
                } else {
                    match name {
                        "q" => return HandoffKey::Quit,
                        "escape" => {
                            self.dismiss();
                            return HandoffKey::Consumed;
                        }
                        "left" => {
                            if !presets.is_empty() {
                                *preset_idx = if *preset_idx == 0 {
                                    presets.len() - 1
                                } else {
                                    *preset_idx - 1
                                };
                            }
                        }
                        "right" => {
                            if !presets.is_empty() {
                                *preset_idx = (*preset_idx + 1) % presets.len();
                            }
                        }
                        "tab" => {
                            *editing = true;
                        }
                        "enter" => {
                            let is_custom = presets
                                .get(*preset_idx)
                                .is_none_or(|p| p.text.is_none());
                            if is_custom {
                                *editing = true;
                            } else {
                                should_launch = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            None => {
                self.dismiss();
                return HandoffKey::Consumed;
            }
        }

        if let Some(err) = custom_error {
            self.pending = None;
            self.state = HandoffState::Error(err);
        } else if should_launch {
            self.launch_pending(host);
        } else if let Some(pending) = &self.pending {
            self.state = state_for_pending(pending);
        }
        HandoffKey::Consumed
    }

    pub(super) fn overlay(&self, canvas: &mut Canvas, p: &Palette) {
        if !self.state.is_idle() && canvas.h >= 3 {
            handoff::draw_handoff_bar(canvas, canvas.h as i32 - 2, &self.state, p);
        }
    }

    fn launch_pending(&mut self, host: &Host) {
        let Some(action) = self.pending.take() else {
            return;
        };
        let env_id = host.db.as_deref().and_then(integration::environment_id);
        let host_base_dir = host.db.as_deref().and_then(base_dir_from_db);
        let wake = self.wake.clone();
        let (tx, rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_worker = Arc::clone(&cancel);
        self.rx = Some(rx);
        self.cancel = Some(cancel);
        self.done_rx = Some(done_rx);

        match action {
            PendingAction::Start {
                title,
                prompt,
                projects,
                project_idx,
                models,
                model_idx,
                base_dir,
                ..
            } => {
                let (project_id, project_name) = projects
                    .get(project_idx)
                    .cloned()
                    .unwrap_or_else(|| (".".into(), "current folder".into()));
                let chosen_model = models
                    .get(model_idx)
                    .cloned()
                    .unwrap_or(ModelChoice::Unselected);
                if chosen_model.is_unselected() {
                    self.finish_worker();
                    self.state = HandoffState::Error(
                        "Pick a provider/model with ↑↓ before starting".into(),
                    );
                    return;
                }
                let dir = base_dir.or(host_base_dir);
                let model_tag = chosen_model.short_label();
                self.state = HandoffState::Running {
                    label: format!("Starting T3 thread in {project_name} ({model_tag})…"),
                };
                let _ = std::thread::Builder::new()
                    .name("loupe-t3-handoff".into())
                    .spawn(move || {
                        let outcome = run_t3_thread_start(
                            &project_id,
                            &project_name,
                            &title,
                            &prompt,
                            &chosen_model,
                            dir.as_deref(),
                            env_id.as_deref(),
                            &cancel_worker,
                        );
                        let _ = tx.send(outcome);
                        let _ = done_tx.send(());
                        if let Some(w) = wake {
                            w.waker().wake();
                        }
                    });
            }
            PendingAction::Send {
                thread_id,
                thread_title,
                presets,
                preset_idx,
                editing,
                draft,
            } => {
                let clean_draft: String = draft
                    .chars()
                    .filter(|&c| !c.is_control() && !terminal::is_hidden(c))
                    .collect();
                let message = if editing {
                    clean_draft.trim().to_owned()
                } else {
                    presets
                        .get(preset_idx)
                        .and_then(|p| p.text.clone())
                        .unwrap_or_else(|| clean_draft.trim().to_owned())
                };
                if message.is_empty() {
                    self.finish_worker();
                    self.state = HandoffState::Idle;
                    return;
                }
                self.state = HandoffState::Running {
                    label: format!("Sending to \"{thread_title}\"…"),
                };
                let _ = std::thread::Builder::new()
                    .name("loupe-t3-handoff".into())
                    .spawn(move || {
                        let outcome = run_t3_thread_send(
                            &thread_id,
                            &thread_title,
                            &message,
                            host_base_dir.as_deref(),
                            &cancel_worker,
                        );
                        let _ = tx.send(outcome);
                        let _ = done_tx.send(());
                        if let Some(w) = wake {
                            w.waker().wake();
                        }
                    });
            }
        }
    }
}

fn state_for_pending(pending: &PendingAction) -> HandoffState {
    match pending {
        PendingAction::Start {
            action_label,
            title,
            prompt,
            projects,
            project_idx,
            models,
            model_idx,
            destructive,
            armed,
            preview,
            preview_scroll,
            base_dir,
            ..
        } => {
            let (project_id, project_name) = projects
                .get(*project_idx)
                .cloned()
                .unwrap_or_else(|| (".".into(), "current folder".into()));
            let chosen = models.get(*model_idx);
            let model_label = chosen.map(ModelChoice::short_label).unwrap_or_default();
            let model_detail = chosen
                .map(ModelChoice::exact_preview_label)
                .unwrap_or_else(|| ModelChoice::Unselected.exact_preview_label());
            let needs_model_pick = chosen.is_none_or(ModelChoice::is_unselected);
            let base_dir_display = base_dir
                .as_deref()
                .and_then(|p| validate_base_dir(p).ok())
                .map(str::to_owned)
                .unwrap_or_else(|| "(default)".into());
            HandoffState::ConfirmStart {
                action_label: action_label.clone(),
                project_name,
                project_id,
                project_count: projects.len(),
                model_label,
                model_detail,
                model_count: models.len(),
                base_dir_display,
                needs_model_pick,
                destructive: *destructive,
                armed: *armed,
                preview: *preview,
                preview_scroll: *preview_scroll,
                title: title.clone(),
                prompt: prompt.clone(),
            }
        }
        PendingAction::Send {
            thread_title,
            presets,
            preset_idx,
            editing,
            draft,
            ..
        } => HandoffState::ComposeSend {
            thread_title: thread_title.clone(),
            preset_label: presets
                .get(*preset_idx)
                .map(|p| p.label.clone())
                .unwrap_or_default(),
            preset_idx: *preset_idx,
            preset_count: presets.len(),
            editing: *editing,
            draft: draft.clone(),
        },
    }
}

pub(super) fn collect_projects(
    state: &State,
    preferred_project_id: Option<&str>,
    scratch_project: Option<(String, String)>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push_unique = |id: &str, name: &str| {
        if id.is_empty()
            || validate_project_arg(id).is_err()
            || out.iter().any(|(existing, _)| existing == id)
        {
            return;
        }
        let label = if name.is_empty() { id } else { name };
        out.push((id.to_owned(), label.to_owned()));
    };

    fn agent_project_key(a: &Agent) -> Option<&str> {
        a.storage
            .as_ref()
            .map(|s| s.project_id.as_str())
            .filter(|s| !s.is_empty())
            .or_else(|| (!a.cwd.is_empty()).then_some(a.cwd.as_str()))
    }

    if let Some(pref_id) = preferred_project_id.filter(|s| !s.is_empty()) {
        if let Some(a) = state.snap.agents.iter().find(|a| {
            a.remote.is_none()
                && (agent_project_key(a) == Some(pref_id) || a.project == pref_id)
        }) {
            if let Some(key) = agent_project_key(a) {
                push_unique(key, &a.project);
            }
        } else {
            push_unique(pref_id, pref_id);
        }
        // Keep T3's "No project" scratch workspace one ←→ tap away even for repo-scoped handoffs.
        if let Some((sid, slabel)) = &scratch_project {
            push_unique(sid, slabel);
        }
    } else if let Some((sid, slabel)) = &scratch_project {
        // Machine-wide handoffs (e.g. Telemetry, global Insights) default to "No project".
        push_unique(sid, slabel);
    }

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

/// Validated executable with an open read-only descriptor held across `spawn()` (pinning its inode)
/// plus its exact `(dev, ino, uid, mode)` identity for immediate pre-exec revalidation.
struct ValidatedBinary {
    path: PathBuf,
    #[cfg(unix)]
    _guard: File,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    mode: u32,
}

/// Resolve `t3` only from trusted absolute PATH directories or `$HOME/.local/bin`,
/// verifying ownership (root or effective user) and non-group/world-writability on the
/// executable AND every ancestor directory component from `/` down.
fn find_t3_binary() -> Result<ValidatedBinary, String> {
    let path_env = std::env::var_os("PATH");
    let home_env = std::env::var_os("HOME");
    find_t3_binary_in(path_env.as_deref(), home_env.as_deref())
}

fn find_t3_binary_in(
    path_env: Option<&std::ffi::OsStr>,
    home_env: Option<&std::ffi::OsStr>,
) -> Result<ValidatedBinary, String> {
    if let Some(path_val) = path_env {
        for dir in std::env::split_paths(path_val) {
            if dir.as_os_str().is_empty() || !dir.is_absolute() {
                continue;
            }
            let cand = dir.join("t3");
            if let Some(valid) = validate_trusted_executable(&cand) {
                return Ok(valid);
            }
        }
    }
    if let Some(home) = home_env.filter(|h| !h.is_empty()) {
        let home_path = Path::new(home);
        if home_path.is_absolute() {
            let local = home_path.join(".local/bin/t3");
            if let Some(valid) = validate_trusted_executable(&local) {
                return Ok(valid);
            }
        }
    }
    Err("t3 CLI not found in trusted PATH directories".into())
}

/// Filter `PATH` down to strictly trusted, canonicalized directories whose lexical AND
/// canonical ancestry are both verified safe. Fails closed (`Err`) if no trusted directory
/// exists so the child process can NEVER inherit an untrusted `PATH`.
fn trusted_child_path() -> Result<OsString, String> {
    let path_env = std::env::var_os("PATH");
    trusted_child_path_from(path_env.as_deref())
}

fn trusted_child_path_from(path_env: Option<&std::ffi::OsStr>) -> Result<OsString, String> {
    let Some(path_val) = path_env else {
        return Err("no trusted directories in PATH for t3 CLI".into());
    };
    let mut safe_dirs: Vec<PathBuf> = Vec::new();
    for dir in std::env::split_paths(path_val) {
        if dir.as_os_str().is_empty() || !is_safe_dir_tree(&dir) {
            continue;
        }
        let Ok(canon) = fs::canonicalize(&dir) else {
            continue;
        };
        if !is_safe_dir_tree(&canon) {
            continue;
        }
        if !safe_dirs.contains(&canon) {
            safe_dirs.push(canon);
        }
    }
    if safe_dirs.is_empty() {
        return Err("no trusted directories in PATH for t3 CLI".into());
    }
    std::env::join_paths(safe_dirs)
        .map_err(|_| "failed to construct trusted PATH for t3 CLI".into())
}

fn validate_trusted_executable(candidate: &Path) -> Option<ValidatedBinary> {
    if !candidate.is_absolute() {
        return None;
    }
    let parent = candidate.parent()?;
    if !is_safe_dir_tree(parent) {
        return None;
    }
    let canon = fs::canonicalize(candidate).ok()?;
    let canon_parent = canon.parent()?;
    if !is_safe_dir_tree(canon_parent) {
        return None;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
        let euid = unsafe { libc::geteuid() };
        // Open without following symlinks and keep the descriptor open until after spawn().
        let guard = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&canon)
            .ok()?;
        let meta = guard.metadata().ok()?;
        if !meta.is_file() {
            return None;
        }
        let uid = meta.uid();
        let mode = meta.permissions().mode();
        if (uid != 0 && uid != euid) || (mode & 0o111) == 0 || (mode & 0o022) != 0 {
            return None;
        }
        Some(ValidatedBinary {
            path: canon,
            _guard: guard,
            dev: meta.dev(),
            ino: meta.ino(),
            uid,
            mode,
        })
    }
    #[cfg(not(unix))]
    {
        let meta = fs::metadata(&canon).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(ValidatedBinary { path: canon })
    }
}

/// Verify `dir` and EVERY ancestor directory up to `/` is a directory owned by root or the
/// effective user and is neither group-writable nor world-writable (`mode & 0o022 == 0`).
fn is_safe_dir_tree(dir: &Path) -> bool {
    if !dir.is_absolute() {
        return false;
    }
    #[cfg(unix)]
    let euid = unsafe { libc::geteuid() };

    for anc in dir.ancestors() {
        if anc.as_os_str().is_empty() {
            continue;
        }
        let Ok(meta) = fs::metadata(anc) else {
            return false;
        };
        if !meta.is_dir() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            let uid = meta.uid();
            let mode = meta.permissions().mode();
            if (uid != 0 && uid != euid) || (mode & 0o022) != 0 {
                return false;
            }
            // If this ancestor path itself is a symlink, verify the symlink's owner as well.
            if let Ok(lmeta) = fs::symlink_metadata(anc) {
                if lmeta.file_type().is_symlink() {
                    let luid = lmeta.uid();
                    if luid != 0 && luid != euid {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn has_gui_display() -> bool {
    has_gui_display_with(&|k| std::env::var_os(k))
}

fn has_gui_display_with(var: &dyn Fn(&str) -> Option<OsString>) -> bool {
    // Suppress automatic desktop handler launches over SSH (including X11 forwarding) on every OS.
    for ssh_key in ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"] {
        if var(ssh_key).is_some_and(|v| !v.is_empty()) {
            return false;
        }
    }
    if cfg!(target_vendor = "apple") {
        true
    } else {
        var("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty())
            || var("DISPLAY").is_some_and(|v| !v.is_empty())
    }
}

fn validate_project_arg(project_id: &str) -> Result<(), String> {
    if project_id.is_empty() || project_id.len() > 512 {
        return Err("invalid project identifier length".into());
    }
    if project_id.starts_with('-') {
        return Err("project identifier must not start with '-'".into());
    }
    if project_id
        .chars()
        .any(|c| c == '\0' || c.is_control() || terminal::is_hidden(c))
    {
        return Err("project identifier contains control or hidden characters".into());
    }
    Ok(())
}

fn validate_base_dir(dir: &Path) -> Result<&str, String> {
    if !dir.is_absolute() {
        return Err("base directory must be absolute".into());
    }
    let s = dir
        .to_str()
        .ok_or_else(|| "base directory path must be valid UTF-8".to_owned())?;
    if s.is_empty() || s.len() > 1024 {
        return Err("invalid base directory length".into());
    }
    if s.chars()
        .any(|c| c == '\0' || c.is_control() || terminal::is_hidden(c))
    {
        return Err("base directory contains control or hidden characters".into());
    }
    Ok(s)
}

fn validate_thread_id(thread_id: &str) -> Result<(), String> {
    if thread_id.is_empty() || thread_id.len() > 256 {
        return Err("invalid thread ID length".into());
    }
    if thread_id.starts_with('-') {
        return Err("thread ID must not start with '-'".into());
    }
    if !thread_id.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err("thread ID contains invalid characters".into());
    }
    Ok(())
}

fn validate_cli_token(kind: &str, value: &str) -> Result<(), String> {
    if handoff::is_valid_cli_token(value) {
        Ok(())
    } else {
        Err(format!("invalid {kind} value for t3 CLI"))
    }
}

struct BoundedOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn read_stream_capped<R: Read>(
    mut reader: R,
    cap: usize,
    overflow: Arc<AtomicBool>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.len().saturating_add(n) > cap {
                    let take = cap.saturating_sub(out.len());
                    out.extend_from_slice(&buf[..take]);
                    overflow.store(true, Ordering::Relaxed);
                    break;
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(_) => break,
        }
    }
    out
}

fn kill_and_reap_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        if pid > 0 {
            // Child was launched in its own dedicated session/process group via setsid();
            // signalling -pid terminates the direct child and every descendant in that group.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn run_bounded_command(
    mut cmd: Command,
    validated: Option<&ValidatedBinary>,
    stdin_bytes: Vec<u8>,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<BoundedOutput, String> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::process::CommandExt;

        let recheck = validated.and_then(|v| {
            let c_path = CString::new(v.path.as_os_str().as_bytes()).ok()?;
            Some((c_path, v.dev, v.ino, v.uid, v.mode))
        });
        // SAFETY: `pre_exec` uses only async-signal-safe syscalls (`setsid`, `geteuid`, `lstat`).
        unsafe {
            cmd.pre_exec(move || {
                // Start a dedicated session & process group so timeout/cancel kills descendants too.
                if libc::setsid() == -1 {
                    let _ = libc::setpgid(0, 0);
                }
                // Revalidate exact binary identity immediately before execve to close check-to-exec races.
                if let Some((ref c_path, expected_dev, expected_ino, expected_uid, expected_mode)) =
                    recheck
                {
                    let mut st: libc::stat = std::mem::zeroed();
                    if libc::lstat(c_path.as_ptr(), &mut st) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    let euid = libc::geteuid();
                    let is_reg = (st.st_mode & libc::S_IFMT) == libc::S_IFREG;
                    let owner_ok = st.st_uid == 0 || st.st_uid == euid;
                    let perms_ok = (st.st_mode & 0o111) != 0 && (st.st_mode & 0o022) == 0;
                    if !is_reg
                        || !owner_ok
                        || !perms_ok
                        || (st.st_dev as u64) != expected_dev
                        || (st.st_ino as u64) != expected_ino
                        || st.st_uid != expected_uid
                        || (st.st_mode as u32) != expected_mode
                    {
                        return Err(io::Error::from_raw_os_error(libc::EACCES));
                    }
                }
                Ok(())
            });
        }
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err("t3 CLI not found in trusted PATH directories".into());
        }
        Err(e) => return Err(format!("failed to run t3: {e}")),
    };

    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdin_pipe = child.stdin.take();

    let overflow = Arc::new(AtomicBool::new(false));
    let out_ov = Arc::clone(&overflow);
    let err_ov = Arc::clone(&overflow);

    let (out_tx, out_rx) = mpsc::channel();
    let (err_tx, err_rx) = mpsc::channel();
    let (in_tx, in_rx) = mpsc::channel();

    std::thread::spawn(move || {
        let data = stdout_pipe
            .map(|p| read_stream_capped(p, MAX_CLI_OUTPUT_BYTES, out_ov))
            .unwrap_or_default();
        let _ = out_tx.send(data);
    });
    std::thread::spawn(move || {
        let data = stderr_pipe
            .map(|p| read_stream_capped(p, MAX_CLI_OUTPUT_BYTES, err_ov))
            .unwrap_or_default();
        let _ = err_tx.send(data);
    });
    std::thread::spawn(move || {
        let res = if let Some(mut stdin) = stdin_pipe {
            stdin.write_all(&stdin_bytes).and_then(|()| stdin.flush())
        } else {
            Ok(())
        };
        let _ = in_tx.send(res);
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if cancel.load(Ordering::Relaxed) {
                    kill_and_reap_group(&mut child);
                    let _ = out_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    let _ = err_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    let _ = in_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    return Err("t3 CLI cancelled".into());
                }
                if overflow.load(Ordering::Relaxed) {
                    kill_and_reap_group(&mut child);
                    let _ = out_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    let _ = err_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    let _ = in_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    return Err("t3 CLI output exceeded 64 KiB limit".into());
                }
                if started.elapsed() >= timeout {
                    kill_and_reap_group(&mut child);
                    let _ = out_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    let _ = err_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    let _ = in_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                    return Err("t3 CLI timed out".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                kill_and_reap_group(&mut child);
                let _ = out_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                let _ = err_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                let _ = in_rx.recv_timeout(PIPE_JOIN_TIMEOUT);
                return Err(format!("t3 CLI failed: {e}"));
            }
        }
    };

    // Bounded pipe collection so even a detached descendant holding a pipe cannot hang Loupe.
    let stdout = out_rx.recv_timeout(PIPE_JOIN_TIMEOUT).unwrap_or_default();
    let stderr = err_rx.recv_timeout(PIPE_JOIN_TIMEOUT).unwrap_or_default();
    let stdin_res = in_rx.recv_timeout(PIPE_JOIN_TIMEOUT).unwrap_or(Ok(()));

    if overflow.load(Ordering::Relaxed) {
        return Err("t3 CLI output exceeded 64 KiB limit".into());
    }
    if status.success() {
        if let Err(e) = stdin_res {
            return Err(format!("failed to send prompt to t3 CLI: {e}"));
        }
    }

    Ok(BoundedOutput {
        status,
        stdout,
        stderr,
    })
}

#[allow(clippy::too_many_arguments)]
fn run_t3_thread_start(
    project_id: &str,
    project_name: &str,
    title: &str,
    prompt: &str,
    chosen_model: &ModelChoice,
    base_dir: Option<&Path>,
    env_id: Option<&str>,
    cancel: &AtomicBool,
) -> WorkerOutcome {
    let explicit = match chosen_model {
        ModelChoice::Unselected => {
            return WorkerOutcome::Error("Pick a provider/model with ↑↓ before starting".into());
        }
        ModelChoice::Explicit {
            provider, model, ..
        } => Some((provider.as_str(), model.as_str())),
        ModelChoice::ProjectDefault { .. } => None,
    };

    let output = match invoke_t3_thread_start(project_id, title, prompt, explicit, base_dir, cancel)
    {
        Ok(out) => out,
        Err(e) => return WorkerOutcome::Error(e),
    };

    if !output.status.success() {
        let err_line = first_error_line(&output.stderr, &output.stdout, "t3 thread start failed");
        if explicit.is_none() && err_line.to_ascii_lowercase().contains("no default model") {
            return WorkerOutcome::Error(format!(
                "No default model for {project_name} — press ↑↓ to choose a provider/model"
            ));
        }
        return WorkerOutcome::Error(err_line);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed_id = handoff::parse_t3_thread_id(&stdout);

    let mut opened_app = false;
    if has_gui_display() {
        if let (Some(env), Some(tid)) = (env_id, &parsed_id) {
            if let Some(link) = integration::thread_link(env, tid) {
                opened_app = integration::open_link(&link).is_ok();
            }
        }
    }

    let short_id = match parsed_id {
        Some(ref id) if id.chars().count() > 14 => {
            let prefix: String = id.chars().take(12).collect();
            format!("{prefix}…")
        }
        Some(id) => id,
        None => "started".into(),
    };
    let tail = if opened_app {
        "opened in T3 Code"
    } else {
        "open in T3 Code"
    };
    let model_suffix = format!(" ({})", chosen_model.short_label());
    WorkerOutcome::Done(format!(
        "Started in {project_name}{model_suffix} · {short_id} · {tail}"
    ))
}

fn invoke_t3_thread_start(
    project_id: &str,
    title: &str,
    prompt: &str,
    explicit_model: Option<(&str, &str)>,
    base_dir: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<BoundedOutput, String> {
    validate_project_arg(project_id)?;
    let safe_title = handoff::sanitize_title(title);
    if let Some((provider, model)) = explicit_model {
        validate_cli_token("provider", provider)?;
        validate_cli_token("model", model)?;
    }

    let validated = find_t3_binary()?;
    let safe_path = trusted_child_path()?;
    let mut cmd = Command::new(&validated.path);
    cmd.env("PATH", safe_path);
    cmd.arg("thread")
        .arg("start")
        .arg(format!("--title={safe_title}"))
        .arg("--json");
    if let Some(dir) = base_dir {
        let dir_str = validate_base_dir(dir)?;
        cmd.arg(format!("--base-dir={dir_str}"));
    }
    if let Some((provider, model)) = explicit_model {
        cmd.arg(format!("--provider={provider}"))
            .arg(format!("--model={model}"));
    }
    cmd.arg("--").arg(project_id).arg("-");

    run_bounded_command(
        cmd,
        Some(&validated),
        prompt.as_bytes().to_vec(),
        CLI_TIMEOUT,
        cancel,
    )
}

fn run_t3_thread_send(
    thread_id: &str,
    thread_title: &str,
    message: &str,
    base_dir: Option<&Path>,
    cancel: &AtomicBool,
) -> WorkerOutcome {
    if let Err(e) = validate_thread_id(thread_id) {
        return WorkerOutcome::Error(e);
    }
    let validated = match find_t3_binary() {
        Ok(b) => b,
        Err(e) => return WorkerOutcome::Error(e),
    };
    let safe_path = match trusted_child_path() {
        Ok(p) => p,
        Err(e) => return WorkerOutcome::Error(e),
    };
    let mut cmd = Command::new(&validated.path);
    cmd.env("PATH", safe_path);
    cmd.arg("thread").arg("send").arg("--json");
    if let Some(dir) = base_dir {
        match validate_base_dir(dir) {
            Ok(dir_str) => {
                cmd.arg(format!("--base-dir={dir_str}"));
            }
            Err(e) => return WorkerOutcome::Error(e),
        }
    }
    cmd.arg("--").arg(thread_id).arg("-");

    let output = match run_bounded_command(
        cmd,
        Some(&validated),
        message.as_bytes().to_vec(),
        CLI_TIMEOUT,
        cancel,
    ) {
        Ok(o) => o,
        Err(e) => return WorkerOutcome::Error(e),
    };

    if !output.status.success() {
        return WorkerOutcome::Error(first_error_line(
            &output.stderr,
            &output.stdout,
            "t3 thread send failed",
        ));
    }

    let short: String = thread_title.chars().take(32).collect();
    WorkerOutcome::Done(format!("Sent to \"{short}\""))
}

fn first_error_line(stderr: &[u8], stdout: &[u8], fallback: &str) -> String {
    let err = String::from_utf8_lossy(stderr);
    let out = String::from_utf8_lossy(stdout);
    let msg = err
        .lines()
        .chain(out.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(fallback);
    msg.chars().take(90).collect()
}

/// Derive T3 Code's `--base-dir` (equivalent to `T3CODE_HOME`) from the path to `statev2.sqlite`.
/// `statev2.sqlite` lives under `<base-dir>/userdata/statev2.sqlite`.
fn base_dir_from_db(db_path: &Path) -> Option<PathBuf> {
    let parent = db_path.parent()?;
    if parent.file_name().and_then(|n| n.to_str()) == Some("userdata") {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_dir_strips_userdata_component() {
        assert_eq!(
            base_dir_from_db(Path::new("/home/alice/.t3/userdata/statev2.sqlite")),
            Some(PathBuf::from("/home/alice/.t3"))
        );
        assert_eq!(
            base_dir_from_db(Path::new("/custom/t3home/userdata/statev2.sqlite")),
            Some(PathBuf::from("/custom/t3home"))
        );
        assert_eq!(
            base_dir_from_db(Path::new("/tmp/flat/statev2.sqlite")),
            Some(PathBuf::from("/tmp/flat"))
        );
    }

    #[test]
    fn cli_validators_reject_option_injection_and_control_chars() {
        assert!(validate_project_arg("proj_123").is_ok());
        assert!(validate_project_arg("/home/alice/repo").is_ok());
        assert!(validate_project_arg(".").is_ok());
        assert!(validate_project_arg("--base-dir=/evil").is_err());
        assert!(validate_project_arg("-p").is_err());
        assert!(validate_project_arg("bad\0id").is_err());
        assert!(validate_project_arg("bad\nid").is_err());
        assert!(validate_project_arg("bad\u{202E}id").is_err());

        assert!(validate_base_dir(Path::new("/home/alice/.t3")).is_ok());
        assert!(validate_base_dir(Path::new("relative/dir")).is_err());
        assert!(validate_base_dir(Path::new("/home/alice/\u{202E}t3")).is_err());
        assert!(validate_base_dir(Path::new("/home/alice/\nt3")).is_err());

        assert!(validate_thread_id("mcp:7832a1ed-7053-47c3-b183-a3ef46cccc85").is_ok());
        assert!(validate_thread_id("--base-dir=/evil").is_err());
        assert!(validate_thread_id("has space").is_err());
        assert!(validate_thread_id("bad\nid").is_err());

        assert!(validate_cli_token("provider", "codex").is_ok());
        assert!(validate_cli_token("model", "gpt-5.4").is_ok());
        assert!(validate_cli_token("provider", "--evil").is_err());
        assert!(validate_cli_token("model", "-x").is_err());

        assert_eq!(handoff::sanitize_title("--evil\n\u{202E}title"), "evil title");
        assert_eq!(handoff::sanitize_title("---"), "Loupe handoff");
    }

    #[test]
    fn ssh_env_suppresses_gui_auto_open_on_all_platforms() {
        let with_x11_ssh = |k: &str| match k {
            "DISPLAY" => Some(OsString::from(":10.0")),
            "SSH_CONNECTION" => Some(OsString::from("10.0.0.1 12345 10.0.0.2 22")),
            _ => None,
        };
        assert!(!has_gui_display_with(&with_x11_ssh));

        let with_ssh_tty = |k: &str| match k {
            "WAYLAND_DISPLAY" => Some(OsString::from("wayland-0")),
            "SSH_TTY" => Some(OsString::from("/dev/pts/1")),
            _ => None,
        };
        assert!(!has_gui_display_with(&with_ssh_tty));

        let local_wayland = |k: &str| match k {
            "WAYLAND_DISPLAY" => Some(OsString::from("wayland-0")),
            _ => None,
        };
        assert!(has_gui_display_with(&local_wayland));
    }

    #[cfg(unix)]
    #[test]
    fn find_t3_binary_rejects_relative_and_writable_ancestors_and_accepts_trusted_symlink() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        // Place test tree under Cargo's target dir so every real ancestor from `/` down is
        // root- or euid-owned and non-writable (unlike `/tmp` which is 01777 and MUST be rejected).
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(format!("loupe-bin-sec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let safe_dir = base.join("safe");
        let real_dir = base.join("real");
        let writable_ancestor = base.join("writable-parent");
        let nested_under_writable = writable_ancestor.join("0755-child");
        fs::create_dir_all(&safe_dir).unwrap();
        fs::create_dir_all(&real_dir).unwrap();
        fs::create_dir_all(&nested_under_writable).unwrap();

        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&safe_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&real_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(&writable_ancestor, fs::Permissions::from_mode(0o777)).unwrap();
        fs::set_permissions(&nested_under_writable, fs::Permissions::from_mode(0o755)).unwrap();

        let bad_bin = nested_under_writable.join("t3");
        fs::write(&bad_bin, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&bad_bin, fs::Permissions::from_mode(0o755)).unwrap();

        let good_target = real_dir.join("t3-real");
        fs::write(&good_target, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&good_target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&good_target, safe_dir.join("t3")).unwrap();

        // A 0755 dir beneath a 0777 ancestor MUST be rejected, as must `/tmp` and relative `.`.
        let bad_path = std::env::join_paths([
            Path::new("."),
            Path::new(""),
            nested_under_writable.as_path(),
        ])
        .unwrap();
        assert!(find_t3_binary_in(Some(bad_path.as_os_str()), None).is_err());

        // Trusted symlink with safe ancestors all the way to `/` is accepted and canonicalized.
        let good_path = std::env::join_paths([
            nested_under_writable.as_path(),
            safe_dir.as_path(),
        ])
        .unwrap();
        let resolved = find_t3_binary_in(Some(good_path.as_os_str()), None).unwrap();
        assert_eq!(resolved.path, fs::canonicalize(&good_target).unwrap());

        // trusted_child_path_from MUST fail closed when only untrusted/relative/symlink-to-writable dirs exist.
        let symlink_to_writable = base.join("link-to-bad");
        symlink(&nested_under_writable, &symlink_to_writable).unwrap();
        let all_bad = std::env::join_paths([
            Path::new("."),
            nested_under_writable.as_path(),
            symlink_to_writable.as_path(),
        ])
        .unwrap();
        assert!(trusted_child_path_from(Some(all_bad.as_os_str())).is_err());
        assert!(trusted_child_path_from(None).is_err());

        // Valid symlink dir under safe ancestors resolves to canonical safe target in child PATH.
        let symlink_to_safe = base.join("link-to-safe");
        symlink(&real_dir, &symlink_to_safe).unwrap();
        let mixed = std::env::join_paths([
            symlink_to_writable.as_path(),
            symlink_to_safe.as_path(),
        ])
        .unwrap();
        let filtered = trusted_child_path_from(Some(mixed.as_os_str())).unwrap();
        let kept: Vec<PathBuf> = std::env::split_paths(&filtered).collect();
        assert_eq!(kept, vec![fs::canonicalize(&real_dir).unwrap()]);

        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn run_bounded_command_kills_descendants_promptly_and_enforces_output_cap() {
        let cancel = AtomicBool::new(false);

        // 1. Timeout kills the entire process group (including a background descendant holding
        //    stdout/stderr pipes) and returns promptly instead of blocking for 10s.
        let mut sleep_cmd = Command::new("/bin/sh");
        sleep_cmd.arg("-c").arg("sleep 10 & sleep 10; wait");
        let t0 = Instant::now();
        let err = run_bounded_command(
            sleep_cmd,
            None,
            Vec::new(),
            Duration::from_millis(120),
            &cancel,
        )
        .err()
        .expect("expected timeout error");
        let elapsed = t0.elapsed();
        assert!(err.contains("timed out"), "unexpected error: {err}");
        assert!(
            elapsed < Duration::from_secs(2),
            "timeout hung waiting on descendant pipes: elapsed={elapsed:?}"
        );

        // 2. Output cap kills a noisy child before unbounded memory growth.
        let mut noisy_cmd = Command::new("/bin/sh");
        noisy_cmd
            .arg("-c")
            .arg("dd if=/dev/zero bs=4096 count=64 2>/dev/null");
        let err = run_bounded_command(
            noisy_cmd,
            None,
            Vec::new(),
            Duration::from_secs(5),
            &cancel,
        )
        .err()
        .expect("expected output cap error");
        assert!(err.contains("64 KiB"), "unexpected error: {err}");
    }

    #[test]
    fn compose_char_rejects_hidden_unicode_bidi_controls() {
        let mut h = T3Handoff::new(HandoffSource::Machine, None);
        let mut agent = Agent::default();
        agent.thread_id = "t-1".into();
        agent.title = "demo".into();
        h.begin_send(&agent, None);
        let host = Host::demo(0.0);
        h.key("tab", &host, (80, 24));
        h.char('a');
        h.char('\u{202E}'); // RLO BiDi override
        h.char('\u{200B}'); // ZWSP
        h.char('b');
        match h.state() {
            HandoffState::ComposeSend { draft, .. } => assert_eq!(draft, "ab"),
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[test]
    fn confirm_start_blocks_enter_when_terminal_too_small_for_preview() {
        let mut h = T3Handoff::new(HandoffSource::Machine, None);
        let host = Host::demo(0.0);
        let state = State::new(loupe_core::model::Snapshot::default());
        h.begin_start(
            "diagnose trace",
            "--leading-dash\ntitle".into(),
            "exact prompt body".into(),
            Some("proj-1"),
            None,
            None,
            false,
            &state,
        );
        // Stored/previewed title must already match the exact sanitized CLI title.
        match h.state() {
            HandoffState::ConfirmStart { title, .. } => {
                assert_eq!(title, "leading-dash title");
            }
            other => panic!("unexpected state: {other:?}"),
        }
        // Pressing Enter on a tiny terminal must NOT launch or error out; it stays blocked.
        h.key("enter", &host, (20, 5));
        assert!(matches!(h.state(), HandoffState::ConfirmStart { .. }));
    }
}
