//! T3 CLEANUP SETTINGS: every T3 auto-delete setting, what this machine has it set to, and
//! what in the worktrees today it would cover. A mode of Storage, like its help. Loupe reads
//! T3's settings and changes nothing; `f` starts a T3 thread to set them up.

use super::cleanup::{Part, bytes, days, draw_parts, keep_text, part, plural, rule_tokens};
use super::{draw_footer, fit, row_ink, width};
use crate::storage_view::data::{
    Cleanup, Count, Estimate, KeepWhen, Pool, ProjectRule, Rules, Setting, Tally, Today,
};
use crate::terminal::{self, Canvas, Palette, Rgb};

pub const WHERE: &str = "T3 Settings › Worktrees › Automatic worktree cleanup";

/// One selectable row of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRow<'a> {
    Setting(Setting),
    Keep,
    Project(&'a str, ProjectRule),
    NoThread,
}

impl PanelRow<'_> {
    /// The enum the `f` handoff names as the row the person asked about.
    pub fn word(&self) -> &'static str {
        match self {
            Self::Setting(Setting::Settled) => "settled",
            Self::Setting(Setting::Inactive) => "inactive",
            Self::Setting(Setting::Deleted) => "deleted-threads",
            Self::Setting(Setting::Merged) => "merged",
            Self::Setting(Setting::Unchanged) => "unchanged",
            Self::Keep => "keep-when",
            Self::Project(..) => "project-rule",
            Self::NoThread => "no-thread-folders",
        }
    }
}

/// The panel's rows in order: the five settings, keep-when, projects with their own rule by
/// name, then folders with no thread.
pub fn rows(band: &Cleanup) -> Vec<PanelRow<'_>> {
    let mut rows: Vec<PanelRow<'_>> = Setting::ALL.into_iter().map(PanelRow::Setting).collect();
    rows.push(PanelRow::Keep);
    let mut projects: Vec<(&str, ProjectRule)> = band
        .project_rules
        .iter()
        .map(|(id, rule)| (id.as_str(), *rule))
        .collect();
    projects.sort_by(|a, b| band.title(a.0).cmp(band.title(b.0)).then(a.0.cmp(b.0)));
    rows.extend(
        projects
            .into_iter()
            .map(|(id, rule)| PanelRow::Project(id, rule)),
    );
    rows.push(PanelRow::NoThread);
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Line {
    Blank,
    Section(&'static str),
    Row(usize),
}

/// Where the panel's parts go for one terminal size.
pub(crate) struct Geometry {
    pub wide: bool,
    /// The full explanation (name, up to three lines, the sizes note) or the two-line one.
    pub full: bool,
    pub headings: i32,
    pub list_top: i32,
    pub list_lines: usize,
    pub explain_top: i32,
    pub lines: Vec<Line>,
}

impl Geometry {
    pub(crate) fn new(band: &Cleanup, w: usize, h: usize) -> Self {
        let wide = w >= 100;
        let h = h as i32;
        let full = wide && h >= 30;
        let rows = rows(band);
        let mut lines = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if wide {
                match row {
                    PanelRow::Project(..) if !matches!(rows[i - 1], PanelRow::Project(..)) => {
                        lines.extend([Line::Blank, Line::Section("PROJECTS WITH THEIR OWN RULE")]);
                    }
                    PanelRow::NoThread => {
                        lines.extend([Line::Blank, Line::Section("NOT COVERED BY ANY SETTING")]);
                    }
                    _ => {}
                }
            }
            lines.push(Line::Row(i));
        }
        let headings = if wide { 3 } else { 2 };
        // The explanation (name, up to three lines, the sizes note; or two short lines) sits
        // above the footer, with a blank line between in the full form.
        let explain_top = if full { h - 7 } else { h - 3 };
        let list_top = headings + 1;
        let list_end = explain_top - 1;
        Self {
            wide,
            full,
            headings,
            list_top,
            list_lines: (list_end - list_top).max(1) as usize,
            explain_top,
            lines,
        }
    }

    pub(crate) fn line_of(&self, row: usize) -> usize {
        self.lines
            .iter()
            .position(|l| *l == Line::Row(row))
            .unwrap_or(0)
    }

    /// The first line shown so that `row` is visible.
    pub(crate) fn scroll(&self, first: usize, row: usize) -> usize {
        let at = self.line_of(row);
        let room = self.list_lines;
        let mut first = first.min(self.lines.len().saturating_sub(room));
        if at < first {
            first = at;
        } else if at >= first + room {
            first = at + 1 - room;
        }
        // A section heading stays with its first row.
        if first > 0 && first == at && matches!(self.lines[first - 1], Line::Section(_)) {
            first -= 1;
        }
        first
    }

    /// The row on terminal line `y`, if any.
    pub(crate) fn row_at(&self, first: usize, y: usize) -> Option<usize> {
        let offset = (y as i32).checked_sub(self.list_top)?;
        if offset < 0 || offset as usize >= self.list_lines {
            return None;
        }
        match self.lines.get(first + offset as usize)? {
            Line::Row(i) => Some(*i),
            _ => None,
        }
    }
}

