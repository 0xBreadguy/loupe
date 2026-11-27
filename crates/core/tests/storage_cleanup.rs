//! The T3 CLEANUP band in every state, at narrow and wide sizes.
//!
//! Set `LOUPE_CAPTURE_DIR` to a folder to also write each frame there as text, for review.
//! The frames come from synthetic data: no database, no settings file, no terminal.

use loupe_core::cleanup_rules::{self, EntryFacts, RulesRead};
use loupe_core::headroom::Volume;
use loupe_core::model::{Snapshot, State};
use loupe_core::storage_view::data::{
    Bytes, Entry, Inventory, Ownership, Project, Scan, Shown, Sizes, ThreadState, VolumePath,
};
use loupe_core::storage_view::{Effect, StorageView};
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

/// Facts in which nothing is settled or idle for a week.
fn quiet_facts() -> HashMap<String, EntryFacts> {
    let mut map = facts(12., 9.);
    for f in map.values_mut() {
        f.settled_at_ms = None;
        f.activity_ms = Some(NOW - DAY);
        if f.ownership == Ownership::Settled {
            f.ownership = Ownership::Active;
        }
    }
    map
}

const SETTLED_1_ANY: &str = r#"{"worktreeCleanup":{"mode":"custom","rules":{"worktreeAfterDays":null,"worktreeSettledAfterDays":1,"worktreeOnMerge":false,"worktreeOnDelete":false,"worktreeUnchanged":false,"worktreeKeepWhen":"any-local-files"}}}"#;

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "off-they-would-cover",
            data: band(parse("{}"), &facts(12., 9.)),
            wide: [
                "off · no auto-delete settings on",
                "unused    settled · inactive · deleted threads · merged · unchanged    up to 5.0 GB they would cover  ·  Tab settings",
            ],
            narrow: [
                "off · no auto-delete on",
                "unused    5 settings                            up to 5.0 GB  ·  Tab settings",
            ],
        },
        Case {
            name: "off-nothing-yet",
            data: band(RulesRead::Absent, &quiet_facts()),
            wide: [
                "off · no auto-delete settings on",
                "nothing for them to cover yet  ·  Tab settings",
            ],
            narrow: ["off · no auto-delete on", "nothing yet  ·  Tab settings"],
        },
        Case {
            name: "on-due",
            data: band(
                parse(
                    r#"{"storageCleanup":{"worktreeSettledAfterDays":3,"worktreeOnMerge":true}}"#,
                ),
                &facts(12., 9.),
            ),
            wide: [
                "on · 2 of 5 auto-delete settings",
                "rules     settled after 3 days · merged · keeps uncommitted changes      3.0 GB due now  ·  3 unused  ·  Tab settings",
            ],
            narrow: [
                "on · 2 of 5 settings",
                "rules     settled 3d · merged · keeps uncommitt…  3.0 GB due  ·  Tab settings",
            ],
        },
        Case {
            name: "on-nothing-due",
            data: band(
                parse(
                    r#"{"storageCleanup":{"worktreeAfterDays":90,"worktreeKeepWhen":"any-local-files"}}"#,
                ),
                &facts(12., 9.),
            ),
            wide: [
                "on · 1 of 5 auto-delete settings",
                "inactive after 90 days · keeps any local files (target/ too)",
            ],
            narrow: ["on · 1 of 5 settings", "nothing due  ·  Tab settings"],
        },
        Case {
            name: "mixed-overrides",
            data: band(
                parse(&format!(
                    r#"{{"storageCleanup":{{"worktreeSettledAfterDays":3}},"projectSettingsOverrides":{{
                        "Map rendering":{{"worktreeCleanup":{{"mode":"off"}}}},
                        "File export":{SETTLED_1_ANY}}}}}"#
                )),
                &facts(12., 9.),
            ),
            wide: [
                "on · 2 projects have their own rule",
                "rules     every project has its own rule · select a project to see it",
            ],
            narrow: ["on · 2 projects differ", "each project has its own rule"],
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
                "storageCleanup.worktreeAfterDays invalid · see T3 Settings",
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
    assert!(canvas.row_text(4).starts_with("  unused"));
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
    assert!(canvas.row_text(4).contains("estimating…  ·  Tab settings"));
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
        .find(|c| c.name == "on-due")
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

