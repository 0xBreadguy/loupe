//! Antigravity conversation metadata. The wire reader skips unrelated fields.
use super::{Counts, Entry, History, directories, files, modified, roots};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    time::Duration,
};

const PROVIDER: &str = "antigravity";

pub(super) fn read(
    state: &Path,
    home: Option<&Path>,
    from: f64,
    until: f64,
    history: &mut History,
    keep_reading: &dyn Fn() -> bool,
) {
    let mut paths: Vec<PathBuf> = directories(&state.join("providers/antigravity"))
        .map(|profile| profile.join("antigravity-acp/conversations"))
        .collect();
    if let Some(home) = home {
        let defaults = [
            "antigravity",
            "antigravity-cli",
            "antigravity-ide",
            "antigravity-backup",
        ]
        .map(|name| home.join(".gemini").join(name));
        paths.extend(
            roots(
                "ANTIGRAVITY_DATA_DIR",
                home,
                defaults
                    .into_iter()
                    .chain([home.join(".config/antigravity")])
                    .collect(),
            )
            .into_iter()
            .map(|root| {
                if root.join("conversations").is_dir() {
                    root.join("conversations")
                } else {
                    root
                }
            }),
        );
    }
    paths = paths
        .into_iter()
        .filter_map(|dir| dir.canonicalize().ok())
        .collect();
    paths.sort();
    paths.dedup();
    let mut i = 0;
    let mut pending = Vec::new();
    let mut names = BTreeMap::<u64, Option<String>>::new();
    let mut visited = HashSet::new();
    while i < paths.len() {
        if !keep_reading() {
            return;
        }
        let dir = paths[i].clone();
        i += 1;
        let Ok(dir) = dir.canonicalize() else {
            continue;
        };
        if !visited.insert(dir.clone()) {
            continue;
        }
        paths.extend(directories(&dir));
        let mut databases = files(&dir, |path| {
            let stem = path.file_stem()?.to_str()?;
            (path.extension()?.to_str()? == "db" && stem != "conversation_summaries").then_some(())
        });
        databases.sort();
        for (path, ()) in databases {
            if !keep_reading() {
                return;
            }
            let Some(mtime) = modified(&path) else {
                continue;
            };
            let mut wal = path.as_os_str().to_os_string();
            wal.push("-wal");
            if mtime.max(modified(Path::new(&wal)).unwrap_or(0.)) < from - 86_400_000. {
                continue;
            }
            match metadata(&path, mtime, keep_reading) {
                Ok(records) => {
                    for item in &records {
                        if item.model_id > 0 && item.model_quality > 1 {
                            names
                                .entry(item.model_id)
                                .and_modify(|name| {
                                    if name.as_deref() != Some(item.entry.model.as_str()) {
                                        *name = None;
                                    }
                                })
                                .or_insert_with(|| Some(item.entry.model.clone()));
                        }
                    }
                    pending.extend(records);
                }
                Err(_) => history.partial = true,
            }
        }
    }
    let mut unique = Identities::default();
    for mut item in pending {
        if !keep_reading() {
            return;
        }
        if item.model_quality < 2 {
            if let Some(name) = names
                .get(&item.model_id)
                .and_then(Option::as_deref)
                .or_else(|| fallback_model(item.model_id))
            {
                item.entry.model = name.to_owned();
                item.model_quality = 2;
            }
            if item.entry.model.is_empty() {
                item.entry.model = if item.model_id == 0 {
                    "Antigravity".into()
                } else {
                    format!("antigravity-model-{}", item.model_id)
                };
            }
        }
        unique.push(item);
    }
    history.entries.extend(
        unique
            .entries
            .into_iter()
            .flatten()
            .map(|item| item.entry)
            .filter(|entry| (from..until).contains(&entry.at)),
    );
}

struct Candidate {
    entry: Entry,
    model_id: u64,
    model_quality: u8,
    time_quality: u8,
    keys: Vec<(u32, String)>,
}

#[derive(Default)]
struct Identities {
    by_key: HashMap<(u32, String), usize>,
    entries: Vec<Option<Candidate>>,
    parent: Vec<usize>,
}

impl Identities {
    fn root(&mut self, mut id: usize) -> usize {
        while self.parent[id] != id {
            self.parent[id] = self.parent[self.parent[id]];
            id = self.parent[id];
        }
        id
    }

    fn push(&mut self, mut item: Candidate) {
        let id = self.entries.len();
        self.parent.push(id);
        for key in std::mem::take(&mut item.keys) {
            if let Some(previous) = self.by_key.insert(key, id) {
                // Multiple aliases can point to the same entry.
                if previous == id {
                    continue;
                }
                let prior = self.root(previous);
                if prior == id {
                    continue;
                }
                self.parent[prior] = id;
                let other = self.entries[prior].take().unwrap();
                item.entry.counts.merge(other.entry.counts);
                if other.model_quality >= item.model_quality {
                    item.entry.model = other.entry.model;
                    item.model_quality = other.model_quality;
                }
                if other.time_quality > item.time_quality
                    || (other.time_quality == item.time_quality && other.entry.at < item.entry.at)
                {
                    item.entry.at = other.entry.at;
                    item.time_quality = other.time_quality;
                }
            }
        }
        self.entries.push(Some(item));
    }
}

