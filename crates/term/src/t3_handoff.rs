//! Unified async T3 Code handoff controller (`t3 thread start` and `t3 thread send`).
//!
//! Shared across Telemetry, Storage, Trace, Insights, and Live so every screen uses the same
//! confirmation flow, project picker, quick-reply composer, SSH-safe launch behavior, and
//! `--demo` / `--db` safety guards.

use crate::platform::Wake;
use loupe_core::{
    handoff::{self, HandoffState, SendPreset},
    host::Host,
    integration,
    model::{Agent, Pace, State},
    terminal::{Canvas, Palette},
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, mpsc},
};

const MAX_DRAFT_CHARS: usize = 400;

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
        title: String,
        prompt: String,
        projects: Vec<(String, String)>,
        project_idx: usize,
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
}

impl T3Handoff {
    pub(super) fn new(source: HandoffSource, wake: Option<Arc<Wake>>) -> Self {
        Self {
            source,
            wake,
            state: HandoffState::Idle,
            pending: None,
            rx: None,
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
    pub(super) fn begin_start(
        &mut self,
        action_label: impl Into<String>,
        title: String,
        prompt: String,
        preferred_project_id: Option<&str>,
        base_dir: Option<PathBuf>,
        dashboard: &State,
    ) {
        if self.state.is_running() {
            return;
        }
        let action_label = action_label.into();
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
                let projects = collect_projects(dashboard, preferred_project_id);
                let project_name = projects[0].1.clone();
                let project_count = projects.len();
                self.state = HandoffState::ConfirmStart {
                    action_label: action_label.clone(),
                    project_name,
                    project_count,
                };
                self.pending = Some(PendingAction::Start {
                    action_label,
                    title,
                    prompt,
                    projects,
                    project_idx: 0,
                    base_dir,
                });
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
                let preset_label = presets[0].label.clone();
                let preset_count = presets.len();
                self.state = HandoffState::ComposeSend {
                    thread_title: agent.title.clone(),
                    preset_label,
                    preset_idx: 0,
                    preset_count,
                    editing: false,
                    draft: String::new(),
                };
                self.pending = Some(PendingAction::Send {
                    thread_id: agent.thread_id.clone(),
                    thread_title: agent.title.clone(),
                    presets,
                    preset_idx: 0,
                    editing: false,
                    draft: String::new(),
                });
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
                self.rx = None;
                self.state = HandoffState::Done { message: msg };
                true
            }
            Ok(WorkerOutcome::Error(err)) => {
                self.rx = None;
                self.state = HandoffState::Error(err);
                true
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.rx = None;
                if self.state.is_running() {
                    self.state = HandoffState::Error("T3 handoff worker stopped unexpectedly".into());
                }
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
        }
    }

    /// Handle a typed character while in free-text compose mode.
    pub(super) fn char(&mut self, ch: char) -> bool {
        let Some(PendingAction::Send {
            editing: true,
            draft,
            thread_title,
            presets,
            preset_idx,
            ..
        }) = &mut self.pending
        else {
            return false;
        };
        if draft.chars().count() < MAX_DRAFT_CHARS {
            draft.push(ch);
            self.state = HandoffState::ComposeSend {
                thread_title: thread_title.clone(),
                preset_label: presets[*preset_idx].label.clone(),
                preset_idx: *preset_idx,
                preset_count: presets.len(),
                editing: true,
                draft: draft.clone(),
            };
        }
        true
    }

    /// Route a key event to the handoff bar when active.
    pub(super) fn key(&mut self, name: &str, host: &Host) -> HandoffKey {
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

        match &mut self.pending {
            Some(PendingAction::Start {
                action_label,
                projects,
                project_idx,
                ..
            }) => match name {
                "q" => return HandoffKey::Quit,
                "escape" => {
                    self.dismiss();
                    return HandoffKey::Consumed;
                }
                "left" => {
                    if !projects.is_empty() {
                        *project_idx = if *project_idx == 0 {
                            projects.len() - 1
                        } else {
                            *project_idx - 1
                        };
                        self.state = HandoffState::ConfirmStart {
                            action_label: action_label.clone(),
                            project_name: projects[*project_idx].1.clone(),
                            project_count: projects.len(),
                        };
                    }
                    return HandoffKey::Consumed;
                }
                "right" | "tab" => {
                    if !projects.is_empty() {
                        *project_idx = (*project_idx + 1) % projects.len();
                        self.state = HandoffState::ConfirmStart {
                            action_label: action_label.clone(),
                            project_name: projects[*project_idx].1.clone(),
                            project_count: projects.len(),
                        };
                    }
                    return HandoffKey::Consumed;
                }
                "enter" => {
                    self.launch_pending(host);
                    return HandoffKey::Consumed;
                }
                _ => return HandoffKey::Consumed,
            },
            Some(PendingAction::Send {
                thread_title,
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
                            self.state = HandoffState::ComposeSend {
                                thread_title: thread_title.clone(),
                                preset_label: presets[*preset_idx].label.clone(),
                                preset_idx: *preset_idx,
                                preset_count: presets.len(),
                                editing: false,
                                draft: draft.clone(),
                            };
                            return HandoffKey::Consumed;
                        }
                        "backspace" => {
                            draft.pop();
                            self.state = HandoffState::ComposeSend {
                                thread_title: thread_title.clone(),
                                preset_label: presets[*preset_idx].label.clone(),
                                preset_idx: *preset_idx,
                                preset_count: presets.len(),
                                editing: true,
                                draft: draft.clone(),
                            };
                            return HandoffKey::Consumed;
                        }
                        "clear-line" => {
                            draft.clear();
                            self.state = HandoffState::ComposeSend {
                                thread_title: thread_title.clone(),
                                preset_label: presets[*preset_idx].label.clone(),
                                preset_idx: *preset_idx,
                                preset_count: presets.len(),
                                editing: true,
                                draft: String::new(),
                            };
                            return HandoffKey::Consumed;
                        }
                        "enter" => {
                            if !draft.trim().is_empty() {
                                self.launch_pending(host);
                            }
                            return HandoffKey::Consumed;
                        }
                        _ => return HandoffKey::Consumed,
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
                                self.state = HandoffState::ComposeSend {
                                    thread_title: thread_title.clone(),
                                    preset_label: presets[*preset_idx].label.clone(),
                                    preset_idx: *preset_idx,
                                    preset_count: presets.len(),
                                    editing: false,
                                    draft: draft.clone(),
                                };
                            }
                            return HandoffKey::Consumed;
                        }
                        "right" => {
                            if !presets.is_empty() {
                                *preset_idx = (*preset_idx + 1) % presets.len();
                                self.state = HandoffState::ComposeSend {
                                    thread_title: thread_title.clone(),
                                    preset_label: presets[*preset_idx].label.clone(),
                                    preset_idx: *preset_idx,
                                    preset_count: presets.len(),
                                    editing: false,
                                    draft: draft.clone(),
                                };
                            }
                            return HandoffKey::Consumed;
                        }
                        "tab" => {
                            *editing = true;
                            self.state = HandoffState::ComposeSend {
                                thread_title: thread_title.clone(),
                                preset_label: presets[*preset_idx].label.clone(),
                                preset_idx: *preset_idx,
                                preset_count: presets.len(),
                                editing: true,
                                draft: draft.clone(),
                            };
                            return HandoffKey::Consumed;
                        }
                        "enter" => {
                            let is_custom = presets
                                .get(*preset_idx)
                                .is_none_or(|p| p.text.is_none());
                            if is_custom {
                                *editing = true;
                                self.state = HandoffState::ComposeSend {
                                    thread_title: thread_title.clone(),
                                    preset_label: presets[*preset_idx].label.clone(),
                                    preset_idx: *preset_idx,
                                    preset_count: presets.len(),
                                    editing: true,
                                    draft: draft.clone(),
                                };
                            } else {
                                self.launch_pending(host);
                            }
                            return HandoffKey::Consumed;
                        }
                        _ => return HandoffKey::Consumed,
                    }
                }
            }
            None => {
                self.dismiss();
                HandoffKey::Consumed
            }
        }
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
        self.rx = Some(rx);

        match action {
            PendingAction::Start {
                title,
                prompt,
                projects,
                project_idx,
                base_dir,
                ..
            } => {
                let (project_id, project_name) = projects
                    .get(project_idx)
                    .cloned()
                    .unwrap_or_else(|| (".".into(), "current folder".into()));
                let dir = base_dir.or(host_base_dir);
                self.state = HandoffState::Running {
                    label: format!("Starting T3 thread in {project_name}…"),
                };
                let _ = std::thread::Builder::new()
                    .name("loupe-t3-handoff".into())
                    .spawn(move || {
                        let outcome = run_t3_thread_start(
                            &project_id,
                            &project_name,
                            &title,
                            &prompt,
                            dir.as_deref(),
                            env_id.as_deref(),
                        );
                        let _ = tx.send(outcome);
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
                let message = if editing {
                    draft.trim().to_owned()
                } else {
                    presets
                        .get(preset_idx)
                        .and_then(|p| p.text.clone())
                        .unwrap_or_else(|| draft.trim().to_owned())
                };
                if message.is_empty() {
                    self.rx = None;
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
                        );
                        let _ = tx.send(outcome);
                        if let Some(w) = wake {
                            w.waker().wake();
                        }
                    });
            }
        }
    }
}

