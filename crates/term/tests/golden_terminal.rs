//! The real terminal, against what the TypeScript original showed and left behind (main
//! `2f1d553`), recorded from `checks/scripts/terminal-parity.ts`. Only `t3-agents-term` runs
//! here, inside a private tmux server, driven through the same steps the gate drove both
//! programs through. No Node.
//!
//! The recording (`checks/fixtures/golden/terminal.json.gz`) holds, for each of the gate's 14
//! screen cases, what `tmux capture-pane -p -e -N` returned for the original after the initial
//! frame and after each of 37 keys, mouse events and a resize, together with the key sent, so
//! this test replays the gate's own script. It also holds what the original left behind on four
//! ways of exiting (exit code, terminal modes, stty, the main screen), how it fared at odd sizes
//! and `TERM=dumb`, and what it prints when it is not on a terminal.
//!
//! A capture is parsed into cells (glyph, colour, background, weight, attributes) exactly as the
//! gate parses it, and the recording stores a digest of the original's parsed cells, so the
//! parser here is proven against the oracle's on the recorded screens before it judges a screen
//! from the Rust program. Differences are reported as the gate reports them.
//!
//! tmux must be installed; if it is not, the tmux tests say so and pass without running.
//! `T3_GOLDEN_DIR` points the test at another export, `T3_GOLDEN_TMUX_JOBS` sets how many tmux
//! cases run at once (default 4: each waits on real time, and too many at once make the
//! gate's "has the screen settled" test unreliable).
#![cfg(unix)]

/// The few helpers of `crates/core/tests/golden_support/mod.rs` this test needs, kept here so
/// the presenter's tests do not also need SQLite: the fixture loader, FNV-1a, and a scratch
/// directory on the target volume.
mod support {
    use serde_json::Value;
    use std::{io::Read, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    pub fn golden_dir() -> PathBuf {
        std::env::var_os("T3_GOLDEN_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../checks/fixtures/golden"))
    }

    pub fn load(name: &str) -> Value {
        let path = golden_dir().join(name);
        let mut data = String::new();
        flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
            .read_to_string(&mut data)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let value: Value = serde_json::from_str(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(value["version"], 1, "{name}: fixture version");
        assert!(value["dashboard"]["commit"].as_str().is_some_and(|c| c.len() == 40), "{name}: records the dashboard revision");
        value
    }

    pub fn recorded_from(fixture: &Value) -> String {
        let dashboard = &fixture["dashboard"];
        let short = |key: &str| dashboard[key].as_str().unwrap_or("?").chars().take(12).collect::<String>();
        format!("dashboard tree {} at {} (src {})", dashboard["tree"].as_str().unwrap_or("?"), short("commit"), short("src"))
    }

    /// An empty directory on the target volume (`/tmp` is small and its paths are long).
    pub fn scratch(name: &str) -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("golden-{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// FNV-1a (64-bit) over UTF-8, the digest convention of every exporter.
    pub fn fnv1a(text: &str) -> String {
        let mut hash: u64 = 0xcbf29ce484222325;
        for byte in text.as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("{hash:016x}")
    }
}

use serde_json::Value;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Condvar, Mutex, OnceLock},
    thread::sleep,
    time::{Duration, Instant},
};
use t3_agents_core::terminal::cell_width;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-term");
const NOW: &str = "1794484800000"; // 2026-11-12T12:00:00Z, the instant the gate pins
const FIXTURE: &str = "terminal.json.gz";

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

// ── permits: how many tmux cases run at once ─────────────────────────────────

fn permit() -> impl Drop {
    static STATE: OnceLock<(Mutex<usize>, Condvar)> = OnceLock::new();
    let (lock, cv) = STATE.get_or_init(|| (Mutex::new(0), Condvar::new()));
    let limit: usize = std::env::var("T3_GOLDEN_TMUX_JOBS").ok().and_then(|v| v.parse().ok()).unwrap_or(4).max(1);
    let mut running = lock.lock().unwrap();
    while *running >= limit {
        running = cv.wait(running).unwrap();
    }
    *running += 1;
    struct Permit;
    impl Drop for Permit {
        fn drop(&mut self) {
            let (lock, cv) = STATE.get().unwrap();
            *lock.lock().unwrap() -= 1;
            cv.notify_one();
        }
    }
    Permit
}

// ── a private tmux server ────────────────────────────────────────────────────

