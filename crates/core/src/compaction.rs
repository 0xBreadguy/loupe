//! Completed context compactions from T3's retained thread records.
use crate::{recap, source::PREFIX, terminal};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub count: u32,
    pub measured: u32,
    pub estimated: u32,
    pub reduced: f64,
}

impl Summary {
    pub(crate) fn add(&mut self, event: &Event) {
        self.count += 1;
        if let Some((amount, estimated)) = event.reduction {
            self.reduced += amount;
            if estimated {
                self.estimated += 1;
            } else {
                self.measured += 1;
            }
        }
    }

    pub fn label(self) -> String {
        if self.count == 0 {
            return "0 recorded".into();
        }
        let sized = self.measured + self.estimated;
        if sized == 0 {
            return format!("{} recorded · size unavailable", self.count);
        }
        let approx = if self.estimated != 0 { "~" } else { "" };
        let amount = terminal::tokens(self.reduced);
        if sized < self.count {
            format!(
                "{} recorded · {approx}{amount} freed in {sized}",
                self.count
            )
        } else {
            format!("{} recorded · {approx}{amount} tokens freed", self.count)
        }
    }
}

pub(crate) struct Event {
    pub thread: String,
    project: Option<String>,
    pub at: f64,
    reduction: Option<(f64, bool)>,
}

struct Boundary {
    thread: String,
    project: Option<String>,
    provider_thread: Option<String>,
    start: String,
    end: String,
    before: Option<f64>,
    after: Option<f64>,
}

struct Sample {
    used: f64,
    max: Option<f64>,
    model: Option<String>,
}

fn count(row: &rusqlite::Row<'_>, column: usize) -> Option<f64> {
    row.get::<_, f64>(column)
        .ok()
        .filter(|n| n.is_finite() && *n >= 0.)
}

