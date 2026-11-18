//! The product command line, without a screen.
//!
//! Ports `parseArgs` and the headless halves of `oracle/src/main.ts`: `--help`, `--once`
//! (plain text), `--status`, and the settings an interactive presenter needs. Product flags are
//! recognised unless one of the experiment-only flags (used by the parity gates) is present, so
//! `cli::headless` can keep serving both front doors from the same binary.
use crate::{
    clock, demo, integration,
    model::{self, Agent, Snapshot, State},
    source::{self, SourceError},
    status, terminal, theme,
};
use std::{
    ffi::OsStr,
    io::{self, IsTerminal, Write},
    path::{Component, Path, PathBuf},
};

/// Byte-for-byte `USAGE` from `oracle/src/main.ts`.
pub const USAGE: &str = "Usage: t3-agents [--window] [--once | --status [--watch]] [--db PATH | --demo] [--interval SECONDS] [--calm]

  --window          open in a dedicated terminal window, or focus it (launcher only)
  --once            print a plain-text snapshot and exit
  --status          print a one-line JSON status (used by the Omarchy bar widget)
  --watch           with --status, print a new line whenever the status changes
  --db PATH         T3 statev2.sqlite (default: ${T3CODE_HOME:-~/.t3}/userdata/statev2.sqlite)
  --demo            show made-up agents and usage instead of yours, to try it out or take
                    screenshots; reads no T3 or Omarchy data except the theme
  --interval SECS   data refresh period, minimum 0.5 (default 2)
  --calm            start in calm mode: nothing animates (on by default when Omarchy's
                    animations are off; m toggles it)
";

/// What `--help` adds to [`USAGE`] where the product's themes are on (not with
/// `T3_AGENTS_NO_THEMES`, the original's behavior, which the parity checks compare).
pub const THEME_USAGE: &str = "  --theme NAME      color theme: a built-in, a theme file, omarchy (follow
                    Omarchy's live theme), or dark:NAME,light:NAME to follow
                    light and dark mode; c and C step through them while it runs
  --themes          list the themes and say which one is in use
  --theme-check NAME
                    a theme's colors, contrast and colorblind separation
";

/// `--help`: the original's text, and the theme options where themes are on.
pub fn usage(themes: bool) -> String {
    if themes {
        format!("{USAGE}{THEME_USAGE}")
    } else {
        USAGE.to_owned()
    }
}

/// The original's message when the interactive dashboard has no terminal to draw on.
pub const NOT_A_TERMINAL: &str = "Run in a terminal, or use --once for a plain-text snapshot.\n";

/// Where an implicit database path came from; `advice` names it in its guidance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Db,
    Env,
    Default,
}

/// What a run reads: an explicit or default database path, or the in-memory demo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Database {
    Path { path: PathBuf, origin: Origin },
    Demo,
}

/// A parsed product invocation, before the clock is read.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Help,
    Status { database: Database, watch: bool, now: Option<f64> },
    Once { database: Database, now: Option<f64> },
    Interactive { database: Database, interval: f64, calm: bool, theme: Option<String> },
    /// `--themes`.
    Themes { theme: Option<String> },
    /// `--theme-check NAME`.
    ThemeCheck { name: String },
}

/// A usage error: `message`, a blank line, then [`USAGE`] on stderr, exiting 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for UsageError {}

/// The environment `main.ts` reads: the two data-folder overrides and Omarchy's animation flag.
#[derive(Clone, Debug, Default)]
pub struct Environment {
    pub t3code_home: Option<std::ffi::OsString>,
    pub home: Option<std::ffi::OsString>,
    /// Omarchy's `animationsOff()`. `None` detects it only when an interactive command is
    /// built, so `--once` and `--status` never look at the user's state directory.
    pub animations_off: Option<bool>,
    /// The product's themes, and their options, are on: not under `T3_AGENTS_NO_THEMES`.
    pub themes: bool,
}

impl Environment {
    pub fn current() -> Self {
        Self {
            t3code_home: std::env::var_os("T3CODE_HOME"),
            home: crate::platform::home_dir(),
            animations_off: None,
            themes: !theme::disabled(),
        }
    }
}

/// How `T3CODE_HOME` and the home directory become T3's data folder.
///
/// The two rule sets differ, and which one applies is chosen per platform by
/// [`PLATFORM_HOME_RULES`]. Both are pure, so both are tested on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HomeRules {
    /// The original app (`main.ts`): `process.env.T3CODE_HOME || join(homedir(), ".t3")`. The
    /// value is used as given: not trimmed, `~` not expanded, a relative path left relative.
    Original,
    /// T3 Code's own (`resolveBaseDir` in its `os-jank.ts`, and the npm launcher's `t3Home()`):
    /// a missing or blank value means `join(home, ".t3")`; any other value is trimmed, a leading
    /// `~`, `~/` or `~\` becomes the home directory, and the result is made absolute.
    T3,
}

/// Linux keeps the original app's rules, because the original is the reference there and the
/// parity gates compare the two byte for byte. Every other system follows T3, whose data folder
/// this program has to find. To make Linux follow T3 as well, change this one line; the cases
/// that would change are listed in the tests below.
pub const PLATFORM_HOME_RULES: HomeRules = if cfg!(target_os = "linux") {
    HomeRules::Original
} else {
    HomeRules::T3
};