fn today(band: &Cleanup) -> Option<&Today> {
    match &band.estimate {
        Estimate::Ready(t) => Some(t),
        _ => None,
    }
}

fn no_estimate(band: &Cleanup, wide: bool, p: &Palette) -> Part {
    part(
        match (band.estimate, wide) {
            (Estimate::Estimating, _) => "estimating…",
            (Estimate::NoSizes, true) => "no estimate: folder sizes unavailable",
            (Estimate::NoTimes, true) => "no estimate: thread times unavailable",
            _ => "no estimate",
        },
        p.dim,
    )
}

fn yours(row: PanelRow<'_>, band: &Cleanup, wide: bool, p: &Palette) -> Part {
    let rules = band.global.unwrap_or_default();
    let after = |n: Option<u16>| match n {
        None => part("off", p.warn),
        Some(0) => part("at next cleanup", p.fg),
        Some(n) => part(format!("after {}", days(n, true)), p.fg),
    };
    let on = |on: bool| {
        if on {
            part("on", p.fg)
        } else {
            part("off", p.warn)
        }
    };
    match row {
        PanelRow::Setting(Setting::Settled) => after(rules.settled_after_days),
        PanelRow::Setting(Setting::Inactive) => after(rules.after_days),
        PanelRow::Setting(Setting::Deleted) => on(rules.on_delete),
        PanelRow::Setting(Setting::Merged) => on(rules.on_merge),
        PanelRow::Setting(Setting::Unchanged) => on(rules.unchanged),
        PanelRow::Keep => part(
            match (rules.keep_when, wide) {
                (KeepWhen::UncommittedChanges, true) => "uncommitted changes",
                (KeepWhen::UncommittedChanges, false) => "uncommitted",
                (KeepWhen::TrackedChanges, true) => "edited tracked files",
                (KeepWhen::TrackedChanges, false) => "tracked edits",
                (KeepWhen::AnyLocalFiles, true) => "any local files",
                (KeepWhen::AnyLocalFiles, false) => "any local",
            },
            if rules.keep_when == KeepWhen::AnyLocalFiles && band.global_build {
                p.warn
            } else {
                p.fg
            },
        ),
        PanelRow::Project(_, rule) if !wide => part(
            if rule.rules().enabled() {
                "own rule"
            } else {
                "own rule: off"
            },
            p.fg,
        ),
        PanelRow::Project(_, rule) => {
            let rules = rule.rules();
            if !rules.enabled() {
                return part("off", p.warn);
            }
            let long = rule_tokens(&rules, true, false).join(" · ");
            if width(&long) <= 21 {
                part(long, p.fg)
            } else {
                part(rule_tokens(&rules, false, false).join(" · "), p.fg)
            }
        }
        PanelRow::NoThread => part("—", p.dim),
    }
}

