//! The product command line, without a screen.
//!
//! Ports `parseArgs` and the headless halves of `oracle/src/main.ts`: `--help`, `--once`
//! (plain text), `--status`, and the settings an interactive presenter needs. Product flags are
//! recognised unless one of the experiment-only flags (used by the parity gates) is present, so
//! `cli::headless` can keep serving both front doors from the same binary.
use crate::{
    clock, demo, integration,
    model::{self, Agent, Snapshot, State},
    recap, recap_view, tools, tools_view,
    source::{self, SourceError},
    trace, trace_view,
    status, terminal, theme,
};
use std::{
    ffi::OsStr,
    io::{self, IsTerminal, Write},
    path::{Component, Path, PathBuf},
};

/// Byte-for-byte `USAGE` from `oracle/src/main.ts`.
pub const USAGE: &str = "Usage: loupe [--window] [--once | --status [--watch]] [--db PATH | --demo] [--interval SECONDS] [--calm]

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
/// `LOUPE_NO_THEMES`, the original's behavior, which the parity checks compare).
pub const THEME_USAGE: &str = "  --theme NAME      color theme: a built-in, a theme file, omarchy (follow
                    Omarchy's live theme), or dark:NAME,light:NAME to follow
                    light and dark mode; c and C step through them while it runs
  --themes          list the themes and say which one is in use
  --theme-check NAME
                    a theme's colors, contrast and colorblind separation
";

/// What `--help` adds for Recap, beside the theme options: the original has neither.
pub const RECAP_USAGE: &str = "  --recap           the last full week of agent work on one card: hours and
                    where they went, tasks, output and models, read from T3's
                    records. For Recap and Tools, LOUPE_CACHE=1 lets them
                    save finished-run facts locally. LOUPE_NO_CACHE=1
                    always turns it off. Nothing is sent
  --this-week       with --recap, the current week so far
  --month, --this-month
                    with --recap, the last full calendar month, or the current
                    month so far
  --wide, --compact with --recap, the 120-column card or the 80-column one,
                    whatever the terminal's size; without either, a terminal
                    of 120 columns by 40 rows or more gets the wide card
  --narrow          with --recap, the short list a terminal under 80 columns
                    gets, whatever the terminal's size
  --json            with --recap, the numbers as JSON
";

/// What `--help` adds for Tools, after Recap's: the original has neither.
pub const TOOLS_USAGE: &str = "  --tools           what agents ran in the last full week and what failed: by kind
                    of tool, and for commands by program, with calls, time and failures
                    (--this-week, --month, --this-month, --json and --demo as --recap)
  --wide, --compact with --tools, the 120-column report or the 80-column one,
                    whatever the terminal's size; without either, a terminal
                    of 120 columns or more gets the wide report
";

/// What `--help` adds for Trace: the original has no counterpart either.
pub const TRACE_USAGE: &str = "  --trace [ID]      one task on a time axis: the model, each tool call, each
                    wait on you and each delegate. The latest task, a thread's
                    latest (its id, or the end of it), or the task that holds a
                    run; with --json, the spans
";