/// `${T3CODE_HOME:-~/.t3}/userdata/statev2.sqlite`, by this platform's [`HomeRules`].
pub fn default_database() -> PathBuf {
    default_database_for(
        std::env::var_os("T3CODE_HOME").as_deref(),
        crate::platform::home_dir().as_deref(),
    )
}

/// [`default_database`] with the environment supplied by the caller.
pub fn default_database_for(t3code_home: Option<&OsStr>, home: Option<&OsStr>) -> PathBuf {
    let cwd = std::env::current_dir().ok();
    database_in(t3code_home, home, cwd.as_deref(), PLATFORM_HOME_RULES)
}

/// `userdata/statev2.sqlite` under T3's data folder. Pure: every input is a parameter.
pub fn database_in(
    t3code_home: Option<&OsStr>,
    home: Option<&OsStr>,
    cwd: Option<&Path>,
    rules: HomeRules,
) -> PathBuf {
    data_folder(t3code_home, home, cwd, rules)
        .join("userdata")
        .join("statev2.sqlite")
}

/// Whether `T3CODE_HOME` names a folder at all, under `rules`. Empty never does; under T3's rules
/// neither does a value that is only whitespace.
pub fn t3code_home_is_set(value: Option<&OsStr>, rules: HomeRules) -> bool {
    match (value, rules) {
        (None, _) => false,
        (Some(value), HomeRules::Original) => !value.is_empty(),
        (Some(value), HomeRules::T3) => match value.to_str() {
            Some(text) => !crate::integration::js_trim(text).is_empty(),
            None => !value.is_empty(),
        },
    }
}

fn default_folder(home: Option<&OsStr>) -> PathBuf {
    home.map_or_else(PathBuf::new, PathBuf::from).join(".t3")
}

/// T3's data folder (the one that holds `userdata`).
pub fn data_folder(
    t3code_home: Option<&OsStr>,
    home: Option<&OsStr>,
    cwd: Option<&Path>,
    rules: HomeRules,
) -> PathBuf {
    if !t3code_home_is_set(t3code_home, rules) {
        return default_folder(home);
    }
    let value = t3code_home.expect("checked by t3code_home_is_set");
    if rules == HomeRules::Original {
        return PathBuf::from(value);
    }
    // A value that is not valid UTF-8 cannot be trimmed or matched against `~`: use it as given.
    let Some(text) = value.to_str() else {
        return absolute(PathBuf::from(value), cwd);
    };
    let text = crate::integration::js_trim(text);
    let expanded = match (text, home) {
        ("~", Some(home)) => PathBuf::from(home),
        (_, Some(home)) if text.starts_with("~/") || text.starts_with("~\\") => {
            // `path.join(home, rest)`: both separators are accepted after the tilde.
            PathBuf::from(home).join(&text[2..])
        }
        _ => PathBuf::from(text),
    };
    absolute(expanded, cwd)
}

/// `path.resolve` for one argument: made absolute against `cwd` and tidied lexically (`.` dropped,
/// `..` taken off the previous name). Nothing touches the file system.
fn absolute(path: PathBuf, cwd: Option<&Path>) -> PathBuf {
    let full = if path.has_root() || path.is_absolute() {
        path
    } else {
        match cwd {
            Some(cwd) => cwd.join(path),
            None => path,
        }
    };
    let mut out = PathBuf::new();
    for part in full.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                let climbs = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if climbs {
                    out.pop();
                } else if !out.has_root() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `parseArgs` in `main.ts`, including its order: unknown options first, then demo/database
/// exclusivity, then help.
pub fn parse(args: &[String]) -> Result<Command, UsageError> {
    parse_in(args, &Environment::current())
}

/// [`parse`] with an explicit environment (for tests and other front doors).
pub fn parse_in(args: &[String], env: &Environment) -> Result<Command, UsageError> {
    let mut db: Option<String> = None;
    let mut interval_raw = "2".to_owned();
    let mut now_ms: Option<f64> = None;
    let (mut once, mut help, mut status, mut watch, mut demo, mut calm) =
        (false, false, false, false, false, false);
    let (mut theme, mut check, mut themes) = (None::<String>, None::<String>, false);
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            break;
        }
        if let Some((flag, value)) = split_flag(arg) {
            let boolean = |slot: &mut bool| -> Result<(), UsageError> {
                if value.is_some() {
                    return Err(UsageError(format!(
                        "Option '{flag}' does not take an argument"
                    )));
                }
                *slot = true;
                Ok(())
            };
            match flag {
                "--once" => boolean(&mut once)?,
                "--help" | "-h" => boolean(&mut help)?,
                "--status" => boolean(&mut status)?,
                "--watch" => boolean(&mut watch)?,
                "--demo" => boolean(&mut demo)?,
                "--calm" => boolean(&mut calm)?,
                "--db" => db = Some(string_value(args, &mut i, flag, value)?),
                "--theme" if env.themes => theme = Some(string_value(args, &mut i, flag, value)?),
                "--theme-check" if env.themes => check = Some(string_value(args, &mut i, flag, value)?),
                "--themes" if env.themes => boolean(&mut themes)?,
                "--interval" => interval_raw = string_value(args, &mut i, flag, value)?,
                "--now-ms" => {
                    let raw = string_value(args, &mut i, flag, value)?;
                    now_ms = Some(match raw.trim().parse::<f64>() {
                        Ok(value) if value.is_finite() => value,
                        _ => {
                            return Err(UsageError(format!(
                                "--now-ms needs a finite number, got {raw:?}"
                            )));
                        }
                    });
                }
                _ => return Err(UsageError(format!("Unknown option '{arg}'"))),
            }
        } else if arg.starts_with('-') && arg != "-" {
            return Err(UsageError(format!("Unknown option '{arg}'")));
        }
        // Bare positionals are allowed, as `parseArgs` collects them and `main.ts` ignores them.
        i += 1;
    }

    let explicit = db.as_deref().is_some_and(|value| !value.is_empty());
    if demo && explicit {
        return Err(UsageError("--demo and --db cannot be used together".into()));
    }
    if help {
        return Ok(Command::Help);
    }
    if let Some(name) = check {
        return Ok(Command::ThemeCheck { name });
    }
    if themes {
        return Ok(Command::Themes { theme });
    }
    let database = if demo {
        Database::Demo
    } else {
        let origin = if explicit {
            Origin::Db
        } else if t3code_home_is_set(env.t3code_home.as_deref(), PLATFORM_HOME_RULES) {
            Origin::Env
        } else {
            Origin::Default
        };
        Database::Path {
            path: db.map_or_else(
                || default_database_for(env.t3code_home.as_deref(), env.home.as_deref()),
                PathBuf::from,
            ),
            origin,
        }
    };
    let interval = interval(Some(&interval_raw));
    if status {
        Ok(Command::Status {
            database,
            watch,
            now: now_ms,
        })
    } else if once {
        Ok(Command::Once {
            database,
            now: now_ms,
        })
    } else {
        Ok(Command::Interactive {
            database,
            interval,
            calm: calm
                || env
                    .animations_off
                    .unwrap_or_else(integration::animations_off),
            theme,
        })
    }
}

