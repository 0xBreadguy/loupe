//! The low-headroom line, separate from the golden dashboard frames.
//!
//! Measurements are synthetic. `--demo` draws stay on the fixture snapshot.

use std::sync::Arc;
use std::time::Duration;

use t3_agents_core::headroom::{DedicatedTemp, Volume, WarningVolume, warning_volumes};
use t3_agents_core::host::{Clock, Host, poll_host};
use t3_agents_core::model::{Snapshot, State};
use t3_agents_core::terminal::Canvas;
use t3_agents_core::ui::{self, Surface};

const GIB: u64 = 1024 * 1024 * 1024;

fn volume(device: u64, available: u64, total: u64, memory: Option<bool>) -> Volume {
    Volume {
        device,
        available_bytes: available,
        total_bytes: total,
        is_tmpfs: memory,
    }
}

fn tmp_low() -> Volume {
    volume(1, 1_100_000_000, 16_000_000_000, Some(true))
}

fn home_low() -> Volume {
    volume(2, 6_200_000_000, 475_000_000_000, Some(false))
}

fn home_fine() -> Volume {
    volume(2, 345_000_000_000, 475_000_000_000, Some(false))
}

fn tmp_fine() -> Volume {
    volume(1, 12_800_000_000, 16_000_000_000, Some(true))
}

fn both_low() -> Vec<WarningVolume> {
    warning_volumes(
        [(home_low(), "/home".into())],
        Some((tmp_low(), "/tmp".into())),
    )
}

/// Home is fine under the product threshold. Only the memory volume is low.
fn tmp_only() -> Vec<WarningVolume> {
    warning_volumes(
        [(home_fine(), "/home".into())],
        Some((tmp_low(), "/tmp".into())),
    )
}

fn demo() -> State {
    let snap: Snapshot =
        serde_json::from_str(include_str!("../../../checks/fixtures/demo.json")).unwrap();
    let mut state = State::new(snap);
    state.follow = false;
    state
}

fn paint(state: &State, width: usize, height: usize) -> (Canvas, Surface) {
    ui::draw_grid(state, width, height, &state.palette)
}

fn warned_row(width: usize, body: &str) -> String {
    let mut cells = vec![' '; width];
    for (i, ch) in body.chars().enumerate() {
        cells[1 + i] = ch;
    }
    for (i, ch) in "s storage".chars().enumerate() {
        cells[width - 10 + i] = ch;
    }
    cells.into_iter().collect::<String>().trim_end().to_string()
}

fn assert_line(state: &State, width: usize, height: usize, body: &str) {
    let (canvas, _) = paint(state, width, height);
    assert_eq!(
        canvas.row_text(height - 2),
        warned_row(width, body),
        "{width}x{height}"
    );
    let footer = canvas.row_text(height - 1);
    assert!(footer.contains("demo data"), "{footer}");
}

fn assert_same_cells(extra: Vec<WarningVolume>) {
    let plain = demo();
    let mut with = plain.clone();
    with.headroom = extra;
    for (width, height) in [(60usize, 20usize), (80, 24), (120, 40)] {
        let (healthy, healthy_surface) = paint(&plain, width, height);
        let (marked, marked_surface) = paint(&with, width, height);
        assert_eq!(healthy.cells, marked.cells, "{width}x{height}");
        assert_eq!(healthy_surface, marked_surface, "{width}x{height}");
    }
}

struct Tick;
impl Clock for Tick {
    fn instant(&mut self) {}
    fn wake_after(&mut self, _after: Duration) {}
    fn clock(&self) -> f64 {
        0.
    }
}

fn watching(state: &State) -> Host {
    let mut host = Host::new(None, None, None, true, state.snap.at);
    host.watch_headroom = true;
    host
}