fn tmux_installed() -> bool {
    Command::new("tmux").arg("-V").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Starts a case that needs tmux, or says it is skipped. Returns false to skip.
fn needs_tmux(case: &str) -> bool {
    if tmux_installed() {
        return true;
    }
    eprintln!("SKIPPED {case}: tmux is not installed, so the real-terminal recording cannot be replayed on this machine");
    false
}

struct Work {
    dir: PathBuf,
    home: PathBuf,
    t3home: PathBuf,
    runtime: PathBuf,
    stub: PathBuf,
}

impl Work {
    fn new(name: &str) -> Work {
        let dir = support::scratch(name);
        let w = Work { home: dir.join("home"), t3home: dir.join("t3home"), runtime: dir.join("run"), stub: dir.join("bin"), dir };
        for d in [&w.home, &w.t3home, &w.runtime, &w.stub] {
            std::fs::create_dir_all(d).unwrap();
        }
        // xdg-open is a stub so Enter cannot raise a real window.
        write_script(&w.stub.join("xdg-open"), "#!/bin/sh\nexit 0\n");
        w
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct Tmux {
    sock: String,
    work_home: PathBuf,
    t3home: PathBuf,
}

impl Tmux {
    fn new(work: &Work) -> Tmux {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Tmux { sock: format!("t3golden-{}-{n}", std::process::id()), work_home: work.home.clone(), t3home: work.t3home.clone() }
    }

    /// The same cleared environment the exporter ran the original under: tmux starts its
    /// panes from it, so neither program sees the desktop that replays the recording.
    fn command(&self, sock: &str) -> Command {
        let mut command = Command::new("tmux");
        command
            .args(["-L", sock, "-f", "/dev/null"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into()))
            .env("HOME", &self.work_home)
            .env("LANG", "C.UTF-8")
            .env("TZ", "UTC")
            .env("T3CODE_HOME", &self.t3home)
            // The original's theme behavior, which the recording holds; the product's themes are
            // a deliberate difference, tested in pty.rs.
            .env("T3_AGENTS_NO_THEMES", "1");
        command
    }

    fn run(&self, args: &[&str], allow_fail: bool) -> Vec<u8> {
        let output = self.command(&self.sock).args(args).output().unwrap_or_else(|e| panic!("tmux {args:?}: {e}"));
        if !output.status.success() && !allow_fail {
            panic!("tmux {}: {}{}", args.join(" "), String::from_utf8_lossy(&output.stderr), String::from_utf8_lossy(&output.stdout));
        }
        output.stdout
    }

    fn text(&self, args: &[&str]) -> String {
        String::from_utf8_lossy(&self.run(args, true)).trim().to_string()
    }

    fn server_up(&self) {
        // A tmux server with no session exits at once and a later session starts a fresh one
        // that has forgotten every option: hold it with exit-empty off before the boot session goes.
        self.run(&["new-session", "-d", "-s", "boot", "-x", "80", "-y", "24", "sleep", "30"], false);
        self.run(&["set-option", "-s", "exit-empty", "off"], false);
        for (option, value) in [("status", "off"), ("mouse", "off"), ("escape-time", "0"), ("focus-events", "off"), ("remain-on-exit", "on"), ("history-limit", "200"), ("default-terminal", "tmux-256color")] {
            self.run(&["set-option", "-g", option, value], false);
        }
        self.run(&["kill-session", "-t", "boot"], false);
    }

    fn capture(&self, session: &str) -> Vec<u8> {
        self.run(&["capture-pane", "-p", "-e", "-N", "-t", session], false)
    }

    /// `paste:<hex>[*<n>]`: the text, repeated n times, given to `paste-buffer -p`, which wraps it
    /// in `ESC[200~` and `ESC[201~` only for a program that switched mode 2004 on.
    /// `pieces:<hex>|<ms>|<hex>...`: each piece raw with `send-keys -H`, and a pause between.
    fn send_key(&self, session: &str, key: &str) {
        if let Some(spec) = key.strip_prefix("paste:") {
            let (hex, times) = spec.split_once('*').unwrap_or((spec, "1"));
            let text: String = hex_bytes(hex).iter().map(|b| *b as char).collect::<String>().repeat(times.parse().unwrap());
            // tmux takes a command no longer than a few KB, so the text goes in through a file.
            let file = self.work_home.with_file_name("paste.txt");
            std::fs::write(&file, text.chars().map(|c| c as u8).collect::<Vec<u8>>()).unwrap();
            self.run(&["load-buffer", "-b", "t3paste", file.to_str().unwrap()], false);
            self.run(&["paste-buffer", "-p", "-d", "-b", "t3paste", "-t", session], false);
        } else if let Some(pieces) = key.strip_prefix("pieces:") {
            for piece in pieces.split('|') {
                if let Some(pause) = piece.strip_suffix("ms") {
                    sleep(ms(pause.parse().unwrap()));
                } else {
                    let bytes: Vec<String> = hex_bytes(piece).iter().map(|b| format!("{b:02x}")).collect();
                    let mut args = vec!["send-keys", "-H", "-t", session];
                    args.extend(bytes.iter().map(String::as_str));
                    self.run(&args, false);
                }
            }
        } else if let Some(hex) = key.strip_prefix("hex:") {
            let mut args = vec!["send-keys", "-H", "-t", session];
            args.extend(hex.split(' '));
            self.run(&args, false);
        } else if key.chars().count() == 1 {
            self.run(&["send-keys", "-l", "-t", session, key], false);
        } else {
            self.run(&["send-keys", "-t", session, key], false);
        }
    }

    fn kill_server(&self, sock: &str) {
        let _ = self.command(sock).arg("kill-server").output();
        // A killed server leaves its socket file behind; this test made it, so it removes it.
        let dir = std::env::var_os("TMUX_TMPDIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"));
        let _ = std::fs::remove_file(dir.join(format!("tmux-{}", unsafe { libc::getuid() })).join(sock));
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        self.kill_server(&self.sock);
    }
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len()).step_by(2).map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap()).collect()
}

/// The exports every launch script makes, as the gate writes them.
fn base_exports(work: &Work, term: &str, colorterm: Option<&str>) -> Vec<String> {
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin".into());
    let mut lines = vec![
        format!("export HOME={}", quote(work.home.to_str().unwrap())),
        format!("export T3CODE_HOME={}", quote(work.t3home.to_str().unwrap())),
        format!("export XDG_RUNTIME_DIR={}", quote(work.runtime.to_str().unwrap())),
        "export TZ=UTC".into(),
        "export LANG=C.UTF-8".into(),
        format!("export PATH={}", quote(&format!("{}:{path}", work.stub.display()))),
        format!("export T3_FIXED_NOW={}", quote(NOW)),
        "export T3_AGENTS_NO_THEMES=1".into(),
        format!("export TERM={}", quote(term)),
        "unset HYPRLAND_INSTANCE_SIGNATURE NO_COLOR FORCE_COLOR COLORTERM".into(),
    ];
    if let Some(colorterm) = colorterm {
        lines.push(format!("export COLORTERM={}", quote(colorterm)));
    }
    lines
}

fn app_command(args: &[&str]) -> String {
    std::iter::once(BINARY).chain(args.iter().copied()).map(quote).collect::<Vec<_>>().join(" ")
}

