//! Local daily Usage totals, with completed turns as the fallback.
use super::{PREFIX, usage_history};
use crate::{
    overview::{ModelUsage, Provider},
    platform,
    recap::{iso, midnight},
    terminal::{self, ProviderFamily},
};
use rusqlite::{Connection, params};
use std::{collections::BTreeMap, path::Path, sync::Arc};

const REFRESH_MS: f64 = 5_000.;
const IDLE_REFRESH_MS: f64 = 60_000.;

#[derive(Debug, Default)]
pub struct DailyUsage {
    pub providers: Vec<Provider>,
    pub source: &'static str,
}

#[derive(Default)]
pub(crate) struct Tracker {
    latest: Option<Report>,
    history: Option<(i64, f64, usage_history::History)>,
}

struct Report {
    day: i64,
    at: f64,
    version: Option<(i64, i64)>,
    usage: Arc<DailyUsage>,
}

impl Tracker {
    pub(crate) fn poll(
        &mut self,
        db: &Connection,
        path: &Path,
        now: f64,
        version: (i64, i64),
    ) -> Arc<DailyUsage> {
        let day = midnight(&platform::local_time, (now / 1000.).floor() as i64);
        let history_due = self
            .history
            .as_ref()
            .is_none_or(|(d, at, _)| *d != day || !(*at..*at + IDLE_REFRESH_MS).contains(&now));
        if let Some(report) = &self.latest {
            let refresh = if report.version == Some(version) {
                IDLE_REFRESH_MS
            } else {
                REFRESH_MS
            };
            if !history_due && report.day == day && (report.at..report.at + refresh).contains(&now)
            {
                return Arc::clone(&report.usage);
            }
        }
        let from = day as f64 * 1000.;
        let until = now.floor() + 1.;
        if history_due {
            self.history = Some((day, now, usage_history::read(path, from, until)));
        }
        let history = &self.history.as_ref().unwrap().2;
        let usage = Arc::new(combine(db, from, until, history));
        self.latest = Some(Report {
            day,
            at: now,
            version: Some(version),
            usage: Arc::clone(&usage),
        });
        usage
    }
}

fn combine(db: &Connection, from: f64, until: f64, history: &usage_history::History) -> DailyUsage {
    let mut groups = BTreeMap::<String, BTreeMap<String, f64>>::new();
    for entry in &history.entries {
        *groups
            .entry(entry.provider.clone())
            .or_default()
            .entry(entry.model.clone())
            .or_default() += entry.counts.total();
    }
    // A provider turn can be attributed to a different instance or a
    // different model label than its Usage record. Adding the two datasets
    // would count the same calls twice.
    if groups.is_empty() {
        return DailyUsage {
            providers: providers(group_turns(db, from, until).unwrap_or_default()),
            source: "T3 Code turn totals",
        };
    }
    DailyUsage {
        providers: providers(groups),
        source: history.source(),
    }
}

fn group_turns(
    db: &Connection,
    from: f64,
    until: f64,
) -> rusqlite::Result<BTreeMap<String, BTreeMap<String, f64>>> {
    let mut groups = BTreeMap::<String, BTreeMap<String, f64>>::new();
    reported_turns(db, from, until, |turn| {
        let family = match terminal::family(
            turn.model.as_deref().unwrap_or_default(),
            turn.provider.as_deref(),
        ) {
            ProviderFamily::Claude => "claude",
            ProviderFamily::OpenAi => "codex",
            ProviderFamily::Grok => "grok",
            ProviderFamily::Gemini => "gemini",
            ProviderFamily::Other => "other",
        };
        let label = turn
            .model
            .or(turn.provider)
            .unwrap_or_else(|| "Unattributed".into());
        *groups
            .entry(family.into())
            .or_default()
            .entry(label)
            .or_default() += turn.input + turn.output;
    })?;
    Ok(groups)
}

