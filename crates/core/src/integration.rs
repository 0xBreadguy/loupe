//! Opt-in file, window-title and link integration. This module has no automatic paths,
//! database access or startup side effects; the caller must select live mode and paths.
//! Ports `oracle/src/{focus,theme,usage}.ts` at the experiment baseline.
use crate::{
    model::Agent,
    overview,
    terminal::{self, Palette},
};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

pub(crate) const MAX_FILE: u64 = 1 << 20;

pub(crate) fn read_bounded(path: &Path, max: u64) -> io::Result<String> {
    let file = File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a bounded regular file",
        ));
    }
    let mut text = String::new();
    file.take(max + 1).read_to_string(&mut text)?;
    if text.len() as u64 > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    Ok(text)
}

/// Only regular `*.json` files immediately inside the explicitly supplied directory.
/// Bad/unready records are ignored, counters are re-derived for the supplied clock.
/// Symlink entries are ignored. No recursion, default HOME path, account calls or writes.
pub fn usage(dir: &Path, now: f64) -> Vec<overview::Provider> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.file_name().to_string_lossy().ends_with(".json")
        })
        .map(|e| e.path())
        .collect();
    files.sort();
    let mut providers: Vec<_> = files
        .iter()
        .filter_map(|path| {
            let text = read_bounded(path, MAX_FILE).ok()?;
            let record = serde_json::from_str(&text).ok()?;
            overview::parse_provider(&record, now)
        })
        .collect();
    providers.sort_by(|a, b| b.today.total_cmp(&a.today));
    providers
}

/// Read exactly the explicit theme file; callers choose fallback/error policy.
pub fn palette(path: &Path) -> io::Result<Palette> {
    read_bounded(path, MAX_FILE).map(|text| derive_palette(&text))
}

