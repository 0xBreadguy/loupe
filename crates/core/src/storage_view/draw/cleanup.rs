//! The T3 CLEANUP band: a section rule with the state, and one body line in the volume-row
//! style (a role word at x = 2, the value at x = 12, a summary on the right). It names the
//! auto-delete settings that are off and what they would cover; `Tab` opens the settings panel.

use super::status::valued_size;
use super::{fit, width};
use crate::storage_view::data::{
    Cleanup, CleanupProblem, CleanupState, Count, Due, Estimate, KeepWhen, Ownership, ProjectRule,
    Rules,
};
use crate::terminal::{self, Canvas, Palette, Rgb};

const WHERE: &str = "T3 Settings › Worktrees";

pub(super) fn plural(n: impl Into<u64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
}

pub(super) fn bytes(count: Count) -> String {
    let text = valued_size(count.bytes, false);
    if count.partial {
        format!("{text}+")
    } else {
        text
    }
}

/// Text, ink, bold.
pub(super) type Part = (String, Rgb, bool);

pub(super) fn part(text: impl Into<String>, ink: Rgb) -> Part {
    (text.into(), ink, false)
}

pub(super) fn draw_parts(c: &mut Canvas, x: i32, y: i32, parts: &[Part], end: i32) -> i32 {
    let mut x = x;
    for (text, ink, bold) in parts {
        let room = end - x;
        if room <= 0 {
            break;
        }
        x += c.text_with(x, y, &fit(text, room), *ink, None, *bold, usize::MAX);
    }
    x
}

pub(super) fn parts_width(parts: &[Part]) -> i32 {
    parts.iter().map(|(t, ..)| width(t)).sum()
}

/// What the band's label says when every known project keeps its own rule.
fn differ(n: usize, wide: bool) -> String {
    if wide {
        format!(
            "{} {} own rule",
            plural(n as u64, "project has", "projects have"),
            if n == 1 { "its" } else { "their" }
        )
    } else {
        plural(n as u64, "project differs", "projects differ")
    }
}

fn cover(band: &Cleanup) -> Option<Count> {
    match band.estimate {
        Estimate::Ready(t) => Some(t.cover),
        _ => None,
    }
}

/// The status label on the rule line and its ink.
fn label(band: &Cleanup, wide: bool, p: &Palette) -> (String, Rgb) {
    let on = band.settings_on();
    match &band.state {
        CleanupState::Off { .. } | CleanupState::On if band.all_own => (
            format!(
                "{} · {}",
                if on > 0 { "on" } else { "off" },
                differ(band.own_rules, wide)
            ),
            p.dim,
        ),
        CleanupState::Off { .. } => (
            (if wide {
                "off · no auto-delete settings on"
            } else {
                "off · no auto-delete on"
            })
            .into(),
            if cover(band).is_some_and(|c| c.bytes > 0) {
                p.warn
            } else {
                p.dim
            },
        ),
        CleanupState::On => (
            if wide {
                format!("on · {on} of 5 auto-delete settings")
            } else {
                format!("on · {on} of 5 settings")
            },
            p.dim,
        ),
        CleanupState::Unknown(CleanupProblem::Unreadable(_)) => (
            (if wide {
                "unknown · could not read T3's settings"
            } else {
                "unknown · settings unreadable"
            })
            .into(),
            p.warn,
        ),
        CleanupState::Unknown(CleanupProblem::Invalid { .. }) => {
            ("unknown · settings not understood".into(), p.warn)
        }
    }
}

pub(super) fn days(n: u16, wide: bool) -> String {
    if wide {
        plural(n, "day", "days")
    } else {
        format!("{n}d")
    }
}

/// Rule tokens in T3's own words, shortened on narrow terminals.
pub(super) fn rule_tokens(rules: &Rules, wide: bool, keep: bool) -> Vec<String> {
    let mut tokens = Vec::new();
    if let Some(n) = rules.settled_after_days {
        tokens.push(if wide {
            format!("settled after {}", days(n, true))
        } else {
            format!("settled {}", days(n, false))
        });
    }
    if let Some(n) = rules.after_days {
        tokens.push(if wide {
            format!("inactive after {}", days(n, true))
        } else {
            format!("inactive {}", days(n, false))
        });
    }
    if rules.on_merge {
        tokens.push("merged".into());
    }
    if rules.on_delete {
        tokens.push(if wide { "deleted threads" } else { "deleted" }.into());
    }
    if rules.unchanged {
        tokens.push("unchanged".into());
    }
    if keep {
        tokens.push(keep_text(rules.keep_when, wide).into());
    }
    tokens
}

