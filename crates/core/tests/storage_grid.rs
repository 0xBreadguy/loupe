use std::time::Duration;
use loupe_core::headroom::Volume;
use loupe_core::model::{Snapshot, State};
use loupe_core::storage_view::data::{
    Bytes, Entry, FolderProgress, Inventory, Project, Scan, Shown, Sizes, ThreadState, VolumePath,
};
use loupe_core::storage_view::{Effect, StorageView};
use loupe_core::terminal::{self, Canvas};

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;

fn entry(title: &str, state: ThreadState, worktree: u64, build: u64, scratch: u64) -> Entry {
    Entry {
        id: title.into(),
        title: title.into(),
        worktree_path: Some(format!("/synthetic/worktrees/{title}").into()),
        sizes: Sizes {
            worktree: Bytes::Exact(worktree),
            build: Bytes::Exact(build),
            scratch: Bytes::Exact(scratch),
        },
        state,
        age: None,
        open_target: (state != ThreadState::Gone).then(|| format!("fixture:{title}")),
    }
}

fn complete(folders: usize, incomplete: usize) -> Scan {
    Scan::Finished {
        folders,
        incomplete,
        elapsed: Some(Duration::from_millis(1400)),
        finished_at: None,
    }
}

fn inventory() -> Inventory {
    let project = |title: &str, threads| Project {
        id: title.into(),
        title: title.into(),
        threads,
    };
    let volume = |label: &str, device, total_bytes, available_bytes, is_tmpfs| VolumePath {
        label: label.into(),
        volume: Some(Volume {
            device,
            available_bytes,
            total_bytes,
            is_tmpfs,
        }),
    };
    Inventory {
        worktree_volumes: vec![volume("/home", 1, 475 * GB, 6200 * MB, Some(false))],
        temporary_volume: Some(volume("/ram", 2, 16 * GB, 1100 * MB, Some(true))),
        scan: complete(9, 0),
        incomplete_coverage: false,
        temp_volume_used: None,
        projects: vec![
            project(
                "Map rendering",
                vec![entry(
                    "Tune labels",
                    ThreadState::Quiet,
                    GB,
                    600 * MB,
                    40 * MB,
                )],
            ),
            project(
                "File export",
                vec![
                    entry(
                        "Handle long exports",
                        ThreadState::Done,
                        2 * GB,
                        1500 * MB,
                        200 * MB,
                    ),
                    entry(
                        "Compare streaming results",
                        ThreadState::Running,
                        3900 * MB,
                        3400 * MB,
                        410 * MB,
                    ),
                    entry(
                        "Choose a retry policy",
                        ThreadState::NeedsYou,
                        1500 * MB,
                        1200 * MB,
                        90 * MB,
                    ),
                ],
            ),
            project(
                "Unicode界",
                vec![entry(
                    "修正 the longest title 一二三四五六七八九十",
                    ThreadState::Queued,
                    400 * MB,
                    50 * MB,
                    0,
                )],
            ),
        ],
        settled_or_gone: vec![
            entry("Old experiment", ThreadState::Settled, GB, 900 * MB, 0),
            entry("tree-3f2", ThreadState::Gone, 1900 * MB, 1400 * MB, 0),
            entry("tree-a12", ThreadState::Gone, 890 * MB, 500 * MB, 0),
        ],
        other: Some(Sizes {
            worktree: Bytes::Exact(300 * MB),
            build: Bytes::Absent,
            scratch: Bytes::Exact(120 * MB),
        }),
        shown: Shown::Current,
        ..Inventory::default()
    }
}

fn frame(view: &mut StorageView, data: &Inventory, w: usize, h: usize) -> Canvas {
    let mut state = State::new(Snapshot::default());
    state.demo = false;
    let theme = loupe_core::theme::builtin(if w < 80 { "paper" } else { "frontier" }).unwrap();
    let palette = loupe_core::integration::derive_themed(theme.text);
    view.render(data, &state, w, h, &palette)
}