pub(crate) fn hex(value: Option<&str>) -> Option<u32> {
    let trimmed = js_trim(value?);
    let s = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if s.len() != 6 || !s.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(s, 16).ok()
}
pub(crate) fn luminance(c: u32) -> f64 {
    let lin = |shift: u32| {
        let s = ((c >> shift) & 255u32) as f64 / 255.;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(16) + 0.7152 * lin(8) + 0.0722 * lin(0)
}
pub(crate) fn contrast(a: u32, b: u32) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub(crate) fn chroma(c: u32) -> u32 {
    let values = [(c >> 16) & 255, (c >> 8) & 255, c & 255];
    values.iter().max().unwrap() - values.iter().min().unwrap()
}

// ── Telling colors apart ────────────────────────────────────────────────────
// The same arithmetic as `deltaE`, `colorblindDelta` and `readsBlue` in `oracle/src/theme.ts`,
// operation for operation, so both derive the same Gemini color.

/// sRGB channels, linearized.
pub(crate) fn linear(c: u32) -> [f64; 3] {
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

/// OKLab, from linear sRGB.
pub(crate) fn oklab(rgb: [f64; 3]) -> [f64; 3] {
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

/// Colorblind simulations at full severity: Machado, Oliveira and Fernandes (2009), on linear RGB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vision {
    Normal,
    Protan,
    Deutan,
}

fn simulate(c: u32, vision: Vision) -> [f64; 3] {
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
pub fn delta_e(a: u32, b: u32, vision: Vision) -> f64 {
    let (a, b) = (oklab(simulate(a, vision)), oklab(simulate(b, vision)));
    let (dl, da, db) = (a[0] - b[0], a[1] - b[1], a[2] - b[2]);
    100. * (dl * dl + da * da + db * db).sqrt()
}

/// The smaller of the protanopia and deuteranopia distances.
pub fn colorblind_delta(a: u32, b: u32) -> f64 {
    delta_e(a, b, Vision::Protan).min(delta_e(a, b, Vision::Deutan))
}

/// OKLCH hue in degrees, and chroma.
pub(crate) fn oklch(c: u32) -> (f64, f64) {
    let [_, a, b] = oklab(linear(c));
    let h = b.atan2(a).to_degrees();
    (if h < 0. { h + 360. } else { h }, (a * a + b * b).sqrt())
}

/// Whether a color reads as blue: OKLCH hue from 230° (past cyan) to 275° (short of violet), and
/// chroma of at least 0.08, so a blue-gray doesn't count.
pub fn reads_blue(c: u32) -> bool {
    let (h, ch) = oklch(c);
    (230. ..=275.).contains(&h) && ch >= 0.08
}

/// The blues Gemini takes when a theme's own don't work, in order: Google's blue, then deeper,
/// lighter and deeper still, all near OKLCH hue 260. A theme whose Grok already holds one of them
/// still has another.
pub const GEMINI_STOCK: [u32; 5] = [0x4285f4, 0x2867e4, 0x83c4ff, 0x1e4eac, 0x113585];

/// Gemini's color when the theme doesn't pin one. The candidates are the theme's `blue`, its
/// `bright_blue`, then [`GEMINI_STOCK`], each darkened in a light theme as the derived providers
/// are. The first that reads as blue, reaches 3:1 on the background and stands at least 12 apart
/// (OKLab ΔE × 100) for normal vision, and 6 for protanopia and deuteranopia, from every other
/// color that means something (`signals`: Claude, OpenAI, Grok, needs you and the warning, as
/// drawn) is Gemini's. When none qualifies, the stock blue that reaches 3:1 and stands farthest
/// from its nearest signal; when none reaches 3:1, the one with the most contrast.
fn derive_gemini(theme: [Option<u32>; 2], shade: impl Fn(u32) -> u32, bg: u32, signals: &[u32; 5]) -> u32 {
    let fits = |c: u32| {
        reads_blue(c)
            && contrast(c, bg) >= 3.
            && signals
                .iter()
                .all(|s| delta_e(c, *s, Vision::Normal) >= 12. && colorblind_delta(c, *s) >= 6.)
    };
    let stock = GEMINI_STOCK.map(&shade);
    if let Some(c) = theme.into_iter().flatten().map(&shade).chain(stock).find(|c| fits(*c)) {
        return c;
    }
    let apart = |c: u32| {
        signals
            .iter()
            .map(|s| (delta_e(c, *s, Vision::Normal) / 12.).min(colorblind_delta(c, *s) / 6.))
            .fold(f64::INFINITY, f64::min)
    };
    let best = |score: &dyn Fn(u32) -> f64, among: &mut dyn Iterator<Item = u32>| {
        among.fold(None, |best: Option<u32>, c| if best.is_none_or(|b| score(c) > score(b)) { Some(c) } else { best })
    };
    let readable = &mut stock.into_iter().filter(|c| contrast(*c, bg) >= 3.);
    best(&apart, readable)
        .or_else(|| best(&|c| contrast(c, bg), &mut stock.into_iter()))
        .unwrap()
}
fn most_chromatic(colors: impl IntoIterator<Item = Option<u32>>) -> Option<u32> {
    // Stable first-on-tie ordering matches JS's stable sort and Object.values order.
    colors.into_iter().flatten().fold(None, |best, c| {
        if best.is_none_or(|b| chroma(c) > chroma(b)) {
            Some(c)
        } else {
            best
        }
    })
}
fn worst(c: u32, surfaces: &[u32]) -> f64 {
    surfaces
        .iter()
        .map(|s| contrast(c, *s))
        .fold(f64::INFINITY, f64::min)
}
pub(crate) fn reach(from: u32, to: u32, target: f64, surfaces: &[u32]) -> u32 {
    if worst(from, surfaces) >= target {
        return from;
    }
    let mut best = from;
    for i in 1..=100 {
        let c = terminal::mix(from, to, i as f64 / 100.);
        if worst(c, surfaces) >= target {
            return c;
        }
        if worst(c, surfaces) > worst(best, surfaces) {
            best = c;
        }
    }
    best
}
fn dimmest(fg: u32, bg: u32, target: f64, surfaces: &[u32]) -> u32 {
    let (mut lo, mut hi) = (0., 1.);
    for _ in 0..16 {
        let t = (lo + hi) / 2.;
        if worst(terminal::mix(fg, bg, t), surfaces) >= target {
            lo = t;
        } else {
            hi = t;
        }
    }
    terminal::mix(fg, bg, lo)
}

/// The dashboard's own keys a theme may add to an Omarchy `colors.toml`, each pinning one role
/// that is otherwise derived: `hot` (needs you), `warn` (a limit is near, a run stalled) and the
/// five provider colors. `derive_palette` ignores them, as the original does.
pub const ROLE_KEYS: [&str; 7] = ["hot", "warn", "claude", "openai", "grok", "gemini", "other"];

/// Role colors a theme pins. `None` leaves the role to the derivation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Roles {
    pub hot: Option<u32>,
    pub warn: Option<u32>,
    pub claude: Option<u32>,
    pub openai: Option<u32>,
    pub grok: Option<u32>,
    pub gemini: Option<u32>,
    pub other: Option<u32>,
}

impl Roles {
    pub fn from_vars(vars: &[(&str, &str)]) -> Self {
        let color = |name| hex(vars.iter().find(|pair| pair.0 == name).map(|pair| pair.1));
        Self {
            hot: color("hot"),
            warn: color("warn"),
            claude: color("claude"),
            openai: color("openai"),
            grok: color("grok"),
            gemini: color("gemini"),
            other: color("other"),
        }
    }
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The same intentionally small TOML subset as `parseColorsToml`, preserving
/// insertion order for ties in the accent selection. No extra TOML dependency.
/// A key that appears again keeps its first place and takes the later value, so a file
/// appended to another overrides it key by key.
pub fn parse_vars(text: &str) -> Vec<(&str, &str)> {
    let mut vars: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((key, rest)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim_end();
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        let Some(rest) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        let Some((value, _)) = rest.split_once('"') else {
            continue;
        };
        if let Some(pair) = vars.iter_mut().find(|pair| pair.0 == key) {
            pair.1 = value;
        } else {
            vars.push((key, value));
        }
    }
    vars
}

/// The original's palette from a `colors.toml`. Role keys are ignored, as the original ignores
/// every key it does not know.
pub fn derive_palette(text: &str) -> Palette {
    derive_from(&parse_vars(text), None)
}

/// [`derive_palette`], with the theme's role keys pinning their roles. For a file without role
/// keys the two are the same palette, number for number.
pub fn derive_themed(text: &str) -> Palette {
    let vars = parse_vars(text);
    let roles = Roles::from_vars(&vars);
    derive_from(&vars, Some(&roles))
}

/// `roles` is `None` for the original's derivation. With roles, a pinned `hot` or `warn` replaces
/// the color the derivation would pick and still goes through the contrast guard (it is drawn as
/// text); pinned provider colors are used as written, in light themes too; and role values never
/// stand in as the most chromatic color of the theme.
fn derive_from(vars: &[(&str, &str)], roles: Option<&Roles>) -> Palette {
    let pins = roles.copied().unwrap_or_default();
    let value = |name| vars.iter().find(|pair| pair.0 == name).map(|pair| pair.1);
    let color = |name| hex(value(name));
    let bg = color("background").unwrap_or(0x101315);
    let dark = value("mode")
        .filter(|m| !m.is_empty())
        .map_or_else(|| luminance(bg) < 0.4, |m| m != "light");
    let ink = if dark { 0xffffff } else { 0x000000 };
    let panel = terminal::mix(bg, color("foreground").unwrap_or(0xcacccc), 0.04);
    let page = [bg, panel];
    let fg = reach(color("foreground").unwrap_or(0xcacccc), ink, 4.5, &page);
    let mut accent = color("accent").or_else(|| color("blue")).unwrap_or(fg);
    if contrast(accent, bg) < 2.4 {
        accent = terminal::mix(accent, fg, 0.5);
    }
    accent = reach(accent, fg, 4.5, &page);
    let hot = pins.hot.unwrap_or_else(|| {
        let hot = most_chromatic([color("bright_red"), color("red")]);
        if hot.is_none_or(|c| chroma(c) < 70) {
            let palette = vars
                .iter()
                .filter(|pair| roles.is_none() || !ROLE_KEYS.contains(&pair.0));
            most_chromatic(palette.map(|pair| hex(Some(pair.1))))
                .filter(|c| chroma(*c) >= 70)
                .unwrap_or(0xde6145)
        } else {
            hot.unwrap()
        }
    });
    let hot = reach(hot, ink, 4.5, &page);
    let warn = pins.warn.unwrap_or_else(|| {
        let warn = most_chromatic([color("yellow"), color("bright_yellow")]);
        if warn.is_none_or(|c| chroma(c) < 60) {
            terminal::mix(hot, fg, 0.5)
        } else {
            warn.unwrap()
        }
    });
    let warn = reach(warn, ink, 4.5, &page);
    let mut sel = color("selection").unwrap_or_else(|| terminal::mix(bg, fg, 0.14));
    if contrast(sel, bg) < 1.25 {
        sel = terminal::mix(bg, accent, 0.3);
    }
    let text2 = dimmest(fg, bg, 4.5, &[bg, panel, sel]);
    let dim = dimmest(fg, bg, 4.5, &page);
    let track = terminal::mix(fg, bg, 0.86);
    let themed = [color("magenta"), color("green"), color("blue")];
    let usable = themed
        .iter()
        .all(|c| c.is_some_and(|c| chroma(c) >= 60 && contrast(c, bg) >= 2.6));
    let base = if usable {
        themed.map(Option::unwrap)
    } else {
        [0x8b5896, 0x1f9b82, 0x4c94ec]
    };
    let shade = |c| if dark { c } else { terminal::mix(c, 0, 0.12) };
    let claude = pins.claude.unwrap_or_else(|| shade(base[0]));
    let openai = pins.openai.unwrap_or_else(|| shade(base[1]));
    let grok = pins.grok.unwrap_or_else(|| shade(base[2]));
    let gemini = pins.gemini.unwrap_or_else(|| {
        derive_gemini([color("blue"), color("bright_blue")], shade, bg, &[claude, openai, grok, hot, warn])
    });
    let mut p = Palette {
        dark,
        bg,
        panel,
        fg,
        text2,
        dim,
        track,
        accent,
        hot,
        warn,
        sel,
        faint: terminal::mix(fg, bg, 0.74),
        rule: terminal::mix(fg, bg, 0.8),
        bright: terminal::mix(fg, ink, 0.5),
        busy: terminal::mix(bg, accent, 0.16),
        floor: 0.,
        providers: [
            claude,
            openai,
            grok,
            gemini,
            pins.other.unwrap_or_else(|| terminal::mix(fg, bg, 0.52)),
        ],
    };
    let tracks = [track, terminal::mix(track, sel, 0.4)];
    let score = |k| worst(terminal::ramp(&p, k), &tracks);
    let mut floor = 0.;
    while floor < 1. && score(floor) < 3. {
        floor = ((floor + 0.01) * 100.).round() / 100.;
    }
    p.floor = if floor < 1. {
        floor
    } else {
        (0..=100)
            .map(|i| i as f64 / 100.)
            .reduce(|a, b| if score(b) > score(a) { b } else { a })
            .unwrap()
    };
    p
}

/// `String.prototype.trim`: the ECMAScript whitespace set, which differs from Rust's `trim`
/// (U+0085 is not trimmed, U+FEFF is).
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(terminal::is_js_whitespace)
}

/// `(a ?? "") > (b ?? "")` with JavaScript's UTF-16 code-unit order, not UTF-8 byte order.
fn js_greater(a: &str, b: &str) -> bool {
    a.encode_utf16().cmp(b.encode_utf16()).is_gt()
}

fn norm(s: &str) -> String {
    js_trim(&terminal::clean(s)).to_owned()
}
/// Project/title wins over title alone; ties use latest update, native subagents excluded.
pub fn thread_for_title<A: std::borrow::Borrow<Agent>>(title: &str, agents: &[A]) -> Option<String> {
    let title = norm(title);
    if title.is_empty() {
        return None;
    }
    let mut best: Option<&Agent> = None;
    let mut best_rank = 0;
    for a in agents
        .iter()
        .map(|a| a.borrow())
        .filter(|a| a.id == a.thread_id || a.remote.as_ref().and_then(crate::model::AgentRemote::thread_ref).is_some())
    {
        let name = norm(&a.title);
        let rank = if title == format!("{} / {}", norm(&a.project), name) {
            2
        } else if title == name {
            1
        } else {
            0
        };
        if rank > best_rank
            || rank > 0
                && rank == best_rank
                && js_greater(
                    a.updated.as_deref().unwrap_or(""),
                    best.and_then(|b| b.updated.as_deref()).unwrap_or(""),
                )
        {
            best = Some(a);
            best_rank = rank;
        }
    }
    best.map(|a| a.id.clone())
}
/// Only the environment-id metadata file immediately beside the explicitly selected DB.
pub fn environment_id(db_path: &Path) -> Option<String> {
    let id = read_bounded(&db_path.parent()?.join("environment-id"), 4096).ok()?;
    let id = js_trim(&id);
    environment_id_valid(id).then(|| id.into())
}
/// `^[\w-]{1,128}$` from identity.ts.
pub fn environment_id_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub(crate) fn encode_segment(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        // encodeURIComponent's exact unescaped set, including punctuation.
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(byte as char);
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}
/// Validated cross-machine identity from raw string parts, matching identity.ts.
pub fn thread_ref_parts<'a>(env: &'a str, id: &'a str) -> Option<(&'a str, &'a str)> {
    if !environment_id_valid(env)
        || js_trim(id).is_empty()
        || id.encode_utf16().count() > 1024
        || id.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
    {
        return None;
    }
    Some((env, id))
}
/// Validated cross-machine identity, matching identity.ts.
pub fn thread_ref(value: &serde_json::Value) -> Option<(&str, &str)> {
    let env = value.get("environmentId")?.as_str()?;
    let id = value.get("threadId")?.as_str()?;
    thread_ref_parts(env, id)
}
/// `JSON.stringify([environmentId, threadId])` from identity.ts.
pub fn thread_key(environment: &str, thread_id: &str) -> String {
    serde_json::to_string(&[environment, thread_id]).expect("strings serialize")
}
pub fn agent_thread_link(a: &Agent, local: Option<&str>) -> Option<String> {
    if let Some((env, id)) = a.remote.as_ref().and_then(crate::model::AgentRemote::thread_ref) {
        thread_link(env, id)
    } else {
        local.and_then(|env| thread_link(env, &a.thread_id))
    }
}
pub fn thread_link(environment: &str, thread_id: &str) -> Option<String> {
    // JS .length is UTF-16 code units, rather than Rust UTF-8 bytes.
    if environment.is_empty() || thread_id.is_empty() || thread_id.encode_utf16().count() > 1024 {
        return None;
    }
    Some(format!(
        "t3code://threads/{}/{}",
        encode_segment(environment),
        encode_segment(thread_id)
    ))
}
/// `calm.ts`'s `animationsOff`: whether Omarchy's toggle left its no-animations flag in
/// `XDG_STATE_HOME` or in `~/.local/state`. Reads only those flag files, never a config.
pub fn animations_off() -> bool {
    animations_off_with(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        crate::platform::home_dir().as_deref(),
    )
}

/// [`animations_off`] with the two state roots supplied. An empty variable counts as unset.
pub fn animations_off_with(
    xdg_state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> bool {
    const FLAG: [&str; 4] = ["omarchy", "toggles", "hypr", "no-animations.lua"];
    let xdg = xdg_state_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let fallback = home
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".local").join("state"));
    [xdg, fallback].into_iter().flatten().any(|dir| {
        let mut flag = dir;
        for part in FLAG {
            flag.push(part);
        }
        flag.exists()
    })
}