fn launch(tmux: &Tmux, work: &Work, session: &str, cols: u64, rows: u64, term: &str, colorterm: Option<&str>, args: &[&str]) {
    let script = work.dir.join(format!("run-{session}.sh"));
    let mut body = vec!["#!/bin/bash".to_string(), format!("cd {}", quote(repo().to_str().unwrap()))];
    body.extend(base_exports(work, term, colorterm));
    body.push(format!("exec {}", app_command(args)));
    body.push(String::new());
    write_script(&script, &body.join("\n"));
    tmux.run(&["new-session", "-d", "-s", session, "-x", &cols.to_string(), "-y", &rows.to_string(), script.to_str().unwrap()], false);
    let size = tmux.text(&["display-message", "-p", "-t", session, "#{pane_width}x#{pane_height}"]);
    assert_eq!(size, format!("{cols}x{rows}"), "{session} is {size}, wanted {cols}x{rows}");
}

// ── reading a capture, as the gate does ──────────────────────────────────────

#[derive(Clone, PartialEq, Debug)]
struct Cell {
    /// `'\0'` marks the second half of a wide glyph.
    ch: char,
    fg: String,
    bg: String,
    bold: bool,
    extra: String,
}

fn parse_grid(raw: &[u8], cols: usize, rows: usize) -> (Vec<Vec<Cell>>, Vec<String>) {
    let text = String::from_utf8_lossy(raw);
    let chars: Vec<char> = text.chars().collect();
    let mut problems = Vec::new();
    let mut grid: Vec<Vec<Cell>> = Vec::new();
    let mut row: Vec<Cell> = Vec::new();
    let (mut fg, mut bg, mut bold) = ("default".to_string(), "default".to_string(), false);
    let mut extra = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while j < chars.len() && (chars[j] as u32) < 0x40 {
                j += 1;
            }
            if j >= chars.len() {
                problems.push("truncated escape".to_string());
                break;
            }
            let final_byte = chars[j];
            let body: String = chars[i + 2..j].iter().collect();
            i = j + 1;
            if final_byte == 'm' {
                let nums: Vec<f64> = if body.is_empty() {
                    vec![0.0]
                } else {
                    body.split(';').map(|part| if part.is_empty() { 0.0 } else { part.parse::<f64>().unwrap_or(f64::NAN) }).collect()
                };
                let mut extras: std::collections::BTreeSet<String> = extra.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
                let at = |k: usize| nums.get(k).copied();
                let show = |value: Option<f64>| value.map_or("undefined".to_string(), |v| v.to_string());
                let mut k = 0;
                while k < nums.len() {
                    let n = nums[k];
                    if n == 0.0 {
                        fg = "default".into();
                        bg = "default".into();
                        bold = false;
                        extras.clear();
                    } else if n == 1.0 {
                        bold = true;
                    } else if n == 22.0 {
                        bold = false;
                    } else if n == 39.0 {
                        fg = "default".into();
                    } else if n == 49.0 {
                        bg = "default".into();
                    } else if (30.0..=37.0).contains(&n) {
                        fg = format!("ansi({})", n - 30.0);
                    } else if (40.0..=47.0).contains(&n) {
                        bg = format!("ansi({})", n - 40.0);
                    } else if (90.0..=97.0).contains(&n) {
                        fg = format!("ansi({})", n - 90.0 + 8.0);
                    } else if (100.0..=107.0).contains(&n) {
                        bg = format!("ansi({})", n - 100.0 + 8.0);
                    } else if [2.0, 3.0, 4.0, 5.0, 7.0, 9.0].contains(&n) {
                        extras.insert(n.to_string());
                    } else if [23.0, 24.0, 25.0, 27.0, 29.0].contains(&n) {
                        extras.remove(&(n - 20.0).to_string());
                    } else if (n == 38.0 || n == 48.0) && at(k + 1) == Some(5.0) {
                        let value = format!("idx({})", show(at(k + 2)));
                        if n == 38.0 { fg = value } else { bg = value }
                        k += 2;
                    } else if (n == 38.0 || n == 48.0) && at(k + 1) == Some(2.0) {
                        let value = format!("rgb({},{},{})", show(at(k + 2)), show(at(k + 3)), show(at(k + 4)));
                        if n == 38.0 { fg = value } else { bg = value }
                        k += 4;
                    }
                    k += 1;
                }
                extra = extras.into_iter().collect::<Vec<_>>().join(",");
            }
            continue;
        }
        i += 1;
        if c == '\n' {
            grid.push(std::mem::take(&mut row));
            continue;
        }
        let width = cell_width(c);
        if width == 0 {
            continue;
        }
        row.push(Cell { ch: c, fg: fg.clone(), bg: bg.clone(), bold, extra: extra.clone() });
        for _ in 1..width {
            row.push(Cell { ch: '\0', fg: fg.clone(), bg: bg.clone(), bold, extra: extra.clone() });
        }
    }
    if !row.is_empty() {
        grid.push(row);
    }
    if grid.len() != rows {
        problems.push(format!("rows {} != {rows}", grid.len()));
    }
    for (y, line) in grid.iter().enumerate() {
        if line.len() != cols {
            problems.push(format!("row {y} width {} != {cols}", line.len()));
        }
    }
    (grid, problems)
}