fn palette() -> loupe_core::terminal::Palette {
    loupe_core::integration::derive_themed(loupe_core::theme::builtin("frontier").unwrap().text)
}

/// Open the panel and press `keys`, then render.
fn panel(data: &Inventory, w: usize, h: usize, keys: &[&str]) -> (StorageView, Canvas) {
    let mut view = StorageView::default();
    let state = State::new(Snapshot::default());
    assert_eq!(view.key("tab", data, w, h), None);
    assert!(view.panel_open());
    for key in keys {
        view.key(key, data, w, h);
    }
    let canvas = view.render(data, &state, w, h, &palette());
    (view, canvas)
}

fn line_with<'a>(text: &'a str, start: &str) -> &'a str {
    text.lines()
        .find(|l| l.trim_start_matches('▌').trim_start().starts_with(start))
        .unwrap_or_else(|| panic!("no line starting {start:?} in\n{text}"))
}

fn one_override() -> Inventory {
    band(
        parse(&format!(
            r#"{{"projectSettingsOverrides":{{"Map rendering":{SETTLED_1_ANY}}}}}"#
        )),
        &facts(12., 9.),
    )
}

#[test]
fn panel_off_lists_every_setting_with_what_it_would_cover() {
    let data = one_override();
    let (_, canvas) = panel(&data, 120, 40, &[]);
    capture("panel-off", &canvas);
    let text = canvas.to_text();
    assert!(canvas.row_text(1).starts_with(" T3 CLEANUP SETTINGS ─"));
    assert!(
        canvas
            .row_text(1)
            .trim_end()
            .ends_with("read from T3's settings.json · Loupe changes nothing")
    );
    assert!(canvas.row_text(3).starts_with("  SETTING                                 YOURS                 WHAT'S IN YOUR WORKTREES TODAY"));
    // Map rendering keeps its own rule, so tree-3f2 leaves the global groups.
    let rows = [
        (
            "Delete settled worktrees",
            "Delete settled worktrees                off                   1 settled · 1.1 GB · oldest settled 12 days ago",
        ),
        (
            "Delete inactive worktrees",
            "Delete inactive worktrees               off                   2 idle a week+ · 3.1 GB · none idle a month+",
        ),
        (
            "Delete worktrees with",
            "Delete worktrees with deleted threads   off                   no deleted threads",
        ),
        (
            "Delete merged",
            "Delete merged worktrees                 off                   not estimated · T3 checks the pull request itself",
        ),
        (
            "Delete unchanged",
            "Delete unchanged worktrees              off                   not estimated · needs git",
        ),
        (
            "Keep worktrees",
            "Keep worktrees with local changes       uncommitted changes   T3 default · edits and untracked files are kept",
        ),
        (
            "Map rendering",
            "Map rendering                           settled after 1 day   keeps any local files · target/ keeps its worktrees",
        ),
        (
            "Folders with no thread",
            "Folders with no thread in T3            —                     1 folder · 890 MB · T3 never removes these",
        ),
    ];
    for (start, expect) in rows {
        assert!(
            line_with(&text, start).contains(expect),
            "{}",
            line_with(&text, start)
        );
    }
    assert!(text.contains("  PROJECTS WITH THEIR OWN RULE"));
    assert!(text.contains("  NOT COVERED BY ANY SETTING"));
    assert!(
        canvas.row_text(4).starts_with("▌ Delete settled worktrees"),
        "the first row is selected"
    );
    // The explanation names the selected row and where to change it.
    assert!(
        text.contains("  Delete settled worktrees \n")
            || text
                .lines()
                .filter(|l| l.trim() == "Delete settled worktrees")
                .count()
                == 1
    );
    assert!(text.contains("by you or by T3 auto-settle (after 7 idle days or a"));
    assert!(text.contains("T3 Settings › Worktrees › Automatic worktree cleanup."));
    assert!(text.contains("Sizes count WORKTREE only and say \"up to\""));
    let footer = canvas.row_text(39);
    assert!(
        footer.starts_with(" ↑↓ select  f set this up with a T3 agent  Tab / Esc back  q quit"),
        "{footer}"
    );
    assert!(footer.trim_end().ends_with("deletes nothing"));
}

