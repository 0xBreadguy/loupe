//! Headless front door shared by every presenter: argument parsing, the initial
//! state, and the parity/benchmark modes. Nothing here opens a window or a terminal.
use crate::{
    demo, integration,
    model::{Snapshot, State},
    overview, source, terminal, ui,
};
use serde_json::json;
use std::{sync::OnceLock, time::Instant};
pub fn interaction_state(s: &State) {
    if let Some(path) = std::env::var_os("T3_EXPERIMENT_INTERACTION") {
        let _=std::fs::write(path,json!({"selected":s.selected,"scope":s.scope,"rows":s.rows().iter().map(|r|&r.id).collect::<Vec<_>>()}).to_string());
    }
}
pub static SOURCE_PROBLEM: OnceLock<Option<(String, String)>> = OnceLock::new();
pub static SOURCE_ERROR: OnceLock<Option<String>> = OnceLock::new();
pub static INITIAL: OnceLock<Snapshot> = OnceLock::new();
pub fn option(name: &str) -> Option<String> {
    let args: Vec<_> = std::env::args().collect();
    args.windows(2).find(|a| a[0] == name).map(|a| a[1].clone())
}
pub fn init() -> State {
    let mut s = State::new(INITIAL.get().unwrap().clone());
    s.calm = !std::env::args().any(|a| a == "--no-calm");
    s.demo = option("--db").is_none();
    if !s.demo {
        s.usage.clear();
    }
    if let Some(path) = option("--usage") {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(usage) = serde_json::from_slice(&bytes) {
                s.usage = usage;
            }
        }
    }
    if let Some(path) = option("--palette") {
        s.palette = integration::palette(std::path::Path::new(&path))
            .unwrap_or_else(|_| terminal::Palette::fallback());
    }
    s.error = SOURCE_ERROR.get().cloned().flatten();
    if let Some((kind, detail)) = SOURCE_PROBLEM.get().cloned().flatten() {
        s.error_kind = Some(kind);
        s.error_detail = Some(detail);
    }
    if let Some(kind) = option("--error-kind") {
        s.error_kind = Some(kind);
    }
    if let Some(e) = option("--error") {
        s.error = Some(e);
    }
    if let Some(actions) = option("--actions") {
        let grid = option("--grid").unwrap_or("233x55".into());
        let (w, h) = grid.split_once('x').unwrap_or(("233", "55"));
        let w = w.parse().unwrap_or(233);
        let h = h.parse().unwrap_or(55);
        let (_, surface) = ui::draw_grid(&s, w, h, &s.palette);
        *s.surface.borrow_mut() = surface;
        for a in actions.split(',').filter(|a| !a.is_empty()) {
            s.action(a);
            let (_, surface) = ui::draw_grid(&s, w, h, &s.palette);
            *s.surface.borrow_mut() = surface;
        }
    }
    interaction_state(&s);
    s
}
#[derive(Default, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct BehaviorSpec {
    calm: bool,
    boot_at: Option<f64>,
    width: Option<usize>,
    height: Option<usize>,
    usage: Option<Vec<overview::Provider>>,
    events: Vec<BehaviorEvent>,
}
#[derive(Clone, Default, serde::Deserialize)]
#[serde(default)]
struct BehaviorEvent {
    ms: f64,
    reduced: Option<bool>,
    snapshot: Option<Snapshot>,
    pick: Option<String>,
    key: Option<String>,
}
/// Controlled-time behavioral/motion oracle; rendering remains pure and never initializes a window.
fn behavior_frames(
    snap: Snapshot,
    spec: &BehaviorSpec,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let epoch = snap.at;
    let mut latest = snap.clone();
    let mut state = State::new(snap);
    state.calm = spec.calm;
    state.follow = false;
    if let Some(usage) = &spec.usage {
        state.usage = usage.clone();
    }
    state.boot_at = spec.boot_at.unwrap_or(epoch);
    let width = spec.width.unwrap_or(233);
    let height = spec.height.unwrap_or(55);
    if width == 0 || height == 0 || width > 1000 || height > 1000 {
        return Err("behavior grid must be1..1000 in each dimension".into());
    }
    let mut frames = Vec::new();
    let mut events = vec![BehaviorEvent::default()];
    events.extend(spec.events.iter().cloned());
    let mut reduced = false;
    for (index, event) in events.iter().enumerate() {
        let at = epoch + event.ms;
        if let Some(value) = event.reduced {
            reduced = value;
        }
        if let Some(snapshot) = &event.snapshot {
            latest = snapshot.clone();
            state.replace_at(latest.clone(), at);
        }
        if let Some(pick) = event.pick.as_deref() {
            state.pick_at(pick, at);
        }
        if let Some(key) = event.key.as_deref() {
            if key == "r" {
                state.replace_at(latest.clone(), at);
            }
            state.action_at(key, at);
        }
        let (smooth, beat) = state.advance_frame(at, reduced);
        let effective = if reduced && !state.calm {
            let mut s = state.clone();
            s.calm = true;
            s.fx = false;
            Some(s)
        } else {
            None
        };
        let shown = effective.as_ref().unwrap_or(&state);
        let (canvas, surface) = ui::draw_grid(shown, width, height, &state.palette);
        state.detail_scroll = state.detail_scroll.min(surface.detail_max);
        state.help_scroll = state.help_scroll.min(surface.help_max);
        *state.surface.borrow_mut() = surface;
        let mut first: Vec<_> = state
            .first_seen
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        first.sort_by(|a, b| a.0.cmp(&b.0));
        let mut shimmer: Vec<_> = state.shimmer.iter().map(|(k, v)| (k.clone(), *v)).collect();
        shimmer.sort_by(|a, b| a.0.cmp(&b.0));
        let mut tween: Vec<_> = state.tween.iter().map(|(k, v)| (k.clone(), *v)).collect();
        tween.sort_by(|a, b| a.0.cmp(&b.0));
        let mut acked: Vec<_> = state.acked.iter().cloned().collect();
        acked.sort();
        let mut waits: Vec<_> = state.waits.iter().map(|(k, v)| (k.clone(), *v)).collect();
        waits.sort_by(|a, b| a.0.cmp(&b.0));
        let fps = if state.calm || reduced || state.help {
            2
        } else if smooth {
            30
        } else if beat || state.fx {
            10
        } else {
            2
        };
        frames.push(json!({"index":index,"now":state.now,"semantic":state.semantic(),"calm":state.calm,"paused":state.paused,"help":state.help,"fx":state.fx,"follow":state.follow,"windowIndex":state.window_index,"detailScroll":state.detail_scroll,"helpScroll":state.help_scroll,"notice":state.notice.as_ref().map(|text|json!({"text":text,"at":state.notice_at,"error":state.notice_error})),"picked":state.picked.as_ref().map(|(id,at)|json!({"id":id,"at":at})),"firstSeen":first,"shimmer":shimmer,"tween":tween,"acked":acked,"waits":waits,"frozenAt":state.frozen_at,"fps":fps,"fxPosition":(state.fx_elapsed/1000.*0.35)%(height as f64*1.24),"ch":canvas.cells.iter().map(|c|c.codepoint()).collect::<Vec<_>>(),"fg":canvas.cells.iter().map(|c|c.fg).collect::<Vec<_>>(),"bg":canvas.cells.iter().map(|c|c.bg).collect::<Vec<_>>(),"attr":canvas.cells.iter().map(|c|if c.bold {1}else{0}).collect::<Vec<_>>() }));
    }
    Ok(json!({"frames":frames}))
}
/// Flags of the parity, benchmark and capture modes. They are not part of the product
/// command line: when one is present, the product parser stays out of the way. A new
/// experiment mode adds its flag here, or uses one of the reserved prefixes.
pub const EXPERIMENT_FLAGS: &[&str] = &[
    "--snapshot",
    "--layout-rows",
    "--layout-strings",
    "--replay",
    "--demo-snapshot",
    "--demo-epoch",
    "--behavior-script",
    "--dump-snapshot",
    "--semantic",
    "--actions",
    "--grid",
    "--usage",
    "--usage-dir",
    "--palette",
    "--error",
    "--error-kind",
    "--no-calm",
    "--fixed-time",
];
const EXPERIMENT_PREFIXES: &[&str] = &["--bench-", "--probe-", "--frame-"];
/// True when any argument is an experiment flag (`--flag` or `--flag=value`).
pub fn experiment_requested() -> bool {
    std::env::args().skip(1).any(|arg| {
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        EXPERIMENT_FLAGS.contains(&name) || EXPERIMENT_PREFIXES.iter().any(|p| name.starts_with(p))
    })
}
/// Runs a headless mode when the arguments ask for one and returns `true`. Otherwise the
/// initial snapshot and source problem are recorded for [`init`] and the caller presents.
pub fn headless() -> Result<bool, Box<dyn std::error::Error>> {
    if crate::probe::run()? { return Ok(true); }
    if std::env::args().any(|a| a == "--layout-rows") { return crate::cluster::layout_rows().map(|()| true); }
    if std::env::args().any(|a| a == "--layout-strings") { return crate::cluster::layout_strings().map(|()| true); }
    if crate::status::requested() { return crate::status::cli().map(|()| true); }
    if let Some(n) = option("--bench-source") { return crate::bench::source(option("--db"), option("--now-ms"), n.parse()?).map(|_| true); }
    if std::env::args().any(|a| a == "--bench-reader-poll") { return crate::bench::reader_poll(option("--db"), option("--now-ms"), std::env::args().any(|a| a == "--self-check")).map(|_| true); }
    // Scale lab: incremental-reader probes (see checks/bench/incremental-probes.ts).
    if std::env::args().any(|a| a == "--bench-probe-incremental") { return crate::bench::probe_incremental(option("--db"), option("--now-ms")).map(|_| true); }
    if let Some(now) = option("--demo-snapshot") {
        let now: f64 = now.parse()?;
        let epoch: f64 = option("--demo-epoch")
            .unwrap_or_else(|| now.to_string())
            .parse()?;
        println!("{}", serde_json::to_string(&demo::snapshot(now, epoch)?)?);
        return Ok(true);
    }

    let bytes = if let Some(path) = option("--snapshot") {
        std::fs::read_to_string(path)?
    } else {
        include_str!("../../../checks/fixtures/demo.json").to_owned()
    };
    let mut snap: Snapshot = serde_json::from_str(&bytes)?;
    let mut source_error = None;
    let mut source_problem = None;
    if let Some(path) = option("--db") {
        let now = option("--now-ms")
            .map(|v| v.parse())
            .transpose()?
            .unwrap_or_else(crate::clock::wall_now);
        match source::snapshot(std::path::Path::new(&path), now) {
            Ok(live) => snap = live,
            Err(e) => {
                source_error = Some(e.to_string());
                source_problem = Some((e.kind.into(), e.detail.clone()));
                snap = Snapshot {
                    path,
                    at: 0.,
                    agents: vec![],
                };
            }
        }
    }
    if let Some(path) = option("--behavior-script") {
        let spec: BehaviorSpec = serde_json::from_slice(&std::fs::read(path)?)?;
        println!("{}", behavior_frames(snap, &spec)?);
        return Ok(true);
    }
    if std::env::args().any(|a| a == "--bench-model") {
        let n: usize = option("--bench-model")
            .ok_or("missing iterations")?
            .parse()?;
        if n == 0 {
            return Err("iterations must be positive".into());
        }
        let s = State::new(snap);
        for _ in 0..20 {
            std::hint::black_box(serde_json::from_str::<Snapshot>(&bytes)?);
            std::hint::black_box(s.rows());
        }
        let start = Instant::now();
        let mut parse_checksum = 0;
        for _ in 0..n {
            let parsed: Snapshot = serde_json::from_str(std::hint::black_box(&bytes))?;
            parse_checksum += std::hint::black_box(parsed.agents.len());
        }
        let parse_ms = start.elapsed().as_secs_f64() * 1000.;
        let start = Instant::now();
        let mut checksum = 0;
        for _ in 0..n {
            checksum += std::hint::black_box(s.rows()).len();
        }
        let model_ms = start.elapsed().as_secs_f64() * 1000.;
        let rows = s.rows();
        println!(
            "{}",
            json!({"operations":n,"errors":0,"checksum":checksum,"parse_checksum":parse_checksum,"parse_ms":parse_ms,"model_ms":model_ms,"agents":s.snap.agents.len(),"rows":rows.len(),"row_ids":rows.iter().map(|r|&r.id).collect::<Vec<_>>()})
        );
        return Ok(true);
    }
    INITIAL.set(snap).map_err(|_| "init already set")?;
    SOURCE_ERROR.set(source_error).ok();
    SOURCE_PROBLEM.set(source_problem).ok();
    if std::env::args().any(|a| a == "--dump-snapshot") {
        println!("{}", serde_json::to_string(INITIAL.get().unwrap())?);
        return Ok(true);
    }

    if crate::frame_bench::headless()? { return Ok(true); }
    if let Some(n) = option("--bench-draw") {
        let n: usize = n.parse()?;
        if n == 0 || n > 10000 {
            return Err("draw iterations must be1..10000".into());
        }
        let state = init();
        let grid = option("--grid").unwrap_or("233x55".into());
        let (w, h) = grid.split_once('x').ok_or("grid WxH")?;
        let (w, h): (usize, usize) = (w.parse()?, h.parse()?);
        let hash = |c: &terminal::Canvas| {
            let mut h = 5381u32;
            for v in &c.cells {
                for value in [v.ch as u32, v.fg, v.bg, if v.bold { 1 } else { 0 }] {
                    h = h.wrapping_mul(33) ^ value;
                }
            }
            h
        };
        for _ in 0..20 {
            let (c, surface) = ui::draw_grid(&state, w, h, &state.palette);
            *state.surface.borrow_mut() = surface;
            std::hint::black_box(hash(&c));
        }
        let start = Instant::now();
        let mut checksum = 0u64;
        let mut output = 0u32;
        for _ in 0..n {
            let (c, surface) = ui::draw_grid(std::hint::black_box(&state), w, h, &state.palette);
            *state.surface.borrow_mut() = surface;
            output = std::hint::black_box(hash(&c));
            checksum += output as u64;
        }
        let draw_ms = start.elapsed().as_secs_f64() * 1000.;
        println!(
            "{}",
            json!({"operations":n,"errors":0,"draw_ms":draw_ms,"checksum":checksum,"output_hash":format!("{output:08x}"),"rows":state.rows().len(),"columns":w,"lines":h})
        );
        return Ok(true);
    }
    if std::env::args().any(|a| a == "--frame-text" || a == "--frame-json") {
        let state = init();
        let grid = option("--grid").unwrap_or("237x57".into());
        let (w, h) = grid.split_once('x').ok_or("grid WxH")?;
        let (c, _) = ui::draw_grid(&state, w.parse()?, h.parse()?, &state.palette);
        if std::env::args().any(|a| a == "--frame-json") {
            println!(
                "{}",
                json!({"w":c.w,"h":c.h,"text":c.to_text(),"ch":c.cells.iter().map(|v|v.codepoint()).collect::<Vec<_>>(),"fg":c.cells.iter().map(|v|v.fg).collect::<Vec<_>>(),"bg":c.cells.iter().map(|v|v.bg).collect::<Vec<_>>(),"at":c.cells.iter().map(|v|if v.bold{1}else{0}).collect::<Vec<_>>()})
            );
        } else {
            println!("{}", c.to_text());
        }
        return Ok(true);
    }

    if std::env::args().any(|a| a == "--semantic") {
        println!("{}", init().semantic());
        return Ok(true);
    }
    Ok(false)
}
