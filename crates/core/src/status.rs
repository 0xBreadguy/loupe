//! `--status`: the Omarchy bar widget's one-line JSON.
//!
//! Legacy fields match `oracle/src/status.ts` (the frozen oracle) byte for byte;
//! `openAsks` and `estimatedAsks` are additive product fields. The line is built
//! by hand rather than through `serde_json` because three details have to match exactly:
//! key order, `JSON.stringify` number formatting (`1`, `0.5`, `0.07`, `null`), and
//! `JSON.stringify` string escaping after JavaScript's UTF-16 `.slice()`.
//!
//! Status-read dependency audit (before designing a narrower database reader):
//! - Counts and selection: Agent.status + requests (model::running/needs_you).
//! - Stable newest ordering: Agent.started and snapshot encounter order.
//! - Shown fields: title, status, context.used_tokens/max_tokens, model,
//!   provider_id (or run.provider), now.kind/tool.
//! - Failure count: every attributed Event.failed/t inside the ten-minute window.
//!
//! source::read feeds these from active threads, latest runs, subagents, latest
//! provider turns and provider threads, nodes, runtime requests, the globally
//! capped 3,000 tool items, and the globally capped 8,000 three-hour events.
//! build_structure supplies remote placeholders, aliases and retry suppression;
//! Attributor determines which agent receives requests/items/events. These cannot
//! be replaced with per-thread counts without proving the same graph and caps.
//! History (160 turns/provider), plans and spans decorate fields status never
//! consumes. They are candidates for omission, not an implemented optimization.
//! The database watcher below runs the incremental reader in its status-only form, which
//! reduces every agent to the fields listed here; `T3_AGENTS_SYNC_READ` keeps the full reader.
//!
use crate::{
    clock, demo, live,
    model::{self, Agent, Snapshot},
    source::{self, SourceError},
    terminal,
};
use std::{
    cmp::Ordering,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};

const FAILED_WINDOW_MS: f64 = 10.0 * 60_000.0;
/// Re-read at least this often so the ten-minute failure window can age out.
const RECHECK_MS: f64 = 30_000.0;
/// The widget's polling period; `--interval` is not part of the status contract.
pub const INTERVAL: Duration = Duration::from_secs(2);

/// The status object as one line of JSON, without a trailing newline.
pub fn status(snap: &Snapshot, now: f64) -> String {
    let mut waiting: Vec<&Agent> = snap.agents.iter().map(|a| &**a).filter(|a| model::needs_you(a)).collect();
    let mut live: Vec<&Agent> = snap.agents.iter().map(|a| &**a).filter(|a| model::running(a)).collect();
    let failed = snap
        .agents
        .iter()
        .flat_map(|a| a.events.iter())
        .filter(|e| e.failed && e.t >= now - FAILED_WINDOW_MS)
        .count();
    // JavaScript's sort is stable and compares `ms(started) ?? 0` newest first.
    let newest = |a: &&Agent, b: &&Agent| {
        started(b)
            .partial_cmp(&started(a))
            .unwrap_or(Ordering::Equal)
    };
    waiting.sort_by(newest);
    live.sort_by(newest);
    let shown: Vec<&Agent> = waiting.iter().chain(live.iter()).take(8).copied().collect();

    let mut out = String::with_capacity(96 + shown.len() * 96);
    out.push_str("{\"v\":1,\"running\":");
    out.push_str(&live.len().to_string());
    out.push_str(",\"waiting\":");
    out.push_str(&waiting.len().to_string());
    out.push_str(",\"failed\":");
    out.push_str(&failed.to_string());
    out.push_str(",\"agents\":[");
    for (i, a) in shown.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"title\":");
        push_js_utf16(&mut out, &model::clean(&a.title), Some(80));
        out.push_str(",\"state\":");
        if model::needs_you(a) {
            push_js(&mut out, "waiting");
        } else {
            push_js(&mut out, &a.status);
        }
        out.push_str(",\"ctx\":");
        match context(a) {
            Some(ctx) => push_js_number(&mut out, ctx),
            None => out.push_str("null"),
        }
        out.push_str(",\"model\":");
        push_js_utf16(
            &mut out,
            &terminal::short_model(&model::clean(&a.model)),
            Some(32),
        );
        out.push_str(",\"family\":");
        push_js(&mut out, family_name(a));
        out.push_str(",\"now\":");
        match &a.now {
            Some(item) => push_js(&mut out, model::category(&item.kind, item.tool.as_deref())),
            None => out.push_str("null"),
        }
        out.push_str(",\"openAsks\":");
        out.push_str(&open_asks(a).to_string());
        out.push_str(",\"estimatedAsks\":");
        out.push_str(&a.estimated_asks.to_string());
        out.push('}');
    }
    out.push_str("],\"openAsks\":");
    out.push_str(&snap.agents.iter().map(|a| open_asks(a)).sum::<u64>().to_string());
    out.push_str(",\"estimatedAsks\":");
    let mut owners = std::collections::HashSet::new();
    let estimated: u64 = snap.agents.iter().filter(|a| a.kind == "thread" && owners.insert(&a.thread_id)).map(|a| a.estimated_asks).sum();
    out.push_str(&estimated.to_string());
    out.push('}');
    out
}