#[test]
fn panel_with_a_setting_on_shows_what_is_due_and_what_the_others_add() {
    let data = band(
        parse(r#"{"storageCleanup":{"worktreeSettledAfterDays":7}}"#),
        &facts(12., 9.),
    );
    let (_, canvas) = panel(&data, 120, 40, &[]);
    capture("panel-on", &canvas);
    let text = canvas.to_text();
    assert!(
        line_with(&text, "Delete settled").contains("after 7 days          2 due now · 3.0 GB")
    );
    // Handle long exports is idle 20 days but not settled: the inactive setting would add it.
    assert!(
        line_with(&text, "Delete inactive")
            .contains("off                   3 idle a week+ · 5.0 GB · adds 1 · 2.0 GB")
    );

    let mut quiet = facts(12., 9.);
    quiet.get_mut("Handle long exports").unwrap().activity_ms = Some(NOW - DAY);
    let data = band(
        parse(r#"{"storageCleanup":{"worktreeSettledAfterDays":7}}"#),
        &quiet,
    );
    let (_, canvas) = panel(&data, 120, 40, &[]);
    let text = canvas.to_text();
    assert!(
        line_with(&text, "Delete inactive")
            .contains("2 idle a week+ · 3.0 GB · already covered by settled")
    );

    // With the deleted-thread setting off and a deleted thread, nothing else covers it.
    let mut deleted = facts(12., 9.);
    deleted.get_mut("tree-3f2").unwrap().ownership = Ownership::Deleted;
    deleted.get_mut("tree-3f2").unwrap().settled_at_ms = None;
    let data = band(
        parse(r#"{"storageCleanup":{"worktreeSettledAfterDays":7}}"#),
        &deleted,
    );
    let (_, canvas) = panel(&data, 120, 40, &[]);
    let text = canvas.to_text();
    assert!(
        line_with(&text, "Delete worktrees with")
            .contains("1 deleted thread · 1.9 GB · nothing else covers it")
    );
    assert!(line_with(&text, "Delete settled").contains("1 due now · 1.1 GB"));

    // Not due yet: when the next batch comes.
    let data = band(
        parse(r#"{"storageCleanup":{"worktreeSettledAfterDays":30}}"#),
        &facts(12., 9.),
    );
    let (_, canvas) = panel(&data, 120, 40, &[]);
    assert!(
        line_with(&canvas.to_text(), "Delete settled").contains("nothing due · next in 18 days")
    );
}

#[test]
fn panel_keys_move_the_selection_and_close_it() {
    let data = one_override();
    let (mut view, canvas) = panel(&data, 120, 40, &["down"]);
    capture("panel-down", &canvas);
    let text = canvas.to_text();
    assert!(
        canvas
            .row_text(5)
            .starts_with("▌ Delete inactive worktrees")
    );
    assert!(
        text.contains("no activity for that many days"),
        "the explanation follows the selection"
    );
    assert_eq!(view.panel_selection(&data), Some("inactive"));
    // f in the panel is the settings handoff; on the list it is the folder cleanup handoff.
    assert_eq!(
        view.key("f", &data, 120, 40),
        Some(Effect::CleanupSettingsInT3)
    );
    for _ in 0..10 {
        view.key("j", &data, 120, 40);
    }
    assert_eq!(view.panel_selection(&data), Some("no-thread-folders"));
    view.key("k", &data, 120, 40);
    assert_eq!(view.panel_selection(&data), Some("project-rule"));
    // Help opens over the panel and Esc returns to it.
    view.key("?", &data, 120, 40);
    let state = State::new(Snapshot::default());
    assert!(
        view.render(&data, &state, 120, 40, &palette())
            .to_text()
            .contains("STORAGE HELP")
    );
    // Keys pressed over help do not reach the hidden panel, and `s` does not leave.
    assert!(view.help_open());
    for key in ["f", "j", "k", "g", "tab", "enter", "s", "r"] {
        assert_eq!(view.key(key, &data, 120, 40), None, "{key}");
    }
    assert_eq!(view.panel_selection(&data), Some("project-rule"));
    view.key("escape", &data, 120, 40);
    assert!(view.panel_open());
    view.key("escape", &data, 120, 40);
    assert!(!view.panel_open());
    assert_eq!(view.key("f", &data, 120, 40), Some(Effect::CleanInT3));
    // Tab and Shift-Tab switch between the folder list and the panel; `c` is the theme key.
    view.key("tab", &data, 120, 40);
    assert!(view.panel_open());
    view.key("shift-tab", &data, 120, 40);
    assert!(!view.panel_open());
    view.key("shift-tab", &data, 120, 40);
    assert!(view.panel_open());
    view.key("tab", &data, 120, 40);
    assert!(!view.panel_open());
    assert_eq!(view.key("c", &data, 120, 40), None);
    assert!(!view.panel_open());
    view.key("tab", &data, 120, 40);
    // `r` reads it again from the panel too: the settings and the folders.
    assert_eq!(view.key("r", &data, 120, 40), Some(Effect::Rescan));
    assert!(view.panel_open());
    assert_eq!(view.key("q", &data, 120, 40), Some(Effect::Quit));
    assert_eq!(view.key("s", &data, 120, 40), Some(Effect::Back));

    // A click selects the row under it.
    let (mut view, canvas) = panel(&data, 120, 40, &[]);
    let y = (0..canvas.h)
        .find(|&y| canvas.row_text(y).starts_with("  Map rendering"))
        .unwrap();
    view.select_at(&data, 120, 40, y);
    assert_eq!(view.panel_selection(&data), Some("project-rule"));
    view.select_at(&data, 120, 40, 3);
    assert_eq!(
        view.panel_selection(&data),
        Some("project-rule"),
        "headings are inert"
    );

    // Tab does nothing without known settings, without the band, or below the minimum size.
    let unknown = band(RulesRead::Unreadable("x".into()), &facts(12., 9.));
    let mut view = StorageView::default();
    view.key("tab", &unknown, 120, 40);
    assert!(!view.panel_open());
    let mut view = StorageView::default();
    view.key("tab", &inventory(), 120, 40);
    assert!(!view.panel_open());
    let mut view = StorageView::default();
    view.key("tab", &data, 40, 12);
    assert!(!view.panel_open());
}

#[test]
fn panel_pages_with_shift_j_and_k() {
    let data = one_override();
    // 120x14 shows six list lines, so a page (five rows) is less than the whole list.
    let (mut view, _) = panel(&data, 120, 14, &[]);
    view.key("J", &data, 120, 14);
    let paged = view.panel_selection(&data);
    assert_ne!(paged, Some("settled"));
    assert_ne!(
        paged,
        Some("no-thread-folders"),
        "J moves a page, not to the end"
    );
    view.key("K", &data, 120, 14);
    assert_eq!(view.panel_selection(&data), Some("settled"));
    view.key("G", &data, 120, 14);
    assert_eq!(view.panel_selection(&data), Some("no-thread-folders"));
    view.key("g", &data, 120, 14);
    assert_eq!(view.panel_selection(&data), Some("settled"));
}

#[test]
fn storage_footer_offers_tab_and_drops_it_first_when_narrow() {
    let data = one_override();
    let wide = frame(&data, 120, 40);
    assert!(
        wide.row_text(39)
            .contains("␣ fold  Tab cleanup settings  f clean in T3"),
        "{}",
        wide.row_text(39)
    );
    let narrow = frame(&data, 80, 24);
    let footer = narrow.row_text(23);
    assert!(!footer.contains("cleanup settings"), "{footer}");
    assert!(footer.contains("q quit"), "{footer}");
    let plain = frame(&inventory(), 120, 40);
    assert!(!plain.row_text(39).contains("cleanup settings"));
}

#[test]
fn panel_narrow_table_and_every_size_fits() {
    let data = one_override();
    let (_, canvas) = panel(&data, 80, 24, &[]);
    capture("panel-off", &canvas);
    let text = canvas.to_text();
    assert!(
        canvas
            .row_text(1)
            .trim_end()
            .ends_with("from T3 · read-only")
    );
    assert!(
        canvas
            .row_text(2)
            .starts_with("  DELETE WORKTREES      YOURS         TODAY")
    );
    for (start, expect) in [
        (
            "settled",
            "settled               off           1 · 1.1 GB · oldest 12d",
        ),
        (
            "inactive",
            "inactive              off           2 idle 7d+ · 3.1 GB",
        ),
        (
            "with deleted",
            "with deleted threads  off           no deleted threads",
        ),
        (
            "merged",
            "merged                off           not estimated",
        ),
        (
            "keep local",
            "keep local changes    uncommitted   T3 default",
        ),
        (
            "Map rendering",
            "Map rendering         own rule      settled 1d · any local",
        ),
        (
            "no thread",
            "no thread in T3       —             1 · 890 MB · never removed",
        ),
    ] {
        assert!(
            line_with(&text, start).contains(expect),
            "{}",
            line_with(&text, start)
        );
    }
    assert!(
        canvas
            .row_text(21)
            .starts_with("  Removes a worktree some days after its thread settles.")
    );
    assert!(
        canvas
            .row_text(22)
            .starts_with("  Set it in T3 Settings › Worktrees.")
    );
    assert!(
        canvas
            .row_text(23)
            .starts_with(" ↑↓ select  f set up in T3  Esc back  q quit")
    );

    let state = State::new(Snapshot::default());
    let mut all: Vec<Inventory> = cases()
        .into_iter()
        .map(|c| c.data)
        .filter(|d| d.cleanup.as_ref().unwrap().panel_available())
        .collect();
    all.push(data);
    for data in &all {
        for w in [48, 60, 79, 80, 99, 100, 120, 160] {
            for h in [14, 19, 20, 24, 30, 40] {
                let mut view = StorageView::default();
                view.key("tab", data, w, h);
                for step in 0..12 {
                    let canvas = view.render(data, &state, w, h, &palette());
                    assert!(
                        (0..canvas.h).all(|y| terminal::width(&canvas.row_text(y)) <= canvas.w),
                        "{w}x{h}"
                    );
                    let text = canvas.to_text();
                    assert!(text.contains("T3 CLEANUP SETTINGS"), "{w}x{h}");
                    assert_eq!(
                        text.matches('▌').count(),
                        1,
                        "the selection stays visible {w}x{h} step {step}\n{text}"
                    );
                    view.key("down", data, w, h);
                }
            }
        }
        // Below Storage's minimum the panel shows Storage's "too small" screen, and Tab still closes it.
        let mut view = StorageView::default();
        view.key("tab", data, 120, 40);
        let canvas = view.render(data, &state, 40, 12, &palette());
        assert!(canvas.to_text().contains("Minimum: 48 × 14"));
        view.key("tab", data, 40, 12);
        assert!(!view.panel_open());
    }
}