/// The digest `gridDigest` in terminal-parity.ts writes.
fn grid_digest(grid: &[Vec<Cell>]) -> String {
    let rows: Vec<String> = grid
        .iter()
        .map(|row| {
            row.iter()
                .map(|c| format!("{}:{}:{}:{}:{}", if c.ch == '\0' { 0 } else { c.ch as u32 }, c.fg, c.bg, u8::from(c.bold), c.extra))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    support::fnv1a(&rows.join("\n"))
}

fn visible(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'[') {
            let mut j = i + 2;
            while j < chars.len() && (chars[j].is_ascii_digit() || chars[j] == ';') {
                j += 1;
            }
            if chars.get(j) == Some(&'m') {
                i = j + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn describe(cell: Option<&Cell>) -> String {
    match cell {
        None => "missing".into(),
        Some(c) => {
            let glyph = if c.ch == '\0' { "CONT".to_string() } else { format!("{:?}", c.ch.to_string()) };
            format!("{glyph} fg={} bg={} bold={}{}", c.fg, c.bg, c.bold, if c.extra.is_empty() { String::new() } else { format!(" attr={}", c.extra) })
        }
    }
}

/// What the gate prints when two captures differ: how many cells, the first few, and the rows.
fn compare_grids(recorded: &[Vec<Cell>], rust: &[Vec<Cell>]) -> Option<String> {
    let height = recorded.len().max(rust.len());
    let (mut count, mut samples, mut rows) = (0, Vec::new(), Vec::new());
    for y in 0..height {
        let a = recorded.get(y).map(Vec::as_slice).unwrap_or(&[]);
        let b = rust.get(y).map(Vec::as_slice).unwrap_or(&[]);
        let mut row_differs = false;
        for x in 0..a.len().max(b.len()) {
            if a.get(x) != b.get(x) {
                count += 1;
                row_differs = true;
                if samples.len() < 6 {
                    samples.push(format!("({x},{y}) recorded {} rust {}", describe(a.get(x)), describe(b.get(x))));
                }
            }
        }
        if row_differs && rows.len() < 8 {
            let text = |line: &[Cell]| line.iter().filter(|c| c.ch != '\0').map(|c| c.ch).collect::<String>();
            rows.push(format!("row {y} recorded {:?} rust {:?}", text(a), text(b)));
        }
    }
    (count > 0).then(|| format!("{count} cells differ: {}; {}", samples.join(" | "), rows.join("; ")))
}

// ── waiting for the screen to settle, as the gate does ───────────────────────

fn wait_stable(tmux: &Tmux, session: &str, timeout: Duration) -> Result<Vec<u8>, String> {
    let start = Instant::now();
    let mut previous: Option<Vec<u8>> = None;
    let mut last = Vec::new();
    while start.elapsed() < timeout {
        last = tmux.capture(session);
        let text = visible(&last);
        let ready = text.contains("T3 Agents") || text.contains("help") || text.contains("Calm");
        if previous.as_deref() == Some(last.as_slice()) && !last.is_empty() && ready {
            return Ok(last);
        }
        previous = Some(last.clone());
        sleep(ms(80));
    }
    let dead = tmux.text(&["display-message", "-p", "-t", session, "#{pane_dead}"]);
    Err(format!("{session} did not settle in {}ms (dead={dead})\n{}", timeout.as_millis(), visible(&last).chars().take(500).collect::<String>()))
}

/// The gate's `afterBoth` for one pane. Calm mode paints at 2 fps, so two captures 80 ms apart
/// can both still be the frame from before the key; a changed frame is accepted only after it
/// has held for one idle frame, and a no-op only after that same idle frame has passed.
fn after(tmux: &Tmux, session: &str, before: &[u8], cols: usize, rows: usize) -> Vec<u8> {
    let start = Instant::now();
    let deadline = start + ms(5_000);
    let mut previous: Option<Vec<u8>> = None;
    let mut held: Option<Instant> = None;
    let mut last = before.to_vec();
    while Instant::now() < deadline {
        last = tmux.capture(session);
        let fits = parse_grid(&last, cols, rows).1.is_empty();
        let same_as_prev = previous.as_deref() == Some(last.as_slice()) && !last.is_empty();
        if fits && same_as_prev {
            let since = held.unwrap_or_else(Instant::now);
            let waited = since.elapsed();
            let changed = last != before;
            held = Some(since);
            if changed && waited >= ms(600) {
                return last;
            }
            if !changed && start.elapsed() >= ms(800) && waited >= ms(160) {
                return last;
            }
        } else {
            held = None;
        }
        previous = Some(last.clone());
        sleep(ms(80));
    }
    last
}

// ── the recording ────────────────────────────────────────────────────────────

fn fixture() -> Value {
    support::load(FIXTURE)
}

fn cases_of<'a>(fixture: &'a Value, kind: &str) -> Vec<&'a Value> {
    fixture["cases"].as_array().unwrap().iter().filter(|case| case["kind"] == kind).collect()
}

fn asset_text<'a>(fixture: &'a Value, token: &str) -> &'a str {
    fixture["assets"][token.trim_start_matches('@')]["text"].as_str().unwrap_or_else(|| panic!("the recording has no asset {token}"))
}

fn base64_decode(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut buffer, mut bits) = (0u32, 0);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        } as u32;
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    out
}

/// Writes the synthetic database the original read, byte for byte, and returns its path.
fn write_database(fixture: &Value, work: &Work) -> PathBuf {
    let database = cases_of(fixture, "database").into_iter().next().expect("the recording holds the database");
    let dir = work.dir.join("db");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("statev2.sqlite"), base64_decode(database["sqlite"].as_str().unwrap())).unwrap();
    std::fs::write(dir.join("environment-id"), database["environmentId"].as_str().unwrap()).unwrap();
    dir.join("statev2.sqlite")
}

// ── screen cases ─────────────────────────────────────────────────────────────

