//! Read-only model and project comparisons from completed local T3 records.
use crate::{
    compaction::{self, Summary},
    recap::{self, Local, Period, Span, Which, iso, local_date, ms},
    source::{SourceError, daily_usage, usage_history},
    terminal,
};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt::Write,
    path::Path,
};

const PREFIX: &str = "orchestration_v2_projection_";
const JSON_SCHEMA: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Tokens {
    pub fresh: f64,
    pub cached: f64,
    pub output: f64,
    pub cache_written: f64,
    pub turns: u32,
}

impl Tokens {
    pub fn total(self) -> f64 {
        self.fresh + self.cached + self.cache_written + self.output
    }

    fn add(&mut self, turn: &daily_usage::ReportedTurn) {
        self.fresh += turn.input - turn.cached;
        self.cached += turn.cached;
        self.output += turn.output;
        self.turns += 1;
    }

    fn add_record(&mut self, record: &usage_history::Entry) {
        self.fresh += record.counts.fresh;
        self.cached += record.counts.cached;
        self.cache_written += record.counts.written;
        self.output += record.counts.output;
        self.turns += 1;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Metrics {
    pub completed: u32,
    pub failed: u32,
    pub median_ms: Option<f64>,
    pub tokens: Tokens,
    /// Completed runs that started in this period and have linked reported usage.
    pub reported_runs: u32,
    pub tokens_per_reported_run: Option<f64>,
    pub compactions: Summary,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Day {
    pub date: String,
    /// Days before this database's recorded history have no value.
    pub tokens: Option<Tokens>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Window {
    pub from: String,
    pub through: String,
    pub first_record: Option<String>,
    pub so_far: bool,
    pub total: Metrics,
    pub days: Vec<Day>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_history: Option<UsageWindow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageWindow {
    pub tokens: Tokens,
    pub models: BTreeMap<String, Tokens>,
    pub providers: BTreeMap<String, Tokens>,
    pub days: Vec<Day>,
    pub partial: bool,
}

impl UsageWindow {
    pub(crate) fn summarize(
        history: &usage_history::History,
        period: &Period,
        local: Local<'_>,
    ) -> Option<Self> {
        let (from, until) = (period.start(), period.until());
        let mut tokens = Tokens::default();
        let mut models = BTreeMap::<String, Tokens>::new();
        let mut providers = BTreeMap::<String, Tokens>::new();
        let mut days = vec![Tokens::default(); period.len()];
        for entry in history
            .entries
            .iter()
            .filter(|entry| (from..until).contains(&entry.at))
        {
            let day = period
                .days
                .partition_point(|date| *date <= entry.at)
                .saturating_sub(1);
            days[day].add_record(entry);
            tokens.add_record(entry);
            models
                .entry(entry.model.clone())
                .or_default()
                .add_record(entry);
            providers
                .entry(entry.provider.clone())
                .or_default()
                .add_record(entry);
        }
        (tokens.turns > 0).then(|| Self {
            tokens,
            models,
            providers,
            days: period
                .days
                .iter()
                .zip(days)
                .take_while(|(date, _)| **date < until)
                .map(|(date, tokens)| Day {
                    date: local_date(local, *date),
                    tokens: (tokens.turns > 0).then_some(tokens),
                })
                .collect(),
            partial: history.partial,
        })
    }

    pub fn source(&self) -> &'static str {
        if self.partial {
            "T3 Usage · partial history"
        } else {
            "T3 Usage · local history"
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Model {
    pub name: String,
    pub selected: Metrics,
    pub prior: Metrics,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub selected: Metrics,
    pub prior: Metrics,
    pub models: Vec<Model>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Comparison {
    pub selected: Window,
    pub prior: Window,
    pub projects: Vec<Project>,
    pub models: Vec<Model>,
}

pub fn printed_json(report: &Comparison) -> Result<String, serde_json::Error> {
    crate::report_json::with_schema(report, JSON_SCHEMA)
}

impl Comparison {
    pub fn model_totals(&self) -> Vec<Model> {
        fn period(which: &Window, models: &mut BTreeMap<String, Model>, selected: bool) {
            let Some(usage) = &which.usage_history else {
                return;
            };
            for model in models.values_mut() {
                let counts = if selected {
                    &mut model.selected
                } else {
                    &mut model.prior
                };
                counts.tokens = Tokens::default();
            }
            for (name, tokens) in &usage.models {
                let name = if models.contains_key(name) {
                    name.clone()
                } else {
                    let qualified = format!("opencode/{name}");
                    if models.contains_key(&qualified) {
                        qualified
                    } else {
                        name.clone()
                    }
                };
                let model = models.entry(name.clone()).or_insert_with(|| Model {
                    name,
                    selected: Metrics::default(),
                    prior: Metrics::default(),
                });
                if selected {
                    model.selected.tokens = *tokens;
                } else {
                    model.prior.tokens = *tokens;
                }
            }
        }
        if self.selected.usage_history.is_none() && self.prior.usage_history.is_none() {
            return self.models.clone();
        }
        let mut models = self
            .models
            .iter()
            .map(|m| (m.name.clone(), m.clone()))
            .collect();
        period(&self.selected, &mut models, true);
        period(&self.prior, &mut models, false);
        let mut rows: Vec<_> = models.into_values().collect();
        rows.sort_by(|a, b| {
            rank(a.selected, a.prior, b.selected, b.prior).then_with(|| a.name.cmp(&b.name))
        });
        rows
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(
            out,
            "INSIGHTS COMPARE | {} to {} | prior {} to {}",
            self.selected.from, self.selected.through, self.prior.from, self.prior.through
        );
        let headers = "Done / failed   Median   Reported tokens   Prior tokens";
        let _ = writeln!(out, "\nPROJECTS   {headers}");
        for project in &self.projects {
            let _ = writeln!(
                out,
                "{}  {}",
                project.name,
                metric_text(project.selected, project.prior)
            );
        }
        let scope = self
            .selected
            .usage_history
            .as_ref()
            .map_or("Thread totals", UsageWindow::source);
        let _ = writeln!(out, "\nMODELS   {headers}   {scope}");
        for model in self.model_totals() {
            let _ = writeln!(
                out,
                "{}  {}",
                model.name,
                metric_text(model.selected, model.prior)
            );
        }
        out
    }
}

fn metric_text(current: Metrics, prior: Metrics) -> String {
    let median = current
        .median_ms
        .map(recap::span_text)
        .unwrap_or_else(|| "—".into());
    format!(
        "{}/{}   {}   {}   {}",
        current.completed,
        current.failed,
        median,
        token_text(current.tokens),
        token_text(prior.tokens)
    )
}

pub fn token_text(tokens: Tokens) -> String {
    if tokens.turns == 0 {
        "—".into()
    } else {
        terminal::tokens(tokens.total())
    }
}

fn periods(now: f64, which: Which, older: u8, local: Local<'_>) -> (Period, Period) {
    let mut selected = Period::new(now, which, local);
    let containing = match which.span() {
        Span::Week if which == Which::Last7Days => Which::Last7Days,
        Span::Week => Which::ThisWeek,
        Span::Month => Which::ThisMonth,
    };
    let previous = |period: &Period| {
        let mut prior = Period::new(period.start() - 1., containing, local);
        prior.so_far = false;
        prior.now = prior.end();
        prior
    };
    for _ in 0..older {
        selected = previous(&selected);
    }
    let mut prior = previous(&selected);
    if selected.so_far {
        prior.now = (prior.start() + (selected.until() - selected.start())).min(prior.end());
        prior.so_far = prior.now < prior.end();
    }
    (selected, prior)
}

pub fn load(
    path: &Path,
    now: f64,
    which: Which,
    older: u8,
    local: Local<'_>,
) -> Result<Comparison, SourceError> {
    let (selected, prior) = periods(now, which, older, local);
    let db = recap::open(path)?;
    let history = usage_history::read(path, prior.start(), selected.until());
    recap::read_in_transaction(&db, path, || read(&db, &selected, &prior, local, &history))
}

type Projects = HashMap<String, (String, String)>;

fn read(
    db: &Connection,
    selected: &Period,
    prior: &Period,
    local: Local<'_>,
    history: &usage_history::History,
) -> rusqlite::Result<Comparison> {
    // Historical threads keep their project key even when the project leaves the sidebar.
    let mut query = db.prepare(&format!(
        "SELECT t.thread_id, t.project_id, p.title, t.created_at
         FROM {PREFIX}threads t LEFT JOIN projection_projects p USING(project_id)"
    ))?;
    let mut rows = query.query([])?;
    let mut by_thread = Projects::new();
    let mut first_record: Option<f64> = None;
    while let Some(row) = rows.next()? {
        let id: String = row.get(1)?;
        let title = row
            .get::<_, Option<String>>(2)?
            .as_deref()
            .and_then(daily_usage::name)
            .unwrap_or_else(|| {
                if id.is_empty() {
                    "No project".into()
                } else {
                    format!("Project {id}")
                }
            });
        by_thread.insert(row.get(0)?, (id, terminal::clean(&title)));
        if let Some(at) = ms(&row.get::<_, String>(3)?) {
            first_record = Some(first_record.map_or(at, |first| first.min(at)));
        }
    }
    let events = compaction::read(db, prior.start(), selected.until())?;
    let a = read_window(db, selected, &by_thread, history, &events, local)?;
    let b = read_window(db, prior, &by_thread, history, &events, local)?;
    let project_ids: BTreeSet<_> = a.projects.keys().chain(b.projects.keys()).collect();
    let mut projects = Vec::with_capacity(project_ids.len());
    for id in project_ids {
        let left = a.projects.get(id);
        let right = b.projects.get(id);
        projects.push(Project {
            id: id.clone(),
            name: left.or(right).unwrap().name.clone(),
            selected: left.map_or_else(Metrics::default, |p| p.total.metrics()),
            prior: right.map_or_else(Metrics::default, |p| p.total.metrics()),
            models: models(left.map(|p| &p.models), right.map(|p| &p.models)),
        });
    }
    projects.sort_by(|a, b| {
        rank(a.selected, a.prior, b.selected, b.prior).then_with(|| a.name.cmp(&b.name))
    });
    Ok(Comparison {
        selected: a.window(selected, first_record, local),
        prior: b.window(prior, first_record, local),
        models: models(Some(&a.models), Some(&b.models)),
        projects,
    })
}

#[derive(Default)]
struct Counter {
    completed: u32,
    failed: u32,
    durations: Vec<f64>,
    tokens: Tokens,
    reported_runs: u32,
    linked_tokens: f64,
    compactions: Summary,
}

impl Counter {
    fn record_run(&mut self, run: &Run, linked_tokens: Option<f64>) {
        if run.status == "failed" {
            self.failed += 1;
        }
        if run.status == "completed" {
            self.completed += 1;
            if let (Some(start), Some(end)) = (run.started, run.completed)
                && start <= end
            {
                self.durations.push(end - start);
            }
            if let Some(count) = linked_tokens {
                self.reported_runs += 1;
                self.linked_tokens += count;
            }
        }
    }

    fn metrics(&self) -> Metrics {
        let mut durations = self.durations.clone();
        durations.sort_by(f64::total_cmp);
        let n = durations.len();
        Metrics {
            completed: self.completed,
            failed: self.failed,
            median_ms: (n > 0).then(|| (durations[(n - 1) / 2] + durations[n / 2]) / 2.),
            tokens: self.tokens,
            reported_runs: self.reported_runs,
            tokens_per_reported_run: (self.reported_runs > 0)
                .then(|| self.linked_tokens / f64::from(self.reported_runs)),
            compactions: self.compactions,
        }
    }
}

#[derive(Default)]
struct Group {
    name: String,
    total: Counter,
    models: BTreeMap<String, Counter>,
}

#[derive(Default)]
struct Totals {
    total: Counter,
    models: BTreeMap<String, Counter>,
    projects: BTreeMap<String, Group>,
    days: Vec<Tokens>,
    usage_history: Option<UsageWindow>,
}

impl Totals {
    fn project(&mut self, threads: &Projects, thread: &str) -> &mut Group {
        let (key, name) = threads
            .get(thread)
            .map_or(("", "No project"), |(id, name)| {
                (id.as_str(), name.as_str())
            });
        self.projects
            .entry(key.to_owned())
            .or_insert_with(|| Group {
                name: name.into(),
                ..Group::default()
            })
    }

    fn window(&self, period: &Period, first: Option<f64>, local: Local<'_>) -> Window {
        let until = period.until();
        let days = period
            .days
            .windows(2)
            .zip(&self.days)
            .take_while(|(range, _)| range[0] < until)
            .map(|(range, tokens)| Day {
                date: local_date(local, range[0]),
                tokens: (tokens.turns > 0
                    || first.is_some_and(|first| first < range[1].min(until)))
                .then_some(*tokens),
            })
            .collect();
        Window {
            from: local_date(local, period.start()),
            through: local_date(local, (until - 1.).max(period.start())),
            first_record: first.map(|stamp| local_date(local, stamp)),
            so_far: period.so_far,
            total: self.total.metrics(),
            days,
            usage_history: self.usage_history.clone(),
        }
    }
}

struct Run {
    thread: String,
    label: String,
    status: String,
    started: Option<f64>,
    completed: Option<f64>,
    by_model: BTreeMap<String, f64>,
}

fn read_window(
    db: &Connection,
    period: &Period,
    threads: &Projects,
    history: &usage_history::History,
    events: &[compaction::Event],
    local: Local<'_>,
) -> rusqlite::Result<Totals> {
    let (from, until) = (period.start(), period.until());
    let mut query = db.prepare_cached(&format!(
        "SELECT run_id, thread_id, provider_thread_id, provider, status, completed_at,
                CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.modelSelection.model') END,
                CASE WHEN json_valid(payload_json) THEN json_extract(payload_json,'$.startedAt') END
         FROM {PREFIX}runs WHERE requested_at < ?2 AND (completed_at IS NULL OR completed_at >= ?1)"
    ))?;
    let mut rows = query.query(params![iso(from), iso(until)])?;
    let stamp = |value: Option<String>| value.as_deref().and_then(ms);
    let mut runs: HashMap<String, Run> = HashMap::new();
    let mut by_provider = HashMap::<String, Vec<String>>::new();
    while let Some(row) = rows.next()? {
        let id: String = row.get(0)?;
        let provider_thread: Option<String> = row.get(2)?;
        if let Some(provider_thread) = &provider_thread {
            by_provider
                .entry(provider_thread.clone())
                .or_default()
                .push(id.clone());
        }
        let label = row
            .get::<_, String>(6)
            .ok()
            .as_deref()
            .and_then(daily_usage::name)
            .or_else(|| {
                row.get::<_, String>(3)
                    .ok()
                    .as_deref()
                    .and_then(daily_usage::name)
            })
            .unwrap_or_else(|| "Unattributed".into());
        runs.insert(
            id,
            Run {
                thread: row.get(1)?,
                label,
                status: row.get(4)?,
                completed: stamp(row.get(5)?),
                started: stamp(row.get::<_, String>(7).ok()),
                by_model: BTreeMap::new(),
            },
        );
    }

    let mut totals = Totals {
        days: vec![Tokens::default(); period.len()],
        usage_history: UsageWindow::summarize(history, period, local),
        ..Totals::default()
    };
    let day_index = |at| {
        period
            .days
            .partition_point(|day| *day <= at)
            .saturating_sub(1)
    };
    for event in events
        .iter()
        .filter(|event| (from..until).contains(&event.at))
    {
        totals.total.compactions.add(event);
        totals
            .project(threads, &event.thread)
            .total
            .compactions
            .add(event);
    }
    daily_usage::reported_turns(db, from, until, |turn| {
        // A provider session can be reused. Use time only when exactly one run owns this turn.
        let run_id = turn.run.as_ref().or_else(|| {
            let mut matching = by_provider.get(&turn.provider_thread)?.iter().filter(|id| {
                let run = &runs[*id];
                run.thread == turn.thread
                    && run.started.is_some_and(|start| start <= turn.completed)
                    && run.completed.is_none_or(|end| turn.completed <= end)
            });
            let first = matching.next()?;
            matching.next().is_none().then_some(first)
        });
        let run = run_id.and_then(|id| runs.get_mut(id));
        let model = turn
            .model
            .clone()
            .or_else(|| run.as_ref().map(|r| r.label.clone()))
            .or_else(|| turn.provider.clone())
            .unwrap_or_else(|| "Unattributed".into());
        if let Some(run) = run {
            *run.by_model.entry(model.clone()).or_default() += turn.input + turn.output;
        }
        totals.total.tokens.add(&turn);
        totals
            .models
            .entry(model.clone())
            .or_default()
            .tokens
            .add(&turn);
        let day = day_index(turn.completed);
        if let Some(day) = totals.days.get_mut(day) {
            day.add(&turn);
        }
        let group = totals.project(threads, &turn.thread);
        group.total.tokens.add(&turn);
        group.models.entry(model).or_default().tokens.add(&turn);
    })?;

    for run in runs.values() {
        if !run
            .completed
            .is_some_and(|end| (from..until).contains(&end))
        {
            continue;
        }
        if !matches!(run.status.as_str(), "completed" | "failed") {
            continue;
        }
        let model = if run.by_model.len() == 1 {
            run.by_model.keys().next().unwrap()
        } else {
            &run.label
        };
        let rate = run
            .started
            .is_some_and(|start| (from..until).contains(&start));
        let linked = (rate && !run.by_model.is_empty()).then(|| run.by_model.values().sum());
        let model_linked = run.by_model.get(model).copied().filter(|_| rate);
        totals.total.record_run(run, linked);
        totals
            .models
            .entry(model.clone())
            .or_default()
            .record_run(run, model_linked);
        let group = totals.project(threads, &run.thread);
        group.total.record_run(run, linked);
        group
            .models
            .entry(model.clone())
            .or_default()
            .record_run(run, model_linked);
    }
    Ok(totals)
}

fn models(
    a: Option<&BTreeMap<String, Counter>>,
    b: Option<&BTreeMap<String, Counter>>,
) -> Vec<Model> {
    let names: BTreeSet<_> = a.into_iter().chain(b).flat_map(BTreeMap::keys).collect();
    let mut rows: Vec<_> = names
        .into_iter()
        .map(|name| Model {
            name: name.clone(),
            selected: a
                .and_then(|a| a.get(name))
                .map_or_else(Metrics::default, Counter::metrics),
            prior: b
                .and_then(|b| b.get(name))
                .map_or_else(Metrics::default, Counter::metrics),
        })
        .collect();
    rows.sort_by(|a, b| {
        rank(a.selected, a.prior, b.selected, b.prior).then_with(|| a.name.cmp(&b.name))
    });
    rows
}

fn rank(a: Metrics, a_prior: Metrics, b: Metrics, b_prior: Metrics) -> std::cmp::Ordering {
    let key = |m: Metrics, prior: Metrics| (m.tokens.total(), m.completed, prior.tokens.total());
    key(b, b_prior)
        .partial_cmp(&key(a, a_prior))
        .unwrap_or(std::cmp::Ordering::Equal)
}

pub fn demo(
    now: f64,
    which: Which,
    older: u8,
    local: Local<'_>,
) -> Result<Comparison, SourceError> {
    let (selected, prior) = periods(now, which, older, local);
    let result = (|| {
        let db = Connection::open_in_memory()?;
        fill_demo(&db, &prior, &selected)?;
        read(
            &db,
            &selected,
            &prior,
            local,
            &usage_history::History::default(),
        )
    })();
    result.map_err(|error: rusqlite::Error| SourceError {
        kind: "schema",
        path: ":memory:".into(),
        detail: error.to_string(),
    })
}

fn fill_demo(db: &Connection, prior: &Period, selected: &Period) -> rusqlite::Result<()> {
    db.execute_batch(include_str!("../../../checks/fixtures/t3-schema.sql"))?;
    for (project, title, model) in [
        ("demo-index", "Search index", "gpt-6.2-sol"),
        ("demo-billing", "Billing migration", "gemini-4-neon"),
    ] {
        let created = iso(prior.start() - recap::HOUR);
        db.execute(
            "INSERT INTO projection_projects(project_id,title,workspace_root,scripts_json,created_at,updated_at)
             VALUES (?1,?2,'/demo','[]',?3,?3)", params![project, title, created],
        )?;
        db.execute(&format!(
            "INSERT INTO {PREFIX}threads(thread_id,project_id,title,default_provider,runtime_mode,interaction_mode,created_at,updated_at,payload_json)
             VALUES (?1,?1,?2,'demo','default','default',?3,?3,'{{}}')"), params![project, title, created])?;
        for (week, period) in [prior, selected].into_iter().enumerate() {
            for (day, &start) in period.days[..period.len()].iter().enumerate() {
                let done = start + (10. + day as f64 / 15.) * recap::HOUR;
                if done >= period.until() {
                    continue;
                }
                let id = format!("{project}-{week}-{day}");
                let started = iso(done - (10. + day as f64 * 3.) * 60_000.);
                let completed = iso(done);
                let status = if day % 7 == 3 { "failed" } else { "completed" };
                db.execute(&format!(
                    "INSERT INTO {PREFIX}runs(run_id,thread_id,ordinal,provider,provider_thread_id,status,requested_at,completed_at,payload_json)
                     VALUES (?1,?2,?3,'demo',?1,?4,?5,?6,?7)"),
                    params![id, project, (week * 32 + day) as i64, status, started, completed,
                        serde_json::json!({"modelSelection":{"model":model},"startedAt":started}).to_string()])?;
                let input = (day + 2) * (week + 3) * 48_000;
                db.execute(&format!(
                    "INSERT INTO {PREFIX}provider_turns(provider_turn_id,thread_id,provider_thread_id,node_id,ordinal,status,completed_at,payload_json)
                     VALUES (?1,?2,?1,?1,1,?3,?4,?5)"),
                    params![id, project, status, completed, serde_json::json!({"model":model,"turnTokenUsage":{
                        "usageStatus":"complete","inputTokens":input,"cachedInputTokens":input*4/5,"outputTokens":1200
                    }}).to_string()])?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_turns_split_model_project_and_cache_without_counting_a_turn_twice() {
        let local = &|_| None;
        let now = ms("2026-11-28T15:00:00.000Z").unwrap();
        let report = demo(now, Which::LastWeek, 0, local).unwrap();
        assert_eq!(
            (report.selected.from.as_str(), report.prior.from.as_str()),
            ("2026-11-16", "2026-11-09")
        );
        assert_eq!(report.projects.len(), 2);
        assert_eq!(report.models.len(), 2);
        let total = report.selected.total;
        assert_eq!(
            (
                total.completed,
                total.failed,
                total.tokens.turns,
                total.reported_runs
            ),
            (12, 2, 14, 12)
        );
        assert_eq!(
            total.tokens.total(),
            report
                .projects
                .iter()
                .map(|p| p.selected.tokens.total())
                .sum::<f64>()
        );
        assert!(total.tokens.cached > total.tokens.fresh);
        assert_eq!(total.median_ms, Some(1_140_000.));
        assert!(report.projects.iter().all(|p| p.models.len() == 1));
        let older = demo(now, Which::LastWeek, 1, local).unwrap();
        assert_eq!(older.selected.from, report.prior.from);
    }
}
