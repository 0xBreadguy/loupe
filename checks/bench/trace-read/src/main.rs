//! What one Trace read costs, and what the live watch costs per refresh, on a made-up
//! database (checks/scripts/scale-db.ts). Built only on Trace's public read API, so the same
//! source builds against an older core for an A/B comparison.
//!
//!   trace-read-bench cold DB TARGET N          N reads on one connection; prints each in ms
//!   trace-read-bench watch DB TARGET SECONDS   the live watch at the dashboard's 2 s period;
//!                                              prints CPU per refresh and refreshes seen
//!   trace-read-bench phases DB THREAD          each of the read's queries on its own, in ms
//!   trace-read-bench draw DB TARGET N COLS ROWS  N frames of the screen as the running
//!                                              dashboard draws them, 100 ms apart; each in ms
//!
//! TARGET is `thread:ID` or `run:ID`. Never point it at a real T3 database (AGENTS.md rule 1).
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use loupe_core::{program, terminal, trace, trace_live, trace_view};

fn target(raw: &str) -> trace::Target {
    match raw.split_once(':') {
        Some(("thread", id)) => trace::Target::Thread(id.into()),
        Some(("run", id)) => trace::Target::Run(id.into()),
        _ => panic!("TARGET is thread:ID or run:ID"),
    }
}