/// `--help`: the original's text, and the theme and report options where they are on.
pub fn usage(themes: bool) -> String {
    if themes {
        format!("{USAGE}{THEME_USAGE}{RECAP_USAGE}{TOOLS_USAGE}{TRACE_USAGE}")
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
    /// `--trace [ID]`: one task on a time axis. `target` is a thread or a run id.
    Trace { database: Database, now: Option<f64>, target: Option<String>, json: bool, theme: Option<String> },
    Status { database: Database, watch: bool, now: Option<f64> },
    Once { database: Database, now: Option<f64> },
    Interactive { database: Database, interval: f64, calm: bool, theme: Option<String> },
    /// `--themes`.
    Themes { theme: Option<String> },
    /// `--theme-check NAME`.
    ThemeCheck { name: String },
    /// `--tools`: what agents ran in a week or a month and what failed, by kind and by program.
    Tools { database: Database, now: Option<f64>, period: recap::Which, json: bool, size: Option<recap_view::Size>, theme: Option<String> },
    /// `--recap`: a week or a month of agent work on one card.
    Recap { database: Database, now: Option<f64>, period: recap::Which, json: bool, size: Option<recap_view::Size>, theme: Option<String> },
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
    /// The product's themes, and their options, are on: not under `LOUPE_NO_THEMES`.
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
    let (mut recap, mut this_week, mut month, mut this_month) = (false, false, false, false);
    let mut tools_flag = false;
    let (mut json, mut wide, mut compact, mut narrow) = (false, false, false, false);
    let mut i = 0;
    let (mut trace, mut trace_target) = (false, None::<String>);
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
                // Recap is the product's own, like the themes: the original rejects it.
                "--recap" if env.themes => boolean(&mut recap)?,
                // So is Tools.
                "--tools" if env.themes => boolean(&mut tools_flag)?,
                "--this-week" if env.themes => boolean(&mut this_week)?,
                "--month" if env.themes => boolean(&mut month)?,
                "--this-month" if env.themes => boolean(&mut this_month)?,
                "--json" if env.themes => boolean(&mut json)?,
                "--wide" if env.themes => boolean(&mut wide)?,
                "--compact" if env.themes => boolean(&mut compact)?,
                "--narrow" if env.themes => boolean(&mut narrow)?,
                // Trace is the product's own too. Its id is optional: `--trace`, `--trace ID` or
                // `--trace=ID`; a following word that starts with `-` is the next option.
                "--trace" if env.themes => {
                    trace = true;
                    trace_target = match value {
                        Some(value) => Some(value.to_owned()),
                        None => match args.get(i + 1) {
                            Some(next) if !next.starts_with('-') => {
                                i += 1;
                                Some(next.clone())
                            }
                            _ => None,
                        },
                    };
                }
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
    if trace {
        if recap || tools_flag || this_week || month || this_month || narrow || wide || compact || status || once || watch {
            return Err(UsageError("--trace goes with --json, --demo, --db and --theme only".into()));
        }
        return Ok(Command::Trace { database, now: now_ms, target: trace_target, json, theme });
    }
    let interval = interval(Some(&interval_raw));
    if recap && tools_flag {
        return Err(UsageError("--recap and --tools cannot be used together".into()));
    }
    if (this_week || month || this_month || json || wide || compact || narrow) && !recap && !tools_flag {
        return Err(UsageError("--this-week, --month, --this-month, --json, --wide, --compact and --narrow go with --recap or --tools".into()));
    }
    if u8::from(this_week) + u8::from(month) + u8::from(this_month) > 1 {
        return Err(UsageError("--this-week, --month and --this-month cannot be used together".into()));
    }
    if tools_flag && narrow {
        return Err(UsageError("--narrow goes with --recap".into()));
    }
    if u8::from(wide) + u8::from(compact) + u8::from(narrow) > 1 {
        let message = if tools_flag {
            "--wide and --compact cannot be used together"
        } else {
            "--wide, --compact and --narrow cannot be used together"
        };
        return Err(UsageError(message.into()));
    }
    if tools_flag {
        let period = match (this_week, month, this_month) {
            (true, ..) => recap::Which::ThisWeek,
            (_, true, _) => recap::Which::LastMonth,
            (_, _, true) => recap::Which::ThisMonth,
            _ => recap::Which::LastWeek,
        };
        let size = if wide { Some(recap_view::Size::Wide) } else { compact.then_some(recap_view::Size::Compact) };
        return Ok(Command::Tools { database, now: now_ms, period, json, size, theme });
    }
    if recap {
        let size = if wide {
            Some(recap_view::Size::Wide)
        } else if compact {
            Some(recap_view::Size::Compact)
        } else {
            narrow.then_some(recap_view::Size::Narrow)
        };
        let period = match (this_week, month, this_month) {
            (true, ..) => recap::Which::ThisWeek,
            (_, true, _) => recap::Which::LastMonth,
            (_, _, true) => recap::Which::ThisMonth,
            _ => recap::Which::LastWeek,
        };
        return Ok(Command::Recap { database, now: now_ms, period, json, size, theme });
    }
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
            let env_theme = std::env::var("LOUPE_THEME").ok();
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
        Command::Tools { database, now, period, json, size, theme } => {
            let code = tools_command(&database, now.unwrap_or_else(clock::wall_now), period, json, size, theme.as_deref())?;
            if code != 0 { std::process::exit(code); }
            Ok(true)
        }
        Command::Recap { database, now, period, json, size, theme } => {
            let code = recap_command(&database, now.unwrap_or_else(clock::wall_now), period, json, size, theme.as_deref())?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(true)
        }

        Command::Trace { database, now, target, json, theme } => {
            let code = trace_command(&database, now.unwrap_or_else(clock::wall_now), target.as_deref(), json, theme.as_deref())?;
            if code != 0 {
                std::process::exit(code);
            }
            Ok(true)
        }
        // The caller presents. A terminal presenter calls `ensure_terminal` first; a window
        // does not need one.
        Command::Interactive { .. } => Ok(false),
    }
}

/// Whether color is wanted, and how deep: none when the output is not a terminal, when
/// `NO_COLOR` is set or the terminal is `dumb`; 24-bit where `COLORTERM` says so, else the
/// 256-color palette. `FORCE_COLOR` or `CLICOLOR_FORCE` asks for color into a pipe.
fn recap_color(terminal: bool, var: &dyn Fn(&str) -> Option<String>) -> recap_view::Color {
    let set = |name: &str| var(name).is_some_and(|v| !v.is_empty() && v != "0");
    let forced = set("FORCE_COLOR") || set("CLICOLOR_FORCE");
    if var("NO_COLOR").is_some_and(|v| !v.is_empty()) || var("TERM").is_some_and(|t| t == "dumb") || !(terminal || forced) {
        return recap_view::Color::None;
    }
    let deep = var("COLORTERM").is_some_and(|c| c.contains("truecolor") || c.contains("24bit"));
    if deep || (forced && !terminal) { recap_view::Color::True } else { recap_view::Color::Indexed }
}