fn metadata(
    path: &Path,
    mtime: f64,
    keep_reading: &dyn Fn() -> bool,
) -> rusqlite::Result<Vec<Candidate>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(100))?;
    let session = path
        .file_stem()
        .and_then(|p| p.to_str())
        .unwrap_or_default();
    read_metadata(&db, session, mtime, keep_reading)
}

fn read_metadata(
    db: &Connection,
    session: &str,
    mtime: f64,
    keep_reading: &dyn Fn() -> bool,
) -> rusqlite::Result<Vec<Candidate>> {
    let tx = db.unchecked_transaction()?;
    let exists = |name| {
        tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
            [name],
            |r| r.get::<_, bool>(0),
        )
    };
    let trajectory = if exists("trajectory_metadata_blob")? {
        tx.query_row(
            "SELECT data FROM trajectory_metadata_blob LIMIT 1",
            [],
            |row| {
                Ok(wire(row.get_ref(0)?.as_blob()?)
                    .and_then(|root| root.nested(2).and_then(timestamp)))
            },
        )
        .optional()?
        .flatten()
    } else {
        None
    };
    let mut output = Vec::new();
    let mut generation_models = HashMap::new();
    for (table, blob, step) in [("gen_metadata", "data", false), ("steps", "metadata", true)] {
        if !exists(table)? {
            continue;
        }
        let mut stmt = tx.prepare(&format!(
            "SELECT idx, {blob} FROM {table} WHERE {blob} IS NOT NULL ORDER BY idx"
        ))?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            if !keep_reading() {
                return Ok(Vec::new());
            }
            let idx: i64 = row.get(0)?;
            let bytes = row.get_ref(1)?.as_blob()?;
            let Some(root) = wire(bytes) else { continue };
            let data = if step {
                root
            } else if let Some(data) = root.nested(1) {
                data
            } else {
                continue;
            };
            let model = if step {
                data.nested(24).unwrap_or_default()
            } else {
                data.clone()
            };
            let primary = data.nested(if step { 9 } else { 4 });
            let model_id = model
                .number(if step { 1 } else { 3 })
                .unwrap_or_else(|| primary.as_ref().map_or(0, |usage| usage.num(1)));
            let names = if step { [12, 8] } else { [19, 21] };
            let name = names
                .into_iter()
                .filter_map(|field| model.text(field))
                .map(normalize)
                .filter(|s| !s.is_empty() && !s.starts_with("model-") && !s.starts_with("model_"))
                .min_by_key(|s| !concrete(s))
                .unwrap_or_default();
            if !step {
                generation_models.insert(idx, (model_id, name.clone()));
            }
            let at = if step {
                data.nested(8)
                    .and_then(timestamp)
                    .or_else(|| data.nested(1).and_then(timestamp))
            } else {
                data.nested(9)
                    .and_then(|step| step.nested(4).and_then(timestamp))
            };
            let retries = data
                .messages(if step { 28 } else { 17 })
                .filter_map(|retry| retry.nested(2));
            for (usage_index, usage) in primary.into_iter().chain(retries).enumerate() {
                let counts = Counts {
                    fresh: usage.num(2) as f64,
                    cached: usage.num(5) as f64,
                    written: usage.num(4) as f64,
                    output: usage.num(3).max(usage.num(9).saturating_add(usage.num(10))) as f64,
                };
                if counts.total() == 0. {
                    continue;
                }
                let id = usage.number(1).unwrap_or(model_id);
                let label = if id == model_id && !name.is_empty() {
                    name.as_str()
                } else {
                    generation_models
                        .get(&idx)
                        .filter(|(m, _)| *m == id)
                        .map(|(_, s)| s.as_str())
                        .unwrap_or_default()
                };
                let mut keys: Vec<_> = [11, 12, 7]
                    .into_iter()
                    .filter_map(|field| {
                        usage
                            .text(field)
                            .filter(|id| !id.is_empty())
                            .map(|id| (field, id.to_owned()))
                    })
                    .collect();
                if keys.is_empty() {
                    keys.push((0, format!("{session}:{table}:{idx}:{usage_index}")));
                }
                output.push(Candidate {
                    entry: Entry {
                        provider: PROVIDER.into(),
                        model: label.into(),
                        at: at.or(trajectory).unwrap_or(mtime),
                        counts,
                    },
                    model_id: id,
                    model_quality: if concrete(label) {
                        3
                    } else {
                        u8::from(!label.is_empty())
                    },
                    time_quality: if at.is_some() {
                        2
                    } else {
                        u8::from(trajectory.is_some())
                    },
                    keys,
                });
            }
        }
    }
    Ok(output)
}

#[derive(Clone, Copy)]
enum Value<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}
#[derive(Clone, Default)]
struct Fields<'a>(Vec<(u32, Value<'a>)>);