#[test]
fn healthy_thresholds_leave_the_dashboard_unchanged() {
    assert_same_cells(Vec::new());
    // Exactly 1 GiB, with the percent rule also quiet.
    assert_same_cells(vec![
        warning_volumes(
            [(volume(3, GIB, 2 * GIB, Some(false)), "/home".into())],
            None,
        )
        .remove(0),
    ]);
    // Exactly 10%, and well above 1 GiB.
    assert_same_cells(vec![
        warning_volumes(
            [(volume(3, 2 * GIB, 20 * GIB, Some(false)), "/home".into())],
            None,
        )
        .remove(0),
    ]);
    // Both boundaries at once.
    assert_same_cells(vec![
        warning_volumes(
            [(volume(3, GIB, 10 * GIB, Some(false)), "/home".into())],
            None,
        )
        .remove(0),
    ]);
    assert_same_cells(vec![
        warning_volumes([(volume(3, 0, 0, Some(false)), "/home".into())], None).remove(0),
    ]);
    assert_same_cells(warning_volumes(
        [(home_fine(), "/home".into())],
        Some((tmp_fine(), "/tmp".into())),
    ));
    let plain = demo();
    for (width, height) in [(60usize, 20usize), (80, 24), (120, 40)] {
        let footer = paint(&plain, width, height).0.row_text(height - 1);
        assert!(footer.contains("demo data"), "{footer}");
    }
}

#[test]
fn prototype_lines_at_60_80_and_120() {
    let frames = [
        (
            60usize,
            20usize,
            false,
            "△ /tmp 1.1G free, in RAM · worktrees 6.2G free",
        ),
        (
            80,
            24,
            false,
            "△ /tmp 1.1 GB free of 16 GB, in memory · worktrees 6.2 GB free",
        ),
        (
            120,
            40,
            false,
            "△ Low space · /tmp 1.1 GB free of 16 GB, in memory · worktrees on /home 6.2 GB free of 475 GB",
        ),
        (60, 20, true, "△ /tmp 1.1 GB free of 16 GB, in memory"),
        (
            80,
            24,
            true,
            "△ Low space · /tmp 1.1 GB free of 16 GB, in memory",
        ),
        (
            120,
            40,
            true,
            "△ Low space · /tmp 1.1 GB free of 16 GB, in memory",
        ),
    ];
    let healthy = demo();
    for (width, height, only_temp, body) in frames {
        let mut state = healthy.clone();
        state.headroom = if only_temp { tmp_only() } else { both_low() };
        let (plain, plain_surface) = paint(&healthy, width, height);
        let (marked, marked_surface) = paint(&state, width, height);
        assert_line(&state, width, height, body);
        assert_eq!(plain.row_text(0), marked.row_text(0), "{width} header");
        assert_eq!(
            plain.row_text(height - 1),
            marked.row_text(height - 1),
            "{width} footer"
        );
        assert_eq!(
            plain_surface.list.h + plain_surface.detail.h,
            marked_surface.list.h + marked_surface.detail.h + 1,
            "{width} reserves the warning row"
        );
        let y = height - 2;
        let palette = &state.palette;
        let triangle = &marked.cells[y * width + 1];
        assert_eq!(triangle.ch, '△');
        assert!(triangle.bold);
        assert_eq!(triangle.fg, palette.warn);
        let at = |needle: &str| {
            let byte = body.find(needle).expect(needle);
            body[..byte].chars().count()
        };
        if body.starts_with("△ Low space · ") {
            let lead = &marked.cells[y * width + 1 + at("·")];
            assert_eq!(lead.ch, '·');
            assert!(lead.bold);
            assert_eq!(lead.fg, palette.warn);
        }
        if let Some(byte) = body.find(" · worktrees") {
            let column = body[..byte].chars().count() + 1;
            let separator = &marked.cells[y * width + 1 + column];
            assert_eq!(separator.ch, '·');
            assert!(!separator.bold);
            assert_eq!(separator.fg, palette.dim);
        }
        let free_cell = &marked.cells[y * width + 1 + at("free")];
        assert_eq!(free_cell.ch, 'f');
        assert!(!free_cell.bold);
        assert_eq!(free_cell.fg, palette.warn);
        let hint = &marked.cells[y * width + width - 10];
        assert_eq!(hint.ch, 's');
        assert!(hint.bold);
        assert_eq!(hint.fg, palette.accent);
        let storage = &marked.cells[y * width + width - 8];
        assert_eq!(storage.ch, 's');
        assert!(!storage.bold);
        assert_eq!(storage.fg, palette.dim);
        assert!(
            marked.cells[y * width..(y + 1) * width]
                .iter()
                .all(|cell| cell.bg == palette.bg)
        );
    }
}