/// The line for a database, or the error line when it cannot be read.
pub fn line(db: &Path, now: f64) -> String {
    match source::snapshot(db, now) {
        Ok(snap) => status(&snap, now),
        Err(err) => error_line(&err),
    }
}

/// `JSON.stringify({v,running,waiting,failed,agents,error})`, as `line()` reports a failed read.
fn error_line(err: &SourceError) -> String {
    let message = match err.kind {
        "missing" => format!(
            "No T3 database at {}. Is T3 Code installed and opened once?",
            tilde(&err.path)
        ),
        "unreadable" => format!(
            "Can't read the T3 database at {}: {}",
            tilde(&err.path),
            err.detail
        ),
        _ => format!("Unsupported T3 schema; {}", err.detail),
    };
    let mut out = String::with_capacity(message.len() + 96);
    out.push_str("{\"v\":1,\"running\":0,\"waiting\":0,\"failed\":0,\"agents\":[],\"error\":");
    push_js(&mut out, &message);
    out.push_str(",\"openAsks\":0,\"estimatedAsks\":0}");
    out
}

fn open_asks(a: &Agent) -> u64 {
    a.open_asks.blocking + a.open_asks.nonblocking + a.open_asks.cards.secret + a.open_asks.cards.link
}

/// `source.ts`'s `tilde`: the home folder shortened the way people type it.
fn tilde(path: &Path) -> String {
    let raw = path.to_string_lossy();
    match crate::platform::home_dir() {
        Some(home) if !home.is_empty() => {
            let home = PathBuf::from(home);
            let home = home.to_string_lossy();
            let prefix = format!("{home}/");
            match raw.strip_prefix(&prefix) {
                Some(rest) => format!("~/{rest}"),
                None => raw.into_owned(),
            }
        }
        _ => raw.into_owned(),
    }
}

fn started(a: &Agent) -> f64 {
    model::ms(a.started.as_deref()).unwrap_or(0.0)
}

/// `typeof used === "number" && typeof max === "number" && max > 0`
/// → `Math.round(Math.min(1, used / max) * 100) / 100`, else `null`.
fn context(a: &Agent) -> Option<f64> {
    let used = a.context.used_tokens?;
    let max = a.context.max_tokens?;
    if !(max > 0.0) {
        return None;
    }
    let ratio = used / max;
    // Math.min(1, NaN) is NaN, while f64::min would return the other operand.
    let capped = if ratio.is_nan() {
        ratio
    } else {
        ratio.min(1.0)
    };
    if !capped.is_finite() {
        // Math.round(NaN) is NaN and JSON.stringify(NaN) is null.
        return None;
    }
    Some(js_round(capped * 100.0) / 100.0)
}

/// `Math.round`: halves go toward +Infinity, so -0.5 becomes -0 and -1.5 becomes -1.
fn js_round(x: f64) -> f64 {
    if !x.is_finite() {
        return x;
    }
    if (x - x.trunc()).abs() == 0.5 {
        x.ceil()
    } else {
        x.round()
    }
}