impl<'a> Fields<'a> {
    fn number(&self, key: u32) -> Option<u64> {
        self.0.iter().find_map(|(tag, value)| match value {
            Value::Number(n) if *tag == key => Some(*n),
            _ => None,
        })
    }
    fn num(&self, key: u32) -> u64 {
        self.number(key).unwrap_or(0)
    }
    fn bytes(&self, key: u32) -> impl Iterator<Item = &'a [u8]> + '_ {
        self.0.iter().filter_map(move |(tag, value)| match value {
            Value::Bytes(bytes) if *tag == key => Some(*bytes),
            _ => None,
        })
    }
    fn text(&self, key: u32) -> Option<&'a str> {
        std::str::from_utf8(self.bytes(key).next()?)
            .ok()
            .map(str::trim)
    }
    fn nested(&self, key: u32) -> Option<Self> {
        wire(self.bytes(key).next()?)
    }
    fn messages(&self, key: u32) -> impl Iterator<Item = Self> + '_ {
        self.bytes(key).filter_map(wire)
    }
}

fn wire(mut bytes: &[u8]) -> Option<Fields<'_>> {
    let mut fields = Fields::default();
    while !bytes.is_empty() {
        let tag = varint(&mut bytes)?;
        let field = u32::try_from(tag >> 3).ok().filter(|n| *n > 0)?;
        let value = match tag & 7 {
            0 => Value::Number(varint(&mut bytes)?),
            2 => {
                let size = usize::try_from(varint(&mut bytes)?).ok()?;
                let (value, following) = bytes.split_at_checked(size)?;
                bytes = following;
                Value::Bytes(value)
            }
            kind @ (1 | 5) => {
                bytes = bytes.get(if kind == 1 { 8 } else { 4 }..)?;
                continue;
            }
            _ => return None,
        };
        fields.0.push((field, value));
    }
    Some(fields)
}

fn varint(bytes: &mut &[u8]) -> Option<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = bytes.split_first()?;
        *bytes = rest;
        if shift == 63 && byte > 1 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
    }
    None
}

fn timestamp(fields: Fields<'_>) -> Option<f64> {
    let secs = fields.num(1);
    (0 < secs && secs < 32_503_680_000)
        .then(|| secs as f64 * 1000. + (fields.num(2) / 1_000_000) as f64)
}

fn normalize(name: &str) -> String {
    crate::terminal::clean(name)
        .to_lowercase()
        .trim_end_matches(')')
        .replace(" (", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn concrete(model: &str) -> bool {
    model.starts_with("gpt-")
        || model.starts_with("claude-")
        || model
            .strip_prefix("gemini-")
            .is_some_and(|m| m.starts_with(|c: char| c.is_ascii_digit()))
}

fn fallback_model(id: u64) -> Option<&'static str> {
    // IDs are from T3's Antigravity metadata reader. Named records are preferred.
    Some(match id {
        246 => "gemini-2.5-pro",
        312 => "gemini-2.5-flash",
        1026 => "claude-opus-4-6",
        1035 => "claude-sonnet-4-6",
        1016 | 1036 | 1037 => "gemini-3.1-pro",
        1402 => "claude-opus-5-5",
        1417..=1419 => "gemini-4-argon",
        1427 | 1437 => "gemini-4-carbon",
        1525..=1527 => "gemini-4-hydrogen",
        1532 | 1533 => "gemini-4-neon",
        1537 => "gemini-pro-orchestrator-neon",
        1667 => "gemini-pro-orchestrator",
        1669 => "gemini-4-hydrogen-orchestrator",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    #[test]
    fn generation_and_step_copies_with_shared_receipts_count_once() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE gen_metadata(idx INTEGER PRIMARY KEY, data BLOB);
             CREATE TABLE steps(idx INTEGER PRIMARY KEY, metadata BLOB);",
        )
        .unwrap();
        let tagged = |tag: u8, data: &[u8]| {
            let mut encoded = vec![tag * 8 + 2, data.len() as u8];
            encoded.extend(data);
            encoded
        };
        let mut usage = vec![16, 20, 24, 8, 32, 6, 40, 100];
        usage.extend(tagged(11, b"shared-receipt"));
        usage.extend(tagged(12, b"second-alias"));
        let mut generation = tagged(4, &usage);
        generation.extend([154, 1, 10]);
        generation.extend(b"gemini-9-x");
        let step = tagged(9, &usage);
        db.execute(
            "INSERT INTO gen_metadata VALUES(3,?1)",
            params![tagged(1, &generation)],
        )
        .unwrap();
        db.execute("INSERT INTO steps VALUES(3,?1)", params![step])
            .unwrap();
        let mut unique = Identities::default();
        assert!(
            read_metadata(&db, "session", 10., &|| false)
                .unwrap()
                .is_empty()
        );
        for candidate in read_metadata(&db, "session", 10., &|| true).unwrap() {
            unique.push(candidate);
        }
        let layer: Vec<_> = unique.entries.into_iter().flatten().collect();
        assert_eq!(layer.len(), 1);
        assert_eq!(layer[0].entry.model, "gemini-9-x");
        assert_eq!(layer[0].entry.counts.total(), 134.);
    }
}
