//! Themes: which colors the dashboard draws with, and where they come from.
//!
//! A theme is an Omarchy `colors.toml`, read with the same small parser and the same derivation
//! as the live Omarchy theme ([`integration::derive_palette`]), plus a few optional keys of the
//! dashboard's own that pin a role instead of deriving it (`hot`, `warn`, `claude`, `openai`,
//! `grok`, `other`; see [`integration::ROLE_KEYS`]). `name` gives the theme a display name and
//! `base` starts it from another theme, whose keys it then overrides one by one.
//!
//! Where a theme can come from, by name: the user's own files in `~/.config/t3-agents/themes/`,
//! the built-in themes in the repository's `themes/` folder (compiled in), and any theme Omarchy
//! has installed (`~/.config/omarchy/themes/NAME/` and `$OMARCHY_PATH/themes/NAME/`). A folder
//! of Omarchy's may carry a `t3-agents.toml` next to its `colors.toml`, the way Omarchy themes
//! carry a file per app; its keys override the palette's. `omarchy` is the live Omarchy theme.
//!
//! Which theme is used: `--theme`, then `T3_AGENTS_THEME`, then `theme = "..."` in
//! `~/.config/t3-agents/config.toml`, then the live Omarchy theme when there is one, and
//! otherwise Frontier in dark mode and Paper in light. `dark:NAME,light:NAME` picks one for each:
//! on Omarchy the live theme's mode says which, elsewhere the terminal's background does.
//!
//! `T3_AGENTS_NO_THEMES` turns all of this off and keeps the original's behavior (the live
//! Omarchy theme, or the gray fallback), so the parity checks can compare the two programs.
use crate::{
    integration::{self, MAX_FILE, ROLE_KEYS},
    terminal::{self, Palette, Rgb},
};
use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

// ── Built-in themes ─────────────────────────────────────────────────────────

/// A theme compiled into the program from the repository's `themes/` folder.
#[derive(Debug)]
pub struct Builtin {
    /// The name `--theme` takes, and the file's name without `.toml`.
    pub name: &'static str,
    pub text: &'static str,
    /// Where the palette comes from: `T3 Agents`, `Omarchy` or `Dracula`.
    pub from: &'static str,
}

macro_rules! builtin {
    ($name:literal, $from:literal) => {
        Builtin {
            name: $name,
            text: include_str!(concat!("../../../themes/", $name, ".toml")),
            from: $from,
        }
    };
}

/// In the order `c` steps through them: the dashboard's own, Dracula, then Omarchy's defaults
/// in the order Omarchy lists them.
pub const BUILTINS: &[Builtin] = &[
    builtin!("frontier", "T3 Agents"),
    builtin!("paper", "T3 Agents"),
    builtin!("phosphor", "T3 Agents"),
    builtin!("dracula", "Dracula"),
    builtin!("catppuccin", "Omarchy"),
    builtin!("catppuccin-latte", "Omarchy"),
    builtin!("ethereal", "Omarchy"),
    builtin!("everforest", "Omarchy"),
    builtin!("flexoki-light", "Omarchy"),
    builtin!("gruvbox", "Omarchy"),
    builtin!("hackerman", "Omarchy"),
    builtin!("jade-horse", "Omarchy"),
    builtin!("kanagawa", "Omarchy"),
    builtin!("last-horizon", "Omarchy"),
    builtin!("lumon", "Omarchy"),
    builtin!("lupine", "Omarchy"),
    builtin!("matte-black", "Omarchy"),
    builtin!("miasma", "Omarchy"),
    builtin!("nord", "Omarchy"),
    builtin!("retro-82", "Omarchy"),
    builtin!("ristretto", "Omarchy"),
    builtin!("rose-pine", "Omarchy"),
    builtin!("solitude", "Omarchy"),
    builtin!("starship", "Omarchy"),
    builtin!("tokyo-night", "Omarchy"),
    builtin!("vantablack", "Omarchy"),
    builtin!("white", "Omarchy"),
];

/// The theme used where nothing chooses one and there is no Omarchy: Frontier in the dark,
/// Paper in the light.
pub const DEFAULT_DARK: &str = "frontier";
pub const DEFAULT_LIGHT: &str = "paper";
/// The name of the live Omarchy theme.
pub const OMARCHY: &str = "omarchy";

pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Whether the original's behavior was asked for: no themes, no `c`, no theme options.
pub fn disabled() -> bool {
    std::env::var_os("T3_AGENTS_NO_THEMES").is_some_and(|v| !v.is_empty())
}

// ── Where things are ────────────────────────────────────────────────────────