fn family_name(a: &Agent) -> &'static str {
    let hint = a
        .provider_id
        .as_deref()
        .or_else(|| a.run.get("provider").and_then(|v| v.as_str()));
    match terminal::family(&a.model, hint) {
        terminal::ProviderFamily::Claude => "claude",
        terminal::ProviderFamily::OpenAi => "openai",
        terminal::ProviderFamily::Grok => "grok",
        terminal::ProviderFamily::Other => "other",
    }
}

// ── JavaScript string and number formatting ─────────────────────────────────

/// Append `JSON.stringify(value)` for a string after JavaScript's `.slice(0, max)`.
/// `max` counts UTF-16 code units, so a cut through an emoji leaves a lone surrogate.
fn push_js_utf16(out: &mut String, value: &str, max: Option<usize>) {
    let units: Vec<u16> = value.encode_utf16().collect();
    let end = max.map_or(units.len(), |max| max.min(units.len()));
    push_js_units(out, &units[..end]);
}

fn push_js(out: &mut String, value: &str) {
    let units: Vec<u16> = value.encode_utf16().collect();
    push_js_units(out, &units);
}

/// `JSON.stringify` string rules: quote and backslash, the short control escapes,
/// other C0 controls as lowercase `\u00xx`, lone surrogates as lowercase `\udxxx`,
/// and every other code point raw (valid non-ASCII stays UTF-8).
fn push_js_units(out: &mut String, units: &[u16]) {
    out.push('"');
    let mut i = 0;
    while i < units.len() {
        let unit = units[i];
        match unit {
            0x22 => out.push_str("\\\""),
            0x5c => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x0a => out.push_str("\\n"),
            0x0d => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            0x00..=0x1f => out.push_str(&format!("\\u{unit:04x}")),
            0xd800..=0xdbff => {
                if let Some(&next) = units.get(i + 1)
                    && (0xdc00..=0xdfff).contains(&next)
                {
                    let cp =
                        0x10000 + ((u32::from(unit) - 0xd800) << 10) + (u32::from(next) - 0xdc00);
                    if let Some(c) = char::from_u32(cp) {
                        out.push(c);
                    } else {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                    i += 1;
                } else {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            0xdc00..=0xdfff => out.push_str(&format!("\\u{unit:04x}")),
            _ => {
                if let Some(c) = char::from_u32(u32::from(unit)) {
                    out.push(c);
                }
            }
        }
        i += 1;
    }
    out.push('"');
}

/// `JSON.stringify` for a number: shortest round-trip digits, fixed notation for
/// `1e-6 <= |x| < 1e21`, exponential outside that, and `null` for non-finite values.
fn push_js_number(out: &mut String, value: f64) {
    if !value.is_finite() {
        out.push_str("null");
        return;
    }
    if value == 0.0 {
        out.push('0');
        return;
    }
    if value < 0.0 {
        out.push('-');
        push_js_number(out, -value);
        return;
    }
    // Rust's LowerExp already prints the shortest decimal that round-trips.
    let exp = format!("{value:e}");
    let (mantissa, exponent) = exp
        .split_once('e')
        .expect("LowerExp always has an exponent");
    let exponent: i32 = exponent.parse().expect("integer exponent");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let k = digits.len() as i32;
    let n = exponent + 1;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if n > 0 && n <= 21 {
        let n = n as usize;
        out.push_str(&digits[..n]);
        out.push('.');
        out.push_str(&digits[n..]);
    } else if n > -6 && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        if n - 1 >= 0 {
            out.push('+');
        }
        out.push_str(&(n - 1).to_string());
    }
}

// ── Watch loop ──────────────────────────────────────────────────────────────

/// Signature of the database and its `-wal` file: mtime and size, like `status.ts`.
fn signature(path: &Path) -> String {
    let entry = |file: &Path| match std::fs::metadata(file) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{mtime}:{}", meta.len())
        }
        Err(_) => "-".to_owned(),
    };
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    format!("{}|{}", entry(path), entry(Path::new(&wal)))
}