pub(super) fn collect_projects(
    state: &State,
    preferred_project_id: Option<&str>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push_unique = |id: &str, name: &str| {
        if id.is_empty() || out.iter().any(|(existing, _)| existing == id) {
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

    // 1. Explicit preferred project (e.g., selected Storage project or traced thread's project).
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
    }

    // 2. Selected or currently focused T3 thread's project.
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

    // 3. All other local projects in snapshot order.
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
    base_dir: Option<&Path>,
    env_id: Option<&str>,
) -> WorkerOutcome {
    let bin = find_t3_binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("thread")
        .arg("start")
        .arg("--title")
        .arg(title)
        .arg("--json");
    if let Some(dir) = base_dir.filter(|p| p.is_absolute()) {
        cmd.arg("--base-dir").arg(dir);
    }
    cmd.arg(project_id)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return WorkerOutcome::Error("t3 CLI not found on PATH".into());
        }
        Err(e) => {
            return WorkerOutcome::Error(format!("failed to run t3: {e}"));
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return WorkerOutcome::Error(format!("t3 CLI failed: {e}")),
    };

    if !output.status.success() {
        return WorkerOutcome::Error(first_error_line(&output.stderr, &output.stdout, "t3 thread start failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let thread_id = handoff::parse_t3_thread_id(&stdout).unwrap_or_else(|| "started".into());

    let mut opened_app = false;
    if has_gui_display() {
        if let Some(env) = env_id {
            if let Some(link) = integration::thread_link(env, &thread_id) {
                opened_app = integration::open_link(&link).is_ok();
            }
        }
    }

    let short_id = if thread_id.len() > 14 {
        format!("{}…", &thread_id[..12])
    } else {
        thread_id
    };
    let tail = if opened_app {
        "opened in T3 Code"
    } else {
        "open in T3 Code"
    };
    WorkerOutcome::Done(format!("Started in {project_name} · {short_id} · {tail}"))
}

fn run_t3_thread_send(
    thread_id: &str,
    thread_title: &str,
    message: &str,
    base_dir: Option<&Path>,
) -> WorkerOutcome {
    let bin = find_t3_binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("thread").arg("send").arg("--json");
    if let Some(dir) = base_dir.filter(|p| p.is_absolute()) {
        cmd.arg("--base-dir").arg(dir);
    }
    cmd.arg(thread_id)
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return WorkerOutcome::Error("t3 CLI not found on PATH".into());
        }
        Err(e) => {
            return WorkerOutcome::Error(format!("failed to run t3: {e}"));
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(message.as_bytes());
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return WorkerOutcome::Error(format!("t3 CLI failed: {e}")),
    };

    if !output.status.success() {
        return WorkerOutcome::Error(first_error_line(&output.stderr, &output.stdout, "t3 thread send failed"));
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
}
