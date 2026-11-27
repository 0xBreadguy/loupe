use super::{Row, Rows, StorageView};
use crate::model::State;
use crate::storage_view::data::{Inventory, Scan, ThreadState};
use crate::terminal::{self, Canvas, Palette, Rgb};

mod cleanup;
mod detail;
pub(super) mod panel;
mod status;
use status::{VolumeLine, draw_volumes, empty, measure, scan_heading, volumes};

pub(super) struct Layout<'a> {
    w: usize,
    h: usize,
    volumes: Vec<VolumeLine<'a>>,
    volume_lines: usize,
    /// Rows for the T3 CLEANUP band: none when hidden, the rule line alone below height 20.
    band: usize,
    pub list_top: i32,
    detail_top: i32,
    legend: bool,
}

impl<'a> Layout<'a> {
    pub(super) fn new(inventory: &'a Inventory, w: usize, h: usize) -> Self {
        let volumes = volumes(inventory);
        let legend = h >= 30;
        let band = match (&inventory.cleanup, h) {
            (None, _) => 0,
            (Some(_), h) if h < 20 => 1,
            _ => 2,
        };
        let volume_lines = volumes.len().clamp(1, h.saturating_sub(11 + band).max(1));
        Self {
            w,
            h,
            volumes,
            volume_lines,
            band,
            list_top: (4 + volume_lines + band) as i32,
            detail_top: h as i32 - 4 - i32::from(legend),
            legend,
        }
    }

    pub(super) fn fits(&self) -> bool {
        Self::fits_size(self.w, self.h)
    }

    pub(super) fn fits_size(w: usize, h: usize) -> bool {
        w >= 48 && h >= 14
    }

    pub(super) fn lines(&self) -> usize {
        (self.detail_top - 1 - self.list_top).max(0) as usize
    }
}

fn width(text: &str) -> i32 {
    terminal::width(text) as i32
}

fn fit(text: &str, cells: i32) -> String {
    terminal::fit(&terminal::clean(text), cells.max(0) as usize)
}

struct Columns {
    short: bool,
    size_w: i32,
    worktree: i32,
    build: i32,
    scratch: i32,
    name_end: i32,
    share: Option<i32>,
}

impl Columns {
    fn new(w: i32) -> Self {
        let short = w < 80;
        let wide = w >= 100;
        let size_w = if short { 6 } else { 8 };
        let scratch = w - if wide { 17 } else { 1 };
        let build = scratch - size_w - 1;
        let worktree = build - size_w - 1;
        let share = wide.then_some(worktree - size_w - 13);
        Self {
            short,
            size_w,
            worktree,
            build,
            scratch,
            name_end: share.map_or(worktree - size_w - 1, |x| x - 2),
            share,
        }
    }

    fn headings(&self, c: &mut Canvas, p: &Palette, y: i32) {
        let (name, wt, build, scratch) = if self.short {
            ("PROJECT", "WTREE", "└BLD", "SCRCH")
        } else {
            ("PROJECT · THREAD", "WORKTREE", "└ BUILD", "SCRATCH")
        };
        c.text(2, y, name, p.dim);
        c.right(self.worktree, y, wt, p.dim);
        c.right(self.build, y, build, p.dim);
        c.right(self.scratch, y, scratch, p.dim);
        if let Some(x) = self.share {
            c.text(x, y, "SHARE", p.dim);
            c.text(self.scratch + 2, y, "STATE", p.dim);
        }
    }
}

fn activity(state: ThreadState, p: &Palette) -> (char, &'static str, Rgb) {
    match state {
        ThreadState::Running => ('◉', "live", p.bright),
        ThreadState::Quiet => ('◌', "quiet", p.text2),
        ThreadState::NeedsYou => ('◆', "needs you", p.hot),
        ThreadState::Done => ('✓', "done", p.dim),
        ThreadState::Settled => ('✓', "settled", p.dim),
        ThreadState::Gone => ('·', "gone", p.dim),
        ThreadState::Queued => ('◔', "queued", p.dim),
        ThreadState::Failed => ('✗', "failed", p.hot),
        ThreadState::Interrupted => ('⊘', "interrupted", p.dim),
    }
}

fn row_ink(p: &Palette, role: Rgb, selected: bool) -> Rgb {
    if !selected {
        return role;
    }
    if role == p.hot || role == p.warn {
        let target = if p.dark { 0xffffff } else { 0 };
        return crate::integration::reach(role, target, 4.5, &[p.sel]);
    }
    if role == p.dim {
        return p.text2;
    }
    if role == p.text2 {
        return p.fg;
    }
    role
}