/// `N due now · bytes · M more in D days · bytes`, or `nothing due · next in D days`.
fn due(tally: Tally, wide: bool, p: &Palette) -> Vec<Part> {
    let mut parts = Vec::new();
    if tally.now.worktrees > 0 {
        if wide {
            parts.push(part(format!("{} due now", tally.now.worktrees), p.fg));
        } else {
            parts.push(part(format!("{} due", tally.now.worktrees), p.fg));
        }
        parts.push(part(" · ", p.dim));
        parts.push(part(bytes(tally.now), p.fg));
        if let Some(next) = tally.next {
            parts.push(part(
                if wide {
                    format!(
                        " · {} more in {} · {}",
                        next.count.worktrees,
                        plural(next.days, "day", "days"),
                        bytes(next.count)
                    )
                } else {
                    format!(" · {} in {}d", next.count.worktrees, next.days)
                },
                p.text2,
            ));
        }
    } else {
        parts.push(part("nothing due", p.fg));
        if let Some(next) = tally.next {
            parts.push(part(
                if wide {
                    format!(" · next in {}", plural(next.days, "day", "days"))
                } else {
                    format!(" · next {}d", next.days)
                },
                p.text2,
            ));
        }
    }
    parts
}

/// An off setting's group: the count, its bytes, and what the settings that are on leave.
fn pool(
    band: &Cleanup,
    pool: Pool,
    noun: String,
    none: &str,
    tail: Vec<Part>,
    wide: bool,
    p: &Palette,
) -> Vec<Part> {
    if pool.count.worktrees == 0 {
        return vec![part(none, p.dim)];
    }
    let something_on = band.settings_on() > 0;
    let uncovered = pool.uncovered.worktrees > 0;
    let mut parts = vec![part(noun, p.fg), part(" · ", p.dim)];
    parts.push(part(
        bytes(pool.count),
        if uncovered { p.warn } else { p.text2 },
    ));
    if !something_on {
        parts.extend(tail);
    } else if pool.uncovered.worktrees == 0 {
        let by = pool.covered_by.single().map_or("your rules", Setting::word);
        parts.push(part(
            if wide {
                format!(" · already covered by {by}")
            } else {
                " · covered".into()
            },
            p.dim,
        ));
    } else if pool.uncovered == pool.count {
        if wide {
            parts.push(part(" · nothing else covers it", p.text2));
        }
    } else {
        parts.push(part(
            if wide {
                format!(
                    " · adds {} · {}",
                    pool.uncovered.worktrees,
                    bytes(pool.uncovered)
                )
            } else {
                format!(" · {} new", pool.uncovered.worktrees)
            },
            p.text2,
        ));
    }
    parts
}