/// The folders themes are read from. Built from the environment once; tests build their own.
#[derive(Clone, Debug, Default)]
pub struct Paths {
    pub home: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME/t3-agents`, or `~/.config/t3-agents`: `config.toml` and `themes/`.
    pub config: Option<PathBuf>,
    /// `~/.local/state/omarchy/current`, where Omarchy assembles the live theme.
    pub omarchy_live: Option<PathBuf>,
    /// `~/.config/omarchy/themes`: the user's Omarchy themes.
    pub omarchy_user: Option<PathBuf>,
    /// `$OMARCHY_PATH/themes`, or `~/.local/share/omarchy/themes`: Omarchy's own.
    pub omarchy_system: Option<PathBuf>,
}

impl Paths {
    pub fn current() -> Self {
        Self::from_env(
            crate::platform::home_dir(),
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("OMARCHY_PATH"),
        )
    }
    /// Omarchy's scripts name their folders from `$HOME` directly, so these do too. Only the
    /// dashboard's own folder follows `XDG_CONFIG_HOME`, and only when it is absolute.
    pub fn from_env(
        home: Option<OsString>,
        xdg_config: Option<OsString>,
        omarchy_path: Option<OsString>,
    ) -> Self {
        let home = home.filter(|h| !h.is_empty()).map(PathBuf::from);
        let xdg = xdg_config.map(PathBuf::from).filter(|p| p.is_absolute());
        let config = xdg
            .or_else(|| home.as_ref().map(|h| h.join(".config")))
            .map(|c| c.join("t3-agents"));
        let omarchy_path = omarchy_path
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|h| h.join(".local/share/omarchy")));
        Self {
            config,
            omarchy_live: home.as_ref().map(|h| h.join(".local/state/omarchy/current")),
            omarchy_user: home.as_ref().map(|h| h.join(".config/omarchy/themes")),
            omarchy_system: omarchy_path.map(|p| p.join("themes")),
            home,
        }
    }
    pub fn config_file(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|c| c.join("config.toml"))
    }
    pub fn user_themes(&self) -> Option<PathBuf> {
        self.config.as_ref().map(|c| c.join("themes"))
    }
    fn live_dir(&self) -> Option<PathBuf> {
        self.omarchy_live.as_ref().map(|d| d.join("theme"))
    }
    /// Whether Omarchy has a live theme on this machine.
    pub fn omarchy_present(&self) -> bool {
        self.live_dir().is_some_and(|d| d.join("colors.toml").is_file())
    }
    /// The live Omarchy theme's name (`theme.name`), as Omarchy wrote it.
    pub fn omarchy_name(&self) -> Option<String> {
        let path = self.omarchy_live.as_ref()?.join("theme.name");
        let text = integration::read_bounded(&path, 4096).ok()?;
        let name = text.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }
}

// ── Names and sources ───────────────────────────────────────────────────────

/// Where a theme's text comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Builtin(&'static str),
    /// A theme file of the user's: `~/.config/t3-agents/themes/NAME.toml`, or a path.
    File(PathBuf),
    /// An installed Omarchy theme's folder: its `colors.toml`, then its `t3-agents.toml`.
    OmarchyDir(PathBuf),
    /// The live Omarchy theme.
    OmarchyLive,
}

/// `Tokyo Night` and `tokyo-night` name the same theme, as they do for `omarchy theme set`.
pub fn normalize(name: &str) -> String {
    name.trim().to_lowercase().replace(' ', "-")
}

fn path_like(reference: &str) -> bool {
    reference.contains('/')
        || reference.contains('\\')
        || reference.starts_with('~')
        || reference.ends_with(".toml")
}

/// A theme name a folder may be called: what Omarchy accepts for its theme folders.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

fn expand(reference: &str, paths: &Paths) -> PathBuf {
    match (reference.strip_prefix("~/"), &paths.home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if reference == "~" => paths.home.clone().unwrap_or_default(),
        _ => PathBuf::from(reference),
    }
}

/// Finds a theme by name or path. `None` when nothing by that name exists.
pub fn find(reference: &str, paths: &Paths) -> Option<Source> {
    if path_like(reference) {
        let path = expand(reference.trim(), paths);
        return path.is_file().then_some(Source::File(path));
    }
    let name = normalize(reference);
    if name == OMARCHY {
        return Some(Source::OmarchyLive);
    }
    if !valid_name(&name) {
        return None;
    }
    if let Some(file) = paths.user_themes().map(|d| d.join(format!("{name}.toml"))) {
        if file.is_file() {
            return Some(Source::File(file));
        }
    }
    if let Some(b) = builtin(&name) {
        return Some(Source::Builtin(b.name));
    }
    [&paths.omarchy_user, &paths.omarchy_system]
        .into_iter()
        .flatten()
        .map(|d| d.join(&name))
        .find(|d| d.join("colors.toml").is_file())
        .map(Source::OmarchyDir)
}

fn read(path: &Path) -> io::Result<String> {
    integration::read_bounded(path, MAX_FILE)
}

fn not_found(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, what)
}

/// One key's value in a theme's text, the way the derivation reads it (the last one wins).
pub fn key<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    integration::parse_vars(text)
        .into_iter()
        .find(|pair| pair.0 == name)
        .map(|pair| pair.1)
}

/// A built-in's role lines, to lay over the live Omarchy theme it matches.
fn role_lines(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        let Some((k, _)) = line.trim_start().split_once('=') else {
            continue;
        };
        if ROLE_KEYS.contains(&k.trim_end()) {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The same palette, as far as a theme's identity goes: background, foreground and accent.
fn same_palette(a: &str, b: &str) -> bool {
    ["background", "foreground", "accent"]
        .iter()
        .all(|k| integration::hex(key(a, k)) == integration::hex(key(b, k)))
}

/// A theme's full text: its own file, laid over its `base` if it names one. Later keys win,
/// so laying one text over another is putting it after it.
pub fn text(source: &Source, paths: &Paths) -> io::Result<String> {
    text_at(source, paths, 0)
}

fn text_at(source: &Source, paths: &Paths, depth: usize) -> io::Result<String> {
    if depth > 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "base themes nest more than four deep",
        ));
    }
    let own = match source {
        Source::Builtin(name) => return Ok(builtin(name).map_or("", |b| b.text).to_owned()),
        Source::File(path) => read(path)?,
        Source::OmarchyDir(dir) => {
            let mut text = read(&dir.join("colors.toml"))?;
            if let Ok(extra) = read(&dir.join("t3-agents.toml")) {
                text.push('\n');
                text.push_str(&extra);
            }
            text
        }
        Source::OmarchyLive => {
            let dir = paths
                .live_dir()
                .ok_or_else(|| not_found("no home folder".into()))?;
            let mut text = read(&dir.join("colors.toml"))?;
            match read(&dir.join("t3-agents.toml")) {
                Ok(extra) => {
                    text.push('\n');
                    text.push_str(&extra);
                }
                // One of Omarchy's own themes, as it ships: take the roles this program chose for it.
                Err(_) => {
                    let known = paths
                        .omarchy_name()
                        .and_then(|n| builtin(&normalize(&n)))
                        .filter(|b| b.from == "Omarchy" && same_palette(b.text, &text));
                    if let Some(b) = known {
                        text.push('\n');
                        text.push_str(&role_lines(b.text));
                    }
                }
            }
            return Ok(text);
        }
    };
    let Some(base) = key(&own, "base").map(str::to_owned) else {
        return Ok(own);
    };
    let found = find(&base, paths).ok_or_else(|| not_found(format!("no theme named {base:?} to start from")))?;
    if found == *source {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a theme can't start from itself"));
    }
    let mut text = text_at(&found, paths, depth + 1)?;
    text.push('\n');
    text.push_str(&own);
    Ok(text)
}

/// The palette a theme draws with. A theme that can't be read draws with the original's fallback,
/// as an unreadable Omarchy theme always has.
pub fn palette(source: &Source, paths: &Paths) -> Palette {
    text(source, paths).map_or_else(|_| Palette::fallback(), |t| integration::derive_themed(&t))
}

/// `tokyo-night` → `Tokyo Night`, as `omarchy theme list` writes names.
pub fn title(slug: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    for c in slug.chars() {
        if c == '-' {
            out.push(' ');
            start = true;
            continue;
        }
        out.push(if start { c.to_ascii_uppercase() } else { c });
        start = false;
    }
    out
}

/// The name a theme shows: its `name` key, else its name or file name, title-cased.
pub fn label(reference: &str, source: &Source, paths: &Paths) -> String {
    if *source == Source::OmarchyLive {
        return match paths.omarchy_name() {
            Some(name) => format!("Omarchy ({})", title(&name)),
            None => "Omarchy".into(),
        };
    }
    if let Some(name) = text(source, paths).ok().as_deref().and_then(|t| key(t, "name")) {
        if !name.trim().is_empty() {
            return name.trim().to_owned();
        }
    }
    let stem = match source {
        Source::File(path) => path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| reference.into()),
        Source::OmarchyDir(dir) => dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| reference.into()),
        Source::Builtin(name) => (*name).into(),
        Source::OmarchyLive => OMARCHY.into(),
    };
    title(&stem)
}

// ── The choice ──────────────────────────────────────────────────────────────

/// What the user picked: one theme, or one for dark mode and one for light.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    One(String),
    Pair { dark: String, light: String },
}

impl Choice {
    /// `NAME`, a path, `omarchy`, or `dark:NAME,light:NAME` (either order; Ghostty's form).
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("an empty theme name".into());
        }
        let paired = text.split(',').any(|part| {
            let part = part.trim_start();
            part.starts_with("dark:") || part.starts_with("light:")
        });
        if !paired {
            return Ok(Self::One(text.to_owned()));
        }
        let (mut dark, mut light) = (None, None);
        for part in text.split(',') {
            let part = part.trim();
            if let Some(name) = part.strip_prefix("dark:") {
                dark = Some(name.trim().to_owned());
            } else if let Some(name) = part.strip_prefix("light:") {
                light = Some(name.trim().to_owned());
            } else {
                return Err(format!("{part:?} is neither dark:NAME nor light:NAME"));
            }
        }
        match (dark, light) {
            (Some(dark), Some(light)) if !dark.is_empty() && !light.is_empty() => {
                Ok(Self::Pair { dark, light })
            }
            _ => Err("a pair needs both dark:NAME and light:NAME".into()),
        }
    }
    /// The theme for the given mode.
    pub fn reference(&self, dark: bool) -> &str {
        match self {
            Self::One(name) => name,
            Self::Pair { dark: d, light: l } => {
                if dark {
                    d
                } else {
                    l
                }
            }
        }
    }
    /// The same choice with the theme for one mode replaced (or the one theme, if there is one).
    pub fn with(&self, dark: bool, reference: String) -> Self {
        match self {
            Self::One(_) => Self::One(reference),
            Self::Pair { dark: d, light: l } => {
                if dark {
                    Self::Pair { dark: reference, light: l.clone() }
                } else {
                    Self::Pair { dark: d.clone(), light: reference }
                }
            }
        }
    }
    /// As it is written in the config file and to `--theme`.
    pub fn value(&self) -> String {
        match self {
            Self::One(name) => name.clone(),
            Self::Pair { dark, light } => format!("dark:{dark},light:{light}"),
        }
    }
    fn references(&self) -> Vec<&str> {
        match self {
            Self::One(name) => vec![name],
            Self::Pair { dark, light } => vec![dark, light],
        }
    }
}

/// Which setting chose the theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Flag,
    Env,
    Config,
    Default,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub choice: Choice,
    pub origin: Origin,
}

/// Parses a setting and checks that every theme it names exists.
pub fn check_choice(value: &str, paths: &Paths) -> Result<Choice, String> {
    let choice = Choice::parse(value)?;
    for reference in choice.references() {
        if find(reference, paths).is_none() {
            return Err(format!("no theme named {reference:?} (t3-agents --themes lists them)"));
        }
    }
    Ok(choice)
}

/// The `theme` line of the config file, if there is one.
pub fn config_value(paths: &Paths) -> Option<String> {
    let text = read(&paths.config_file()?).ok()?;
    key(&text, "theme").map(str::to_owned)
}

/// Which theme to use, and any setting that was skipped because it names nothing.
pub fn setting(flag: Option<&str>, env: Option<&str>, paths: &Paths) -> Result<(Setting, Vec<String>), String> {
    if let Some(value) = flag {
        let choice = check_choice(value, paths).map_err(|e| format!("--theme: {e}"))?;
        return Ok((Setting { choice, origin: Origin::Flag }, vec![]));
    }
    let mut skipped = Vec::new();
    if let Some(value) = env.filter(|v| !v.trim().is_empty()) {
        match check_choice(value, paths) {
            Ok(choice) => return Ok((Setting { choice, origin: Origin::Env }, skipped)),
            Err(e) => skipped.push(format!("T3_AGENTS_THEME: {e}")),
        }
    }
    if let Some(value) = config_value(paths) {
        match check_choice(&value, paths) {
            Ok(choice) => return Ok((Setting { choice, origin: Origin::Config }, skipped)),
            Err(e) => skipped.push(format!("theme in {}: {e}", paths.config_file().unwrap_or_default().display())),
        }
    }
    let choice = if paths.omarchy_present() {
        Choice::One(OMARCHY.into())
    } else {
        Choice::Pair { dark: DEFAULT_DARK.into(), light: DEFAULT_LIGHT.into() }
    };
    Ok((Setting { choice, origin: Origin::Default }, skipped))
}

/// Writes `theme = "VALUE"` into the config file, replacing the line that is there and keeping
/// every other line. The new file is written beside the old one and renamed over it.
pub fn save(paths: &Paths, choice: &Choice) -> io::Result<PathBuf> {
    let value = choice.value();
    if value.contains('"') || value.contains('\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a theme name with a quote can't be saved"));
    }
    let file = paths
        .config_file()
        .ok_or_else(|| not_found("no home folder for the config file".into()))?;
    let dir = file.parent().expect("config file has a folder");
    fs::create_dir_all(dir)?;
    let old = match read(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let line = format!("theme = \"{value}\"");
    let mut replaced = false;
    let mut out = String::new();
    for l in old.lines() {
        let is_theme = l
            .trim_start()
            .split_once('=')
            .is_some_and(|(k, _)| k.trim_end() == "theme");
        if is_theme && !replaced {
            out.push_str(&line);
            replaced = true;
        } else if is_theme {
            continue;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !replaced {
        out.push_str(&line);
        out.push('\n');
    }
    let temp = dir.join(format!(".config.toml.{}.tmp", std::process::id()));
    fs::write(&temp, out)?;
    fs::rename(&temp, &file).inspect_err(|_| {
        let _ = fs::remove_file(&temp);
    })?;
    Ok(file)
}

// ── The list ────────────────────────────────────────────────────────────────

/// A theme `c` can step to, and `--themes` lists.
#[derive(Clone, Debug)]
pub struct Entry {
    pub reference: String,
    pub source: Source,
    pub from: String,
}

fn toml_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(read) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut files: Vec<_> = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    files
}

fn theme_dirs(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(read) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut dirs: Vec<_> = read
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, path)| valid_name(name) && path.join("colors.toml").is_file())
        .collect();
    dirs.sort();
    dirs
}

/// Every theme by name: the live Omarchy theme when there is one, the built-ins, the user's
/// files, then Omarchy's installed themes that are not built in.
pub fn entries(paths: &Paths) -> Vec<Entry> {
    let mut out = Vec::new();
    if paths.omarchy_present() {
        out.push(Entry { reference: OMARCHY.into(), source: Source::OmarchyLive, from: "Omarchy, live".into() });
    }
    let mut taken: Vec<String> = Vec::new();
    let user: Vec<_> = paths.user_themes().map(|d| toml_files(&d)).unwrap_or_default();
    let user_names: Vec<String> = user
        .iter()
        .filter_map(|p| p.file_stem().map(|s| normalize(&s.to_string_lossy())))
        .collect();
    for b in BUILTINS {
        // A file of the user's by the same name is the one `--theme NAME` finds.
        if user_names.iter().any(|n| n == b.name) {
            continue;
        }
        out.push(Entry { reference: b.name.into(), source: Source::Builtin(b.name), from: b.from.into() });
        taken.push(b.name.into());
    }
    for (path, name) in user.iter().zip(&user_names) {
        if !valid_name(name) || taken.contains(name) {
            continue;
        }
        out.push(Entry { reference: name.clone(), source: Source::File(path.clone()), from: "yours".into() });
        taken.push(name.clone());
    }
    for (dir, from) in [(&paths.omarchy_user, "Omarchy, yours"), (&paths.omarchy_system, "Omarchy, installed")] {
        let Some(dir) = dir else { continue };
        for (name, path) in theme_dirs(dir) {
            let name = normalize(&name);
            if taken.contains(&name) || name == OMARCHY {
                continue;
            }
            out.push(Entry { reference: name.clone(), source: Source::OmarchyDir(path), from: from.into() });
            taken.push(name);
        }
    }
    out
}

// ── The running dashboard's themes ──────────────────────────────────────────

/// The theme state of a running dashboard: the setting, and the mode it is in.
#[derive(Clone, Debug)]
pub struct Themes {
    pub paths: Paths,
    pub setting: Setting,
    omarchy: bool,
    /// The terminal's background, as it last reported it: dark or not.
    terminal_dark: Option<bool>,
}

impl Themes {
    pub fn new(setting: Setting, paths: Paths) -> Self {
        let omarchy = paths.omarchy_present();
        Self { paths, setting, omarchy, terminal_dark: None }
    }
    /// From the command line's `--theme`, `T3_AGENTS_THEME`, the config file and this machine.
    pub fn from_environment(flag: Option<&str>) -> Result<(Self, Vec<String>), String> {
        let paths = Paths::current();
        let env = std::env::var("T3_AGENTS_THEME").ok();
        let (setting, skipped) = setting(flag, env.as_deref(), &paths)?;
        Ok((Self::new(setting, paths), skipped))
    }
    /// Whether the dashboard has to ask the terminal for its background: a dark/light pair away
    /// from Omarchy, whose live theme says the mode itself.
    pub fn follows_terminal(&self) -> bool {
        matches!(self.setting.choice, Choice::Pair { .. }) && !self.omarchy
    }
    /// Records the terminal's background. Returns whether the mode changed.
    pub fn set_terminal_dark(&mut self, dark: bool) -> bool {
        let before = self.dark();
        self.terminal_dark = Some(dark);
        before != self.dark()
    }
    /// The mode: the live Omarchy theme's on Omarchy, else the terminal's, else dark.
    pub fn dark(&self) -> bool {
        if self.omarchy {
            if let Some(dir) = self.paths.live_dir() {
                if let Ok(text) = read(&dir.join("colors.toml")) {
                    return integration::derive_palette(&text).dark;
                }
            }
        }
        self.terminal_dark.unwrap_or(true)
    }
    /// The theme in use now.
    pub fn active(&self) -> String {
        self.setting.choice.reference(self.dark()).to_owned()
    }
    pub fn source(&self) -> Source {
        let active = self.active();
        find(&active, &self.paths).unwrap_or(Source::OmarchyLive)
    }
    pub fn palette(&self) -> Palette {
        match find(&self.active(), &self.paths) {
            Some(source) => palette(&source, &self.paths),
            None => Palette::fallback(),
        }
    }
    /// `c` (or `C`, `back`): the next theme in [`entries`], for the current mode when the choice
    /// is a pair. Saved to the config file unless `--theme` or `T3_AGENTS_THEME` chose the theme.
    /// Returns the notice to show.
    pub fn cycle(&mut self, back: bool) -> String {
        let list = entries(&self.paths);
        if list.is_empty() {
            return "No themes to choose from".into();
        }
        let dark = self.dark();
        let current = normalize(self.setting.choice.reference(dark));
        let at = list.iter().position(|e| e.reference == current);
        let next = match (at, back) {
            (Some(i), false) => (i + 1) % list.len(),
            (Some(i), true) => (i + list.len() - 1) % list.len(),
            (None, false) => 0,
            (None, true) => list.len() - 1,
        };
        let entry = &list[next];
        self.setting.choice = self.setting.choice.with(dark, entry.reference.clone());
        let name = label(&entry.reference, &entry.source, &self.paths);
        let which = match self.setting.choice {
            Choice::Pair { .. } if dark => "Dark theme",
            Choice::Pair { .. } => "Light theme",
            Choice::One(_) => "Theme",
        };
        let saved = match self.setting.origin {
            Origin::Flag => " · not saved: --theme chose it".to_owned(),
            Origin::Env => " · not saved: T3_AGENTS_THEME chose it".to_owned(),
            Origin::Config | Origin::Default => match save(&self.paths, &self.setting.choice) {
                Ok(_) => {
                    self.setting.origin = Origin::Config;
                    String::new()
                }
                Err(e) => format!(" · not saved: {e}"),
            },
        };
        format!("{which}: {name} · c next, C back{saved}")
    }
}

/// Whether a terminal background is light: OpenTUI 0.8.3's rule (`src/renderer.ts`,
/// `inferThemeModeFromBackgroundColor`): brightness `(299 r + 587 g + 114 b) / 1000` above 128.
pub fn light_background(rgb: Rgb) -> bool {
    let (r, g, b) = ((rgb >> 16) & 255, (rgb >> 8) & 255, rgb & 255);
    (r * 299 + g * 587 + b * 114) as f64 / 1000. > 128.
}

// ── Checking a theme ────────────────────────────────────────────────────────

/// OKLab, from sRGB.
fn oklab(rgb: [f64; 3]) -> [f64; 3] {
    let [r, g, b] = rgb;
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn linear(c: Rgb) -> [f64; 3] {
    let ch = |shift: u32| {
        let s = ((c >> shift) & 255) as f64 / 255.;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    [ch(16), ch(8), ch(0)]
}

/// Colorblind simulations at full severity: Machado, Oliveira and Fernandes (2009), on linear RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vision {
    Normal,
    Protan,
    Deutan,
}

fn simulate(c: Rgb, vision: Vision) -> [f64; 3] {
    let m = match vision {
        Vision::Normal => return linear(c),
        Vision::Protan => [
            [0.152286, 1.052583, -0.204868],
            [0.114503, 0.786281, 0.099216],
            [-0.003882, -0.048116, 1.051998],
        ],
        Vision::Deutan => [
            [0.367322, 0.860646, -0.227968],
            [0.280085, 0.672501, 0.047413],
            [-0.011820, 0.042940, 0.968881],
        ],
    };
    let [r, g, b] = linear(c);
    let row = |i: usize| (m[i][0] * r + m[i][1] * g + m[i][2] * b).clamp(0., 1.);
    [row(0), row(1), row(2)]
}

/// The distance between two colors as seen with `vision`: OKLab ΔE, times 100.
pub fn delta_e(a: Rgb, b: Rgb, vision: Vision) -> f64 {
    let (a, b) = (oklab(simulate(a, vision)), oklab(simulate(b, vision)));
    100. * ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// The smaller of the protanopia and deuteranopia distances.
pub fn colorblind_delta(a: Rgb, b: Rgb) -> f64 {
    delta_e(a, b, Vision::Protan).min(delta_e(a, b, Vision::Deutan))
}

/// OKLCH hue in degrees, and chroma.
fn hue(c: Rgb) -> (f64, f64) {
    let [_, a, b] = oklab(linear(c));
    (b.atan2(a).to_degrees().rem_euclid(360.), a.hypot(b))
}

fn hue_gap(a: Rgb, b: Rgb) -> f64 {
    let d = (hue(a).0 - hue(b).0).abs() % 360.;
    d.min(360. - d)
}

/// Below this, colorblind readers can't tell two colors apart.
pub const COLORBLIND_FLOOR: f64 = 6.;
/// Two colors this far apart are easy to tell apart, colorblind or not.
pub const COLORBLIND_COMFORT: f64 = 8.;
/// The least distance two colors that mean different things should have with normal vision.
pub const NORMAL_FLOOR: f64 = 12.;

/// The roles that have to stay apart, as drawn.
pub fn signal_roles(p: &Palette) -> [(&'static str, Rgb); 5] {
    [
        ("Claude", p.providers[0]),
        ("OpenAI", p.providers[1]),
        ("Grok", p.providers[2]),
        ("needs you", p.hot),
        ("warning", p.warn),
    ]
}

/// The pair of signal roles closest together, as seen with `vision` (or by the colorblind
/// measure when `None`): the distance and the two names.
pub fn closest(p: &Palette, vision: Option<Vision>) -> (f64, &'static str, &'static str) {
    let roles = signal_roles(p);
    let mut best = (f64::INFINITY, "", "");
    for (i, a) in roles.iter().enumerate() {
        for b in &roles[i + 1..] {
            let d = match vision {
                Some(v) => delta_e(a.1, b.1, v),
                None => colorblind_delta(a.1, b.1),
            };
            if d < best.0 {
                best = (d, a.0, b.0);
            }
        }
    }
    best
}

fn hex6(c: Rgb) -> String {
    format!("#{c:06x}")
}

/// Edit distance, for spotting a misspelt role key.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != *cb)).min(row[j] + 1).min(row[j + 1] + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

/// The keys the dashboard reads.
const READ_KEYS: [&str; 15] = [
    "name", "base", "mode", "background", "foreground", "accent", "selection", "red", "bright_red",
    "yellow", "bright_yellow", "magenta", "green", "blue", "theme",
];

/// `--theme-check`: what the theme draws with, what the dashboard changed and why, and how far
/// apart the colors that mean different things are.
pub fn check(reference: &str, paths: &Paths) -> Result<String, String> {
    let source = find(reference, paths).ok_or_else(|| format!("no theme named {reference:?} (t3-agents --themes lists them)"))?;
    let text = text(&source, paths).map_err(|e| format!("can't read the theme {reference:?}: {e}"))?;
    let vars = integration::parse_vars(&text);
    let p = integration::derive_themed(&text);
    let roles = integration::Roles::from_vars(&vars);
    let name = label(reference, &source, paths);
    let place = match &source {
        Source::Builtin(_) => "built in".to_owned(),
        Source::File(path) => path.display().to_string(),
        Source::OmarchyDir(dir) => dir.display().to_string(),
        Source::OmarchyLive => "the live Omarchy theme".to_owned(),
    };
    let mut out = format!("{name} ({place}) · {}\n\n", if p.dark { "dark" } else { "light" });
    let rows = [
        ("background", p.bg),
        ("text", p.fg),
        ("secondary text", p.text2),
        ("captions", p.dim),
        ("ramp / accent", p.accent),
        ("selected row", p.sel),
        ("needs you", p.hot),
        ("warning", p.warn),
        ("Claude", p.providers[0]),
        ("OpenAI / Codex", p.providers[1]),
        ("Grok", p.providers[2]),
        ("other providers", p.providers[3]),
    ];
    out.push_str("Colors, as drawn\n");
    for (role, c) in rows {
        out.push_str(&format!("  {role:<17} {}\n", hex6(c)));
    }

    let mut notes: Vec<String> = Vec::new();
    let page = [p.bg, p.panel];
    let worst_text = [p.fg, p.text2, p.dim]
        .iter()
        .flat_map(|c| page.iter().map(move |s| integration::contrast(*c, *s)))
        .fold(f64::INFINITY, f64::min);
    let written = |k: &str| integration::hex(key(&text, k));
    if let Some(fg) = written("foreground") {
        if fg != p.fg {
            notes.push(format!("text {} is drawn {} to read at 4.5:1", hex6(fg), hex6(p.fg)));
        }
    }
    if let Some(accent) = written("accent").or_else(|| written("blue")) {
        if accent != p.accent {
            notes.push(format!("the accent {} is drawn {} to read at 4.5:1 (key hints are drawn in it)", hex6(accent), hex6(p.accent)));
        }
    }
    for (k, pinned, drawn, role) in [("hot", roles.hot, p.hot, "needs you"), ("warn", roles.warn, p.warn, "warning")] {
        if let Some(c) = pinned.filter(|c| *c != drawn) {
            notes.push(format!("{k} {} is drawn {} so the {role} text reads at 4.5:1", hex6(c), hex6(drawn)));
        }
    }
    if roles.claude.is_none() && roles.openai.is_none() && roles.grok.is_none() {
        // The derivation's stock set, darkened in light themes as it darkens it.
        let stock = [0x8b5896, 0x1f9b82, 0x4c94ec].map(|c| if p.dark { c } else { terminal::mix(c, 0, 0.12) });
        if p.providers[..3] == stock {
            notes.push("providers use the stock plum, teal and blue: this palette's magenta, green and blue are too gray or too dim; set claude, openai and grok to choose".into());
        }
    }
    let ac = hue(p.accent).1;
    for (role, c) in &signal_roles(&p)[..3] {
        if ac > 0.04 && hue(*c).1 > 0.04 && hue_gap(*c, p.accent) < 20. {
            notes.push(format!("{role}'s color is {:.0}° from the accent's hue: its lanes will look like context bars", hue_gap(*c, p.accent)));
        }
        for (alert, a) in [("needs you", p.hot), ("warning", p.warn)] {
            if hue(*c).1 > 0.04 && hue(a).1 > 0.05 && hue_gap(*c, a) < 15. {
                notes.push(format!("{role}'s color is {:.0}° from {alert}'s hue: one color, two meanings", hue_gap(*c, a)));
            }
        }
        let cr = integration::contrast(*c, p.bg);
        if cr < 3. {
            notes.push(format!("{role}'s color is {cr:.2}:1 on the background; a dot and a lane want 3:1"));
        }
    }
    for (k, _) in &vars {
        if READ_KEYS.contains(k) || ROLE_KEYS.contains(k) {
            continue;
        }
        if let Some(near) = ROLE_KEYS.iter().chain(READ_KEYS.iter()).find(|r| distance(k, r) <= 2 && k.len() > 2) {
            notes.push(format!("{k:?} is not a key the dashboard reads; did you mean {near:?}?"));
        }
    }

    out.push_str("\nContrast\n");
    out.push_str(&format!("  text, worst role   {worst_text:.2}:1   (4.5 needed)\n"));
    out.push_str(&format!("  needs you          {:.2}:1   (4.5 needed)\n", integration::contrast(p.hot, p.bg)));
    out.push_str(&format!("  warning            {:.2}:1   (4.5 needed)\n", integration::contrast(p.warn, p.bg)));

    let (cb, a, b) = closest(&p, None);
    let (nv, na, nb) = closest(&p, Some(Vision::Normal));
    let verdict = if cb >= COLORBLIND_COMFORT {
        "comfortable"
    } else if cb >= COLORBLIND_FLOOR {
        "readable"
    } else {
        "too close: colorblind readers may see one color"
    };
    out.push_str("\nSeparation of Claude, OpenAI, Grok, needs you and warning (OKLab ΔE × 100)\n");
    out.push_str(&format!("  colorblind     {cb:5.1}  {a} vs {b}: {verdict} (6 floor, 8 comfortable)\n"));
    out.push_str(&format!(
        "  normal vision  {nv:5.1}  {na} vs {nb}: {}\n",
        if nv >= NORMAL_FLOOR {
            "fine (12 floor)"
        } else if nv >= 10. {
            "close (12 floor)"
        } else {
            "too close (12 floor)"
        }
    ));
    out.push_str("  (protanopia and deuteranopia, simulated after Machado, Oliveira and Fernandes, 2009)\n");

    out.push_str("\nNotes\n");
    if notes.is_empty() {
        out.push_str("  none\n");
    }
    for n in notes {
        out.push_str(&format!("  - {n}\n"));
    }
    Ok(out)
}

/// `--themes`: every theme by name, which one is in use, and which are colorblind-safe.
pub fn list(themes: Option<&Themes>, paths: &Paths) -> String {
    let active = themes.map(|t| normalize(&t.active()));
    let rows: Vec<(String, String, String, String, bool)> = entries(paths)
        .into_iter()
        .map(|e| {
            let p = palette(&e.source, paths);
            let safe = closest(&p, None).0 >= COLORBLIND_FLOOR;
            (
                e.reference.clone(),
                label(&e.reference, &e.source, paths),
                if p.dark { "dark".into() } else { "light".into() },
                e.from,
                safe,
            )
        })
        .collect();
    let w0 = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(4).max(4);
    let w1 = rows.iter().map(|r| r.1.chars().count()).max().unwrap_or(4).max(4);
    let mut out = String::new();
    if let Some(t) = themes {
        let how = match t.setting.origin {
            Origin::Flag => "--theme",
            Origin::Env => "T3_AGENTS_THEME",
            Origin::Config => "the config file",
            Origin::Default => "the default",
        };
        out.push_str(&format!("In use: {} ({how})\n\n", t.setting.choice.value()));
    }
    out.push_str(&format!("  {:<w0$}  {:<w1$}  mode   colorblind-safe  from\n", "name", "shows as"));
    for (reference, label, mode, from, safe) in rows {
        let mark = if active.as_deref() == Some(reference.as_str()) { "*" } else { " " };
        out.push_str(&format!(
            "{mark} {reference:<w0$}  {label:<w1$}  {mode:<5}  {:<15}  {from}\n",
            if safe { "yes" } else { "no" }
        ));
    }
    out.push_str("\n--theme NAME picks one; dark:NAME,light:NAME follows light and dark mode.\n");
    out.push_str("c and C step through them in the dashboard and remember the choice.\n");
    if let Some(file) = paths.config_file() {
        out.push_str(&format!("Saved in {}. Your own themes go in {}.\n", file.display(), paths.user_themes().unwrap_or_default().display()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A folder of its own under the system's temporary folder, removed when dropped. Every path a
    /// test reads lives in one; nothing reads the real home folder.
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "t3-agents-theme-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn write(&self, rel: &str, text: &str) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, text).unwrap();
            path
        }
        fn paths(&self) -> Paths {
            Paths::from_env(Some(self.0.clone().into()), None, None)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const TOKYO: &str = "background=\"#1a1b26\"\nforeground=\"#c0caf5\"\naccent=\"#7aa2f7\"\nselection=\"#33467c\"\nred=\"#f7768e\"\nyellow=\"#e0af68\"\ngreen=\"#9ece6a\"\nblue=\"#7aa2f7\"\nmagenta=\"#bb9af7\"";

    fn without_roles(text: &str) -> String {
        text.lines()
            .filter(|l| !l.trim_start().split_once('=').is_some_and(|(k, _)| ROLE_KEYS.contains(&k.trim_end())))
            .map(|l| format!("{l}\n"))
            .collect()
    }

    #[test]
    fn without_role_keys_the_themed_palette_is_the_originals() {
        let light = "mode=\"light\"\nbackground=\"#fafafa\"\nforeground=\"#222222\"\naccent=\"#3366cc\"\nred=\"#cc3333\"\nyellow=\"#b58900\"\ngreen=\"#2e7d32\"\nblue=\"#1565c0\"\nmagenta=\"#8e24aa\"";
        let solitude = "mode=\"dark\"\naccent=\"#798186\"\nselection=\"#343d41\"\nbackground=\"#101315\"\nforeground=\"#cacccc\"\nred=\"#565d60\"\nyellow=\"#d9dbdc\"\nbright_red=\"#de6145\"\nbright_yellow=\"#c9c2b4\"";
        for text in ["", TOKYO, light, solitude] {
            assert_eq!(integration::derive_themed(text), integration::derive_palette(text));
        }
        assert_eq!(integration::derive_themed(TOKYO), Palette::tokyo());
        // Every built-in, with its role lines taken out, is the original's palette of its palette.
        for b in BUILTINS {
            let plain = without_roles(b.text);
            assert_eq!(integration::derive_themed(&plain), integration::derive_palette(&plain), "{}", b.name);
        }
    }

    #[test]
    fn role_keys_pin_their_roles_and_text_roles_keep_their_contrast() {
        let pinned = format!("{TOKYO}\nclaude=\"#d97757\"\nopenai=\"#91dfd2\"\ngrok=\"#9ea4ff\"\nother=\"#777777\"\nwarn=\"#f2bb25\"");
        let p = integration::derive_themed(&pinned);
        assert_eq!(p.providers, [0xd97757, 0x91dfd2, 0x9ea4ff, 0x777777]);
        assert_eq!(p.warn, 0xf2bb25);
        let o = Palette::tokyo();
        assert_eq!((p.bg, p.fg, p.accent, p.hot, p.sel, p.floor), (o.bg, o.fg, o.accent, o.hot, o.sel, o.floor));
        // A pinned needs-you color that can't be read as text is lifted until it can.
        let dim = integration::derive_themed(&format!("{TOKYO}\nhot=\"#401020\""));
        assert_ne!(dim.hot, 0x401020);
        assert!(integration::contrast(dim.hot, dim.bg) >= 4.5);
        // In a light theme a pinned provider is drawn as written; the derived ones are darkened.
        let light = "mode=\"light\"\nbackground=\"#fafafa\"\nforeground=\"#222222\"\nmagenta=\"#8e24aa\"\ngreen=\"#2e7d32\"\nblue=\"#1565c0\"";
        let lp = integration::derive_themed(&format!("{light}\nclaude=\"#5f3369\""));
        assert_eq!(lp.providers[0], 0x5f3369);
        assert_eq!(lp.providers[1], integration::derive_palette(light).providers[1]);
        // With no usable yellow, the warning comes from the pinned needs-you color.
        let gray = "background=\"#101315\"\nforeground=\"#cacccc\"\nyellow=\"#d9dbdc\"";
        let a = integration::derive_themed(&format!("{gray}\nhot=\"#ff3ea5\""));
        assert_ne!(a.warn, integration::derive_palette(gray).warn);
    }

    #[test]
    fn role_values_never_stand_in_for_the_most_chromatic_color() {
        // A gray red sends the original looking for the most chromatic value in the file.
        let gray = "background=\"#101315\"\nforeground=\"#cacccc\"\nred=\"#565d60\"";
        let with_role = format!("{gray}\nclaude=\"#ff0000\"");
        assert_eq!(integration::derive_themed(&with_role).hot, integration::derive_palette(gray).hot);
        // The original never knew the key, so for it the value is just another color.
        assert_ne!(integration::derive_palette(&with_role).hot, integration::derive_palette(gray).hot);
    }

    #[test]
    fn every_builtin_reads_and_keeps_what_means_different_things_apart() {
        let mut names: Vec<_> = BUILTINS.iter().map(|b| b.name).collect();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), BUILTINS.len(), "built-in names are unique");
        for b in BUILTINS {
            let p = integration::derive_themed(b.text);
            let mode = key(b.text, "mode").unwrap_or("dark");
            assert_eq!(p.dark, mode != "light", "{}: mode", b.name);
            for (role, c) in &signal_roles(&p)[..3] {
                assert!(integration::contrast(*c, p.bg) >= 3., "{}: {role} at 3:1", b.name);
            }
            // Catppuccin keeps its own peach beside its yellow at 11.2; everything else reaches 12.
            assert!(closest(&p, Some(Vision::Normal)).0 >= 11., "{}: normal vision {:?}", b.name, closest(&p, Some(Vision::Normal)));
            assert!(closest(&p, None).0 >= 5.5, "{}: colorblind {:?}", b.name, closest(&p, None));
            if b.from != "Omarchy" {
                assert!(closest(&p, None).0 >= COLORBLIND_FLOOR, "{}: colorblind-safe", b.name);
            }
        }
        // The defaults exist and are the dark and light pair the documentation names.
        assert!(!integration::derive_themed(builtin(DEFAULT_DARK).unwrap().text).dark == false);
        assert!(!integration::derive_themed(builtin(DEFAULT_LIGHT).unwrap().text).dark);
    }

    #[test]
    fn choices_are_names_paths_or_a_dark_and_light_pair() {
        assert_eq!(Choice::parse(" paper ").unwrap(), Choice::One("paper".into()));
        assert_eq!(Choice::parse("~/t.toml").unwrap(), Choice::One("~/t.toml".into()));
        let pair = Choice::parse("light:paper, dark:frontier").unwrap();
        assert_eq!(pair, Choice::Pair { dark: "frontier".into(), light: "paper".into() });
        assert_eq!(pair.reference(true), "frontier");
        assert_eq!(pair.reference(false), "paper");
        assert_eq!(pair.value(), "dark:frontier,light:paper");
        assert_eq!(pair.with(false, "white".into()).value(), "dark:frontier,light:white");
        assert!(Choice::parse("dark:frontier").is_err());
        assert!(Choice::parse("dark:frontier,paper").is_err());
        assert!(Choice::parse("  ").is_err());
    }

    #[test]
    fn names_find_the_users_file_then_a_builtin_then_an_installed_omarchy_theme() {
        let t = Temp::new();
        let paths = t.paths();
        assert_eq!(find("Tokyo Night", &paths), Some(Source::Builtin("tokyo-night")));
        assert_eq!(find("omarchy", &paths), Some(Source::OmarchyLive));
        assert_eq!(find("nothing-by-that-name", &paths), None);
        assert_eq!(find("../escape", &paths), None);
        let mine = t.write(".config/t3-agents/themes/paper.toml", "background=\"#ffffff\"");
        assert_eq!(find("paper", &paths), Some(Source::File(mine)));
        t.write(".config/omarchy/themes/osaka-jade/colors.toml", "background=\"#111c18\"");
        assert_eq!(find("osaka-jade", &paths), Some(Source::OmarchyDir(t.0.join(".config/omarchy/themes/osaka-jade"))));
        t.write(".local/share/omarchy/themes/aurora/colors.toml", "background=\"#000000\"");
        assert_eq!(find("aurora", &paths), Some(Source::OmarchyDir(t.0.join(".local/share/omarchy/themes/aurora"))));
        let file = t.write("elsewhere/x.toml", "background=\"#000000\"");
        assert_eq!(find("~/elsewhere/x.toml", &paths), Some(Source::File(file)));
        // The list shows each name once: the user's paper replaces the built-in one.
        let list = entries(&paths);
        assert_eq!(list.iter().filter(|e| e.reference == "paper").count(), 1);
        assert!(list.iter().any(|e| e.reference == "osaka-jade"));
        assert!(!list.iter().any(|e| e.reference == "omarchy"), "no live Omarchy theme here");
    }

    #[test]
    fn a_theme_starts_from_its_base_and_overrides_it_key_by_key() {
        let t = Temp::new();
        let paths = t.paths();
        let file = t.write(".config/t3-agents/themes/mine.toml", "base = \"tokyo-night\"\nclaude = \"#ff0000\"\n");
        let p = palette(&Source::File(file), &paths);
        let base = integration::derive_themed(builtin("tokyo-night").unwrap().text);
        assert_eq!(p.providers[0], 0xff0000);
        assert_eq!((p.bg, p.providers[1], p.providers[2]), (base.bg, base.providers[1], base.providers[2]));
        let looped = t.write(".config/t3-agents/themes/loop.toml", "base = \"loop\"\n");
        assert!(text(&Source::File(looped), &paths).is_err());
        let missing = t.write(".config/t3-agents/themes/orphan.toml", "base = \"no-such-theme\"\n");
        assert!(text(&Source::File(missing.clone()), &paths).is_err());
        assert_eq!(palette(&Source::File(missing), &paths), Palette::fallback());
    }

    #[test]
    fn the_live_omarchy_theme_takes_our_roles_only_when_it_is_one_of_omarchys_own() {
        let t = Temp::new();
        let paths = t.paths();
        let tokyo = builtin("tokyo-night").unwrap();
        let shipped = without_roles(tokyo.text);
        t.write(".local/state/omarchy/current/theme/colors.toml", &shipped);
        t.write(".local/state/omarchy/current/theme.name", "tokyo-night\n");
        assert!(paths.omarchy_present());
        assert_eq!(palette(&Source::OmarchyLive, &paths), integration::derive_themed(tokyo.text));
        // Edited, it is the user's palette now: the derivation alone, as the original draws it.
        let edited = shipped.replace("#1a1b26", "#16161e");
        t.write(".local/state/omarchy/current/theme/colors.toml", &edited);
        assert_eq!(palette(&Source::OmarchyLive, &paths), integration::derive_palette(&edited));
        // A theme's own t3-agents.toml wins over everything.
        t.write(".local/state/omarchy/current/theme/t3-agents.toml", "claude = \"#123456\"\n");
        assert_eq!(palette(&Source::OmarchyLive, &paths).providers[0], 0x123456);
        assert_eq!(label(OMARCHY, &Source::OmarchyLive, &paths), "Omarchy (Tokyo Night)");
    }

    #[test]
    fn the_setting_comes_from_the_flag_the_environment_the_config_then_this_machine() {
        let t = Temp::new();
        let paths = t.paths();
        let (s, _) = setting(None, None, &paths).unwrap();
        assert_eq!(s.choice, Choice::Pair { dark: DEFAULT_DARK.into(), light: DEFAULT_LIGHT.into() });
        assert_eq!(s.origin, Origin::Default);
        t.write(".local/state/omarchy/current/theme/colors.toml", TOKYO);
        assert_eq!(setting(None, None, &paths).unwrap().0.choice, Choice::One(OMARCHY.into()));
        t.write(".config/t3-agents/config.toml", "# mine\ntheme = \"dracula\"\n");
        assert_eq!(setting(None, None, &paths).unwrap().0, Setting { choice: Choice::One("dracula".into()), origin: Origin::Config });
        assert_eq!(setting(None, Some("paper"), &paths).unwrap().0.origin, Origin::Env);
        assert_eq!(setting(Some("white"), Some("paper"), &paths).unwrap().0.origin, Origin::Flag);
        // A bad flag is an error; a bad environment or config value is skipped, and said.
        assert!(setting(Some("nope"), None, &paths).is_err());
        let (s, skipped) = setting(None, Some("nope"), &paths).unwrap();
        assert_eq!((s.origin, skipped.len()), (Origin::Config, 1));
    }

    #[test]
    fn saving_replaces_the_theme_line_and_keeps_the_rest() {
        let t = Temp::new();
        let paths = t.paths();
        let file = save(&paths, &Choice::One("paper".into())).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "theme = \"paper\"\n");
        fs::write(&file, "# keep me\ntheme = \"paper\"\nother = \"x\"\ntheme = \"old\"\n").unwrap();
        save(&paths, &Choice::Pair { dark: "dracula".into(), light: "paper".into() }).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "# keep me\ntheme = \"dark:dracula,light:paper\"\nother = \"x\"\n");
        assert!(save(&paths, &Choice::One("a\"b".into())).is_err());
        assert_eq!(fs::read_dir(file.parent().unwrap()).unwrap().count(), 1, "no temporary file left");
    }

    #[test]
    fn c_steps_through_the_list_and_changes_only_the_current_modes_theme() {
        let t = Temp::new();
        let paths = t.paths();
        let (s, _) = setting(None, None, &paths).unwrap();
        let mut themes = Themes::new(s, paths.clone());
        assert!(themes.follows_terminal());
        assert_eq!(themes.active(), DEFAULT_DARK);
        let notice = themes.cycle(false);
        assert_eq!(themes.setting.choice, Choice::Pair { dark: "paper".into(), light: DEFAULT_LIGHT.into() });
        assert!(notice.starts_with("Dark theme: Paper"), "{notice}");
        assert_eq!(config_value(&paths).as_deref(), Some("dark:paper,light:paper"));
        assert_eq!(themes.setting.origin, Origin::Config);
        themes.set_terminal_dark(false);
        themes.cycle(true);
        assert_eq!(themes.setting.choice, Choice::Pair { dark: "paper".into(), light: "frontier".into() });
        // Chosen by --theme, the choice changes but is not saved.
        let mut flagged = Themes::new(Setting { choice: Choice::One("white".into()), origin: Origin::Flag }, paths.clone());
        let notice = flagged.cycle(true);
        assert_eq!(flagged.setting.choice, Choice::One("vantablack".into()));
        assert!(notice.contains("not saved"), "{notice}");
        assert_eq!(config_value(&paths).as_deref(), Some("dark:paper,light:frontier"));
    }

    #[test]
    fn on_omarchy_the_mode_comes_from_the_live_theme_not_the_terminal() {
        let t = Temp::new();
        let paths = t.paths();
        t.write(".local/state/omarchy/current/theme/colors.toml", "mode = \"light\"\nbackground = \"#ffffff\"\n");
        let pair = Setting { choice: Choice::parse("dark:frontier,light:paper").unwrap(), origin: Origin::Config };
        let mut themes = Themes::new(pair, paths);
        assert!(!themes.follows_terminal());
        themes.set_terminal_dark(true);
        assert_eq!(themes.active(), "paper");
    }

    #[test]
    fn a_light_background_is_opentuis_brightness_over_128() {
        assert!(light_background(0xfafafa));
        assert!(!light_background(0x1a1b26));
        assert!(!light_background(0x808080)); // brightness 128 is not above 128
        assert!(light_background(0x818181));
    }

    #[test]
    fn the_check_says_what_changed_and_spots_a_misspelt_key() {
        let t = Temp::new();
        let paths = t.paths();
        t.write(".config/t3-agents/themes/odd.toml", "background = \"#1a1b26\"\nforeground = \"#c0caf5\"\naccent = \"#7aa2f7\"\nhot = \"#401020\"\ngrok = \"#7ba3f8\"\nclaud = \"#d97757\"\n");
        let report = check("odd", &paths).unwrap();
        assert!(report.contains("hot #401020 is drawn"), "{report}");
        assert!(report.contains("\"claud\" is not a key the dashboard reads; did you mean \"claude\"?"), "{report}");
        assert!(report.contains("Grok's color is"), "{report}");
        let frontier = check("frontier", &paths).unwrap();
        assert!(frontier.contains("hot #bf3ca4 is drawn #c550ad so the needs you text reads at 4.5:1"), "{frontier}");
        assert!(check("nothing-here", &paths).is_err());
        let listed = list(None, &paths);
        assert!(listed.contains("frontier") && listed.contains("tokyo-night"), "{listed}");
    }
}