/// `--flag=value` or a bare `--flag`; a single-dash short flag is kept whole.
fn split_flag(arg: &str) -> Option<(&str, Option<&str>)> {
    if !arg.starts_with('-') || arg == "-" {
        return None;
    }
    match arg.split_once('=') {
        Some((flag, value)) => Some((flag, Some(value))),
        None => Some((arg, None)),
    }
}

/// A string option's value: inline after `=`, else the next argument.
fn string_value(
    args: &[String],
    i: &mut usize,
    flag: &str,
    inline: Option<&str>,
) -> Result<String, UsageError> {
    if let Some(value) = inline {
        return Ok(value.to_owned());
    }
    match args.get(*i + 1) {
        Some(value) if !value.starts_with('-') || value == "-" => {
            *i += 1;
            Ok(value.clone())
        }
        _ => Err(UsageError(format!(
            "Option '{flag} <value>' argument missing"
        ))),
    }
}

/// `Math.max(0.5, Number(raw) || 2)`.
fn interval(raw: Option<&str>) -> f64 {
    let n = js_parse_number(raw.unwrap_or("2"));
    let n = if n == 0.0 || n.is_nan() { 2.0 } else { n };
    n.max(0.5)
}

/// JavaScript `Number(string)` for the forms an environment variable can hold.
fn js_parse_number(raw: &str) -> f64 {
    let text = raw.trim();
    if text.is_empty() {
        return 0.0; // Number("") is 0
    }
    match text {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    for (prefix, radix) in [("0x", 16), ("0X", 16), ("0b", 2), ("0B", 2), ("0o", 8), ("0O", 8)] {
        if let Some(digits) = text.strip_prefix(prefix) {
            return i64::from_str_radix(digits, radix)
                .map(|n| n as f64)
                .unwrap_or(f64::NAN);
        }
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// True when the product command line owns these arguments: no experiment flag is present.
pub fn requested() -> bool {
    !crate::cli::experiment_requested()
}

/// `process.stdin.isTTY && process.stdout.isTTY`.
pub fn is_terminal() -> bool {
    // On Unix this is `isatty`; on Windows it asks whether the handle is a console.
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// The original's interactive check: print the message and exit 1 when there is no terminal.
pub fn ensure_terminal() {
    if !is_terminal() {
        let _ = io::stderr().write_all(NOT_A_TERMINAL.as_bytes());
        std::process::exit(1);
    }
}

/// Handle the product command line when it applies. Returns `Ok(false)` only for an interactive
/// run on a terminal, which the caller presents.
pub fn headless() -> Result<bool, Box<dyn std::error::Error>> {
    if !requested() {
        return Ok(false);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = Environment::current();
    let command = match parse_in(&args, &env) {
        Ok(command) => command,
        Err(error) => {
            let usage = usage(env.themes);
            let _ = io::stderr().write_all(format!("{error}\n\n{usage}").as_bytes());
            std::process::exit(2);
        }
    };
    match command {
        Command::Help => {
            let mut out = io::stdout().lock();
            out.write_all(usage(env.themes).as_bytes())?;
            out.flush()?;
            Ok(true)
        }
        Command::Themes { theme } => {
            let paths = theme::Paths::current();
            let env_theme = std::env::var("T3_AGENTS_THEME").ok();
            let (setting, skipped) = match theme::setting(theme.as_deref(), env_theme.as_deref(), &paths) {
                Ok(found) => found,
                Err(e) => {
                    let _ = io::stderr().write_all(format!("{e}\n").as_bytes());
                    std::process::exit(2);
                }
            };
            for line in skipped {
                let _ = io::stderr().write_all(format!("skipped {line}\n").as_bytes());
            }
            let themes = theme::Themes::new(setting, paths.clone());
            let mut out = io::stdout().lock();
            out.write_all(theme::list(Some(&themes), &paths).as_bytes())?;
            out.flush()?;
            Ok(true)
        }
        Command::ThemeCheck { name } => match theme::check(&name, &theme::Paths::current()) {
            Ok(report) => {
                let mut out = io::stdout().lock();
                out.write_all(report.as_bytes())?;
                out.flush()?;
                Ok(true)
            }
            Err(e) => {
                let _ = io::stderr().write_all(format!("{e}\n").as_bytes());
                std::process::exit(1);
            }
        },
        Command::Status {
            database,
            watch,
            now,
        } => {
            let now = now.unwrap_or_else(clock::wall_now);
            let mut out = io::stdout().lock();
            match database {
                Database::Demo => {
                    if watch {
                        status::watch_demo(now)?;
                        return Ok(true);
                    }
                    let snap = demo::snapshot(now, now)?;
                    out.write_all(status::status(&snap, now).as_bytes())?;
                    out.write_all(b"\n")?;
                }
                Database::Path { path, .. } => {
                    if watch {
                        // A closed pipe is a normal way for the bar to go away.
                        let _ = status::watch(&path);
                        return Ok(true);
                    }
                    out.write_all((status::line(&path, now) + "\n").as_bytes())?;
                }
            }
            out.flush()?;
            Ok(true)
        }
        Command::Once { database, now } => {
            let now = now.unwrap_or_else(clock::wall_now);
            let origin = match &database {
                Database::Path { origin, .. } => *origin,
                Database::Demo => Origin::Default,
            };
            let read = match &database {
                Database::Demo => demo::snapshot(now, now).map(|snap| (snap, true)),
                Database::Path { path, .. } => {
                    source::snapshot(path, now).map(|snap| (snap, false))
                }
            };
            let (snap, is_demo) = match read {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    // The original's advice on stderr, then exit 1.
                    let _ = io::stderr().write_all(advice(&error, origin).as_bytes());
                    std::process::exit(1);
                }
            };
            let text = once(&snap, now, is_demo);
            let mut out = io::stdout().lock();
            match out
                .write_all(format!("{text}\n").as_bytes())
                .and_then(|()| out.flush())
            {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(true),
                Err(error) => Err(error.into()),
            }
        }
        // The caller presents. A terminal presenter calls `ensure_terminal` first; a window
        // does not need one.
        Command::Interactive { .. } => Ok(false),
    }
}

/// What to try when T3's database can't be read, as `advice()` in `main.ts` writes it.
pub fn advice(err: &SourceError, origin: Origin) -> String {
    let env_home = std::env::var("T3CODE_HOME").unwrap_or_default();
    let lines: Vec<String> = match err.kind {
        "missing" => vec![
            format!("no T3 database at {}", err.path.display()),
            "Is T3 Code installed, and has it been opened at least once?".into(),
            where_to_look(origin, &env_home),
            "To try the dashboard without T3 data: t3-agents --demo".into(),
        ],
        "unreadable" => vec![
            format!(
                "can't read the T3 database at {}: {}",
                err.path.display(),
                err.detail
            ),
            "Check that it is T3 Code's statev2.sqlite and that your user can read it.".into(),
            where_to_look(origin, &env_home),
            "To try the dashboard without T3 data: t3-agents --demo".into(),
        ],
        _ => vec![
            format!(
                "unsupported T3 schema in {}; {}",
                err.path.display(),
                err.detail
            ),
            "T3 Code may have changed its database in an update: check for a newer T3 Agents."
                .into(),
            "To try the dashboard without T3 data: t3-agents --demo".into(),
        ],
    };
    let mut out = format!("t3-agents: {}\n", lines[0]);
    for line in &lines[1..] {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn where_to_look(origin: Origin, t3code_home: &str) -> String {
    match origin {
        Origin::Db => "Check the path given to --db.".into(),
        Origin::Env => format!(
            "T3CODE_HOME is set to {t3code_home}: check that it is T3's data folder, or unset it to use ~/.t3."
        ),
        Origin::Default => {
            "If T3 keeps its data somewhere else, set T3CODE_HOME to that folder or pass --db PATH."
                .into()
        }
    }
}

/// `once()` in `main.ts`: the header line and one line per tree row, with no trailing newline.
pub fn once(snap: &Snapshot, now: f64, demo: bool) -> String {
    let live = snap.agents.iter().filter(|a| model::running(a)).count();
    let waiting = snap.agents.iter().filter(|a| model::needs_you(a)).count();
    let mut out = format!(
        "T3 AGENTS · {} · {} · {live} running · {waiting} waiting · {} agents\n",
        if demo { "demo data" } else { "read-only" },
        clock::clock(now),
        snap.agents.len()
    );
    // `tree(snap.agents, new Set())` is called without `recent`, so every subagent is listed.
    // The Rust tree always computes the recent split; expanding every parent forces the same
    // "all" stage without changing the order or the guides.
    let mut state = State::new(snap.clone());
    state.expanded = snap.agents.iter().map(|a| a.id.clone()).collect();
    for row in model::tree(&state) {
        let a = &snap.agents[row.agent];
        let prefix = row
            .guides
            .iter()
            .map(|guide| if *guide { "│ " } else { "  " })
            .collect::<String>()
            + if row.depth > 0 {
                if row.last { "└─" } else { "├─" }
            } else {
                ""
            };
        let name = pad_end(
            &terminal::fit(
                &terminal::clean(&format!("{prefix}{} {}", mark(a), a.title)),
                44,
            ),
            44,
        );
        let context = context(a);
        let age = age(a, now);
        out.push_str(&format!(
            "{name}  {}  {}  {context}  {age}\n",
            pad_end(&terminal::fit(&terminal::short_model(&a.model), 16), 16),
            pad_end(&a.status, 11),
        ));
    }
    out.pop();
    out
}

/// The header's mark: waiting on you, running, or how a finished run ended.
fn mark(a: &Agent) -> &'static str {
    if model::needs_you(a) {
        "◆"
    } else if model::running(a) {
        "●"
    } else {
        match a.status.as_str() {
            "completed" => "✓",
            "failed" | "error" => "✗",
            "interrupted" => "⊘",
            _ => "·",
        }
    }
}

/// The context column: an 8th-cell bar and a percentage, or `context n/a`.
fn context(a: &Agent) -> String {
    let (Some(used), Some(max)) = (a.context.used_tokens, a.context.max_tokens) else {
        return "context n/a".into();
    };
    if !(max > 0.0) {
        return "context n/a".into();
    }
    let frac = used / max;
    format!(
        "{} {}%",
        progress(frac, 16),
        pad_start(&js_number_string(js_round(frac * 100.0)), 3)
    )
}

/// `bar(frac, w)`: eighths of a block, then a track.
fn progress(frac: f64, w: usize) -> String {
    let capped = if frac.is_nan() {
        frac
    } else {
        frac.clamp(0.0, 1.0)
    };
    let units = (capped * w as f64 * 8.0 + 0.5).floor();
    if !units.is_finite() {
        // `${array[NaN]}` is the string "undefined", then padEnd(w, "░") fills the bar.
        return format!("undefined{}", "░".repeat(w.saturating_sub(9)));
    }
    let units = units.max(0.0) as usize;
    let full = units / 8;
    let remainder = units % 8;
    let mut out = "█".repeat(full);
    out.push_str(["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][remainder]);
    out.push_str(&"░".repeat(w.saturating_sub(full + usize::from(remainder > 0))));
    out
}

/// The run's age column, with the same finite/invalid-date distinctions as `once()`.
fn age(a: &Agent, now: f64) -> String {
    let started = a
        .started
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| model::ms(Some(value)).unwrap_or(f64::NAN))
        .unwrap_or(f64::NAN);
    let counting = model::live(a) || model::needs_you(a);
    let ended = a
        .ended
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(|value| model::ms(Some(value)).unwrap_or(f64::NAN));
    let end = if counting { None } else { ended };
    if !started.is_finite() {
        return "--".into();
    }
    if !counting && end.is_none() {
        return format!("{}  started {}", pad_start("--", 5), terminal::clock(started, now));
    }
    let elapsed = (end.unwrap_or(now) - started) / 1000.0;
    let span = pad_start(&terminal::span(Some(elapsed)), 5);
    let when = match end {
        None => terminal::when(started, None, now),
        Some(end) if end.is_finite() => terminal::when(started, Some(end), now),
        // `new Date(NaN)` in `clock()`: the invalid date's getters concatenate to these.
        Some(_) => format!(
            "{} → undefined NaN NaN:NaN",
            terminal::clock(started, now)
        ),
    };
    format!("{span}  {when}")
}

/// `Math.round`: halves go toward +Infinity.
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

/// `String(number)` for the integers `Math.round` returns.
fn js_number_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.abs() < 1e21 {
        return format!("{x:.0}");
    }
    let exp = format!("{x:e}");
    match exp.split_once('e') {
        Some((mantissa, exponent)) => {
            let exponent: i32 = exponent.parse().unwrap_or(0);
            format!(
                "{mantissa}e{}{exponent}",
                if exponent >= 0 { "+" } else { "" }
            )
        }
        None => exp,
    }
}

/// `String.prototype.padEnd` counts UTF-16 code units, not display columns.
fn pad_end(s: &str, width: usize) -> String {
    let units = s.encode_utf16().count();
    if units >= width {
        s.to_owned()
    } else {
        format!("{s}{}", " ".repeat(width - units))
    }
}

/// `String.prototype.padStart` counts UTF-16 code units.
fn pad_start(s: &str, width: usize) -> String {
    let units = s.encode_utf16().count();
    if units >= width {
        s.to_owned()
    } else {
        format!("{}{s}", " ".repeat(width - units))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(home: &str, t3code_home: Option<&str>) -> Environment {
        Environment {
            t3code_home: t3code_home.map(Into::into),
            home: Some(home.into()),
            animations_off: Some(false),
            themes: false,
        }
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    fn interval_of(values: &[&str]) -> f64 {
        match parse_in(&args(values), &env("/tmp/home", None)).unwrap() {
            Command::Interactive { interval, .. } => interval,
            other => panic!("expected interactive, got {other:?}"),
        }
    }

    #[test]
    fn theme_options_are_the_products_and_the_original_rejects_them() {
        let themed = Environment { themes: true, ..env("/tmp/home", None) };
        match parse_in(&args(&["--demo", "--theme", "dark:frontier,light:paper"]), &themed).unwrap() {
            Command::Interactive { theme, .. } => assert_eq!(theme.as_deref(), Some("dark:frontier,light:paper")),
            other => panic!("expected interactive, got {other:?}"),
        }
        assert_eq!(parse_in(&args(&["--themes"]), &themed).unwrap(), Command::Themes { theme: None });
        assert_eq!(
            parse_in(&args(&["--theme-check=paper"]), &themed).unwrap(),
            Command::ThemeCheck { name: "paper".into() }
        );
        assert_eq!(parse_in(&args(&["--themes", "--help"]), &themed).unwrap(), Command::Help);
        // The theme is a screen setting: --once and --status take it and draw no color.
        assert!(matches!(parse_in(&args(&["--once", "--theme", "paper"]), &themed).unwrap(), Command::Once { .. }));
        assert!(parse_in(&args(&["--theme"]), &themed).is_err());
        // The original's command line, which the parity checks compare, knows none of them.
        for flag in ["--theme=paper", "--themes", "--theme-check=paper"] {
            assert_eq!(parse_in(&args(&[flag]), &env("/tmp/home", None)), Err(UsageError(format!("Unknown option '{flag}'"))));
        }
        assert_eq!(usage(false), USAGE);
        assert_eq!(usage(true), format!("{USAGE}{THEME_USAGE}"));
        assert!(THEME_USAGE.lines().all(|l| l.chars().count() <= 80));
    }

    #[test]
    fn help_wins_after_the_demo_database_check() {
        assert_eq!(
            parse_in(&args(&["--help"]), &env("/tmp/home", None)),
            Ok(Command::Help)
        );
        assert_eq!(
            parse_in(&args(&["-h", "--once"]), &env("/tmp/home", None)),
            Ok(Command::Help)
        );
        assert!(matches!(
            parse_in(&args(&["--help", "--demo", "--db", "x"]), &env("/tmp/home", None)),
            Err(UsageError(message)) if message == "--demo and --db cannot be used together"
        ));
    }

    #[test]
    fn demo_and_database_are_exclusive() {
        assert!(parse_in(&args(&["--demo", "--db", "x"]), &env("/tmp", None)).is_err());
        assert!(parse_in(&args(&["--db", "x", "--demo"]), &env("/tmp", None)).is_err());
        // An empty --db is falsy in main.ts, like no --db at all.
        assert!(parse_in(&args(&["--demo", "--db="]), &env("/tmp", None)).is_ok());
    }

    #[test]
    fn unknown_options_and_missing_values_are_usage_errors() {
        assert_eq!(
            parse_in(&args(&["--bogus"]), &env("/tmp", None)),
            Err(UsageError("Unknown option '--bogus'".into()))
        );
        assert_eq!(
            parse_in(&args(&["-x"]), &env("/tmp", None)),
            Err(UsageError("Unknown option '-x'".into()))
        );
        assert_eq!(
            parse_in(&args(&["--db"]), &env("/tmp", None)),
            Err(UsageError("Option '--db <value>' argument missing".into()))
        );
        assert_eq!(
            parse_in(&args(&["--interval"]), &env("/tmp", None)),
            Err(UsageError(
                "Option '--interval <value>' argument missing".into()
            ))
        );
        assert!(parse_in(&args(&["--once=1"]), &env("/tmp", None)).is_err());
    }

    #[test]
    fn interval_matches_javascript_number_and_minimum() {
        assert_eq!(interval_of(&[]), 2.0);
        assert_eq!(interval_of(&["--interval", "0"]), 2.0);
        assert_eq!(interval_of(&["--interval", "0.1"]), 0.5);
        assert_eq!(interval_of(&["--interval", "abc"]), 2.0);
        assert_eq!(interval_of(&["--interval", "5"]), 5.0);
        assert_eq!(interval_of(&["--interval="]), 2.0);
        assert_eq!(interval_of(&["--interval", "0x10"]), 16.0);
        assert_eq!(interval_of(&["--interval", "Infinity"]), f64::INFINITY);
        assert!(parse_in(&args(&["--interval", "-2"]), &env("/tmp/home", None)).is_err());
    }

    #[test]
    fn database_path_and_origin_follow_environment() {
        let command = parse_in(
            &args(&["--once"]),
            &env("/tmp/home", Some("/tmp/t3code")),
        )
        .unwrap();
        assert_eq!(
            command,
            Command::Once {
                database: Database::Path {
                    path: PathBuf::from("/tmp/t3code/userdata/statev2.sqlite"),
                    origin: Origin::Env,
                },
                now: None,
            }
        );
        let command = parse_in(&args(&["--once"]), &env("/tmp/home", Some(""))).unwrap();
        assert_eq!(
            command,
            Command::Once {
                database: Database::Path {
                    path: PathBuf::from("/tmp/home/.t3/userdata/statev2.sqlite"),
                    origin: Origin::Default,
                },
                now: None,
            }
        );
        let command = parse_in(
            &args(&["--once", "--db=/tmp/explicit.sqlite"]),
            &env("/tmp/home", Some("/tmp/t3code")),
        )
        .unwrap();
        assert_eq!(
            command,
            Command::Once {
                database: Database::Path {
                    path: PathBuf::from("/tmp/explicit.sqlite"),
                    origin: Origin::Db,
                },
                now: None,
            }
        );
        let command = parse_in(&args(&["--status", "--watch"]), &env("/tmp/home", None)).unwrap();
        assert_eq!(
            command,
            Command::Status {
                database: Database::Path {
                    path: PathBuf::from("/tmp/home/.t3/userdata/statev2.sqlite"),
                    origin: Origin::Default,
                },
                watch: true,
                now: None,
            }
        );
    }

    #[test]
    fn calm_is_the_flag_or_omarchys_animation_setting() {
        let mut off = env("/tmp/home", None);
        off.animations_off = Some(true);
        assert!(matches!(
            parse_in(&args(&[]), &off).unwrap(),
            Command::Interactive { calm: true, .. }
        ));
        let mut on = env("/tmp/home", None);
        on.animations_off = Some(false);
        assert!(matches!(
            parse_in(&args(&[]), &on).unwrap(),
            Command::Interactive { calm: false, .. }
        ));
        assert!(matches!(
            parse_in(&args(&["--calm"]), &on).unwrap(),
            Command::Interactive { calm: true, .. }
        ));
    }

    #[test]
    fn context_bars_round_like_javascript() {
        let context = |used: f64, max: f64| {
            let agent = Agent {
                context: crate::model::Context {
                    used_tokens: Some(used),
                    max_tokens: Some(max),
                    ..Default::default()
                },
                ..Agent::default()
            };
            crate::product::context(&agent)
        };
        assert_eq!(
            context(150.0, 100.0),
            "████████████████ 150%",
            "above max clamps the bar, not the number"
        );
        assert_eq!(context(-50.0, 100.0), "░░░░░░░░░░░░░░░░ -50%");
        assert_eq!(context(0.0, 100.0), "░░░░░░░░░░░░░░░░   0%");
        assert_eq!(context(0.005, 1.0), "▏░░░░░░░░░░░░░░░   1%");
        assert_eq!(context(1.0, 3.0), "█████▍░░░░░░░░░░  33%");
        assert_eq!(context(1.0, 0.0), "context n/a");
        assert_eq!(context(1.0, -1.0), "context n/a");
    }

    #[test]
    fn advice_names_the_database_origin() {
        let error = SourceError {
            kind: "missing",
            path: PathBuf::from("/tmp/synthetic/statev2.sqlite"),
            detail: String::new(),
        };
        assert_eq!(
            advice(&error, Origin::Db),
            "t3-agents: no T3 database at /tmp/synthetic/statev2.sqlite\n  Is T3 Code installed, and has it been opened at least once?\n  Check the path given to --db.\n  To try the dashboard without T3 data: t3-agents --demo\n"
        );
        let schema = SourceError {
            kind: "schema",
            path: PathBuf::from("/tmp/synthetic/statev2.sqlite"),
            detail: "missing table".into(),
        };
        assert_eq!(
            advice(&schema, Origin::Default),
            "t3-agents: unsupported T3 schema in /tmp/synthetic/statev2.sqlite; missing table\n  T3 Code may have changed its database in an update: check for a newer T3 Agents.\n  To try the dashboard without T3 data: t3-agents --demo\n"
        );
    }

    #[test]
    fn padding_counts_utf16_units_like_javascript() {
        assert_eq!(pad_end("abc", 5), "abc  ");
        assert_eq!(pad_end("日本語", 4), "日本語 ");
        assert_eq!(pad_end("🍞", 3), "🍞 ");
        assert_eq!(pad_end("abcdef", 3), "abcdef");
        assert_eq!(pad_start("7", 3), "  7");
        assert_eq!(pad_start("100", 3), "100");
        assert_eq!(pad_start("--", 5), "   --");
    }

    #[test]
    fn once_rows_and_header_match_the_plain_text_shape() {
        let now = 1_794_484_800_000.0;
        let agent = Agent {
            id: "a".into(),
            title: "Fix the build".into(),
            status: "running".into(),
            model: "claude-opus-4-5".into(),
            started: Some("2026-11-12T11:30:00.000Z".into()),
            context: crate::model::Context {
                used_tokens: Some(1.0),
                max_tokens: Some(2.0),
                ..Default::default()
            },
            ..Agent::default()
        };
        let snap = Snapshot {
            agents: vec![agent.into()],
            at: now,
            path: "synthetic".into(),
        };
        let text = once(&snap, now, false);
        let mut lines = text.lines();
        assert_eq!(
            lines.next().unwrap(),
            format!(
                "T3 AGENTS · read-only · {} · 1 running · 0 waiting · 1 agents",
                clock::clock(now)
            )
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("● Fix the build"), "{row}");
        assert!(row.contains("opus 4.5"), "{row}");
        assert!(row.contains("████████░░░░░░░░  50%"), "{row}");
        assert!(
            row.ends_with(&format!(
                "since {}",
                terminal::clock(
                    model::ms(Some("2026-11-12T11:30:00.000Z")).unwrap(),
                    now
                )
            )),
            "{row}"
        );
    }
}

#[cfg(test)]
mod home_rule_tests {
    use super::*;

    fn db(t3: Option<&str>, home: Option<&str>, cwd: Option<&str>, rules: HomeRules) -> PathBuf {
        database_in(
            t3.map(OsStr::new),
            home.map(OsStr::new),
            cwd.map(Path::new),
            rules,
        )
    }
    const FILE: &str = "userdata/statev2.sqlite";

    /// The original app's rule, the one every Linux case in `cli-parity.ts` exercises.
    #[test]
    fn original_rules_use_the_value_as_given() {
        use HomeRules::Original as R;
        assert_eq!(db(Some("/data/t3"), Some("/home/a"), None, R), Path::new("/data/t3").join(FILE));
        assert_eq!(db(None, Some("/home/a"), None, R), Path::new("/home/a/.t3").join(FILE));
        // Empty counts as unset on both sides.
        assert_eq!(db(Some(""), Some("/home/a"), None, R), Path::new("/home/a/.t3").join(FILE));
        // No HOME: a relative `.t3`, as before.
        assert_eq!(db(None, None, None, R), Path::new(".t3").join(FILE));
        // The three places T3 differs. The original keeps the text exactly.
        assert_eq!(db(Some("  /x  "), Some("/home/a"), None, R), Path::new("  /x  ").join(FILE));
        assert_eq!(db(Some(" "), Some("/home/a"), None, R), Path::new(" ").join(FILE));
        assert_eq!(db(Some("~"), Some("/home/a"), None, R), Path::new("~").join(FILE));
        assert_eq!(db(Some("~/t3"), Some("/home/a"), None, R), Path::new("~/t3").join(FILE));
        assert_eq!(db(Some("rel/t3"), Some("/home/a"), Some("/work"), R), Path::new("rel/t3").join(FILE));
        assert!(t3code_home_is_set(Some(OsStr::new(" ")), R));
        assert!(!t3code_home_is_set(Some(OsStr::new("")), R));
    }

    /// T3's `resolveBaseDir`: trim, blank is unset, `~` expands, the rest is made absolute.
    #[test]
    fn t3_rules_trim_expand_and_resolve() {
        use HomeRules::T3 as R;
        let home = Some("/home/a");
        let cwd = Some("/work/here");
        assert_eq!(db(Some("/data/t3"), home, cwd, R), Path::new("/data/t3").join(FILE));
        assert_eq!(db(None, home, cwd, R), Path::new("/home/a/.t3").join(FILE));
        assert_eq!(db(Some(""), home, cwd, R), Path::new("/home/a/.t3").join(FILE));
        // Blank after trim is unset: spaces, a tab, a newline, NBSP and the byte order mark.
        for blank in [" ", "\t\n", "\u{a0}", "\u{feff}", "  \u{2003} "] {
            assert_eq!(db(Some(blank), home, cwd, R), Path::new("/home/a/.t3").join(FILE), "{blank:?}");
            assert!(!t3code_home_is_set(Some(OsStr::new(blank)), R));
        }
        // U+0085 is not JavaScript whitespace, so `trim` keeps it and it is a name.
        assert!(t3code_home_is_set(Some(OsStr::new("\u{85}")), R));
        assert_eq!(db(Some("  /data/t3\n"), home, cwd, R), Path::new("/data/t3").join(FILE));
        // `~`, `~/x` and `~\x` expand against the home directory.
        assert_eq!(db(Some("~"), home, cwd, R), Path::new("/home/a").join(FILE));
        assert_eq!(db(Some(" ~ "), home, cwd, R), Path::new("/home/a").join(FILE));
        assert_eq!(db(Some("~/t3"), home, cwd, R), Path::new("/home/a/t3").join(FILE));
        assert_eq!(db(Some("~\\t3"), home, cwd, R), Path::new("/home/a/t3").join(FILE));
        // `~user` is not an expansion; it is a relative name under the working directory.
        assert_eq!(db(Some("~user/t3"), home, cwd, R), Path::new("/work/here/~user/t3").join(FILE));
        // A relative value is resolved against the working directory and tidied.
        assert_eq!(db(Some("rel/t3"), home, cwd, R), Path::new("/work/here/rel/t3").join(FILE));
        assert_eq!(db(Some("./a/../b"), home, cwd, R), Path::new("/work/here/b").join(FILE));
        assert_eq!(db(Some("/a/b/../../.."), home, cwd, R), Path::new("/").join(FILE));
        // The default is joined, not resolved. With no home it stays the relative `.t3`.
        assert_eq!(db(None, None, cwd, R), Path::new(".t3").join(FILE));
        // No working directory known: the value stays as trimmed.
        assert_eq!(db(Some("rel"), home, None, R), Path::new("rel").join(FILE));
        // A tilde with no home to expand to is left alone.
        assert_eq!(db(Some("~/x"), None, cwd, R), Path::new("/work/here/~/x").join(FILE));
    }

    #[cfg(unix)]
    #[test]
    fn t3_rules_pass_non_utf8_through_untrimmed() {
        use std::os::unix::ffi::OsStrExt;
        let raw = OsStr::from_bytes(b"/data/\xff ");
        let got = database_in(Some(raw), Some(OsStr::new("/h")), None, HomeRules::T3);
        assert_eq!(got.parent().unwrap().parent().unwrap().as_os_str().as_bytes(), b"/data/\xff ");
    }

    /// Linux follows the original; macOS and Windows follow T3.
    #[test]
    fn the_platform_picks_its_rules() {
        let want = if cfg!(target_os = "linux") { HomeRules::Original } else { HomeRules::T3 };
        assert_eq!(PLATFORM_HOME_RULES, want);
    }

    /// The default, with the environment passed in, equals the original's join on this platform.
    #[test]
    fn default_database_for_matches_the_rules_for_this_platform() {
        let home = Some(OsStr::new("/home/a"));
        assert_eq!(default_database_for(None, home), Path::new("/home/a/.t3").join(FILE));
        assert_eq!(default_database_for(Some(OsStr::new("")), home), Path::new("/home/a/.t3").join(FILE));
    }
}
