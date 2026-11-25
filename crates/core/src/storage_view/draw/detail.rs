use super::status::prose_measure;
use super::{Row, activity, fit, titled_state};
use crate::storage_view::data::{Bytes, Inventory};
use crate::terminal::{Canvas, Palette, Rgb};

type Part<'a> = (&'a str, Rgb, bool);

fn detail_line(c: &mut Canvas, y: i32, parts: &[Part<'_>]) {
    let mut x = 1;
    for &(text, ink, bold) in parts {
        let room = c.w as i32 - 1 - x;
        if room <= 0 {
            break;
        }
        x += c.text_with(x, y, &fit(text, room), ink, None, bold, usize::MAX);
    }
}

pub(super) fn draw(c: &mut Canvas, p: &Palette, y: i32, row: &Row<'_>, inventory: &Inventory) {
    match row {
        Row::Group(group) => {
            detail_line(c, y, &[(group.title, p.fg, true)]);
            if matches!(group.id, super::super::GroupId::SettledOrGone) {
                detail_line(
                    c,
                    y + 1,
                    &[(
                        "Worktrees for settled threads or folders with no thread in T3.",
                        p.text2,
                        false,
                    )],
                );
            } else {
                let count = group.entries.len();
                let line = format!(
                    "{count} {} with a worktree",
                    if count == 1 { "thread" } else { "threads" }
                );
                detail_line(
                    c,
                    y + 1,
                    &[
                        (&line, p.text2, false),
                        (" · project folder excluded", p.dim, false),
                    ],
                );
            }
            let fold = if group.folded {
                "␣ or ⏎ unfolds it"
            } else {
                "␣ or ⏎ folds it"
            };
            let clean_hint = if matches!(group.id, super::super::GroupId::SettledOrGone) {
                " · f clean in T3"
            } else {
                " · sizes are allocated bytes"
            };
            detail_line(
                c,
                y + 2,
                &[
                    (fold, p.dim, false),
                    (
                        clean_hint,
                        if matches!(group.id, super::super::GroupId::SettledOrGone) {
                            p.accent
                        } else {
                            p.dim
                        },
                        false,
                    ),
                ],
            );
        }
        Row::Entry { entry, .. } => {
            let path = entry
                .worktree_path
                .as_ref()
                .map(|path| format!("  {}", path.display()));
            detail_line(
                c,
                y,
                &[
                    (&entry.title, p.fg, true),
                    (path.as_deref().unwrap_or(""), p.dim, false),
                ],
            );

            let sizes = entry.sizes;
            if inventory.scan.scanning() && sizes.worktree == Bytes::Unknown {
                detail_line(c, y + 1, &[("measuring…", p.dim, false)]);
            } else {
                let worktree = match sizes.worktree {
                    Bytes::Unknown => "worktree size unknown".into(),
                    Bytes::Absent => "no worktree".into(),
                    bytes => format!("{} on disk", prose_measure(bytes)),
                };
                let ink = if matches!(sizes.worktree, Bytes::AtLeast(_)) {
                    p.warn
                } else {
                    p.fg
                };
                let build = match sizes.build {
                    Bytes::Absent => String::new(),
                    Bytes::Unknown => " · build size unknown".into(),
                    bytes => format!(" · {} of it build", prose_measure(bytes)),
                };
                let scratch = match sizes.scratch {
                    Bytes::Absent | Bytes::Exact(0) => String::new(),
                    Bytes::Unknown => " · scratch size unknown".into(),
                    bytes => format!(" · {} scratch", prose_measure(bytes)),
                };
                detail_line(
                    c,
                    y + 1,
                    &[
                        (&worktree, ink, ink == p.warn),
                        (&build, p.text2, false),
                        (&scratch, p.text2, false),
                    ],
                );
            }

            let (_, _, color) = activity(entry.state, p);
            let state = if row.gone() {
                "gone · no thread in T3 for this folder".into()
            } else {
                titled_state(entry.state, entry.age.as_deref(), p)
            };
            let open = if row.open_target().is_some() {
                " · ⏎ opens it in T3"
            } else if matches!(
                entry.state,
                crate::storage_view::data::ThreadState::Gone
                    | crate::storage_view::data::ThreadState::Settled
            ) {
                " · f clean in T3"
            } else {
                ""
            };
            detail_line(c, y + 2, &[(&state, color, false), (open, p.dim, false)]);
        }
        Row::Other(sizes) => {
            detail_line(c, y, &[("Other · not tied to a thread", p.fg, true)]);
            let temp = inventory.temporary_volume.as_ref();
            let label = temp.map_or("temp", |temp| temp.label.as_str());
            let memory = if temp
                .and_then(|path| path.volume.as_ref())
                .and_then(|volume| volume.is_tmpfs)
                == Some(true)
            {
                ", in memory"
            } else {
                ""
            };
            let derived = inventory.displayed_other().1;
            let scratch = if derived {
                format!(
                    "{} volume remainder · capacity now, latest folder scan",
                    prose_measure(sizes.scratch)
                )
            } else {
                match sizes.scratch {
                    Bytes::Absent => "No unattributed scratch".into(),
                    Bytes::Unknown => format!("Unattributed space in {label} is unknown"),
                    bytes => format!(
                        "{} measured unassigned scratch in {label}{memory}",
                        prose_measure(bytes)
                    ),
                }
            };
            let worktree = match sizes.worktree {
                Bytes::Absent | Bytes::Exact(0) => "No unattributed worktree folders".into(),
                Bytes::Unknown => "Unattributed worktree space is unknown".into(),
                bytes => format!("{} in unattributed worktree folders", prose_measure(bytes)),
            };
            detail_line(c, y + 1, &[(&scratch, p.text2, false)]);
            detail_line(c, y + 2, &[(&worktree, p.text2, false)]);
        }
    }
}
