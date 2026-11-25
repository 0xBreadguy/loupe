//! Shared T3 Code handoff prompts, presets, and action-bar rendering.
//!
//! Loupe itself stays strictly read-only. Every handoff in this module builds a non-secret,
//! structured prompt or quick-reply message from data already shown on screen, and only runs
//! `t3 thread start` or `t3 thread send` in the terminal layer after explicit user confirmation.

use crate::model::{self, Agent, Pace};
use crate::recap::Recap;
use crate::storage_view::data::{Bytes, Entry, Inventory, Sizes, ThreadState};
use crate::telemetry;
use crate::terminal::{self, Canvas, Palette};
use crate::tools::{self, Kind as ToolKind, Tools};
use crate::trace::{self, Kind as TraceKind, Trace};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One selectable model option for `t3 thread start`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelChoice {
    /// Let T3 Code use the project's configured default model (no `--provider`/`--model` flags).
    ProjectDefault { model_hint: Option<String> },
    /// Inherit a specific `(providerInstanceId, model)` from recent local T3 runs/threads.
    Explicit { provider: String, model: String },
}

impl ModelChoice {
    pub fn short_label(&self) -> String {
        match self {
            Self::ProjectDefault {
                model_hint: Some(model),
            } => {
                format!("default ({})", terminal::short_model(model))
            }
            Self::ProjectDefault { model_hint: None } => "project default".into(),
            Self::Explicit { model, .. } => terminal::short_model(model),
        }
    }
}

/// Interactive T3 Code handoff bar state shared across Telemetry, Storage, Trace, Insights, and Live.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum HandoffState {
    #[default]
    Idle,
    /// Confirm starting a new T3 thread (`t3 thread start`).
    ConfirmStart {
        action_label: String,
        project_name: String,
        project_count: usize,
        model_label: String,
        model_count: usize,
    },
    /// Confirm or compose a message to an existing local T3 thread (`t3 thread send`).
    ComposeSend {
        thread_title: String,
        preset_label: String,
        preset_idx: usize,
        preset_count: usize,
        editing: bool,
        draft: String,
    },
    /// Background `t3` CLI call in flight.
    Running { label: String },
    /// Succeeded (`t3 thread start` or `t3 thread send`).
    Done { message: String },
    /// Demo mode preview (no `t3` CLI invoked).
    DemoPreview(String),
    /// Error from invocation or validation.
    Error(String),
}

impl HandoffState {
    pub fn is_idle(&self) -> bool {
        matches!(self, Self::Idle)
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running { .. })
    }
}

/// One quick-reply preset for `t3 thread send` on the Live dashboard.
/// `text: None` marks the interactive custom-reply slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendPreset {
    pub label: String,
    pub text: Option<String>,
}

/// Selected storage context passed from `StorageScreen` to `storage_prompt`.
#[derive(Clone, Debug, Default)]
pub struct StorageContext {
    pub target: StorageTarget,
    pub volumes: Vec<StorageVolume>,
    pub settled_or_gone_count: usize,
    pub settled_or_gone_bytes: u64,
    pub top_items: Vec<StorageItem>,
}

#[derive(Clone, Debug, Default)]
pub enum StorageTarget {
    #[default]
    Overview,
    Project {
        id: String,
        title: String,
        thread_count: usize,
        sizes: Sizes,
    },
    SettledOrGone {
        count: usize,
        sizes: Sizes,
    },
    Entry {
        title: String,
        path: Option<PathBuf>,
        state: ThreadState,
        sizes: Sizes,
    },
    Other {
        sizes: Sizes,
        temp_label: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub struct StorageVolume {
    pub label: String,
    pub available_bytes: u64,
    pub total_bytes: u64,
    pub is_tmpfs: Option<bool>,
}

#[derive(Clone, Debug)]
pub struct StorageItem {
    pub title: String,
    pub path: Option<PathBuf>,
    pub state: ThreadState,
    pub worktree_bytes: Option<u64>,
    pub build_bytes: Option<u64>,
    pub scratch_bytes: Option<u64>,
}

/// Parse a `threadId` from `t3 thread start --json` output.
pub fn parse_t3_thread_id(stdout: &str) -> Option<String> {
    telemetry::parse_t3_thread_id(stdout)
}

/// Resolve ordered model choices for `t3 thread start` on `project_id` from T3's local SQLite
/// database, strictly read-only (`PRAGMA query_only=ON`).
///
/// Priority order:
/// 1. The project's own configured `default_model_selection_json` (if set in `projection_projects`)
/// 2. Most recent `(providerInstanceId, model)` used in runs/threads of that same project
/// 3. Most recent `(providerInstanceId, model)` used in any local T3 project
///
/// Never opens secrets, auth tables, prompts, or message text.
pub fn read_model_choices(db_path: &Path, project_id: &str) -> Vec<ModelChoice> {
    let Ok(db) = crate::source::open_read_only(db_path) else {
        return vec![ModelChoice::ProjectDefault { model_hint: None }];
    };
    let _ = db.busy_timeout(Duration::from_millis(250));
    if db.execute_batch("PRAGMA query_only=ON").is_err() {
        return vec![ModelChoice::ProjectDefault { model_hint: None }];
    }
    read_model_choices_from(&db, project_id)
}

fn read_model_choices_from(db: &Connection, project_id: &str) -> Vec<ModelChoice> {
    let mut choices: Vec<ModelChoice> = Vec::new();
    let mut seen_explicit: Vec<(String, String)> = Vec::new();

    // 1. Check whether the project has an explicit default model in T3.
    let mut resolved_project_id = project_id.to_owned();
    let project_default: Option<(String, Option<String>)> = db
        .query_row(
            "SELECT project_id, json_extract(default_model_selection_json, '$.model')
             FROM projection_projects
             WHERE (project_id = ?1 OR workspace_root = ?1) AND deleted_at IS NULL
             LIMIT 1",
            params![project_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .ok();
    if let Some((pid, model_hint)) = project_default {
        resolved_project_id = pid;
        if let Some(model) = model_hint.filter(|m| !m.trim().is_empty()) {
            choices.push(ModelChoice::ProjectDefault {
                model_hint: Some(model),
            });
        }
    }

    let mut push_explicit = |provider: Option<String>, model: Option<String>| {
        if seen_explicit.len() >= 10 {
            return;
        }
        let Some(p) = provider.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()) else {
            return;
        };
        let Some(m) = model
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty() && s != "--")
        else {
            return;
        };
        if seen_explicit.iter().any(|(ep, em)| ep == &p && em == &m) {
            return;
        }
        seen_explicit.push((p.clone(), m.clone()));
        choices.push(ModelChoice::Explicit {
            provider: p,
            model: m,
        });
    };

    // 2. Most recent runs in the target project.
    if let Ok(mut stmt) = db.prepare(
        "SELECT json_extract(r.payload_json, '$.providerInstanceId'),
                json_extract(r.payload_json, '$.modelSelection.model')
         FROM orchestration_v2_projection_runs r
         JOIN orchestration_v2_projection_threads t ON t.thread_id = r.thread_id
         WHERE t.project_id = ?1 AND t.deleted_at IS NULL
         ORDER BY r.requested_at DESC, r.rowid DESC
         LIMIT 30",
    ) {
        if let Ok(mut rows) = stmt.query(params![resolved_project_id]) {
            while let Ok(Some(row)) = rows.next() {
                push_explicit(row.get(0).ok().flatten(), row.get(1).ok().flatten());
            }
        }
    }

    // 2b. Most recent threads in the target project (covers configured threads without runs yet).
    if let Ok(mut stmt) = db.prepare(
        "SELECT COALESCE(json_extract(payload_json, '$.providerInstanceId'), default_provider),
                json_extract(payload_json, '$.modelSelection.model')
         FROM orchestration_v2_projection_threads
         WHERE project_id = ?1 AND deleted_at IS NULL
         ORDER BY updated_at DESC, rowid DESC
         LIMIT 20",
    ) {
        if let Ok(mut rows) = stmt.query(params![resolved_project_id]) {
            while let Ok(Some(row)) = rows.next() {
                push_explicit(row.get(0).ok().flatten(), row.get(1).ok().flatten());
            }
        }
    }

    // 3. Most recent runs across all local projects.
    if let Ok(mut stmt) = db.prepare(
        "SELECT json_extract(r.payload_json, '$.providerInstanceId'),
                json_extract(r.payload_json, '$.modelSelection.model')
         FROM orchestration_v2_projection_runs r
         JOIN orchestration_v2_projection_threads t ON t.thread_id = r.thread_id
         WHERE t.deleted_at IS NULL
         ORDER BY r.requested_at DESC, r.rowid DESC
         LIMIT 40",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                push_explicit(row.get(0).ok().flatten(), row.get(1).ok().flatten());
            }
        }
    }

