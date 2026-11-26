//! The T3 CLEANUP band in every state, at narrow and wide sizes.
//!
//! Set `LOUPE_CAPTURE_DIR` to a folder to also write each frame there as text, for review.
//! The frames come from synthetic data: no database, no settings file, no terminal.

use loupe_core::cleanup_rules::{self, EntryFacts, RulesRead};
use loupe_core::headroom::Volume;
use loupe_core::model::{Snapshot, State};
use loupe_core::storage_view::StorageView;
use loupe_core::storage_view::data::{
    Bytes, Entry, Inventory, Ownership, Project, Scan, Shown, Sizes, ThreadState, VolumePath,
};
use loupe_core::terminal::{self, Canvas};
use std::collections::HashMap;
use std::time::Duration;

const GB: u64 = 1_000_000_000;
const MB: u64 = 1_000_000;
const NOW: f64 = 1_795_026_000_000.;
const DAY: f64 = 86_400_000.;

fn entry(id: &str, state: ThreadState, worktree: u64, build: u64, scratch: u64) -> Entry {
    Entry {
        id: id.into(),
        title: id.into(),
        worktree_path: Some(format!("/synthetic/worktrees/{id}").into()),
        sizes: Sizes {
            worktree: Bytes::Exact(worktree),
            build: Bytes::Exact(build),
            scratch: Bytes::Exact(scratch),
        },
        state,
        age: None,
        open_target: (state != ThreadState::Gone).then(|| format!("fixture:{id}")),
    }
}

fn inventory() -> Inventory {
    let project = |id: &str, threads| Project {
        id: id.into(),
        title: id.into(),
        threads,
    };
    Inventory {
        worktree_volumes: vec![VolumePath {
            label: "/home".into(),
            volume: Some(Volume {
                device: 1,
                available_bytes: 62 * GB,
                total_bytes: 475 * GB,
                is_tmpfs: Some(false),
            }),
        }],
        scan: Scan::Finished {
            folders: 8,
            incomplete: 0,
            elapsed: Some(Duration::from_millis(1400)),
            finished_at: None,
        },
        projects: vec![
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
                "Map rendering",
                vec![entry(
                    "Tune labels",
                    ThreadState::Quiet,
                    GB,
                    600 * MB,
                    40 * MB,
                )],
            ),
        ],
        settled_or_gone: vec![
            entry(
                "Old experiment",
                ThreadState::Settled,
                1100 * MB,
                900 * MB,
                0,
            ),
            entry("tree-3f2", ThreadState::Gone, 1900 * MB, 1400 * MB, 0),
            entry("unowned tree-a12", ThreadState::Gone, 890 * MB, 500 * MB, 0),
        ],
        settled_projects: [
            ("Old experiment".to_owned(), "File export".to_owned()),
            ("tree-3f2".to_owned(), "Map rendering".to_owned()),
        ]
        .into(),
        shown: Shown::Current,
        ..Inventory::default()
    }
}

/// Settled ages in days for the settled thread and the archived folder.
fn facts(settled: f64, archived: f64) -> HashMap<String, EntryFacts> {
    let f = |project: &str, ownership, idle, activity: f64, settled: Option<f64>| EntryFacts {
        project_id: Some(project.into()),
        ownership,
        idle,
        managed: true,
        activity_ms: Some(NOW - activity * DAY),
        settled_at_ms: settled.map(|d| NOW - d * DAY),
    };
    [
        (
            "Handle long exports",
            f("File export", Ownership::Active, true, 20., None),
        ),
        (
            "Compare streaming results",
            f("File export", Ownership::Active, false, 0., None),
        ),
        (
            "Choose a retry policy",
            f("File export", Ownership::Active, false, 0., None),
        ),
        (
            "Tune labels",
            f("Map rendering", Ownership::Active, false, 0., None),
        ),
        (
            "Old experiment",
            f(
                "File export",
                Ownership::Settled,
                true,
                settled,
                Some(settled),
            ),
        ),
        (
            "tree-3f2",
            f(
                "Map rendering",
                Ownership::Archived,
                true,
                archived + 1.,
                Some(archived),
            ),
        ),
        (
            "unowned tree-a12",
            EntryFacts {
                project_id: None,
                ..f("", Ownership::NoThread, true, 99., None)
            },
        ),
    ]
    .into_iter()
    .map(|(id, f)| (id.to_owned(), f))
    .collect()
}

fn band(read: RulesRead, facts: &HashMap<String, EntryFacts>) -> Inventory {
    let mut data = inventory();
    let projects = ["File export".to_owned(), "Map rendering".to_owned()];
    data.cleanup = Some(cleanup_rules::summarize(
        &read,
        &data,
        Some(facts),
        &projects,
        false,
        NOW,
    ));
    data
}

fn frame(data: &Inventory, w: usize, h: usize) -> Canvas {
    let mut state = State::new(Snapshot::default());
    state.demo = false;
    let theme = loupe_core::theme::builtin("frontier").unwrap();
    let palette = loupe_core::integration::derive_themed(theme.text);
    StorageView::default().render(data, &state, w, h, &palette)
}