enum Step {
    Idle,
    Wrote,
}

/// The state machine behind `watch`, with an injected writer and clock so tests can drive it.
struct Watcher<'a> {
    path: &'a Path,
    seen: String,
    checked: f64,
    last: String,
    /// The incremental reader, kept open between ticks. `None` until the first read, after
    /// a read error, and always when `T3_AGENTS_SYNC_READ` asks for the original's full read.
    reader: Option<live::Reader>,
    incremental: bool,
}

impl<'a> Watcher<'a> {
    fn new(path: &'a Path) -> Self {
        Self {
            path,
            seen: String::new(),
            checked: 0.0,
            last: String::new(),
            reader: None,
            incremental: std::env::var_os("T3_AGENTS_SYNC_READ").is_none(),
        }
    }

    /// The line for the database as it is now. The incremental reader returns the same
    /// snapshot the full reader does, for the cost of what changed. Any error from it is
    /// answered by the full read, so an error line is exactly the original's.
    fn read(&mut self, now: f64) -> String {
        if self.incremental {
            if self.reader.is_none() {
                // The line reads a few fields of each agent; the reader publishes
                // only those (`live::status_view`) and never reads item text.
                self.reader = live::Reader::open(self.path).ok().map(live::Reader::status_only);
            }
            if let Some(reader) = self.reader.as_mut() {
                match reader.poll(now) {
                    Ok(update) => {
                        let text = status(&update.snapshot, now);
                        drop(update);
                        // A fallback to the full read leaves its heap behind, like any full read.
                        clock::release_memory();
                        return text;
                    }
                    Err(_) => self.reader = None,
                }
            }
        }
        let text = line(self.path, now);
        // The watcher keeps one line. Don't let it hold the heap its read needed.
        clock::release_memory();
        text
    }

    /// One `tick()` from `status.ts`: re-read only when the database changed or 30 s passed,
    /// and write only when the line differs from the last one written.
    fn tick<W: Write>(&mut self, now: f64, out: &mut W) -> io::Result<Step> {
        let sig = signature(self.path);
        if sig == self.seen && now - self.checked < RECHECK_MS {
            return Ok(Step::Idle);
        }
        self.seen = sig;
        self.checked = now;
        let next = self.read(now);
        if next != self.last {
            self.last = next.clone();
            out.write_all(next.as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
            return Ok(Step::Wrote);
        }
        Ok(Step::Idle)
    }
}

/// Print a line whenever the status changes; return when stdout closes or the parent changes.
pub fn watch(db: &Path) -> io::Result<()> {
    let parent = parent_pid();
    let mut out = io::stdout().lock();
    let mut watcher = Watcher::new(db);
    loop {
        if parent_pid() != parent {
            return Ok(());
        }
        if watcher.tick(clock::wall_now(), &mut out).is_err() {
            return Ok(());
        }
        std::thread::sleep(INTERVAL);
    }
}

/// Demo refreshes and status polls have independent schedules, just like Demo + watch
/// in the oracle. Drive the same state machine with virtual time in the parity gate.
struct DemoWatcher {
    epoch: f64,
    next_refresh: f64,
    next_poll: f64,
    snapshot: Snapshot,
    dirty: bool,
    checked: f64,
    last: String,
}

impl DemoWatcher {
    fn new(epoch: f64) -> Result<Self, SourceError> {
        Ok(Self {
            epoch,
            next_refresh: epoch + demo::REFRESH_MS,
            next_poll: epoch,
            snapshot: demo::snapshot(epoch, epoch)?,
            dirty: true,
            checked: epoch,
            last: String::new(),
        })
    }