fn what(row: PanelRow<'_>, band: &Cleanup, wide: bool, p: &Palette) -> Vec<Part> {
    let rules = band.global.unwrap_or_default();
    let estimable = matches!(
        row,
        PanelRow::Setting(Setting::Settled | Setting::Inactive | Setting::Deleted)
            | PanelRow::NoThread
    );
    let Some(t) = today(band) else {
        if estimable {
            return vec![no_estimate(band, wide, p)];
        }
        return what_fixed(row, band, &rules, wide, p);
    };
    match row {
        PanelRow::Setting(Setting::Settled) if rules.settled_after_days.is_some() => {
            due(t.settled.due, wide, p)
        }
        PanelRow::Setting(Setting::Settled) => {
            let tail = match t.oldest_settled_days {
                Some(d) if wide => vec![part(
                    match d {
                        0 => " · oldest settled today".to_owned(),
                        d => format!(" · oldest settled {} ago", plural(d, "day", "days")),
                    },
                    p.text2,
                )],
                Some(d) => vec![part(format!(" · oldest {d}d"), p.text2)],
                None => Vec::new(),
            };
            let n = t.settled.count.worktrees;
            let noun = if wide {
                format!("{n} settled")
            } else {
                n.to_string()
            };
            pool(band, t.settled, noun, "none settled", tail, wide, p)
        }
        PanelRow::Setting(Setting::Inactive) if rules.after_days.is_some() => {
            due(t.idle_week.due, wide, p)
        }
        PanelRow::Setting(Setting::Inactive) => {
            let month = t.idle_month;
            let tail = match (wide, month.worktrees) {
                (false, _) => Vec::new(),
                (true, 0) => vec![part(" · none idle a month+", p.text2)],
                (true, n) => vec![part(
                    format!(" · {n} idle a month+ · {}", bytes(month)),
                    p.text2,
                )],
            };
            let n = t.idle_week.count.worktrees;
            let noun = if wide {
                format!("{n} idle a week+")
            } else {
                format!("{n} idle 7d+")
            };
            let none = if wide {
                "none idle a week+"
            } else {
                "none idle 7d+"
            };
            pool(band, t.idle_week, noun, none, tail, wide, p)
        }
        PanelRow::Setting(Setting::Deleted) if rules.on_delete => due(t.deleted.due, wide, p),
        PanelRow::Setting(Setting::Deleted) => {
            let n = t.deleted.count.worktrees as u64;
            let noun = if wide {
                plural(n, "deleted thread", "deleted threads")
            } else {
                n.to_string()
            };
            pool(
                band,
                t.deleted,
                noun,
                "no deleted threads",
                Vec::new(),
                wide,
                p,
            )
        }
        PanelRow::NoThread => {
            let count: Count = t.no_thread;
            if count.worktrees == 0 {
                return vec![part("none", p.dim)];
            }
            let size = super::status::valued_size(count.bytes, false);
            let size = if count.partial {
                format!("{size}+")
            } else {
                size
            };
            if wide {
                vec![
                    part(
                        format!(
                            "{} · {size}",
                            plural(count.worktrees as u64, "folder", "folders")
                        ),
                        p.fg,
                    ),
                    part(" · press f on it to clean it up", p.text2),
                ]
            } else {
                vec![
                    part(format!("{} · {size}", count.worktrees), p.fg),
                    part(" · never removed", p.text2),
                ]
            }
        }
        _ => what_fixed(row, band, &rules, wide, p),
    }
}

/// The rows that do not depend on the estimate.
fn what_fixed(
    row: PanelRow<'_>,
    band: &Cleanup,
    rules: &Rules,
    wide: bool,
    p: &Palette,
) -> Vec<Part> {
    match row {
        PanelRow::Setting(Setting::Merged) => vec![part(
            if wide {
                "not estimated · T3 checks the pull request itself"
            } else {
                "not estimated"
            },
            p.dim,
        )],
        PanelRow::Setting(Setting::Unchanged) => vec![part(
            if wide {
                "not estimated · needs git"
            } else {
                "not estimated"
            },
            p.dim,
        )],
        PanelRow::Keep => {
            let mut parts = vec![part(
                match (rules.keep_when, wide) {
                    (KeepWhen::UncommittedChanges, true) => {
                        "T3 default · edits and untracked files are kept"
                    }
                    (KeepWhen::UncommittedChanges, false) => "T3 default",
                    (KeepWhen::TrackedChanges, true) => {
                        "untracked and ignored files are deleted, including .env"
                    }
                    (KeepWhen::TrackedChanges, false) => "edits only",
                    (KeepWhen::AnyLocalFiles, true) => {
                        "ignored files count too, except node_modules"
                    }
                    (KeepWhen::AnyLocalFiles, false) => "ignored too",
                },
                p.text2,
            )];
            if rules.keep_when == KeepWhen::AnyLocalFiles && band.global_build {
                parts.push(part(
                    if wide {
                        " · target/ keeps its worktrees"
                    } else {
                        " · target/ keeps"
                    },
                    p.warn,
                ));
            }
            parts
        }
        PanelRow::Project(id, rule) => {
            let rules = rule.rules();
            if !rules.enabled() {
                return vec![part(
                    if wide {
                        "keeps its worktrees until you delete them"
                    } else {
                        "off"
                    },
                    p.text2,
                )];
            }
            let any = rules.keep_when == KeepWhen::AnyLocalFiles;
            if !wide {
                let mut text = rule_tokens(&rules, false, false).join(" · ");
                text.push_str(" · ");
                text.push_str(&keep_text(rules.keep_when, false).replace("keeps ", ""));
                return vec![part(text, if any { p.warn } else { p.text2 })];
            }
            let mut parts = vec![part(
                keep_text(rules.keep_when, true),
                if any { p.warn } else { p.text2 },
            )];
            if any && band.project_build.contains(id) {
                parts.push(part(" · target/ keeps its worktrees", p.text2));
            }
            parts
        }
        _ => Vec::new(),
    }
}