fn screen_case(scenario: &str) {
    if !needs_tmux(scenario) {
        return;
    }
    let _permit = permit();
    let fixture = fixture();
    let steps: Vec<&Value> = cases_of(&fixture, "screen").into_iter().filter(|case| case["scenario"] == scenario).collect();
    assert!(!steps.is_empty(), "the recording has no screen case `{scenario}`");
    let first = steps[0];
    let (cols, rows) = (first["cols"].as_u64().unwrap(), first["rows"].as_u64().unwrap());
    let term = first["term"].as_str().unwrap();
    let colorterm = first["colorterm"].as_str();
    let work = Work::new("terminal");
    let db = if first["source"] == "db" { Some(write_database(&fixture, &work)) } else { None };
    let mut args = vec!["--calm", "--interval", "30"];
    match &db {
        Some(path) => args.extend(["--db", path.to_str().unwrap()]),
        None => args.push("--demo"),
    }
    let tmux = Tmux::new(&work);
    tmux.server_up();
    let mut failures: Vec<String> = Vec::new();
    let mut compared = 0;
    // Key bytes come from tmux's terminfo, so the pane's terminal has to be the one the
    // application is told it is running in.
    tmux.run(&["set-option", "-g", "default-terminal", term], false);
    launch(&tmux, &work, "rust", cols, rows, term, colorterm, &args);
    let mut check = |step: &Value, raw: &[u8], failures: &mut Vec<String>| {
        compared += 1;
        let name = step["step"].as_str().unwrap();
        let (cols, rows) = (step["cols"].as_u64().unwrap() as usize, step["rows"].as_u64().unwrap() as usize);
        let (recorded, recorded_problems) = parse_grid(asset_text(&fixture, step["capture"].as_str().unwrap()).as_bytes(), cols, rows);
        if !recorded_problems.is_empty() || grid_digest(&recorded) != step["cells"].as_str().unwrap() {
            failures.push(format!("{scenario} {name}: this test's capture parser does not read the recorded screen the way the original's did (digest {} against recorded {}; problems {recorded_problems:?}). Fix the parser, not the program", grid_digest(&recorded), step["cells"].as_str().unwrap()));
            return;
        }
        let (rust, problems) = parse_grid(raw, cols, rows);
        let mut found: Vec<String> = Vec::new();
        if !problems.is_empty() {
            found.push(format!("rust capture: {}", problems.join("; ")));
        }
        if let Some(difference) = compare_grids(&recorded, &rust) {
            found.push(difference);
        }
        if !found.is_empty() {
            failures.push(format!("{scenario} {name}: {}", found.join("; ")));
        }
    };
    let result: Result<(), String> = (|| {
        // The gate takes one capture once the screen settles and another, back to back, once it has
        // stopped changing on its own; the second is the one compared.
        wait_stable(&tmux, "rust", ms(15_000))?;
        let mut raw = wait_stable(&tmux, "rust", ms(4_000))?;
        check(steps[0], &raw, &mut failures);
        let (mut cur_cols, mut cur_rows) = (cols as usize, rows as usize);
        for step in &steps[1..] {
            let before = raw.clone();
            if let Some(size) = step["resize"].as_array() {
                cur_cols = size[0].as_u64().unwrap() as usize;
                cur_rows = size[1].as_u64().unwrap() as usize;
                tmux.run(&["resize-window", "-t", "rust", "-x", &cur_cols.to_string(), "-y", &cur_rows.to_string()], false);
            } else {
                tmux.send_key("rust", step["key"].as_str().unwrap());
            }
            if (step["cols"].as_u64().unwrap() as usize, step["rows"].as_u64().unwrap() as usize) != (cur_cols, cur_rows) {
                return Err(format!("{scenario} {}: the recording says {}x{}, the replay is {cur_cols}x{cur_rows}", step["step"], step["cols"], step["rows"]));
            }
            raw = after(&tmux, "rust", &before, cur_cols, cur_rows);
            check(step, &raw, &mut failures);
        }
        Ok(())
    })();
    if let Err(error) = result {
        failures.push(format!("error: {scenario}: {error}"));
    }
    tmux.run(&["kill-session", "-t", "rust"], true);
    assert!(
        failures.is_empty(),
        "{} of {} screens of `{scenario}` differ from the recording (recorded from {}):\n{}",
        failures.len(),
        steps.len(),
        support::recorded_from(&fixture),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
    println!("{scenario}: {compared} screens match the recording");
}

#[test]
fn screen_demo_80x24_tmux_truecolor() {
    screen_case("demo 80x24 tmux-256color truecolor");
}
#[test]
fn screen_demo_120x40_tmux_truecolor() {
    screen_case("demo 120x40 tmux-256color truecolor");
}
#[test]
fn screen_demo_233x55_tmux_truecolor() {
    screen_case("demo 233x55 tmux-256color truecolor");
}
#[test]
fn screen_demo_60x18_tmux_truecolor() {
    screen_case("demo 60x18 tmux-256color truecolor");
}
#[test]
fn screen_db_80x24_tmux_truecolor() {
    screen_case("db 80x24 tmux-256color truecolor");
}
#[test]
fn screen_db_120x40_tmux_truecolor() {
    screen_case("db 120x40 tmux-256color truecolor");
}
#[test]
fn screen_db_233x55_tmux_truecolor() {
    screen_case("db 233x55 tmux-256color truecolor");
}
#[test]
fn screen_db_60x18_tmux_truecolor() {
    screen_case("db 60x18 tmux-256color truecolor");
}
#[test]
fn screen_demo_80x24_tmux_no_colorterm() {
    screen_case("demo 80x24 tmux-256color no-colorterm");
}
#[test]
fn screen_demo_80x24_xterm_truecolor() {
    screen_case("demo 80x24 xterm-256color truecolor");
}
#[test]
fn screen_demo_80x24_xterm_no_colorterm() {
    screen_case("demo 80x24 xterm-256color no-colorterm");
}
#[test]
fn screen_demo_80x24_foot_truecolor() {
    screen_case("demo 80x24 foot truecolor");
}
#[test]
fn screen_demo_80x24_foot_no_colorterm() {
    screen_case("demo 80x24 foot no-colorterm");
}
#[test]
fn screen_db_80x24_tmux_no_colorterm() {
    screen_case("db 80x24 tmux-256color no-colorterm");
}
/// Bracketed paste: pastes of quit keys, newlines, escape sequences and 100 KB, a paste in three
/// reads, each followed by a `j`, replayed from what the original showed.
#[test]
fn screen_paste_demo_80x24_tmux_truecolor() {
    screen_case("paste demo 80x24 tmux-256color truecolor");
}

/// The recording and the tests above cover the same 14 screen cases, 38 screens each.
#[test]
fn recording_holds_exactly_the_screen_cases_replayed_here() {
    let fixture = fixture();
    let mut scenarios: BTreeMap<String, usize> = BTreeMap::new();
    for case in cases_of(&fixture, "screen") {
        *scenarios.entry(case["scenario"].as_str().unwrap().to_string()).or_default() += 1;
    }
    let tested = [
        "demo 80x24 tmux-256color truecolor", "demo 120x40 tmux-256color truecolor", "demo 233x55 tmux-256color truecolor", "demo 60x18 tmux-256color truecolor",
        "db 80x24 tmux-256color truecolor", "db 120x40 tmux-256color truecolor", "db 233x55 tmux-256color truecolor", "db 60x18 tmux-256color truecolor",
        "demo 80x24 tmux-256color no-colorterm", "demo 80x24 xterm-256color truecolor", "demo 80x24 xterm-256color no-colorterm",
        "demo 80x24 foot truecolor", "demo 80x24 foot no-colorterm", "db 80x24 tmux-256color no-colorterm",
    ];
    let paste = "paste demo 80x24 tmux-256color truecolor";
    let mut recorded: Vec<&str> = scenarios.keys().map(String::as_str).filter(|name| *name != paste).collect();
    recorded.sort_unstable();
    let mut expected = tested.to_vec();
    expected.sort_unstable();
    assert_eq!(recorded, expected, "the screen cases in the recording and the tests in this file");
    assert!(scenarios.iter().filter(|(name, _)| name.as_str() != paste).all(|(_, screens)| *screens == 38), "every screen case holds its initial frame and 37 steps: {scenarios:?}");
    // The bracketed-paste case: the initial frame and 11 steps (`PASTE_PLAN` in the gate).
    assert_eq!(scenarios.get(paste), Some(&12), "the paste case holds its initial frame and 11 steps: {scenarios:?}");
}

// ── what the program leaves behind on exit ───────────────────────────────────

fn wait_file(path: &Path, timeout: Duration) -> Result<String, String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.trim().is_empty() {
                return Ok(text.trim().to_string());
            }
        }
        sleep(ms(50));
    }
    Err(format!("timed out waiting for {}", path.display()))
}