#[test]
fn boundaries_zero_unknown_and_same_volume() {
    let mut under_gib = demo();
    under_gib.headroom = warning_volumes(
        [(volume(4, GIB - 1, 8 * GIB, Some(false)), "/home".into())],
        None,
    );
    let row = paint(&under_gib, 120, 40).0.row_text(38);
    assert!(row.contains('△'), "{row}");
    assert!(row.contains("free"), "{row}");

    let mut under_tenth = demo();
    under_tenth.headroom = warning_volumes(
        [(
            volume(4, 2 * GIB - 1, 20 * GIB, Some(false)),
            "/home".into(),
        )],
        None,
    );
    let row = paint(&under_tenth, 120, 40).0.row_text(38);
    assert!(row.contains('△'), "{row}");
    assert!(row.contains("free"), "{row}");

    let mut empty_disk = demo();
    empty_disk.headroom = warning_volumes(
        Vec::<(Volume, String)>::new(),
        Some((volume(1, 0, 16_000_000_000, Some(true)), "/tmp".into())),
    );
    let row = paint(&empty_disk, 80, 24).0.row_text(22);
    assert!(row.contains("0 KB free"), "{row}");
    assert!(row.contains("in memory"), "{row}");
    assert!(!row.contains("in RAM"), "{row}");

    let mut unknown_kind = demo();
    unknown_kind.headroom = warning_volumes(
        Vec::<(Volume, String)>::new(),
        Some((volume(1, 0, 16_000_000_000, None), "/tmp".into())),
    );
    let row = paint(&unknown_kind, 80, 24).0.row_text(22);
    assert!(row.contains("0 KB free"), "{row}");
    assert!(!row.contains("in memory"), "{row}");
    assert!(!row.contains("in RAM"), "{row}");

    let work = volume(9, 6_200_000_000, 475_000_000_000, Some(false));
    let temp = volume(9, 1_100_000_000, 16_000_000_000, Some(true));
    let folded = warning_volumes(
        [(work, "/home".into())],
        Some((temp.clone(), "/tmp".into())),
    );
    assert_eq!(folded.len(), 1);
    assert!(folded[0].worktrees && folded[0].temporary);
    assert_eq!(folded[0].volume, temp);
    assert!(folded[0].dedicated_temp.is_none());
    assert_eq!(folded[0].worktree_mount, "/home");
    assert_eq!(folded[0].temp_mount, "/tmp");
    let mut shared = demo();
    shared.headroom = folded;
    let row = paint(&shared, 80, 24).0.row_text(22);
    assert_eq!(
        row,
        warned_row(
            80,
            "△ Low space · worktrees on /home 1.1 GB free of 16 GB, in memory"
        )
    );
    assert_eq!(row.matches("free of").count(), 1, "{row}");
    assert!(!row.contains("/tmp"), "{row}");
}