fn name(row: PanelRow<'_>, band: &Cleanup, wide: bool) -> String {
    match (row, wide) {
        (PanelRow::Setting(Setting::Settled), true) => "Delete settled worktrees".into(),
        (PanelRow::Setting(Setting::Inactive), true) => "Delete inactive worktrees".into(),
        (PanelRow::Setting(Setting::Deleted), true) => {
            "Delete worktrees with deleted threads".into()
        }
        (PanelRow::Setting(Setting::Merged), true) => "Delete merged worktrees".into(),
        (PanelRow::Setting(Setting::Unchanged), true) => "Delete unchanged worktrees".into(),
        (PanelRow::Keep, true) => "Keep worktrees with local changes".into(),
        (PanelRow::NoThread, true) => "Folders with no thread in T3".into(),
        (PanelRow::Setting(Setting::Settled), false) => "settled".into(),
        (PanelRow::Setting(Setting::Inactive), false) => "inactive".into(),
        (PanelRow::Setting(Setting::Deleted), false) => "with deleted threads".into(),
        (PanelRow::Setting(Setting::Merged), false) => "merged".into(),
        (PanelRow::Setting(Setting::Unchanged), false) => "unchanged".into(),
        (PanelRow::Keep, false) => "keep local changes".into(),
        (PanelRow::NoThread, false) => "no thread in T3".into(),
        (PanelRow::Project(id, _), _) => terminal::clean(band.title(id)),
    }
}

fn auto_settle(band: &Cleanup) -> String {
    let days = band.auto_settle_days.map(|d| {
        let d = (d * 10.).round() / 10.;
        if d.fract() == 0. {
            plural(d as u64, "idle day", "idle days")
        } else {
            format!("{d} idle days")
        }
    });
    match (days, band.auto_settle_on_merge) {
        (Some(d), true) => format!("by you or by T3 auto-settle (after {d} or a merge)"),
        (Some(d), false) => format!("by you or by T3 auto-settle (after {d})"),
        (None, true) => "by you or by T3 auto-settle after a merge".into(),
        (None, false) => "by you (T3 auto-settle is off)".into(),
    }
}

/// What the selected row does, in T3's terms, and where to change it.
fn explain(row: PanelRow<'_>, band: &Cleanup, p: &Palette) -> (Vec<Part>, Option<Part>) {
    let t = |s: &str| part(s, p.text2);
    let at_where = Some(part(WHERE, p.fg));
    match row {
        PanelRow::Setting(Setting::Settled) => (
            vec![t(&format!(
                "Removes a worktree some days after its thread is settled, {}. \
                 The days count from the later of settling and the thread's last activity. \
                 Branches and commits stay.",
                auto_settle(band)
            ))],
            at_where,
        ),
        PanelRow::Setting(Setting::Inactive) => (
            vec![t(
                "Removes a worktree once its thread has had no activity for that many days: no message, \
                 no run. Archived threads count too. Branches and commits stay.",
            )],
            at_where,
        ),
        PanelRow::Setting(Setting::Deleted) => (
            vec![t(
                "Removes a worktree when you delete its thread, active or archived. No other setting \
                 removes a deleted thread's worktree, so without it the folder stays.",
            )],
            at_where,
        ),
        PanelRow::Setting(Setting::Merged) => (
            vec![t(
                "Removes a worktree once its pull request is merged and its commits are in the default \
                 branch. It needs a remote; T3 checks the pull request itself, so Loupe does not estimate it.",
            )],
            at_where,
        ),
        PanelRow::Setting(Setting::Unchanged) => (
            vec![t(
                "Removes a worktree with no commits beyond the default branch, so nothing in it is \
                 already in the default branch. T3 asks git, which Loupe does not run.",
            )],
            at_where,
        ),
        PanelRow::Keep => (
            vec![t(
                "Every setting above skips a worktree with local changes. Uncommitted changes (T3's \
                 default) keeps edits and untracked files; Edited tracked files keeps edits only; Any \
                 local files also keeps ignored files except node_modules, such as target/.",
            )],
            at_where,
        ),
        PanelRow::Project(id, rule) => {
            let rules = rule.rules();
            let what = if rules.enabled() {
                format!(
                    "{} has its own rule: {}. T3 uses it instead of the settings above.",
                    terminal::clean(band.title(id)),
                    rule_tokens(&rules, true, true).join(" · ")
                )
            } else {
                format!(
                    "{} has its own rule: off. T3 keeps its worktrees until you delete them.",
                    terminal::clean(band.title(id))
                )
            };
            (
                vec![t(&what)],
                Some(part(
                    "T3 Settings › Worktrees, with that project selected",
                    p.fg,
                )),
            )
        }
        PanelRow::NoThread => (
            vec![t(
                "T3 removes only worktrees a thread records, so no setting ever removes these folders. \
                 Select one in Storage and press f to clean it up with a T3 agent.",
            )],
            None,
        ),
    }
}