    // 3b. Most recent threads across all local projects.
    if let Ok(mut stmt) = db.prepare(
        "SELECT COALESCE(json_extract(payload_json, '$.providerInstanceId'), default_provider),
                json_extract(payload_json, '$.modelSelection.model')
         FROM orchestration_v2_projection_threads
         WHERE deleted_at IS NULL
         ORDER BY updated_at DESC, rowid DESC
         LIMIT 20",
    ) {
        if let Ok(mut rows) = stmt.query([]) {
            while let Ok(Some(row)) = rows.next() {
                push_explicit(row.get(0).ok().flatten(), row.get(1).ok().flatten());
            }
        }
    }

    if choices.is_empty() {
        choices.push(ModelChoice::ProjectDefault { model_hint: None });
    }
    choices
}

/// Build the Telemetry Check handoff `(title, prompt)`.
pub fn telemetry_prompt(report: &telemetry::Report) -> (String, String) {
    telemetry::t3_handoff_prompt(report)
}

/// Build the Storage cleanup handoff `(title, prompt)`.
/// Contains only folder paths, measured sizes, thread states, and volume headroom already shown on screen.
pub fn storage_prompt(ctx: &StorageContext) -> (String, String) {
    let title = match &ctx.target {
        StorageTarget::Project { title, .. } => {
            format!("Clean up worktree storage ({})", short_title(title, 32))
        }
        StorageTarget::SettledOrGone { .. } => "Clean up settled & gone worktrees".into(),
        StorageTarget::Entry { title, .. } => {
            format!("Clean up storage ({})", short_title(title, 36))
        }
        StorageTarget::Other { .. } => "Clean up unassigned temp/scratch space".into(),
        StorageTarget::Overview => "Clean up T3 worktree & build storage".into(),
    };

    let mut out = String::new();
    out.push_str(
        "I noticed high disk usage in Loupe's Storage view. Please inspect these worktrees, build caches, and scratch folders, and help me reclaim disk space safely.\n\n",
    );

    if !ctx.volumes.is_empty() {
        out.push_str("## Volume headroom\n");
        for v in &ctx.volumes {
            let pct_free = if v.total_bytes > 0 {
                (v.available_bytes as f64 / v.total_bytes as f64) * 100.0
            } else {
                0.0
            };
            let mem = if v.is_tmpfs == Some(true) {
                " (tmpfs / in memory)"
            } else {
                ""
            };
            out.push_str(&format!(
                "- `{}`{mem}: {} free of {} ({pct_free:.0}% free)\n",
                v.label,
                fmt_bytes(v.available_bytes),
                fmt_bytes(v.total_bytes),
            ));
        }
        out.push('\n');
    }

    out.push_str("## Selected focus\n");
    match &ctx.target {
        StorageTarget::Project {
            title,
            thread_count,
            sizes,
            ..
        } => {
            out.push_str(&format!(
                "- Project: **{}** ({thread_count} worktree threads)\n- Measured footprint: {}\n",
                terminal::clean(title),
                fmt_sizes(*sizes),
            ));
        }
        StorageTarget::SettledOrGone { count, sizes } => {
            out.push_str(&format!(
                "- Group: **Settled or gone** ({count} folders whose thread is settled or no longer in T3)\n- Measured footprint: {}\n",
                fmt_sizes(*sizes),
            ));
        }
        StorageTarget::Entry {
            title,
            path,
            state,
            sizes,
        } => {
            out.push_str(&format!(
                "- Entry: **{}** (state: {})\n",
                terminal::clean(title),
                thread_state_word(*state),
            ));
            if let Some(p) = path {
                out.push_str(&format!("- Path: `{}`\n", p.display()));
            }
            out.push_str(&format!("- Measured footprint: {}\n", fmt_sizes(*sizes)));
        }
        StorageTarget::Other { sizes, temp_label } => {
            out.push_str(&format!(
                "- Group: **Other** (not tied to a thread; temp volume: `{}`)\n- Measured footprint: {}\n",
                temp_label.as_deref().unwrap_or("temp"),
                fmt_sizes(*sizes),
            ));
        }
        StorageTarget::Overview => {
            out.push_str("- Scope: overall T3 worktree and scratch usage\n");
        }
    }

    if ctx.settled_or_gone_count > 0 {
        out.push_str(&format!(
            "- Total settled/gone folders: {} (~{})\n",
            ctx.settled_or_gone_count,
            fmt_bytes(ctx.settled_or_gone_bytes),
        ));
    }

    if !ctx.top_items.is_empty() {
        out.push_str("\n## Largest folders in scope\n");
        for item in ctx.top_items.iter().take(12) {
            let mut parts = Vec::new();
            if let Some(w) = item.worktree_bytes.filter(|&b| b > 0) {
                parts.push(format!("worktree {}", fmt_bytes(w)));
            }
            if let Some(b) = item.build_bytes.filter(|&b| b > 0) {
                parts.push(format!("build {}", fmt_bytes(b)));
            }
            if let Some(s) = item.scratch_bytes.filter(|&b| b > 0) {
                parts.push(format!("scratch {}", fmt_bytes(s)));
            }
            let size_desc = if parts.is_empty() {
                "size unknown".into()
            } else {
                parts.join(", ")
            };
            let path_part = item
                .path
                .as_ref()
                .map(|p| format!(" — `{}`", p.display()))
                .unwrap_or_default();
            out.push_str(&format!(
                "- **{}** [{}]: {size_desc}{path_part}\n",
                terminal::clean(&item.title),
                thread_state_word(item.state),
            ));
        }
    }

    out.push_str(
        "\n## What to do\n\
1. Inspect the folders above (checking `git status`, unpushed branches/commits, and whether any process is actively using them).\n\
2. Rank the safest, highest-impact space savings first: e.g. removing settled/gone worktrees with no unpushed work, then cleaning heavy build caches (`target/`, `node_modules/`, `.next/`, etc.) or stale scratch files.\n\
3. Present the exact cleanup plan with estimated bytes reclaimed and **ask for my confirmation before deleting any files or worktrees**.\n",
    );

    (title, out)
}