/// Call only for an explicit open action in live mode. No shell or desktop configuration writes.
pub fn open_link(link: &str) -> io::Result<()> {
    if !link.starts_with("t3code://") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a T3 Code link",
        ));
    }
    crate::platform::open_url(link)
}

/// This session's environment only. It is the caller's choice whether to start a watcher.
pub fn hypr_dir() -> Option<PathBuf> {
    hypr_dir_for(
        std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
        std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok().as_deref(),
    )
}
fn hypr_dir_for(runtime: Option<&std::ffi::OsStr>, signature: Option<&str>) -> Option<PathBuf> {
    let (runtime, signature) = (runtime?, signature?);
    if runtime.is_empty()
        || signature.is_empty()
        || !signature
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    {
        return None;
    }
    Some(PathBuf::from(runtime).join("hypr").join(signature))
}

#[cfg(unix)]
mod focus {
    use super::*;
    use std::{
        collections::HashMap,
        io::Write,
        net::Shutdown,
        os::unix::net::UnixStream,
        sync::{
            Arc, Mutex,
            mpsc::{self, Sender},
        },
        thread::{self, JoinHandle},
        time::Duration,
    };
    const RETRY: Duration = Duration::from_secs(10);
    #[derive(Default)]
    struct Windows {
        windows: HashMap<String, String>,
        current: Option<String>,
        title: Option<String>,
    }
    fn strip(s: &str) -> String {
        s.trim().strip_prefix("0x").unwrap_or(s.trim()).into()
    }
    fn is_t3(s: &str) -> bool {
        s.to_ascii_lowercase().contains("t3code")
    }
    impl Windows {
        fn sync(&mut self, dir: &Path) -> io::Result<()> {
            let mut socket = UnixStream::connect(dir.join(".socket.sock"))?;
            socket.set_read_timeout(Some(Duration::from_millis(1500)))?;
            socket.set_write_timeout(Some(Duration::from_millis(1500)))?;
            socket.write_all(b"j/clients")?;
            let mut reply = String::new();
            socket.take(MAX_FILE + 1).read_to_string(&mut reply)?;
            if reply.len() as u64 > MAX_FILE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "window list exceeds limit",
                ));
            }
            // Unknown client fields are discarded during deserialization; only matching T3
            // addresses/titles survive this function. No app titles are logged or exported.
            #[derive(serde::Deserialize)]
            struct Client {
                address: Option<String>,
                class: Option<String>,
                title: Option<String>,
                #[serde(rename = "focusHistoryID")]
                focus: Option<f64>,
            }
            let clients: Vec<Client> = serde_json::from_str(&reply).map_err(io::Error::other)?;
            let mut windows = HashMap::new();
            let mut best = None;
            let mut rank = f64::INFINITY;
            for c in clients {
                let Some(address) = c.address.filter(|s| !s.is_empty()) else {
                    continue;
                };
                if !is_t3(c.class.as_deref().unwrap_or("")) {
                    continue;
                }
                let address = strip(&address);
                windows.insert(address.clone(), c.title.unwrap_or_default());
                let focus = c.focus.unwrap_or(1e9);
                if best.is_none() || focus < rank {
                    best = Some(address);
                    rank = focus;
                }
            }
            self.windows = windows;
            self.current = best;
            Ok(())
        }
        fn event(&mut self, line: &str) -> bool {
            let Some((name, data)) = line.split_once(">>") else {
                return false;
            };
            match name {
                "windowtitlev2" => {
                    if let Some((address, title)) = data.split_once(',') {
                        if let Some(known) = self.windows.get_mut(&strip(address)) {
                            *known = title.into();
                        }
                    }
                }
                "activewindowv2" => {
                    let address = strip(data);
                    if self.windows.contains_key(&address) {
                        self.current = Some(address);
                    }
                }
                "closewindow" => return self.windows.contains_key(&strip(data)),
                "openwindow" => return is_t3(data),
                _ => {}
            }
            false
        }
        fn publish(&mut self, sender: &Sender<Option<String>>) -> bool {
            let title = self
                .current
                .as_ref()
                .and_then(|id| self.windows.get(id))
                .cloned();
            if self.title == title {
                return true;
            }
            self.title = title.clone();
            sender.send(title).is_ok()
        }
    }
    /// One blocking event subscription with a bounded command re-sync on T3 open/close.
    /// No title polling. Drop shuts down the socket and interrupts reconnect waits.
    pub struct FocusWatcher {
        stop: Option<Sender<()>>,
        socket: Arc<Mutex<Option<UnixStream>>>,
        worker: Option<JoinHandle<()>>,
    }
    impl FocusWatcher {
        pub fn start(dir: PathBuf, sender: Sender<Option<String>>) -> io::Result<Self> {
            let (stop_tx, stop_rx) = mpsc::channel();
            let socket = Arc::new(Mutex::new(None::<UnixStream>));
            let shared = socket.clone();
            let worker = thread::Builder::new()
                .name("t3-rune-focus".into())
                .spawn(move || {
                    let mut windows = Windows::default();
                    loop {
                        if stop_rx.try_recv().is_ok() {
                            break;
                        }
                        // Subscribe before the initial list, so queued events close the race.
                        if let Ok(mut events) = UnixStream::connect(dir.join(".socket2.sock")) {
                            *shared.lock().unwrap() = events.try_clone().ok();
                            let _ = windows.sync(&dir);
                            if !windows.publish(&sender) {
                                break;
                            }
                            let mut buffer = Vec::new();
                            let mut chunk = [0; 8192];
                            loop {
                                if stop_rx.try_recv().is_ok() {
                                    return;
                                }
                                let n = match events.read(&mut chunk) {
                                    Ok(0) | Err(_) => break,
                                    Ok(n) => n,
                                };
                                buffer.extend_from_slice(&chunk[..n]);
                                while let Some(nl) = buffer.iter().position(|c| *c == b'\n') {
                                    if nl <= 65536 {
                                        if let Ok(line) = std::str::from_utf8(&buffer[..nl]) {
                                            if windows.event(line) {
                                                let _ = windows.sync(&dir);
                                            }
                                            if !windows.publish(&sender) {
                                                return;
                                            }
                                        }
                                    }
                                    buffer.drain(..=nl);
                                }
                                if buffer.len() > 65536 {
                                    buffer.clear();
                                }
                            }
                            *shared.lock().unwrap() = None;
                        }
                        match stop_rx.recv_timeout(RETRY) {
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            _ => break,
                        }
                    }
                })?;
            Ok(Self {
                stop: Some(stop_tx),
                socket,
                worker: Some(worker),
            })
        }
        pub fn stop(&mut self) {
            if let Some(stop) = self.stop.take() {
                let _ = stop.send(());
            }
            if let Some(socket) = self.socket.lock().unwrap().as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
    impl Drop for FocusWatcher {
        fn drop(&mut self) {
            self.stop();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::net::UnixListener;
        #[test]
        fn synthetic_socket_follows_only_t3_titles_and_focus() {
            let temp = super::super::tests::Temp::new();
            let commands = UnixListener::bind(temp.0.join(".socket.sock")).unwrap();
            let events = UnixListener::bind(temp.0.join(".socket2.sock")).unwrap();
            let (clients_tx, clients_rx) = mpsc::channel();
            let server = thread::spawn(move || {
                for clients in clients_rx {
                    let (mut s, _) = commands.accept().unwrap();
                    let mut request = [0; 9];
                    s.read_exact(&mut request).unwrap();
                    assert_eq!(&request, b"j/clients");
                    s.write_all(clients).unwrap();
                }
            });
            clients_tx.send(br#"[{"address":"0xaa","class":"com.t3tools.T3Code","title":"p / One","focusHistoryID":3},{"address":"0xbb","class":"foot","title":"private unrelated title","focusHistoryID":0},{"address":"0xcc","class":"T3CODE","title":"p / Two","focusHistoryID":1}]"#.as_slice()).unwrap();
            let (tx, rx) = mpsc::channel();
            let mut watcher = FocusWatcher::start(temp.0.clone(), tx).unwrap();
            let (mut stream, _) = events.accept().unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap().as_deref(),
                Some("p / Two")
            );
            stream
                .write_all(b"windowtitlev2>>cc,p / Two, renamed\n")
                .unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap().as_deref(),
                Some("p / Two, renamed")
            );
            stream
                .write_all(b"activewindowv2>>bb\nactivewindowv2>>aa\n")
                .unwrap();
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(3)).unwrap().as_deref(),
                Some("p / One")
            );
            clients_tx.send(b"[]".as_slice()).unwrap();
            stream.write_all(b"closewindow>>aa\n").unwrap();
            assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap(), None);
            watcher.stop();
            drop(clients_tx);
            server.join().unwrap();
        }
    }
}
#[cfg(unix)]
pub use focus::FocusWatcher;
/// Off Unix the watcher is a stand-in whose `start` fails, so `Host` can still name the type.
#[cfg(not(unix))]
pub use crate::platform::FocusWatcher;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    pub(super) struct Temp(pub PathBuf);
    impl Temp {
        pub(super) fn new() -> Self {
            static N: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "t3-rune-integration-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    #[test]
    fn calm_follows_omarchys_no_animations_flag_in_either_state_root() {
        let home = Temp::new();
        let xdg = Temp::new();
        assert!(!animations_off_with(
            Some(xdg.0.as_os_str()),
            Some(home.0.as_os_str())
        ));
        let flag = |root: &Path| {
            let dir = root.join("omarchy/toggles/hypr");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("no-animations.lua"), "").unwrap();
        };
        flag(&home.0.join(".local/state"));
        assert!(animations_off_with(
            Some(xdg.0.as_os_str()),
            Some(home.0.as_os_str())
        ));
        // An empty XDG_STATE_HOME counts as unset, as in `env.XDG_STATE_HOME ? ...`.
        assert!(animations_off_with(Some("".as_ref()), Some(home.0.as_os_str())));
        std::fs::remove_dir_all(home.0.join(".local/state/omarchy")).unwrap();
        assert!(!animations_off_with(
            Some(xdg.0.as_os_str()),
            Some(home.0.as_os_str())
        ));
        flag(&xdg.0);
        assert!(animations_off_with(
            Some(xdg.0.as_os_str()),
            Some(home.0.as_os_str())
        ));
        assert!(animations_off_with(Some(xdg.0.as_os_str()), None));
        assert!(!animations_off_with(Some("".as_ref()), Some("".as_ref())));
    }

    #[test]
    fn identity_helpers_match_javascript_string_semantics() {
        // `trim` and `\s` trim U+FEFF but not U+0085.
        assert_eq!(js_trim("\u{feff} e \u{feff}"), "e");
        assert_eq!(js_trim("\u{85}e\u{85}"), "\u{85}e\u{85}");
        assert!(environment_id_valid("e_1-2"));
        for bad in ["", ".", "a b", "é", "中"] {
            assert!(!environment_id_valid(bad), "{bad:?}");
        }
        assert!(!environment_id_valid(&"a".repeat(129)));
        assert!(environment_id_valid(&"a".repeat(128)));
        for value in [
            serde_json::json!({"environmentId":"e","threadId":"\u{feff}"}),
            serde_json::json!({"environmentId":"e","threadId":" \u{feff} "}),
        ] {
            assert!(thread_ref(&value).is_none());
        }
        assert!(thread_ref(&serde_json::json!({"environmentId":"e","threadId":"\u{85}"})).is_some());
        // JSON.stringify of the two strings, printed by Node.
        for (env, id, want) in [
            ("e", "t", r#"["e","t"]"#),
            ("a\"b", "c\\d\u{1f}", r#"["a\"b","c\\d\u001f"]"#),
            ("日", "🍞", "[\"日\",\"🍞\"]"),
            ("", "\u{0}\u{7f}\u{2028}\u{feff}", "[\"\",\"\\u0000\u{7f}\u{2028}\u{feff}\"]"),
        ] {
            assert_eq!(thread_key(env, id), want);
        }
        // Theme colors: `hex` trims like JavaScript before the six hex digits.
        assert_eq!(hex(Some(" \u{feff}#123456 ")), Some(0x123456));
        assert_eq!(hex(Some("#AbCdEf")), Some(0xabcdef));
        assert_eq!(hex(Some("#12345")), None);
        assert_eq!(hex(None), None);
    }

    #[test]
    fn title_matching_compares_updated_as_utf16_code_units() {
        // U+FFFF sorts above an astral character in UTF-16 and below it in UTF-8.
        let agent = |id: &str, updated: &str| Agent {
            id: id.into(),
            thread_id: id.into(),
            title: "Fix".into(),
            project: "p".into(),
            updated: Some(updated.into()),
            ..Agent::default()
        };
        let agents = vec![agent("astral", "𐀀"), agent("bmp", "\u{ffff}")];
        assert_eq!(thread_for_title("Fix", &agents).as_deref(), Some("bmp"));
    }

    #[test]
    fn usage_window_length_follows_javascript_regex_rules() {
        assert_eq!(crate::overview::window_length("5-hour"), Some(18_000_000.));
        assert_eq!(crate::overview::window_length("5 \u{feff}- hour"), Some(18_000_000.));
        // U+0085 is not JavaScript whitespace: the number no longer reaches the unit.
        assert_eq!(crate::overview::window_length("5\u{85}hour"), None);
        assert_eq!(crate::overview::window_length("12x3hour"), Some(10_800_000.));
        assert_eq!(crate::overview::window_length("biweekly"), Some(604_800_000.));
        assert_eq!(crate::overview::window_length("Monthly"), Some(2_592_000_000.));
    }

    #[test]
    fn theme_matches_exact_baseline_palettes() {
        assert_eq!(derive_palette(""), Palette::fallback());
        let tokyo = "background=\"#1a1b26\"\nforeground=\"#c0caf5\"\naccent=\"#7aa2f7\"\nselection=\"#33467c\"\nred=\"#f7768e\"\nyellow=\"#e0af68\"\ngreen=\"#9ece6a\"\nblue=\"#7aa2f7\"\nmagenta=\"#bb9af7\"";
        assert_eq!(derive_palette(tokyo), Palette::tokyo());
        // Exact numbers printed by oracle/src/theme.ts derivePalette(THEMES.light).
        let light = derive_palette(
            "mode=\"light\"\nbackground=\"#fafafa\"\nforeground=\"#222222\"\naccent=\"#3366cc\"\nred=\"#cc3333\"\nyellow=\"#b58900\"\ngreen=\"#2e7d32\"\nblue=\"#1565c0\"\nmagenta=\"#8e24aa\"",
        );
        assert_eq!(
            light,
            Palette {
                dark: false,
                bg: 16448250,
                fg: 2236962,
                accent: 3368652,
                hot: 13382451,
                warn: 9136384,
                sel: 14474460,
                panel: 15856113,
                text2: 6381921,
                dim: 7237230,
                track: 14474460,
                faint: 12763842,
                rule: 13619151,
                bright: 1118481,
                busy: 14344947,
                floor: 0.4,
                providers: [8200342, 2649644, 1202601, 995189, 9605778]
            }
        );
        let solitude = derive_palette(
            "mode=\"dark\"\naccent=\"#798186\"\nselection=\"#343d41\"\nbackground=\"#101315\"\nforeground=\"#cacccc\"\nred=\"#565d60\"\nyellow=\"#d9dbdc\"\nbright_red=\"#de6145\"\nbright_yellow=\"#c9c2b4\"",
        );
        assert_eq!(
            solitude,
            Palette {
                dark: true,
                bg: 1053461,
                fg: 13290700,
                accent: 8094599,
                hot: 14573893,
                warn: 13932425,
                sel: 3423553,
                panel: 1514012,
                text2: 10725030,
                dim: 8356482,
                track: 2764079,
                faint: 4211525,
                rule: 3487802,
                bright: 15066854,
                busy: 2172199,
                floor: 0.48,
                providers: [9132182, 2071426, 5018860, 2648036, 6909037]
            }
        );
        let temp = Temp::new();
        fs::write(temp.0.join("theme"), tokyo).unwrap();
        assert_eq!(palette(&temp.0.join("theme")).unwrap(), Palette::tokyo());
        assert!(palette(&temp.0.join("missing")).is_err());
    }
    #[test]
    fn usage_reads_only_explicit_bounded_json_files_and_rederives_expiry() {
        let temp = Temp::new();
        fs::write(temp.0.join("a.json"),r#"{"id":"a","todayTotalTokens":8,"limits":[{"label":"5-hour","percent":"0.5","resetsAt":"2026-11-12T12:00:00Z"}]}"#).unwrap();
        fs::write(
            temp.0.join("b.json"),
            r#"{"id":"b","todayTotalTokens":20,"todayTokensByModel":{"gpt":20}}"#,
        )
        .unwrap();
        fs::write(temp.0.join("bad.json"), "{").unwrap();
        fs::write(
            temp.0.join("ignore.txt"),
            r#"{"id":"wrong","todayTotalTokens":500}"#,
        )
        .unwrap();
        fs::write(
            temp.0.join("huge.json"),
            vec![b' '; (MAX_FILE + 1) as usize],
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(temp.0.join("b.json"), temp.0.join("link.json")).unwrap();
        let now = crate::model::ms(Some("2026-11-12T11:00:00Z")).unwrap();
        let p = usage(&temp.0, now);
        assert_eq!(
            p.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(p[1].limits[0].percent, 0.5);
        assert_eq!(p[1].limits[0].elapsed, Some(0.8));
        assert_eq!(usage(&temp.0, now + 3_600_000.)[1].limits[0].percent, 0.);
        assert!(usage(&temp.0.join("missing"), now).is_empty());
    }
    #[test]
    fn remote_rows_open_and_follow_their_actual_thread() {
        let remote = Agent {
            id: "node".into(),
            thread_id: "local".into(),
            title: "Remote".into(),
            remote: Some(crate::model::AgentRemote::from_ref("machine-b", "parent")),
            ..Agent::default()
        };
        assert_eq!(
            thread_for_title("Remote", &[remote.clone()]).as_deref(),
            Some("node")
        );
        assert_eq!(
            agent_thread_link(&remote, Some("machine-a")).as_deref(),
            Some("t3code://threads/machine-b/parent")
        );
        let native = Agent {
            remote: None,
            ..remote.clone()
        };
        assert_eq!(thread_for_title("Remote", &[native.clone()]), None);
        assert_eq!(
            agent_thread_link(&native, Some("machine-a")).as_deref(),
            Some("t3code://threads/machine-a/local")
        );
        for value in [
            serde_json::json!({"environmentId":"../bad","threadId":"x"}),
            serde_json::json!({"environmentId":"a"}),
            serde_json::json!({"environmentId":"a","threadId":" "}),
            serde_json::json!({"environmentId":"a","threadId":"x\n"}),
        ] {
            assert!(thread_ref(&value).is_none());
        }
    }
    #[test]
    fn title_matching_links_and_environment_metadata_match_baseline() {
        let agent = |id: &str, title: &str, project: &str, updated: &str| Agent {
            id: id.into(),
            thread_id: id.into(),
            title: title.into(),
            project: project.into(),
            updated: Some(updated.into()),
            ..Agent::default()
        };
        let mut agents = vec![
            agent("a", "Fix the build", "dotfiles", "2026-11-12"),
            agent("b", "Fix the build", "other", "2026-11-13"),
            agent("c", "Notes / ideas", "dotfiles", "2026-11-12"),
            agent("sub", "Explore", "dotfiles", "2026-11-12"),
        ];
        agents[3].thread_id = "a".into();
        assert_eq!(
            thread_for_title("dotfiles / Fix the build", &agents).as_deref(),
            Some("a")
        );
        assert_eq!(
            thread_for_title("Fix the build", &agents).as_deref(),
            Some("b")
        );
        assert_eq!(
            thread_for_title("dotfiles / Notes / ideas", &agents).as_deref(),
            Some("c")
        );
        assert_eq!(thread_for_title("dotfiles / Explore", &agents), None);
        assert_eq!(
            thread_link("e-1", "thread:task:a%3Ab/c").as_deref(),
            Some("t3code://threads/e-1/thread%3Atask%3Aa%253Ab%2Fc")
        );
        assert_eq!(
            thread_link("日本", "🍞/!").as_deref(),
            Some("t3code://threads/%E6%97%A5%E6%9C%AC/%F0%9F%8D%9E%2F!")
        );
        assert_eq!(thread_link("", "x"), None);
        assert_eq!(thread_link("e", &"🍞".repeat(513)), None);
        let temp = Temp::new();
        let db = temp.0.join("synthetic.sqlite");
        assert_eq!(environment_id(&db), None);
        fs::write(temp.0.join("environment-id"), "e_123-abc\n").unwrap();
        assert_eq!(environment_id(&db).as_deref(), Some("e_123-abc"));
        fs::write(temp.0.join("environment-id"), "../escape").unwrap();
        assert_eq!(environment_id(&db), None);
        assert_eq!(
            hypr_dir_for(Some(std::ffi::OsStr::new("/tmp/run")), Some("abc_1")),
            Some(PathBuf::from("/tmp/run/hypr/abc_1"))
        );
        assert_eq!(
            hypr_dir_for(Some(std::ffi::OsStr::new("/tmp/run")), Some("../escape")),
            None
        );
        assert!(open_link("https://example.invalid").is_err()); // Reject before spawning anything.
    }
}
