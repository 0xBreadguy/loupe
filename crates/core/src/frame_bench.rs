//! Deterministic interactive frame scoreboard. Parsing and warmup are excluded;
//! input, snapshot replacement, composition and every output cell are included.
use crate::{
    cli,
    model::{Changed, Event, State},
    ui,
};
use std::time::Instant;
pub fn headless() -> Result<bool, Box<dyn std::error::Error>> {
    let Some(workload) = cli::option("--frame-workload") else {
        return Ok(false);
    };
    let n: usize = cli::option("--frames").unwrap_or("300".into()).parse()?;
    if n == 0 {
        return Err("frames must be positive".into());
    }
    let grid = cli::option("--grid").unwrap_or("233x55".into());
    let (w, h) = grid.split_once('x').ok_or("grid WxH")?;
    let (w, h) = (w.parse()?, h.parse()?);
    let mut s = cli::init();
    s.follow = false;
    let draw = |s: &State, chain: &mut u32| {
        let (c, surface) = ui::draw_grid(s, w, h, &s.palette);
        *s.surface.borrow_mut() = surface;
        for cell in c.cells {
            for field in [cell.codepoint(), cell.fg, cell.bg, u32::from(cell.bold)] {
                *chain = chain.wrapping_mul(33) ^ field;
            }
        }
    };
    for _ in 0..20 {
        draw(&s, &mut 5381);
    }
    let epoch = s.now;
    let mut chain = 5381u32;
    let start = Instant::now();
    for i in 0..n {
        let update_start = Instant::now();
        match workload.as_str() {
            "repeat" => {}
            "navigate" => s.action(if i % 2 == 0 { "down" } else { "up" }),
            "scroll" => s.action(if i + 2 == n {
                "end"
            } else if i + 1 == n {
                "home"
            } else {
                "down"
            }),
            "clock" => s.now = epoch + (i + 1) as f64 * 100.,
            "clock10s" => s.now = epoch + (i + 1) as f64 * 10000.,
            // The TS draw already includes tween/acknowledgement/host bookkeeping.
            "host" => {
                s.advance_frame(epoch + (i + 1) as f64 * 100., false);
            }
            "delta" | "burst" => {
                let mut snap = s.snap.clone();
                let len = snap.agents.len();
                let count = if workload == "burst" {
                    len.div_ceil(100)
                } else {
                    usize::from(len > 0)
                };
                let mut ids = Vec::with_capacity(count);
                for j in 0..count {
                    let a = snap.agent_mut((i * count + j) % len);
                    ids.push(a.id.clone());
                    a.status = if a.status == "running" {
                        "completed"
                    } else {
                        "running"
                    }
                    .into();
                    a.events.push(Event {
                        t: epoch,
                        cat: "shell".into(),
                        failed: false,
                    });
                }
                s.apply(snap, &Changed::Agents(ids));
            }
            "structural" => {
                let mut snap = s.snap.clone();
                let mut ids = Vec::new();
                match i % 3 {
                    0 => {
                        if let Some(first) = snap.agents.first() {
                            let mut a = (**first).clone();
                            a.id = format!("frame-added-{i}");
                            a.parent = None;
                            ids.push(a.id.clone());
                            snap.agents.push(std::sync::Arc::new(a));
                        }
                    }
                    1 => {
                        if let Some(a) = snap.agents.pop() {
                            ids.push(a.id.clone());
                        }
                    }
                    _ => {
                        if snap.agents.len() > 1 {
                            let parent = snap.agents[0].id.clone();
                            let a = snap.agent_mut(1);
                            a.parent = if a.parent.as_ref() == Some(&parent) {
                                None
                            } else {
                                Some(parent)
                            };
                            ids.push(a.id.clone());
                        }
                    }
                }
                s.apply(snap, &Changed::Agents(ids));
            }
            "replace" => {
                let mut snap = s.snap.clone();
                if !snap.agents.is_empty() {
                    let len = snap.agents.len();
                    let a = std::sync::Arc::make_mut(&mut snap.agents[i % len]);
                    a.status = if a.status == "running" {
                        "completed"
                    } else {
                        "running"
                    }
                    .into();
                    a.events.push(Event {
                        t: epoch,
                        cat: "shell".into(),
                        failed: false,
                    });
                }
                s.replace(snap);
            }
            _ => return Err("unknown workload".into()),
        }
        if std::env::var_os("T3_FRAME_PROFILE").is_some() {
            eprintln!("input_us {}", update_start.elapsed().as_micros());
        }
        draw(&s, &mut chain);
    }
    println!(
        "{}",
        serde_json::json!({"operations":n,"errors":0,"draw_ms":start.elapsed().as_secs_f64()*1000.,"output_hash":format!("{chain:08x}")})
    );
    Ok(true)
}