/// Build a `StorageContext` from the current inventory and optional selected entry/group info.
pub fn storage_context_from_inventory(
    inventory: &Inventory,
    target: StorageTarget,
    focus_entries: &[&Entry],
) -> StorageContext {
    let mut volumes = Vec::new();
    for v in &inventory.worktree_volumes {
        if let Some(vol) = &v.volume {
            if vol.total_bytes > 0 {
                volumes.push(StorageVolume {
                    label: v.label.clone(),
                    available_bytes: vol.available_bytes,
                    total_bytes: vol.total_bytes,
                    is_tmpfs: vol.is_tmpfs,
                });
            }
        }
    }
    if let Some(temp) = &inventory.temporary_volume {
        if let Some(vol) = &temp.volume {
            if vol.total_bytes > 0
                && !volumes.iter().any(|existing| {
                    existing.label == temp.label && existing.total_bytes == vol.total_bytes
                })
            {
                volumes.push(StorageVolume {
                    label: temp.label.clone(),
                    available_bytes: vol.available_bytes,
                    total_bytes: vol.total_bytes,
                    is_tmpfs: vol.is_tmpfs,
                });
            }
        }
    }

    let settled_or_gone_count = inventory.settled_or_gone.len();
    let settled_or_gone_bytes: u64 = inventory
        .settled_or_gone
        .iter()
        .filter_map(|e| e.sizes.footprint())
        .map(|b| u64::try_from(b).unwrap_or(u64::MAX))
        .fold(0u64, u64::saturating_add);

    let mut ranked: Vec<&Entry> = if !focus_entries.is_empty() {
        focus_entries.to_vec()
    } else {
        inventory
            .settled_or_gone
            .iter()
            .chain(inventory.projects.iter().flat_map(|p| p.threads.iter()))
            .collect()
    };
    ranked.sort_by_key(|e| std::cmp::Reverse(e.sizes.footprint().unwrap_or(0)));

    let top_items = ranked
        .into_iter()
        .take(12)
        .map(|e| StorageItem {
            title: e.title.clone(),
            path: e.worktree_path.clone(),
            state: e.state,
            worktree_bytes: e.sizes.worktree.measured(),
            build_bytes: e.sizes.build.measured(),
            scratch_bytes: e.sizes.scratch.measured(),
        })
        .collect();

    StorageContext {
        target,
        volumes,
        settled_or_gone_count,
        settled_or_gone_bytes,
        top_items,
    }
}

/// Build the Trace investigation handoff `(title, prompt)`.
/// Strictly read-only & non-secret: uses only timing splits, statuses, allowlisted program names,
/// delegate model/titles, and IDs already present in `Trace`.
pub fn trace_prompt(
    t: &Trace,
    selected_span_key: Option<&str>,
    project_name: Option<&str>,
) -> (String, String) {
    let task_name = t.title.as_deref().unwrap_or("Untitled task");
    let title = format!("Investigate trace: {}", short_title(task_name, 42));

    let hi = t.ended.unwrap_or(t.now).max(t.requested);
    let elapsed_ms = (hi - t.requested).max(0.0);
    let state_word = match t.state {
        trace::State::Queued => "queued",
        trace::State::Running => "running",
        trace::State::Waiting => "waiting on you",
        trace::State::Delegated => "delegates working",
        trace::State::Completed => "completed",
        trace::State::Failed => "failed",
        trace::State::Interrupted => "interrupted",
        trace::State::Cancelled => "cancelled",
    };

    let mut out = String::new();
    out.push_str(
        "I inspected this task in Loupe's Trace view and want to understand why it was slow, failed, or stalled — and optimize the root cause.\n\n",
    );
    out.push_str("## Task summary (from Loupe Trace)\n");
    out.push_str(&format!("- Task: **{}**\n", terminal::clean(task_name)));
    if let Some(proj) = project_name.filter(|s| !s.is_empty()) {
        out.push_str(&format!("- Project: `{}`\n", terminal::clean(proj)));
    }
    out.push_str(&format!("- Thread ID: `{}`\n", t.thread));
    if let Some(run) = t.runs.last() {
        out.push_str(&format!(
            "- Latest run ID: `{}` (status: {}, {} total {})\n",
            run.id,
            run.status,
            t.runs.len(),
            if t.runs.len() == 1 { "run" } else { "runs" }
        ));
    }
    out.push_str(&format!(
        "- Model: `{}` ({}) · State: **{}** · Elapsed: **{}**\n",
        t.model,
        t.provider,
        state_word,
        fmt_ms(elapsed_ms),
    ));
    out.push_str(&format!(
        "- Steps: {} tool calls ({} failed, {} asks, {} delegates)\n",
        t.steps.tools, t.steps.failed, t.steps.asks, t.steps.delegates,
    ));

    let sp = &t.split;
    let total = sp.total.max(1.0);
    out.push_str("\n## Where the time went\n");
    out.push_str(&format!(
        "- Tools: {} ({:.0}%)\n",
        fmt_ms(sp.tools),
        100.0 * sp.tools / total
    ));
    out.push_str(&format!(
        "- Model: {} ({:.0}% — {} seen thinking, {} inferred)\n",
        fmt_ms(sp.seen + sp.model),
        100.0 * (sp.seen + sp.model) / total,
        fmt_ms(sp.seen),
        fmt_ms(sp.model),
    ));
    if sp.waiting > 0.0 {
        out.push_str(&format!(
            "- Waiting on you: {} ({:.0}%)\n",
            fmt_ms(sp.waiting),
            100.0 * sp.waiting / total
        ));
    }
    if sp.queue > 0.0 {
        out.push_str(&format!(
            "- Queued: {} ({:.0}%)\n",
            fmt_ms(sp.queue),
            100.0 * sp.queue / total
        ));
    }
    if sp.between > 0.0 {
        out.push_str(&format!(
            "- Idle / between runs: {} ({:.0}%)\n",
            fmt_ms(sp.between),
            100.0 * sp.between / total
        ));
    }

    if let Some(key) = selected_span_key {
        if let Some(s) = t.spans.iter().find(|s| s.key == key) {
            let dur = (s.end.unwrap_or(t.now) - s.start).max(0.0);
            out.push_str("\n## Highlighted step\n");
            out.push_str(&format!("- {}\n", format_trace_span(s, dur)));
        }
    }

    let mut failed_spans: Vec<_> = t
        .spans
        .iter()
        .filter(|s| {
            matches!(s.kind, TraceKind::Tool | TraceKind::Delegate)
                && matches!(s.status.as_str(), "failed" | "error")
        })
        .collect();
    failed_spans.sort_by(|a, b| {
        let da = a.end.unwrap_or(t.now) - a.start;
        let db = b.end.unwrap_or(t.now) - b.start;
        db.total_cmp(&da)
    });
    if !failed_spans.is_empty() {
        out.push_str("\n## Failed steps\n");
        for s in failed_spans.iter().take(8) {
            let dur = (s.end.unwrap_or(t.now) - s.start).max(0.0);
            out.push_str(&format!("- {}\n", format_trace_span(s, dur)));
        }
    }

    let mut slow_spans: Vec<_> = t
        .spans
        .iter()
        .filter(|s| {
            matches!(s.kind, TraceKind::Tool | TraceKind::Delegate) && !s.closed_together
        })
        .collect();
    slow_spans.sort_by(|a, b| {
        let da = a.end.unwrap_or(t.now) - a.start;
        let db = b.end.unwrap_or(t.now) - b.start;
        db.total_cmp(&da)
    });
    if !slow_spans.is_empty() {
        out.push_str("\n## Slowest steps\n");
        for s in slow_spans.iter().take(8) {
            let dur = (s.end.unwrap_or(t.now) - s.start).max(0.0);
            if dur >= 1_000.0 || s.end.is_none() {
                out.push_str(&format!("- {}\n", format_trace_span(s, dur)));
            }
        }
    }

    out.push_str(
        "\n## What to do\n\
1. Read thread `",
    );
    out.push_str(&t.thread);
    out.push_str(
        "` (using `t3_thread_read` if available) and inspect the slow or failing steps above.\n\
2. Pinpoint why the task spent so much time or failed (slow build/test command, retries, flaky tool, poor prompt/delegation split, or environment issue).\n\
3. Recommend concrete fixes — and if they belong in this repo (scripts, tests, config, or `AGENTS.md` guidance), propose implementing them.\n",
    );

    (title, out)
}

fn format_trace_span(s: &trace::Span, dur_ms: f64) -> String {
    let mut desc = if let Some(d) = &s.delegate {
        let label = d.title.as_deref().unwrap_or(&d.model);
        let remote = if d.remote { ", remote" } else { "" };
        let child = d
            .child_thread
            .as_deref()
            .map(|id| format!(", child thread `{id}`"))
            .unwrap_or_default();
        format!(
            "delegate **{}** (`{}`{remote}{child})",
            terminal::clean(label),
            d.model
        )
    } else if !s.programs.is_empty() {
        format!("command `{}` ({})", s.programs.join(" → "), s.category)
    } else {
        format!("tool `{}` ({})", terminal::clean(&s.label), s.category)
    };
    let open = if s.end.is_none() { " (still running)" } else { "" };
    desc.push_str(&format!(" — {} [{}]{open}", fmt_ms(dur_ms), s.status));
    desc
}