    fn advance<W: Write>(&mut self, now: f64, out: &mut W) -> Result<(), Box<dyn std::error::Error>> {
        // Timer callbacks at the same deadline run in registration order: the demo
        // registers its refresh before watch registers its poll.
        if now >= self.next_refresh {
            self.snapshot = demo::snapshot(now, self.epoch)?;
            self.dirty = true;
            let periods = ((now - self.next_refresh) / demo::REFRESH_MS).floor() + 1.0;
            self.next_refresh += periods * demo::REFRESH_MS;
        }
        if now >= self.next_poll {
            let period = INTERVAL.as_millis() as f64;
            self.next_poll += (((now - self.next_poll) / period).floor() + 1.0) * period;
            if self.dirty || now - self.checked >= RECHECK_MS {
                self.dirty = false;
                self.checked = now;
                let next = status(&self.snapshot, now);
                if next != self.last {
                    out.write_all(next.as_bytes())?;
                    out.write_all(b"\n")?;
                    out.flush()?;
                    self.last = next;
                }
            }
        }
        Ok(())
    }

    fn next(&self) -> f64 {
        self.next_refresh.min(self.next_poll)
    }
}

/// In-memory demo watch: no user database, and no scratch file to keep alive.
pub fn watch_demo(epoch: f64) -> Result<(), Box<dyn std::error::Error>> {
    let parent = parent_pid();
    let start = std::time::Instant::now();
    let mut watcher = DemoWatcher::new(epoch)?;
    let mut out = io::stdout().lock();
    loop {
        if parent_pid() != parent { return Ok(()); }
        let now = epoch + start.elapsed().as_secs_f64() * 1000.0;
        if let Err(error) = watcher.advance(now, &mut out) {
            if error.downcast_ref::<io::Error>().is_some() { return Ok(()); }
            return Err(error);
        }
        let delay = (watcher.next() - epoch - start.elapsed().as_secs_f64() * 1000.0).max(0.0);
        std::thread::sleep(Duration::from_secs_f64(delay / 1000.0));
    }
}

/// Experiment-only deterministic timer replay, using the production state machine.
fn demo_replay(epoch: f64, duration: f64) -> Result<(), Box<dyn std::error::Error>> {
    let mut watcher = DemoWatcher::new(epoch)?;
    let mut out = io::stdout().lock();
    while watcher.next() <= epoch + duration {
        watcher.advance(watcher.next(), &mut out)?;
    }
    Ok(())
}

fn parent_pid() -> i32 {
    crate::platform::parent_pid()
}

// ── CLI ─────────────────────────────────────────────────────────────────────

const USAGE: &str =
    "Usage: t3-agents --status [--watch] [--db PATH | --snapshot FILE] [--now-ms MS]