#[test]
fn resize_and_cache_follow_headroom_without_a_new_snapshot() {
    let mut cached = demo();
    let mut plain = demo();
    plain.cache_enabled = false;
    let pointers: Vec<_> = cached.snap.agents.iter().map(Arc::as_ptr).collect();

    let store = |left: &State, right: &State, width, height| {
        let (a, sa) = paint(left, width, height);
        let (b, sb) = paint(right, width, height);
        assert_eq!(a.cells, b.cells, "{width}x{height}");
        assert_eq!(sa, sb, "{width}x{height}");
        (a, sa, sb)
    };
    let (healthy, healthy_surface, plain_surface) = store(&cached, &plain, 80, 24);
    *cached.surface.borrow_mut() = healthy_surface;
    *plain.surface.borrow_mut() = plain_surface;

    cached.headroom = both_low();
    plain.headroom = both_low();
    let (warned, warned_surface, plain_surface) = store(&cached, &plain, 80, 24);
    assert_eq!(
        warned.row_text(22),
        warned_row(
            80,
            "△ /tmp 1.1 GB free of 16 GB, in memory · worktrees 6.2 GB free"
        )
    );
    assert_eq!(healthy.row_text(0), warned.row_text(0));
    assert_eq!(healthy.row_text(23), warned.row_text(23));
    assert_ne!(healthy.row_text(22), warned.row_text(22));
    *cached.surface.borrow_mut() = warned_surface;
    *plain.surface.borrow_mut() = plain_surface;

    for (width, height, body) in [
        (
            60usize,
            20usize,
            "△ /tmp 1.1G free, in RAM · worktrees 6.2G free",
        ),
        (
            120,
            40,
            "△ Low space · /tmp 1.1 GB free of 16 GB, in memory · worktrees on /home 6.2 GB free of 475 GB",
        ),
        (
            80,
            24,
            "△ /tmp 1.1 GB free of 16 GB, in memory · worktrees 6.2 GB free",
        ),
    ] {
        let (canvas, left, right) = store(&cached, &plain, width, height);
        assert_eq!(canvas.row_text(height - 2), warned_row(width, body));
        *cached.surface.borrow_mut() = left;
        *plain.surface.borrow_mut() = right;
    }

    cached.headroom = tmp_only();
    plain.headroom = tmp_only();
    let (canvas, left, right) = store(&cached, &plain, 80, 24);
    assert_eq!(
        canvas.row_text(22),
        warned_row(80, "△ Low space · /tmp 1.1 GB free of 16 GB, in memory")
    );
    *cached.surface.borrow_mut() = left;
    *plain.surface.borrow_mut() = right;

    cached.headroom.clear();
    plain.headroom.clear();
    let (again, _, _) = store(&cached, &plain, 80, 24);
    assert_eq!(again.cells, healthy.cells);
    assert_eq!(
        pointers,
        cached
            .snap
            .agents
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>()
    );
}

#[test]
fn refresh_uses_injected_volumes_on_the_dashboard_interval() {
    let mut state = demo();
    state.interval = 2.;
    let pointers: Vec<_> = state.snap.agents.iter().map(Arc::as_ptr).collect();
    let mut host = watching(&state);
    host.headroom_inject = Some(both_low());
    let mut clock = Tick;

    assert!(poll_host(&mut state, 0., &mut clock, &mut host));
    assert_eq!(state.headroom, both_low());

    host.headroom_inject = Some(tmp_only());
    assert!(!poll_host(&mut state, 1., &mut clock, &mut host));
    assert_eq!(state.headroom, both_low());

    assert!(poll_host(&mut state, 2., &mut clock, &mut host));
    assert_eq!(state.headroom, tmp_only());
    assert_eq!(
        pointers,
        state
            .snap
            .agents
            .iter()
            .map(Arc::as_ptr)
            .collect::<Vec<_>>()
    );

    // Changes in dedicated-temp usage reach consumers even when the warning stays the same.
    let mut dedicated = tmp_only();
    dedicated[0].dedicated_temp = Some(DedicatedTemp {
        used_bytes: Some(3 * GIB),
    });
    host.headroom_inject = Some(dedicated.clone());
    assert!(poll_host(&mut state, 4., &mut clock, &mut host));
    assert_eq!(state.headroom, dedicated);

    let mut paused = demo();
    paused.paused = true;
    paused.headroom = both_low();
    let mut host = watching(&paused);
    host.headroom_inject = Some(tmp_only());
    assert!(!poll_host(&mut paused, 0., &mut clock, &mut host));
    assert_eq!(paused.headroom, both_low());

    let mut quiet = demo();
    quiet.headroom = both_low();
    let mut host = Host::new(None, None, None, true, quiet.snap.at);
    host.headroom_inject = Some(tmp_only());
    assert!(!poll_host(&mut quiet, 0., &mut clock, &mut host));
    assert_eq!(quiet.headroom, both_low());
}

#[test]
fn storage_hint_does_not_bind_s() {
    let mut state = demo();
    state.headroom = both_low();
    let before = state.semantic();
    let headroom = state.headroom.clone();
    state.action_at("s", state.now);
    assert_eq!(state.semantic(), before);
    assert_eq!(state.headroom, headroom);
    assert!(!state.help);
}