/// STATE for a row: a gone folder says whether its thread is archived or deleted.
fn row_state(entry: &crate::storage_view::data::Entry, inventory: &Inventory, p: &Palette) -> String {
    use crate::storage_view::data::Ownership;
    if entry.state == ThreadState::Gone {
        match inventory
            .cleanup
            .as_ref()
            .and_then(|band| band.entries.get(&entry.id))
            .map(|e| e.ownership)
        {
            Some(Ownership::Archived) => return "archived".into(),
            Some(Ownership::Deleted) => return "deleted".into(),
            _ => {}
        }
    }
    titled_state(entry.state, entry.age.as_deref(), p)
}

fn titled_state(state: ThreadState, age: Option<&str>, p: &Palette) -> String {
    let (_, label, _) = activity(state, p);
    match age.filter(|age| !age.is_empty()) {
        Some(age) => format!("{label} {age}"),
        None => label.into(),
    }
}

fn draw_row(
    c: &mut Canvas,
    p: &Palette,
    y: i32,
    row: &Row<'_>,
    cols: &Columns,
    selected: bool,
    largest: u128,
    inventory: &Inventory,
) {
    if selected {
        c.fill(0, y, c.w as i32, 1, p.sel);
        c.set(0, y, '▌', p.bright, None, false);
    }
    let ink = |color| row_ink(p, color, selected);
    let sizes = row.sizes();
    let project = match row {
        Row::Entry { entry, last } => {
            c.text(2, y, if *last { "└─" } else { "├─" }, p.rule);
            let (mark, _, color) = activity(entry.state, p);
            c.set(4, y, mark, ink(color), None, true);
            let role = if matches!(
                entry.state,
                ThreadState::Done | ThreadState::Settled | ThreadState::Gone
            ) {
                p.text2
            } else {
                p.fg
            };
            c.text_with(
                6,
                y,
                &fit(&entry.title, cols.name_end - 6),
                ink(role),
                None,
                selected,
                usize::MAX,
            );
            if cols.share.is_some() {
                let color = if color == p.hot {
                    p.hot
                } else if entry.state == ThreadState::Running {
                    p.fg
                } else {
                    p.dim
                };
                let state = row_state(entry, inventory, p);
                c.text(cols.scratch + 2, y, &fit(&state, 14), ink(color));
            }
            false
        }
        Row::Group(group) => {
            let project = matches!(group.id, super::GroupId::Project(_));
            let role = if project { p.fg } else { p.text2 };
            let count = group.entries.len();
            let noun = match (project, count) {
                (true, 1) => "thread",
                (true, _) => "threads",
                (false, 1) => "folder",
                (false, _) => "folders",
            };
            let marker = if group.folded { " ▸" } else { "" };
            let title = fit(group.title, cols.name_end - 2 - width(marker));
            let x = 2 + c.text_bold(2, y, &title, ink(role));
            let x = x + c.text(x, y, marker, ink(p.dim));
            let suffix = format!(" · {count} {noun}");
            if width(&suffix) <= cols.name_end - x {
                c.text(x, y, &suffix, ink(p.dim));
            } else if group.folded && cols.name_end - x >= 5 {
                let suffix = format!(" · {count}");
                if width(&suffix) <= cols.name_end - x {
                    c.text(x, y, &suffix, ink(p.dim));
                }
            }
            project
        }
        Row::Other(_) => {
            let x = 2 + c.text_bold(2, y, "Other", ink(p.text2));
            let suffix = " · not tied to a thread";
            if width(suffix) <= cols.name_end - x {
                c.text(x, y, suffix, ink(p.dim));
            }
            false
        }
    };

    let measurements = [
        (cols.worktree, sizes.worktree, p.fg),
        (cols.build, sizes.build, p.text2),
        (cols.scratch, sizes.scratch, if matches!(row, Row::Other(_)) { p.dim } else { p.text2 }),
    ];
    for (x, bytes, role) in measurements {
        if x == cols.build && matches!(row, Row::Other(_)) {
            continue;
        }
        let mut text = measure(bytes, cols.short);
        if width(&text) > cols.size_w {
            text = measure(bytes, true);
        }
        if let Some(number) = text.strip_suffix('+') {
            c.right_with(x, y, "+", ink(p.warn), None, true);
            c.right_with(x - 1, y, number, ink(role), None, project);
        } else {
            let muted = matches!(text.as_str(), "…" | "·");
            c.right_with(
                x,
                y,
                &text,
                ink(if muted { p.dim } else { role }),
                None,
                project && !muted,
            );
        }
    }

    if let Some(x) = cols.share.filter(|_| largest > 0) {
        if let Some(bytes) = sizes.footprint().filter(|&bytes| bytes > 0) {
            let fg = if matches!(row, Row::Entry { .. }) {
                p.dim
            } else {
                p.text2
            };
            terminal::hbar(
                c,
                x,
                y,
                12,
                bytes as f64 / largest as f64,
                ink(fg),
                if selected { p.sel } else { p.bg },
            );
        }
    }
}

