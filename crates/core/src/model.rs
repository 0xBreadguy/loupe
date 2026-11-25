//! Pure, deterministic dashboard model. No database, network, Rune or wall-clock access.
//! Tree/folding/scope rules are ported from oracle/src/source.ts at 5c590eb.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// FxHash's word mixing: a fast hasher for maps keyed by ids the program holds itself, where
/// SipHash's resistance to chosen keys buys nothing. Iteration order differs from SipHash's;
/// nothing that writes output iterates such a map without sorting it.
#[derive(Clone, Copy, Default)]
pub struct IdHasher(u64);

impl IdHasher {
    #[inline]
    fn add(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

impl std::hash::Hasher for IdHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            self.add(u64::from_le_bytes(chunk.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut word = [0u8; 8];
            word[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_le_bytes(word));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(u64::from(i));
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type IdHash = std::hash::BuildHasherDefault<IdHasher>;

pub const RECENT_MS: f64 = 3.0 * 3_600_000.0;
pub const RECENT_MAX: usize = 3;
pub const WINDOWS: [u32; 4] = [15, 30, 60, 180];

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Snapshot {
    /// Shared, so a new snapshot can reuse every agent that did not change.
    pub agents: Vec<std::sync::Arc<Agent>>,
    pub at: f64,
    pub path: String,
}
/// Which agents differ between one snapshot and the one before it. An incremental reader
/// reports this so the state can re-derive only what changed.
#[derive(Clone, Debug, PartialEq)]
pub enum Changed {
    /// Nothing is known about the difference: treat every agent as changed.
    Everything,
    /// Exactly these agent ids were added, removed or replaced. Every other agent is the
    /// same `Arc` as before, in a snapshot whose `agents` order may still have changed.
    Agents(Vec<String>),
}
impl Snapshot {
    /// One agent for editing in place. Copies it first if another snapshot still shares it.
    pub fn agent_mut(&mut self, index: usize) -> &mut Agent {
        std::sync::Arc::make_mut(&mut self.agents[index])
    }
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Context {
    pub used_tokens: Option<f64>,
    pub max_tokens: Option<f64>,
    pub input_tokens: Option<f64>,
    pub cached_input_tokens: Option<f64>,
    pub output_tokens: Option<f64>,
    pub reasoning_output_tokens: Option<f64>,
    pub prompt_cache_ttl_ms: Option<f64>,
    pub prompt_cache_expires_at: Option<String>,
    pub updated_at: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Item {
    pub thread_id: String,
    pub run_id: Option<String>,
    pub node_id: Option<String>,
    pub provider_thread_id: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub label: String,
    /// SQL yields null when a command input or file name is missing.
    /// The original's reader passes that null on and its view then throws on it; here a
    /// missing text is an empty one, which is also what the incremental reader keeps.
    #[serde(deserialize_with = "null_as_empty")]
    pub command: String,
    #[serde(deserialize_with = "null_as_empty")]
    pub file: String,
    pub tool: Option<String>,
    pub exit_code: Option<i32>,
}
fn null_as_empty<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Event {
    pub t: f64,
    pub cat: String,
    pub failed: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Point {
    pub pct: f64,
    pub used: f64,
    pub max: f64,
    pub at: f64,
    pub provider: String,
    pub model: String,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Step {
    pub text: String,
    pub status: String,
}
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Shelf {
    pub listed: bool,
    pub settled: bool,
    pub snoozed_until: Option<String>,
}
/// Pending, structured asks attributed to an agent. Runtime approvals stay in
/// `Agent::requests`; prose and card contents are outside this contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenAsks {
    pub blocking: u64,
    pub nonblocking: u64,
    pub cards: AskCards,
}

impl OpenAsks {
    pub fn total(&self) -> u64 {
        self.blocking + self.nonblocking + self.cards.secret + self.cards.link
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AskCards {
    pub secret: u64,
    pub link: u64,
}

/// Canonical 24-byte UTC timestamp parser (`YYYY-MM-DDTHH:MM:SS.mmmZ`), accepting only
/// text that `source::iso` prints back byte-for-byte.
pub(crate) fn parse_canonical_utc_ms(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let digits = |start: usize, len: usize| -> Option<i64> {
        let mut val = 0i64;
        for &ch in &b[start..start + len] {
            if !ch.is_ascii_digit() {
                return None;
            }
            val = val * 10 + i64::from(ch - b'0');
        }
        Some(val)
    };
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    let hour = digits(11, 2)?;
    let minute = digits(14, 2)?;
    let second = digits(17, 2)?;
    let ms = digits(20, 3)?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => return None,
    };
    if !(1..=max_day).contains(&day) {
        return None;
    }
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86_400_000 + hour * 3_600_000 + minute * 60_000 + second * 1_000 + ms)
}

/// Compact 16-byte timestamp representation: canonical UTC timestamps store only `i64`
/// milliseconds (zero heap allocation); any other string stays verbatim in an `Arc<String>`.
#[derive(Clone, Debug)]
pub enum IsoStamp {
    Ms(i64),
    Raw(std::sync::Arc<String>),
}

impl IsoStamp {
    pub fn parse(text: &str) -> Self {
        match parse_canonical_utc_ms(text) {
            Some(ms) => Self::Ms(ms),
            None => Self::Raw(std::sync::Arc::new(text.to_owned())),
        }
    }

    pub fn text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Ms(ms) => std::borrow::Cow::Owned(crate::source::iso(*ms as f64)),
            Self::Raw(s) => std::borrow::Cow::Borrowed(s.as_str()),
        }
    }
}

impl PartialEq for IsoStamp {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ms(a), Self::Ms(b)) => a == b,
            (Self::Raw(a), Self::Raw(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for IsoStamp {}

impl Serialize for IsoStamp {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Ms(ms) => s.serialize_str(&crate::source::iso(*ms as f64)),
            Self::Raw(raw) => s.serialize_str(raw.as_str()),
        }
    }
}

/// Exact JSON numeric scalar, keeping integer vs float serialization parity with `serde_json`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JsonNum {
    Uint(u64),
    Int(i64),
    Float(f64),
}

impl JsonNum {
    pub fn from_number(n: &serde_json::Number) -> Self {
        if let Some(u) = n.as_u64() {
            Self::Uint(u)
        } else if let Some(i) = n.as_i64() {
            Self::Int(i)
        } else {
            Self::Float(n.as_f64().unwrap_or(0.0))
        }
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Self::Uint(u) => u as f64,
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }
}

impl Serialize for JsonNum {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match *self {
            Self::Uint(u) => s.serialize_u64(u),
            Self::Int(i) => s.serialize_i64(i),
            Self::Float(f) => s.serialize_f64(f),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum OptionValue {
    Null,
    Bool(bool),
    Num(JsonNum),
    Str(std::sync::Arc<str>),
}

impl OptionValue {
    pub fn from_value(v: &Value) -> Option<Self> {
        Self::from_value_interned(v, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_value_interned(
        v: &Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Option<Self> {
        match v {
            Value::Null => Some(Self::Null),
            Value::Bool(b) => Some(Self::Bool(*b)),
            Value::Number(n) => Some(Self::Num(JsonNum::from_number(n))),
            Value::String(s) => Some(Self::Str(intern(s.as_str()))),
            Value::Array(_) | Value::Object(_) => None,
        }
    }

    pub fn from_value_owned(v: Value) -> Result<Self, Value> {
        match v {
            Value::Null => Ok(Self::Null),
            Value::Bool(b) => Ok(Self::Bool(b)),
            Value::Number(n) => Ok(Self::Num(JsonNum::from_number(&n))),
            Value::String(s) => Ok(Self::Str(std::sync::Arc::from(s))),
            other => Err(other),
        }
    }

    pub fn display(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Str(s) => std::borrow::Cow::Borrowed(s),
            Self::Null => std::borrow::Cow::Borrowed("null"),
            Self::Bool(true) => std::borrow::Cow::Borrowed("true"),
            Self::Bool(false) => std::borrow::Cow::Borrowed("false"),
            Self::Num(JsonNum::Uint(u)) => std::borrow::Cow::Owned(u.to_string()),
            Self::Num(JsonNum::Int(i)) => std::borrow::Cow::Owned(i.to_string()),
            Self::Num(JsonNum::Float(f)) => std::borrow::Cow::Owned(
                serde_json::Number::from_f64(*f)
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "null".into()),
            ),
        }
    }
}

impl Serialize for OptionValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => s.serialize_unit(),
            Self::Bool(b) => s.serialize_bool(*b),
            Self::Num(n) => n.serialize(s),
            Self::Str(st) => s.serialize_str(st),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentOption {
    Typed {
        id: std::sync::Arc<str>,
        value: OptionValue,
    },
    Raw(Box<Value>),
}

impl AgentOption {
    pub fn from_value(v: &Value) -> Self {
        Self::from_value_interned(v, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_value_interned(
        v: &Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Self {
        if let Value::Object(map) = v {
            if map.len() == 2 {
                if let (Some(Value::String(id)), Some(val)) = (map.get("id"), map.get("value")) {
                    if let Some(value) = OptionValue::from_value_interned(val, intern) {
                        return Self::Typed {
                            id: intern(id.as_str()),
                            value,
                        };
                    }
                }
            }
        }
        Self::Raw(Box::new(v.clone()))
    }

    pub fn from_value_owned(v: Value) -> Self {
        if let Value::Object(mut map) = v {
            if map.len() == 2
                && map.get("id").is_some_and(Value::is_string)
                && map.contains_key("value")
            {
                let id = match map.remove("id").unwrap() {
                    Value::String(s) => s,
                    _ => unreachable!(),
                };
                let val = map.remove("value").unwrap();
                match OptionValue::from_value_owned(val) {
                    Ok(value) => {
                        return Self::Typed {
                            id: std::sync::Arc::from(id),
                            value,
                        };
                    }
                    Err(val) => {
                        map.insert("id".into(), Value::String(id));
                        map.insert("value".into(), val);
                        return Self::Raw(Box::new(Value::Object(map)));
                    }
                }
            }
            return Self::Raw(Box::new(Value::Object(map)));
        }
        Self::Raw(Box::new(v))
    }

    pub fn id_and_display(&self) -> Option<(&str, std::borrow::Cow<'_, str>)> {
        match self {
            Self::Typed { id, value } => Some((id, value.display())),
            Self::Raw(v) => {
                let id = v.get("id").and_then(Value::as_str)?;
                let val = v.get("value")?;
                let shown = match val.as_str() {
                    Some(s) => std::borrow::Cow::Borrowed(s),
                    None => std::borrow::Cow::Owned(val.to_string()),
                };
                Some((id, shown))
            }
        }
    }
}

impl Serialize for AgentOption {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Typed { id, value } => {
                use serde::ser::SerializeMap;
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("id", id.as_ref())?;
                map.serialize_entry("value", value)?;
                map.end()
            }
            Self::Raw(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for AgentOption {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_value_owned(Value::deserialize(d)?))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedRemote {
    pub environment_id: Option<std::sync::Arc<str>>,
    pub thread_id: Option<std::sync::Arc<str>>,
    pub label: Option<std::sync::Arc<str>>,
    pub has_label: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AgentRemote {
    Projected(Box<ProjectedRemote>),
    Raw(Box<Value>),
}

impl AgentRemote {
    pub fn from_ref(env: &str, thread_id: &str) -> Self {
        Self::Projected(Box::new(ProjectedRemote {
            environment_id: Some(std::sync::Arc::from(env)),
            thread_id: Some(std::sync::Arc::from(thread_id)),
            label: None,
            has_label: false,
        }))
    }

    pub fn from_value(v: &Value) -> Self {
        Self::from_value_interned(v, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_value_interned(
        v: &Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Self {
        if let Value::Object(map) = v {
            let mut opt_str = |k: &str| -> Option<Option<std::sync::Arc<str>>> {
                match map.get(k)? {
                    Value::Null => Some(None),
                    Value::String(s) => Some(Some(intern(s.as_str()))),
                    _ => None,
                }
            };
            if map.len() == 2 {
                if let (Some(environment_id), Some(thread_id)) =
                    (opt_str("environmentId"), opt_str("threadId"))
                {
                    return Self::Projected(Box::new(ProjectedRemote {
                        environment_id,
                        thread_id,
                        label: None,
                        has_label: false,
                    }));
                }
            } else if map.len() == 3 {
                if let (Some(environment_id), Some(thread_id), Some(label)) = (
                    opt_str("environmentId"),
                    opt_str("threadId"),
                    opt_str("label"),
                ) {
                    return Self::Projected(Box::new(ProjectedRemote {
                        environment_id,
                        thread_id,
                        label,
                        has_label: true,
                    }));
                }
            }
        }
        Self::Raw(Box::new(v.clone()))
    }

    pub fn from_value_owned(v: Value) -> Self {
        match Self::from_value(&v) {
            Self::Raw(_) => Self::Raw(Box::new(v)),
            typed => typed,
        }
    }

    pub fn thread_ref(&self) -> Option<(&str, &str)> {
        match self {
            Self::Projected(p) => crate::integration::thread_ref_parts(
                p.environment_id.as_deref()?,
                p.thread_id.as_deref()?,
            ),
            Self::Raw(v) => crate::integration::thread_ref(v),
        }
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Projected(p) => p.label.as_deref(),
            Self::Raw(v) => v.get("label").and_then(Value::as_str),
        }
    }
}

impl Serialize for AgentRemote {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Projected(p) => {
                use serde::ser::SerializeMap;
                let mut map = s.serialize_map(Some(if p.has_label { 3 } else { 2 }))?;
                map.serialize_entry("environmentId", &p.environment_id.as_deref())?;
                if p.has_label {
                    map.serialize_entry("label", &p.label.as_deref())?;
                }
                map.serialize_entry("threadId", &p.thread_id.as_deref())?;
                map.end()
            }
            Self::Raw(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for AgentRemote {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_value_owned(Value::deserialize(d)?))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelSelection {
    pub model: Option<std::sync::Arc<str>>,
    pub options: Option<Box<[AgentOption]>>,
}

impl ModelSelection {
    pub fn from_value(v: &Value) -> Option<Self> {
        Self::from_value_interned(v, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_value_interned(
        v: &Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Option<Self> {
        let map = v.as_object()?;
        if map.len() != 2 {
            return None;
        }
        let model = match map.get("model")? {
            Value::Null => None,
            Value::String(s) => Some(intern(s.as_str())),
            _ => return None,
        };
        let options = match map.get("options")? {
            Value::Null => None,
            Value::Array(arr) => Some(
                arr.iter()
                    .map(|item| AgentOption::from_value_interned(item, intern))
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            ),
            _ => return None,
        };
        Some(Self { model, options })
    }
}

impl Serialize for ModelSelection {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("model", &self.model.as_deref())?;
        map.serialize_entry("options", &self.options.as_deref())?;
        map.end()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedRun {
    pub id: Option<std::sync::Arc<str>>,
    pub thread_id: Option<std::sync::Arc<str>>,
    pub status: Option<std::sync::Arc<str>>,
    pub provider_thread_id: Option<std::sync::Arc<str>>,
    pub started_at: Option<IsoStamp>,
    pub requested_at: Option<IsoStamp>,
    pub completed_at: Option<IsoStamp>,
    pub model_selection: Option<ModelSelection>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum AgentRun {
    #[default]
    Null,
    Empty,
    IdOnly(Option<std::sync::Arc<str>>),
    ProviderOnly(std::sync::Arc<str>),
    Projected(Box<ProjectedRun>),
    Raw(Box<Value>),
}

impl AgentRun {
    pub fn from_id_value(v: Option<&Value>) -> Self {
        match v {
            None | Some(Value::Null) => Self::IdOnly(None),
            Some(Value::String(s)) => Self::IdOnly(Some(std::sync::Arc::from(s.as_str()))),
            Some(other) => Self::Raw(Box::new(json!({ "id": other }))),
        }
    }

    pub fn from_value(v: &Value) -> Self {
        Self::from_value_interned(v, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_value_interned(
        v: &Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Object(map) => {
                if map.is_empty() {
                    return Self::Empty;
                }
                if map.len() == 1 {
                    if let Some(id) = map.get("id") {
                        return match id {
                            Value::Null => Self::IdOnly(None),
                            Value::String(s) => Self::IdOnly(Some(intern(s.as_str()))),
                            _ => Self::Raw(Box::new(v.clone())),
                        };
                    }
                    if let Some(Value::String(p)) = map.get("provider") {
                        return Self::ProviderOnly(intern(p.as_str()));
                    }
                } else if map.len() == 8 {
                    let opt_str =
                        |k: &str, intern: &mut dyn FnMut(&str) -> std::sync::Arc<str>| -> Option<Option<std::sync::Arc<str>>> {
                            match map.get(k)? {
                                Value::Null => Some(None),
                                Value::String(s) => Some(Some(intern(s.as_str()))),
                                _ => None,
                            }
                        };
                    let opt_stamp = |k: &str| -> Option<Option<IsoStamp>> {
                        match map.get(k)? {
                            Value::Null => Some(None),
                            Value::String(s) => Some(Some(IsoStamp::parse(s.as_str()))),
                            _ => None,
                        }
                    };
                    if let (
                        Some(id),
                        Some(thread_id),
                        Some(status),
                        Some(provider_thread_id),
                        Some(started_at),
                        Some(requested_at),
                        Some(completed_at),
                        Some(ms_val),
                    ) = (
                        opt_str("id", intern),
                        opt_str("threadId", intern),
                        opt_str("status", intern),
                        opt_str("providerThreadId", intern),
                        opt_stamp("startedAt"),
                        opt_stamp("requestedAt"),
                        opt_stamp("completedAt"),
                        map.get("modelSelection"),
                    ) {
                        let model_selection = match ms_val {
                            Value::Null => Some(None),
                            _ => ModelSelection::from_value_interned(ms_val, intern).map(Some),
                        };
                        if let Some(model_selection) = model_selection {
                            return Self::Projected(Box::new(ProjectedRun {
                                id,
                                thread_id,
                                status,
                                provider_thread_id,
                                started_at,
                                requested_at,
                                completed_at,
                                model_selection,
                            }));
                        }
                    }
                }
                Self::Raw(Box::new(v.clone()))
            }
            _ => Self::Raw(Box::new(v.clone())),
        }
    }

    pub fn from_value_owned(v: Value) -> Self {
        match Self::from_value(&v) {
            Self::Raw(_) => Self::Raw(Box::new(v)),
            typed => typed,
        }
    }

    pub fn from_value_owned_interned(
        v: Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Self {
        match Self::from_value_interned(&v, intern) {
            Self::Raw(_) => Self::Raw(Box::new(v)),
            typed => typed,
        }
    }

    pub fn id(&self) -> Option<&str> {
        match self {
            Self::IdOnly(id) => id.as_deref(),
            Self::Projected(r) => r.id.as_deref(),
            Self::Raw(v) => v.get("id").and_then(Value::as_str),
            Self::Null | Self::Empty | Self::ProviderOnly(_) => None,
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::Projected(r) => r.thread_id.as_deref(),
            Self::Raw(v) => v.get("threadId").and_then(Value::as_str),
            Self::Null | Self::Empty | Self::IdOnly(_) | Self::ProviderOnly(_) => None,
        }
    }

    pub fn started_at(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            Self::Projected(r) => r.started_at.as_ref().map(IsoStamp::text),
            Self::Raw(v) => v
                .get("startedAt")
                .and_then(Value::as_str)
                .map(std::borrow::Cow::Borrowed),
            Self::Null | Self::Empty | Self::IdOnly(_) | Self::ProviderOnly(_) => None,
        }
    }

    pub fn provider(&self) -> Option<&str> {
        match self {
            Self::ProviderOnly(p) => Some(p.as_ref()),
            Self::Raw(v) => v.get("provider").and_then(Value::as_str),
            Self::Null | Self::Empty | Self::IdOnly(_) | Self::Projected(_) => None,
        }
    }

    /// `{"provider": run.provider}` when the run has that key, else `{}`, as
    /// `live::status_view` built it from the JSON run.
    pub fn into_status_view(&self) -> Self {
        match self {
            Self::ProviderOnly(p) => Self::ProviderOnly(std::sync::Arc::clone(p)),
            Self::Raw(v) => match v.get("provider") {
                Some(Value::String(p)) => Self::ProviderOnly(std::sync::Arc::from(p.as_str())),
                Some(other) => Self::Raw(Box::new(json!({ "provider": other }))),
                None => Self::Empty,
            },
            Self::Null | Self::Empty | Self::IdOnly(_) | Self::Projected(_) => Self::Empty,
        }
    }

    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).expect("AgentRun serializes")
    }
}

impl Serialize for AgentRun {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::Null => s.serialize_unit(),
            Self::Empty => s.serialize_map(Some(0))?.end(),
            Self::IdOnly(id) => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("id", &id.as_deref())?;
                map.end()
            }
            Self::ProviderOnly(provider) => {
                let mut map = s.serialize_map(Some(1))?;
                map.serialize_entry("provider", provider.as_ref())?;
                map.end()
            }
            Self::Projected(r) => {
                let mut map = s.serialize_map(Some(8))?;
                map.serialize_entry("completedAt", &r.completed_at)?;
                map.serialize_entry("id", &r.id.as_deref())?;
                map.serialize_entry("modelSelection", &r.model_selection)?;
                map.serialize_entry("providerThreadId", &r.provider_thread_id.as_deref())?;
                map.serialize_entry("requestedAt", &r.requested_at)?;
                map.serialize_entry("startedAt", &r.started_at)?;
                map.serialize_entry("status", &r.status.as_deref())?;
                map.serialize_entry("threadId", &r.thread_id.as_deref())?;
                map.end()
            }
            Self::Raw(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for AgentRun {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_value_owned(Value::deserialize(d)?))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypedTokenUsage {
    pub cached_input_tokens: Option<JsonNum>,
    pub has_subagents: Option<bool>,
    pub input_tokens: Option<JsonNum>,
    pub max_tokens: Option<JsonNum>,
    pub output_tokens: Option<JsonNum>,
    pub prompt_cache_expires_at: Option<std::sync::Arc<str>>,
    pub prompt_cache_ttl_ms: Option<JsonNum>,
    pub reasoning_output_tokens: Option<JsonNum>,
    pub reasoning_tokens: Option<JsonNum>,
    pub updated_at: Option<std::sync::Arc<str>>,
    pub usage_status: Option<std::sync::Arc<str>>,
    pub used_tokens: Option<JsonNum>,
    /// Bitmask of the 12 known keys above that were explicitly present with JSON `null`.
    pub null_mask: u16,
    /// Additional scalar keys (sorted lexicographically) preserved without `Value`.
    pub extras: Option<Box<[(std::sync::Arc<str>, OptionValue)]>>,
}

impl TypedTokenUsage {
    pub fn from_map(map: &serde_json::Map<String, Value>) -> Option<Self> {
        Self::from_map_interned(map, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_map_interned(
        map: &serde_json::Map<String, Value>,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Option<Self> {
        let mut out = Self::default();
        let mut extras: Vec<(std::sync::Arc<str>, OptionValue)> = Vec::new();
        for (k, v) in map {
            match k.as_str() {
                "cachedInputTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 0,
                    Value::Number(n) => out.cached_input_tokens = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                "hasSubagents" => match v {
                    Value::Null => out.null_mask |= 1 << 1,
                    Value::Bool(b) => out.has_subagents = Some(*b),
                    _ => return None,
                },
                "inputTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 2,
                    Value::Number(n) => out.input_tokens = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                "maxTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 3,
                    Value::Number(n) => out.max_tokens = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                "outputTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 4,
                    Value::Number(n) => out.output_tokens = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                "promptCacheExpiresAt" => match v {
                    Value::Null => out.null_mask |= 1 << 5,
                    Value::String(s) => {
                        out.prompt_cache_expires_at = Some(std::sync::Arc::from(s.as_str()))
                    }
                    _ => return None,
                },
                "promptCacheTtlMs" => match v {
                    Value::Null => out.null_mask |= 1 << 6,
                    Value::Number(n) => out.prompt_cache_ttl_ms = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                "reasoningOutputTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 7,
                    Value::Number(n) => {
                        out.reasoning_output_tokens = Some(JsonNum::from_number(n))
                    }
                    _ => return None,
                },
                "reasoningTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 8,
                    Value::Number(n) => out.reasoning_tokens = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                "updatedAt" => match v {
                    Value::Null => out.null_mask |= 1 << 9,
                    Value::String(s) => out.updated_at = Some(std::sync::Arc::from(s.as_str())),
                    _ => return None,
                },
                "usageStatus" => match v {
                    Value::Null => out.null_mask |= 1 << 10,
                    Value::String(s) => out.usage_status = Some(intern(s.as_str())),
                    _ => return None,
                },
                "usedTokens" => match v {
                    Value::Null => out.null_mask |= 1 << 11,
                    Value::Number(n) => out.used_tokens = Some(JsonNum::from_number(n)),
                    _ => return None,
                },
                other => {
                    extras.push((intern(other), OptionValue::from_value(v)?));
                }
            }
        }
        if !extras.is_empty() {
            extras.sort_by(|a, b| a.0.cmp(&b.0));
            out.extras = Some(extras.into_boxed_slice());
        }
        Some(out)
    }
}

impl Serialize for TypedTokenUsage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let extras = self.extras.as_deref().unwrap_or(&[]);
        let mut map = s.serialize_map(None)?;
        let mut ei = 0usize;
        let mut flush_before = |map: &mut S::SerializeMap, key: &str| -> Result<(), S::Error> {
            while ei < extras.len() && extras[ei].0.as_ref() < key {
                map.serialize_entry(extras[ei].0.as_ref(), &extras[ei].1)?;
                ei += 1;
            }
            Ok(())
        };
        macro_rules! emit_opt {
            ($name:literal, $bit:expr, $val:expr) => {
                if $val.is_some() || (self.null_mask & (1 << $bit)) != 0 {
                    flush_before(&mut map, $name)?;
                    map.serialize_entry($name, &$val)?;
                }
            };
        }
        emit_opt!("cachedInputTokens", 0, self.cached_input_tokens);
        emit_opt!("hasSubagents", 1, self.has_subagents);
        emit_opt!("inputTokens", 2, self.input_tokens);
        emit_opt!("maxTokens", 3, self.max_tokens);
        emit_opt!("outputTokens", 4, self.output_tokens);
        emit_opt!(
            "promptCacheExpiresAt",
            5,
            self.prompt_cache_expires_at.as_deref()
        );
        emit_opt!("promptCacheTtlMs", 6, self.prompt_cache_ttl_ms);
        emit_opt!("reasoningOutputTokens", 7, self.reasoning_output_tokens);
        emit_opt!("reasoningTokens", 8, self.reasoning_tokens);
        emit_opt!("updatedAt", 9, self.updated_at.as_deref());
        emit_opt!("usageStatus", 10, self.usage_status.as_deref());
        emit_opt!("usedTokens", 11, self.used_tokens);
        while ei < extras.len() {
            map.serialize_entry(extras[ei].0.as_ref(), &extras[ei].1)?;
            ei += 1;
        }
        map.end()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TurnUsage {
    Null,
    Typed(Box<TypedTokenUsage>),
    Raw(Box<Value>),
}

impl TurnUsage {
    pub fn from_value(v: &Value) -> Self {
        Self::from_value_interned(v, &mut |s: &str| std::sync::Arc::from(s))
    }

    pub fn from_value_interned(
        v: &Value,
        intern: &mut impl FnMut(&str) -> std::sync::Arc<str>,
    ) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Object(map) => match TypedTokenUsage::from_map_interned(map, intern) {
                Some(typed) => Self::Typed(Box::new(typed)),
                None => Self::Raw(Box::new(v.clone())),
            },
            _ => Self::Raw(Box::new(v.clone())),
        }
    }

    pub fn as_obj(&self) -> Option<TurnUsageRef<'_>> {
        match self {
            Self::Typed(u) => Some(TurnUsageRef::Typed(u)),
            Self::Raw(v) if v.is_object() => Some(TurnUsageRef::Raw(v)),
            _ => None,
        }
    }
}

impl Serialize for TurnUsage {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => s.serialize_unit(),
            Self::Typed(u) => u.serialize(s),
            Self::Raw(v) => v.serialize(s),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TurnUsageRef<'a> {
    Typed(&'a TypedTokenUsage),
    Raw(&'a Value),
}

impl<'a> TurnUsageRef<'a> {
    pub fn input_tokens(self) -> f64 {
        match self {
            Self::Typed(u) => u.input_tokens.map(JsonNum::as_f64).unwrap_or(0.0),
            Self::Raw(v) => v.get("inputTokens").and_then(Value::as_f64).unwrap_or(0.0),
        }
    }

    pub fn cached_input_tokens(self) -> f64 {
        match self {
            Self::Typed(u) => u.cached_input_tokens.map(JsonNum::as_f64).unwrap_or(0.0),
            Self::Raw(v) => v
                .get("cachedInputTokens")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        }
    }

    pub fn output_tokens(self) -> f64 {
        match self {
            Self::Typed(u) => u.output_tokens.map(JsonNum::as_f64).unwrap_or(0.0),
            Self::Raw(v) => v.get("outputTokens").and_then(Value::as_f64).unwrap_or(0.0),
        }
    }

    pub fn reasoning_tokens(self) -> Option<f64> {
        match self {
            Self::Typed(u) => u.reasoning_tokens.map(JsonNum::as_f64),
            Self::Raw(v) => v.get("reasoningTokens").and_then(Value::as_f64),
        }
    }

    pub fn reasoning_output_tokens(self) -> f64 {
        match self {
            Self::Typed(u) => u
                .reasoning_output_tokens
                .map(JsonNum::as_f64)
                .unwrap_or(0.0),
            Self::Raw(v) => v
                .get("reasoningOutputTokens")
                .and_then(Value::as_f64)
                .unwrap_or(0.0),
        }
    }

    pub fn usage_status(self) -> Option<&'a str> {
        match self {
            Self::Typed(u) => u.usage_status.as_deref(),
            Self::Raw(v) => v.get("usageStatus").and_then(Value::as_str),
        }
    }

    pub fn has_subagents(self) -> bool {
        match self {
            Self::Typed(u) => u.has_subagents == Some(true),
            Self::Raw(v) => v.get("hasSubagents").and_then(Value::as_bool) == Some(true),
        }
    }

    pub fn prompt_cache_expires_at(self) -> Option<&'a str> {
        match self {
            Self::Typed(u) => u.prompt_cache_expires_at.as_deref(),
            Self::Raw(v) => v.get("promptCacheExpiresAt").and_then(Value::as_str),
        }
    }

    pub fn prompt_cache_ttl_ms(self) -> Option<f64> {
        match self {
            Self::Typed(u) => u.prompt_cache_ttl_ms.map(JsonNum::as_f64),
            Self::Raw(v) => v.get("promptCacheTtlMs").and_then(Value::as_f64),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectedTurn {
    pub provider_thread_id: Option<std::sync::Arc<str>>,
    pub model: Option<std::sync::Arc<str>>,
    pub token_usage: TurnUsage,
    pub turn_token_usage: TurnUsage,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum AgentTurn {
    #[default]
    Null,
    Empty,
    Projected(Box<ProjectedTurn>),
    Raw(Box<Value>),
}

impl AgentTurn {
    pub fn from_value(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Object(map) => {
                if map.is_empty() {
                    return Self::Empty;
                }
                if map.len() == 4 {
                    let opt_str = |k: &str| -> Option<Option<std::sync::Arc<str>>> {
                        match map.get(k)? {
                            Value::Null => Some(None),
                            Value::String(s) => Some(Some(std::sync::Arc::from(s.as_str()))),
                            _ => None,
                        }
                    };
                    if let (
                        Some(provider_thread_id),
                        Some(model),
                        Some(token_usage),
                        Some(turn_token_usage),
                    ) = (
                        opt_str("providerThreadId"),
                        opt_str("model"),
                        map.get("tokenUsage"),
                        map.get("turnTokenUsage"),
                    ) {
                        return Self::Projected(Box::new(ProjectedTurn {
                            provider_thread_id,
                            model,
                            token_usage: TurnUsage::from_value(token_usage),
                            turn_token_usage: TurnUsage::from_value(turn_token_usage),
                        }));
                    }
                }
                Self::Raw(Box::new(v.clone()))
            }
            _ => Self::Raw(Box::new(v.clone())),
        }
    }

    pub fn from_value_owned(v: Value) -> Self {
        match Self::from_value(&v) {
            Self::Raw(_) => Self::Raw(Box::new(v)),
            typed => typed,
        }
    }

    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Projected(t) => t.model.as_deref(),
            Self::Raw(v) => v.get("model").and_then(Value::as_str),
            Self::Null | Self::Empty => None,
        }
    }

    pub fn token_usage_obj(&self) -> Option<TurnUsageRef<'_>> {
        match self {
            Self::Projected(t) => t.token_usage.as_obj(),
            Self::Raw(v) => v
                .get("tokenUsage")
                .filter(|u| u.is_object())
                .map(TurnUsageRef::Raw),
            Self::Null | Self::Empty => None,
        }
    }

    pub fn turn_token_usage_obj(&self) -> Option<TurnUsageRef<'_>> {
        match self {
            Self::Projected(t) => t.turn_token_usage.as_obj(),
            Self::Raw(v) => v
                .get("turnTokenUsage")
                .filter(|u| u.is_object())
                .map(TurnUsageRef::Raw),
            Self::Null | Self::Empty => None,
        }
    }
}

impl Serialize for AgentTurn {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            Self::Null => s.serialize_unit(),
            Self::Empty => s.serialize_map(Some(0))?.end(),
            Self::Projected(t) => {
                let mut map = s.serialize_map(Some(4))?;
                map.serialize_entry("model", &t.model.as_deref())?;
                map.serialize_entry("providerThreadId", &t.provider_thread_id.as_deref())?;
                map.serialize_entry("tokenUsage", &t.token_usage)?;
                map.serialize_entry("turnTokenUsage", &t.turn_token_usage)?;
                map.end()
            }
            Self::Raw(v) => v.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for AgentTurn {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_value_owned(Value::deserialize(d)?))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Agent {
    pub id: String,
    pub parent: Option<String>,
    pub title: String,
    pub kind: String,
    pub project: String,
    pub cwd: String,
    pub model: String,
    pub status: String,
    pub run: AgentRun,
    pub provider_id: Option<String>,
    pub context: Context,
    pub turn: AgentTurn,
    pub updated: Option<String>,
    pub started: Option<String>,
    pub ended: Option<String>,
    pub remote: Option<AgentRemote>,
    #[serde(rename = "referenceOnly", skip_serializing_if = "Option::is_none")]
    pub reference_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_ids: Option<Vec<String>>,
    pub shelf: Option<Shelf>,
    pub items: Vec<Item>,
    #[serde(rename = "recentFailures")]
    pub recent_failures: Vec<Item>,
    pub requests: Vec<String>,
    // Internal reader/model contract. Keep the legacy snapshot wire shape.
    #[serde(skip)]
    pub open_asks: OpenAsks,
    /// Estimated prose asks owned by this thread (not part of legacy snapshots).
    #[serde(skip)]
    pub estimated_asks: u64,
    /// Storage identity for a local thread projection row, including linked local children.
    #[serde(skip)]
    pub storage: Option<ThreadStorage>,
    pub asked: Option<String>,
    pub thread_id: String,
    pub branch: Option<String>,
    pub options: Vec<AgentOption>,
    pub history: Vec<Point>,
    pub events: Vec<Event>,
    pub spans: Vec<(f64, Option<f64>)>,
    pub plan: Option<Vec<Step>>,
    pub mix: indexmap::IndexMap<String, u64>,
    pub failures: u64,
    pub now: Option<Item>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadStorage {
    pub project_id: String,
    pub project_root: Option<String>,
    /// The thread's explicit checkout. The display cwd may fall back to the project root.
    pub worktree: Option<String>,
}

/// Milliseconds for a timestamp, read the way `Date.parse` in Node reads the ECMAScript
/// date-time format: `YYYY[-MM[-DD]]`, then optionally `T`, `t` or a space and
/// `HH:mm[:ss[.fraction]]`, then optionally `Z`, `z`, `±HH:mm` or `±HHmm`. A date alone is
/// UTC; a date and time without a zone is local time. Fractions truncate to milliseconds and
/// a day past the month's end rolls over, as Node does. The legacy forms Node also accepts
/// ("Nov 12 2026") are not timestamps T3 writes and stay unknown.
pub fn ms(stamp: Option<&str>) -> Option<f64> {
    let s = stamp?.as_bytes();
    let digits = |at: usize, n: usize| -> Option<i64> {
        let part = s.get(at..at + n)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        std::str::from_utf8(part).ok()?.parse().ok()
    };
    let (year, mut i) = match *s.first()? {
        sign @ (b'+' | b'-') => {
            let year = digits(1, 6)?;
            if sign == b'-' && year == 0 {
                return None;
            }
            (if sign == b'-' { -year } else { year }, 7)
        }
        _ => (digits(0, 4)?, 4),
    };
    let (mut month, mut day) = (1, 1);
    if s.get(i) == Some(&b'-') {
        month = digits(i + 1, 2)?;
        i += 3;
        if s.get(i) == Some(&b'-') {
            day = digits(i + 1, 2)?;
            i += 3;
        }
    }
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (mut hour, mut minute, mut second, mut milli) = (0, 0, 0, 0);
    let timed = matches!(s.get(i), Some(b'T' | b't' | b' '));
    if timed {
        hour = digits(i + 1, 2)?;
        if s.get(i + 3) != Some(&b':') {
            return None;
        }
        minute = digits(i + 4, 2)?;
        i += 6;
        if s.get(i) == Some(&b':') {
            second = digits(i + 1, 2)?;
            i += 3;
            if s.get(i) == Some(&b'.') {
                let start = i + 1;
                let mut end = start;
                while s.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
                if end == start {
                    return None;
                }
                for k in start..start + 3 {
                    milli = milli * 10 + if k < end { i64::from(s[k] - b'0') } else { 0 };
                }
                i = end;
            }
        }
        if hour > 24
            || minute > 59
            || second > 59
            || (hour == 24 && (minute != 0 || second != 0 || milli != 0))
        {
            return None;
        }
    }
    let offset = match s.get(i).copied() {
        None => None,
        Some(b'Z' | b'z') if timed => {
            i += 1;
            Some(0)
        }
        Some(sign @ (b'+' | b'-')) if timed => {
            let hours = digits(i + 1, 2)?;
            let (minutes, used) = if s.get(i + 3) == Some(&b':') {
                (digits(i + 4, 2)?, 6)
            } else {
                (digits(i + 3, 2)?, 5)
            };
            if hours > 23 || minutes > 59 {
                return None;
            }
            i += used;
            Some((hours * 3600 + minutes * 60) * if sign == b'+' { 1 } else { -1 })
        }
        Some(_) => return None,
    };
    if i != s.len() {
        return None;
    }
    let seconds = match offset {
        Some(offset) => civil_seconds(year, month, day, hour, minute, second) - offset,
        None if !timed => civil_seconds(year, month, day, hour, minute, second),
        None => local_seconds(year, month, day, hour, minute, second)?,
    };
    Some((seconds * 1000 + milli) as f64)
}
/// Seconds since the epoch for a UTC civil time. Days and hours past their range roll over.
fn civil_seconds(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468 + day - 1;
    days * 86400 + hour * 3600 + minute * 60 + second
}
/// Seconds since the epoch for a civil time in the process's local zone.
fn local_seconds(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> Option<i64> {
    #[cfg(unix)]
    {
        // SAFETY: an all-zero `tm` is a valid value; every field mktime reads is set below.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = i32::try_from(year - 1900).ok()?;
        tm.tm_mon = (month - 1) as i32;
        tm.tm_mday = day as i32;
        tm.tm_hour = hour as i32;
        tm.tm_min = minute as i32;
        tm.tm_sec = second as i32;
        tm.tm_isdst = -1;
        // SAFETY: `tm` is a live, initialized value; mktime normalizes it in place.
        Some(i64::from(unsafe { libc::mktime(&mut tm) }))
    }
    #[cfg(not(unix))]
    {
        Some(civil_seconds(year, month, day, hour, minute, second))
    }
}
pub fn live(a: &Agent) -> bool {
    matches!(
        a.status.as_str(),
        "running" | "starting" | "preparing" | "queued" | "waiting" | "pending" | "active"
    )
}
fn has_pending_request(a: &Agent) -> bool {
    !a.requests.is_empty() || a.open_asks != OpenAsks::default()
}
/// Attention can coexist with work on a nonblocking ask.
pub fn wants_attention(a: &Agent) -> bool {
    has_pending_request(a) || a.status == "waiting"
}
/// The legacy waiting view and the running view are mutually exclusive.
pub fn needs_you(a: &Agent) -> bool {
    blocks_activity(a) || (!live(a) && a.open_asks.cards.link > 0)
}

/// Keep the legacy approval/waiting indicator alongside the exact question and card counts.
pub(crate) fn pending_count(a: &Agent) -> u64 {
    let exact = a.open_asks.total();
    if a.open_asks.blocking == 0 {
        exact + u64::from(!a.requests.is_empty() || (exact == 0 && a.status == "waiting"))
    } else {
        exact + u64::from(a.requests.iter().any(|kind| kind != "user_input"))
    }
}

pub(crate) fn waiting_since(a: &Agent) -> Option<f64> {
    let item = a.items.iter().find(|i| {
        category(&i.kind, i.tool.as_deref()) == "ask"
            && matches!(
                i.status.as_str(),
                "running" | "starting" | "preparing" | "queued" | "waiting" | "pending" | "active"
            )
    });
    ms(a.asked.as_deref())
        .or_else(|| item.and_then(|i| ms(i.started_at.as_deref())))
        .or_else(|| item.and_then(|i| ms(Some(&i.updated_at))))
        .or_else(|| ms(a.updated.as_deref()))
}

/// Use the existing question label for the legacy single blocking presentation.
pub(crate) fn shows_blocking_now(a: &Agent) -> bool {
    a.open_asks.blocking == 1 && a.open_asks.total() == 1
}

pub(crate) fn shows_recent_activity(a: &Agent, item: &Item) -> bool {
    match item.kind.as_str() {
        "reasoning" | "assistant_message" => false,
        "user_input_request" => a.open_asks.nonblocking == 0,
        "secret_request" => a.open_asks.cards.secret == 0,
        "link_request" => a.open_asks.cards.link == 0,
        _ => true,
    }
}
pub fn blocks_activity(a: &Agent) -> bool {
    !a.requests.is_empty()
        || a.status == "waiting"
        || a.open_asks.blocking > 0
        || a.open_asks.cards.secret > 0
}
pub fn running(a: &Agent) -> bool {
    live(a) && !blocks_activity(a)
}
pub fn last_seen(a: &Agent) -> f64 {
    ms(a.ended.as_deref()).unwrap_or_else(|| {
        ms(a.updated.as_deref())
            .unwrap_or(0.0)
            .max(ms(a.started.as_deref()).unwrap_or(0.0))
    })
}
pub fn in_sidebar(a: &Agent, now: f64) -> bool {
    a.shelf.as_ref().is_none_or(|s| {
        s.listed
            && !s.settled
            && !(ms(s.snoozed_until.as_deref()).is_some_and(|until| until > now) && !wants_attention(a))
    })
}
pub fn category(kind: &str, tool: Option<&str>) -> &'static str {
    match kind {
        "command_execution" => "shell",
        "file_change" => "edit",
        "web_search" => "web",
        "subagent" | "handoff" => "agent",
        "user_input_request" | "secret_request" | "link_request" => "ask",
        "reasoning" => "think",
        "assistant_message" => "reply",
        "user_message" => "you",
        "notification" => "note",
        "dynamic_tool" => {
            let t = tool.unwrap_or("").to_lowercase();
            if matches!(
                t.as_str(),
                "read" | "glob" | "grep" | "ls" | "view" | "view_image" | "notebookread"
            ) {
                "read"
            } else if ["edit", "write", "patch"].iter().any(|p| t.contains(p)) {
                "edit"
            } else if ["web", "fetch", "search", "browse"]
                .iter()
                .any(|p| t.contains(p))
            {
                "web"
            } else if ["agent", "task", "delegate", "spawn"]
                .iter()
                .any(|p| t.starts_with(p))
            {
                "agent"
            } else {
                "tool"
            }
        }
        _ => "other",
    }
}
/// The glyph a category is drawn with (`view.ts`'s `GLYPH[cat] ?? "·"`).
pub fn glyph(cat: &str) -> char {
    match cat {
        "shell" => '❯',
        "read" => '◎',
        "edit" => '±',
        "web" => '⊕',
        "agent" => '⇉',
        "ask" => '◆',
        "think" => '∴',
        "reply" => '≡',
        "you" => '»',
        "note" => '•',
        "tool" => '◈',
        _ => '·',
    }
}
/// `view.ts`'s `ratio`: context used over its window, clamped, or `None` when it is unknown.
/// A NaN ratio stays NaN, as `Math.max(0, Math.min(1, NaN))` does.
pub fn ratio(used: Option<f64>, max: Option<f64>) -> Option<f64> {
    let (used, max) = (used?, max?);
    if max > 0.0 {
        Some((used / max).clamp(0.0, 1.0))
    } else {
        None
    }
}
impl Item {
    /// `view.ts`'s `itemLabel`. `home` is `process.env.HOME` as the program read it at startup
    /// (empty when unset); it shortens a leading command or file name and the first occurrence
    /// anywhere in any other label. Tidying happens first, so an empty or blank first line
    /// falls back to the type's words, and an unlabelled entry shows its type with spaces.
    pub fn display(&self, home: &str) -> String {
        let fallback = |text: &str, otherwise: &str| {
            if text.is_empty() {
                otherwise.to_owned()
            } else {
                text.to_owned()
            }
        };
        match self.kind.as_str() {
            "command_execution" => fallback(&tidy(&self.command, home), "command"),
            "file_change" => fallback(&tidy(&self.file, home), "file change"),
            "reasoning" => "thinking".into(),
            "assistant_message" => "writing reply".into(),
            _ => {
                let label = replace_first(&tidy(&self.label, home), home, "~");
                fallback(&label, &self.kind.replace('_', " "))
            }
        }
    }
}
/// `view.ts`'s `tidy`: the first line, cleaned, its leading home folder shortened to `~`, then
/// trimmed. With an empty home the original tests `/^$/`, which matches only an empty line and
/// turns it into `~`; the trim then leaves that alone.
pub fn tidy(s: &str, home: &str) -> String {
    let line = clean(s.split('\n').next().unwrap_or(""));
    let line = if home.is_empty() {
        if line.is_empty() {
            "~".to_owned()
        } else {
            line
        }
    } else {
        match line.strip_prefix(home) {
            Some(rest) => format!("~{rest}"),
            None => line,
        }
    };
    js_trim(&line).to_owned()
}
/// `String.prototype.replace(searchString, replacement)` with a string search: the first
/// occurrence only, literally, and an empty search inserts at the start.
pub fn replace_first(s: &str, search: &str, replacement: &str) -> String {
    if search.is_empty() {
        return format!("{replacement}{s}");
    }
    match s.find(search) {
        Some(at) => format!("{}{replacement}{}", &s[..at], &s[at + search.len()..]),
        None => s.to_owned(),
    }
}
/// `String.prototype.trim`: JavaScript's whitespace, not Rust's.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(crate::terminal::is_js_whitespace)
}
pub fn clean(s: &str) -> String {
    crate::terminal::clean(s)
}
#[derive(Clone, Debug, Default)]
pub struct Pace {
    pub tool_ms: f64,
    pub quiet_ms: f64,
    #[allow(dead_code)] // Semantic oracle exposes this diagnostic.
    pub delegating: bool,
    pub quiet: bool,
    pub stalled: bool,
}
pub fn pace(a: &Agent, now: f64, children_live: bool) -> Option<Pace> {
    pace_with_last(a, now, children_live, None)
}
pub(crate) fn pace_with_last(
    a: &Agent,
    now: f64,
    children_live: bool,
    known_last: Option<f64>,
) -> Option<Pace> {
    if blocks_activity(a) || !matches!(a.status.as_str(), "running" | "active") {
        return None;
    }
    if let Some(tool) = a
        .now
        .as_ref()
        .filter(|t| !matches!(t.kind.as_str(), "reasoning" | "assistant_message"))
    {
        let tool_ms = (now
            - ms(tool.started_at.as_deref())
                .or_else(|| ms(Some(&tool.updated_at)))
                .unwrap_or(now))
        .max(0.0);
        let delegating = category(&tool.kind, tool.tool.as_deref()) == "agent";
        return Some(Pace {
            tool_ms,
            delegating,
            stalled: !delegating && tool_ms >= 300_000.0,
            ..Pace::default()
        });
    }
    let last = known_last.unwrap_or_else(|| pace_last(a));
    let quiet_ms = if a.now.is_some() || children_live || last == 0.0 {
        0.0
    } else {
        (now - last).max(0.0)
    };
    Some(Pace {
        quiet_ms,
        delegating: children_live,
        quiet: quiet_ms >= 120_000.,
        stalled: quiet_ms >= 480_000.,
        ..Pace::default()
    })
}
pub(crate) fn pace_last(a: &Agent) -> f64 {
    let mut last = ms(a.started.as_deref())
        .unwrap_or(0.0)
        .max(
            a.now
                .as_ref()
                .and_then(|i| ms(Some(&i.updated_at)))
                .unwrap_or(0.0),
        )
        .max(
            a.items
                .first()
                .and_then(|i| ms(Some(&i.updated_at)))
                .unwrap_or(0.0),
        );
    for e in &a.events {
        last = last.max(e.t);
    }
    last
}

/// Retains classifications independently of tree-row lifetimes. Clock reversal and
/// changed topology rebuild conservatively; forward ticks visit only due entries.
#[derive(Clone, Debug)]
pub(crate) struct PaceStore {
    values: std::sync::Arc<HashMap<String, Pace>>,
    parents: Vec<Option<usize>>,
    live_below: Vec<usize>,
    last: Vec<f64>,
    due: std::collections::BTreeSet<(u64, usize)>,
    deadlines: Vec<Option<u64>>,
    dirty: HashSet<usize>,
    at: f64,
    touched: Vec<usize>,
}
fn time_key(t: f64) -> u64 {
    let bits = t.to_bits();
    if bits >> 63 != 0 {
        !bits
    } else {
        bits ^ (1 << 63)
    }
}
impl PaceStore {
    fn new(s: &State) -> Option<Self> {
        let ids = s.id_index();
        if ids.len() != s.snap.agents.len() {
            return None;
        }
        let n = s.snap.agents.len();
        let parents = s
            .snap
            .agents
            .iter()
            .map(|a| a.parent.as_deref().and_then(|id| ids.get(id).copied()))
            .collect();
        let mut live_below = vec![0; n];
        for a in &s.snap.agents {
            let mut seen = HashSet::new();
            let mut parent = a.parent.as_deref();
            while let Some(id) = parent {
                if !seen.insert(id) {
                    break;
                }
                let Some(&i) = ids.get(id) else {
                    break;
                };
                if live(a) {
                    live_below[i] += 1;
                }
                parent = s.snap.agents[i].parent.as_deref();
            }
        }
        Some(Self {
            values: Default::default(),
            parents,
            live_below,
            last: {
                let mut cache = s.render_cache.borrow_mut();
                s.snap.agents.iter().map(|a| cache.last(a)).collect()
            },
            due: Default::default(),
            deadlines: vec![None; n],
            dirty: (0..n).collect(),
            at: s.now,
            touched: Vec::new(),
        })
    }
    pub(crate) fn accept(&mut self, old: &Snapshot, new: &Snapshot, changed: &[usize]) {
        for &i in changed {
            if live(&old.agents[i]) != live(&new.agents[i]) {
                let mut at = self.parents[i];
                let mut seen = HashSet::new();
                while let Some(up) = at {
                    if !seen.insert(up) {
                        break;
                    }
                    at = self.parents[up];
                    if live(&new.agents[i]) {
                        self.live_below[up] += 1;
                    } else {
                        self.live_below[up] -= 1;
                    }
                    self.dirty.insert(up);
                }
            }
            self.last[i] = pace_last(&new.agents[i]);
            self.dirty.insert(i);
        }
    }
    fn needs_refresh(&self, now: f64) -> bool {
        !self.dirty.is_empty()
            || now < self.at
            || !now.is_finite()
            || self.due.first().is_some_and(|(t, _)| *t <= time_key(now))
    }
    fn refresh(&mut self, s: &State) -> std::sync::Arc<HashMap<String, Pace>> {
        if s.now < self.at || !s.now.is_finite() {
            self.dirty.extend(0..s.snap.agents.len());
        }
        self.at = s.now;
        while let Some(&(t, i)) = self.due.first() {
            if t > time_key(s.now) {
                break;
            }
            self.due.pop_first();
            self.deadlines[i] = None;
            self.dirty.insert(i);
        }
        if self.dirty.is_empty() {
            return self.values.clone();
        }
        let values = std::sync::Arc::make_mut(&mut self.values);
        self.touched.clear();
        for i in self.dirty.drain() {
            self.touched.push(i);
            if let Some(t) = self.deadlines[i].take() {
                self.due.remove(&(t, i));
            }
            let a = &s.snap.agents[i];
            let pc = pace_with_last(a, s.now, self.live_below[i] > 0, Some(self.last[i]));
            let mut next = f64::INFINITY;
            if let Some(p) = &pc {
                if let Some(tool) = a
                    .now
                    .as_ref()
                    .filter(|t| !matches!(t.kind.as_str(), "reasoning" | "assistant_message"))
                {
                    if !p.delegating {
                        if let Some(start) =
                            ms(tool.started_at.as_deref()).or_else(|| ms(Some(&tool.updated_at)))
                        {
                            let t = start + 300_000.;
                            if t > s.now {
                                next = t;
                            }
                        }
                    }
                } else if a.now.is_none() && !p.delegating && self.last[i] != 0. {
                    for t in [self.last[i] + 120_000., self.last[i] + 480_000.] {
                        if t > s.now {
                            next = next.min(t);
                        }
                    }
                }
            }
            if next.is_finite() {
                let key = time_key(next);
                self.due.insert((key, i));
                self.deadlines[i] = Some(key);
            }
            if let Some(p) = pc {
                values.insert(a.id.clone(), p);
            } else {
                values.remove(&a.id);
            }
        }
        self.values.clone()
    }
}

// Only inputs read by tree/scopes belong here. Histories, items and context do not.
fn same_structure(a: &Agent, b: &Agent) -> bool {
    a.id == b.id
        && a.parent == b.parent
        && a.kind == b.kind
        && a.status == b.status
        && has_pending_request(a) == has_pending_request(b)
        && a.updated == b.updated
        && a.started == b.started
        && a.ended == b.ended
        && a.shelf
            .as_ref()
            .map(|s| (s.listed, s.settled, &s.snoozed_until))
            == b.shelf
                .as_ref()
                .map(|s| (s.listed, s.settled, &s.snoozed_until))
}

#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub agent: usize,
    pub depth: usize,
    pub has_children: bool,
    pub stage: String,
    pub kept: usize,
    pub tucked: usize,
    pub earlier: Vec<usize>,
    pub guides: Vec<bool>,
    pub last: bool,
}
impl Row {
    pub fn is_earlier(&self) -> bool {
        !self.earlier.is_empty()
    }
}

/// Root blocks share row payloads across updates; the directory only stores block ends.
#[derive(Clone, Debug, Default)]
pub(crate) struct Rows {
    blocks: Vec<std::sync::Arc<Vec<Row>>>,
    ends: Vec<usize>,
}
impl Rows {
    fn push(&mut self, block: std::sync::Arc<Vec<Row>>) {
        self.ends.push(self.len() + block.len());
        self.blocks.push(block);
    }
    pub(crate) fn len(&self) -> usize {
        self.ends.last().copied().unwrap_or(0)
    }
    pub(crate) fn first(&self) -> Option<&Row> {
        self.iter().next()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Row> {
        self.blocks.iter().flat_map(|b| b.iter())
    }
    fn flat(rows: Vec<Row>) -> Self {
        let mut result = Self::default();
        if !rows.is_empty() {
            result.push(std::sync::Arc::new(rows));
        }
        result
    }
}
impl std::ops::Index<usize> for Rows {
    type Output = Row;
    fn index(&self, i: usize) -> &Row {
        let block = self.ends.partition_point(|end| *end <= i);
        let start = if block == 0 { 0 } else { self.ends[block - 1] };
        &self.blocks[block][i - start]
    }
}

#[derive(Clone, Debug)]
pub struct State {
    pub snap: Snapshot,
    pub(crate) generation: u64,
    pub(crate) render_cache: std::cell::RefCell<crate::ui::RenderCache>,
    /// Disable derived-data reuse for differential checks. Snapshot changes must
    /// enter through replace/replace_at so the snapshot generation advances.
    pub cache_enabled: bool,
    pub selected: Option<String>,
    pub scope: String,
    pub collapsed: HashSet<String>,
    pub expanded: HashSet<String>,
    pub detail_scroll: usize,
    pub calm: bool,
    pub paused: bool,
    pub help: bool,
    pub help_scroll: usize,
    pub usage: Vec<crate::overview::Provider>,
    pub demo: bool,
    pub interval: f64,
    pub fx: bool,
    /// The product's themes are on: the help lists `c`. Off in the original's behavior.
    pub themes: bool,
    /// The presenter offers Storage and lists its key in the dashboard help.
    pub storage_key_in_help: bool,
    pub acked: HashSet<String>,
    pub follow: bool,
    pub t3_link: bool,
    pub surface: std::cell::RefCell<crate::ui::Surface>,
    pub first_seen: HashMap<String, f64>,
    observed_items: HashMap<String, usize>,
    pub shimmer: HashMap<String, f64>,
    pub seeded: bool,
    pub boot_at: f64,
    /// Keyed by agent id and read for every agent on every frame, so hashed with [`IdHash`].
    pub tween: HashMap<String, f64, IdHash>,
    pub palette: crate::terminal::Palette,
    pub notice_at: f64,
    pub notice_error: bool,
    pub last_frame: f64,
    pub fx_elapsed: f64,
    pub last_effect_frame: f64,
    pub last_effect_delta: f64,
    pub last_effect_base: f64,
    pub frozen_at: Option<f64>,
    pub picked: Option<(String, f64)>,
    pub waits: HashMap<String, f64>,
    pub window_index: usize,
    pub error: Option<String>,
    pub error_kind: Option<String>,
    pub error_detail: Option<String>,
    pub now: f64,
    pub linked: Option<String>,
    pub notice: Option<String>,
    /// Volumes for the low-headroom line. Empty draws nothing. The live host
    /// replaces this on each dashboard refresh, including when the database
    /// snapshot did not change. Demo, `--db`, and tests set it themselves.
    /// Drawing never probes.
    pub headroom: Vec<crate::headroom::WarningVolume>,
    /// Latest read-only telemetry check report from the background host worker.
    /// Always `None` under `LOUPE_NO_THEMES`, `--demo`, `--db`, and tests unless set.
    pub telemetry: Option<std::sync::Arc<crate::telemetry::Report>>,
    pub telemetry_revision: u64,
    /// `process.env.HOME ?? ""`, read once when the state is built, as `view.ts` reads it at
    /// module load. Drawing never reads the environment. It lives on the state because the
    /// presenters build their own state instead of going through `cli`.
    pub home: String,
}
impl State {
    pub fn telemetry_alert(&self) -> Option<crate::telemetry::Alert> {
        if !self.themes || self.demo {
            return None;
        }
        self.telemetry.as_deref().and_then(crate::telemetry::Report::alert)
    }
    pub fn new(snap: Snapshot) -> Self {
        let now = snap.at;
        let mut s = Self {
            snap,
            generation: 0,
            render_cache: Default::default(),
            cache_enabled: true,
            selected: None,
            scope: "all".into(),
            collapsed: HashSet::new(),
            expanded: HashSet::new(),
            detail_scroll: 0,
            calm: true,
            paused: false,
            help: false,
            help_scroll: 0,
            usage: crate::overview::demo_usage(now),
            demo: true,
            interval: 2.,
            fx: false,
            themes: false,
            storage_key_in_help: false,
            acked: HashSet::new(),
            follow: true,
            t3_link: false,
            surface: std::cell::RefCell::new(crate::ui::Surface::default()),
            first_seen: HashMap::new(),
            observed_items: HashMap::new(),
            shimmer: HashMap::new(),
            seeded: false,
            boot_at: now,
            tween: HashMap::default(),
            palette: crate::terminal::Palette::default(),
            notice_at: 0.,
            notice_error: false,
            last_frame: 0.,
            fx_elapsed: 0.,
            last_effect_frame: now,
            last_effect_delta: 0.,
            last_effect_base: 0.,
            frozen_at: None,
            picked: None,
            waits: HashMap::new(),
            window_index: 1,
            error: None,
            error_kind: None,
            error_detail: None,
            now,
            linked: None,
            notice: None,
            headroom: Vec::new(),
            telemetry: None,
            telemetry_revision: 0,
            home: std::env::var_os("HOME")
                .map(|home| home.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        s.normalize();
        s.observe();
        s
    }
    pub(crate) fn id_index(&self) -> std::sync::Arc<HashMap<String, usize>> {
        if self.cache_enabled {
            if let Some(index) = &self.render_cache.borrow().id_index {
                return index.clone();
            }
        }
        let mut index = HashMap::new();
        for (i, a) in self.snap.agents.iter().enumerate() {
            index.entry(a.id.clone()).or_insert(i);
        }
        let index = std::sync::Arc::new(index);
        if self.cache_enabled {
            self.render_cache.borrow_mut().id_index = Some(index.clone());
        }
        index
    }
    /// Selection of a root or a live child does not affect folding. Duplicate
    /// identifiers conservatively retain the selection in the structural key.
    pub(crate) fn tree_selection(&self) -> Option<&str> {
        let index = self.id_index();
        self.selected.as_deref().filter(|id| {
            index.len() != self.snap.agents.len()
                || index.get(*id).is_some_and(|i| {
                    let a = &self.snap.agents[*i];
                    a.parent.is_some() && !live(a) && !wants_attention(a)
                })
        })
    }
    pub(crate) fn shared_rows(&self) -> std::sync::Arc<Rows> {
        if !self.cache_enabled {
            return std::sync::Arc::new(Rows::flat(tree(self)));
        }
        let phase = {
            let mut cache = self.render_cache.borrow_mut();
            let edges = cache.tree_edges.get_or_insert_with(|| {
                let mut edges = Vec::new();
                for a in &self.snap.agents {
                    let t = last_seen(a) + RECENT_MS;
                    if t.is_finite() {
                        edges.push(t.next_up());
                    }
                    if let Some(t) = a
                        .shelf
                        .as_ref()
                        .and_then(|s| ms(s.snoozed_until.as_deref()))
                    {
                        edges.push(t);
                    }
                }
                edges.sort_by(f64::total_cmp);
                edges
            });
            edges.partition_point(|t| *t <= self.now)
        };
        let selection = self.tree_selection();
        if let Some((_, rows)) = self.render_cache.borrow().rows.as_ref().filter(|(k, _)| {
            k.matches(
                self.generation,
                phase,
                &self.scope,
                &self.collapsed,
                &self.expanded,
                selection,
                self.linked.as_deref(),
            )
        }) {
            return rows.clone();
        }
        let context = TreeContextKey {
            phase,
            scope: self.scope.clone(),
            collapsed: self.collapsed.clone(),
            expanded: self.expanded.clone(),
            selection: selection.map(str::to_owned),
            linked: self.linked.clone(),
        };
        let rows = if self.scope == "all" {
            let mut cache = self.render_cache.borrow_mut();
            let index = cache
                .tree_index
                .get_or_insert_with(|| TreeIndex::new(&self.snap));
            if index.context.as_ref() != Some(&context) {
                index.blocks.clear();
                index.context = Some(context.clone());
            }
            std::sync::Arc::new(tree_indexed(self, Some(index)))
        } else {
            std::sync::Arc::new(Rows::flat(tree(self)))
        };
        let key = RowsKey {
            generation: self.generation,
            context,
        };
        let mut cache = self.render_cache.borrow_mut();
        cache.rows_revision = cache.rows_revision.wrapping_add(1);
        cache.rows = Some((key, rows.clone()));
        rows
    }
    pub fn rows(&self) -> Vec<Row> {
        tree(self)
    }
    /// Ancestors of live agents are delegating rather than quiet, even at several levels.
    pub fn paces(&self) -> HashMap<String, Pace> {
        self.uncached_paces()
    }
    pub(crate) fn shared_paces(&self) -> std::sync::Arc<HashMap<String, Pace>> {
        if !self.cache_enabled {
            return std::sync::Arc::new(self.uncached_paces());
        }
        // Drop the prepared owner before editing the map, avoiding a full COW clone.
        let existing = self.render_cache.borrow_mut().paces.take();
        let mut store = existing.or_else(|| PaceStore::new(self));
        let Some(mut store) = store.take() else {
            return std::sync::Arc::new(self.uncached_paces());
        };
        if store.needs_refresh(self.now) {
            let mut cache = self.render_cache.borrow_mut();
            cache.clear_prepared_paces();
        }
        let result = store.refresh(self);
        self.render_cache
            .borrow_mut()
            .count_dirty
            .extend(store.touched.drain(..));
        self.render_cache.borrow_mut().paces = Some(store);
        result
    }
    fn uncached_paces(&self) -> HashMap<String, Pace> {
        let lasts = if self.cache_enabled {
            let mut cache = self.render_cache.borrow_mut();
            if cache.pace_lasts.is_none() {
                let lasts = self.snap.agents.iter().map(|a| cache.last(a)).collect();
                cache.pace_lasts = Some(std::sync::Arc::new(lasts));
            }
            cache.pace_lasts.as_ref().unwrap().clone()
        } else {
            std::sync::Arc::new(
                self.snap
                    .agents
                    .iter()
                    .map(|a| pace_last(a))
                    .collect::<Vec<_>>(),
            )
        };

        let parents: HashMap<&str, Option<&str>> = self
            .snap
            .agents
            .iter()
            .map(|a| (a.id.as_str(), a.parent.as_deref()))
            .collect();
        let mut waiting = HashSet::new();
        for a in self.snap.agents.iter().filter(|a| live(a)) {
            let mut seen = HashSet::new();
            let mut parent = a.parent.as_deref();
            while let Some(up) = parent {
                if !seen.insert(up) {
                    break;
                }
                waiting.insert(up);
                parent = parents.get(up).copied().flatten();
            }
        }
        self.snap
            .agents
            .iter()
            .enumerate()
            .filter_map(|(i, a)| {
                pace_with_last(a, self.now, waiting.contains(a.id.as_str()), Some(lasts[i]))
                    .map(|p| (a.id.clone(), p))
            })
            .collect()
    }
    pub fn normalize(&mut self) {
        let rows = self.shared_rows();
        if !rows.iter().any(|r| Some(&r.id) == self.selected.as_ref()) {
            self.selected = rows.first().map(|r| r.id.clone());
            self.detail_scroll = 0;
        }
    }
    #[cfg(test)]
    pub fn pick(&mut self, id: &str) {
        self.pick_at(id, self.now);
    }
    pub fn pick_at(&mut self, id: &str, at: f64) {
        if self.selected.as_deref() != Some(id) {
            self.detail_scroll = 0;
        }
        self.selected = Some(id.into());
        self.picked = Some((id.into(), at));
        self.acknowledge(self.now);
    }
    /// Transient host messages use event time, even while help freezes the display clock.
    pub fn note(&mut self, text: impl Into<String>, error: bool, at: f64) {
        self.notice = Some(text.into());
        self.notice_at = at;
        self.notice_error = error;
    }
    pub fn thread_to_open(&self) -> Option<&Agent> {
        let selected = self
            .snap
            .agents
            .iter()
            .find(|a| Some(&a.id) == self.selected.as_ref())?;
        if selected
            .remote
            .as_ref()
            .and_then(AgentRemote::thread_ref)
            .is_some()
        {
            return Some(selected);
        }
        Some(
            self.snap
                .agents
                .iter()
                .find(|a| a.id == selected.thread_id)
                .unwrap_or(selected),
        )
    }
    pub fn acknowledge(&mut self, now: f64) {
        let waiting: HashSet<&str> = self
            .snap
            .agents
            .iter()
            .filter(|a| wants_attention(a))
            .map(|a| a.id.as_str())
            .collect();
        self.waits.retain(|id, _| waiting.contains(id.as_str()));
        self.acked.retain(|id| waiting.contains(id.as_str()));
        for id in waiting {
            let since = *self.waits.entry(id.into()).or_insert(now);
            if self
                .picked
                .as_ref()
                .is_some_and(|(picked, at)| picked == id && *at >= since)
            {
                self.acked.insert(id.into());
            }
        }
    }
    /// Original draw-clock semantics, independent of Rune's presentation backend.
    /// Returns (smooth, beat) for the adaptive host; repeated draws at one time do not advance a tween.
    pub fn advance_frame(&mut self, at: f64, reduced: bool) -> (bool, bool) {
        self.now = if self.help {
            *self.frozen_at.get_or_insert(at)
        } else {
            self.frozen_at = None;
            at
        };
        let dt = if self.last_frame != 0. {
            ((self.now - self.last_frame) / 1000.).clamp(0., 0.25)
        } else {
            0.
        };
        self.last_frame = self.now;
        let calm = self.calm || reduced;
        self.last_effect_delta = (at - self.last_effect_frame).max(0.);
        self.last_effect_base = self.fx_elapsed;
        if self.fx && !calm {
            self.fx_elapsed += self.last_effect_delta;
        }
        self.last_effect_frame = at;
        let mut smooth = false;
        for a in &self.snap.agents {
            let target = a
                .context
                .used_tokens
                .zip(a.context.max_tokens)
                .filter(|(_, m)| *m > 0.)
                .map(|(u, m)| (u / m).clamp(0., 1.))
                .unwrap_or(0.);
            // `entry(a.id.clone()).or_insert(0.)` without cloning the id when it is already there,
            // which after the first frame it always is.
            let cur = match self.tween.get_mut(&a.id) {
                Some(cur) => cur,
                None => self.tween.entry(a.id.clone()).or_insert(0.),
            };
            let next = if calm {
                target
            } else {
                *cur + (target - *cur) * (1. - (-dt * 7.).exp())
            };
            *cur = if (target - next).abs() < 0.0015 {
                target
            } else {
                smooth = true;
                next
            };
        }
        self.acknowledge(self.now);
        let paces = if !calm {
            Some(self.shared_paces())
        } else {
            None
        };
        let working = |a: &Agent| {
            matches!(a.status.as_str(), "running" | "active")
                && !blocks_activity(a)
                && !paces
                    .as_ref()
                    .and_then(|ps| ps.get(&a.id))
                    .is_some_and(|p| p.quiet)
        };
        if !calm {
            smooth |= self.now - self.boot_at < 650.;
            smooth |= self.snap.agents.iter().any(|a| {
                working(a)
                    && self
                        .shimmer
                        .get(&a.id)
                        .is_some_and(|at| self.now >= *at && self.now - *at < 450.)
            });
        }
        let flash = self
            .snap
            .agents
            .iter()
            .find(|a| Some(&a.id) == self.selected.as_ref())
            .is_some_and(|a| {
                let mut key = String::new();
                a.items
                    .iter()
                    .filter(|i| !matches!(i.kind.as_str(), "reasoning" | "assistant_message"))
                    .take(16)
                    .any(|i| {
                        item_key_into(i, &mut key);
                        self.first_seen
                            .get(key.as_str())
                            .is_some_and(|at| *at > 0. && (self.now - *at).max(0.) < 1600.)
                    })
            });
        let beat = !calm
            && (self.snap.agents.iter().any(|a| working(a))
                || self
                    .snap
                    .agents
                    .iter()
                    .any(|a| needs_you(a) && !self.acked.contains(&a.id))
                || flash);
        if self.help {
            (false, false)
        } else {
            (smooth, beat)
        }
    }
    fn sync_effect_toggle(&mut self, at: f64) {
        // Rune polls before input handlers; correct this frame's phase if its key toggled the effect.
        if at == self.last_effect_frame {
            let reduced = self.surface.borrow().system_reduced;
            self.fx_elapsed = self.last_effect_base
                + if self.fx && !self.calm && !reduced {
                    self.last_effect_delta
                } else {
                    0.
                };
        }
    }
    fn snap_context(&mut self) {
        for a in &self.snap.agents {
            let target = a
                .context
                .used_tokens
                .zip(a.context.max_tokens)
                .filter(|(_, m)| *m > 0.)
                .map(|(u, m)| (u / m).clamp(0., 1.))
                .unwrap_or(0.);
            self.tween.insert(a.id.clone(), target);
        }
    }
    pub fn selected_agent(&self) -> Option<&Agent> {
        self.rows()
            .iter()
            .find(|r| Some(&r.id) == self.selected.as_ref())
            .map(|r| &*self.snap.agents[r.agent])
    }
    pub fn observe(&mut self) {
        self.observe_at(self.now);
    }
    pub fn observe_at(&mut self, now: f64) {
        let mut seen = HashMap::new();
        self.observed_items.clear();
        for a in &self.snap.agents {
            let mut call = false;
            for i in &a.items {
                if matches!(i.kind.as_str(), "reasoning" | "assistant_message") {
                    continue;
                }
                let key = item_key(i);
                *self.observed_items.entry(key.clone()).or_default() += 1;
                if let Some(before) = self
                    .first_seen
                    .get(&key)
                    .or_else(|| seen.get(&key))
                    .copied()
                {
                    seen.insert(key, before);
                } else {
                    seen.insert(key, if self.seeded { now } else { 0. });
                    if self.seeded
                        && !matches!(
                            category(&i.kind, i.tool.as_deref()),
                            "you" | "think" | "reply"
                        )
                    {
                        call = true;
                    }
                }
            }
            if call {
                self.shimmer.insert(a.id.clone(), now);
            }
        }
        self.first_seen = seen;
        self.seeded = true;
        self.shimmer.retain(|_, at| now - *at < 450.);
    }
    fn observe_delta(
        &mut self,
        removed: &[std::sync::Arc<Agent>],
        added: &[std::sync::Arc<Agent>],
        now: f64,
    ) {
        let mut touched = HashSet::new();
        for a in removed {
            for i in &a.items {
                if matches!(i.kind.as_str(), "reasoning" | "assistant_message") {
                    continue;
                }
                let key = item_key(i);
                if let Some(n) = self.observed_items.get_mut(&key) {
                    *n -= 1;
                }
                touched.insert(key);
            }
        }
        for a in added {
            let mut call = false;
            for i in &a.items {
                if matches!(i.kind.as_str(), "reasoning" | "assistant_message") {
                    continue;
                }
                let key = item_key(i);
                *self.observed_items.entry(key.clone()).or_default() += 1;
                if !self.first_seen.contains_key(&key) {
                    self.first_seen
                        .insert(key, if self.seeded { now } else { 0. });
                    call |= self.seeded
                        && !matches!(
                            category(&i.kind, i.tool.as_deref()),
                            "you" | "think" | "reply"
                        );
                }
            }
            if call {
                self.shimmer.insert(a.id.clone(), now);
            }
        }
        for key in touched {
            if self.observed_items.get(&key) == Some(&0) {
                self.observed_items.remove(&key);
                self.first_seen.remove(&key);
            }
        }
        self.seeded = true;
        self.shimmer.retain(|_, at| now - *at < 450.);
    }
    pub fn replace(&mut self, snap: Snapshot) {
        let at = snap.at;
        self.replace_at(snap, at);
    }
    /// Takes a new snapshot together with what changed in it. The result must equal
    /// [`State::replace`] with the same snapshot in every observable way; `changed` only
    /// supplies an advisory hint. Arc identity is the authority.
    pub fn apply(&mut self, snap: Snapshot, changed: &Changed) {
        let at = snap.at;
        self.apply_at(snap, changed, at);
    }
    pub fn apply_at(&mut self, snap: Snapshot, changed: &Changed, observed_at: f64) {
        let _ = changed;
        self.replace_at(snap, observed_at);
    }
    pub fn replace_at(&mut self, snap: Snapshot, observed_at: f64) {
        if !self.paused {
            if !self.help {
                self.now = snap.at;
            }
            // Hints are deliberately not consulted: compare every identity and Arc.
            // The old snapshot owns its Arcs throughout this comparison.
            let same_order = self.snap.agents.len() == snap.agents.len()
                && self
                    .snap
                    .agents
                    .iter()
                    .zip(&snap.agents)
                    .all(|(a, b)| a.id == b.id);
            let changed: Vec<usize> = snap
                .agents
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    (!same_order || !std::sync::Arc::ptr_eq(a, &self.snap.agents[i])).then_some(i)
                })
                .collect();
            let structural = !same_order
                || changed
                    .iter()
                    .any(|&i| !same_structure(&self.snap.agents[i], &snap.agents[i]));
            let observed_delta = self
                .render_cache
                .borrow_mut()
                .accept(&self.snap, &snap, &changed, same_order, structural);
            if structural {
                self.generation = self.generation.wrapping_add(1);
            }
            let delta = self.cache_enabled.then_some(observed_delta);
            self.snap = snap;
            self.error = None;
            self.error_kind = None;
            self.error_detail = None;
            self.normalize();
            if let Some((removed, added)) = delta {
                self.observe_delta(&removed, &added, observed_at);
            } else {
                self.observe_at(observed_at);
            }
            let index = self.id_index();
            self.acked.retain(|id| {
                if index.len() == self.snap.agents.len() {
                    index
                        .get(id)
                        .is_some_and(|&i| wants_attention(&self.snap.agents[i]))
                } else {
                    self.snap.agents.iter().any(|a| &a.id == id && wants_attention(a))
                }
            });
        }
    }
    pub fn set_stage(&mut self, id: &str, stage: &str) {
        if stage == "folded" {
            self.collapsed.insert(id.into());
        } else {
            self.collapsed.remove(id);
        }
        if stage == "all" {
            self.expanded.insert(id.into());
        } else {
            self.expanded.remove(id);
        }
    }
    pub fn step(&mut self, row: &Row, delta: isize) -> bool {
        if !row.has_children || row.is_earlier() {
            return false;
        }
        let stages: &[&str] = if row.kept > 0 && row.tucked > 0 {
            &["folded", "recent", "all"]
        } else {
            &["folded", "all"]
        };
        let at = stages.iter().position(|x| *x == row.stage).unwrap_or(0) as isize + delta;
        if at < 0 || at as usize >= stages.len() {
            return false;
        }
        let next = stages[at as usize];
        let id = self.snap.agents[row.agent].id.clone();
        self.set_stage(
            &id,
            if next == "all" && row.tucked == 0 {
                "recent"
            } else {
                next
            },
        );
        true
    }
    pub fn show_earlier_at(&mut self, row: &Row, at: f64) {
        if row.earlier.is_empty() {
            return;
        }
        let id = self.snap.agents[row.agent].id.clone();
        self.set_stage(&id, "all");
        for idx in &row.earlier {
            if let Some(parent) = self.snap.agents[*idx].parent.clone() {
                self.set_stage(&parent, "all");
            }
        }
        let id = self.snap.agents[row.earlier[0]].id.clone();
        self.pick_at(&id, at);
    }
    pub fn next_waiting_at(&mut self, at: f64) {
        let mut unfolded = self.clone();
        unfolded.collapsed.clear();
        let order = unfolded.rows();
        let mut waiting: Vec<_> = order
            .iter()
            .enumerate()
            .filter(|(_, r)| !r.is_earlier() && wants_attention(&self.snap.agents[r.agent]))
            .collect();
        let exact = waiting
            .iter()
            .any(|(_, r)| self.snap.agents[r.agent].open_asks.total() > 0);
        if exact {
            waiting.sort_by(|(_, l), (_, r)| {
                let (l, r) = (&self.snap.agents[l.agent], &self.snap.agents[r.agent]);
                (!needs_you(l)).cmp(&!needs_you(r)).then_with(|| {
                    waiting_since(l)
                        .unwrap_or(at)
                        .total_cmp(&waiting_since(r).unwrap_or(at))
                })
            });
        }
        let start = if exact {
            waiting
                .iter()
                .position(|(_, r)| Some(&r.id) == self.selected.as_ref())
        } else {
            // Legacy approvals-only navigation follows the visible list from the current row.
            let position = order
                .iter()
                .position(|r| Some(&r.id) == self.selected.as_ref());
            position.and_then(|i| waiting.iter().rposition(|(index, _)| *index <= i))
        }
        .map_or(0, |i| i + 1);
        let Some((_, next)) = waiting.get(start).or_else(|| waiting.first()) else {
            self.note("Nobody is waiting on you", false, at);
            return;
        };
        let id = next.id.clone();
        let parents: HashMap<_, _> = self
            .snap
            .agents
            .iter()
            .map(|a| (a.id.clone(), a.parent.clone()))
            .collect();
        let mut parent = parents.get(&id).cloned().flatten();
        let mut seen = HashSet::new();
        while let Some(up) = parent {
            if !seen.insert(up.clone()) {
                break;
            }
            self.collapsed.remove(&up);
            parent = parents.get(&up).cloned().flatten();
        }
        self.pick_at(&id, at);
    }
    /// Keyboard vocabulary deliberately matches App.key; pick:ID is a scripted click.
    pub fn action(&mut self, key: &str) {
        self.action_at(key, self.now);
    }
    pub fn action_at(&mut self, key: &str, at: f64) {
        if let Some(id) = key.strip_prefix("pick:") {
            self.pick_at(id, at);
            self.normalize();
            return;
        }
        let rows = self.shared_rows();
        let row = rows
            .iter()
            .find(|r| Some(&r.id) == self.selected.as_ref())
            .cloned();
        if matches!(key, "?" | "h") {
            self.help = !self.help;
            self.help_scroll = 0;
            if self.help {
                self.frozen_at = Some(at);
                self.now = at;
            } else {
                let reduced = self.surface.borrow().system_reduced;
                self.advance_frame(at, reduced);
            }
            return;
        }
        if key == "escape" && self.help {
            self.help = false;
            let reduced = self.surface.borrow().system_reduced;
            self.advance_frame(at, reduced);
            return;
        }
        if self.help
            && matches!(
                key,
                "j" | "down" | "k" | "up" | "space" | "pagedown" | "J" | "pageup" | "K"
            )
        {
            let max = self.surface.borrow().help_max;
            match key {
                "j" | "down" => self.help_scroll = (self.help_scroll + 1).min(max),
                "k" | "up" => self.help_scroll = self.help_scroll.saturating_sub(1),
                "space" | "pagedown" | "J" => self.help_scroll = (self.help_scroll + 5).min(max),
                "pageup" | "K" => self.help_scroll = self.help_scroll.saturating_sub(5),
                _ => {}
            };
            return;
        }
        match key {
            "j" | "down" | "k" | "up" | "home" | "g" | "end" | "G" => {
                if !rows.is_empty() {
                    let index = rows
                        .iter()
                        .position(|r| Some(&r.id) == self.selected.as_ref())
                        .unwrap_or(0);
                    let next = match key {
                        "k" | "up" => index.saturating_sub(1),
                        "home" | "g" => 0,
                        "end" | "G" => rows.len() - 1,
                        _ => (index + 1).min(rows.len() - 1),
                    };
                    // `pick` runs even when the row is already selected: it still records the
                    // deliberate pick, which acknowledges a wait on that row.
                    self.pick_at(&rows[next].id, at);
                }
            }
            "J" | "pagedown" => {
                self.detail_scroll = self
                    .detail_scroll
                    .saturating_add(5)
                    .min(self.surface.borrow().detail_max)
            }
            "K" | "pageup" => self.detail_scroll = self.detail_scroll.saturating_sub(5),
            "a" => {
                self.scope = match self.scope.as_str() {
                    "all" => "active",
                    "active" => "t3",
                    _ => "all",
                }
                .into();
                self.note(match self.scope.as_str(){"active"=>"Active only: what is running or waiting on you · a for T3's sidebar","t3"=>"T3 sidebar: threads not settled or snoozed, and anything active · a for all",_=>"All threads: every unarchived T3 thread · a for active only"},false,at);
            }
            "n" => self.next_waiting_at(at),
            "t" => self.window_index = (self.window_index + 1) % WINDOWS.len(),
            "m" => {
                self.calm = !self.calm;
                if self.calm {
                    self.fx = false;
                    self.snap_context();
                }
                self.sync_effect_toggle(at);
                self.note(
                    if self.calm {
                        "Calm mode: nothing moves but the data · m to leave"
                    } else {
                        "Calm mode off"
                    },
                    false,
                    at,
                );
            }
            "x" => {
                if !self.calm {
                    self.fx = !self.fx;
                } else {
                    self.note(
                        "Calm mode keeps CRT effects off · m leaves calm mode",
                        false,
                        at,
                    );
                }
                self.sync_effect_toggle(at);
            }
            "f" => {
                self.follow = !self.follow;
                if self.follow {
                    self.linked = None; // Runtime re-resolves the retained title and unfolds its path.
                }
            }
            "p" => self.paused = !self.paused,
            "space" | " " | "right" | "return" | "enter" | "o"
                if row.as_ref().is_some_and(Row::is_earlier) =>
            {
                self.show_earlier_at(row.as_ref().unwrap(), at)
            }
            "space" | " " => {
                if let Some(r) = &row {
                    if r.stage == "folded" {
                        self.step(r, 1);
                    } else if r.has_children {
                        let id = self.snap.agents[r.agent].id.clone();
                        self.set_stage(&id, "folded");
                    }
                }
            }
            "right" => {
                if let Some(r) = &row {
                    self.step(r, 1);
                }
            }
            "left" => {
                if let Some(r) = &row {
                    let parent = if r.is_earlier() {
                        Some(self.snap.agents[r.agent].id.clone())
                    } else {
                        self.snap.agents[r.agent].parent.clone()
                    };
                    if !self.step(r, -1) {
                        if let Some(p) = parent {
                            self.pick_at(&p, at);
                        }
                    }
                }
            }
            "return" | "enter" | "o" => {
                if let Some(thread) = self.thread_to_open() {
                    if self.demo {
                        self.note(
                            format!("Demo: would open {} in T3 Code", thread.title),
                            false,
                            at,
                        );
                    }
                }
            }
            _ => {}
        }
        self.normalize();
    }
    pub fn semantic(&self) -> Value {
        let mut collapsed: Vec<_> = self.collapsed.iter().collect();
        collapsed.sort();
        let mut expanded: Vec<_> = self.expanded.iter().collect();
        expanded.sort();
        json!({ "selected": self.selected, "scope": self.scope, "collapsed": collapsed, "expanded": expanded,
            "rows": self.rows().iter().map(|r| json!({"id":r.id,"depth":r.depth,"stage":r.stage,"kept":r.kept,"tucked":r.tucked,"earlier":r.earlier.iter().map(|i|&self.snap.agents[*i].id).collect::<Vec<_>>()})).collect::<Vec<_>>() })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TreeContextKey {
    phase: usize,
    scope: String,
    collapsed: HashSet<String>,
    expanded: HashSet<String>,
    selection: Option<String>,
    linked: Option<String>,
}

impl TreeContextKey {
    fn matches(
        &self,
        phase: usize,
        scope: &str,
        collapsed: &HashSet<String>,
        expanded: &HashSet<String>,
        selection: Option<&str>,
        linked: Option<&str>,
    ) -> bool {
        self.phase == phase
            && self.scope == scope
            && self.selection.as_deref() == selection
            && self.linked.as_deref() == linked
            && self.collapsed == *collapsed
            && self.expanded == *expanded
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RowsKey {
    generation: u64,
    context: TreeContextKey,
}

impl RowsKey {
    fn matches(
        &self,
        generation: u64,
        phase: usize,
        scope: &str,
        collapsed: &HashSet<String>,
        expanded: &HashSet<String>,
        selection: Option<&str>,
        linked: Option<&str>,
    ) -> bool {
        self.generation == generation
            && self
                .context
                .matches(phase, scope, collapsed, expanded, selection, linked)
    }
}

/// Owned adjacency and parsed sort keys. Only the all scope uses this index;
/// filtered scopes keep their independent from-scratch construction.
#[derive(Clone, Debug, Default)]
pub(crate) struct TreeIndex {
    children: HashMap<Option<String>, Vec<usize>>,
    updated: Vec<f64>,
    roots: Option<Vec<usize>>,
    blocks: HashMap<usize, std::sync::Arc<Vec<Row>>>,
    context: Option<TreeContextKey>,
}
impl TreeIndex {
    pub(crate) fn new(snap: &Snapshot) -> Self {
        let mut index = Self::default();
        for (i, a) in snap.agents.iter().enumerate() {
            index.children.entry(a.parent.clone()).or_default().push(i);
            index.updated.push(ms(a.updated.as_deref()).unwrap_or(0.));
        }
        let keys: Vec<_> = index.children.keys().cloned().collect();
        index.sort(snap, keys);
        index.roots = Self::roots(snap);
        index
    }
    fn roots(snap: &Snapshot) -> Option<Vec<usize>> {
        let ids: HashMap<_, _> = snap
            .agents
            .iter()
            .enumerate()
            .map(|(i, a)| (a.id.as_str(), i))
            .collect();
        if ids.len() != snap.agents.len() {
            return None;
        }
        let mut roots = vec![0; snap.agents.len()];
        let mut marks = vec![0u8; snap.agents.len()];
        for start in 0..snap.agents.len() {
            if marks[start] == 2 {
                continue;
            }
            let mut path = Vec::new();
            let mut at = start;
            let root = loop {
                if marks[at] == 2 {
                    break roots[at];
                }
                if marks[at] == 1 {
                    return None;
                }
                marks[at] = 1;
                path.push(at);
                match snap.agents[at].parent.as_deref() {
                    None => break at,
                    Some(id) => at = *ids.get(id)?,
                }
            };
            for i in path {
                roots[i] = root;
                marks[i] = 2;
            }
        }
        Some(roots)
    }

    fn sort(&mut self, snap: &Snapshot, keys: Vec<Option<String>>) {
        for key in keys {
            if let Some(list) = self.children.get_mut(&key) {
                list.sort_by(|&i, &j| {
                    let (a, b) = (&snap.agents[i], &snap.agents[j]);
                    (!has_pending_request(a))
                        .cmp(&!has_pending_request(b))
                        .then_with(|| (!live(a)).cmp(&!live(b)))
                        .then_with(|| self.updated[j].total_cmp(&self.updated[i]))
                        .then_with(|| a.id.cmp(&b.id))
                        .then_with(|| i.cmp(&j))
                });
            }
        }
    }
    pub(crate) fn update(&mut self, old: &Snapshot, new: &Snapshot, changed: &[usize]) {
        let mut dirty = HashSet::new();
        let mut reparented = false;
        for &i in changed {
            if !same_structure(&old.agents[i], &new.agents[i]) {
                if let Some(roots) = &self.roots {
                    self.blocks.remove(&roots[i]);
                }
            }
            let (a, b) = (&old.agents[i], &new.agents[i]);
            if a.parent != b.parent {
                reparented = true;
                if let Some(list) = self.children.get_mut(&a.parent) {
                    list.retain(|&j| j != i);
                }
                if self.children.get(&a.parent).is_some_and(Vec::is_empty) {
                    self.children.remove(&a.parent);
                }
                self.children.entry(b.parent.clone()).or_default().push(i);
                dirty.insert(a.parent.clone());
                dirty.insert(b.parent.clone());
            }
            if a.updated != b.updated
                || live(a) != live(b)
                || has_pending_request(a) != has_pending_request(b)
            {
                self.updated[i] = ms(b.updated.as_deref()).unwrap_or(0.);
                dirty.insert(b.parent.clone());
            }
        }
        self.sort(new, dirty.into_iter().collect());
        if reparented {
            self.roots = Self::roots(new);
            // Re-parenting can affect both ancestor chains and cycle reachability.
            // Keep the conservative fallback until those repairs are indexed too.
            self.blocks.clear();
        }
    }
}

pub fn tree(s: &State) -> Vec<Row> {
    tree_indexed(s, None).iter().cloned().collect()
}
fn tree_indexed(s: &State, mut index: Option<&mut TreeIndex>) -> Rows {
    let agents = &s.snap.agents;
    let by_id: HashMap<&str, usize> = agents
        .iter()
        .enumerate()
        .map(|(i, a)| (a.id.as_str(), i))
        .collect();
    let mut allowed: HashSet<usize> = if s.scope == "all" {
        (0..agents.len()).collect()
    } else {
        HashSet::new()
    };
    if s.scope != "all" {
        for (start, a) in agents.iter().enumerate() {
            if !live(a) && !has_pending_request(a) {
                continue;
            }
            let mut seen = HashSet::new();
            let mut at = Some(start);
            while let Some(i) = at {
                if !seen.insert(i) {
                    break;
                }
                allowed.insert(i);
                at = agents[i]
                    .parent
                    .as_deref()
                    .and_then(|p| by_id.get(p).copied());
            }
        }
        if s.scope == "t3" {
            let mut shown = s.linked.as_deref().and_then(|id| by_id.get(id).copied());
            let mut seen = HashSet::new();
            while let Some(i) = shown {
                if !seen.insert(i) {
                    break;
                }
                let Some(parent) = agents[i]
                    .parent
                    .as_deref()
                    .and_then(|p| by_id.get(p).copied())
                else {
                    break;
                };
                shown = Some(parent);
            }
            let mut below: HashMap<&str, Vec<usize>> = HashMap::new();
            for (i, a) in agents.iter().enumerate() {
                if let Some(p) = &a.parent {
                    below.entry(p).or_default().push(i);
                }
            }
            let mut stack: Vec<usize> = agents
                .iter()
                .enumerate()
                .filter(|(i, a)| a.parent.is_none() && (Some(*i) == shown || in_sidebar(a, s.now)))
                .map(|(i, _)| i)
                .collect();
            let mut done = HashSet::new();
            while let Some(i) = stack.pop() {
                if !done.insert(i) {
                    continue;
                }
                allowed.insert(i);
                if let Some(kids) = below.get(agents[i].id.as_str()) {
                    stack.extend(kids);
                }
            }
        }
    }
    let owned_children = index.as_ref().map(|index| index.children.clone());
    let children: HashMap<Option<&str>, Vec<usize>> = if let Some(children) = &owned_children {
        children
            .iter()
            .map(|(key, list)| (key.as_deref(), list.clone()))
            .collect()
    } else {
        let mut children: HashMap<Option<&str>, Vec<usize>> = HashMap::new();
        for (i, a) in agents.iter().enumerate() {
            if allowed.contains(&i) {
                children.entry(a.parent.as_deref()).or_default().push(i);
            }
        }
        for list in children.values_mut() {
            list.sort_by(|p, q| {
                let (p, q) = (&agents[*p], &agents[*q]);
                (!has_pending_request(p))
                    .cmp(&!has_pending_request(q))
                    .then_with(|| (!live(p)).cmp(&!live(q)))
                    .then_with(|| {
                        ms(q.updated.as_deref())
                            .unwrap_or(0.0)
                            .total_cmp(&ms(p.updated.as_deref()).unwrap_or(0.0))
                    })
                    .then_with(|| p.id.cmp(&q.id))
            });
        }
        children
    };
    fn stays(
        i: usize,
        s: &State,
        children: &HashMap<Option<&str>, Vec<usize>>,
        cache: &mut HashMap<usize, bool>,
        path: &mut HashSet<usize>,
    ) -> bool {
        if let Some(v) = cache.get(&i) {
            return *v;
        }
        if !path.insert(i) {
            return false;
        }
        let a = &s.snap.agents[i];
        let v = live(a)
            || wants_attention(a)
            || s.selected.as_deref() == Some(&a.id)
            || s.linked.as_deref() == Some(&a.id)
            || children
                .get(&Some(a.id.as_str()))
                .is_some_and(|kids| kids.iter().any(|k| stays(*k, s, children, cache, path)));
        cache.insert(i, v);
        v
    }
    struct Walk<'a> {
        s: &'a State,
        children: HashMap<Option<&'a str>, Vec<usize>>,
        busy: HashMap<usize, bool>,
        rows: Vec<Row>,
        seen: HashSet<usize>,
    }
    impl Walk<'_> {
        fn visit(&mut self, i: usize, depth: usize, guides: Vec<bool>, last: bool) {
            if !self.seen.insert(i) {
                return;
            }
            let agents = &self.s.snap.agents;
            let a = &agents[i];
            let kids = self
                .children
                .get(&Some(a.id.as_str()))
                .cloned()
                .unwrap_or_default();
            let mut done: Vec<_> = kids
                .iter()
                .copied()
                .filter(|k| {
                    !stays(
                        *k,
                        self.s,
                        &self.children,
                        &mut self.busy,
                        &mut HashSet::new(),
                    )
                })
                .collect();
            done.sort_by(|p, q| last_seen(&agents[*q]).total_cmp(&last_seen(&agents[*p])));
            let latest: HashSet<_> = done
                .iter()
                .take(RECENT_MAX)
                .filter(|k| last_seen(&agents[**k]) >= self.s.now - RECENT_MS)
                .copied()
                .collect();
            let kept: Vec<_> = kids
                .iter()
                .copied()
                .filter(|k| {
                    latest.contains(k)
                        || stays(
                            *k,
                            self.s,
                            &self.children,
                            &mut self.busy,
                            &mut HashSet::new(),
                        )
                })
                .collect();
            let tucked: Vec<_> = done.into_iter().filter(|k| !latest.contains(k)).collect();
            let stage = if kids.is_empty() {
                "all"
            } else if self.s.collapsed.contains(&a.id) {
                "folded"
            } else if tucked.is_empty() || self.s.expanded.contains(&a.id) {
                "all"
            } else if !kept.is_empty() {
                "recent"
            } else {
                "folded"
            };
            self.rows.push(Row {
                id: a.id.clone(),
                agent: i,
                depth,
                has_children: !kids.is_empty(),
                stage: stage.into(),
                kept: kept.len(),
                tucked: tucked.len(),
                earlier: vec![],
                guides: guides.clone(),
                last,
            });
            let shown = match stage {
                "all" => kids,
                "recent" => kept,
                _ => vec![],
            };
            let more = if stage == "recent" { tucked } else { vec![] };
            let mut inner = guides;
            if depth > 0 {
                inner.push(!last);
            }
            for (n, k) in shown.iter().enumerate() {
                self.visit(
                    *k,
                    depth + 1,
                    inner.clone(),
                    n == shown.len() - 1 && more.is_empty(),
                );
            }
            if !more.is_empty() {
                let mut hidden = Vec::new();
                let mut included = HashSet::new();
                let mut stack = more;
                while let Some(k) = stack.pop() {
                    if self.seen.contains(&k) || !included.insert(k) {
                        continue;
                    }
                    hidden.push(k);
                    if let Some(desc) = self.children.get(&Some(agents[k].id.as_str())) {
                        stack.extend(desc);
                    }
                }
                hidden.sort_by(|p, q| last_seen(&agents[*q]).total_cmp(&last_seen(&agents[*p])));
                self.rows.push(Row {
                    id: format!("earlier:{}", a.id),
                    agent: i,
                    depth: depth + 1,
                    has_children: false,
                    stage: "all".into(),
                    kept: 0,
                    tucked: 0,
                    earlier: hidden,
                    guides: inner,
                    last: true,
                });
            }
        }
    }
    let roots = children.get(&None).cloned().unwrap_or_default();
    let mut walk = Walk {
        s,
        children,
        busy: HashMap::new(),
        rows: vec![],
        seen: HashSet::new(),
    };
    let mut result = Rows::default();
    for (n, i) in roots.iter().enumerate() {
        let last = n == roots.len() - 1;
        if let Some(index) = index.as_mut().filter(|index| index.roots.is_some()) {
            let block = index.blocks.entry(*i).or_insert_with(|| {
                walk.visit(*i, 0, vec![], last);
                std::sync::Arc::new(std::mem::take(&mut walk.rows))
            });
            if block[0].last != last {
                std::sync::Arc::make_mut(block)[0].last = last;
            }
            result.push(block.clone());
        } else {
            walk.visit(*i, 0, vec![], last);
            result.push(std::sync::Arc::new(std::mem::take(&mut walk.rows)));
        }
    }
    result
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Lane {
    pub counts: Vec<usize>,
    pub fails: Vec<bool>,
    pub you: Vec<bool>,
    pub busy: Vec<bool>,
}
#[cfg(test)]
pub fn lane(
    events: &[Event],
    spans: &[(f64, Option<f64>)],
    cols: usize,
    window_ms: f64,
    now: f64,
) -> Lane {
    if cols == 0 {
        return Lane::default();
    }
    let size = ((window_ms / cols as f64 / 1000.0).round() * 1000.0).max(1000.0);
    let last = (now / size).floor();
    let col = |t: f64| cols as isize - 1 - (last - (t / size).floor()) as isize;
    let mut d = Lane {
        counts: vec![0; cols],
        fails: vec![false; cols],
        you: vec![false; cols],
        busy: vec![false; cols],
    };
    for e in events {
        let i = col(e.t).min(cols as isize - 1);
        if i < 0 || e.t > now + 5000.0 {
            continue;
        }
        let i = i as usize;
        if e.cat == "you" {
            d.you[i] = true;
        } else {
            d.counts[i] += 1;
        }
        if e.failed {
            d.fails[i] = true;
        }
    }
    for (start, end) in spans {
        let i0 = col(*start).max(0);
        let i1 = col(end.unwrap_or(now)).min(cols as isize - 1);
        for i in i0..=i1 {
            if i >= 0 && (i as usize) < cols {
                d.busy[i as usize] = true;
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    fn agent(id: &str, parent: Option<&str>, status: &str, ago: i64) -> Agent {
        let now = ms(Some("2026-11-12T12:00:00Z")).unwrap();
        let mut a = Agent {
            id: id.into(),
            parent: parent.map(Into::into),
            title: id.into(),
            status: status.into(),
            ..Agent::default()
        };
        a.events = vec![Event {
            t: now - ago as f64 * 60000.0,
            ..Event::default()
        }];
        a.updated = Some(format!(
            "2026-11-12T{:02}:{:02}:00Z",
            12 - ago / 60 - i64::from(ago % 60 > 0),
            (60 - ago % 60) % 60
        ));
        if !live(&a) {
            a.ended = a.updated.clone();
        }
        a
    }
    fn state() -> State {
        let now = ms(Some("2026-11-12T12:00:00Z")).unwrap();
        State::new(Snapshot {
            at: now,
            agents: vec![
                agent("root", None, "running", 0).into(),
                agent("ask", Some("root"), "waiting", 0).into(),
                agent("run", Some("root"), "running", 1).into(),
                agent("d5", Some("root"), "completed", 5).into(),
                agent("d20", Some("root"), "completed", 20).into(),
                agent("d40", Some("root"), "failed", 40).into(),
                agent("d90", Some("root"), "completed", 90).into(),
                agent("d200", Some("root"), "failed", 200).into(),
            ],
            ..Snapshot::default()
        })
    }
    #[test]
    fn iso_time_is_timezone_independent() {
        assert_eq!(ms(Some("1970-01-01T00:00:00Z")), Some(0.0));
        assert_eq!(
            ms(Some("2026-11-12T12:00:00Z")),
            ms(Some("2026-11-12T13:00:00+01:00"))
        );
        assert_eq!(ms(Some("1970-01-01T00:00:00.125Z")), Some(125.0));
        assert_eq!(ms(Some("oops")), None);
        // What Node's Date.parse returns for forms that do not depend on the local zone.
        for (stamp, want) in [
            ("2026", Some(1767225600000.)),
            ("2026-11", Some(1793491200000.)),
            ("2026-11-12", Some(1794441600000.)),
            ("2026-11-12T10:20Z", Some(1794478800000.)),
            ("2026-11-12T10:20:30Z", Some(1794478830000.)),
            ("2026-11-12T10:20:30.5Z", Some(1794478830500.)),
            ("2026-11-12T10:20:30.123456Z", Some(1794478830123.)),
            ("2026-11-12T10:20:30.9999Z", Some(1794478830999.)),
            ("2026-11-12T10:20:30.000+05:30", Some(1794459030000.)),
            ("2026-11-12T10:20:30+0530", Some(1794459030000.)),
            ("2026-11-12T10:20:30+05", None),
            ("2026-11-12t10:20:30z", Some(1794478830000.)),
            ("2026-11-12 10:20:30Z", Some(1794478830000.)),
            ("2026-11-12T24:00:00Z", Some(1794528000000.)),
            ("2026-11-12T24:00:01Z", None),
            ("2026-02-30T00:00:00Z", Some(1772409600000.)),
            ("2026-13-01T00:00:00Z", None),
            ("2026-11-12T10:20:60Z", None),
            ("1794484800000", None),
            ("+002026-11-12T10:20:30Z", Some(1794478830000.)),
            ("2026-11-12T10:20:30 Z", None),
            (" 2026-11-12T10:20:30Z", None),
            ("2026-11-12T10:20:30Z ", None),
            ("2026-11-12T10:20:30.Z", None),
            ("2026-1-2T10:20:30Z", None),
            ("2026-11-12T1:20:30Z", None),
            ("20261112T102030Z", None),
            ("2026-11-12T10:20:30-00:00", Some(1794478830000.)),
            ("2026-11-12T10:20:30+24:00", None),
            ("2026-11-12T10:20:30+05:60", None),
            ("garbage", None),
            ("T10:20:30Z", None),
            ("2026-11-12T", None),
            ("", None),
        ] {
            assert_eq!(ms(Some(stamp)), want, "{stamp:?}");
        }
    }
    #[test]
    fn older_children_fold_and_earlier_reveals() {
        let mut s = state();
        assert_eq!(
            s.rows().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            vec!["root", "ask", "run", "d5", "d20", "d40", "earlier:root"]
        );
        s.pick("earlier:root");
        s.action("enter");
        assert_eq!(s.selected.as_deref(), Some("d90"));
        assert_eq!(s.rows().len(), 8);
        s.pick("root");
        s.action("space");
        assert_eq!(s.rows().len(), 1);
        s.action("n");
        assert_eq!(s.selected.as_deref(), Some("ask"));
        assert!(s.rows().len() > 1);
    }
    #[test]
    fn active_keeps_ancestors_and_sidebar_keeps_listed_families() {
        let mut s = state();
        s.snap.agent_mut(0).status = "completed".into();
        s.snap
            .agents
            .push(agent("idle", None, "completed", 1).into());
        s.scope = "active".into();
        assert!(s.rows().iter().any(|r| r.id == "root"));
        assert!(!s.rows().iter().any(|r| r.id == "idle"));
        s.scope = "t3".into();
        assert!(s.rows().iter().any(|r| r.id == "idle"));
        std::sync::Arc::make_mut(s.snap.agents.last_mut().unwrap()).shelf = Some(Shelf {
            listed: true,
            settled: true,
            ..Shelf::default()
        });
        assert!(!s.rows().iter().any(|r| r.id == "idle"));
    }
    #[test]
    fn in_flight_tools_and_live_descendants_do_not_look_quiet() {
        let mut s = state();
        s.snap.agent_mut(0).started = Some("2026-11-12T10:00:00Z".into());
        s.snap.agent_mut(0).events.clear();
        assert!(pace(&s.snap.agents[0], s.now, false).unwrap().quiet);
        assert!(!s.paces()["root"].quiet);
        let a = s.snap.agent_mut(2);
        a.events.clear();
        a.now = Some(Item {
            kind: "command_execution".into(),
            started_at: Some("2026-11-12T11:00:00Z".into()),
            ..Item::default()
        });
        let p = pace(a, s.now, false).unwrap();
        assert!(p.stalled && !p.quiet && p.tool_ms == 3_600_000.0);
        a.now.as_mut().unwrap().kind = "subagent".into();
        let p = pace(a, s.now, false).unwrap();
        assert!(p.delegating && !p.stalled && !p.quiet);
    }
    #[test]
    fn selection_before_request_does_not_acknowledge_and_help_freezes_clock() {
        let mut s = state();
        s.pick("run");
        assert!(!s.acked.contains("run"));
        let mut snap = s.snap.clone();
        let run = snap.agents.iter().position(|a| a.id == "run").unwrap();
        snap.agent_mut(run).status = "waiting".into();
        snap.at += 1000.;
        s.replace(snap);
        assert!(!s.acked.contains("run"));
        s.pick("run");
        assert!(s.acked.contains("run"));
        s.help = true;
        let frozen = s.now;
        let mut snap = s.snap.clone();
        snap.at += 2000.;
        s.replace(snap);
        assert_eq!(s.now, frozen);
        assert!(s.snap.at > frozen);
    }
    #[test]
    fn help_routes_navigation_but_keeps_host_shortcuts_and_event_timestamps() {
        let mut s = state();
        let t = s.now;
        s.advance_frame(t, false);
        s.action_at("?", t + 100.);
        s.advance_frame(t + 100., false);
        let selected = s.selected.clone();
        s.action_at("down", t + 200.);
        assert_eq!(s.selected, selected);
        s.action_at("a", t + 300.);
        assert_eq!(s.scope, "active");
        assert_eq!(s.notice_at, t + 300.);
        s.action_at("m", t + 400.);
        assert!(!s.calm);
        s.advance_frame(t + 900., false);
        assert_eq!(s.now, t + 100.);
        s.action_at("escape", t + 1000.);
        s.advance_frame(t + 1000., false);
        assert_eq!(s.now, t + 1000.);
        assert!(s.frozen_at.is_none());
    }
    #[test]
    fn unrelated_keys_do_not_extend_notices_and_child_opens_parent() {
        let mut s = state();
        let t = s.now;
        s.action_at("x", t + 100.);
        assert_eq!(s.notice_at, t + 100.);
        s.action_at("down", t + 500.);
        assert_eq!(s.notice_at, t + 100.);
        s.snap.agent_mut(1).thread_id = "root".into();
        s.pick_at("ask", t + 600.);
        s.action_at("enter", t + 700.);
        assert_eq!(
            s.notice.as_deref(),
            Some("Demo: would open root in T3 Code")
        );
        assert_eq!(s.notice_at, t + 700.);
    }
    #[test]
    fn retargeted_context_starts_from_current_value_and_reduced_mode_snaps() {
        let mut s = state();
        s.calm = false;
        let t = s.now;
        s.snap.agent_mut(0).context.used_tokens = Some(50.);
        s.snap.agent_mut(0).context.max_tokens = Some(100.);
        s.advance_frame(t, false);
        assert_eq!(s.tween["root"], 0.);
        s.advance_frame(t + 100., false);
        let first = s.tween["root"];
        assert!((first - 0.5 * (1. - (-0.7f64).exp())).abs() < 1e-12);
        s.snap.agent_mut(0).context.used_tokens = Some(10.);
        s.advance_frame(t + 200., false);
        assert!((s.tween["root"] - (first + (0.1 - first) * (1. - (-0.7f64).exp()))).abs() < 1e-12);
        s.advance_frame(t + 300., true);
        assert_eq!(s.tween["root"], 0.1);
    }
    #[test]
    fn arrivals_only_shimmer_for_tools_and_effect_phase_survives_toggles() {
        let mut s = state();
        let t = s.now;
        s.snap.agent_mut(0).items.push(Item {
            kind: "user_message".into(),
            updated_at: "one".into(),
            ..Item::default()
        });
        s.observe_at(t + 100.);
        assert!(!s.shimmer.contains_key("root"));
        s.snap.agent_mut(0).items.push(Item {
            kind: "command_execution".into(),
            updated_at: "two".into(),
            ..Item::default()
        });
        s.observe_at(t + 200.);
        assert_eq!(s.shimmer.get("root"), Some(&(t + 200.)));
        s.observe_at(t + 700.);
        assert!(!s.shimmer.contains_key("root"));
        s.calm = false;
        s.fx = true;
        s.advance_frame(t + 800., false);
        assert_eq!(s.fx_elapsed, 800.);
        s.fx = false;
        s.advance_frame(t + 900., false);
        assert_eq!(s.fx_elapsed, 800.);
        s.fx = true;
        s.help = true;
        s.advance_frame(t + 1000., false);
        assert_eq!(s.fx_elapsed, 900.);
        s.advance_frame(t + 1100., true);
        assert_eq!(s.fx_elapsed, 900.);
    }
    #[test]
    fn lanes_reject_future_and_mark_failed_and_prompt() {
        let events = vec![
            Event {
                t: 10000.0,
                cat: "you".into(),
                failed: false,
            },
            Event {
                t: 11000.0,
                cat: "shell".into(),
                failed: true,
            },
            Event {
                t: 999999.0,
                cat: "shell".into(),
                failed: false,
            },
        ];
        let d = lane(&events, &[(9000.0, None)], 4, 4000.0, 12000.0);
        assert_eq!(d.counts, vec![0, 0, 1, 0]);
        assert!(d.you[1] && d.fails[2]);
        assert!(d.busy.iter().all(|x| *x));
    }
    #[test]
    fn cycles_and_orphans_are_bounded() {
        let mut s = state();
        s.snap
            .agents
            .push(agent("orphan", Some("missing"), "running", 0).into());
        s.snap
            .agents
            .push(agent("a", Some("b"), "running", 0).into());
        s.snap
            .agents
            .push(agent("b", Some("a"), "running", 0).into());
        s.scope = "active".into();
        assert!(!s.rows().iter().any(|r| r.id == "orphan"));
    }
}

#[cfg(test)]
mod label_tests {
    #[test]
    fn item_key_is_the_joined_fields() {
        let mut item = super::Item::default();
        let reference = |i: &super::Item| {
            format!(
                "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
                i.thread_id,
                i.run_id.as_deref().unwrap_or(""),
                i.node_id.as_deref().unwrap_or(""),
                i.kind,
                i.tool.as_deref().unwrap_or(""),
                i.started_at.as_deref().unwrap_or(&i.updated_at),
                i.command,
                i.file
            )
        };
        assert_eq!(super::item_key(&item), reference(&item));
        item.thread_id = "t-1".into();
        item.run_id = Some("r".into());
        item.kind = "command_execution".into();
        item.updated_at = "2026-11-20T00:00:00Z".into();
        item.command = "ls\0x ✓".into();
        assert_eq!(super::item_key(&item), reference(&item));
        item.started_at = Some("2026-11-19T23:59:59Z".into());
        item.tool = Some("bash".into());
        item.node_id = Some("n".into());
        item.file = "a/b".into();
        let mut reused = String::from("left over");
        super::item_key_into(&item, &mut reused);
        assert_eq!(reused, reference(&item));
    }
    use super::*;
    fn item(kind: &str, label: &str, command: &str, file: &str) -> Item {
        Item {
            kind: kind.into(),
            label: label.into(),
            command: command.into(),
            file: file.into(),
            ..Item::default()
        }
    }
    /// `view.ts`'s `itemLabel`: the home folder at the start of a command or file, the first
    /// occurrence anywhere in another label, the tidied first line, and the type's words.
    #[test]
    fn labels_shorten_the_home_folder_as_the_original_does() {
        let home = "/home/user";
        let command = |c: &str| item("command_execution", "", c, "").display(home);
        assert_eq!(command("/home/user/bin/app"), "~/bin/app");
        // A path that only begins with the same characters is shortened too: the original
        // matches the string prefix, not a path segment.
        assert_eq!(command("/home/username/app"), "~name/app");
        // The replacement happens before the trim, so leading space prevents it.
        assert_eq!(command("  /home/user/app"), "/home/user/app");
        // Only the first line is read, and a line that is empty or blank falls back.
        assert_eq!(command("/home/user/app\nsecond"), "~/app");
        assert_eq!(command(""), "command");
        assert_eq!(command("   "), "command");
        assert_eq!(command("\nsecond"), "command");
        // A command's home folder in the middle is left alone.
        assert_eq!(command("run /home/user/app"), "run /home/user/app");
        assert_eq!(item("file_change", "", "", "/home/user/src/a.rs").display(home), "~/src/a.rs");
        assert_eq!(item("file_change", "", "", " ").display(home), "file change");
        assert_eq!(item("reasoning", "ignored", "", "").display(home), "thinking");
        assert_eq!(item("assistant_message", "", "", "").display(home), "writing reply");
        assert_eq!(item("web_search", "", "", "").display(home), "web search");
        assert_eq!(item("unknown_widget", "", "", "").display(home), "unknown widget");
        // The first occurrence anywhere is replaced, and only the first.
        assert_eq!(item("web_search", "a /home/user/b /home/user/c", "", "").display(home), "a ~/b /home/user/c");
        // `tidy` shortens the leading one; the replacement then finds the next occurrence.
        assert_eq!(item("web_search", "/home/user/a /home/user/b", "", "").display(home), "~/a ~/b");
        assert_eq!(item("web_search", "   ", "", "").display(home), "web search");
        assert_eq!(item("web_search", "web\u{200b}search", "", "").display(home), "web search");
    }
    /// A home folder whose characters are special to a regular expression is matched literally,
    /// and `HOME` unset is its own case: `/^$/` turns an empty line into `~`, and
    /// `"x".replace("", "~")` puts `~` in front of every label.
    #[test]
    fn regex_characters_and_an_unset_home_match_the_original() {
        let home = "/home/user.name[x]";
        assert_eq!(item("command_execution", "", "/home/user.name[x]/a", "").display(home), "~/a");
        assert_eq!(
            item("command_execution", "", "/home/userXname[x]/a", "").display(home),
            "/home/userXname[x]/a"
        );
        assert_eq!(item("web_search", "/home/userXname[x]/a", "", "").display(home), "/home/userXname[x]/a");
        assert_eq!(item("web_search", "/home/user.name[x]/a", "", "").display(home), "~/a");
        assert_eq!(item("command_execution", "", "", "").display(""), "~");
        assert_eq!(item("command_execution", "", "   ", "").display(""), "command");
        assert_eq!(item("web_search", "label", "", "").display(""), "~label");
        assert_eq!(item("web_search", "", "", "").display(""), "~~");
        assert_eq!(item("web_search", "  ", "", "").display(""), "~");
    }
    #[test]
    fn replace_first_matches_javascript() {
        assert_eq!(replace_first("abc", "", "~"), "~abc");
        assert_eq!(replace_first("", "", "~"), "~");
        assert_eq!(replace_first("aXbXc", "X", "~"), "a~bXc");
        assert_eq!(replace_first("abc", "z", "~"), "abc");
        assert_eq!(js_trim("\u{feff} x \u{85}"), "x \u{85}");
    }
    #[test]
    fn typed_agent_fields_preserve_exact_json_wire_parity() {
        let samples = [
            json!(null),
            json!({}),
            json!({"id": "run-1"}),
            json!({"id": null}),
            json!({"provider": "anthropic"}),
            json!({
                "id": "r1",
                "threadId": "t1",
                "status": "running",
                "providerThreadId": "pt1",
                "startedAt": "2026-11-12T10:00:00Z",
                "requestedAt": "2026-11-12T09:59:59Z",
                "completedAt": null,
                "modelSelection": {
                    "model": "claude-opus-4-6",
                    "options": [
                        {"id": "reasoningEffort", "value": "high"},
                        {"id": "thinking", "value": true},
                        {"id": "budget", "value": 16000},
                        {"id": "ratio", "value": 0.5},
                        {"id": "nested", "value": {"a": 1}}
                    ]
                }
            }),
            json!({
                "id": "r2",
                "threadId": 5,
                "status": "completed",
                "providerThreadId": null,
                "startedAt": null,
                "requestedAt": null,
                "completedAt": null,
                "modelSelection": null
            }),
        ];
        for sample in &samples {
            let typed = AgentRun::from_value(sample);
            let round = serde_json::to_value(&typed).unwrap();
            assert_eq!(&round, sample, "AgentRun roundtrip mismatch for {sample}");
            let de: AgentRun = serde_json::from_value(sample.clone()).unwrap();
            assert_eq!(de, typed);
            assert_eq!(serde_json::to_string(&typed).unwrap(), serde_json::to_string(sample).unwrap());
        }
        assert!(matches!(AgentRun::from_value(&samples[5]), AgentRun::Projected(_)));
        assert_eq!(AgentRun::from_value(&samples[5]).id(), Some("r1"));
        assert!(matches!(AgentRun::from_value(&samples[6]), AgentRun::Raw(_)));
        assert_eq!(AgentRun::from_value(&samples[4]).into_status_view(), AgentRun::ProviderOnly("anthropic".into()));
        assert_eq!(AgentRun::from_value(&samples[5]).into_status_view(), AgentRun::Empty);

        let turn_samples = [
            json!(null),
            json!({}),
            json!({
                "providerThreadId": "pt1",
                "model": "gpt-5",
                "tokenUsage": {
                    "usedTokens": 12000,
                    "maxTokens": 200000,
                    "inputTokens": 9000,
                    "cachedInputTokens": 4000,
                    "outputTokens": 1200,
                    "reasoningOutputTokens": 300,
                    "promptCacheExpiresAt": "2026-11-12T12:05:00Z",
                    "promptCacheTtlMs": 300000,
                    "totalTokens": 10200,
                    "cacheCreationInputTokens": 500,
                    "updatedAt": null
                },
                "turnTokenUsage": {
                    "inputTokens": 1000,
                    "cachedInputTokens": 200,
                    "outputTokens": 100,
                    "reasoningTokens": 50,
                    "usageStatus": "final",
                    "hasSubagents": true
                }
            }),
            json!({
                "providerThreadId": "pt2",
                "model": null,
                "tokenUsage": {"usedTokens": 1, "weird": [1, 2]},
                "turnTokenUsage": null
            }),
        ];
        for sample in &turn_samples {
            let typed = AgentTurn::from_value(sample);
            let round = serde_json::to_value(&typed).unwrap();
            assert_eq!(&round, sample, "AgentTurn roundtrip mismatch for {sample}");
            let de: AgentTurn = serde_json::from_value(sample.clone()).unwrap();
            assert_eq!(de, typed);
            assert_eq!(serde_json::to_string(&typed).unwrap(), serde_json::to_string(sample).unwrap());
        }
        let proj_turn = AgentTurn::from_value(&turn_samples[2]);
        assert!(matches!(proj_turn, AgentTurn::Projected(_)));
        let u = proj_turn.turn_token_usage_obj().unwrap();
        assert_eq!(u.input_tokens(), 1000.0);
        assert_eq!(u.cached_input_tokens(), 200.0);
        assert_eq!(u.output_tokens(), 100.0);
        assert_eq!(u.reasoning_tokens(), Some(50.0));
        assert_eq!(u.usage_status(), Some("final"));
        assert!(u.has_subagents());

        let remote_samples = [
            json!({"environmentId": "machine-b", "threadId": "t1"}),
            json!({"environmentId": "machine-b", "threadId": "t1", "label": "Remote Box"}),
            json!({"environmentId": "machine-b", "threadId": "t1", "label": null}),
            json!({"environmentId": "machine-b", "threadId": "t1", "extra": 1}),
        ];
        for sample in &remote_samples {
            let typed = AgentRemote::from_value(sample);
            let round = serde_json::to_value(&typed).unwrap();
            assert_eq!(&round, sample, "AgentRemote roundtrip mismatch for {sample}");
            assert_eq!(serde_json::to_string(&typed).unwrap(), serde_json::to_string(sample).unwrap());
        }
        assert_eq!(AgentRemote::from_value(&remote_samples[0]).thread_ref(), Some(("machine-b", "t1")));
        assert_eq!(AgentRemote::from_value(&remote_samples[1]).label(), Some("Remote Box"));
    }
}

/// Projection metadata identity; never includes prompt, message, or result bodies.
pub fn item_key(i: &Item) -> String {
    let mut key = String::new();
    item_key_into(i, &mut key);
    key
}
/// [`item_key`] written into `key`, replacing what it held, so a caller can reuse one buffer.
pub fn item_key_into(i: &Item, key: &mut String) {
    let parts = [
        i.thread_id.as_str(),
        i.run_id.as_deref().unwrap_or(""),
        i.node_id.as_deref().unwrap_or(""),
        i.kind.as_str(),
        i.tool.as_deref().unwrap_or(""),
        i.started_at.as_deref().unwrap_or(&i.updated_at),
        i.command.as_str(),
        i.file.as_str(),
    ];
    key.clear();
    key.reserve(parts.iter().map(|p| p.len() + 1).sum());
    for (n, part) in parts.iter().enumerate() {
        if n > 0 {
            key.push('\0');
        }
        key.push_str(part);
    }
}