fn providers(groups: BTreeMap<String, BTreeMap<String, f64>>) -> Vec<Provider> {
    let mut providers: Vec<_> = groups
        .into_iter()
        .map(|(id, models)| Provider {
            name: id.clone(),
            id,
            today: models.values().sum(),
            models: models
                .into_iter()
                .map(|(model, tokens)| ModelUsage { model, tokens })
                .collect(),
            ..Provider::default()
        })
        .collect();
    providers.sort_by(|a, b| b.today.total_cmp(&a.today));
    providers
}

#[cfg(test)]
fn read(db: &Connection, from: f64, until: f64) -> rusqlite::Result<Vec<Provider>> {
    Ok(providers(group_turns(db, from, until.floor() + 1.)?))
}

pub(crate) struct ReportedTurn {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub input: f64,
    pub output: f64,
    pub cached: f64,
    pub thread: String,
    pub provider_thread: String,
    pub run: Option<String>,
    pub completed: f64,
}

pub(crate) fn reported_turns(
    db: &Connection,
    from: f64,
    until: f64,
    mut visit: impl FnMut(ReportedTurn),
) -> rusqlite::Result<()> {
    // Prefer the turn's reported model. The owning run can supply its selection when
    // it belongs to the same provider thread, including after the user switches models.
    let mut query = db.prepare_cached(&format!(
        "SELECT json_extract(t.payload_json,'$.model'),
                CASE WHEN json_valid(r.payload_json) THEN json_extract(r.payload_json,'$.modelSelection.model') END,
                COALESCE(NULLIF(p.provider,''), r.provider),
                json_extract(t.payload_json,'$.turnTokenUsage.inputTokens'),
                json_extract(t.payload_json,'$.turnTokenUsage.outputTokens'),
                json_extract(t.payload_json,'$.turnTokenUsage.cachedInputTokens'),
                t.thread_id, t.provider_thread_id, r.run_id, t.completed_at
         FROM {PREFIX}provider_turns t
         LEFT JOIN {PREFIX}provider_threads p ON p.provider_thread_id = t.provider_thread_id
         LEFT JOIN {PREFIX}nodes n ON n.node_id = t.node_id AND n.thread_id = t.thread_id
         LEFT JOIN {PREFIX}runs r ON r.run_id = n.run_id AND r.thread_id = t.thread_id
                                  AND r.provider_thread_id = t.provider_thread_id
         WHERE t.completed_at >= ?1 AND t.completed_at < ?2
           AND CASE WHEN json_valid(t.payload_json) THEN
             COALESCE(json_extract(t.payload_json,'$.turnTokenUsage.usageStatus'),'') <> 'unavailable'
             AND (json_type(t.payload_json,'$.turnTokenUsage.inputTokens') IN ('integer', 'real')
               OR json_type(t.payload_json,'$.turnTokenUsage.outputTokens') IN ('integer', 'real'))
           ELSE 0 END"
    ))?;
    let mut rows = query.query(params![iso(from), iso(until)])?;
    while let Some(row) = rows.next()? {
        let count = |column| {
            row.get::<_, f64>(column)
                .ok()
                .filter(|n| n.is_finite() && *n >= 0.)
        };
        let (input, output) = (count(3), count(4));
        if input.is_none() && output.is_none() {
            continue;
        }
        let Some(completed) = crate::recap::ms(&row.get::<_, String>(9)?) else {
            continue;
        };
        let label = |column| row.get::<_, String>(column).ok().as_deref().and_then(name);
        let input = input.unwrap_or_default();
        visit(ReportedTurn {
            model: label(0).or_else(|| label(1)),
            provider: label(2),
            input,
            output: output.unwrap_or_default(),
            cached: count(5).unwrap_or_default().min(input),
            thread: row.get(6)?,
            provider_thread: row.get(7)?,
            run: row.get(8)?,
            completed,
        });
    }
    Ok(())
}