fn footer_width(keys: &[(&str, &str)]) -> i32 {
    keys.iter()
        .map(|(key, label)| width(key) + width(label) + 3)
        .sum()
}

/// Every key and the "deletes nothing" status fit on the footer line.
fn footer_fits(w: i32, keys: &[(&str, &str)]) -> bool {
    footer_width(keys) + width("deletes nothing") + 2 <= w
}

fn draw_footer(c: &mut Canvas, p: &Palette, keys: &[(&str, &str)]) {
    let w = c.w as i32;
    let y = c.h as i32 - 1;
    let controls_width = footer_width(keys);
    let status = "deletes nothing";
    let room = w - if controls_width + width(status) + 2 <= w {
        width(status) + 2
    } else {
        1
    };
    c.fill(0, y, w, 1, p.panel);
    let mut x = 1;
    for &(key, label) in keys {
        if x + width(key) + width(label) + 1 > room {
            break;
        }
        x += c.text_bold(x, y, key, p.accent) + 1;
        x += c.text(x, y, label, p.dim) + 2;
    }
    if room < w - 1 {
        c.right(w - 1, y, status, p.dim);
    }
}

fn help(c: &mut Canvas, p: &Palette, cleanup: bool) {
    terminal::section(c, 1, 1, c.w as i32 - 2, "STORAGE HELP", p, "");
    let mut keys = vec![
        ("↑↓ / j k", "select a row · mouse wheel moves selection"),
        ("PgUp / PgDn", "move a page · Home / End jump to an end"),
        ("␣ / ⏎", "fold or unfold the selected group"),
        ("⏎ / o", "open in T3 when the thread is available"),
        ("f", "start a T3 thread to inspect & clean up this selection"),
    ];
    if cleanup {
        keys.push((
            "Tab",
            "T3 cleanup settings: each auto-delete setting and what it would cover",
        ));
    }
    keys.extend([
        ("r", "measure again when no scan is running"),
        ("s / Esc", "dashboard · Esc closes this help first"),
    ]);
    let notes_y = 2 + keys.len() as i32;
    for (i, (key, label)) in keys.into_iter().enumerate() {
        let y = 2 + i as i32;
        c.text_bold(2, y, key, p.accent);
        c.text(15, y, &fit(label, c.w as i32 - 16), p.text2);
    }
    let mut notes = vec![
        "WORKTREE is allocated space. BUILD is its build subset.",
        "SCRATCH is owned temp space. Other has no thread owner.",
        "+ marks a lower bound. … means the size is unknown.",
        "This view measures folder sizes and deletes nothing.",
    ];
    if cleanup {
        notes.extend([
            "T3 CLEANUP names T3's auto-delete settings that are off and what they would cover.",
            "Loupe reads T3's settings.json and changes nothing. Set them in T3 Settings › Worktrees.",
            "Estimates count WORKTREE only and say \"up to\": T3 keeps local changes.",
        ]);
    }
    for (i, text) in notes.into_iter().enumerate() {
        let y = notes_y + i as i32;
        if y < c.h as i32 - 1 {
            c.text(2, y, &fit(text, c.w as i32 - 3), p.dim);
        }
    }
    draw_footer(c, p, &[("?", "close help"), ("s", "back"), ("q", "quit")]);
}