fn capture(name: &str, canvas: &Canvas) {
    assert!((0..canvas.h).all(|y| terminal::width(&canvas.row_text(y)) <= canvas.w));
    if let Some(dir) = std::env::var_os("LOUPE_CAPTURE_DIR") {
        let path = std::path::Path::new(&dir).join(format!("{name}-{}x{}.txt", canvas.w, canvas.h));
        std::fs::write(path, canvas.to_text() + "\n").unwrap();
    }
}

fn parse(json: &str) -> RulesRead {
    cleanup_rules::parse(json)
}

struct Case {
    name: &'static str,
    data: Inventory,
    /// Expected (rule line, body line) fragments at 120 and at 80 columns.
    wide: [&'static str; 2],
    narrow: [&'static str; 2],
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "off-suggestion",
            data: band(parse("{}"), &facts(12., 9.)),
            wide: [
                "off · T3 removes no worktrees on its own",
                "suggest   Delete settled worktrees after 7 days  →  up to 3.0 GB · 2 worktrees now",
            ],
            narrow: [
                "off · removes no worktrees",
                "suggest   settled after 7 days → up to 3.0 GB · 2 now",
            ],
        },
        Case {
            name: "off-nothing-due",
            data: band(RulesRead::Absent, &facts(2., 3.)),
            wide: [
                "off · T3 defaults, no settings file",
                "→  nothing due yet · 1.9 GB in 4 days",
            ],
            narrow: ["off · T3 defaults", "→ none due · 1.9 GB in 4d"],
        },
        Case {
            name: "on-adequate",
            data: band(
                parse(
                    r#"{"storageCleanup":{"worktreeSettledAfterDays":3,"worktreeOnMerge":true}}"#,
                ),
                &facts(12., 9.),
            ),
            wide: [
                "on · T3 checks hourly",
                "rules     settled after 3 days · merged · keeps uncommitted changes",
            ],
            narrow: ["on · checked hourly", "3.0 GB due"],
        },
        Case {
            name: "on-loose",
            data: band(
                parse(
                    r#"{"storageCleanup":{"worktreeAfterDays":90,"worktreeKeepWhen":"any-local-files"}}"#,
                ),
                &facts(12., 9.),
            ),
            wide: [
                "on · loose",
                "inactive after 90 days · keeps any local files  →  add settled after 7 days: up to 3.0 GB more",
            ],
            narrow: ["on · loose", "inactive 90d → settled 7d: up to 3.0 GB more"],
        },
        Case {
            name: "mixed-overrides",
            data: band(
                parse(
                    r#"{"storageCleanup":{"worktreeSettledAfterDays":3},"projectSettingsOverrides":{
                        "Map rendering":{"worktreeCleanup":{"mode":"off"}},
                        "File export":{"worktreeCleanup":{"mode":"custom","rules":{"worktreeAfterDays":null,"worktreeSettledAfterDays":1,"worktreeOnMerge":false,"worktreeOnDelete":false,"worktreeUnchanged":false,"worktreeKeepWhen":"any-local-files"}}}}}"#,
                ),
                &facts(12., 9.),
            ),
            wide: [
                "on · 2 projects have their own rule",
                "rules     settled after 3 days · keeps uncommitted changes      ",
            ],
            narrow: ["on · 2 projects differ", "1.1 GB due"],
        },
        Case {
            name: "unknown-unreadable",
            data: band(
                RulesRead::Unreadable("symlink not followed".into()),
                &facts(12., 9.),
            ),
            wide: [
                "unknown · could not read T3's settings",
                "T3's settings.json could not be read (symlink not followed). Check T3 Settings › Worktrees.",
            ],
            narrow: [
                "unknown · settings unreadable",
                "settings.json: symlink not followed · see T3 Settings",
            ],
        },
        Case {
            name: "unknown-invalid",
            data: band(
                parse(r#"{"storageCleanup":{"worktreeAfterDays":0}}"#),
                &facts(12., 9.),
            ),
            wide: [
                "unknown · settings not understood",
                "storageCleanup.worktreeAfterDays is not a whole number from 1 to 3650 or null.",
            ],
            narrow: [
                "unknown · settings not understood",
                "storageCleanup.worktreeAfterDays is not a whole",
            ],
        },
    ]
}

