//! Local usage metadata also used by T3's Usage screen.
//! Only counts, timestamps, model names and deduplication identifiers are decoded.
mod antigravity;
mod opencode;

use crate::platform;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::{self, File},
    io::BufReader,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Counts {
    pub fresh: f64,
    pub cached: f64,
    pub written: f64,
    pub output: f64,
}

impl Counts {
    pub fn total(self) -> f64 {
        self.fresh + self.cached + self.written + self.output
    }

    fn merge(&mut self, other: Self) {
        self.fresh = self.fresh.max(other.fresh);
        self.cached = self.cached.max(other.cached);
        self.written = self.written.max(other.written);
        self.output = self.output.max(other.output);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub provider: String,
    pub model: String,
    pub at: f64,
    pub counts: Counts,
}

#[derive(Default)]
pub(crate) struct History {
    pub entries: Vec<Entry>,
    pub partial: bool,
}

impl History {
    pub(crate) fn source(&self) -> &'static str {
        if self.partial {
            "T3 Usage · partial history"
        } else {
            "T3 Usage · local history"
        }
    }
}

#[derive(Deserialize)]
struct ScanCache {
    version: u32,
    models: Vec<String>,
    files: BTreeMap<String, CacheFile>,
}

#[derive(Deserialize)]
struct CacheFile {
    p: String,
    #[serde(default)]
    r: Vec<CacheRow>,
    #[serde(default)]
    t: Vec<CacheRow>,
}

// Tuple positions from T3's normalised usage scan file. Unused costs,
// service tiers, native sessions and parser state are discarded by serde.
#[derive(Deserialize)]
#[serde(from = "Vec<CacheField>")]
struct CacheRow {
    at: Option<f64>,
    model_index: Option<usize>,
    counts: Counts,
    identity: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CacheField {
    Number(f64),
    Text(String),
    Other(serde::de::IgnoredAny),
}

impl From<Vec<CacheField>> for CacheRow {
    fn from(fields: Vec<CacheField>) -> Self {
        let num = |index| match fields.get(index) {
            Some(CacheField::Number(n)) if n.is_finite() && *n >= 0. => Some(*n),
            _ => None,
        };
        let value = |index| num(index).unwrap_or_default();
        Self {
            at: num(0),
            model_index: num(1).filter(|n| n.fract() == 0.).map(|n| n as usize),
            counts: Counts {
                fresh: value(3),
                cached: value(4),
                written: value(5),
                output: value(6),
            },
            identity: match fields.get(8) {
                Some(CacheField::Text(key)) => Some(key.clone()),
                _ => None,
            },
        }
    }
}

pub(crate) fn read(db: &Path, from: f64, until: f64) -> History {
    read_cancellable(db, from, until, &|| true).unwrap_or_default()
}

pub(crate) fn read_cancellable(
    db: &Path,
    from: f64,
    until: f64,
    keep_reading: &dyn Fn() -> bool,
) -> Option<History> {
    if !keep_reading() {
        return None;
    }
    let Some(state_dir) = db.parent() else {
        return Some(History::default());
    };
    let mut history = History::default();
    read_scan_cache(state_dir, from, until, &mut history, keep_reading);
    if !keep_reading() {
        return None;
    }
    let home = platform::home_dir().map(PathBuf::from).filter(|home| {
        // A supplied archive or isolated T3 home must not pick up the
        // machine's unrelated provider histories.
        home.join(".t3").join("userdata") == state_dir
    });
    antigravity::read(
        state_dir,
        home.as_deref(),
        from,
        until,
        &mut history,
        keep_reading,
    );
    if let Some(home) = home {
        opencode::read(&home, from, until, &mut history, keep_reading);
    }
    keep_reading().then_some(history)
}

fn read_scan_cache(
    dir: &Path,
    from: f64,
    until: f64,
    history: &mut History,
    keep_reading: &dyn Fn() -> bool,
) {
    let paths = files(dir, |path| {
        let name = path.file_name()?.to_str()?;
        if name == "usage-scan-cache.json" {
            return Some(0);
        }
        name.strip_prefix("usage-scan-cache-v")?
            .strip_suffix(".json")?
            .parse::<u32>()
            .ok()
    });
    let mut paths = paths;
    paths.sort_by_key(|(_, version)| std::cmp::Reverse(*version));
    let Some((path, _)) = paths.first() else {
        return;
    };
    let cache = File::open(path)
        .ok()
        .and_then(|file| serde_json::from_reader::<_, ScanCache>(BufReader::new(file)).ok());
    let Some(cache) = cache.filter(|cache| cache.version >= 4) else {
        history.partial = true;
        return;
    };
    let mut duplicate = HashMap::<(String, String), usize>::new();
    for file in cache.files.into_values() {
        if !keep_reading() {
            return;
        }
        let Some(provider) = super::daily_usage::name(&file.p) else {
            continue;
        };
        // These providers are scanned from their own stores by T3.
        if matches!(provider.as_str(), "antigravity" | "opencode") {
            continue;
        }
        for record in file.r.into_iter().chain(file.t) {
            let Some(at) = record.at.filter(|at| (from..until).contains(at)) else {
                continue;
            };
            let Some(model) = record
                .model_index
                .and_then(|i| cache.models.get(i))
                .and_then(|name| super::daily_usage::name(name))
            else {
                continue;
            };
            if record.counts.total() == 0. || (provider == "claude" && model == "<synthetic>") {
                continue;
            }
            let entry = Entry {
                provider: provider.clone(),
                model,
                at,
                counts: record.counts,
            };
            if let Some(identity) = record.identity.filter(|id| !id.is_empty()) {
                let index = duplicate
                    .entry((provider.clone(), identity))
                    .or_insert(history.entries.len());
                if let Some(previous) = history.entries.get_mut(*index) {
                    previous.counts.merge(entry.counts);
                    continue;
                }
            }
            history.entries.push(entry);
        }
    }
}

fn directories(path: &Path) -> impl Iterator<Item = PathBuf> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
}

fn files<T>(path: &Path, filter: impl Fn(&Path) -> Option<T>) -> Vec<(PathBuf, T)> {
    fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let path = entry.path();
            let value = filter(&path)?;
            Some((path, value))
        })
        .collect()
}

fn modified(path: &Path) -> Option<f64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|t| t.as_secs_f64() * 1000.)
}

fn roots(var: &str, home: &Path, defaults: Vec<PathBuf>) -> HashSet<PathBuf> {
    let configured = std::env::var(var)
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .filter_map(|part| {
                    let part = part.trim();
                    if let Some(relative) =
                        part.strip_prefix("~/").or_else(|| part.strip_prefix("~\\"))
                    {
                        return Some(home.join(relative));
                    }
                    let path = Path::new(part);
                    path.is_absolute().then(|| path.to_path_buf())
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let candidates = if configured.is_empty() {
        defaults
    } else {
        configured
    };
    candidates
        .into_iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .collect()
}