pub(super) fn keep_text(keep: KeepWhen, wide: bool) -> &'static str {
    match (keep, wide) {
        (KeepWhen::UncommittedChanges, true) => "keeps uncommitted changes",
        (KeepWhen::UncommittedChanges, false) => "keeps uncommitted",
        (KeepWhen::TrackedChanges, true) => "keeps edited tracked files",
        (KeepWhen::TrackedChanges, false) => "keeps tracked edits",
        (KeepWhen::AnyLocalFiles, true) => "keeps any local files",
        (KeepWhen::AnyLocalFiles, false) => "keeps any local",
    }
}

/// A project's own rule for the detail pane.
pub(super) fn project_rule(rule: ProjectRule) -> String {
    let rules = rule.rules();
    if rules.enabled() {
        format!(
            "own cleanup rule: {}",
            rule_tokens(&rules, true, true).join(" · ")
        )
    } else {
        "own cleanup rule: off".into()
    }
}

/// The detail pane's words for one entry, while its project's rules are on.
pub(super) fn due_text(due: Due) -> Option<String> {
    Some(match due {
        Due::Now => "T3 cleanup: due now if clean".into(),
        Due::InDays(n) => format!("T3 cleanup: due in {}", plural(n, "day", "days")),
        Due::NotCovered => "T3 cleanup: not covered".into(),
        Due::Busy => "T3 cleanup: kept while busy".into(),
        Due::Outside => "T3 cleanup: outside its worktree folders".into(),
        Due::Never => return None,
    })
}

/// The detail pane's state words for a row with no live thread.
pub(super) fn ownership_text(ownership: Ownership) -> Option<&'static str> {
    match ownership {
        Ownership::Archived => Some("archived thread · T3's rules still apply"),
        Ownership::Deleted => Some("deleted thread · only \"on delete\" removes it"),
        Ownership::NoThread => Some("gone · no thread in T3 · T3 never removes it"),
        _ => None,
    }
}

/// The estimate in place of a number while there is none.
fn no_estimate(band: &Cleanup, wide: bool, p: &Palette) -> Option<Part> {
    Some(match band.estimate {
        Estimate::Ready(_) => return None,
        Estimate::Estimating => part("estimating…", p.dim),
        Estimate::NoSizes if wide => part("no estimate: folder sizes unavailable", p.dim),
        Estimate::NoTimes if wide => part("no estimate: thread times unavailable", p.dim),
        _ => part("no estimate", p.dim),
    })
}

/// Lay out one body line: a role word, the first left form and the first right form that fit
/// together. Forms run from the longest to the shortest; the right side is right-aligned.
fn body_line(c: &mut Canvas, y: i32, role: Part, left: &[Vec<Part>], right: &[Vec<Part>]) {
    let end = c.w as i32 - 1;
    draw_parts(c, 2, y, &[role], 12);
    let shortest = left.last().map_or(0, |f| parts_width(f).min(10));
    let right = right
        .iter()
        .find(|form| 12 + shortest + 2 + parts_width(form) <= end)
        .or(right.last())
        .cloned()
        .unwrap_or_default();
    let right_w = parts_width(&right);
    let right_x = (end - right_w).max(12);
    draw_parts(c, right_x, y, &right, end);
    let room = right_x - 2;
    let left = left
        .iter()
        .find(|form| 12 + parts_width(form) <= room)
        .or(left.last())
        .cloned()
        .unwrap_or_default();
    draw_parts(c, 12, y, &left, room);
}

fn sep(p: &Palette) -> Part {
    part("  ·  ", p.dim)
}

fn key(p: &Palette) -> Vec<Part> {
    vec![("Tab".into(), p.accent, true), part(" settings", p.dim)]
}

/// The right side: the summary forms, each followed by `Tab settings`, then `Tab settings` alone.
fn with_key(p: &Palette, band: &Cleanup, wide: bool, forms: Vec<Vec<Part>>) -> Vec<Vec<Part>> {
    let mut out = Vec::new();
    let custom =
        (wide && band.custom_root).then(|| part("custom worktree folder not scanned", p.warn));
    for form in forms {
        let mut tail = form;
        tail.push(sep(p));
        tail.extend(key(p));
        if let Some(custom) = &custom {
            let mut long = vec![custom.clone(), sep(p)];
            long.extend(tail.iter().cloned());
            out.push(long);
        }
        out.push(tail);
    }
    out.push(key(p));
    out
}