fn read_text(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Modes a pane should be handed back in: parsed from the bytes the program wrote.
fn final_modes(raw: &[u8]) -> BTreeMap<String, String> {
    let mut state: BTreeMap<String, bool> = [("1", false), ("7", true), ("25", true), ("1000", false), ("1002", false), ("1003", false), ("1004", false), ("1006", false), ("1049", false), ("2004", false), ("2026", false), ("2027", false), ("2031", false)]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    let (mut modify, mut kitty) = ("0".to_string(), "off".to_string());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] != 0x1b {
            i += 1;
            continue;
        }
        if raw.get(i + 1) != Some(&b'[') {
            i += 2;
            continue;
        }
        let mut j = i + 2;
        let marker = raw.get(j).copied();
        let (private, gt) = (marker == Some(b'?'), marker == Some(b'>'));
        if private || gt {
            j += 1;
        }
        let start = j;
        while j < raw.len() && (0x20..=0x3f).contains(&raw[j]) {
            j += 1;
        }
        if j >= raw.len() {
            break;
        }
        let final_byte = raw[j];
        let body = String::from_utf8_lossy(&raw[start..j]).to_string();
        if private && (final_byte == b'h' || final_byte == b'l') {
            for part in body.split(';') {
                if !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()) {
                    state.insert(part.to_string(), final_byte == b'h');
                }
            }
        } else if gt && final_byte == b'm' && body.starts_with("4;") {
            modify = body[2..].to_string();
        } else if gt && final_byte == b'u' {
            kitty = if body.is_empty() { "0".into() } else { body };
        }
        i = j + 1;
    }
    let mut out: BTreeMap<String, String> = state.into_iter().map(|(k, v)| (k, if v { "on" } else { "off" }.to_string())).collect();
    out.insert("kitty".into(), kitty);
    out.insert("modifyOtherKeys".into(), modify);
    out
}

