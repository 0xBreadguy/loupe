//! The T3 CLEANUP band: a section rule with the state, and one body line in the volume-row
//! style (a role word at x = 2, the value at x = 12).

use super::status::valued_size;
use super::{fit, width};
use crate::storage_view::data::{
    Cleanup, CleanupProblem, CleanupState, Count, Due, Estimate, KeepWhen, Ownership, ProjectRule,
    Rules, Suggestion,
};
use crate::terminal::{self, Canvas, Palette, Rgb};

const WHERE: &str = "T3 Settings › Worktrees";
const LARGE: u64 = 1_000_000_000;

fn plural(n: impl Into<u64>, one: &str, many: &str) -> String {
    let n = n.into();
    format!("{n} {}", if n == 1 { one } else { many })
}

fn bytes(count: Count) -> String {
    let text = valued_size(count.bytes, false);
    if count.partial {
        format!("{text}+")
    } else {
        text
    }
}

/// The status label on the rule line and its ink.
fn label(band: &Cleanup, wide: bool, p: &Palette) -> (String, Rgb) {
    let differ = |n: usize| {
        if wide {
            format!(
                "{} {} own rule",
                plural(n as u64, "project has", "projects have"),
                if n == 1 { "its" } else { "their" }
            )
        } else {
            plural(n as u64, "project differs", "projects differ")
        }
    };
    let heavy = match band.estimate {
        Estimate::Ready(t) => t.now.bytes.max(t.extra.map_or(0, |e| e.bytes)) >= LARGE,
        _ => false,
    };
    match &band.state {
        CleanupState::Off { defaults } => {
            let text = if band.own_rules > 0 {
                format!("off · {}", differ(band.own_rules))
            } else if *defaults {
                (if wide {
                    "off · T3 defaults, no settings file"
                } else {
                    "off · T3 defaults"
                })
                .into()
            } else {
                (if wide {
                    "off · T3 removes no worktrees on its own"
                } else {
                    "off · removes no worktrees"
                })
                .into()
            };
            (text, if heavy { p.warn } else { p.dim })
        }
        CleanupState::On => {
            let text = if band.own_rules > 0 {
                format!("on · {}", differ(band.own_rules))
            } else if wide {
                "on · T3 checks hourly".into()
            } else {
                "on · checked hourly".into()
            };
            (text, p.dim)
        }
        CleanupState::Loose => ("on · loose".into(), p.warn),
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

fn days(n: u16, wide: bool) -> String {
    if wide {
        plural(n, "day", "days")
    } else {
        format!("{n}d")
    }
}

pub(super) fn suggestion_text(suggestion: Suggestion, wide: bool) -> String {
    match (suggestion, wide) {
        (Suggestion::Settled(n), true) => {
            format!("Delete settled worktrees after {}", days(n, true))
        }
        (Suggestion::Inactive(n), true) => {
            format!("Delete inactive worktrees after {}", days(n, true))
        }
        (Suggestion::Settled(n), false) => format!("settled after {}", days(n, true)),
        (Suggestion::Inactive(n), false) => format!("inactive after {}", days(n, true)),
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

type Part = (String, Rgb);

fn draw_parts(c: &mut Canvas, x: i32, y: i32, parts: &[Part], end: i32) -> i32 {
    let mut x = x;
    for (text, ink) in parts {
        let room = end - x;
        if room <= 0 {
            break;
        }
        x += c.text(x, y, &fit(text, room), *ink);
    }
    x
}

fn parts_width(parts: &[Part]) -> i32 {
    parts.iter().map(|(t, _)| width(t)).sum()
}

/// What the estimate says after the arrow, from the longest form to the shortest.
fn off_estimate(band: &Cleanup, suggestion: Suggestion, wide: bool, p: &Palette) -> Vec<Part> {
    let mut forms: Vec<Part> = Vec::new();
    let dim = p.dim;
    match band.estimate {
        Estimate::Estimating => forms.push(("estimating…".into(), dim)),
        Estimate::NoSizes => forms.push((
            if wide {
                "no estimate: folder sizes unavailable"
            } else {
                "no estimate"
            }
            .into(),
            dim,
        )),
        Estimate::NoTimes => forms.push((
            if wide {
                "no estimate: thread times unavailable"
            } else {
                "no estimate"
            }
            .into(),
            dim,
        )),
        Estimate::Ready(t) if t.now.worktrees > 0 => {
            let ink = if t.now.bytes >= LARGE {
                p.warn
            } else {
                p.text2
            };
            if wide {
                forms.push((
                    format!(
                        "up to {} · {} now",
                        bytes(t.now),
                        plural(t.now.worktrees as u64, "worktree", "worktrees")
                    ),
                    ink,
                ));
            }
            forms.push((
                format!("up to {} · {} now", bytes(t.now), t.now.worktrees),
                ink,
            ));
            forms.push((format!("up to {}", bytes(t.now)), ink));
        }
        Estimate::Ready(t) => match t.next {
            Some(next) => {
                if wide {
                    forms.push((
                        format!(
                            "nothing due yet · {} in {}",
                            bytes(next.count),
                            plural(next.days, "day", "days")
                        ),
                        p.text2,
                    ));
                }
                forms.push((
                    format!("none due · {} in {}d", bytes(next.count), next.days),
                    p.text2,
                ));
                forms.push((format!("next in {}d", next.days), p.text2));
            }
            None => {
                let noun = match suggestion {
                    Suggestion::Settled(_) => "settled",
                    Suggestion::Inactive(_) => "idle",
                };
                if wide {
                    forms.push((format!("no {noun} worktrees yet"), dim));
                }
                forms.push(("none yet".into(), dim));
            }
        },
    }
    forms
}

fn on_due(band: &Cleanup, wide: bool, p: &Palette) -> Vec<Part> {
    let mut forms: Vec<Part> = Vec::new();
    match band.estimate {
        Estimate::Estimating => forms.push(("estimating…".into(), p.dim)),
        Estimate::NoSizes | Estimate::NoTimes => forms.push(("no estimate".into(), p.dim)),
        Estimate::Ready(t) if t.now.worktrees > 0 => {
            if wide {
                forms.push((
                    format!(
                        "due now: up to {} · {}",
                        bytes(t.now),
                        plural(t.now.worktrees as u64, "worktree", "worktrees")
                    ),
                    p.text2,
                ));
            }
            forms.push((format!("{} due", bytes(t.now)), p.text2));
        }
        Estimate::Ready(t) => match t.next {
            Some(next) => {
                if wide {
                    forms.push((
                        format!("nothing due · next in {}", plural(next.days, "day", "days")),
                        p.dim,
                    ));
                }
                forms.push((format!("next in {}d", next.days), p.dim));
            }
            None => forms.push(("nothing due".into(), p.dim)),
        },
    }
    forms
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
    match &band.state {
        CleanupState::Unknown(problem) => {
            let parts: Vec<Part> = match problem {
                CleanupProblem::Unreadable(reason) if wide => vec![
                    (
                        format!("T3's settings.json could not be read ({reason}). "),
                        p.text2,
                    ),
                    (format!("Check {WHERE}."), p.dim),
                ],
                CleanupProblem::Unreadable(reason) => vec![
                    (format!("settings.json: {reason}"), p.text2),
                    (" · see T3 Settings".into(), p.dim),
                ],
                CleanupProblem::Invalid { key, constraint } if wide => vec![
                    ((*key).into(), p.fg),
                    (format!(" {constraint}. "), p.text2),
                    (format!("Check {WHERE}."), p.dim),
                ],
                CleanupProblem::Invalid { key, constraint } => {
                    vec![((*key).into(), p.fg), (format!(" {constraint}"), p.text2)]
                }
            };
            draw_parts(c, 2, y, &parts, end);
        }
        CleanupState::Off { .. } => {
            let Some(suggestion) = band.suggestion else {
                c.text(2, y, "rules", p.fg);
                let text = if wide {
                    "every project has its own rule · select a project to see it"
                } else {
                    "each project has its own rule"
                };
                c.text(12, y, &fit(text, end - 12), p.text2);
                return;
            };
            c.text(2, y, "suggest", p.fg);
            let rule: Part = (suggestion_text(suggestion, wide), p.fg);
            let arrow: Part = (if wide { "  →  " } else { " → " }.into(), p.dim);
            let hint: Option<Part> = wide.then(|| {
                if band.custom_root {
                    ("custom worktree folder not scanned".into(), p.warn)
                } else {
                    (WHERE.into(), p.dim)
                }
            });
            let forms = off_estimate(band, suggestion, wide, p);
            let left = |estimate: &Part| vec![rule.clone(), arrow.clone(), estimate.clone()];
            // Keep the rule and the bytes; drop the hint, then shorten the estimate.
            for (i, estimate) in forms.iter().enumerate() {
                let parts = left(estimate);
                let need = 12 + parts_width(&parts);
                if let Some(hint) = &hint
                    && need + 3 + width(&hint.0) <= end
                {
                    draw_parts(c, 12, y, &parts, end);
                    c.right(end, y, &hint.0, hint.1);
                    return;
                }
                if need <= end || i + 1 == forms.len() {
                    draw_parts(c, 12, y, &parts, end);
                    return;
                }
            }
        }
        CleanupState::On => {
            let rules = band.global.unwrap_or_default();
            c.text(2, y, "rules", p.fg);
            let forms = on_due(band, wide, p);
            let due = forms
                .iter()
                .find(|form| 12 + 12 + 3 + width(&form.0) <= end)
                .or(forms.last())
                .cloned()
                .unwrap_or_default();
            let right_x = end - width(&due.0);
            c.right(end, y, &due.0, due.1);
            let mut parts: Vec<Part> = rule_tokens(&rules, wide, false)
                .into_iter()
                .flat_map(|t| [(t, p.text2), (" · ".to_owned(), p.dim)])
                .collect();
            let keep = keep_text(rules.keep_when, wide);
            if band.build_kept {
                parts.push((
                    if wide {
                        format!("{keep} (target/ too)")
                    } else {
                        keep.into()
                    },
                    p.warn,
                ));
            } else {
                parts.push((keep.into(), p.dim));
            }
            draw_parts(c, 12, y, &parts, right_x - 2);
        }
        CleanupState::Loose => {
            let rules = band.global.unwrap_or_default();
            c.text(2, y, "loose", p.warn);
            let extra = match band.estimate {
                Estimate::Ready(t) => t.extra.unwrap_or_default(),
                _ => Count::default(),
            };
            let suggestion = band.suggestion.unwrap_or(Suggestion::Settled(7));
            let current = rule_tokens(&rules, wide, wide).join(" · ");
            let target = if wide {
                format!(
                    "add {}: up to {} more",
                    suggestion_text(suggestion, false),
                    bytes(extra)
                )
            } else {
                let short = match suggestion {
                    Suggestion::Settled(n) => format!("settled {n}d"),
                    Suggestion::Inactive(n) => format!("inactive {n}d"),
                };
                format!("{short}: up to {} more", bytes(extra))
            };
            let arrow = if wide { "  →  " } else { " → " };
            // The target never shortens; the current rules give way.
            let target_w = width(arrow) + width(&target);
            let x = draw_parts(c, 12, y, &[(current, p.text2)], (end - target_w).max(14));
            draw_parts(c, x, y, &[(arrow.into(), p.dim), (target, p.warn)], end);
        }
    }
}
