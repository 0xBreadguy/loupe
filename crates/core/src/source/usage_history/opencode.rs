//! Usage counts from OpenCode's SQLite message metadata.
use super::{Counts, Entry, History, files, roots};
use rusqlite::{Connection, OpenFlags, params};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) fn read(
    home: &Path,
    from: f64,
    until: f64,
    history: &mut History,
    keep_reading: &dyn Fn() -> bool,
) {
    if !keep_reading() {
        return;
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    let mut dirs: Vec<_> = roots("OPENCODE_DATA_DIR", home, vec![data.join("opencode")])
        .into_iter()
        .collect();
    dirs.sort();
    let mut seen = HashSet::<String>::new();
    for dir in dirs {
        let mut paths = files(&dir, |path| {
            let name = path.file_name()?.to_str()?;
            let suffix = name.strip_prefix("opencode")?.strip_suffix(".db")?;
            (suffix.is_empty()
                || suffix.strip_prefix('-').is_some_and(|suffix| {
                    !suffix.is_empty()
                        && suffix
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                }))
            .then_some(!suffix.is_empty())
        });
        paths.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        for (path, _) in paths {
            if !keep_reading() {
                return;
            }
            match read_sqlite(&path, from, until, keep_reading) {
                Ok(records) => {
                    history
                        .entries
                        .extend(records.into_iter().filter_map(|(id, entry)| {
                            (id.is_empty() || seen.insert(id)).then_some(entry)
                        }))
                }
                Err(_) => history.partial = true,
            }
        }
    }
}

fn read_sqlite(
    path: &Path,
    from: f64,
    until: f64,
    keep_reading: &dyn Fn() -> bool,
) -> rusqlite::Result<Vec<(String, Entry)>> {
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(Duration::from_millis(100))?;
    let tx = db.unchecked_transaction()?;
    let mut result = Vec::new();
    for table in ["message", "session_message"] {
        let mut schema = tx.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = schema
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        if columns.is_empty() {
            continue;
        }
        let time = if columns.contains("time_created") {
            "time_created"
        } else {
            "NULL"
        };
        let id = if columns.contains("id") { "id" } else { "NULL" };
        let kind = if table == "session_message" && columns.contains("type") {
            "AND type IN ('assistant','compaction')"
        } else {
            ""
        };
        let mut stmt = tx.prepare(&format!(
            "SELECT {id}, COALESCE(json_extract(data,'$.time.created'),{time}),
             COALESCE(NULLIF(json_extract(data,'$.model.id'),''),NULLIF(json_extract(data,'$.model.modelID'),''),json_extract(data,'$.modelID')),
             json_extract(data,'$.tokens.input'), json_extract(data,'$.tokens.cache.read'),
             json_extract(data,'$.tokens.cache.write'), json_extract(data,'$.tokens.output'),
             json_extract(data,'$.tokens.reasoning')
             FROM {table}
             WHERE ({time} IS NULL OR ({time} >= ?1 AND {time} < ?2)) {kind}
              AND CASE WHEN typeof(data)='text' AND json_valid(data) THEN
                json_type(data,'$.role') IS NULL OR json_extract(data,'$.role')='assistant' END"
        ))?;
        let mut rows = stmt.query(params![from, until])?;
        while let Some(row) = rows.next()? {
            if !keep_reading() {
                return Ok(Vec::new());
            }
            let at = row
                .get::<_, f64>(1)
                .ok()
                .filter(|at| (from..until).contains(at));
            let model = row
                .get::<_, String>(2)
                .ok()
                .as_deref()
                .and_then(crate::source::daily_usage::name);
            let (Some(at), Some(model)) = (at, model) else {
                continue;
            };
            let count = |i| {
                row.get::<_, f64>(i)
                    .ok()
                    .filter(|n| n.is_finite() && *n >= 0.)
                    .unwrap_or_default()
            };
            let counts = Counts {
                fresh: count(3),
                cached: count(4),
                written: count(5),
                output: count(6) + count(7),
            };
            if counts.total() > 0. {
                result.push((
                    row.get(0).unwrap_or_default(),
                    Entry {
                        provider: "opencode".into(),
                        model,
                        at,
                        counts,
                    },
                ));
            }
        }
    }
    Ok(result)
}