/// The typed value and the period that produced it. The CLI fallback can
/// return the current period when the requested full period is empty.
#[derive(Debug)]
pub struct ReportRead<T> {
    pub returned: recap::Which,
    pub value: T,
}

fn load_report<T>(
    selected: recap::Which,
    load: impl Fn(recap::Which) -> Result<T, SourceError>,
    is_empty: impl Fn(&T) -> bool,
) -> Result<ReportRead<T>, SourceError> {
    let value = load(selected)?;
    if !selected.so_far() && is_empty(&value) {
        let current = match selected.span() {
            recap::Span::Week => recap::Which::ThisWeek,
            recap::Span::Month => recap::Which::ThisMonth,
        };
        let current_value = load(current)?;
        if !is_empty(&current_value) {
            return Ok(ReportRead { returned: current, value: current_value });
        }
    }
    Ok(ReportRead { returned: selected, value })
}

pub fn load_tools(
    database: &Database,
    now: f64,
    selected: recap::Which,
    local: recap::Local<'_>,
) -> Result<ReportRead<tools::Tools>, SourceError> {
    load_report(selected, |which| {
        let period = recap::Period::new(now, which, local);
        match database {
            Database::Demo => tools::demo(&period, local),
            Database::Path { path, .. } => tools::load(path, &period, local),
        }
    }, tools::Tools::is_empty)
}

pub fn load_recap(
    database: &Database,
    now: f64,
    selected: recap::Which,
    local: recap::Local<'_>,
) -> Result<ReportRead<recap::Recap>, SourceError> {
    load_report(selected, |which| {
        let period = recap::Period::new(now, which, local);
        match database {
            Database::Demo => recap::demo(&period, local),
            Database::Path { path, .. } => recap::load(path, &period, local),
        }
    }, recap::Recap::is_empty)
}

/// `--tools`. Returns the exit code: 0, or 1 when T3's database can't be read.
fn tools_command(database: &Database, now: f64, which: recap::Which, json: bool, size: Option<recap_view::Size>, theme: Option<&str>) -> Result<i32, Box<dyn std::error::Error>> {
    let local: recap::Local = &crate::platform::local_time;
    let month = which.span() == recap::Span::Month;
    let (span, demo_tip) = if month { ("month", "Just looking? loupe --tools --month --demo shows a made-up month.") } else { ("week", "Just looking? loupe --tools --demo shows a made-up week.") };
    let read = load_tools(database, now, which, local).map(|read| read.value);
    let source = if matches!(database, Database::Demo) { recap_view::Source::Demo } else { recap_view::Source::Records };
    let terminal = io::stdout().is_terminal();
    let color = recap_color(terminal, &|name| std::env::var(name).ok());
    // Into a pipe, or where the terminal's size can't be asked for: a default terminal's 80 by 24.
    let columns = terminal.then(|| crate::platform::terminal_size().0).flatten().unwrap_or(80);
    // A report that was asked for is drawn whatever the terminal; otherwise its columns decide.
    // Rows do not: the report is taller than a screen at either width.
    let narrow = size.is_none() && columns < tools_view::REPORT_COLUMNS;
    let wide = match size {
        Some(recap_view::Size::Wide) => true,
        Some(recap_view::Size::Compact | recap_view::Size::Narrow) => false,
        None => columns >= tools_view::WIDE_COLUMNS,
    };
    let palette = || -> Result<terminal::Palette, Box<dyn std::error::Error>> {
        let (themes, skipped) = theme::Themes::from_environment(theme)?;
        for line in skipped {
            let _ = io::stderr().write_all(format!("skipped {line}\n").as_bytes());
        }
        Ok(themes.palette())
    };
    let mono = color == recap_view::Color::None;
    let (text, code) = match read {
        Ok(week) if json => (serde_json::to_string_pretty(&week)?, 0),
        Ok(week) if week.is_empty() => {
            let period = recap_view::period_text(week.period, &week.from, &week.to);
            let within = if month { format!("in {period}") } else { period.clone() };
            let message = if week.so_far { format!("No tool calls recorded {within} so far.") } else { format!("No tool calls recorded {within}, or this {span} so far.") };
            let tips = ["Tools reads the tool calls T3 Code has recorded on this machine.".to_owned(), demo_tip.to_owned()];
            if narrow {
                (format!("{message}\n{}", tips.join("\n")), 0)
            } else {
                (recap_view::paint(&tools_view::notice(&palette()?, &period, "no tool calls", false, &message, &tips), color), 0)
            }
        }
        Ok(week) if narrow => (tools_view::list(&week, source, columns), 0),
        Ok(week) if wide => (recap_view::paint(&tools_view::wide(&week, &palette()?, source, mono), color), 0),
        Ok(week) => (recap_view::paint(&tools_view::report(&week, &palette()?, source, mono), color), 0),
        Err(error) => {
            let origin = match database {
                Database::Path { origin, .. } => *origin,
                Database::Demo => Origin::Default,
            };
            let advice = advice(&error, origin);
            if json || !terminal || narrow {
                let _ = io::stderr().write_all(advice.as_bytes());
                return Ok(1);
            }
            let period = recap::Period::new(now, which, local);
            let dates = |stamp: f64| recap::local_date(local, stamp);
            let first = advice.lines().next().unwrap_or_default().trim_start_matches("loupe: ");
            let mut message: String = first.chars().take(1).flat_map(char::to_uppercase).chain(first.chars().skip(1)).collect();
            if !message.ends_with('.') {
                message.push('.');
            }
            let home = crate::platform::home_dir().map(|h| h.to_string_lossy().into_owned()).filter(|h| h.len() > 1);
            let short = |text: &str| home.as_deref().map_or_else(|| text.to_owned(), |h| text.replace(h, "~"));
            let message = short(&message);
            let mut tips: Vec<String> = advice.lines().skip(1).map(|line| short(line.trim())).collect();
            if let Some(last) = tips.last_mut() {
                *last = demo_tip.into();
            }
            let card = tools_view::notice(&palette()?, &recap_view::period_text(period.span, &dates(period.start()), &dates(period.last_day())), "can't read T3", true, &message, &tips);
            (recap_view::paint(&card, color), 1)
        }
    };
    let mut out = io::stdout().lock();
    match out.write_all(format!("{text}\n").as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Ok(code),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(code),
        Err(error) => Err(error.into()),
    }
}