pub(crate) fn name(value: &str) -> Option<String> {
    let cleaned = terminal::clean(value);
    let name = cleaned.trim();
    (!name.is_empty() && name != "--" && !name.eq_ignore_ascii_case("unknown"))
        .then(|| name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recap::ms;
    use serde_json::{Value, json};

    #[test]
    fn uses_reported_turn_totals_in_the_day_once_per_turn() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "CREATE TABLE {PREFIX}provider_threads (provider_thread_id TEXT PRIMARY KEY, provider TEXT);
             CREATE TABLE {PREFIX}provider_turns (provider_turn_id TEXT PRIMARY KEY,
                 provider_thread_id TEXT, node_id TEXT, thread_id TEXT, completed_at TEXT, payload_json TEXT);
             CREATE TABLE {PREFIX}nodes (node_id TEXT PRIMARY KEY, thread_id TEXT, run_id TEXT);
             CREATE TABLE {PREFIX}runs (run_id TEXT PRIMARY KEY, thread_id TEXT,
                 provider_thread_id TEXT, provider TEXT, payload_json TEXT);
             INSERT INTO {PREFIX}provider_threads VALUES ('c','codex'), ('g','antigravity');
             INSERT INTO {PREFIX}runs VALUES
                ('g-run','thread','g','antigravity','{{\"modelSelection\":{{\"model\":\"new-provider/new-model-v7\"}}}}'),
                ('c-run','thread','c','codex','{{\"modelSelection\":{{\"model\":\"a-different-model\"}}}}');
             INSERT INTO {PREFIX}nodes VALUES ('g','thread','g-run'), ('a','thread','c-run'),
                ('provider-only','thread','c-run');"
        )).unwrap();
        let insert = |id: &str, provider: &str, stamp: Option<&str>, payload: Value| {
            db.execute(
                &format!(
                    "INSERT OR REPLACE INTO {PREFIX}provider_turns VALUES (?1,?2,?1,'thread',?3,?4)"
                ),
                params![id, provider, stamp, payload.to_string()],
            )
            .unwrap();
        };
        let start = "2026-11-28T00:00:00.000Z";
        let now = "2026-11-28T15:00:00.000Z";
        let turn = |input, output| {
            json!({"model":"gpt-6.2-sol", "turnTokenUsage": {
            "usageStatus":"complete", "inputTokens":input, "outputTokens":output,
            "cachedInputTokens":500, "reasoningTokens":100, "hasSubagents":true }})
        };
        insert("a", "c", Some(start), turn(600, 100));
        insert("b", "c", Some(now), turn(200, 40));
        insert("b", "c", Some(now), turn(210, 40));
        insert(
            "g",
            "g",
            Some(now),
            json!({"turnTokenUsage":{"outputTokens":55}}),
        );
        insert(
            "provider-only",
            "g",
            Some(now),
            json!({"model":"unknown", "turnTokenUsage":{"outputTokens":10}}),
        );
        insert(
            "yesterday",
            "c",
            Some("2026-11-27T23:59:59.999Z"),
            turn(9999, 0),
        );
        insert(
            "future",
            "c",
            Some("2026-11-28T15:00:00.001Z"),
            turn(9999, 0),
        );
        insert("running", "c", None, turn(9999, 0));
        insert(
            "call",
            "c",
            Some(now),
            json!({"model":"gpt-6.2-sol", "tokenUsage":{"usedTokens":99_000,"inputTokens":9000}}),
        );
        insert(
            "missing",
            "c",
            Some(now),
            json!({"turnTokenUsage":{"usageStatus":"unavailable","inputTokens":9000}}),
        );

        let usage = read(&db, ms(start).unwrap(), ms(now).unwrap()).unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!((usage[0].id.as_str(), usage[0].today), ("codex", 950.));
        assert_eq!(usage[0].models[0].model, "gpt-6.2-sol");
        assert_eq!(usage[0].models[0].tokens, 950.);
        assert_eq!((usage[1].id.as_str(), usage[1].today), ("gemini", 65.));
        assert_eq!(
            (usage[1].models[0].model.as_str(), usage[1].models[0].tokens),
            ("antigravity", 10.)
        );
        assert_eq!(
            (usage[1].models[1].model.as_str(), usage[1].models[1].tokens),
            ("new-provider/new-model-v7", 55.)
        );
        assert!(usage.iter().all(|provider| provider.limits.is_empty()));
    }
}