fn explain_short(row: PanelRow<'_>) -> [&'static str; 2] {
    match row {
        PanelRow::Setting(Setting::Settled) => [
            "Removes a worktree some days after its thread settles.",
            "Set it in T3 Settings › Worktrees.",
        ],
        PanelRow::Setting(Setting::Inactive) => [
            "Removes a worktree after days with no activity.",
            "Set it in T3 Settings › Worktrees.",
        ],
        PanelRow::Setting(Setting::Deleted) => [
            "Removes a worktree when its thread is deleted.",
            "Set it in T3 Settings › Worktrees.",
        ],
        PanelRow::Setting(Setting::Merged) => [
            "Removes a worktree once its pull request merges.",
            "Set it in T3 Settings › Worktrees.",
        ],
        PanelRow::Setting(Setting::Unchanged) => [
            "Removes a worktree with no new commits.",
            "Set it in T3 Settings › Worktrees.",
        ],
        PanelRow::Keep => [
            "Every setting skips worktrees with these changes.",
            "Set it in T3 Settings › Worktrees.",
        ],
        PanelRow::Project(..) => [
            "This project uses its own rule instead.",
            "Set it in T3 Settings › Worktrees, that project.",
        ],
        PanelRow::NoThread => [
            "No T3 setting removes a folder with no thread.",
            "Select it in Storage and press f.",
        ],
    }
}

/// Greedy word wrap that keeps each word's ink.
fn wrap(parts: &[Part], cells: i32) -> Vec<Vec<Part>> {
    let mut lines: Vec<Vec<Part>> = vec![Vec::new()];
    let mut used = 0;
    for (text, ink, bold) in parts {
        for word in text.split(' ').filter(|w| !w.is_empty()) {
            let w = width(word);
            // Punctuation closes the previous word without a space.
            let joined = word.starts_with(['.', ',', ';', ':']);
            let mut gap = i32::from(used > 0 && !joined);
            if used > 0 && used + gap + w > cells {
                lines.push(Vec::new());
                used = 0;
                gap = 0;
            }
            let line = lines.last_mut().expect("one line");
            let text = if gap > 0 {
                format!(" {word}")
            } else {
                word.to_owned()
            };
            used += width(&text);
            match line.last_mut() {
                Some(last) if last.1 == *ink && last.2 == *bold => last.0.push_str(&text),
                _ => line.push((text, *ink, *bold)),
            }
        }
    }
    lines
}