/// `--recap`. Returns the exit code: 0, or 1 when T3's database can't be read.
fn recap_command(database: &Database, now: f64, which: recap::Which, json: bool, size: Option<recap_view::Size>, theme: Option<&str>) -> Result<i32, Box<dyn std::error::Error>> {
    let local: recap::Local = &crate::platform::local_time;
    let month = which.span() == recap::Span::Month;
    let (span, demo_tip) = if month { ("month", "Just looking? loupe --recap --month --demo shows a made-up month.") } else { ("week", "Just looking? loupe --recap --demo shows a made-up week.") };
    let read = load_recap(database, now, which, local).map(|read| read.value);
    let source = if matches!(database, Database::Demo) { recap_view::Source::Demo } else { recap_view::Source::Records };
    let terminal = io::stdout().is_terminal();
    let color = recap_color(terminal, &|name| std::env::var(name).ok());
    // Into a pipe, or where the terminal's size can't be asked for: a default terminal's 80 by 24.
    // The width and the height are each taken where they are known, so a terminal that gives
    // 60 columns and no rows is still 60 columns, and gets the list.
    let (known_columns, known_rows) = if terminal { crate::platform::terminal_size() } else { (None, None) };
    let (columns, rows) = (known_columns.unwrap_or(80), known_rows.unwrap_or(24));
    // A card that was asked for is drawn whatever the terminal; otherwise its size decides.
    // `--narrow`, or a terminal under 80 columns when no size was asked for, gets the list.
    let narrow = size == Some(recap_view::Size::Narrow) || (size.is_none() && columns < recap_view::CARD_COLUMNS);
    let wide = match size {
        Some(size) => size == recap_view::Size::Wide,
        None => columns >= recap_view::WIDE_COLUMNS && rows >= recap_view::WIDE_ROWS,
    };
    let palette = || -> Result<terminal::Palette, Box<dyn std::error::Error>> {
        let (themes, skipped) = theme::Themes::from_environment(theme)?;
        for line in skipped {
            let _ = io::stderr().write_all(format!("skipped {line}\n").as_bytes());
        }
        Ok(themes.palette())
    };
    let mono = color == recap_view::Color::None;
    let (text, code, drawn) = match read {
        Ok(week) if json => (serde_json::to_string_pretty(&week)?, 0, None),
        Ok(week) if week.is_empty() => {
            let period = recap_view::period_text(week.period, &week.from, &week.to);
            // "recorded Nov 9 – 15", and for a month, "recorded in October 2026".
            let within = if month { format!("in {period}") } else { period.clone() };
            let message = if week.so_far { format!("No agent runs recorded {within} so far.") } else { format!("No agent runs recorded {within}, or this {span} so far.") };
            let tips = ["Recap reads the runs T3 Code has recorded on this machine.".to_owned(), demo_tip.to_owned()];
            if narrow {
                (format!("{message}\n{}", tips.join("\n")), 0, None)
            } else {
                (recap_view::paint(&recap_view::notice(&palette()?, &period, "no records", false, &message, &tips), color), 0, Some(recap_view::CARD_COLUMNS))
            }
        }
        Ok(week) if narrow => (recap_view::paint(&recap_view::list(&week, &palette()?, source, mono), color), 0, Some(recap_view::LIST_COLUMNS)),
        Ok(week) if wide => (recap_view::paint(&recap_view::wide(&week, &palette()?, source, mono), color), 0, Some(recap_view::WIDE_COLUMNS)),
        Ok(week) => (recap_view::paint(&recap_view::card(&week, &palette()?, source, mono), color), 0, Some(recap_view::CARD_COLUMNS)),
        Err(error) => {
            let origin = match database {
                Database::Path { origin, .. } => *origin,
                Database::Demo => Origin::Default,
            };
            let advice = advice(&error, origin);
            if json || !terminal || narrow {
                // As --once does: the advice on stderr, and nothing on stdout.
                let _ = io::stderr().write_all(advice.as_bytes());
                return Ok(1);
            }
            let period = recap::Period::new(now, which, local);
            let dates = |stamp: f64| recap::local_date(local, stamp);
            // The advice's own first line, as a sentence: "No T3 database at PATH."
            let first = advice.lines().next().unwrap_or_default().trim_start_matches("loupe: ");
            let mut message: String = first.chars().take(1).flat_map(char::to_uppercase).chain(first.chars().skip(1)).collect();
            if !message.ends_with('.') {
                message.push('.');
            }
            // A path under the home folder is written with ~, as a person would write it.
            let home = crate::platform::home_dir().map(|h| h.to_string_lossy().into_owned()).filter(|h| h.len() > 1);
            let short = |text: &str| home.as_deref().map_or_else(|| text.to_owned(), |h| text.replace(h, "~"));
            let message = short(&message);
            let mut tips: Vec<String> = advice.lines().skip(1).map(|line| short(line.trim())).collect();
            if let Some(last) = tips.last_mut() {
                *last = demo_tip.into();
            }
            let card = recap_view::notice(&palette()?, &recap_view::period_text(period.span, &dates(period.start()), &dates(period.last_day())), "can't read T3", true, &message, &tips);
            (recap_view::paint(&card, color), 1, Some(recap_view::CARD_COLUMNS))
        }
    };
    let mut out = io::stdout().lock();
    match out.write_all(format!("{text}\n").as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => {
            // After the card, so it is the last thing on a screen the card did not fit.
            if let Some(note) = too_wide(drawn, known_columns, size) {
                let _ = io::stderr().write_all(note.as_bytes());
            }
            Ok(code)
        }
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(code),
        Err(error) => Err(error.into()),
    }
}