/// This process's CPU time, user and system, in milliseconds.
fn cpu_ms() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat.rsplit_once(')').unwrap().1.split_whitespace().collect();
    let ticks: f64 = fields[11].parse::<f64>().unwrap() + fields[12].parse::<f64>().unwrap();
    ticks * 1000. / 100. // CLK_TCK is 100 on Linux
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = Path::new(&args[2]);
    match args[1].as_str() {
        "cold" => {
            let db = trace::open(path).unwrap();
            let target = target(&args[3]);
            let n: usize = args[4].parse().unwrap();
            let mut times = vec![];
            for _ in 0..n {
                let start = Instant::now();
                let t = trace::read_from(&db, path, &target, loupe_core::clock::wall_now()).unwrap().expect("target exists");
                times.push(start.elapsed().as_secs_f64() * 1000.);
                std::hint::black_box(t);
            }
            println!("{}", times.iter().map(|t| format!("{t:.3}")).collect::<Vec<_>>().join(" "));
        }
        "watch" => {
            let seconds: f64 = args[4].parse().unwrap();
            let ticks = Arc::new(AtomicU64::new(0));
            let seen = ticks.clone();
            let watch = trace_live::watch(path, target(&args[3]), Duration::from_secs(2), move || {
                seen.fetch_add(1, Ordering::Relaxed);
            });
            // The first read is the open; measure the refreshes after it.
            while ticks.load(Ordering::Relaxed) == 0 {
                std::thread::sleep(Duration::from_millis(5));
            }
            std::thread::sleep(Duration::from_millis(500));
            let (c0, t0) = (cpu_ms(), ticks.load(Ordering::Relaxed));
            std::thread::sleep(Duration::from_secs_f64(seconds));
            let (c1, t1) = (cpu_ms(), ticks.load(Ordering::Relaxed));
            drop(watch);
            let refreshes = (t1 - t0).max(1);
            println!("cpu_ms {:.1} refreshes {} cpu_ms_per_refresh {:.3} percent {:.3}", c1 - c0, t1 - t0, (c1 - c0) / refreshes as f64, (c1 - c0) / (seconds * 10.));
        }
        "phases" => {
            let db = trace::open(path).unwrap();
            program::register(&db).unwrap();
            let thread = &args[3];
            let p = "orchestration_v2_projection_";
            let runs: Vec<String> = db
                .prepare(&format!("SELECT run_id FROM {p}runs WHERE thread_id=?1 ORDER BY ordinal"))
                .unwrap()
                .query_map([thread], |r| r.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            let time = |name: &str, sql: &str, params: &[&str]| {
                let mut best = f64::INFINITY;
                let mut rows = 0;
                for _ in 0..5 {
                    let start = Instant::now();
                    let mut q = db.prepare(sql).unwrap();
                    let columns = q.column_count();
                    let mut r = q.query(rusqlite::params_from_iter(params)).unwrap();
                    rows = 0;
                    while let Some(row) = r.next().unwrap() {
                        for c in 0..columns {
                            std::hint::black_box(row.get_ref(c).unwrap());
                        }
                        rows += 1;
                    }
                    best = best.min(start.elapsed().as_secs_f64() * 1000.);
                }
                println!("{name:28} {best:8.3} ms  {rows} rows");
            };
            time("runs", &format!("SELECT run_id,ordinal,requested_at,completed_at,json_extract(payload_json,'$.startedAt'),json_extract(payload_json,'$.status') FROM {p}runs WHERE thread_id=?1 ORDER BY ordinal"), &[thread]);
            let ids = runs.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let run_refs: Vec<&str> = runs.iter().map(String::as_str).collect();
            time("delegates by payload runId", &format!("SELECT subagent_id,json_extract(payload_json,'$.status') FROM {p}subagents WHERE json_extract(payload_json,'$.runId') IN ({ids})"), &run_refs);
            time("delegates by run_id column", &format!("SELECT subagent_id,json_extract(payload_json,'$.status') FROM {p}subagents WHERE run_id IN ({ids})"), &run_refs);
            let types = "'command_execution','dynamic_tool','file_change','file_search','web_search','user_input_request','approval_request','secret_request','link_request','reasoning','assistant_message','compaction','user_message','run_interrupt_request','error'";
            let full = format!("SELECT type,status,ordinal,updated_at,json_extract(payload_json,'$.startedAt'),json_extract(payload_json,'$.completedAt'),CASE WHEN type='dynamic_tool' THEN json_extract(payload_json,'$.toolName') END,CASE WHEN type='command_execution' THEN t3_programs(json_extract(payload_json,'$.input')) END,CASE WHEN type='error' THEN json_extract(payload_json,'$.failure.class') END,turn_item_id,node_id IS NOT NULL FROM {p}turn_items WHERE run_id=?1 AND type IN ({types}) ORDER BY ordinal");
            let no_programs = full.replace("t3_programs(json_extract(payload_json,'$.input'))", "NULL");
            let columns_only = format!("SELECT type,status,ordinal,updated_at,turn_item_id FROM {p}turn_items WHERE run_id=?1 AND type IN ({types}) ORDER BY ordinal");
            for run in &runs {
                time(&format!("items {}", &run[run.len().saturating_sub(6)..]), &full, &[run]);
                time("  without t3_programs", &no_programs, &[run]);
                time("  columns only, no JSON", &columns_only, &[run]);
            }
            time("runless asks", &format!("SELECT type FROM {p}turn_items WHERE thread_id=?1 AND run_id IS NULL AND type IN ('user_input_request','approval_request','secret_request','link_request') ORDER BY ordinal"), &[thread]);
            time("runtime requests", &format!("SELECT runtime_request_id,kind,status FROM {p}runtime_requests WHERE thread_id=?1"), &[thread]);
            time("data_version", "PRAGMA data_version", &[]);
        }
        "draw" => {
            let db = trace::open(path).unwrap();
            let now = loupe_core::clock::wall_now();
            let t = trace::read_from(&db, path, &target(&args[3]), now).unwrap().expect("target exists");
            let (n, cols, rows): (usize, usize, usize) = (args[4].parse().unwrap(), args[5].parse().unwrap(), args[6].parse().unwrap());
            let palette = terminal::Palette::default();
            let mut times = vec![];
            for frame in 0..n {
                let at = now + frame as f64 * 100.;
                let view = trace_view::View { selected: None, scroll: 0, calm: false, mono: false, spin: (at / 100.) as usize, demo: false, now: at };
                let start = Instant::now();
                std::hint::black_box(trace_view::draw(&t, &palette, cols, rows, &view, &loupe_core::platform::local_time));
                times.push(start.elapsed().as_secs_f64() * 1000.);
            }
            println!("{}", times.iter().map(|t| format!("{t:.4}")).collect::<Vec<_>>().join(" "));
        }
        _ => panic!("mode is cold, watch, phases or draw"),
    }
}