  --status          print a one-line JSON status (used by the Omarchy bar widget)
  --watch           with --status, print a new line whenever the status changes
  --db PATH         T3 statev2.sqlite (default: ${T3CODE_HOME:-~/.t3}/userdata/statev2.sqlite)
  --snapshot FILE   read a synthetic snapshot fixture instead of a database (testing)
  --now-ms MS       clock to evaluate the snapshot at (default: now, or the snapshot's at)
";

/// True when the arguments ask for `--status`. Checked before the other headless modes.
pub fn requested() -> bool {
    std::env::args().any(|arg| arg == "--status")
}

/// Run `--status`. Argument errors exit 2 like `parseArgs` in `main.ts`.
pub fn cli() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut db: Option<String> = None;
    let mut snapshot: Option<String> = None;
    let mut now_ms: Option<f64> = None;
    let mut watching = false;
    let mut replay = None;
    let mut probe_database = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].clone();
        match arg.as_str() {
            "--status" | "--demo" => {}
            "--probe-status-db" => probe_database = true,
            "--probe-status-demo" => {
                replay = Some(now_value(&flag_value(&args, &mut i, "--probe-status-demo")));
            }
            "--watch" => watching = true,
            "--db" => db = Some(flag_value(&args, &mut i, "--db")),
            "--snapshot" => snapshot = Some(flag_value(&args, &mut i, "--snapshot")),
            "--now-ms" => {
                let raw = flag_value(&args, &mut i, "--now-ms");
                now_ms = Some(now_value(&raw));
            }
            _ if arg.starts_with("--db=") => db = Some(arg["--db=".len()..].to_owned()),
            _ if arg.starts_with("--snapshot=") => {
                snapshot = Some(arg["--snapshot=".len()..].to_owned());
            }
            _ if arg.starts_with("--now-ms=") => {
                now_ms = Some(now_value(&arg["--now-ms=".len()..]));
            }
            other => usage_error(&format!("Unknown option {other:?}")),
        }
        i += 1;
    }
    if let Some(duration) = replay {
        return demo_replay(now_ms.unwrap_or_else(clock::wall_now), duration);
    }
    if db.is_some() && snapshot.is_some() {
        usage_error("--db and --snapshot cannot be used together");
    }

    // Test transport only: drive the production watcher with one clock per
    // stdin line and acknowledge even silent ticks with its last emitted line.
    if probe_database {
        use std::io::BufRead;
        let path = PathBuf::from(db.ok_or("--probe-status-db requires --db")?);
        let mut watcher = Watcher::new(&path);
        let mut out = io::stdout().lock();
        for input in io::stdin().lock().lines() {
            let now = now_value(&input?);
            watcher.tick(now, &mut io::sink())?;
            writeln!(out, "{}", watcher.last)?;
            out.flush()?;
        }
        return Ok(());
    }

    let mut out = io::stdout().lock();
    let write = |out: &mut io::StdoutLock<'_>, text: &str| {
        // A closed pipe is a normal way for the bar to go away.
        out.write_all(text.as_bytes())
            .and_then(|()| out.flush())
            .is_ok()
    };

    if let Some(file) = snapshot {
        let bytes = std::fs::read(&file)?;
        let snap: Snapshot = serde_json::from_slice(&bytes)?;
        let now = now_ms.unwrap_or(snap.at);
        let text = status(&snap, now);
        write(&mut out, &(text + "\n"));
        return Ok(());
    }

    let path = db
        .map(PathBuf::from)
        .unwrap_or_else(crate::product::default_database);
    if watching {
        // A closed pipe ends the watch without an error.
        let _ = watch(&path);
        return Ok(());
    }
    let now = now_ms.unwrap_or_else(clock::wall_now);
    write(&mut out, &(line(&path, now) + "\n"));
    Ok(())
}

/// The value of `--name VALUE`; a missing value is a usage error.
fn flag_value(args: &[String], i: &mut usize, name: &str) -> String {
    *i += 1;
    match args.get(*i) {
        Some(value) => value.clone(),
        None => usage_error(&format!("Option '{name} <value>' argument missing")),
    }
}

fn now_value(raw: &str) -> f64 {
    match raw.parse::<f64>() {
        Ok(value) if value.is_finite() => value,
        _ => usage_error(&format!("--now-ms needs a finite number, got {raw:?}")),
    }
}