/// Build the Insights → Tools investigation & optimization handoff `(title, prompt)`.
/// Strictly non-secret: uses only aggregated program names, tool/MCP names, durations,
/// failure counts, integer exit codes, and thread/run IDs already present in `Tools`.
/// Zero command arguments or message bodies are ever read or included.
pub fn tools_prompt(report: &Tools) -> (String, String) {
    let period_word = match (report.period, report.so_far) {
        (crate::recap::Span::Week, true) => "this week",
        (crate::recap::Span::Week, false) => "last week",
        (crate::recap::Span::Month, true) => "this month",
        (crate::recap::Span::Month, false) => "last month",
    };
    let title = format!("Audit & optimize slow/failing tools ({period_word})");

    let mut out = String::new();
    out.push_str(
        "I reviewed Loupe's Tools report and want to investigate my top tool bottlenecks, command failures, and recurring friction — then optimize my toolchain, scripts, or agent setup.\n\n",
    );

    out.push_str(&format!(
        "## Tools overview ({} to {}, {period_word})\n",
        report.from, report.to
    ));
    let fail_pct = if report.calls > 0 {
        100.0 * f64::from(report.failed) / f64::from(report.calls)
    } else {
        0.0
    };
    out.push_str(&format!(
        "- Total calls: **{}** across {} threads ({} failed, {fail_pct:.1}%)\n",
        report.calls, report.threads, report.failed,
    ));
    out.push_str(&format!(
        "- Tool time: **{}** total ({} wall busy time)\n",
        fmt_ms(report.ms),
        fmt_ms(report.busy_ms),
    ));
    if report.commands.calls > 0 {
        let cmd_fail_pct =
            100.0 * f64::from(report.commands.failed) / f64::from(report.commands.calls);
        out.push_str(&format!(
            "- Shell commands: {} calls, {}, {} failed ({cmd_fail_pct:.1}%)\n",
            report.commands.calls,
            fmt_ms(report.commands.ms),
            report.commands.failed,
        ));
        let ex = &report.commands.exits;
        if ex.failed_known > 0 {
            let mut parts = Vec::new();
            if ex.one > 0 {
                parts.push(format!("exit 1: {}", ex.one));
            }
            if ex.two > 0 {
                parts.push(format!("exit 2 (usage/syntax): {}", ex.two));
            }
            if ex.timed_out > 0 {
                parts.push(format!("exit 124 (timed out): {}", ex.timed_out));
            }
            if ex.not_found > 0 {
                parts.push(format!("exit 127 (not found): {}", ex.not_found));
            }
            if ex.not_runnable > 0 {
                parts.push(format!("exit 126 (not runnable): {}", ex.not_runnable));
            }
            if ex.signal > 0 {
                parts.push(format!("killed by signal (129+): {}", ex.signal));
            }
            if ex.other > 0 {
                parts.push(format!("other nonzero: {}", ex.other));
            }
            if !parts.is_empty() {
                out.push_str(&format!("- Command failure exit codes: {}\n", parts.join(", ")));
            }
        }
    }

    // Top programs by total time
    let mut by_time: Vec<&tools::Row> = report.programs.iter().collect();
    by_time.sort_by(|a, b| b.ms.total_cmp(&a.ms));
    if !by_time.is_empty() {
        out.push_str("\n## Slowest shell programs (by total time)\n");
        for row in by_time.iter().take(10) {
            let name = row.name.as_deref().unwrap_or("(unnamed/local script)");
            let med = row
                .median_ms
                .map(|m| format!(", median {}", fmt_ms(m)))
                .unwrap_or_default();
            let rate = if row.calls > 0 {
                100.0 * f64::from(row.failed) / f64::from(row.calls)
            } else {
                0.0
            };
            let exits = summarize_exits(&row.exits);
            let exit_tail = if exits.is_empty() {
                String::new()
            } else {
                format!(" [{exits}]")
            };
            out.push_str(&format!(
                "- `{name}`: {} total across {} calls{med}, longest {}, {} failed ({rate:.0}%){exit_tail}\n",
                fmt_ms(row.ms),
                row.calls,
                fmt_ms(row.longest_ms),
                row.failed,
            ));
        }
    }

    // Top failing programs (highlighting actionable failures; noting benign exit-1 for rg/grep/diff)
    let mut by_fail: Vec<&tools::Row> = report
        .programs
        .iter()
        .filter(|r| r.failed > 0)
        .filter(|r| {
            let name = r.name.as_deref().unwrap_or("");
            // Skip pure exit-1 "found nothing" / "files differ" noise if that accounts for all failures.
            let benign_exit_one = (tools::FOUND_NOTHING.contains(&name)
                || tools::FILES_DIFFER.contains(&name))
                && r.exits.failed_known > 0
                && r.exits.one == r.exits.failed_known;
            !benign_exit_one
        })
        .collect();
    by_fail.sort_by(|a, b| b.failed.cmp(&a.failed).then(b.ms.total_cmp(&a.ms)));
    if !by_fail.is_empty() {
        out.push_str("\n## Most failing shell programs (actionable failures)\n");
        for row in by_fail.iter().take(8) {
            let name = row.name.as_deref().unwrap_or("(unnamed/local script)");
            let rate = if row.calls > 0 {
                100.0 * f64::from(row.failed) / f64::from(row.calls)
            } else {
                0.0
            };
            let exits = summarize_exits(&row.exits);
            let exit_tail = if exits.is_empty() {
                String::new()
            } else {
                format!(" — {exits}")
            };
            out.push_str(&format!(
                "- `{name}`: **{} failed** / {} calls ({rate:.0}%), {} total{exit_tail}\n",
                row.failed,
                row.calls,
                fmt_ms(row.ms),
            ));
        }
    }

    // Top non-shell tools / MCP tools (when slow or failing)
    let mut top_tools: Vec<&tools::Row> = report
        .tools
        .iter()
        .filter(|r| r.ms >= 10_000.0 || r.failed > 0)
        .collect();
    top_tools.sort_by(|a, b| b.ms.total_cmp(&a.ms).then(b.failed.cmp(&a.failed)));
    if !top_tools.is_empty() {
        out.push_str("\n## Notable non-shell / MCP tools\n");
        for row in top_tools.iter().take(8) {
            let label = match (&row.server, &row.name) {
                (Some(srv), Some(name)) => format!("{srv}/{name}"),
                (_, Some(name)) => name.clone(),
                _ => format!("{:?}", row.kind).to_lowercase(),
            };
            out.push_str(&format!(
                "- `{label}`: {} across {} calls, longest {}, {} failed\n",
                fmt_ms(row.ms),
                row.calls,
                fmt_ms(row.longest_ms),
                row.failed,
            ));
        }
    }

    // Slowest individual calls with thread IDs so the agent can inspect them directly!
    if !report.longest.is_empty() {
        out.push_str("\n## Longest individual calls (with thread IDs to inspect)\n");
        for item in report.longest.iter().take(6) {
            let name = match (&item.server, &item.name) {
                (Some(srv), Some(name)) => format!("{srv}/{name}"),
                (_, Some(name)) => name.clone(),
                _ => match item.kind {
                    ToolKind::Shell => "shell command".into(),
                    other => format!("{other:?}").to_lowercase(),
                },
            };
            let status = if item.failed {
                match item.exit {
                    Some(code) => format!(" [failed, exit {code}]"),
                    None => " [failed]".into(),
                }
            } else {
                String::new()
            };
            out.push_str(&format!(
                "- `{name}`: **{}**{status} ({}, {}, thread `{}`)\n",
                fmt_ms(item.ms),
                item.provider,
                item.when,
                item.thread,
            ));
        }
    }

    out.push_str(
        "\n## What to do\n\
1. Diagnose the top 2–3 time sinks and failure patterns above (feel free to inspect the referenced thread IDs with `t3_thread_read` or check this repository's build/test scripts, CI, and `AGENTS.md`).\n\
2. Propose high-leverage, concrete optimizations across:\n\
   - **Build / test / lint speed** (incremental caching, narrower test invocations, parallel runners, avoiding redundant full rebuilds).\n\
   - **Recurring command failures** (missing PATH/binaries for exit 127, timeouts for exit 124, bad flags for exit 2, flaky scripts or MCP tool friction).\n\
   - **Agent workflow / repo guidance** (scripts, aliases, or `AGENTS.md` rules that steer agents onto the fast, reliable path automatically).\n\
3. Highlight quick wins we can implement right now, and ask which ones I want you to apply.\n",
    );

    (title, out)
}

