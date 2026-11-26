use super::{Layout, fit, width};
use crate::headroom::{Volume, is_low_space};
use crate::storage_view::data::{Bytes, Inventory, Scan, Shown};
use crate::terminal::{self, BarOpts, Canvas, Palette};
use std::collections::HashSet;

pub(super) struct VolumeLine<'a> {
    role: &'static str,
    temporary: bool,
    label: &'a str,
    volume: Option<&'a Volume>,
}

pub(super) fn volumes(inventory: &Inventory) -> Vec<VolumeLine<'_>> {
    let mut seen = HashSet::new();
    let temp_device = inventory
        .temporary_volume
        .as_ref()
        .and_then(|temp| temp.volume.as_ref())
        .map(|volume| volume.device);
    let mut shared = false;
    let mut lines = Vec::new();
    for worktrees in &inventory.worktree_volumes {
        let volume = worktrees.volume.as_ref();
        if volume.is_some_and(|volume| !seen.insert(volume.device)) {
            continue;
        }
        let same = temp_device.is_some() && volume.map(|v| v.device) == temp_device;
        shared |= same;
        lines.push(VolumeLine {
            role: if same {
                "worktrees + temp"
            } else {
                "worktrees"
            },
            temporary: same,
            label: &worktrees.label,
            volume,
        });
    }
    if let Some(temp) = inventory.temporary_volume.as_ref().filter(|_| !shared) {
        lines.push(VolumeLine {
            role: "temp",
            temporary: true,
            label: &temp.label,
            volume: temp.volume.as_ref(),
        });
    }
    lines
}

pub(super) fn valued_size(bytes: u64, short: bool) -> String {
    let (unit, factor) = [
        ("EB", 1_000_000_000_000_000_000),
        ("PB", 1_000_000_000_000_000),
        ("TB", 1_000_000_000_000),
        ("GB", 1_000_000_000),
        ("MB", 1_000_000),
        ("KB", 1_000),
        ("B", 1),
    ]
    .into_iter()
    .find(|&(_, factor)| bytes >= factor)
    .unwrap_or(("B", 1));
    let n = bytes as f64 / factor as f64;
    let digits = usize::from(factor >= 1_000_000_000 && n < 10.);
    let unit = if short { &unit[..1] } else { unit };
    let gap = if short { "" } else { " " };
    format!("{n:.digits$}{gap}{unit}")
}

pub(super) fn measure(bytes: Bytes, short: bool) -> String {
    match bytes {
        Bytes::Absent | Bytes::Exact(0) => "·".into(),
        Bytes::Exact(n) => valued_size(n, short),
        Bytes::AtLeast(n) => format!("{}+", valued_size(n, short)),
        Bytes::Unknown => "…".into(),
    }
}

pub(super) fn prose_measure(bytes: Bytes) -> String {
    match bytes {
        Bytes::Unknown => "unknown".into(),
        Bytes::AtLeast(n) => format!("at least {}", valued_size(n, false)),
        bytes => valued_size(bytes.measured().unwrap_or_default(), false),
    }
}

// Keep both ends of a mount label so sibling volumes remain distinguishable.
fn mount_label(path: &str, home: &str, cells: i32) -> String {
    let path = match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    };
    if width(&path) <= cells {
        return path;
    }
    let suffix_room = cells / 2;
    let mut suffix_width = 0;
    let mut start = path.len();
    for (index, ch) in path.char_indices().rev() {
        let next = suffix_width + terminal::cell_width(ch) as i32;
        if next > suffix_room {
            break;
        }
        start = index;
        suffix_width = next;
    }
    fit(&path, cells - suffix_width) + &path[start..]
}