fn usage_error(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    #[test]
    fn counts_all_asks_and_only_thread_estimates() {
        let mut thread = Agent { kind: "thread".into(), status: "running".into(), estimated_asks: 3, ..Default::default() };
        thread.open_asks = model::OpenAsks { blocking: 1, nonblocking: 2, cards: model::AskCards { secret: 3, link: 4 } };
        thread.requests.push("approval".into());
        let child = Agent { kind: "subagent".into(), status: "running".into(), estimated_asks: 3, ..Default::default() };
        let snap = Snapshot { agents: vec![thread.into(), child.into()], ..Default::default() };
        let value: serde_json::Value = serde_json::from_str(&status(&snap, 0.0)).unwrap();
        assert_eq!(value["openAsks"], 10);
        assert_eq!(value["estimatedAsks"], 3);
        assert_eq!(value["agents"][0]["openAsks"], 10);
        assert_eq!(value["agents"][0]["estimatedAsks"], 3);
        let empty: serde_json::Value = serde_json::from_str(&status(&Snapshot::default(), 0.0)).unwrap();
        assert_eq!(empty["openAsks"], 0);
        assert_eq!(empty["estimatedAsks"], 0);
    }

    fn temp_path(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "t3-status-{}-{}-{name}.sqlite",
            std::process::id(),
            NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        ))
    }

    /// One running thread with a title: enough for a status line that changes with it.
    fn database(path: &Path, title: &str) {
        let db = Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT,payload_json TEXT,deleted_at TEXT,archived_at TEXT);
             CREATE TABLE orchestration_v2_projection_runs(thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
             CREATE TABLE orchestration_v2_projection_subagents(thread_id TEXT,payload_json TEXT);
             CREATE TABLE orchestration_v2_projection_nodes(thread_id TEXT,node_id TEXT,parent_node_id TEXT,run_id TEXT);
             CREATE TABLE orchestration_v2_projection_provider_threads(thread_id TEXT,payload_json TEXT);
             CREATE TABLE orchestration_v2_projection_provider_turns(thread_id TEXT,provider_thread_id TEXT,ordinal INTEGER,payload_json TEXT,started_at TEXT,completed_at TEXT);
             CREATE TABLE orchestration_v2_projection_turn_items(thread_id TEXT,run_id TEXT,node_id TEXT,provider_thread_id TEXT,type TEXT,status TEXT,updated_at TEXT,ordinal INTEGER,payload_json TEXT);
             CREATE TABLE orchestration_v2_projection_runtime_requests(thread_id TEXT,node_id TEXT,kind TEXT,status TEXT,resolved_at TEXT);
             CREATE TABLE projection_projects(project_id TEXT,title TEXT,workspace_root TEXT);",
        )
        .unwrap();
        db.execute(
            "INSERT INTO projection_projects VALUES('p','Project','/synthetic')",
            [],
        )
        .unwrap();
        db.execute(
            "INSERT INTO orchestration_v2_projection_threads VALUES('t',?,NULL,NULL)",
            [serde_json::json!({"id":"t","projectId":"p","title":title,"activeProviderThreadId":"pt"}).to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO orchestration_v2_projection_runs VALUES('t',1,?,NULL,NULL)",
            [serde_json::json!({"id":"r","threadId":"t","status":"running","providerThreadId":"pt","startedAt":"2026-11-14T00:00:00.000Z"}).to_string()],
        )
        .unwrap();
        db.execute(
            "INSERT INTO orchestration_v2_projection_provider_threads VALUES('t',?)",
            [
                serde_json::json!({"id":"pt","contextUsage":{"usedTokens":1,"maxTokens":10}})
                    .to_string(),
            ],
        )
        .unwrap();
    }

    fn retitle(path: &Path, title: &str) {
        let db = Connection::open(path).unwrap();
        db.execute(
            "UPDATE orchestration_v2_projection_threads SET payload_json=? WHERE thread_id='t'",
            [serde_json::json!({"id":"t","projectId":"p","title":title,"activeProviderThreadId":"pt"}).to_string()],
        )
        .unwrap();
    }

    struct Closed;
    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[test]
    fn watch_prints_at_start_then_only_when_the_line_changes() {
        let file = temp_path("watch");
        database(&file, "First");
        let now = 1_794_484_800_000.0;
        let mut watcher = Watcher::new(&file);
        let mut out = Vec::new();
        assert!(matches!(watcher.tick(now, &mut out), Ok(Step::Wrote)));
        let first = out.clone();
        assert!(first.ends_with(b"\n"));
        let text = String::from_utf8(first.clone()).unwrap();
        assert!(text.contains("\"title\":\"First\""), "{text}");
        // Same signature: not even read.
        assert!(matches!(
            watcher.tick(now + 1_000.0, &mut out),
            Ok(Step::Idle)
        ));
        assert_eq!(out, first);
        // A write changes the signature and the line.
        std::thread::sleep(Duration::from_millis(20));
        retitle(&file, "Second");
        assert!(matches!(
            watcher.tick(now + 2_000.0, &mut out),
            Ok(Step::Wrote)
        ));
        assert!(out.len() > first.len());
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("\"title\":\"Second\"")
        );
        drop(watcher);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn status_watcher_emits_changed_ask_count() {
        let file = temp_path("ask-watch");
        database(&file, "First");
        let now = 1_794_484_800_000.0;
        let mut watcher = Watcher::new(&file);
        let mut out = Vec::new();
        assert!(matches!(watcher.tick(now, &mut out), Ok(Step::Wrote)));
        let db = Connection::open(&file).unwrap();
        db.execute("INSERT INTO orchestration_v2_projection_runtime_requests VALUES('t',NULL,'user_input','pending',NULL)", []).unwrap();
        drop(db);
        // Force the normal periodic refresh, independent of filesystem timestamp resolution.
        assert!(matches!(watcher.tick(now + 30_000.0, &mut out), Ok(Step::Wrote)));
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<_> = text.lines().collect();
        let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
        assert_eq!(last["openAsks"], 1);
        drop(watcher);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn watch_exits_when_stdout_is_closed() {
        let file = temp_path("closed");
        database(&file, "First");
        let mut watcher = Watcher::new(&file);
        let mut closed = Closed;
        assert!(watcher.tick(1_794_484_800_000.0, &mut closed).is_err());
        drop(watcher);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn watch_rereads_after_thirty_seconds_even_without_a_change() {
        let file = temp_path("recheck");
        database(&file, "First");
        let now = 1_794_484_800_000.0;
        let mut watcher = Watcher::new(&file);
        let mut out = Vec::new();
        assert!(matches!(watcher.tick(now, &mut out), Ok(Step::Wrote)));
        // Aging can move the failure window, so a 30 s check re-reads; same content stays silent.
        let before = out.len();
        assert!(matches!(
            watcher.tick(now + 29_999.0, &mut out),
            Ok(Step::Idle)
        ));
        assert_eq!(out.len(), before);
        assert!(matches!(
            watcher.tick(now + 30_000.0, &mut out),
            Ok(Step::Idle)
        ));
        assert_eq!(out.len(), before);
        drop(watcher);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn demo_watch_keeps_refresh_and_poll_independent() {
        let epoch = 1_794_484_800_000.0;
        let mut watcher = DemoWatcher::new(epoch).unwrap();
        let mut out = Vec::new();
        watcher.advance(epoch, &mut out).unwrap();
        let initial = out.clone();
        watcher.advance(epoch + 2_000.0, &mut out).unwrap();
        assert_eq!(out, initial);
        watcher.advance(epoch + 3_000.0, &mut out).unwrap();
        assert_eq!(out, initial, "refresh alone must not emit");
        watcher.advance(epoch + 4_000.0, &mut out).unwrap();
        assert_eq!(watcher.checked, epoch + 4_000.0);
        assert!(!watcher.dirty);
        assert_eq!(watcher.last, status(&watcher.snapshot, epoch + 4_000.0));
        // The first refresh can serialize identically; the queued agent starting
        // at one minute guarantees a visible change.
        watcher.advance(epoch + 60_000.0, &mut out).unwrap();
        assert!(out.len() > initial.len(), "queued start must change the line");
        let mut closed = Closed;
        let mut watcher = DemoWatcher::new(epoch).unwrap();
        assert!(watcher.advance(epoch, &mut closed).is_err());
    }

    #[test]
    fn javascript_numbers_and_strings_match_json_stringify() {
        let mut out = String::new();
        for (value, want) in [
            (1.0, "1"),
            (0.5, "0.5"),
            (0.07, "0.07"),
            (-0.0, "0"),
            (0.01, "0.01"),
            (1e-7, "1e-7"),
            (1e-6, "0.000001"),
            (1e20, "100000000000000000000"),
            (1e21, "1e+21"),
            (-1.25, "-1.25"),
            (123.456, "123.456"),
        ] {
            out.clear();
            push_js_number(&mut out, value);
            assert_eq!(out, want, "{value}");
        }
        out.clear();
        push_js(&mut out, "a\"b\\c\u{1}\u{1f}\n\t\r\u{8}\u{c}é🍞\u{7f}");
        assert_eq!(
            out,
            "\"a\\\"b\\\\c\\u0001\\u001f\\n\\t\\r\\b\\f\u{e9}🍞\u{7f}\""
        );
        // A cut through an emoji leaves a lone high surrogate, as JSON.stringify writes it.
        out.clear();
        push_js_utf16(&mut out, &("x".repeat(79) + "🍞"), Some(80));
        assert!(out.ends_with("x\\ud83c\""), "{out}");
    }
}