fn show(name: &str, canvas: &Canvas) {
    if std::env::var_os("T3_STORAGE_TEST_ANSI").is_some() {
        use loupe_core::recap_view::{Color, paint};
        println!(
            "FRAME {name} {} {}\n{}\nEND FRAME",
            canvas.w,
            canvas.h,
            paint(canvas, Color::True)
        );
    } else {
        println!(
            "FRAME {name} {} {}\n{}\nEND FRAME",
            canvas.w,
            canvas.h,
            canvas.to_text()
        );
    }
    assert!((0..canvas.h).all(|y| terminal::width(&canvas.row_text(y)) <= canvas.w));
}

#[test]
fn synthetic_complete_snapshots_at_three_widths() {
    let data = inventory();
    for (w, h) in [(60, 20), (80, 24), (120, 40)] {
        let mut view = StorageView::default();
        let canvas = frame(&mut view, &data, w, h);
        show("complete", &canvas);
        assert!(canvas.row_text(0).contains("STORAGE"));
        assert!(canvas.row_text(3).contains("in memory") && canvas.row_text(3).contains('△'));
        assert!(canvas.row_text(6).starts_with("▌ File export · 3 threads"));
        assert!(
            canvas
                .row_text(7)
                .starts_with("  ├─◉ Compare streaming results")
        );
        assert!(canvas.row_text(8).starts_with("  ├─✓ Handle long exports"));
        assert!(
            canvas
                .row_text(5)
                .contains(if w == 60 { "└BLD" } else { "└ BUILD" })
        );
        assert!(!canvas.row_text(h - 1).contains("open"));
        assert!(canvas.row_text(h - 1).contains("␣ fold"));
        if w == 60 {
            assert!(canvas.row_text(6).ends_with("7.4G   6.1G   700M"));
            assert!(canvas.to_text().contains("▼ 5 more · 2 gone"));
        }
        if w == 120 {
            assert!(canvas.row_text(5).contains("SHARE"));
            assert!(canvas.row_text(6).contains("████"));
            assert!(canvas.to_text().contains("Settled or gone"));
            assert!(canvas.to_text().contains("Other · not tied to a thread"));
        }
    }
}

#[test]
fn progress_partial_empty_and_unavailable_snapshots() {
    let mut data = inventory();
    let mut view = StorageView::default();
    data.scan = Scan::Scanning(FolderProgress {
        done: 0,
        total: None,
    });
    data.projects[0].threads[0].sizes = Sizes::default();
    let discovering = frame(&mut view, &data, 120, 40);
    show("scanning-before-count", &discovering);
    assert!(discovering.row_text(4).contains("scanning…"));
    assert!(!discovering.row_text(4).contains('━'));
    assert!(discovering.row_text(6).contains('…'));
    assert!(!discovering.row_text(7).contains('█'));
    assert!(!discovering.row_text(39).contains("rescan"));
    assert_eq!(view.key("r", &data, 120, 40), None);
    data.scan = Scan::Scanning(FolderProgress {
        done: 3,
        total: Some(9),
    });
    let counted = frame(&mut view, &data, 80, 24);
    show("scanning-counted", &counted);
    assert!(counted.row_text(4).contains("3 of 9 folders"));
    assert!(counted.row_text(4).contains('━'));

    data.scan = complete(9, 2);
    data.projects[1].threads[1].sizes.worktree = Bytes::AtLeast(3900 * MB);
    view.key("home", &data, 80, 24);
    view.key("down", &data, 80, 24);
    let partial = frame(&mut view, &data, 80, 24);
    show("partial", &partial);
    assert!(partial.row_text(4).contains("2 of 9 not fully read"));
    assert!(partial.row_text(6).contains("7.4 GB+"));
    assert!(partial.row_text(7).contains("3.9 GB+"));
    assert!(partial.row_text(21).contains("at least 3.9 GB"));
    assert_eq!(view.key("r", &data, 80, 24), Some(Effect::Rescan));

    data.scan = Scan::Unavailable;
    let unavailable = frame(&mut view, &data, 80, 24);
    show("unavailable", &unavailable);
    assert!(
        unavailable
            .row_text(6)
            .contains("Folder sizes are unavailable.")
    );
    assert!(unavailable.row_text(3).contains("in memory"));
    assert!(unavailable.row_text(23).contains("r try again"));
    assert!(!unavailable.row_text(23).contains("select"));
    assert_eq!(view.key("enter", &data, 80, 24), None);
    assert_eq!(view.key("r", &data, 80, 24), Some(Effect::Rescan));

    data.scan = complete(0, 0);
    data.projects.clear();
    data.settled_or_gone.clear();
    data.other = None;
    let empty = frame(&mut view, &data, 60, 20);
    show("empty", &empty);
    assert!(empty.row_text(4).contains("nothing to measure"));
    assert!(empty.row_text(6).contains("No worktrees to measure."));
    assert!(!empty.row_text(19).contains("fold"));
}

