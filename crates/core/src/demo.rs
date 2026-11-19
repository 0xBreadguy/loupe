//! Native, deterministic port of oracle/src/demo.ts.
//! Authored synthetic roles become an in-memory scratch projection, then pass
//! through the same narrow adapter as explicit read-only live mode. No files,
//! services, user database, usage records or credentials are opened here.
use crate::{
    model::Snapshot,
    source::{self, SourceError},
    trace::{self, Target, Trace},
};
use rusqlite::{Connection, params_from_iter, types::Value as SqlValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::OnceLock};
const PREFIX: &str = "orchestration_v2_projection_";
const MIN: f64 = 60_000.;
pub const CYCLE_MS: f64 = 20. * MIN;
pub const REFRESH_MS: f64 = 3000.;
const GROWTH: f64 = 0.0025;
#[derive(Clone, Debug, Deserialize)]
struct Scenario {
    projects: Vec<Project>,
    roles: Vec<Role>,
}
#[derive(Clone, Debug, Deserialize)]
struct Project {
    id: String,
    title: String,
    root: String,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct Role {
    id: String,
    title: String,
    model: String,
    #[serde(rename = "max")]
    maximum: f64,
    ctx: Option<[f64; 2]>,
    status: String,
    from: f64,
    to: Option<f64>,
    until: Option<f64>,
    queued: Option<f64>,
    rest: Option<f64>,
    every: f64,
    work: Vec<String>,
    tail: Vec<Vec<Value>>,
    plan: Option<Vec<String>>,
    cache: Option<f64>,
    options: Vec<Value>,
    project: Option<String>,
    branch: Option<String>,
    before: Option<Before>,
    subs: Vec<Role>,
    child: bool,
    remote: Option<String>,
    shelf: Option<String>,
}
#[derive(Clone, Debug, Deserialize)]
struct Before {
    model: String,
    #[serde(rename = "max")]
    maximum: f64,
    from: f64,
    to: f64,
    ctx: [f64; 2],
}
#[derive(Clone, Debug)]
struct Where {
    thread: String,
    run: String,
    provider: Option<String>,
}
#[derive(Clone, Debug)]
struct Host {
    thread: String,
    run: String,
    node: String,
}
#[derive(Clone, Debug)]
struct Life {
    status: String,
    start: f64,
    ended: Option<f64>,
    end: f64,
    busy: bool,
}
struct Call {
    kind: &'static str,
    payload: Value,
    flaky: bool,
}
fn call(spec: &str) -> Call {
    let flaky = spec.starts_with('!');
    let text = if flaky { &spec[1..] } else { spec };
    let rest = text.get(2..).unwrap_or("");
    let (kind, payload) = if text.starts_with("$ ") {
        ("command_execution", json!({"input":rest}))
    } else if text.starts_with("~ ") {
        ("file_change", json!({"fileName":rest}))
    } else if text.starts_with("? ") {
        ("web_search", json!({"title":rest}))
    } else if text.starts_with("@ ") {
        ("user_input_request", json!({"title":rest}))
    } else if text.starts_with("% ") {
        ("subagent", json!({"title":rest}))
    } else {
        (
            "dynamic_tool",
            json!({"toolName":text.split(' ').next().unwrap_or(""),"title":text}),
        )
    };
    Call {
        kind,
        payload,
        flaky,
    }
}
fn random(keys: &[&str]) -> f64 {
    let mut h = 2166136261u32;
    for ch in keys.join("|").chars() {
        let code = ch as u32;
        h = (h ^ code).wrapping_mul(16777619);
    }
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b3c6d);
    h ^= h >> 12;
    h as f64 / 4294967296.
}
fn iso(stamp: f64) -> String {
    // The public projection clock parser is UTC; this inverse matches Date.toISOString.
    let millis = stamp.trunc() as i64;
    let seconds = millis.div_euclid(1000);
    let days = seconds.div_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let dayseconds = seconds.rem_euclid(86400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        dayseconds / 3600,
        (dayseconds % 3600) / 60,
        dayseconds % 60,
        millis.rem_euclid(1000)
    )
}
fn stamp(t: Option<f64>) -> Value {
    t.map(|t| Value::String(iso(t))).unwrap_or(Value::Null)
}
fn error(error: impl std::fmt::Display) -> SourceError {
    SourceError {
        kind: "schema",
        path: PathBuf::from("synthetic:demo"),
        detail: format!("synthetic demo: {error}"),
    }
}
struct Writer<'a> {
    db: &'a Connection,
    ordinal: u64,
}
impl Writer<'_> {
    fn row(&self, table: &str, values: Value) -> rusqlite::Result<()> {
        let values = values.as_object().expect("authored demo row object");
        let keys: Vec<&str> = values.keys().map(String::as_str).collect();
        let sql = format!(
            "INSERT INTO {table} ({}) VALUES ({})",
            keys.join(","),
            vec!["?"; keys.len()].join(",")
        );
        let params: Vec<SqlValue> = values
            .values()
            .map(|v| match v {
                Value::Null => SqlValue::Null,
                Value::String(s) => SqlValue::Text(s.clone()),
                Value::Number(n) => n
                    .as_i64()
                    .map(SqlValue::Integer)
                    .unwrap_or_else(|| SqlValue::Real(n.as_f64().unwrap_or(0.))),
                Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
                v => SqlValue::Text(v.to_string()),
            })
            .collect();
        self.db
            .prepare_cached(&sql)?
            .execute(params_from_iter(params))?;
        Ok(())
    }
    fn item(
        &mut self,
        w: &Where,
        kind: &str,
        status: &str,
        start: f64,
        updated: f64,
        mut payload: Value,
    ) -> rusqlite::Result<()> {
        let n = self.ordinal;
        self.ordinal += 1;
        payload
            .as_object_mut()
            .expect("authored payload object")
            .insert("startedAt".into(), json!(iso(start)));
        self.row(&format!("{PREFIX}turn_items"),json!({"turn_item_id":format!("item-{n}"),"thread_id":w.thread,"run_id":w.run,"node_id":null,"provider_thread_id":w.provider,"type":kind,"status":status,"updated_at":iso(updated),"ordinal":n,"payload_json":payload}))
    }
}
struct Scene<'a> {
    w: Writer<'a>,
    now: f64,
    epoch: f64,
    base: f64,
}
impl Scene<'_> {
    fn at(&self, minutes: f64) -> f64 {
        self.base - minutes * MIN
    }
    fn life(&self, r: &Role) -> Life {
        if let Some(delay) = r.queued {
            let starts = self.base + delay * MIN;
            let queued = self.now < starts;
            return Life {
                status: if queued { "queued" } else { "running" }.into(),
                start: if queued { self.at(r.from) } else { starts },
                ended: None,
                end: self.now,
                busy: !queued,
            };
        }
        let ended = r.to.map(|t| self.at(t));
        let until = r.until.map(|t| self.at(t)).or(ended).unwrap_or(self.now);
        Life {
            status: r.status.clone(),
            start: self.at(r.from),
            ended,
            end: until.min(self.now),
            busy: r.status == "running" && ended.is_none() && r.until.is_none(),
        }
    }
    fn working(&self, r: &Role, time: f64) -> bool {
        r.rest.is_none_or(|rest| {
            rest == 0. || (time - self.base + 6. * MIN).rem_euclid((3. + rest) * MIN) < 3. * MIN
        })
    }
    fn activity(
        &mut self,
        r: &Role,
        w: &Where,
        life: &Life,
        prompts: bool,
    ) -> rusqlite::Result<f64> {
        let mut last = life.start;
        if life.status == "queued" {
            return Ok(last);
        }
        if prompts {
            self.w.item(
                w,
                "user_message",
                "completed",
                life.start - 2000.,
                life.start - 2000.,
                json!({}),
            )?;
            if life.end - life.start > 25. * MIN {
                let t = life.start + ((life.end - life.start) * 0.55).round();
                self.w
                    .item(w, "user_message", "completed", t, t, json!({}))?;
            }
        }
        let period = r.every * 1000.;
        let offset = (random(&[&r.id, &w.run, "offset"]) * period).floor();
        let mut in_flight = false;
        if !r.work.is_empty() {
            let mut j = ((life.start - self.epoch - offset) / period).ceil() as i64;
            loop {
                let t = self.epoch + offset + j as f64 * period;
                if t > life.end {
                    break;
                }
                let key = j.to_string();
                if t >= life.start && self.working(r, t) {
                    let wave = 0.5
                        + 0.5
                            * ((2. * std::f64::consts::PI * t) / (13. * MIN)
                                + random(&[&r.id, "wave"]) * 2. * std::f64::consts::PI)
                                .sin();
                    if random(&[&r.id, &key, "skip"]) >= 0.08 + 0.32 * wave {
                        let spec = &r.work[(random(&[&r.id, &key, "pick"]) * r.work.len() as f64)
                            .floor() as usize];
                        let c = call(spec);
                        let share = if c.kind == "command_execution" {
                            0.4 + 0.45 * random(&[&r.id, &key, "dur"])
                        } else {
                            0.15 + 0.45 * random(&[&r.id, &key, "dur"])
                        };
                        let dur = (period * share).round();
                        let running = t + dur > life.end;
                        if !running || life.busy {
                            let status = if running {
                                "running"
                            } else if c.flaky && random(&[&r.id, &key, "fail"]) < 0.4 {
                                "failed"
                            } else {
                                "completed"
                            };
                            self.w.item(
                                w,
                                c.kind,
                                status,
                                t,
                                if running { t } else { t + dur },
                                c.payload,
                            )?;
                            in_flight |= running;
                            last = last.max(if running { t } else { t + dur });
                        }
                    }
                }
                j += 1;
            }
        }
        for tail in &r.tail {
            let minutes = tail.first().and_then(Value::as_f64).unwrap_or(0.);
            let spec = tail.get(1).and_then(Value::as_str).unwrap_or("");
            let status = tail.get(2).and_then(Value::as_str).unwrap_or("completed");
            let ran = tail.get(3).and_then(Value::as_f64).unwrap_or(0.);
            let t = self.at(minutes);
            if t > self.now {
                continue;
            }
            let mut c = call(spec);
            if c.kind == "user_input_request" && status == "pending" {
                c.payload["requestId"] = json!(format!("req-{}", r.id));
            }
            let open = status == "running" || status == "pending";
            let done = self.now.min(t + ran * MIN);
            self.w
                .item(w, c.kind, status, t, if open { t } else { done }, c.payload)?;
            in_flight |= open;
            last = last.max(if open { t } else { done });
        }
        if life.busy && !in_flight && self.working(r, self.now) {
            self.w
                .item(w, "reasoning", "running", last, last, json!({}))?;
        }
        Ok(last)
    }
    fn turns(
        &mut self,
        r: &Role,
        w: &Where,
        model: &str,
        max: f64,
        ctx: [f64; 2],
        life: &Life,
        last: f64,
        open: bool,
    ) -> rusqlite::Result<()> {
        if last <= life.start + 5000. {
            return Ok(());
        }
        let Some(provider) = w.provider.as_deref() else {
            return Ok(());
        };
        let anchor = self.base.min(last);
        let fill = |t: f64| {
            if t <= self.base {
                let u = ((t - life.start) / (anchor - life.start).max(MIN)).clamp(0., 1.);
                ctx[0] + (ctx[1] - ctx[0]) * (u - 0.05 * (2. * std::f64::consts::PI * u).sin())
            } else {
                ctx[1]
                    .max(0.97)
                    .min(ctx[1] + GROWTH * (t - self.base) / MIN)
            }
        };
        let period = r.every * 5000.;
        let offset = (random(&[&r.id, provider, "turns"]) * period).floor();
        let mut starts = vec![life.start + 5000.];
        let mut k = ((life.start + 30_000. - self.epoch - offset) / period).ceil() as i64;
        loop {
            let t = self.epoch + offset + k as f64 * period;
            if t >= last {
                break;
            }
            starts.push(t);
            k += 1;
        }
        let fam = if model.contains("claude") {
            0
        } else if model.contains("gpt") {
            1
        } else {
            2
        };
        let ttl = r.cache.filter(|n| *n != 0.).map(|n| n * MIN);
        for (k, t) in starts.iter().enumerate() {
            let latest = k == starts.len() - 1;
            let done = if latest { last } else { starts[k + 1] };
            let used = (fill(done) * max).round();
            let key = k.to_string();
            let output = (800. + 5200. * random(&[&r.id, &key, "out"])).round();
            let input = (used - output).max(0.);
            let cached = (input * [0.94, 0.82, 0.7][fam]).round();
            let reasoning = (output * if fam == 0 { 0.4 } else { 0.9 }).round();
            let calls = 3. + (random(&[&r.id, &key, "calls"]) * 7.).floor();
            let complete = !(latest && open);
            let mut usage = json!({"usedTokens":used,"maxTokens":max,"inputTokens":input,"cachedInputTokens":cached,"outputTokens":output,"reasoningOutputTokens":reasoning,"updatedAt":iso(done)});
            if let Some(ttl) = ttl {
                usage["promptCacheTtlMs"] = json!(ttl);
                usage["promptCacheExpiresAt"] = json!(iso(done + ttl));
            }
            let mut payload = json!({"providerThreadId":provider,"model":model,"status":if complete{"completed"}else{"running"},"tokenUsage":usage});
            if complete {
                payload["turnTokenUsage"] = json!({"inputTokens":input*calls,"cachedInputTokens":cached*calls,"outputTokens":output*2.,"reasoningOutputTokens":reasoning*2.,"usageStatus":"complete","hasSubagents":!r.subs.is_empty()});
            }
            self.w.row(&format!("{PREFIX}provider_turns"),json!({"thread_id":w.thread,"provider_thread_id":provider,"ordinal":k+1,"started_at":iso(*t),"completed_at":if complete{json!(iso(done))}else{Value::Null},"payload_json":payload}))?;
        }
        Ok(())
    }
    fn plan(&mut self, r: &Role, w: &Where, when: f64) -> rusqlite::Result<()> {
        let Some(plan) = &r.plan else {
            return Ok(());
        };
        let steps:Vec<Value>=plan.iter().map(|step|json!({"text":step.chars().skip(2).collect::<String>(),"status":if step.starts_with('✓'){"completed"}else if step.starts_with('▸'){"in_progress"}else{"pending"}})).collect();
        self.w.item(
            w,
            "todo_list",
            "completed",
            when,
            when,
            json!({"steps":steps}),
        )
    }
    fn work(&mut self, r: &Role, w: &Where, life: &Life, prompts: bool) -> rusqlite::Result<f64> {
        let last = self.activity(r, w, life, prompts)?;
        if let Some(ctx) = r.ctx {
            if life.status != "queued" {
                let asking = r.tail.iter().any(|tail| {
                    tail.get(1)
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.starts_with('@'))
                        && tail.get(2).and_then(Value::as_str) == Some("pending")
                });
                self.turns(
                    r,
                    w,
                    &r.model,
                    r.maximum,
                    ctx,
                    life,
                    last,
                    life.status == "running" && !asking,
                )?;
            }
        }
        self.plan(r, w, last.min(life.start + (last - life.start) * 0.8))?;
        Ok(last)
    }
    fn run_row(
        &self,
        r: &Role,
        w: &Where,
        life: &Life,
        ordinal: i32,
        model: &str,
    ) -> rusqlite::Result<()> {
        let requested = life.start - 1500.;
        self.w.row(&format!("{PREFIX}runs"),json!({"run_id":w.run,"thread_id":w.thread,"ordinal":ordinal,"requested_at":iso(requested),"completed_at":stamp(life.ended),"payload_json":{"id":w.run,"threadId":w.thread,"status":life.status,"providerThreadId":w.provider,"requestedAt":iso(requested),"startedAt":iso(life.start),"completedAt":stamp(life.ended),"modelSelection":{"model":model,"options":r.options}}}))
    }
    fn thread(&mut self, r: &Role, host: Option<&Host>) -> rusqlite::Result<()> {
        let pt = format!("pt-{}", r.id);
        let run = format!("run-{}", r.id);
        let mut ordinal = 1;
        if let Some(b) = &r.before {
            let before = Life {
                status: "completed".into(),
                start: self.at(b.from),
                ended: Some(self.at(b.to)),
                end: self.at(b.to),
                busy: false,
            };
            let w = Where {
                thread: r.id.clone(),
                run: format!("{run}-0"),
                provider: Some(format!("{pt}-0")),
            };
            self.w.row(&format!("{PREFIX}provider_threads"),json!({"provider_thread_id":w.provider,"thread_id":r.id,"payload_json":{"id":w.provider}}))?;
            self.run_row(r, &w, &before, ordinal, &b.model)?;
            ordinal += 1;
            let mut old = r.clone();
            old.tail.clear();
            let last = self.activity(&old, &w, &before, true)?;
            self.turns(r, &w, &b.model, b.maximum, b.ctx, &before, last, false)?;
        }
        let life = self.life(r);
        let w = Where {
            thread: r.id.clone(),
            run: run.clone(),
            provider: Some(pt.clone()),
        };
        self.w.row(
            &format!("{PREFIX}provider_threads"),
            json!({"provider_thread_id":pt,"thread_id":r.id,"payload_json":{"id":pt}}),
        )?;
        self.w.row(&format!("{PREFIX}nodes"),json!({"node_id":format!("node-{}",r.id),"thread_id":r.id,"parent_node_id":null,"run_id":run}))?;
        self.run_row(r, &w, &life, ordinal, &r.model)?;
        let last = self.work(r, &w, &life, host.is_none())?;
        for tail in &r.tail {
            let minutes = tail.first().and_then(Value::as_f64).unwrap_or(0.);
            let spec = tail.get(1).and_then(Value::as_str).unwrap_or("");
            let status = tail.get(2).and_then(Value::as_str).unwrap_or("");
            if !spec.starts_with('@') || status != "pending" || self.at(minutes) > self.now {
                continue;
            }
            self.w.row(&format!("{PREFIX}runtime_requests"),json!({"request_id":format!("req-{}",r.id),"thread_id":r.id,"node_id":null,"kind":"user_input","status":"pending","created_at":iso(self.at(minutes)),"resolved_at":null}))?;
        }
        let mut payload = json!({"id":r.id,"projectId":r.project,"title":r.title,"branch":r.branch,"activeProviderThreadId":pt,"modelSelection":{"model":r.model,"options":r.options},"updatedAt":iso(last),"settledOverride":if r.shelf.as_deref()==Some("settled"){json!("settled")}else{Value::Null},"snoozedUntil":if r.shelf.as_deref()==Some("snoozed"){json!(iso(self.now+14_400_000.))}else{Value::Null}});
        if let Some(host) = host {
            payload["lineage"] =
                json!({"relationshipToParent":"subagent","parentThreadId":host.thread});
        }
        self.w.row(
            &format!("{PREFIX}threads"),
            json!({"thread_id":r.id,"deleted_at":null,"archived_at":null,"payload_json":payload}),
        )?;
        if let Some(host) = host {
            self.w.row(&format!("{PREFIX}subagents"),json!({"subagent_id":format!("sub-{}",r.id),"thread_id":host.thread,"payload_json":{"id":format!("sub-{}",r.id),"threadId":host.thread,"parentNodeId":host.node,"runId":host.run,"childThreadId":r.id,"providerThreadId":pt,"status":life.status,"title":r.title,"model":r.model,"startedAt":iso(life.start),"completedAt":stamp(life.ended),"updatedAt":iso(last)}}))?;
        }
        for sub in &r.subs {
            let host = Host {
                thread: r.id.clone(),
                run: run.clone(),
                node: format!("node-{}", r.id),
            };
            if sub.child {
                let mut child = sub.clone();
                if child.project.is_none() {
                    child.project = r.project.clone();
                }
                self.thread(&child, Some(&host))?;
            } else {
                self.sub(sub, &host)?;
            }
        }
        Ok(())
    }
    fn sub(&mut self, r: &Role, host: &Host) -> rusqlite::Result<()> {
        let life = self.life(r);
        let pt = if r.remote.is_some() {
            None
        } else {
            Some(format!("pt-{}", r.id))
        };
        let mut last = life.ended.unwrap_or(life.start);
        if pt.is_some() {
            self.w.row(
                &format!("{PREFIX}provider_threads"),
                json!({"provider_thread_id":pt,"thread_id":host.thread,"payload_json":{"id":pt}}),
            )?;
            last = self.work(
                r,
                &Where {
                    thread: host.thread.clone(),
                    run: host.run.clone(),
                    provider: pt.clone(),
                },
                &life,
                false,
            )?;
        }
        self.w.row(&format!("{PREFIX}nodes"),json!({"node_id":format!("node-{}",r.id),"thread_id":host.thread,"parent_node_id":r.id,"run_id":host.run}))?;
        let mut payload = json!({"id":r.id,"threadId":host.thread,"parentNodeId":host.node,"runId":host.run,"providerThreadId":pt,"status":life.status,"title":r.title,"model":r.model,"startedAt":iso(life.start),"completedAt":stamp(life.ended),"updatedAt":iso(last)});
        if let Some(remote) = &r.remote {
            payload["remoteChild"] = json!({"label":remote});
        }
        self.w.row(
            &format!("{PREFIX}subagents"),
            json!({"subagent_id":r.id,"thread_id":host.thread,"payload_json":payload}),
        )?;
        for sub in &r.subs {
            self.sub(
                sub,
                &Host {
                    thread: host.thread.clone(),
                    run: host.run.clone(),
                    node: format!("node-{}", r.id),
                },
            )?;
        }
        Ok(())
    }
}
const SCHEMA:&str="
CREATE TABLE projection_projects(project_id TEXT PRIMARY KEY,title TEXT,workspace_root TEXT);
CREATE TABLE orchestration_v2_projection_threads(thread_id TEXT PRIMARY KEY,payload_json TEXT,deleted_at TEXT,archived_at TEXT);
CREATE TABLE orchestration_v2_projection_runs(run_id TEXT PRIMARY KEY,thread_id TEXT,ordinal INTEGER,payload_json TEXT,requested_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_subagents(subagent_id TEXT PRIMARY KEY,thread_id TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_nodes(node_id TEXT PRIMARY KEY,thread_id TEXT,parent_node_id TEXT,run_id TEXT);
CREATE TABLE orchestration_v2_projection_provider_threads(provider_thread_id TEXT PRIMARY KEY,thread_id TEXT,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_provider_turns(thread_id TEXT,provider_thread_id TEXT,ordinal INTEGER,payload_json TEXT,started_at TEXT,completed_at TEXT);
CREATE TABLE orchestration_v2_projection_turn_items(turn_item_id TEXT PRIMARY KEY,thread_id TEXT,run_id TEXT,node_id TEXT,provider_thread_id TEXT,type TEXT,status TEXT,updated_at TEXT,ordinal INTEGER,payload_json TEXT);
CREATE TABLE orchestration_v2_projection_runtime_requests(request_id TEXT PRIMARY KEY,thread_id TEXT,node_id TEXT,kind TEXT,status TEXT,created_at TEXT,resolved_at TEXT);";

pub fn snapshot(now: f64, epoch: f64) -> Result<Snapshot, SourceError> {
    source::synthetic_snapshot(&projection(now, epoch)?, now)
}

/// Read task history at the dashboard snapshot's point in the demo cycle.
pub fn trace(now: f64, epoch: f64, target: &Target) -> Result<Option<Trace>, SourceError> {
    trace::read_from(&projection(now, epoch)?, std::path::Path::new("synthetic:demo"), target, now)
}

fn projection(now: f64, epoch: f64) -> Result<Connection, SourceError> {
    if !now.is_finite() || !epoch.is_finite() {
        return Err(error("invalid demo clock"));
    }
    static SCENARIO: OnceLock<Result<Scenario, String>> = OnceLock::new();
    let scenario = SCENARIO
        .get_or_init(|| {
            serde_json::from_str(include_str!("../../../checks/fixtures/scenario.json"))
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(error)?;
    let db = Connection::open_in_memory().map_err(error)?;
    db.execute_batch("PRAGMA journal_mode=OFF;PRAGMA synchronous=OFF;")
        .map_err(error)?;
    db.execute_batch(SCHEMA).map_err(error)?;
    db.execute_batch("BEGIN").map_err(error)?;
    let mut scene = Scene {
        w: Writer {
            db: &db,
            ordinal: 0,
        },
        now,
        epoch,
        base: now - (now - epoch).max(0.).rem_euclid(CYCLE_MS),
    };
    for p in &scenario.projects {
        scene
            .w
            .row(
                "projection_projects",
                json!({"project_id":p.id,"title":p.title,"workspace_root":p.root}),
            )
            .map_err(error)?;
    }
    for role in &scenario.roles {
        scene.thread(role, None).map_err(error)?;
    }
    db.execute_batch("COMMIT").map_err(error)?;
    Ok(db)
}

#[cfg(test)]
mod tests {
    use super::*;
    const EPOCH: f64 = 1_794_484_800_000.;
    #[test]
    fn queue_transitions_and_twenty_minute_cycle_match_the_authored_scenario() {
        let initial = snapshot(EPOCH, EPOCH).unwrap();
        assert_eq!(initial.agents.len(), 25);
        assert_eq!(
            initial
                .agents
                .iter()
                .find(|a| a.id == "demo-headers")
                .unwrap()
                .status,
            "queued"
        );
        assert!(
            initial
                .agents
                .iter()
                .find(|a| a.id == "demo-search")
                .is_some_and(|a| a.requests == ["user_input"])
        );
        let started = snapshot(EPOCH + MIN, EPOCH).unwrap();
        assert_eq!(
            started
                .agents
                .iter()
                .find(|a| a.id == "demo-headers")
                .unwrap()
                .status,
            "running"
        );
        let later = snapshot(EPOCH + 2. * MIN, EPOCH).unwrap();
        assert!(
            later
                .agents
                .iter()
                .find(|a| a.id == "demo-billing")
                .unwrap()
                .context
                .used_tokens
                .unwrap()
                > initial
                    .agents
                    .iter()
                    .find(|a| a.id == "demo-billing")
                    .unwrap()
                    .context
                    .used_tokens
                    .unwrap()
        );
        let cycle = snapshot(EPOCH + CYCLE_MS, EPOCH).unwrap();
        assert_eq!(
            cycle
                .agents
                .iter()
                .find(|a| a.id == "demo-headers")
                .unwrap()
                .status,
            "queued"
        );
        assert_eq!(cycle.agents.len(), 25);
    }
    #[test]
    fn repeated_clock_is_deterministic_and_invalid_clock_is_an_error() {
        let a = serde_json::to_value(snapshot(EPOCH + 180_000., EPOCH).unwrap()).unwrap();
        let b = serde_json::to_value(snapshot(EPOCH + 180_000., EPOCH).unwrap()).unwrap();
        assert_eq!(a, b);
        assert!(snapshot(f64::NAN, EPOCH).is_err());
    }
}