/// Build the Insights → Recap workflow optimization handoff `(title, prompt)`.
/// Strictly non-secret: uses only aggregated counts, durations, failure categories, and findings.
pub fn recap_prompt(report: &Recap) -> (String, String) {
    let period_word = match (report.period, report.so_far) {
        (crate::recap::Span::Week, true) => "this week",
        (crate::recap::Span::Week, false) => "last week",
        (crate::recap::Span::Month, true) => "this month",
        (crate::recap::Span::Month, false) => "last month",
    };
    let title = format!("Optimize agent workflow from Recap ({period_word})");

    let mut out = String::new();
    out.push_str(
        "I reviewed Loupe's Recap report and want to turn these patterns and findings into concrete workflow, model-routing, context-management, or repo setup optimizations.\n\n",
    );
    out.push_str(&format!(
        "## Period summary ({} to {}, {period_word})\n",
        report.from, report.to
    ));
    out.push_str(&format!(
        "- Agent time: **{:.1}h** across {} runs, {} threads, {} projects (peak concurrency: {})\n",
        report.hours, report.runs, report.threads, report.projects, report.most_at_once,
    ));
    let split_total = (report.split.tools + report.split.model + report.split.waiting).max(0.001);
    out.push_str(&format!(
        "- Time split: tools {:.1}h ({:.0}%), model {:.1}h ({:.0}%), waiting on you {:.1}h ({:.0}%)\n",
        report.split.tools,
        100.0 * report.split.tools / split_total,
        report.split.model,
        100.0 * report.split.model / split_total,
        report.split.waiting,
        100.0 * report.split.waiting / split_total,
    ));
    out.push_str(&format!(
        "- Tasks: {} total ({} finished, {} failed, {} stopped, {} cancelled)\n",
        report.tasks.total,
        report.tasks.finished,
        report.tasks.failed,
        report.tasks.stopped,
        report.tasks.cancelled,
    ));
    if report.tasks.failed > 0 {
        out.push_str(&format!(
            "- Failure causes: {} setup, {} plan limit, {} provider\n",
            report.tasks.failed_setup, report.tasks.failed_limit, report.tasks.failed_provider,
        ));
    }
    if !report.tasks.limit_by.is_empty() {
        let limits: Vec<String> = report
            .tasks
            .limit_by
            .iter()
            .map(|(p, n)| format!("{p} ({n})"))
            .collect();
        out.push_str(&format!("- Plan-limit stops by provider: {}\n", limits.join(", ")));
    }
    if report.compactions > 0 || report.turns_past_90 > 0 {
        out.push_str(&format!(
            "- Context pressure: {} compactions, {} turns past 90% window\n",
            report.compactions, report.turns_past_90,
        ));
    }
    if report.asks.count > 0 {
        let typical = report
            .asks
            .typical_ms
            .map(|m| format!(", median wait {}", fmt_ms(m)))
            .unwrap_or_default();
        let longest = report
            .asks
            .longest_ms
            .map(|m| format!(", longest {}", fmt_ms(m)))
            .unwrap_or_default();
        out.push_str(&format!(
            "- Waiting on you: {} asks ({:.1}h total{typical}{longest})\n",
            report.asks.count, report.asks.hours,
        ));
    }
    if report.handed.count > 0 {
        out.push_str(&format!(
            "- Delegate handoffs: {} total ({:.1}h, {} failed)\n",
            report.handed.count, report.handed.hours, report.handed.failed,
        ));
    }
    if report.commands > 0 {
        let fail_pct = 100.0 * f64::from(report.commands_failed) / f64::from(report.commands);
        out.push_str(&format!(
            "- Shell commands: {} ({} failed, {fail_pct:.1}%)\n",
            report.commands, report.commands_failed,
        ));
    }

    if !report.findings.is_empty() {
        out.push_str("\n## Loupe's flagged findings\n");
        for f in &report.findings {
            out.push_str(&format!(
                "- **{}** — {} ({})\n",
                terminal::clean(&f.number),
                terminal::clean(&f.text),
                terminal::clean(&f.rests_on),
            ));
        }
    }

    out.push_str(
        "\n## What to do\n\
1. Identify the top 2–3 leverage points from this period (e.g. reducing wait-on-you interruptions, avoiding context compaction / >90% turns, cutting tool or setup failures, tuning delegate handoffs, or balancing providers around plan limits).\n\
2. Check this repository's `AGENTS.md`, scripts, or workflow conventions where relevant.\n\
3. Give me concrete, actionable changes ranked by impact — and ask which ones I want you to implement.\n",
    );

    (title, out)
}

/// Build contextual quick-reply presets for `t3 thread send` on the Live dashboard.
pub fn live_send_presets(agent: &Agent, pace: Option<&Pace>) -> Vec<SendPreset> {
    let mut presets = Vec::new();
    let ratio = model::ratio(agent.context.used_tokens, agent.context.max_tokens);
    let stalled = pace.is_some_and(|p| p.stalled);
    let failed = matches!(agent.status.as_str(), "failed" | "error") || agent.failures > 0;
    let high_ctx = ratio.is_some_and(|r| r >= 0.85);
    let needs_you = model::needs_you(agent);

    if needs_you {
        presets.push(SendPreset {
            label: "custom reply…".into(),
            text: None,
        });
        presets.push(SendPreset {
            label: "proceed with recommended option".into(),
            text: Some(
                "Go ahead with your recommended option and keep going until verified.".into(),
            ),
        });
        presets.push(SendPreset {
            label: "summarize what you need".into(),
            text: Some(
                "Briefly summarize the exact decision you need from me and the tradeoffs in 3 bullet points.".into(),
            ),
        });
        return presets;
    }

    if stalled {
        presets.push(SendPreset {
            label: "nudge: status check".into(),
            text: Some(
                "Status check — you appear stalled. What's currently blocking you, and what's the fastest path to finish or wrap up cleanly?".into(),
            ),
        });
    }
    if failed {
        presets.push(SendPreset {
            label: "diagnose failure & recover".into(),
            text: Some(
                "Inspect the recent failure, explain the root cause briefly, and retry with a safer fix (or tell me if you're blocked).".into(),
            ),
        });
    }
    if high_ctx {
        presets.push(SendPreset {
            label: "wrap up & hand off (high context)".into(),
            text: Some(
                "Context is getting full. Pause new scope, checkpoint verified work, and write a concise handoff (status, commits, next steps) so we can continue cleanly.".into(),
            ),
        });
    }
    if presets.is_empty() {
        presets.push(SendPreset {
            label: "status update".into(),
            text: Some(
                "Quick status check: what's done, what's in flight, and are there any blockers?".into(),
            ),
        });
        presets.push(SendPreset {
            label: "checkpoint & summarize".into(),
            text: Some(
                "Pause at the next clean stopping point, verify what's changed so far, and summarize remaining work.".into(),
            ),
        });
    }
    presets.push(SendPreset {
        label: "custom message…".into(),
        text: None,
    });
    presets
}