pub(super) fn draw_volumes(c: &mut Canvas, p: &Palette, layout: &Layout<'_>, home: &str, demo: bool) {
    let w = layout.w as i32;
    let short = w < 80;
    terminal::section(
        c,
        1,
        1,
        w - 2,
        "VOLUMES",
        p,
        if demo {
            "demo data · made up"
        } else if w >= 100 {
            "now, from the file system"
        } else {
            ""
        },
    );
    if layout.volumes.is_empty() {
        c.text(2, 2, "Headroom unavailable.", p.dim);
    }

    let overflow = layout.volumes.len() > layout.volume_lines;
    let shown = layout.volume_lines - usize::from(overflow);
    let mut visible: Vec<_> = layout.volumes.iter().take(shown).collect();
    if let Some(temp) = layout
        .volumes
        .iter()
        .skip(shown)
        .find(|line| line.temporary)
        && let Some(last) = visible.last_mut()
    {
        *last = temp;
    }
    let labels = visible.iter().map(|line| {
        let Some(volume) = line.volume.filter(|volume| volume.total_bytes > 0) else {
            return ("headroom unavailable  ".to_string(), false);
        };
        let low = is_low_space(volume.available_bytes, volume.total_bytes) == Some(true);
        (
            format!(
                "{} free of {}{}",
                valued_size(volume.available_bytes, short),
                valued_size(volume.total_bytes, short),
                if low { " △" } else { "  " },
            ),
            low,
        )
    });
    let tails: Vec<_> = labels.collect();
    let tail_w = tails.iter().map(|(tail, _)| width(tail)).max().unwrap_or(0);
    for (i, (line, (tail, low))) in visible.into_iter().zip(tails).enumerate() {
        let y = 2 + i as i32;
        let mount_x = (width(line.role) + 3).max(12);
        let meter_end = w - tail_w - 3;
        // Leave 11 columns for the volume kind and 12 for the meter when shown.
        let reserved = if short { 10 } else { 24 };
        let label = mount_label(line.label, home, (meter_end - mount_x - reserved).max(5));
        let memory_x = mount_x + width(&label).max(5) + 1;
        c.text(2, y, line.role, p.fg);
        c.text(mount_x, y, &label, p.text2);
        if let Some(volume) = line.volume {
            let (kind, color) = match volume.is_tmpfs {
                Some(true) => ("in memory", if low { p.warn } else { p.dim }),
                Some(false) => ("disk", p.dim),
                None => ("", p.dim),
            };
            c.text(memory_x, y, &fit(kind, w - tail_w - memory_x - 2), color);
            if !short && volume.total_bytes > 0 {
                let meter_w = (meter_end - (memory_x + 11)).clamp(0, 40);
                let used = volume.total_bytes.saturating_sub(volume.available_bytes);
                terminal::hbar_with(
                    c,
                    meter_end - meter_w,
                    y,
                    meter_w,
                    used as f64 / volume.total_bytes as f64,
                    &BarOpts {
                        track: p.faint,
                        color: &|_, _| p.text2,
                        marker: None,
                        line: true,
                    },
                );
            }
        }
        c.right_with(
            w - 1,
            y,
            &tail,
            if low { p.warn } else { p.text2 },
            None,
            low,
        );
    }
    if overflow {
        c.text(
            2,
            2 + shown as i32,
            &format!(
                "▼ {} more volumes · enlarge to see all",
                layout.volumes.len() - shown
            ),
            p.dim,
        );
    }
}

pub(super) fn empty(inventory: &Inventory) -> bool {
    matches!(inventory.scan, Scan::Finished { incomplete: 0, .. })
        && !inventory.incomplete_coverage
        && inventory
            .projects
            .iter()
            .all(|project| project.threads.is_empty())
        && inventory.settled_or_gone.is_empty()
}

pub(super) fn scan_heading(c: &mut Canvas, p: &Palette, y: i32, inventory: &Inventory) {
    let w = c.w as i32;
    let wide = w >= 100;
    let mut proportion = None;
    let (label, ink) = match inventory.scan {
        Scan::Unavailable => ("unavailable".into(), p.dim),
        Scan::Scanning(progress) => {
            let label = match progress.total {
                Some(total) if progress.done <= total => {
                    if total > 0 {
                        proportion = Some(progress.done as f64 / total as f64);
                    }
                    format!("scanning · {} of {total} folders", progress.done)
                }
                _ if progress.done > 0 => format!("scanning · {} folders read", progress.done),
                _ => "scanning…".into(),
            };
            (label, p.dim)
        }
        Scan::Finished {
            folders,
            incomplete,
            ..
        } if incomplete > 0 => {
            let counts = format!("{incomplete} of {folders}");
            let label = if wide {
                format!("scanned with gaps · {counts} folders not fully read")
            } else {
                format!("scanned · {counts} not fully read")
            };
            (label, p.warn)
        }
        Scan::Finished { .. } if inventory.incomplete_coverage => {
            ("scanned with gaps".into(), p.warn)
        }
        Scan::Finished { .. } if empty(inventory) => ("nothing to measure".into(), p.dim),
        Scan::Finished {
            folders,
            elapsed,
            finished_at,
            ..
        } => {
            let noun = if folders == 1 { "folder" } else { "folders" };
            if let Shown::Kept { at_ms } | Shown::Refreshing { at_ms } = inventory.shown {
                let mut label = format!(
                    "{folders} {noun} · last scan {}",
                    crate::clock::clock(at_ms as f64)
                );
                if matches!(inventory.shown, Shown::Refreshing { .. }) {
                    label.push_str(" · refreshing");
                }
                (label, p.dim)
            } else {
                let mut label = format!("{folders} {noun} · scanned");
                if let Some(ms) = finished_at
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|since_epoch| since_epoch.as_secs_f64() * 1000.)
                {
                    label.push(' ');
                    label.push_str(&crate::clock::clock(ms)[..5]);
                }
                if let Some(elapsed) = elapsed {
                    label.push_str(&format!(" in {:.1} s", elapsed.as_secs_f64()));
                }
                (label, p.dim)
            }
        }
    };
    let title = "BY PROJECT";
    let label = fit(&label, w - width(title) - 5);
    terminal::section_with(c, 1, y, w - 2, title, p, &label, ink);
    if let Some(proportion) = proportion {
        let start = 2 + width(title);
        let end = w - 3 - width(&label);
        let done = ((end - start).max(0) as f64 * proportion).round() as i32;
        for x in start..start + done {
            c.set(x, y, '━', p.fg, None, false);
        }
    }
}