#[test]
fn selection_folding_scrolling_and_host_effects() {
    let mut data = inventory();
    data.scan = Scan::Scanning(FolderProgress {
        done: 3,
        total: Some(9),
    });
    let mut view = StorageView::default();
    view.key("down", &data, 60, 20);
    assert_eq!(
        view.key("enter", &data, 60, 20),
        Some(Effect::OpenInT3("fixture:Tune labels".into()))
    );
    data.scan = complete(9, 0);
    assert_eq!(
        view.key("enter", &data, 80, 24),
        Some(Effect::OpenInT3("fixture:Tune labels".into())),
        "selection survives sorting and resize"
    );

    view.key("home", &data, 60, 20);
    assert_eq!(view.key("enter", &data, 60, 20), None);
    let folded = frame(&mut view, &data, 60, 20);
    show("folded", &folded);
    assert!(folded.row_text(6).contains("File export ▸ · 3 threads"));
    assert!(!folded.to_text().contains("Compare streaming results"));
    view.key("space", &data, 60, 20);
    assert!(
        frame(&mut view, &data, 60, 20)
            .to_text()
            .contains("Compare streaming results")
    );

    view.key("end", &data, 60, 20);
    let scrolled = frame(&mut view, &data, 60, 20);
    show("scrolled", &scrolled);
    assert!(scrolled.to_text().contains("▲ 5 above"));
    assert!(!scrolled.to_text().contains("▼ 0 more"));
    assert_eq!(view.key("enter", &data, 60, 20), None);
    view.key("up", &data, 60, 20);
    data.settled_or_gone[2].open_target = Some("invalid fixture target".into());
    let gone = frame(&mut view, &data, 120, 40);
    show("gone", &gone);
    assert!(
        gone.row_text(37)
            .contains("no thread in T3 for this folder")
    );
    assert!(!gone.row_text(39).contains("open in T3"));
    assert_eq!(view.key("enter", &data, 120, 40), None);

    view.select_at(&data, 120, 40, 7);
    assert_eq!(
        view.key("o", &data, 120, 40),
        Some(Effect::OpenInT3("fixture:Compare streaming results".into()))
    );
    view.key("?", &data, 60, 20);
    let help = frame(&mut view, &data, 60, 20);
    show("help", &help);
    assert!(help.row_text(1).contains("STORAGE HELP"));
    assert_eq!(view.key("r", &data, 60, 20), None);
    assert_eq!(view.key("escape", &data, 60, 20), None);
    assert_eq!(view.key("s", &data, 60, 20), Some(Effect::Back));
    assert_eq!(view.key("q", &data, 60, 20), Some(Effect::Quit));
}

#[test]
fn volumes_share_by_device_and_unknown_capacity_is_unavailable() {
    let mut data = inventory();
    let shared = data.worktree_volumes[0].volume.clone();
    data.temporary_volume.as_mut().unwrap().volume = shared.clone();
    data.worktree_volumes.push(VolumePath {
        label: "/same".into(),
        volume: shared,
    });
    data.worktree_volumes.push(VolumePath {
        label: "/none".into(),
        volume: Some(Volume {
            device: 3,
            available_bytes: 0,
            total_bytes: 0,
            is_tmpfs: None,
        }),
    });
    let output = frame(&mut StorageView::default(), &data, 80, 24);
    show("shared-volume", &output);
    assert!(output.row_text(2).contains("worktrees + temp"));
    assert!(output.row_text(3).contains("headroom unavailable"));
    assert!(!output.row_text(3).contains('△'));
    assert!(!output.to_text().contains("/same"));
    assert!(!output.to_text().contains("in memory"));
}