/// Optional contextual nudge label for the Live Details pane (product mode only).
/// Returns `None` when the thread is healthy so Live stays quiet.
pub fn live_nudge_hint(agent: &Agent, pace: Option<&Pace>) -> Option<&'static str> {
    if agent.remote.is_some() || agent.reference_only == Some(true) || agent.thread_id.is_empty() {
        return None;
    }
    if model::needs_you(agent) {
        return Some("reply / nudge");
    }
    if pace.is_some_and(|p| p.stalled) {
        return Some("nudge stalled thread");
    }
    if matches!(agent.status.as_str(), "failed" | "error") {
        return Some("ask thread to recover");
    }
    let ratio = model::ratio(agent.context.used_tokens, agent.context.max_tokens);
    if ratio.is_some_and(|r| r >= 0.85) {
        return Some("ask for handoff");
    }
    None
}

/// Draw the unified 1-row T3 handoff bar onto row `y` of any full-screen `Canvas`.
pub fn draw_handoff_bar(c: &mut Canvas, y: i32, state: &HandoffState, p: &Palette) {
    if y < 0 || y >= c.h as i32 || state.is_idle() {
        return;
    }
    let w = c.w as i32;
    let fit = |s: &str, max: i32| terminal::fit(&terminal::clean(s), max.max(0) as usize);

    match state {
        HandoffState::Idle => {}
        HandoffState::ConfirmStart {
            action_label,
            project_name,
            project_count,
            model_label,
            model_count,
        } => {
            c.fill(0, y, w, 1, p.sel);
            let mut x = 1;
            x += c.text_bg(x, y, "⏎", p.accent, p.sel, true) + 1;
            let proj_hint = if *project_count > 1 {
                " · ←→ project"
            } else {
                ""
            };
            let model_hint = if *model_count > 1 {
                " · ↑↓ model"
            } else {
                ""
            };
            let suffix = format!("{proj_hint}{model_hint} · Esc cancel");
            let suffix_w = terminal::width(&suffix) as i32;
            let lead_room = (w - 2 - x - suffix_w).max(12);
            let model_part = if model_label.is_empty() {
                String::new()
            } else {
                format!(" · {model_label}")
            };
            let lead = if w >= 104 {
                format!("start T3 thread in {project_name}{model_part} ({action_label})")
            } else {
                format!("start in {project_name}{model_part} ({action_label})")
            };
            x += c.text_bg(x, y, &fit(&lead, lead_room), p.fg, p.sel, true);
            if x + 1 < w - 1 {
                c.text_bg(
                    x,
                    y,
                    &fit(&suffix, w - 1 - x),
                    p.text2,
                    p.sel,
                    false,
                );
            }
        }
        HandoffState::ComposeSend {
            thread_title,
            preset_label,
            preset_idx,
            preset_count,
            editing,
            draft,
        } => {
            c.fill(0, y, w, 1, p.sel);
            let mut x = 1;
            if *editing {
                let prefix = format!("Send to {}: ", short_title(thread_title, 22));
                x += c.text_bg(x, y, &fit(&prefix, (w / 3).max(12)), p.accent, p.sel, true);
                let tail = "  ⏎ send · Esc back";
                let tail_w = terminal::width(tail) as i32;
                let input_room = (w - 2 - x - tail_w).max(6);
                let shown_draft = tail_fit(draft, input_room.saturating_sub(1) as usize);
                x += c.text_bg(x, y, &shown_draft, p.bright, p.sel, false);
                if x < w - 1 {
                    x += c.text_bg(x, y, "▏", p.accent, p.sel, true);
                }
                if w - 1 - tail_w > x {
                    c.text_bg(w - 1 - tail_w, y, tail, p.dim, p.sel, false);
                }
            } else {
                let is_custom = preset_label.ends_with('…');
                let enter_word = if is_custom { "type" } else { "send" };
                x += c.text_bg(x, y, "⏎", p.accent, p.sel, true) + 1;
                let idx_hint = if *preset_count > 1 {
                    format!(" ({}/{})", preset_idx + 1, preset_count)
                } else {
                    String::new()
                };
                let tail = " · ←→ pick · Tab type · Esc cancel";
                let tail_w = terminal::width(tail) as i32;
                let lead_room = (w - 2 - x - tail_w).max(10);
                let lead = format!(
                    "{enter_word} to {}: {preset_label}{idx_hint}",
                    short_title(thread_title, 20)
                );
                x += c.text_bg(x, y, &fit(&lead, lead_room), p.fg, p.sel, true);
                if x + 1 < w - 1 {
                    c.text_bg(x, y, &fit(tail, w - 1 - x), p.text2, p.sel, false);
                }
            }
        }
        HandoffState::Running { label } => {
            c.fill(0, y, w, 1, p.panel);
            c.text_bg(
                1,
                y,
                &fit(&format!("… {label}"), w - 2),
                p.text2,
                p.panel,
                false,
            );
        }
        HandoffState::Done { message } => {
            c.fill(0, y, w, 1, p.panel);
            c.text_bg(
                1,
                y,
                &fit(&format!("✓ {message} · Esc dismiss"), w - 2),
                p.bright,
                p.panel,
                false,
            );
        }
        HandoffState::DemoPreview(msg) => {
            c.fill(0, y, w, 1, p.panel);
            c.text_bg(
                1,
                y,
                &fit(&format!("· {msg} · Esc dismiss"), w - 2),
                p.text2,
                p.panel,
                false,
            );
        }
        HandoffState::Error(err) => {
            c.fill(0, y, w, 1, p.panel);
            c.text_bg(
                1,
                y,
                &fit(&format!("✗ {err} · Esc dismiss"), w - 2),
                p.hot,
                p.panel,
                false,
            );
        }
    }
}

/// Overlay the subtle `f optimize in T3` footer/help hint and active handoff bar onto an
/// `InsightsView` canvas without mutating `InsightsView`'s fixture-tested renderer.
pub fn decorate_insights_canvas(
    c: &mut Canvas,
    tab: crate::insights_view::Tab,
    help: bool,
    actionable: bool,
    handoff: &HandoffState,
    p: &Palette,
) {
    let w = c.w as i32;
    let h = c.h as i32;
    if w < 52 || h < 14 {
        return;
    }

    if help {
        // Place the `f` key note cleanly into the first blank row below the existing help notes.
        for y in 10..h.saturating_sub(2) {
            let blank = (0..c.w).all(|x| {
                c.get(x as i32, y)
                    .is_none_or(|cell| cell.ch == ' ' || cell.ch == '\0')
            });
            if blank {
                c.text_bold(2, y, "f", p.accent);
                c.text(
                    15,
                    y,
                    &terminal::fit(
                        "start a T3 thread to investigate & optimize this report",
                        c.w.saturating_sub(16),
                    ),
                    p.text2,
                );
                break;
            }
        }
    } else if actionable {
        // Insert `f optimize in T3` into the footer right after the existing keys if space permits.
        let fy = h - 1;
        // Find where the left-hand key list ends (stop before right-aligned status).
        // Insights footer has keys starting at x=1 separated by double spaces; right status sits at w-1.
        let row_text = c.row_text(fy as usize);
        let label = match tab {
            crate::insights_view::Tab::Tools => "optimize tools",
            crate::insights_view::Tab::Recap => "optimize",
        };
        let need = 2 + 1 + 1 + terminal::width(label) as i32;
        if let Some((keys_end, status_start)) = footer_gap(&row_text) {
            if keys_end + need + 2 <= status_start {
                let mut x = keys_end + 2;
                x += c.text_bg(x, fy, "f", p.accent, p.panel, true) + 1;
                c.text_bg(x, fy, label, p.dim, p.panel, false);
            }
        }
    }

    if !handoff.is_idle() {
        draw_handoff_bar(c, h - 2, handoff, p);
    }
}