#[test]
fn every_band_state_at_80_and_120_columns() {
    for case in cases() {
        for (w, h, expect) in [(120, 40, case.wide), (80, 24, case.narrow)] {
            let canvas = frame(&case.data, w, h);
            capture(&format!("band-{}", case.name), &canvas);
            let rule = canvas.row_text(3);
            let body = canvas.row_text(4);
            assert!(rule.starts_with(" T3 CLEANUP ─"), "{}: {rule}", case.name);
            assert!(
                rule.trim_end().ends_with(expect[0]),
                "{} {w}: {rule}",
                case.name
            );
            assert!(body.contains(expect[1]), "{} {w}: {body}", case.name);
            assert!(
                canvas.row_text(5).starts_with(" BY PROJECT"),
                "{}",
                case.name
            );
            assert!(
                canvas.row_text(7).starts_with("▌ File export"),
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn hidden_band_keeps_the_old_layout_and_short_terminals_keep_only_the_rule() {
    let plain = inventory();
    let canvas = frame(&plain, 80, 24);
    assert!(canvas.row_text(3).starts_with(" BY PROJECT"));
    assert!(!canvas.to_text().contains("T3 CLEANUP"));

    let data = band(parse("{}"), &facts(12., 9.));
    for h in [14, 18, 19] {
        let canvas = frame(&data, 80, h);
        capture("band-short", &canvas);
        assert!(canvas.row_text(3).starts_with(" T3 CLEANUP"), "{h}");
        assert!(canvas.row_text(4).starts_with(" BY PROJECT"), "{h}");
    }
    let canvas = frame(&data, 80, 20);
    assert!(canvas.row_text(4).starts_with("  suggest"));
}

#[test]
fn every_state_fits_every_width_and_height() {
    let mut all: Vec<Inventory> = cases().into_iter().map(|c| c.data).collect();
    let mut estimating = inventory();
    estimating.projects.clear();
    estimating.settled_or_gone.clear();
    estimating.scan = Scan::Scanning(Default::default());
    estimating.cleanup = Some(cleanup_rules::summarize(
        &RulesRead::Absent,
        &estimating,
        None,
        &[],
        true,
        NOW,
    ));
    all.push(estimating);
    for data in &all {
        for w in [48, 60, 79, 80, 99, 100, 120, 160] {
            for h in [14, 19, 20, 24, 40] {
                let canvas = frame(data, w, h);
                assert!((0..canvas.h).all(|y| terminal::width(&canvas.row_text(y)) <= canvas.w));
                let text = canvas.to_text();
                assert!(text.contains("T3 CLEANUP"), "{w}x{h}");
            }
        }
    }
    let canvas = frame(all.last().unwrap(), 120, 40);
    capture("band-estimating", &canvas);
    assert!(canvas.row_text(4).contains("→  estimating…"));
    assert!(
        canvas
            .row_text(4)
            .contains("custom worktree folder not scanned")
    );
}

#[test]
fn detail_pane_shows_own_rules_ownership_and_due() {
    let mixed = cases()
        .into_iter()
        .find(|c| c.name == "mixed-overrides")
        .unwrap()
        .data;
    let mut view = StorageView::default();
    let state = State::new(Snapshot::default());
    let palette = loupe_core::integration::derive_themed(
        loupe_core::theme::builtin("frontier").unwrap().text,
    );
    let render = |view: &mut StorageView| view.render(&mixed, &state, 120, 40, &palette);
    let canvas = render(&mut view);
    let detail = canvas.row_text(36);
    assert!(
        detail.contains("own cleanup rule: settled after 1 day · keeps any local files"),
        "{detail}"
    );

    // Down to "Handle long exports" (Done, File export: settled after 1 day; never settled).
    for _ in 0..2 {
        view.key("down", &mixed, 120, 40);
    }
    let canvas = render(&mut view);
    capture("band-detail-entry", &canvas);
    let text = canvas.to_text();
    assert!(text.contains("T3 cleanup: not covered"), "{text}");

    let on = cases()
        .into_iter()
        .find(|c| c.name == "on-adequate")
        .unwrap()
        .data;
    let mut view = StorageView::default();
    // Rows by size: the archived tree-3f2 is the first entry of Settled or gone.
    let rows = [
        "File export",
        "Compare",
        "Handle long",
        "Choose",
        "Map rendering",
        "Tune",
        "Settled or gone",
        "tree-3f2",
    ];
    for _ in 0..rows.len() - 1 {
        view.key("down", &on, 120, 40);
    }
    let canvas = view.render(&on, &state, 120, 40, &palette);
    let text = canvas.to_text();
    assert!(
        text.contains("archived thread · T3's rules still apply · T3 cleanup: due now if clean"),
        "{text}"
    );
    assert!(text.contains("archived"));
}

#[test]
fn keeping_any_local_files_is_flagged_when_build_output_would_keep_worktrees() {
    let data = band(
        parse(
            r#"{"storageCleanup":{"worktreeSettledAfterDays":3,"worktreeKeepWhen":"any-local-files"}}"#,
        ),
        &facts(12., 9.),
    );
    assert!(data.cleanup.as_ref().unwrap().build_kept);
    let wide = frame(&data, 120, 40);
    capture("band-on-any-local", &wide);
    assert!(
        wide.row_text(4)
            .contains("settled after 3 days · keeps any local files (target/ too)")
    );
    let narrow = frame(&data, 80, 24);
    assert!(narrow.row_text(4).contains("settled 3d · keeps any local"));
}