pub(super) fn draw(c: &mut Canvas, p: &Palette, band: &Cleanup, selected: usize, first: usize) {
    let w = c.w as i32;
    let g = Geometry::new(band, c.w, c.h);
    let rows = rows(band);
    let wide = g.wide;
    terminal::section(
        c,
        1,
        1,
        w - 2,
        "T3 CLEANUP SETTINGS",
        p,
        if wide {
            "read from T3's settings.json · Loupe changes nothing"
        } else if w >= 60 {
            "from T3 · read-only"
        } else {
            ""
        },
    );
    let (yours_x, what_x) = if wide { (42, 64) } else { (24, 38) };
    let headings = if wide {
        ["SETTING", "YOURS", "WHAT'S IN YOUR WORKTREES TODAY"]
    } else {
        ["DELETE WORKTREES", "YOURS", "TODAY"]
    };
    c.text(2, g.headings, headings[0], p.dim);
    c.text(yours_x, g.headings, headings[1], p.dim);
    c.text(what_x, g.headings, &fit(headings[2], w - 1 - what_x), p.dim);

    let end = w - 1;
    for (n, line) in g.lines.iter().skip(first).take(g.list_lines).enumerate() {
        let y = g.list_top + n as i32;
        match *line {
            Line::Blank => {}
            Line::Section(title) => {
                c.text(2, y, &fit(title, w - 3), p.dim);
            }
            Line::Row(i) => {
                let row = rows[i];
                let on = i == selected;
                if on {
                    c.fill(0, y, w, 1, p.sel);
                    c.set(0, y, '▌', p.bright, None, false);
                }
                let ink = |color: Rgb| row_ink(p, color, on);
                let inked = |parts: Vec<Part>| -> Vec<Part> {
                    parts.into_iter().map(|(t, i, b)| (t, ink(i), b)).collect()
                };
                draw_parts(
                    c,
                    2,
                    y,
                    &[part(name(row, band, wide), ink(p.fg))],
                    yours_x - 1,
                );
                if what_x < end {
                    draw_parts(
                        c,
                        yours_x,
                        y,
                        &inked(vec![yours(row, band, wide, p)]),
                        what_x - 1,
                    );
                    draw_parts(c, what_x, y, &inked(what(row, band, wide, p)), end);
                }
            }
        }
    }
    let shown_end = first + g.list_lines;
    if first > 0 || shown_end < g.lines.len() {
        let more = g.lines.len().saturating_sub(shown_end);
        let text = match (first > 0, more > 0, wide) {
            (true, true, true) => "▲▼ more",
            (true, false, true) => "▲ more above",
            (false, _, true) => "▼ more below",
            (true, true, false) => "▲▼",
            (true, false, false) => "▲",
            (false, _, false) => "▼",
        };
        c.right(end, g.headings, text, p.dim);
    }

    if let Some(&row) = rows.get(selected) {
        let y = g.explain_top;
        if g.full {
            c.text_bold(2, y, &fit(&name(row, band, true), w - 3), p.fg);
            let (what, at) = explain(row, band, p);
            let mut parts = what;
            if let Some(at) = at {
                parts.push(part("Change it in", p.text2));
                parts.push(at);
                parts.push(part(".", p.text2));
            }
            let mut lines = wrap(&parts, w - 4);
            lines.truncate(3);
            for (i, line) in lines.iter().enumerate() {
                draw_parts(c, 2, y + 1 + i as i32, line, end);
            }
            c.text(
                2,
                y + 1 + lines.len() as i32,
                &fit(
                    "Sizes count WORKTREE only and say \"up to\": T3 still keeps worktrees with local changes.",
                    w - 3,
                ),
                p.dim,
            );
        } else {
            for (i, text) in explain_short(row).into_iter().enumerate() {
                c.text(2, y + i as i32, &fit(text, w - 3), p.text2);
            }
        }
    }

    let keys: &[(&str, &str)] = if wide {
        &[
            ("↑↓", "select"),
            ("f", "set this up with a T3 agent"),
            ("c / Esc", "back"),
            ("q", "quit"),
        ]
    } else {
        &[
            ("↑↓", "select"),
            ("f", "set up in T3"),
            ("c", "back"),
            ("q", "quit"),
        ]
    };
    draw_footer(c, p, keys);
}