fn exit_case(how: &str) {
    if !needs_tmux(&format!("exit {how}")) {
        return;
    }
    let _permit = permit();
    let fixture = fixture();
    let recorded = cases_of(&fixture, "exit").into_iter().find(|case| case["how"] == how).unwrap_or_else(|| panic!("the recording has no `exit {how}`"));
    let work = Work::new("terminal-exit");
    let dir = work.dir.join("exit-rust");
    std::fs::create_dir_all(&dir).unwrap();
    let file = |name: &str| dir.join(name);
    let script = file("run.sh");
    let mut body = vec!["#!/bin/bash".to_string(), format!("cd {}", quote(repo().to_str().unwrap()))];
    body.extend(base_exports(&work, "tmux-256color", Some("truecolor")));
    body.extend([
        format!("stty -g > {}", quote(file("stty-before").to_str().unwrap())),
        format!("stty -a > {}", quote(file("stty-a-before").to_str().unwrap())),
        "(".into(),
        // The subshell's pid, which `exec` hands to the app. Not `$BASHPID`: macOS's /bin/bash is
        // 3.2, which has none. A child's `$PPID` is the same number on every bash.
        format!("  sh -c 'echo $PPID' > {}", quote(file("pid").to_str().unwrap())),
        // The app waits here so pipe-pane is attached before the first query.
        format!("  while [ ! -f {} ]; do sleep 0.02; done", quote(file("go").to_str().unwrap())),
        format!("  exec {}", app_command(&["--calm", "--interval", "30", "--demo"])),
        ")".into(),
        "status=$?".into(),
        format!("echo $status > {}", quote(file("exit").to_str().unwrap())),
        format!("stty -g > {}", quote(file("stty-after").to_str().unwrap())),
        format!("stty -a > {}", quote(file("stty-a-after").to_str().unwrap())),
        "echo DONE:$status".into(),
        "sleep 30".into(),
        String::new(),
    ]);
    write_script(&script, &body.join("\n"));
    let tmux = Tmux::new(&work);
    tmux.server_up();
    tmux.run(&["set-option", "-g", "default-terminal", "tmux-256color"], false);
    tmux.run(&["new-session", "-d", "-s", "rust", "-x", "80", "-y", "24", script.to_str().unwrap()], false);
    wait_file(&file("pid"), ms(5_000)).unwrap();
    tmux.run(&["pipe-pane", "-t", "rust", "-o", &format!("cat > {}", file("raw.bin").display())], false);
    std::fs::write(file("go"), "1").unwrap();
    wait_stable(&tmux, "rust", ms(15_000)).unwrap();
    // `paste+...`: a bracketed paste of quit keys first; the program must still be running after it.
    let action = how.strip_prefix("paste+").unwrap_or(how);
    let mut survived_paste: Option<bool> = None;
    if action != how {
        tmux.send_key("rust", &format!("paste:{}", "qqqq q?jk".bytes().map(|b| format!("{b:02x}")).collect::<String>()));
        sleep(ms(1_500));
        survived_paste = Some(!visible(&tmux.capture("rust")).contains("DONE:"));
    }
    match action {
        "q" => tmux.send_key("rust", "q"),
        "C-c" => tmux.send_key("rust", "C-c"),
        signal => {
            let pid = wait_file(&file("pid"), ms(1_000)).unwrap();
            let name = if signal == "SIGTERM" { "TERM" } else { "HUP" };
            assert!(Command::new("kill").args([&format!("-{name}"), &pid]).status().unwrap().success(), "kill -{name} {pid}");
        }
    }
    let exit_code = wait_file(&file("exit"), ms(8_000)).unwrap();
    sleep(ms(200));
    let raw_out = std::fs::read(file("raw.bin")).unwrap_or_default();
    let mut problems: Vec<String> = Vec::new();
    if survived_paste != recorded["survivedPaste"].as_bool() {
        problems.push(format!("survived a bracketed paste of quit keys: {survived_paste:?}, the original: {}", recorded["survivedPaste"]));
    }
    if survived_paste.is_some() {
        // It asked for bracketed paste (tmux sends the markers only then) and gave it back.
        let text: String = raw_out.iter().map(|b| *b as char).collect();
        match (text.rfind("\x1b[?2004h"), text.rfind("\x1b[?2004l")) {
            (None, _) => problems.push("never switched mode 2004 on".into()),
            (Some(on), Some(off)) if off > on => {}
            _ => problems.push("did not switch mode 2004 off after the last time it switched it on".into()),
        }
    }
    if exit_code != recorded["exitCode"].as_str().unwrap() {
        problems.push(format!("exit code {} where the original's was {}", exit_code, recorded["exitCode"]));
    }
    let restored = read_text(&file("stty-after")) == read_text(&file("stty-before")) && read_text(&file("stty-a-after")) == read_text(&file("stty-a-before"));
    if restored != recorded["sttyRestored"].as_bool().unwrap() {
        problems.push(format!("stty restored: {restored}, the original: {}", recorded["sttyRestored"]));
    }
    if raw_out.len() < 16 {
        problems.push(format!("raw output is {} bytes", raw_out.len()));
    }
    let modes = final_modes(&raw_out);
    let theirs: BTreeMap<String, String> = recorded["modes"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string())).collect();
    let keys: std::collections::BTreeSet<&String> = modes.keys().chain(theirs.keys()).collect();
    // The original turns mode 2027 (grapheme clustering) on and leaves it on when it exits;
    // the Rust program never touches it. That one difference is deliberate and counted: it
    // must be exactly "original on, Rust off", and anything else still fails.
    let mut deliberate = 0;
    let differing: Vec<String> = keys
        .into_iter()
        .filter(|key| {
            let (a, b) = (theirs.get(*key), modes.get(*key));
            if a == b {
                return false;
            }
            if key.as_str() == "2027" && a.map(String::as_str) == Some("on") && b.map(String::as_str) == Some("off") {
                deliberate += 1;
                return false;
            }
            true
        })
        .map(|key| format!("{key} original={} rust={}", theirs.get(key).map_or("-", String::as_str), modes.get(key).map_or("-", String::as_str)))
        .collect();
    if !differing.is_empty() {
        problems.push(format!("terminal modes differ: {}", differing.join(", ")));
    }
    for key in ["1000", "1002", "1003", "1004", "1006", "1049", "2004", "2026"] {
        if modes[key] == "on" {
            problems.push(format!("rust left mode {key} on"));
        }
    }
    if modes["25"] != "on" {
        problems.push("rust left the cursor hidden".into());
    }
    if modes["kitty"] != "off" {
        problems.push(format!("kitty keyboard rust={}", modes["kitty"]));
    }
    if modes["modifyOtherKeys"] != "0" {
        problems.push(format!("modifyOtherKeys rust={}", modes["modifyOtherKeys"]));
    }
    let main_screen = visible(&tmux.capture("rust")).contains("DONE:");
    if main_screen != recorded["mainScreen"].as_bool().unwrap() {
        problems.push(format!("pane back on the main screen: {main_screen}, the original: {}", recorded["mainScreen"]));
    }
    tmux.run(&["kill-session", "-t", "rust"], true);
    assert!(problems.is_empty(), "exit {how} differs from the recording (recorded from {}): {}", support::recorded_from(&fixture), problems.join("; "));
    println!("exit {how}: exit code {exit_code}, modes and stty match the recording ({deliberate} deliberate difference: mode 2027)");
}

#[test]
fn exit_q() {
    exit_case("q");
}
#[test]
fn exit_sigterm() {
    exit_case("SIGTERM");
}
#[test]
fn exit_sighup() {
    exit_case("SIGHUP");
}
#[test]
fn exit_ctrl_c() {
    exit_case("C-c");
}
#[test]
fn exit_paste_then_q() {
    exit_case("paste+q");
}
#[test]
fn exit_paste_then_sigterm() {
    exit_case("paste+SIGTERM");
}

// ── odd sizes, a dumb terminal, a nested tmux ────────────────────────────────