/// Find `(end_of_left_keys, start_of_right_status)` in a rendered footer row (in display cells).
fn footer_gap(row: &str) -> Option<(i32, i32)> {
    // Look for the widest run of 3+ spaces separating left controls from right status.
    let chars: Vec<(i32, char)> = {
        let mut x = 0i32;
        let mut out = Vec::new();
        for ch in row.chars() {
            let cw = terminal::width(&ch.to_string()).max(1) as i32;
            out.push((x, ch));
            x += cw;
        }
        out
    };
    let mut best: Option<(i32, i32, i32)> = None; // (gap_len, start, end)
    let mut i = 0;
    while i < chars.len() {
        if chars[i].1 == ' ' {
            let start_x = chars[i].0;
            let mut j = i;
            while j < chars.len() && chars[j].1 == ' ' {
                j += 1;
            }
            if i > 0 && j < chars.len() {
                let end_x = chars[j].0;
                let len = end_x - start_x;
                if len >= 3 && best.is_none_or(|b| len > b.0) {
                    best = Some((len, start_x, end_x));
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    best.map(|(_, start, end)| (start, end))
}

fn summarize_exits(ex: &tools::Exits) -> String {
    let mut parts = Vec::new();
    if ex.timed_out > 0 {
        parts.push(format!("{} timed out (124)", ex.timed_out));
    }
    if ex.not_found > 0 {
        parts.push(format!("{} not found (127)", ex.not_found));
    }
    if ex.not_runnable > 0 {
        parts.push(format!("{} not runnable (126)", ex.not_runnable));
    }
    if ex.two > 0 {
        parts.push(format!("{} usage/exit 2", ex.two));
    }
    if ex.signal > 0 {
        parts.push(format!("{} signal", ex.signal));
    }
    if ex.one > 0 {
        parts.push(format!("{} exit 1", ex.one));
    }
    if ex.other > 0 {
        parts.push(format!("{} other exit", ex.other));
    }
    parts.join(", ")
}

fn thread_state_word(state: ThreadState) -> &'static str {
    match state {
        ThreadState::Running => "running",
        ThreadState::Quiet => "quiet",
        ThreadState::NeedsYou => "needs you",
        ThreadState::Done => "done",
        ThreadState::Settled => "settled",
        ThreadState::Gone => "gone (no thread in T3)",
        ThreadState::Queued => "queued",
        ThreadState::Failed => "failed",
        ThreadState::Interrupted => "interrupted",
    }
}

fn fmt_sizes(sizes: Sizes) -> String {
    let mut parts = Vec::new();
    match sizes.worktree {
        Bytes::Exact(n) => parts.push(format!("worktree {}", fmt_bytes(n))),
        Bytes::AtLeast(n) => parts.push(format!("worktree ≥{}", fmt_bytes(n))),
        Bytes::Absent | Bytes::Unknown => {}
    }
    match sizes.build {
        Bytes::Exact(n) if n > 0 => parts.push(format!("build {}", fmt_bytes(n))),
        Bytes::AtLeast(n) if n > 0 => parts.push(format!("build ≥{}", fmt_bytes(n))),
        _ => {}
    }
    match sizes.scratch {
        Bytes::Exact(n) if n > 0 => parts.push(format!("scratch {}", fmt_bytes(n))),
        Bytes::AtLeast(n) if n > 0 => parts.push(format!("scratch ≥{}", fmt_bytes(n))),
        _ => {}
    }
    if parts.is_empty() {
        "not yet measured".into()
    } else {
        parts.join(", ")
    }
}

fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1_000.0;
    const MB: f64 = 1_000_000.0;
    const GB: f64 = 1_000_000_000.0;
    const TB: f64 = 1_000_000_000_000.0;
    let b = bytes as f64;
    if b >= TB {
        format!("{:.1} TB", b / TB)
    } else if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

fn fmt_ms(ms: f64) -> String {
    let s = (ms.max(0.0) / 1000.0).round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        let m = s / 60;
        format!("{}h {:02}m", m / 60, m % 60)
    }
}

fn short_title(s: &str, max_cells: usize) -> String {
    terminal::fit(&terminal::clean(s), max_cells)
}

/// Keep the trailing characters of `text` so the user always sees what they are typing.
fn tail_fit(text: &str, max_cells: usize) -> String {
    if max_cells == 0 {
        return String::new();
    }
    let clean = terminal::clean(text);
    if terminal::width(&clean) <= max_cells {
        return clean;
    }
    let mut chars: Vec<char> = Vec::new();
    let mut used = 1usize; // reserve 1 cell for leading '…'
    for ch in clean.chars().rev() {
        let mut buf = [0u8; 4];
        let cw = terminal::width(ch.encode_utf8(&mut buf));
        if used + cw > max_cells {
            break;
        }
        used += cw;
        chars.push(ch);
    }
    chars.reverse();
    let mut out = String::from("…");
    for ch in chars {
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Context, Pace};

    #[test]
    fn tools_prompt_highlights_bottlenecks_and_filters_benign_rg_exit_one() {
        let mut report = Tools {
            period: crate::recap::Span::Week,
            from: "2026-11-16".into(),
            to: "2026-11-22".into(),
            so_far: true,
            calls: 120,
            ms: 900_000.0,
            busy_ms: 720_000.0,
            failed: 18,
            threads: 9,
            ..Tools::default()
        };
        report.commands.calls = 80;
        report.commands.ms = 800_000.0;
        report.commands.failed = 15;
        report.commands.exits.failed_known = 15;
        report.commands.exits.one = 9;
        report.commands.exits.timed_out = 4;
        report.commands.exits.not_found = 2;

        report.programs.push(tools::Row {
            kind: ToolKind::Shell,
            name: Some("cargo".into()),
            calls: 20,
            ms: 540_000.0,
            failed: 6,
            longest_ms: 120_000.0,
            median_ms: Some(18_000.0),
            exits: tools::Exits {
                known: 20,
                failed_known: 6,
                one: 2,
                timed_out: 4,
                ..tools::Exits::default()
            },
            ..tools::Row::default()
        });
        // rg failing ONLY with exit 1 should be treated as benign "found nothing" in the failure list.
        report.programs.push(tools::Row {
            kind: ToolKind::Shell,
            name: Some("rg".into()),
            calls: 30,
            ms: 5_000.0,
            failed: 7,
            longest_ms: 400.0,
            median_ms: Some(80.0),
            exits: tools::Exits {
                known: 30,
                failed_known: 7,
                one: 7,
                ..tools::Exits::default()
            },
            ..tools::Row::default()
        });
        report.longest.push(tools::Long {
            kind: ToolKind::Shell,
            name: Some("cargo".into()),
            ms: 120_000.0,
            failed: true,
            provider: "claude".into(),
            thread: "thread-abc12345".into(),
            when: "Thu 14:10".into(),
            exit: Some(124),
            ..tools::Long::default()
        });

        let (title, prompt) = tools_prompt(&report);
        assert!(title.contains("tools"));
        assert!(prompt.contains("`cargo`"));
        assert!(prompt.contains("exit 124 (timed out): 4"));
        assert!(prompt.contains("exit 127 (not found): 2"));
        assert!(prompt.contains("thread `thread-abc12345`"));
        // rg should not be listed under actionable failures when 100% of failures are exit 1.
        let fail_section = prompt
            .split("## Most failing shell programs")
            .nth(1)
            .and_then(|s| s.split("## ").next())
            .unwrap_or("");
        assert!(fail_section.contains("`cargo`"));
        assert!(!fail_section.contains("`rg`"));
    }

    #[test]
    fn trace_prompt_contains_timing_and_spans_without_secrets() {
        let t = trace::demo(1_795_000_000_000.0).unwrap();
        let (title, prompt) = trace_prompt(&t, None, Some("loupe"));
        assert!(title.starts_with("Investigate trace:"));
        assert!(prompt.contains(&t.thread));
        assert!(prompt.contains("Where the time went"));
        assert!(prompt.contains("Project: `loupe`"));
    }

    #[test]
    fn recap_prompt_includes_findings_and_metrics() {
        let mut r = Recap {
            period: crate::recap::Span::Week,
            from: "2026-11-16".into(),
            to: "2026-11-22".into(),
            so_far: true,
            hours: 14.5,
            runs: 42,
            threads: 12,
            projects: 3,
            compactions: 5,
            turns_past_90: 7,
            ..Recap::default()
        };
        r.findings.push(crate::recap::Finding {
            kind: "compaction",
            number: "5".into(),
            text: "5 compactions this week".into(),
            rests_on: "42 runs".into(),
        });
        let (title, prompt) = recap_prompt(&r);
        assert!(title.contains("Recap"));
        assert!(prompt.contains("5 compactions"));
        assert!(prompt.contains("7 turns past 90%"));
    }

    #[test]
    fn storage_prompt_asks_for_confirmation_before_deleting() {
        let ctx = StorageContext {
            target: StorageTarget::SettledOrGone {
                count: 4,
                sizes: Sizes {
                    worktree: Bytes::Exact(18_400_000_000),
                    build: Bytes::Exact(14_200_000_000),
                    scratch: Bytes::Exact(500_000_000),
                },
            },
            volumes: vec![StorageVolume {
                label: "/home".into(),
                available_bytes: 12_000_000_000,
                total_bytes: 500_000_000_000,
                is_tmpfs: Some(false),
            }],
            settled_or_gone_count: 4,
            settled_or_gone_bytes: 18_900_000_000,
            top_items: vec![StorageItem {
                title: "old-refactor".into(),
                path: Some(PathBuf::from("/tmp/wt/old-refactor")),
                state: ThreadState::Gone,
                worktree_bytes: Some(12_000_000_000),
                build_bytes: Some(10_500_000_000),
                scratch_bytes: None,
            }],
        };
        let (title, prompt) = storage_prompt(&ctx);
        assert!(title.contains("settled"));
        assert!(prompt.contains("18.4 GB"));
        assert!(prompt.contains("/tmp/wt/old-refactor"));
        assert!(prompt.contains("ask for my confirmation before deleting"));
    }

    #[test]
    fn live_presets_adapt_to_agent_state() {
        let mut agent = Agent::default();
        agent.thread_id = "thread-1".into();
        agent.status = "running".into();
        agent.context = Context {
            used_tokens: Some(180_000.0),
            max_tokens: Some(200_000.0),
            ..Context::default()
        };
        let stalled_pace = Pace {
            quiet: true,
            stalled: true,
            quiet_ms: 600_000.0,
            tool_ms: 0.0,
            delegating: false,
        };
        let presets = live_send_presets(&agent, Some(&stalled_pace));
        assert!(presets.iter().any(|p| p.label.contains("stalled") || p.label.contains("nudge")));
        assert!(presets.iter().any(|p| p.label.contains("hand off")));
        assert!(presets.last().unwrap().text.is_none());
        assert_eq!(live_nudge_hint(&agent, Some(&stalled_pace)), Some("nudge stalled thread"));
        agent.thread_id.clear();
        assert_eq!(live_nudge_hint(&agent, Some(&stalled_pace)), None);
    }

    #[test]
    fn draw_handoff_bar_never_panics_across_widths_and_states() {
        let p = Palette::fallback();
        let states = [
            HandoffState::Idle,
            HandoffState::ConfirmStart {
                action_label: "Optimize slow & failing tools".into(),
                project_name: "loupe".into(),
                project_count: 3,
                model_label: "opus-5.5".into(),
                model_count: 4,
            },
            HandoffState::ComposeSend {
                thread_title: "Refactor telemetry check and verify CI".into(),
                preset_label: "nudge: status check".into(),
                preset_idx: 0,
                preset_count: 3,
                editing: false,
                draft: String::new(),
            },
            HandoffState::ComposeSend {
                thread_title: "Refactor telemetry check".into(),
                preset_label: "custom reply…".into(),
                preset_idx: 2,
                preset_count: 3,
                editing: true,
                draft: "Please wrap up the tests and summarize what is left to do".into(),
            },
            HandoffState::Running {
                label: "Starting T3 thread in loupe…".into(),
            },
            HandoffState::Done {
                message: "Started in loupe · t3-123456 · open in T3 Code".into(),
            },
            HandoffState::DemoPreview("Demo: would start a T3 thread".into()),
            HandoffState::Error("t3 CLI not found on PATH".into()),
        ];
        for w in [48, 52, 80, 120, 220] {
            for st in &states {
                let mut c = Canvas::new(w, 20);
                c.clear(p.bg, p.fg);
                draw_handoff_bar(&mut c, 18, st, &p);
            }
        }
    }

    #[test]
    fn read_model_choices_follows_project_default_then_project_recent_then_global_recent() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE projection_projects (
                project_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                workspace_root TEXT NOT NULL,
                default_model_selection_json TEXT,
                deleted_at TEXT
             );
             CREATE TABLE orchestration_v2_projection_threads (
                thread_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                title TEXT NOT NULL,
                default_provider TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                deleted_at TEXT,
                payload_json TEXT NOT NULL
             );
             CREATE TABLE orchestration_v2_projection_runs (
                run_id TEXT PRIMARY KEY,
                thread_id TEXT NOT NULL,
                requested_at TEXT NOT NULL,
                payload_json TEXT NOT NULL
             );
             INSERT INTO projection_projects VALUES
               ('p-with-default', 'Alpha', '/w/alpha', '{\"model\":\"claude-opus-5-5\"}', NULL),
               ('p-no-default',   'Beta',  '/w/beta',  NULL, NULL),
               ('p-fresh',        'Gamma', '/w/gamma', NULL, NULL);
             INSERT INTO orchestration_v2_projection_threads VALUES
               ('t-alpha', 'p-with-default', 'A', 'claudeAgent', '2026-11-24T10:00:00.000Z', NULL, '{\"providerInstanceId\":\"claudeAgent\",\"modelSelection\":{\"model\":\"claude-opus-5-5\"}}'),
               ('t-beta',  'p-no-default',   'B', 'codex',       '2026-11-24T11:00:00.000Z', NULL, '{\"providerInstanceId\":\"codex\",\"modelSelection\":{\"model\":\"gpt-6.1-sol\"}}');
             INSERT INTO orchestration_v2_projection_runs VALUES
               ('r1', 't-alpha', '2026-11-24T10:05:00.000Z', '{\"providerInstanceId\":\"antigravity\",\"modelSelection\":{\"model\":\"gemini-4-neon\"}}'),
               ('r2', 't-beta',  '2026-11-24T11:05:00.000Z', '{\"providerInstanceId\":\"codex\",\"modelSelection\":{\"model\":\"gpt-6.1-sol\"}}');",
        )
        .unwrap();

        // 1. Project WITH default puts ProjectDefault first, followed by recent explicit models.
        let alpha = read_model_choices_from(&db, "p-with-default");
        assert_eq!(
            alpha[0],
            ModelChoice::ProjectDefault {
                model_hint: Some("claude-opus-5-5".into()),
            }
        );
        assert_eq!(
            alpha[1],
            ModelChoice::Explicit {
                provider: "antigravity".into(),
                model: "gemini-4-neon".into(),
            }
        );

        // 2. Project WITHOUT default inherits its own most recent run first (no hard stop!).
        let beta = read_model_choices_from(&db, "p-no-default");
        assert_eq!(
            beta[0],
            ModelChoice::Explicit {
                provider: "codex".into(),
                model: "gpt-6.1-sol".into(),
            }
        );
        assert!(beta.iter().any(|c| matches!(c, ModelChoice::Explicit { model, .. } if model == "gemini-4-neon")));

        // 3. Brand-new project with no default and no runs inherits globally most recent run.
        let gamma = read_model_choices_from(&db, "p-fresh");
        assert_eq!(
            gamma[0],
            ModelChoice::Explicit {
                provider: "codex".into(),
                model: "gpt-6.1-sol".into(),
            }
        );
    }
}