pub(crate) fn read(db: &Connection, from: f64, until: f64) -> rusqlite::Result<Vec<Event>> {
    let boundaries = {
        let mut stmt = db.prepare_cached(&format!(
            "SELECT i.thread_id, i.provider_thread_id, i.updated_at,
                    CASE WHEN json_valid(i.payload_json) THEN json_extract(i.payload_json,'$.startedAt') END,
                    CASE WHEN json_valid(i.payload_json) THEN json_extract(i.payload_json,'$.beforeTokenCount') END,
                    CASE WHEN json_valid(i.payload_json) THEN json_extract(i.payload_json,'$.afterTokenCount') END,
                    t.project_id
             FROM {PREFIX}turn_items i LEFT JOIN {PREFIX}threads t ON t.thread_id=i.thread_id
             WHERE i.type='compaction' AND i.status='completed'
             ORDER BY i.thread_id, i.provider_thread_id, i.updated_at, i.ordinal"
        ))?;
        stmt.query_map([], |row| {
            let end: String = row.get(2)?;
            let start = row
                .get::<_, String>(3)
                .ok()
                .filter(|s| recap::ms(s).is_some() && s <= &end)
                .unwrap_or_else(|| end.clone());
            Ok(Boundary {
                thread: row.get(0)?,
                project: row.get(6)?,
                provider_thread: row.get(1)?,
                start,
                end,
                before: count(row, 4),
                after: count(row, 5),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let used = "json_extract(payload_json,'$.tokenUsage.usedTokens')";
    let statement = |side: &str, edge: &str, order: &str| {
        format!(
            "SELECT {used}, json_extract(payload_json,'$.tokenUsage.maxTokens'),
                    json_extract(payload_json,'$.model')
             FROM {PREFIX}provider_turns
             WHERE thread_id=?1 AND provider_thread_id=?2
              AND completed_at {side} ?3 AND (?4 IS NULL OR completed_at {edge} ?4)
              AND CASE WHEN json_valid(payload_json) THEN {used} >= 0 END
             ORDER BY completed_at {order}, ordinal {order} LIMIT 1"
        )
    };
    let before_sql = statement("<", ">", "DESC");
    let after_sql = statement(">=", "<", "ASC");
    let mut events = Vec::new();
    for (i, b) in boundaries.iter().enumerate() {
        let Some(at) = recap::ms(&b.end).filter(|at| (from..until).contains(at)) else {
            continue;
        };
        let same_session = |other: &&Boundary| {
            b.thread == other.thread && b.provider_thread == other.provider_thread
        };
        let previous = i
            .checked_sub(1)
            .and_then(|p| boundaries.get(p))
            .filter(same_session);
        let next = boundaries.get(i + 1).filter(same_session);
        let reduction = match (b.before, b.after) {
            (Some(before), Some(after)) => Some(((before - after).max(0.), false)),
            _ if b.provider_thread.is_some() => {
                let sample = |sql: &str, edge: &str, neighbor: Option<&str>| {
                    db.prepare_cached(sql)?
                        .query_row(
                            params![b.thread, b.provider_thread, edge, neighbor],
                            |row| {
                                Ok(count(row, 0).map(|used| Sample {
                                    used,
                                    max: count(row, 1).filter(|max| *max > 0.),
                                    model: row.get(2).ok(),
                                }))
                            },
                        )
                        .optional()
                        .map(Option::flatten)
                };
                let before = sample(&before_sql, &b.start, previous.map(|p| p.end.as_str()))?;
                let after = sample(&after_sql, &b.end, next.map(|n| n.start.as_str()))?;
                estimate(b, before.as_ref(), after.as_ref()).map(|n| (n, true))
            }
            _ => None,
        };
        events.push(Event {
            thread: b.thread.clone(),
            project: b.project.clone(),
            at,
            reduction,
        });
    }
    Ok(events)
}

fn estimate(b: &Boundary, before: Option<&Sample>, after: Option<&Sample>) -> Option<f64> {
    // Two sampled endpoints must use the same model/window. A provider's own
    // boundary value can also pair with the nearest same-session sample.
    if b.before.is_none() && b.after.is_none() {
        let (a, b) = (before?, after?);
        if a.max.is_none() || a.max != b.max || a.model != b.model {
            return None;
        }
    }
    let delta =
        b.before.or_else(|| before.map(|p| p.used))? - b.after.or_else(|| after.map(|p| p.used))?;
    (delta > 0.).then_some(delta)
}

#[derive(Debug, Default)]
pub struct Totals {
    pub chats: HashMap<String, Summary>,
    pub projects: HashMap<String, Summary>,
}

#[derive(Default)]
pub(crate) struct Tracker {
    latest: Option<(f64, (i64, i64), Arc<Totals>)>,
}

impl Tracker {
    pub(crate) fn poll(
        &mut self,
        db: &Connection,
        now: f64,
        version: (i64, i64),
    ) -> Option<Arc<Totals>> {
        if let Some((last, previous, summaries)) = &self.latest
            && (*previous == version || (*last..*last + 4000.).contains(&now))
        {
            return Some(Arc::clone(summaries));
        }
        let summaries = db.unchecked_transaction().and_then(|tx| {
            let mut summaries = Totals::default();
            for event in read(&tx, f64::NEG_INFINITY, f64::INFINITY)? {
                summaries
                    .chats
                    .entry(event.thread.clone())
                    .or_default()
                    .add(&event);
                if let Some(project) = &event.project {
                    summaries
                        .projects
                        .entry(project.clone())
                        .or_default()
                        .add(&event);
                }
            }
            Ok(summaries)
        });
        if let Ok(summaries) = summaries {
            let summaries = Arc::new(summaries);
            self.latest = Some((now, version, Arc::clone(&summaries)));
            return Some(summaries);
        }
        self.latest
            .as_ref()
            .map(|(_, _, summaries)| Arc::clone(summaries))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn retained_records_measure_compaction_and_estimate_only_same_session_drops() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(include_str!("../../../checks/fixtures/t3-schema.sql"))
            .unwrap();
        let floor = recap::ms("2026-11-27T00:00:00.000Z").unwrap();
        let stamp = |minute| recap::iso(floor + f64::from(minute) * 60_000.);
        let event = |id, thread, provider, minute, status, payload: serde_json::Value| {
            db.execute(&format!(
                "INSERT INTO {PREFIX}turn_items
                 (turn_item_id,thread_id,provider_thread_id,ordinal,type,status,updated_at,payload_json)
                 VALUES(?1,?2,?3,?4,'compaction',?5,?6,?7)"
            ), params![id, thread, provider, minute, status, stamp(minute), payload.to_string()]).unwrap();
        };
        let turn = |id, thread, provider, minute, model, used| {
            db.execute(&format!(
                "INSERT INTO {PREFIX}provider_turns
                 (provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,completed_at,payload_json)
                 VALUES(?1,?2,?3,'node',?4,'completed',?5,?6)"
            ), params![id, thread, provider, minute, stamp(minute),
                json!({"model":model,"tokenUsage":{"usedTokens":used,"maxTokens":475000}}).to_string()]).unwrap();
        };
        event(
            "previous",
            "archived",
            "old-provider",
            10,
            "completed",
            json!({"beforeTokenCount":430000,"afterTokenCount":90000,"summary":"private"}),
        );
        turn("before", "new", "new-provider", 20, "future-model", 313400);
        event(
            "estimated",
            "new",
            "new-provider",
            30,
            "completed",
            json!({}),
        );
        turn("after", "new", "new-provider", 40, "future-model", 84400);
        event(
            "switched",
            "new",
            "new-provider",
            50,
            "completed",
            json!({}),
        );
        turn(
            "switched-after",
            "new",
            "new-provider",
            60,
            "different-model",
            25000,
        );
        event(
            "failed",
            "new",
            "new-provider",
            65,
            "failed",
            json!({"beforeTokenCount":99999,"afterTokenCount":1}),
        );
        // An otherwise similar drop across another session has no bearing on this event.
        turn(
            "unrelated",
            "new",
            "other-session",
            25,
            "future-model",
            410000,
        );
        db.execute_batch(&format!(
            "INSERT INTO {PREFIX}threads
             (thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,archived_at,payload_json)
             VALUES ('archived','project','Archived','codex','default','default','','','2026-11-27','{{}}'),
                    ('new','project','New','codex','default','default','','',NULL,'{{}}');"
        )).unwrap();
        let result = Tracker::default()
            .poll(&db, floor + recap::HOUR, (1, 1))
            .unwrap();
        assert_eq!(
            result.chats["archived"],
            Summary {
                count: 1,
                measured: 1,
                estimated: 0,
                reduced: 340000.
            }
        );
        assert_eq!(
            result.chats["new"],
            Summary {
                count: 2,
                measured: 0,
                estimated: 1,
                reduced: 229000.
            }
        );
        assert_eq!(
            result.projects["project"],
            Summary {
                count: 3,
                measured: 1,
                estimated: 1,
                reduced: 569000.
            }
        );
    }

    #[test]
    fn unavailable_compaction_history_has_no_count() {
        let db = Connection::open_in_memory().unwrap();
        assert!(Tracker::default().poll(&db, 0., (1, 1)).is_none());
    }
}