#[test]
fn short_view_keeps_temp_headroom_visible_with_many_volumes() {
    let mut data = inventory();
    let original = data.worktree_volumes[0].volume.clone().unwrap();
    for device in 3..6 {
        let mut volume = original.clone();
        volume.device = device;
        data.worktree_volumes.push(VolumePath {
            label: format!("/disk{device}"),
            volume: Some(volume),
        });
    }
    let output = frame(&mut StorageView::default(), &data, 60, 14);
    assert!(output.row_text(3).contains("in memory"));
    assert!(output.row_text(4).contains("3 more volumes"));
}

#[test]
fn volume_remainder_detail_fits_prototype_widths() {
    let mut data = inventory();
    data.set_temp_volume_used(Some(20 * GB));
    for w in [60, 80, 120] {
        let mut view = StorageView::default();
        let canvas = frame(&mut view, &data, w, 40);
        let row = (0..canvas.h).find(|&y| canvas.row_text(y).contains("Other")).unwrap();
        view.select_at(&data, w, 40, row);
        let canvas = frame(&mut view, &data, w, 40);
        assert!(canvas.to_text().contains("capacity now, latest folder scan"));
        assert!(canvas.row_text(38).contains("unassigned temp"));
        show("other-selected", &canvas);
        assert!((0..canvas.h).all(|y| terminal::width(&canvas.row_text(y)) <= w));
    }
}

#[test]
fn long_volume_labels_preserve_the_mount_suffix_and_meter() {
    let mut data = inventory();
    data.worktree_volumes[0].label = "/synthetic/home/long-working-directory/managed/project/with-a-separate-mounted-development-checkout/folder/worktrees".into();
    let mut state = State::new(Snapshot::default());
    state.home = "/synthetic/home".into();
    for w in [60, 80, 120] {
        let mut view = StorageView::default();
        let canvas = view.render(&data, &state, w, 24, &state.palette);
        let line = canvas.row_text(2);
        assert!(line.contains("~/") && line.contains("free of"), "{line}");
        assert!(line.contains("…") && line.contains("trees"), "{line}");
        if w >= 80 {
            let meter = line.chars().filter(|ch| "█▏▎▍▌▋▊▉─".contains(*ch)).count();
            assert!(meter >= 12, "{line}");
        }
        show("long-volume", &canvas);
    }
}

#[test]
fn a_retained_measurement_is_not_labeled_as_freshly_scanned() {
    let mut kept = inventory();
    kept.shown = Shown::Kept { at_ms: 1_795_026_000_000 };
    let mut refreshing = kept.clone();
    refreshing.shown = Shown::Refreshing { at_ms: 1_795_026_000_000 };
    let mut view = StorageView::default();
    let kept_frame = frame(&mut view, &kept, 100, 30);
    let kept_text = (0..kept_frame.h).map(|y| kept_frame.row_text(y)).collect::<Vec<_>>().join("\n");
    assert!(kept_text.contains("last scan"), "{kept_text}");
    assert!(kept_text.contains("1.0 GB"), "{kept_text}");
    assert!(!kept_text.contains("· scanned"), "{kept_text}");
    assert!(!kept_text.contains("refreshing"), "{kept_text}");
    let refreshing_frame = frame(&mut view, &refreshing, 100, 30);
    let refreshing_text = (0..refreshing_frame.h)
        .map(|y| refreshing_frame.row_text(y))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(refreshing_text.contains("last scan"), "{refreshing_text}");
    assert!(refreshing_text.contains("· refreshing"), "{refreshing_text}");
    assert!(refreshing_text.contains("1.0 GB"), "{refreshing_text}");
    assert!(!refreshing_text.contains("· scanned"), "{refreshing_text}");
}