/// What to say after a drawing that is wider than the terminal it was drawn in. A size that
/// was asked for is drawn whatever the terminal, since a terminal can give the wrong width;
/// where the width it gives is under the drawing's, every row wraps, and this says why. The
/// list has no third line: there is no smaller drawing to offer.
fn too_wide(drawn: Option<usize>, columns: Option<usize>, size: Option<recap_view::Size>) -> Option<String> {
    let (card, columns) = (drawn?, columns?);
    if card <= columns {
        return None;
    }
    // Short lines: the terminal this is written for is a narrow one, often a phone.
    let list = card == recap_view::LIST_COLUMNS;
    let name = if list { "list" } else { "card" };
    let mut note = format!("loupe: the {name} is {card} columns wide,\nthis terminal is {columns}, so its lines wrap.\n");
    if !list {
        match size {
            Some(recap_view::Size::Wide) => note.push_str("Leave out --wide for what fits here.\n"),
            Some(recap_view::Size::Compact) => note.push_str("Leave out --compact for what fits here.\n"),
            Some(recap_view::Size::Narrow) | None => {}
        }
    }
    Some(note)
}

/// What to try when T3's database can't be read, as `advice()` in `main.ts` writes it.
pub fn advice(err: &SourceError, origin: Origin) -> String {
    let env_home = std::env::var("T3CODE_HOME").unwrap_or_default();
    let lines: Vec<String> = match err.kind {
        "missing" => vec![
            format!("no T3 database at {}", err.path.display()),
            "Is T3 Code installed, and has it been opened at least once?".into(),
            where_to_look(origin, &env_home),
            "To try the dashboard without T3 data: loupe --demo".into(),
        ],
        "unreadable" => vec![
            format!(
                "can't read the T3 database at {}: {}",
                err.path.display(),
                err.detail
            ),
            "Check that it is T3 Code's statev2.sqlite and that your user can read it.".into(),
            where_to_look(origin, &env_home),
            "To try the dashboard without T3 data: loupe --demo".into(),
        ],
        _ => vec![
            format!(
                "unsupported T3 schema in {}; {}",
                err.path.display(),
                err.detail
            ),
            "T3 Code may have changed its database in an update: check for a newer Loupe."
                .into(),
            "To try the dashboard without T3 data: loupe --demo".into(),
        ],
    };
    let mut out = format!("loupe: {}\n", lines[0]);
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
        "LOUPE · {} · {} · {live} running · {waiting} waiting · {} agents\n",
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

/// `--trace [ID]`. Returns the exit code: 0; 1 when T3's database can't be read or the id
/// names no thread or run.
fn trace_command(database: &Database, now: f64, target: Option<&str>, json: bool, theme: Option<&str>) -> Result<i32, Box<dyn std::error::Error>> {
    // An id names a run, a thread, or the one thread whose id ends with it (as the Tools view
    // prints ids). Ending more than one is said, not guessed.
    let named = |path: &Path, id: &str| -> Result<Result<Option<trace::Trace>, String>, SourceError> {
        for target in [trace::Target::Run(id.to_owned()), trace::Target::Thread(id.to_owned())] {
            if let Some(found) = trace::load(path, &target, now)? {
                return Ok(Ok(Some(found)));
            }
        }
        Ok(match trace::threads_ending(path, id)?.as_slice() {
            [one] => Ok(trace::load(path, &trace::Target::Thread(one.clone()), now)?),
            [] => Ok(None),
            _ => Err(format!("{id:?} is the end of more than one thread id; give more of it.\n")),
        })
    };
    let read = match database {
        Database::Demo => trace::demo(now).map(Some),
        Database::Path { path, .. } => match target {
            None => trace::load(path, &trace::Target::Latest, now),
            Some(id) => match named(path, id) {
                Ok(Ok(found)) => Ok(found),
                Ok(Err(message)) => {
                    let _ = io::stderr().write_all(message.as_bytes());
                    return Ok(1);
                }
                Err(e) => Err(e),
            },
        },
    };
    match read {
        Ok(Some(found)) if json => {
            let mut out = io::stdout().lock();
            out.write_all(serde_json::to_string_pretty(&found)?.as_bytes())?;
            out.write_all(b"\n")?;
            Ok(0)
        }
        Ok(Some(found)) => {
            let terminal = io::stdout().is_terminal();
            let color = recap_color(terminal, &|name| std::env::var(name).ok());
            // The terminal's size, less a row for the prompt after it; into a pipe, 80 by 40.
            // 60 by 16 at the least. The longest steps get the rows, as on the dashboard.
            let (columns, rows) = if terminal {
                let (c, r) = crate::platform::terminal_size();
                (c.unwrap_or(80), r.unwrap_or(41).saturating_sub(1))
            } else { (80, 40) };
            let (columns, rows) = (columns.max(60), rows.max(16));
            let (themes, skipped) = theme::Themes::from_environment(theme)?;
            for line in skipped {
                let _ = io::stderr().write_all(format!("skipped {line}\n").as_bytes());
            }
            let view = trace_view::View {
                selected: None,
                scroll: 0,
                // Printed once: nothing on it moves, so the open marks are drawn still.
                calm: true,
                mono: color == recap_view::Color::None,
                spin: 0,
                demo: matches!(database, Database::Demo),
                now,
            };
            let drawn = trace_view::draw(&found, &themes.palette(), columns, rows, &view, &crate::platform::local_time);
            let mut out = io::stdout().lock();
            out.write_all(recap_view::paint(&drawn.canvas, color).as_bytes())?;
            out.write_all(b"\n")?;
            Ok(0)
        }
        Ok(None) => {
            let message = match target {
                Some(id) => format!("No thread or run {id:?} on record.\n"),
                None => "Nothing to trace yet: T3 Code has no runs recorded on this machine.\n".to_owned(),
            };
            let _ = io::stderr().write_all(message.as_bytes());
            Ok(i32::from(target.is_some()))
        }
        Err(error) => {
            let origin = match database {
                Database::Path { origin, .. } => *origin,
                Database::Demo => Origin::Default,
            };
            let _ = io::stderr().write_all(advice(&error, origin).as_bytes());
            Ok(1)
        }
    }
}

#[cfg(test)]
mod trace_command_tests {
    use super::*;

    fn env() -> Environment {
        Environment { themes: true, ..Environment::default() }
    }
    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn trace_takes_an_optional_id_and_goes_with_json() {
        let parse = |words: &[&str]| parse_in(&args(words), &env());
        let target = |words: &[&str]| match parse(words) {
            Ok(Command::Trace { target, json, .. }) => (target, json),
            other => panic!("{other:?}"),
        };
        assert_eq!(target(&["--trace"]), (None, false));
        assert_eq!(target(&["--trace", "thread-1"]), (Some("thread-1".into()), false));
        assert_eq!(target(&["--trace=run-2", "--json"]), (Some("run-2".into()), true));
        assert_eq!(target(&["--trace", "--json", "--demo"]), (None, true));
        assert!(parse(&["--trace", "--recap"]).is_err());
        for incompatible in ["--status", "--tools", "--month", "--this-month", "--narrow"] {
            assert!(parse(&["--trace", incompatible]).is_err(), "{incompatible}");
        }
        assert!(parse(&["--tools", "--narrow"]).is_err());
    }

    #[test]
    fn the_original_rejects_trace() {
        let original = Environment { themes: false, ..Environment::default() };
        assert!(parse_in(&args(&["--trace"]), &original).unwrap_err().0.contains("Unknown option"));
        assert!(!usage(false).contains("--trace"));
        assert!(usage(true).contains("--trace"));
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
        assert_eq!(usage(true), format!("{USAGE}{THEME_USAGE}{RECAP_USAGE}{TOOLS_USAGE}{TRACE_USAGE}"));
        assert!(THEME_USAGE.lines().chain(RECAP_USAGE.lines()).chain(TRACE_USAGE.lines()).all(|l| l.chars().count() <= 80));
    }

    #[test]
    fn tools_is_the_products_and_the_original_rejects_it() {
        let themed = Environment { themes: true, ..env("/tmp/home", None) };
        for (flag, period) in [("", recap::Which::LastWeek), ("--this-week", recap::Which::ThisWeek), ("--month", recap::Which::LastMonth), ("--this-month", recap::Which::ThisMonth)] {
            let mut flags = vec!["--tools", "--json", "--demo", "--theme=paper", "--now-ms", "5"];
            if !flag.is_empty() { flags.push(flag); }
            assert!(matches!(parse_in(&args(&flags), &themed).unwrap(), Command::Tools { database: Database::Demo, now: Some(5.0), period: asked, json: true, size: None, theme: Some(theme) } if asked == period && theme == "paper"));
        }
        assert_eq!(parse_in(&args(&["--tools", "--recap"]), &themed), Err(UsageError("--recap and --tools cannot be used together".into())));
        for (flag, size) in [("--wide", recap_view::Size::Wide), ("--compact", recap_view::Size::Compact)] {
            assert!(matches!(parse_in(&args(&["--tools", flag]), &themed).unwrap(), Command::Tools { size: Some(asked), .. } if asked == size));
        }
        assert_eq!(parse_in(&args(&["--tools", "--wide", "--compact"]), &themed), Err(UsageError("--wide and --compact cannot be used together".into())));
        assert_eq!(parse_in(&args(&["--tools"]), &env("/tmp/home", None)), Err(UsageError("Unknown option '--tools'".into())));
        assert!(usage(true).contains("--tools") && !usage(false).contains("--tools"));
    }

    #[test]
    fn recap_is_the_products_and_the_original_rejects_it() {
        let themed = Environment { themes: true, ..env("/tmp/home", None) };
        assert_eq!(
            parse_in(&args(&["--recap", "--demo"]), &themed).unwrap(),
            Command::Recap { database: Database::Demo, now: None, period: recap::Which::LastWeek, json: false, size: None, theme: None }
        );
        match parse_in(&args(&["--recap", "--this-week", "--json", "--theme=paper", "--now-ms", "5"]), &themed).unwrap() {
            Command::Recap { period: recap::Which::ThisWeek, json: true, now: Some(now), theme: Some(theme), size: None, database: Database::Path { .. } } => {
                assert_eq!((now, theme.as_str()), (5., "paper"));
            }
            other => panic!("{other:?}"),
        }
        // A drawing can be asked for by name, one at a time.
        for (flag, size) in [("--wide", recap_view::Size::Wide), ("--compact", recap_view::Size::Compact), ("--narrow", recap_view::Size::Narrow)] {
            assert!(matches!(parse_in(&args(&["--recap", flag]), &themed).unwrap(), Command::Recap { size: Some(asked), .. } if asked == size));
        }
        for pair in [["--wide", "--compact"], ["--wide", "--narrow"], ["--compact", "--narrow"]] {
            assert_eq!(parse_in(&args(&["--recap", pair[0], pair[1]]), &themed), Err(UsageError("--wide, --compact and --narrow cannot be used together".into())));
        }
        assert_eq!(parse_in(&args(&["--recap", "--wide", "--compact", "--narrow"]), &themed), Err(UsageError("--wide, --compact and --narrow cannot be used together".into())));
        // A month, the last full one or the current one so far; one period at a time.
        for (flag, period) in [("--month", recap::Which::LastMonth), ("--this-month", recap::Which::ThisMonth)] {
            assert!(matches!(parse_in(&args(&["--recap", flag]), &themed).unwrap(), Command::Recap { period: asked, .. } if asked == period));
        }
        for pair in [["--month", "--this-month"], ["--this-week", "--month"], ["--this-week", "--this-month"]] {
            assert_eq!(parse_in(&args(&["--recap", pair[0], pair[1]]), &themed), Err(UsageError("--this-week, --month and --this-month cannot be used together".into())));
        }
        // The seven that only mean something with it say so.
        for flag in ["--json", "--this-week", "--month", "--this-month", "--wide", "--compact", "--narrow"] {
            assert_eq!(parse_in(&args(&[flag]), &themed), Err(UsageError("--this-week, --month, --this-month, --json, --wide, --compact and --narrow go with --recap or --tools".into())));
        }
        assert_eq!(parse_in(&args(&["--recap", "--help"]), &themed).unwrap(), Command::Help);
        assert!(parse_in(&args(&["--recap=1"]), &themed).is_err());
        assert!(usage(true).contains("--recap") && !usage(false).contains("--recap"));
        // With the original's behavior asked for, they are unknown options, as they are to it.
        for flag in ["--recap", "--this-week", "--month", "--this-month", "--json", "--wide", "--compact", "--narrow"] {
            assert_eq!(parse_in(&args(&[flag]), &env("/tmp/home", None)), Err(UsageError(format!("Unknown option '{flag}'"))));
        }
    }

    #[test]
    fn color_is_for_terminals_unless_asked_for_or_refused() {
        let with = |pairs: &'static [(&'static str, &'static str)]| move |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| (*v).to_owned());
        use recap_view::Color;
        assert_eq!(recap_color(true, &with(&[("COLORTERM", "truecolor")])), Color::True);
        assert_eq!(recap_color(true, &with(&[("TERM", "xterm-256color")])), Color::Indexed);
        assert_eq!(recap_color(false, &with(&[("COLORTERM", "truecolor")])), Color::None);
        assert_eq!(recap_color(false, &with(&[("FORCE_COLOR", "1")])), Color::True);
        assert_eq!(recap_color(true, &with(&[("NO_COLOR", "1"), ("FORCE_COLOR", "1")])), Color::None);
        assert_eq!(recap_color(true, &with(&[("TERM", "dumb")])), Color::None);
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
    fn a_card_wider_than_its_terminal_says_why_its_lines_wrap() {
        use recap_view::Size;
        // A phone of 60 columns asked for the 80-column card: it is drawn, and this follows it.
        let note = too_wide(Some(80), Some(60), Some(Size::Compact)).unwrap();
        assert_eq!(note, "loupe: the card is 80 columns wide,\nthis terminal is 60, so its lines wrap.\nLeave out --compact for what fits here.\n");
        // Each line fits a phone of 40 columns, for the wide card too.
        assert!(note.lines().all(|line| line.len() <= 40));
        let note = too_wide(Some(120), Some(100), Some(Size::Wide)).unwrap();
        assert_eq!(note, "loupe: the card is 120 columns wide,\nthis terminal is 100, so its lines wrap.\nLeave out --wide for what fits here.\n");
        assert!(note.lines().all(|line| line.len() <= 40));
        // A card that fits, a terminal that does not give its width, and a list that was not drawn: nothing.
        assert_eq!(too_wide(Some(80), Some(80), Some(Size::Compact)), None);
        assert_eq!(too_wide(Some(120), Some(200), Some(Size::Wide)), None);
        assert_eq!(too_wide(Some(80), None, Some(Size::Compact)), None);
        assert_eq!(too_wide(None, Some(40), None), None);
        // The list is 39 columns. Under that, the note names the list and stops, asked for or not.
        let list_note = "loupe: the list is 39 columns wide,\nthis terminal is 30, so its lines wrap.\n";
        assert_eq!(too_wide(Some(39), Some(30), Some(Size::Narrow)).as_deref(), Some(list_note));
        assert_eq!(too_wide(Some(39), Some(30), None).as_deref(), Some(list_note));
        assert_eq!(too_wide(Some(39), Some(39), Some(Size::Narrow)), None);
        assert!(list_note.lines().all(|line| line.len() <= 40));
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
            "loupe: no T3 database at /tmp/synthetic/statev2.sqlite\n  Is T3 Code installed, and has it been opened at least once?\n  Check the path given to --db.\n  To try the dashboard without T3 data: loupe --demo\n"
        );
        let schema = SourceError {
            kind: "schema",
            path: PathBuf::from("/tmp/synthetic/statev2.sqlite"),
            detail: "missing table".into(),
        };
        assert_eq!(
            advice(&schema, Origin::Default),
            "loupe: unsupported T3 schema in /tmp/synthetic/statev2.sqlite; missing table\n  T3 Code may have changed its database in an update: check for a newer Loupe.\n  To try the dashboard without T3 data: loupe --demo\n"
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
                "LOUPE · read-only · {} · 1 running · 0 waiting · 1 agents",
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