pub(super) fn render(
    view: &StorageView,
    rows: &Rows<'_>,
    inventory: &Inventory,
    dashboard: &State,
    layout: Layout<'_>,
    p: &Palette,
) -> Canvas {
    let mut c = Canvas::new(layout.w, layout.h);
    c.clear(p.bg, p.fg);
    if !layout.fits() {
        c.text_bold(1, 0, "Loupe · Storage", p.bright);
        c.text(1, 2, if layout.w < 17 { "Enlarge terminal" } else if layout.w < 42 { "Minimum: 48 × 14" } else { "Enlarge the terminal to at least 48 × 14." }, p.dim);
        c.text(1, 3, "s back   q quit", p.accent);
        return c;
    }

    crate::overview::draw_header(&mut c, dashboard, p, None, true);
    let cleanup = inventory
        .cleanup
        .as_ref()
        .filter(|band| band.panel_available());
    if view.help {
        help(&mut c, p, cleanup.is_some());
        return c;
    }
    if let (Some(state), Some(band)) = (&view.panel, cleanup) {
        panel::draw(&mut c, p, band, state.index, state.first);
        return c;
    }

    let cols = Columns::new(layout.w as i32);
    draw_volumes(&mut c, p, &layout, &dashboard.home, dashboard.demo);
    if let Some(band) = &inventory.cleanup {
        cleanup::draw(&mut c, p, 2 + layout.volume_lines as i32, band, layout.band > 1);
    }
    scan_heading(&mut c, p, layout.list_top - 2, inventory);
    if !rows.items.is_empty() {
        cols.headings(&mut c, p, layout.list_top - 1);
    }

    let capacity = StorageView::capacity(rows.items.len(), layout.lines());
    let largest = if inventory.scan.scanning() {
        0
    } else {
        rows.largest
    };
    let shown = rows.items.iter().skip(view.first).take(capacity);
    let shown_count = shown.len();
    for (n, row) in shown.enumerate() {
        draw_row(
            &mut c,
            p,
            layout.list_top + n as i32,
            row,
            &cols,
            view.first + n == view.index,
            largest,
            inventory,
        );
    }
    let end = view.first + shown_count;
    if end < rows.items.len() || view.first > 0 {
        let below = rows.items.len() - end;
        let gone = rows.items[..view.first]
            .iter()
            .chain(&rows.items[end..])
            .filter(|row| row.gone())
            .count();
        let mut parts = Vec::new();
        if view.first > 0 {
            parts.push(format!("▲ {} above", view.first));
        }
        if below > 0 {
            parts.push(format!("▼ {below} more"));
        }
        if gone > 0 {
            parts.push(format!("{gone} gone"));
        }
        let text = fit(&parts.join(" · "), layout.w as i32 - 3);
        if shown_count < layout.lines() {
            c.text(2, layout.list_top + shown_count as i32, &text, p.text2);
        }
    }

    if let Some(selected) = rows.items.get(view.index) {
        detail::draw(&mut c, p, layout.detail_top, selected, inventory);
    }

    let text: &[&str] = if matches!(inventory.scan, Scan::Unavailable) {
        if dashboard.error.is_some() {
            &[
                "Folder sizes are unavailable.",
                "See the dashboard for T3 source errors.",
            ]
        } else {
            &["Folder sizes are unavailable."]
        }
    } else if empty(inventory) {
        &[
            "No worktrees to measure.",
            "Threads in a project folder share it.",
            "Project folders are excluded.",
        ]
    } else if inventory.scan.scanning() && rows.items.is_empty() {
        &["Waiting for folder measurements…"]
    } else if rows.items.is_empty() {
        &["No folder measurements available."]
    } else {
        &[]
    };
    let message_at = if rows.items.is_empty() {
        layout.list_top
    } else {
        layout.list_top + 2
    };
    for (i, &line) in text
        .iter()
        .take((layout.detail_top - message_at).max(0) as usize)
        .enumerate()
    {
        let line = fit(line, layout.w as i32 - 3);
        c.text(2, message_at + i as i32, &line, p.dim);
    }

    if let Some(notice) = dashboard.notice.as_ref().filter(|_| dashboard.now - dashboard.notice_at < 4000.) {
        let y = layout.h as i32 - 2;
        c.fill(0, y, layout.w as i32, 1, p.bg);
        let ink = if dashboard.notice_error { p.hot } else { p.text2 };
        c.text(1, y, &fit(notice, layout.w as i32 - 2), ink);
    } else if layout.legend {
        let legend = if matches!(rows.items.get(view.index), Some(Row::Other(_))) {
            if layout.w >= 80 {
                "└ BUILD is part of WORKTREE · Other shows unassigned temp under SCRATCH"
            } else {
                "SCRATCH on Other includes unassigned temp"
            }
        } else if layout.w >= 110 {
            "└ BUILD: the part of WORKTREE in target/ and node_modules/ · SCRATCH: the agent's own temp folder"
        } else {
            "└ BUILD: part of WORKTREE · SCRATCH: agent-owned temp"
        };
        c.text(1, layout.h as i32 - 2, legend, p.dim);
    }

    let mut keys = Vec::new();
    if rows.items.len() > 1 {
        keys.push(("↑↓", "select"));
    }
    keys.push(("s", "back"));
    match rows.items.get(view.index) {
        Some(Row::Group(group)) => keys.push(("␣", if group.folded { "unfold" } else { "fold" })),
        Some(row) if row.open_target().is_some() => {
            keys.push(("⏎", if cols.short { "open" } else { "open in T3" }))
        }
        _ => {}
    }
    let settings_at = keys.len();
    if !rows.items.is_empty() {
        keys.push(("f", if cols.short { "clean" } else { "clean in T3" }));
    }
    match inventory.scan {
        Scan::Scanning(_) => {}
        Scan::Unavailable => keys.push(("r", "try again")),
        Scan::Finished { .. } => keys.push(("r", "rescan")),
    }
    keys.extend([("?", "help"), ("q", "quit")]);
    // `Tab` is the first item to go when the footer is narrow.
    if cleanup.is_some() {
        let mut with = keys.clone();
        with.insert(settings_at, ("Tab", "cleanup settings"));
        if footer_fits(layout.w as i32, &with) {
            keys = with;
        }
    }
    draw_footer(&mut c, p, &keys);
    c
}