fn survival_case(label: &str) {
    if !needs_tmux(label) {
        return;
    }
    let _permit = permit();
    let fixture = fixture();
    let recorded = cases_of(&fixture, "survival").into_iter().find(|case| case["label"] == label).unwrap_or_else(|| panic!("the recording has no `{label}`"));
    let (cols, rows) = (recorded["cols"].as_u64().unwrap(), recorded["rows"].as_u64().unwrap());
    let term = recorded["term"].as_str().unwrap();
    let inner = recorded["inner"].as_bool().unwrap();
    let work = Work::new("terminal-survival");
    let tmux = Tmux::new(&work);
    tmux.server_up();
    let inner_sock = format!("{}-rust", tmux.sock);
    let observe = || -> String {
        let args = ["--calm", "--demo"];
        if !inner {
            launch(&tmux, &work, "rust", cols, rows, term, Some("truecolor"), &args);
        } else {
            let inner_script = work.dir.join("inner-rust.sh");
            let mut body = vec!["#!/bin/bash".to_string(), format!("cd {}", quote(repo().to_str().unwrap()))];
            body.extend(base_exports(&work, "tmux-256color", Some("truecolor")));
            body.push(format!("exec {}", app_command(&args)));
            body.push(String::new());
            write_script(&inner_script, &body.join("\n"));
            let outer = work.dir.join("outer-rust.sh");
            write_script(
                &outer,
                &format!("#!/bin/bash\nexec tmux -L {} -f /dev/null new-session -x {} -y {} {}\n", quote(&inner_sock), cols - 4, rows - 2, quote(inner_script.to_str().unwrap())),
            );
            tmux.run(&["new-session", "-d", "-s", "rust", "-x", &cols.to_string(), "-y", &rows.to_string(), outer.to_str().unwrap()], false);
        }
        sleep(ms(2_500));
        let dead = tmux.text(&["display-message", "-p", "-t", "rust", "#{pane_dead}"]);
        if dead == "1" {
            return format!("dead during startup: {}", visible(&tmux.capture("rust")).chars().take(240).collect::<String>());
        }
        if !inner {
            tmux.send_key("rust", "q");
        } else {
            // q typed at the outer pane can stop at the inner client; send it to the
            // application's pane on the inner server.
            let _ = tmux.command(&inner_sock).args(["send-keys", "q"]).output();
        }
        sleep(ms(1_500));
        let dead_now = tmux.text(&["display-message", "-p", "-t", "rust", "#{pane_dead}"]);
        let text: String = visible(&tmux.run(&["capture-pane", "-p", "-t", "rust"], true)).chars().take(240).collect();
        if inner {
            let output = tmux.command(&inner_sock).args(["display-message", "-p", "#{pane_current_command}"]).output();
            if let Ok(output) = output {
                let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if output.status.success() && (command == "node" || command.contains("t3-agents")) {
                    return format!("still running after q (inner {command}): {text}");
                }
            }
        } else if dead_now != "1" && !text.contains("DONE") && !text.to_lowercase().contains("exit") {
            let command = tmux.text(&["display-message", "-p", "-t", "rust", "#{pane_current_command}"]);
            if command == "node" || command.contains("t3-agents") {
                return format!("still running after q ({command}): {text}");
            }
        }
        format!("alive then quit ({})", text.split_whitespace().collect::<Vec<_>>().join(" "))
    };
    let outcome = observe();
    tmux.run(&["kill-session", "-t", "rust"], true);
    if inner {
        tmux.kill_server(&inner_sock);
    }
    // The gate's verdict is a pattern match on the outcome, and the pane's own "Pane is dead"
    // text satisfies it for some runs; it is reproduced as it is. The class of outcome (before
    // any detail) is held as well: neither program may fail at startup or keep running after q.
    let class = outcome.split([':', '(']).next().unwrap_or("").trim().to_string();
    assert_eq!(class, recorded["outcome"].as_str().unwrap(), "{label}: the original's outcome was `{}`, the Rust program's is `{outcome}`", recorded["outcome"]);
    let bad = ["dead", "still running", "error"].iter().any(|word| outcome.contains(word));
    assert_eq!(
        bad,
        recorded["bad"].as_bool().unwrap(),
        "{label}: the original {} ({}), the Rust program {} ({outcome}); recorded from {}",
        if recorded["bad"].as_bool().unwrap() { "failed" } else { "started and quit" },
        recorded["outcome"].as_str().unwrap_or(""),
        if bad { "failed" } else { "started and quit" },
        support::recorded_from(&fixture)
    );
    println!("{label}: {outcome}");
}

#[test]
fn survival_term_dumb() {
    survival_case("TERM=dumb");
}
#[test]
fn survival_size_10x3() {
    survival_case("size 10x3");
}
#[test]
fn survival_size_1x1() {
    survival_case("size 1x1");
}
#[test]
fn survival_nested_tmux() {
    survival_case("nested tmux");
}

// ── not on a terminal (no tmux needed) ───────────────────────────────────────

#[test]
fn not_a_terminal_matches_the_recording() {
    let fixture = fixture();
    let recorded = cases_of(&fixture, "not-a-terminal").into_iter().next().expect("the recording has the not-a-terminal case");
    let work = Work::new("terminal-plain");
    let path = format!("{}:{}", work.stub.display(), std::env::var("PATH").unwrap_or_default());
    let output = Command::new(BINARY)
        .arg("--demo")
        .current_dir(repo())
        .env_clear()
        .env("PATH", path)
        .env("HOME", &work.home)
        .env("T3CODE_HOME", &work.t3home)
        .env("XDG_RUNTIME_DIR", &work.runtime)
        .env("TZ", "UTC")
        .env("T3_FIXED_NOW", NOW)
        .env("T3_AGENTS_NO_THEMES", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let mut problems = Vec::new();
    if i64::from(output.status.code().unwrap_or(-1)) != recorded["status"].as_i64().unwrap() {
        problems.push(format!("exit {:?} where the original exited {}", output.status.code(), recorded["status"]));
    }
    if String::from_utf8_lossy(&output.stderr) != recorded["stderr"].as_str().unwrap() {
        problems.push(format!("stderr {:?} against {:?}", String::from_utf8_lossy(&output.stderr), recorded["stderr"]));
    }
    if String::from_utf8_lossy(&output.stdout) != recorded["stdout"].as_str().unwrap() {
        problems.push(format!("stdout {:?} against {:?}", String::from_utf8_lossy(&output.stdout), recorded["stdout"]));
    }
    assert!(problems.is_empty(), "not a terminal: {} (recorded from {})", problems.join("; "), support::recorded_from(&fixture));
}