pub(super) fn draw(c: &mut Canvas, p: &Palette, y: i32, band: &Cleanup, body: bool) {
    let w = c.w as i32;
    let wide = w >= 100;
    let (text, ink) = label(band, wide, p);
    let title = "T3 CLEANUP";
    let text = fit(&text, w - width(title) - 5);
    terminal::section_with(c, 1, y, w - 2, title, p, &text, ink);
    if !body {
        return;
    }
    let y = y + 1;
    let end = w - 1;
    let rules = band.global.unwrap_or_default();
    match &band.state {
        CleanupState::Unknown(problem) => {
            let parts: Vec<Part> = match problem {
                CleanupProblem::Unreadable(reason) if wide => vec![
                    part(
                        format!("T3's settings.json could not be read ({reason}). "),
                        p.text2,
                    ),
                    part(format!("Check {WHERE}."), p.dim),
                ],
                CleanupProblem::Unreadable(reason) => vec![
                    part(format!("settings.json: {reason}"), p.text2),
                    part(" · see T3 Settings", p.dim),
                ],
                CleanupProblem::Invalid { key, constraint } if wide => vec![
                    part(*key, p.fg),
                    part(format!(" {constraint}. "), p.text2),
                    part(format!("Check {WHERE}."), p.dim),
                ],
                // Neon's narrow copy: the key, and where to fix it.
                CleanupProblem::Invalid { key, .. } => vec![
                    part(*key, p.fg),
                    part(" invalid", p.text2),
                    part(" · see T3 Settings", p.dim),
                ],
            };
            draw_parts(c, 2, y, &parts, end);
        }
        _ if band.all_own => {
            let left = [
                vec![part(
                    "every project has its own rule · select a project to see it",
                    p.text2,
                )],
                vec![part("each project has its own rule", p.text2)],
            ];
            let left = if wide { &left[..] } else { &left[1..] };
            body_line(c, y, part("rules", p.fg), left, &[key(p)]);
        }
        CleanupState::Off { .. } => {
            let list = "settled · inactive · deleted threads · merged · unchanged";
            let mut left = Vec::new();
            if wide {
                left.push(vec![part(list, p.fg)]);
            }
            left.push(vec![part("5 settings", p.fg)]);
            let right = match (no_estimate(band, wide, p), cover(band)) {
                (Some(none), _) => vec![vec![none]],
                (None, Some(cover)) if cover.worktrees > 0 => {
                    let upto = format!("up to {}", bytes(cover));
                    let mut forms = Vec::new();
                    if wide {
                        forms.push(vec![part(format!("{upto} they would cover"), p.warn)]);
                    }
                    forms.push(vec![part(upto, p.warn)]);
                    forms
                }
                _ => {
                    let mut forms = Vec::new();
                    if wide {
                        forms.push(vec![part("nothing for them to cover yet", p.dim)]);
                    }
                    forms.push(vec![part("nothing yet", p.dim)]);
                    forms
                }
            };
            body_line(
                c,
                y,
                part("unused", p.fg),
                &left,
                &with_key(p, band, wide, right),
            );
        }
        CleanupState::On => {
            let mut left = Vec::new();
            for tokens_wide in [true, false] {
                if tokens_wide && !wide {
                    continue;
                }
                let mut parts: Vec<Part> = rule_tokens(&rules, tokens_wide, false)
                    .into_iter()
                    .flat_map(|t| [part(t, p.text2), part(" · ", p.dim)])
                    .collect();
                let keep = keep_text(rules.keep_when, tokens_wide);
                if band.build_kept {
                    parts.push(part(
                        if tokens_wide {
                            format!("{keep} (target/ too)")
                        } else {
                            keep.into()
                        },
                        p.warn,
                    ));
                } else {
                    parts.push(part(keep, p.dim));
                }
                left.push(parts);
            }
            let unused = 5 - band.settings_on();
            let right = match (no_estimate(band, wide, p), band.estimate) {
                (Some(none), _) => vec![vec![none]],
                (None, Estimate::Ready(t)) => {
                    let due = if t.due.now.worktrees > 0 {
                        part(
                            format!("{} due{}", bytes(t.due.now), if wide { " now" } else { "" }),
                            p.text2,
                        )
                    } else {
                        part("nothing due", p.dim)
                    };
                    let mut forms = Vec::new();
                    if wide && unused > 0 {
                        forms.push(vec![
                            due.clone(),
                            part(format!("  ·  {unused} unused"), p.dim),
                        ]);
                    }
                    forms.push(vec![due]);
                    forms
                }
                _ => Vec::new(),
            };
            body_line(
                c,
                y,
                part("rules", p.fg),
                &left,
                &with_key(p, band, wide, right),
            );
        }
    }
}
