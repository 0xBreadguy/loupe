//! T3's automatic worktree cleanup rules, read without changing anything.
//!
//! Loupe reads only the worktree cleanup keys of T3's `userdata/settings.json` (the file also
//! holds speech API keys and provider settings, which are skipped while parsing and never kept),
//! ports T3's resolver word for word, and reads a few timestamps per thread so Storage can say
//! what a rule would make eligible. It never writes the file and never removes anything.
//!
//! Every rule here comes from T3 Code 0.1.13-nightly.20261125 (`app.asar`):
//! - `resolveWorktreeCleanup` and `resolveProjectOverrides`: `packages/shared/src/projectSettings.ts`.
//! - `StorageCleanupSettings`, `WorktreeCleanupRules`, `WorktreeCleanup`, `StorageRetentionDays`
//!   (1..3650 or null), `SettledWorktreeRetentionDays` (0..3650 or null), `WorktreeKeepWhen`,
//!   `SidebarAutoSettleAfterDays` (1..90 or null, default 7), `sidebarAutoSettleOnMerge`
//!   (default true): `packages/contracts/src/settings.ts`.
//! - `storageCleanupActivityAt` = max(createdAt, latestUserMessageAt, latestRunRequestedAt,
//!   latestRunStartedAt, latestRunCompletedAt); the settled clock is
//!   `now - max(settledAt, activityAt)`; inactive is `activityAt < now - days`; deleted threads
//!   are removed only by `worktreeOnDelete`: `apps/server/src/storageCleanup.ts`.
//! - `resolveWorktreesDirectory` / `managedWorktreesDirectories`: `apps/server/src/vcs`.

use crate::storage_view::data::{
    Bytes, Cleanup, CleanupProblem, CleanupState, Count, Due, EntryCleanup, Estimate, Inventory,
    Next, Ownership, ProjectRule, Scan, Suggestion, Tally, ThreadState,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

const DAY_MS: f64 = 86_400_000.;
/// The settings file is capped like Telemetry Check's read of the same file.
const SETTINGS_CAP: u64 = 4 * 1024 * 1024;
/// A rule that frees less than this beyond the current rules is not called loose.
pub const LOOSE_BYTES: u64 = 1_000_000_000;
/// The owner's choice (2026-11-26): suggest settled after 7 days while auto-settle is on,
/// otherwise inactive after 14 days.
pub const SUGGEST_SETTLED_DAYS: u16 = 7;
pub const SUGGEST_INACTIVE_DAYS: u16 = 14;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepWhen {
    AnyLocalFiles,
    UncommittedChanges,
    TrackedChanges,
}

impl KeepWhen {
    pub fn word(self) -> &'static str {
        match self {
            Self::AnyLocalFiles => "any-local-files",
            Self::UncommittedChanges => "uncommitted-changes",
            Self::TrackedChanges => "tracked-changes",
        }
    }
}

/// The six worktree fields T3 resolves for one project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rules {
    pub keep_when: KeepWhen,
    /// Remove N days after the last activity.
    pub after_days: Option<u16>,
    /// Remove N days after the thread settled (or its last activity, if later).
    pub settled_after_days: Option<u16>,
    pub on_merge: bool,
    pub on_delete: bool,
    pub unchanged: bool,
}

impl Default for Rules {
    /// `StorageCleanupSettings` decoded from `{}`: every rule off.
    fn default() -> Self {
        Self {
            keep_when: KeepWhen::UncommittedChanges,
            after_days: None,
            settled_after_days: None,
            on_merge: false,
            on_delete: false,
            unchanged: false,
        }
    }
}

impl Rules {
    /// T3's `worktreeCleanupEnabled`: any of the five triggers is set.
    pub fn enabled(&self) -> bool {
        self.after_days.is_some()
            || self.settled_after_days.is_some()
            || self.on_merge
            || self.on_delete
            || self.unchanged
    }

    fn with(self, suggestion: Suggestion) -> Self {
        let tighter = |current: Option<u16>, days| Some(current.map_or(days, |c| c.min(days)));
        match suggestion {
            Suggestion::Settled(days) => Self {
                settled_after_days: tighter(self.settled_after_days, days),
                ..self
            },
            Suggestion::Inactive(days) => Self {
                after_days: tighter(self.after_days, days),
                ..self
            },
        }
    }

    fn suggested(suggestion: Suggestion) -> Self {
        Self::default().with(suggestion)
    }
}

/// `WorktreeCleanup`: `null | {mode:"off"} | {mode:"custom", rules}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    Inherit,
    Off,
    Custom(Rules),
}

/// The allowlisted cleanup keys, decoded with T3's defaults. Nothing else is kept.
#[derive(Clone, Debug, PartialEq)]
pub struct CleanupSettings {
    /// Top-level `worktreeCleanup`. The resolver starts from it.
    pub top: Policy,
    /// The six `storageCleanup.worktree*` fields.
    pub storage: Rules,
    /// `projectSettingsOverrides.<id>.worktreeCleanup`, only where the key is present.
    pub overrides: BTreeMap<String, Policy>,
    pub auto_settle_days: Option<f64>,
    pub auto_settle_on_merge: bool,
    pub worktrees_directory: String,
    pub previous_worktrees_directories: Vec<String>,
}

impl Default for CleanupSettings {
    fn default() -> Self {
        Self {
            top: Policy::Inherit,
            storage: Rules::default(),
            overrides: BTreeMap::new(),
            auto_settle_days: Some(7.),
            auto_settle_on_merge: true,
            worktrees_directory: String::new(),
            previous_worktrees_directories: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RulesRead {
    /// No settings file: T3's defaults, which remove nothing.
    Absent,
    /// An error category only, never file content.
    Unreadable(String),
    /// A key name and the constraint it breaks. The value is never kept.
    Invalid {
        key: &'static str,
        constraint: &'static str,
    },
    Ok(CleanupSettings),
}

impl RulesRead {
    pub fn settings(&self) -> Option<std::borrow::Cow<'_, CleanupSettings>> {
        match self {
            Self::Absent => Some(std::borrow::Cow::Owned(CleanupSettings::default())),
            Self::Ok(settings) => Some(std::borrow::Cow::Borrowed(settings)),
            _ => None,
        }
    }
}

/// T3's `resolveWorktreeCleanup`, word for word. A project override replaces the top-level
/// policy whenever its key is present, so an explicit `null` there means `storageCleanup`
/// even when the top level is off or custom (`resolveProjectOverrides`).
pub fn resolve(settings: &CleanupSettings, project: Option<&str>) -> Rules {
    let policy = project
        .and_then(|id| settings.overrides.get(id))
        .unwrap_or(&settings.top);
    match policy {
        Policy::Custom(rules) => *rules,
        Policy::Off => Rules {
            keep_when: settings.storage.keep_when,
            ..Rules::default()
        },
        Policy::Inherit => settings.storage,
    }
}

// ── settings.json (allowlist) ────────────────────────────────────────────────

/// Keeps a present JSON `null` apart from an absent key.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawSettings {
    #[serde(default, deserialize_with = "present")]
    worktree_cleanup: Option<Value>,
    #[serde(default)]
    storage_cleanup: Option<RawStorage>,
    #[serde(default)]
    project_settings_overrides: Option<BTreeMap<String, RawOverride>>,
    #[serde(default, deserialize_with = "present")]
    sidebar_auto_settle_after_days: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    sidebar_auto_settle_on_merge: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    worktrees_directory: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    previous_worktrees_directories: Option<Value>,
}

/// Only the six worktree fields. `browserArtifactsAfterDays` and `logsAfterDays` are skipped.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawStorage {
    #[serde(default, deserialize_with = "present")]
    worktree_keep_when: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    worktree_after_days: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    worktree_settled_after_days: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    worktree_on_merge: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    worktree_on_delete: Option<Value>,
    #[serde(default, deserialize_with = "present")]
    worktree_unchanged: Option<Value>,
}

/// Only `worktreeCleanup`. Every other override key is skipped.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawOverride {
    #[serde(default, deserialize_with = "present")]
    worktree_cleanup: Option<Value>,
}

type Invalid = (&'static str, &'static str);

const DAYS_1: &str = "is not a whole number from 1 to 3650 or null";
const DAYS_0: &str = "is not a whole number from 0 to 3650 or null";
const BOOL: &str = "is not true or false";
const KEEP: &str = "is not any-local-files, uncommitted-changes or tracked-changes";
const POLICY: &str = "is not null, {mode: off} or {mode: custom, rules}";
const MISSING: &str = "is missing";

fn days(
    value: Option<&Value>,
    min: u16,
    key: &'static str,
    default: Option<u16>,
) -> Result<Option<u16>, Invalid> {
    let constraint = if min == 0 { DAYS_0 } else { DAYS_1 };
    match value {
        None => Ok(default),
        Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => {
            let f = n.as_f64().ok_or((key, constraint))?;
            if f.fract() == 0. && f >= f64::from(min) && f <= 3650. {
                Ok(Some(f as u16))
            } else {
                Err((key, constraint))
            }
        }
        Some(_) => Err((key, constraint)),
    }
}

fn boolean(
    value: Option<&Value>,
    key: &'static str,
    default: Option<bool>,
) -> Result<bool, Invalid> {
    match value {
        None => default.ok_or((key, MISSING)),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err((key, BOOL)),
    }
}

fn keep_when(value: Option<&Value>, key: &'static str) -> Result<KeepWhen, Invalid> {
    match value.map(|v| v.as_str()) {
        None => Ok(KeepWhen::UncommittedChanges),
        Some(Some("any-local-files")) => Ok(KeepWhen::AnyLocalFiles),
        Some(Some("uncommitted-changes")) => Ok(KeepWhen::UncommittedChanges),
        Some(Some("tracked-changes")) => Ok(KeepWhen::TrackedChanges),
        Some(_) => Err((key, KEEP)),
    }
}

/// `WorktreeCleanupRules`: keepWhen and settledAfterDays have defaults; the other four are
/// required, as in T3's schema.
fn custom_rules(value: &Value, scope: Scope) -> Result<Rules, Invalid> {
    let Value::Object(map) = value else {
        return Err((scope.key("rules"), POLICY));
    };
    Ok(Rules {
        keep_when: keep_when(map.get("worktreeKeepWhen"), scope.key("worktreeKeepWhen"))?,
        after_days: match map.get("worktreeAfterDays") {
            None => return Err((scope.key("worktreeAfterDays"), MISSING)),
            value => days(value, 1, scope.key("worktreeAfterDays"), None)?,
        },
        settled_after_days: days(
            map.get("worktreeSettledAfterDays"),
            0,
            scope.key("worktreeSettledAfterDays"),
            None,
        )?,
        on_merge: boolean(
            map.get("worktreeOnMerge"),
            scope.key("worktreeOnMerge"),
            None,
        )?,
        on_delete: boolean(
            map.get("worktreeOnDelete"),
            scope.key("worktreeOnDelete"),
            None,
        )?,
        unchanged: boolean(
            map.get("worktreeUnchanged"),
            scope.key("worktreeUnchanged"),
            None,
        )?,
    })
}

/// Static key names, so a message never carries a project id.
#[derive(Clone, Copy)]
enum Scope {
    Global,
    Project,
}

impl Scope {
    fn key(self, field: &str) -> &'static str {
        match (self, field) {
            (Self::Global, "policy") => "worktreeCleanup",
            (Self::Project, "policy") => "projectSettingsOverrides.*.worktreeCleanup",
            (Self::Global, "rules") => "worktreeCleanup.rules",
            (Self::Project, "rules") => "projectSettingsOverrides.*.worktreeCleanup.rules",
            (Self::Global, "worktreeKeepWhen") => "worktreeCleanup.rules.worktreeKeepWhen",
            (Self::Global, "worktreeAfterDays") => "worktreeCleanup.rules.worktreeAfterDays",
            (Self::Global, "worktreeSettledAfterDays") => {
                "worktreeCleanup.rules.worktreeSettledAfterDays"
            }
            (Self::Global, "worktreeOnMerge") => "worktreeCleanup.rules.worktreeOnMerge",
            (Self::Global, "worktreeOnDelete") => "worktreeCleanup.rules.worktreeOnDelete",
            (Self::Global, "worktreeUnchanged") => "worktreeCleanup.rules.worktreeUnchanged",
            (Self::Project, "worktreeKeepWhen") => {
                "projectSettingsOverrides.*.worktreeCleanup.rules.worktreeKeepWhen"
            }
            (Self::Project, "worktreeAfterDays") => {
                "projectSettingsOverrides.*.worktreeCleanup.rules.worktreeAfterDays"
            }
            (Self::Project, "worktreeSettledAfterDays") => {
                "projectSettingsOverrides.*.worktreeSettledAfterDays"
            }
            (Self::Project, "worktreeOnMerge") => {
                "projectSettingsOverrides.*.worktreeCleanup.rules.worktreeOnMerge"
            }
            (Self::Project, "worktreeOnDelete") => {
                "projectSettingsOverrides.*.worktreeCleanup.rules.worktreeOnDelete"
            }
            (Self::Project, "worktreeUnchanged") => {
                "projectSettingsOverrides.*.worktreeCleanup.rules.worktreeUnchanged"
            }
            _ => "worktreeCleanup",
        }
    }
}

fn policy(value: &Value, scope: Scope) -> Result<Policy, Invalid> {
    match value {
        Value::Null => Ok(Policy::Inherit),
        Value::Object(map) => match map.get("mode").and_then(Value::as_str) {
            Some("off") => Ok(Policy::Off),
            Some("custom") => {
                let rules = map.get("rules").ok_or((scope.key("rules"), MISSING))?;
                custom_rules(rules, scope).map(Policy::Custom)
            }
            _ => Err((scope.key("policy"), POLICY)),
        },
        _ => Err((scope.key("policy"), POLICY)),
    }
}

/// Decode the allowlisted keys with T3's defaults and range checks. Values outside T3's
/// schema are reported by key and constraint, never clamped.
pub fn parse(json: &str) -> RulesRead {
    let raw: RawSettings = match serde_json::from_str::<Value>(json) {
        Ok(Value::Object(_)) => match serde_json::from_str(json) {
            Ok(raw) => raw,
            Err(_) => {
                return RulesRead::Invalid {
                    key: "projectSettingsOverrides",
                    constraint: "is not an object of project settings",
                };
            }
        },
        Ok(_) => {
            return RulesRead::Invalid {
                key: "settings.json",
                constraint: "is not a JSON object",
            };
        }
        Err(_) => {
            return RulesRead::Invalid {
                key: "settings.json",
                constraint: "is not valid JSON",
            };
        }
    };
    match decode(raw) {
        Ok(settings) => RulesRead::Ok(settings),
        Err((key, constraint)) => RulesRead::Invalid { key, constraint },
    }
}

fn decode(raw: RawSettings) -> Result<CleanupSettings, Invalid> {
    let top = match &raw.worktree_cleanup {
        None => Policy::Inherit,
        Some(value) => policy(value, Scope::Global)?,
    };
    let s = raw.storage_cleanup.unwrap_or_default();
    let storage = Rules {
        keep_when: keep_when(
            s.worktree_keep_when.as_ref(),
            "storageCleanup.worktreeKeepWhen",
        )?,
        after_days: days(
            s.worktree_after_days.as_ref(),
            1,
            "storageCleanup.worktreeAfterDays",
            None,
        )?,
        settled_after_days: days(
            s.worktree_settled_after_days.as_ref(),
            0,
            "storageCleanup.worktreeSettledAfterDays",
            None,
        )?,
        on_merge: boolean(
            s.worktree_on_merge.as_ref(),
            "storageCleanup.worktreeOnMerge",
            Some(false),
        )?,
        on_delete: boolean(
            s.worktree_on_delete.as_ref(),
            "storageCleanup.worktreeOnDelete",
            Some(false),
        )?,
        unchanged: boolean(
            s.worktree_unchanged.as_ref(),
            "storageCleanup.worktreeUnchanged",
            Some(false),
        )?,
    };
    let mut overrides = BTreeMap::new();
    for (project, entry) in raw.project_settings_overrides.unwrap_or_default() {
        if let Some(value) = &entry.worktree_cleanup {
            overrides.insert(project, policy(value, Scope::Project)?);
        }
    }
    let auto_settle_days = match &raw.sidebar_auto_settle_after_days {
        None => Some(7.),
        Some(Value::Null) => None,
        Some(Value::Number(n)) => match n.as_f64() {
            Some(f) if (1. ..=90.).contains(&f) => Some(f),
            _ => {
                return Err((
                    "sidebarAutoSettleAfterDays",
                    "is not a number from 1 to 90 or null",
                ));
            }
        },
        Some(_) => {
            return Err((
                "sidebarAutoSettleAfterDays",
                "is not a number from 1 to 90 or null",
            ));
        }
    };
    let auto_settle_on_merge = boolean(
        raw.sidebar_auto_settle_on_merge.as_ref(),
        "sidebarAutoSettleOnMerge",
        Some(true),
    )?;
    let worktrees_directory = match &raw.worktrees_directory {
        None => String::new(),
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(_) => return Err(("worktreesDirectory", "is not a string")),
    };
    let previous_worktrees_directories = match &raw.previous_worktrees_directories {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(|s| s.trim().to_owned()))
            .collect::<Option<Vec<_>>>()
            .ok_or(("previousWorktreesDirectories", "is not a list of strings"))?,
        Some(_) => return Err(("previousWorktreesDirectories", "is not a list of strings")),
    };
    Ok(CleanupSettings {
        top,
        storage,
        overrides,
        auto_settle_days,
        auto_settle_on_merge,
        worktrees_directory,
        previous_worktrees_directories,
    })
}

/// Read `<data folder>/userdata/settings.json` with Telemetry Check's defenses: no symlink in
/// any component, a FIFO never blocks, a regular file only, at most 4 MiB.
pub fn read(data_folder: &Path) -> RulesRead {
    use crate::safe_file::{ReadError, open_under_root_nofollow, read_bounded};
    let opened = open_under_root_nofollow(data_folder, &["userdata", "settings.json"])
        .and_then(|(file, meta)| read_bounded(file, &meta, SETTINGS_CAP));
    match opened {
        Ok(text) => parse(&text),
        Err(ReadError::Absent) => RulesRead::Absent,
        Err(ReadError::Unreadable(reason)) => RulesRead::Unreadable(reason),
        Err(ReadError::TooLarge(_)) => RulesRead::Unreadable("larger than 4 MiB".into()),
    }
}

/// A cheap stat of the settings file (device, inode, mtime, size) without following links.
/// Re-read the file only when this changes.
pub fn fingerprint(data_folder: &Path) -> Option<(u64, u64, i128, u64)> {
    crate::safe_file::sig_under_root_nofollow(data_folder, &["userdata", "settings.json"])
}

/// T3's `managedWorktreesDirectories`: the default root, `worktreesDirectory` and every
/// `previousWorktreesDirectories` entry that resolves to an absolute, non-root folder.
pub fn managed_roots(
    settings: &CleanupSettings,
    default_dir: &Path,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = vec![default_dir.to_path_buf()];
    for setting in std::iter::once(&settings.worktrees_directory)
        .chain(&settings.previous_worktrees_directories)
    {
        if let Some(dir) = resolve_worktrees_directory(setting, default_dir, home)
            && !roots.contains(&dir)
        {
            roots.push(dir);
        }
    }
    roots
}

/// Whether T3 creates new worktrees outside the folder Storage scans.
pub fn custom_root(settings: &CleanupSettings, default_dir: &Path, home: Option<&Path>) -> bool {
    resolve_worktrees_directory(&settings.worktrees_directory, default_dir, home)
        .is_some_and(|dir| dir != default_dir)
}

fn resolve_worktrees_directory(
    setting: &str,
    default_dir: &Path,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if setting.is_empty() {
        return Some(default_dir.to_path_buf());
    }
    // expandHomePath: a leading `~` followed by the end or a separator.
    let expanded = match setting.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            PathBuf::from(format!("{}{rest}", home?.display()))
        }
        _ => PathBuf::from(setting),
    };
    if !expanded.is_absolute() {
        return None;
    }
    let mut resolved = PathBuf::from("/");
    for part in expanded.components() {
        match part {
            Component::Normal(name) => resolved.push(name),
            Component::ParentDir => {
                resolved.pop();
            }
            _ => {}
        }
    }
    (resolved.parent().is_some()).then_some(resolved)
}

// ── thread timestamps (one read-only query, Storage only) ────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettledOverride {
    Settled,
    Active,
}

/// What Storage reads for each thread row that records a worktree, including archived and
/// deleted rows. No titles, prompts, messages or PR text.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadFact {
    pub thread_id: String,
    pub project_id: String,
    pub worktree_path: String,
    pub created_ms: Option<f64>,
    pub archived: bool,
    pub deleted: bool,
    pub settled_at_ms: Option<f64>,
    pub settled_override: Option<SettledOverride>,
    /// The latest of the thread's run requested, started and completed times.
    pub last_run_ms: Option<f64>,
}

impl ThreadFact {
    /// T3's inactivity clock, without `latestUserMessageAt`: a user message nearly always
    /// starts a run, and reading it would mean querying message rows.
    pub fn activity_ms(&self) -> Option<f64> {
        match (self.created_ms, self.last_run_ms) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        }
    }

    /// Settled as T3's cleanup sees it: `settledAt` is set (manual settle or auto-settle).
    pub fn settled(&self) -> bool {
        self.settled_at_ms.is_some() && self.settled_override != Some(SettledOverride::Active)
    }
}

const FACTS_SQL: &str = "SELECT t.thread_id, t.project_id, t.created_at,
       t.archived_at IS NOT NULL, t.deleted_at IS NOT NULL,
       json_extract(t.payload_json,'$.worktreePath'),
       json_extract(t.payload_json,'$.settledAt'),
       json_extract(t.payload_json,'$.settledOverride'),
       (SELECT max(r.requested_at) FROM orchestration_v2_projection_runs r WHERE r.thread_id=t.thread_id),
       (SELECT max(r.completed_at) FROM orchestration_v2_projection_runs r WHERE r.thread_id=t.thread_id),
       (SELECT max(json_extract(r.payload_json,'$.startedAt')) FROM orchestration_v2_projection_runs r WHERE r.thread_id=t.thread_id)
  FROM orchestration_v2_projection_threads t
 WHERE json_type(t.payload_json,'$.worktreePath')='text'";

/// One read-only, `query_only` query over thread rows that record a worktree. Run it on the
/// storage worker when Storage opens or rescans, never on the live reader's path.
pub fn read_thread_facts(db: &Path) -> Result<Vec<ThreadFact>, String> {
    let conn = crate::source::open_read_only(db).map_err(|_| "unreadable".to_owned())?;
    conn.busy_timeout(std::time::Duration::from_millis(250))
        .map_err(|_| "unreadable".to_owned())?;
    conn.execute_batch("PRAGMA query_only=ON")
        .map_err(|_| "unreadable".to_owned())?;
    let mut stmt = conn.prepare(FACTS_SQL).map_err(|_| "schema".to_owned())?;
    let text = |row: &rusqlite::Row<'_>, i| row.get::<_, Option<String>>(i).ok().flatten();
    let rows = stmt
        .query_map([], |row| {
            let stamp = |i| crate::model::ms(text(row, i).as_deref());
            let last_run = [stamp(8), stamp(9), stamp(10)]
                .into_iter()
                .flatten()
                .reduce(f64::max);
            Ok(ThreadFact {
                thread_id: text(row, 0).unwrap_or_default(),
                project_id: text(row, 1).unwrap_or_default(),
                created_ms: stamp(2),
                archived: row.get::<_, i64>(3).unwrap_or(0) != 0,
                deleted: row.get::<_, i64>(4).unwrap_or(0) != 0,
                worktree_path: text(row, 5).unwrap_or_default(),
                settled_at_ms: stamp(6),
                settled_override: match text(row, 7).as_deref() {
                    Some("settled") => Some(SettledOverride::Settled),
                    Some("active") => Some(SettledOverride::Active),
                    _ => None,
                },
                last_run_ms: last_run,
            })
        })
        .map_err(|_| "query failed".to_owned())?;
    let mut facts = Vec::new();
    for row in rows {
        let fact = row.map_err(|_| "query failed".to_owned())?;
        if !fact.worktree_path.is_empty() {
            facts.push(fact);
        }
    }
    Ok(facts)
}

// ── per-entry facts and the band ─────────────────────────────────────────────

/// What the estimate knows about one Storage row.
#[derive(Clone, Debug, PartialEq)]
pub struct EntryFacts {
    pub project_id: Option<String>,
    pub ownership: Ownership,
    /// T3 would consider the thread idle (no run, queue, request or background task).
    pub idle: bool,
    /// Inside one of T3's managed worktree roots.
    pub managed: bool,
    pub activity_ms: Option<f64>,
    pub settled_at_ms: Option<f64>,
}

/// Match Storage rows to thread facts. Thread rows match by thread id (`thread:<id>`), folders
/// without a live thread by worktree path, which tells archived, deleted and no-thread apart.
pub fn entry_facts(
    inventory: &Inventory,
    facts: &[ThreadFact],
    idle: &dyn Fn(&str) -> bool,
    roots: &[PathBuf],
) -> HashMap<String, EntryFacts> {
    let by_id: HashMap<&str, &ThreadFact> =
        facts.iter().map(|f| (f.thread_id.as_str(), f)).collect();
    let mut by_path: HashMap<&Path, Vec<&ThreadFact>> = HashMap::new();
    for fact in facts {
        by_path
            .entry(Path::new(&fact.worktree_path))
            .or_default()
            .push(fact);
    }
    let mut out = HashMap::new();
    let projects = inventory.projects.iter().flat_map(|p| {
        p.threads
            .iter()
            .map(move |t| (t.id.as_str(), p.id.as_str()))
    });
    let project_of: HashMap<&str, &str> = projects.collect();
    for entry in inventory
        .projects
        .iter()
        .flat_map(|p| &p.threads)
        .chain(&inventory.settled_or_gone)
    {
        let managed = entry.worktree_path.as_deref().is_some_and(|path| {
            roots
                .iter()
                .any(|root| path.starts_with(root) && path != root)
        });
        let known_project = project_of
            .get(entry.id.as_str())
            .map(|s| (*s).to_owned())
            .or_else(|| inventory.settled_projects.get(&entry.id).cloned());
        let facts = if let Some(thread) = entry.id.strip_prefix("thread:") {
            let fact = by_id.get(thread);
            EntryFacts {
                project_id: known_project.or_else(|| fact.map(|f| f.project_id.clone())),
                ownership: if entry.state == ThreadState::Settled {
                    Ownership::Settled
                } else {
                    Ownership::Active
                },
                idle: idle(thread),
                managed,
                activity_ms: fact.and_then(|f| f.activity_ms()),
                settled_at_ms: fact.filter(|f| f.settled()).and_then(|f| f.settled_at_ms),
            }
        } else {
            let group = entry
                .worktree_path
                .as_deref()
                .and_then(|path| by_path.get(path))
                .map(Vec::as_slice)
                .unwrap_or_default();
            let kept: Vec<&&ThreadFact> = group.iter().filter(|f| !f.deleted).collect();
            let (ownership, members): (Ownership, Vec<&ThreadFact>) = if group.is_empty() {
                (Ownership::NoThread, Vec::new())
            } else if kept.is_empty() {
                (Ownership::Deleted, group.to_vec())
            } else if kept.iter().all(|f| f.archived) {
                (Ownership::Archived, kept.into_iter().copied().collect())
            } else {
                // A live thread records this folder, but Storage did not tie it to one.
                (Ownership::Active, kept.into_iter().copied().collect())
            };
            let settled = members
                .iter()
                .filter(|f| f.settled())
                .filter_map(|f| f.settled_at_ms)
                .reduce(f64::max);
            EntryFacts {
                project_id: known_project.or_else(|| members.first().map(|f| f.project_id.clone())),
                ownership,
                idle: matches!(ownership, Ownership::Archived | Ownership::Deleted),
                managed,
                activity_ms: members
                    .iter()
                    .filter_map(|f| f.activity_ms())
                    .reduce(f64::max),
                settled_at_ms: settled,
            }
        };
        out.insert(entry.id.clone(), facts);
    }
    out
}

/// A folder whose thread is archived or deleted has a thread after all: drop the "unowned"
/// prefix the scanner gave it and record its project, so the detail pane and the `f` handoff
/// scope it like the other rows of that project.
pub fn attribute(inventory: &mut Inventory, facts: &HashMap<String, EntryFacts>) {
    let Inventory {
        settled_or_gone,
        settled_projects,
        ..
    } = inventory;
    for entry in settled_or_gone {
        let Some(f) = facts.get(&entry.id) else {
            continue;
        };
        if !matches!(f.ownership, Ownership::Archived | Ownership::Deleted) {
            continue;
        }
        if let Some(rest) = entry.title.strip_prefix("unowned ") {
            entry.title = rest.to_owned();
        }
        if let Some(project) = &f.project_id {
            settled_projects
                .entry(entry.id.clone())
                .or_insert_with(|| project.clone());
        }
    }
}

/// When `rules` would make an entry eligible: `Some(0.)` for now, `Some(ms)` for later, `None`
/// if no timed rule covers it.
fn due_in_ms(rules: &Rules, facts: &EntryFacts, now: f64) -> Option<f64> {
    if facts.ownership == Ownership::Deleted {
        return rules.on_delete.then_some(0.);
    }
    let activity = facts.activity_ms?;
    let mut soonest: Option<f64> = None;
    let mut consider = |at: f64| soonest = Some(soonest.map_or(at, |s| s.min(at)));
    if let Some(days) = rules.after_days {
        // Inactive: activityAt < now - days (strict).
        let at = activity + f64::from(days) * DAY_MS;
        consider(if at < now { 0. } else { (at - now).max(1.) });
    }
    if let (Some(days), Some(settled)) = (rules.settled_after_days, facts.settled_at_ms) {
        // Settled: now - max(settledAt, activityAt) >= days.
        let at = settled.max(activity) + f64::from(days) * DAY_MS;
        consider((at - now).max(0.));
    }
    soonest
}

fn candidate(facts: &EntryFacts) -> bool {
    facts.managed
        && facts.idle
        && matches!(
            facts.ownership,
            Ownership::Active | Ownership::Settled | Ownership::Archived | Ownership::Deleted
        )
}

#[derive(Default)]
struct Totals {
    now: Count,
    later: Vec<(u32, u64, bool)>,
}

impl Totals {
    fn add(&mut self, due: f64, bytes: Bytes) {
        let (n, partial) = match bytes {
            Bytes::Exact(n) => (n, false),
            Bytes::AtLeast(n) => (n, true),
            Bytes::Absent => (0, false),
            Bytes::Unknown => (0, true),
        };
        if due <= 0. {
            self.now.worktrees += 1;
            self.now.bytes = self.now.bytes.saturating_add(n);
            self.now.partial |= partial;
        } else {
            self.later.push(((due / DAY_MS).ceil() as u32, n, partial));
        }
    }

    fn tally(self) -> Tally {
        let days = self.later.iter().map(|l| l.0).min();
        let next = days.map(|days| {
            let mut count = Count::default();
            for (d, n, partial) in &self.later {
                if *d == days {
                    count.worktrees += 1;
                    count.bytes = count.bytes.saturating_add(*n);
                    count.partial |= partial;
                }
            }
            Next { days, count }
        });
        Tally {
            now: self.now,
            next,
            extra: None,
        }
    }
}

/// Everything the T3 CLEANUP band, the detail pane and the `f` handoff show. Pure.
///
/// `facts` is `None` when the thread timestamps could not be read. `projects` lists the
/// project ids Storage knows, which decides how many keep their own rule.
pub fn summarize(
    read: &RulesRead,
    inventory: &Inventory,
    facts: Option<&HashMap<String, EntryFacts>>,
    projects: &[String],
    custom_root: bool,
    now: f64,
) -> Cleanup {
    let settings = read.settings();
    let mut entries = BTreeMap::new();
    let rows: Vec<_> = inventory
        .projects
        .iter()
        .flat_map(|p| &p.threads)
        .chain(&inventory.settled_or_gone)
        .collect();

    let Some(settings) = settings else {
        if let Some(facts) = facts {
            for entry in &rows {
                if let Some(f) = facts.get(&entry.id) {
                    entries.insert(
                        entry.id.clone(),
                        EntryCleanup {
                            ownership: f.ownership,
                            due: None,
                        },
                    );
                }
            }
        }
        return Cleanup {
            state: CleanupState::Unknown(match read {
                RulesRead::Unreadable(reason) => CleanupProblem::Unreadable(reason.clone()),
                RulesRead::Invalid { key, constraint } => {
                    CleanupProblem::Invalid { key, constraint }
                }
                _ => CleanupProblem::Unreadable("unknown".into()),
            }),
            global: None,
            own_rules: 0,
            project_rules: BTreeMap::new(),
            suggestion: None,
            estimate: Estimate::NoTimes,
            build_kept: false,
            custom_root,
            auto_settle_days: None,
            entries,
        };
    };

    let global = resolve(&settings, None);
    let own: Vec<&String> = projects
        .iter()
        .filter(|p| settings.overrides.contains_key(*p))
        .collect();
    let project_rules = own
        .iter()
        .map(|id| {
            let rule = match settings.overrides[*id] {
                Policy::Off => ProjectRule::Off(resolve(&settings, Some(id))),
                Policy::Custom(rules) => ProjectRule::Custom(rules),
                Policy::Inherit => ProjectRule::Storage(settings.storage),
            };
            ((*id).clone(), rule)
        })
        .collect();
    let follows_global =
        |project: Option<&str>| project.is_none_or(|p| !settings.overrides.contains_key(p));
    let suggestion = (projects.is_empty() || own.len() < projects.len()).then_some(
        if settings.auto_settle_days.is_some() {
            Suggestion::Settled(SUGGEST_SETTLED_DAYS)
        } else {
            Suggestion::Inactive(SUGGEST_INACTIVE_DAYS)
        },
    );

    let estimate_state = if matches!(inventory.scan, Scan::Unavailable) {
        Some(Estimate::NoSizes)
    } else if inventory.scan.scanning() && rows.is_empty() {
        Some(Estimate::Estimating)
    } else if facts.is_none() {
        Some(Estimate::NoTimes)
    } else {
        None
    };

    let mut effective = Totals::default();
    let mut suggested = Totals::default();
    let mut combined = Totals::default();
    let mut build_kept = false;
    if let Some(facts) = facts {
        for entry in &rows {
            let Some(f) = facts.get(&entry.id) else {
                continue;
            };
            let rules = resolve(&settings, f.project_id.as_deref());
            let due = if !rules.enabled() {
                None
            } else if f.ownership == Ownership::NoThread {
                Some(Due::Never)
            } else if !f.managed {
                Some(Due::Outside)
            } else if !f.idle {
                Some(Due::Busy)
            } else {
                Some(match due_in_ms(&rules, f, now) {
                    Some(ms) if ms <= 0. => Due::Now,
                    Some(ms) => Due::InDays((ms / DAY_MS).ceil() as u32),
                    None => Due::NotCovered,
                })
            };
            entries.insert(
                entry.id.clone(),
                EntryCleanup {
                    ownership: f.ownership,
                    due,
                },
            );
            if !candidate(f) {
                continue;
            }
            let bytes = entry.sizes.worktree;
            if rules.enabled() {
                // The band shows the global rules, so only their keepWhen earns the warning.
                if global.enabled()
                    && global.keep_when == KeepWhen::AnyLocalFiles
                    && follows_global(f.project_id.as_deref())
                    && matches!(entry.sizes.build, Bytes::Exact(n) | Bytes::AtLeast(n) if n > 0)
                {
                    build_kept = true;
                }
                if let Some(ms) = due_in_ms(&rules, f, now) {
                    effective.add(ms, bytes);
                }
            }
            if let (Some(s), true) = (suggestion, follows_global(f.project_id.as_deref())) {
                if let Some(ms) = due_in_ms(&Rules::suggested(s), f, now) {
                    suggested.add(ms, bytes);
                }
                if let Some(ms) = due_in_ms(&global.with(s), f, now) {
                    combined.add(ms, bytes);
                }
            }
        }
    }

    let global_now = {
        // What the current global rules free now, over projects that follow them.
        let mut t = Totals::default();
        if let (Some(facts), true) = (facts, global.enabled()) {
            for entry in &rows {
                if let Some(f) = facts
                    .get(&entry.id)
                    .filter(|f| candidate(f) && follows_global(f.project_id.as_deref()))
                    && let Some(ms) = due_in_ms(&global, f, now)
                {
                    t.add(ms, entry.sizes.worktree);
                }
            }
        }
        t.now
    };

    let (state, suggestion, estimate) = if global.enabled() {
        let combined = combined.tally();
        let extra = Count {
            worktrees: combined.now.worktrees.saturating_sub(global_now.worktrees),
            bytes: combined.now.bytes.saturating_sub(global_now.bytes),
            partial: combined.now.partial,
        };
        let loose = estimate_state.is_none() && suggestion.is_some() && extra.bytes >= LOOSE_BYTES;
        let mut tally = effective.tally();
        if loose {
            tally.extra = Some(extra);
        }
        (
            if loose {
                CleanupState::Loose
            } else {
                CleanupState::On
            },
            if loose { suggestion } else { None },
            estimate_state.unwrap_or(Estimate::Ready(tally)),
        )
    } else {
        (
            CleanupState::Off {
                defaults: matches!(read, RulesRead::Absent),
            },
            suggestion,
            estimate_state.unwrap_or(Estimate::Ready(suggested.tally())),
        )
    };

    Cleanup {
        state,
        global: Some(global),
        own_rules: own.len(),
        project_rules,
        suggestion,
        estimate,
        build_kept,
        custom_root,
        auto_settle_days: settings.auto_settle_days,
        entries,
    }
}

// ── made-up data for `--demo` ────────────────────────────────────────────────

/// Demo settings: T3's defaults (rules off) and one project with its own rule.
pub fn demo_settings() -> RulesRead {
    let mut settings = CleanupSettings::default();
    settings.overrides.insert(
        "atlas".into(),
        Policy::Custom(Rules {
            keep_when: KeepWhen::AnyLocalFiles,
            settled_after_days: Some(1),
            ..Rules::default()
        }),
    );
    RulesRead::Ok(settings)
}

/// Extra demo worktree folders whose threads are archived, deleted or gone:
/// `(folder name, project id, worktree MiB, build MiB)`.
pub const DEMO_FOLDERS: &[(&str, Option<&str>, u64, u64)] = &[
    (
        "checkout-refactor-4e1a0b3d5e7f9c2a",
        Some("shop"),
        1335,
        1050,
    ),
    ("legacy-cart-9f2c4e1a0b3d5e7f", Some("shop"), 1050, 782),
    ("export-spike-0a1b2c3d4e5f6a7b", Some("pipe"), 582, 512),
    ("t3-7c1d2e3f4a5b6c7d", None, 430, 390),
];

/// Demo thread facts relative to `now`: the settled demo thread settled 18 minutes ago, two
/// archived storefront threads settled 12 and 9 days ago, one deleted pipeline thread.
pub fn demo_facts(now: f64, folder: &dyn Fn(&str) -> String) -> Vec<ThreadFact> {
    let minutes = |m: f64| Some(now - m * 60_000.);
    let days = |d: f64| Some(now - d * DAY_MS);
    let fact = |id: &str, project: &str, path: String| ThreadFact {
        thread_id: id.into(),
        project_id: project.into(),
        worktree_path: path,
        created_ms: days(20.),
        archived: false,
        deleted: false,
        settled_at_ms: None,
        settled_override: None,
        last_run_ms: None,
    };
    vec![
        ThreadFact {
            settled_at_ms: minutes(18.),
            last_run_ms: minutes(25.),
            settled_override: Some(SettledOverride::Settled),
            ..fact("demo-docs", "book", folder("demo-docs"))
        },
        ThreadFact {
            last_run_ms: minutes(44.),
            ..fact("demo-checkout", "shop", folder("demo-checkout"))
        },
        ThreadFact {
            last_run_ms: minutes(38.),
            ..fact("demo-billing", "atlas", folder("demo-billing"))
        },
        ThreadFact {
            archived: true,
            settled_at_ms: days(12.),
            last_run_ms: days(13.),
            ..fact("demo-archived-1", "shop", folder(DEMO_FOLDERS[0].0))
        },
        ThreadFact {
            archived: true,
            settled_at_ms: days(9.),
            last_run_ms: days(9.5),
            ..fact("demo-archived-2", "shop", folder(DEMO_FOLDERS[1].0))
        },
        ThreadFact {
            deleted: true,
            last_run_ms: days(4.),
            ..fact("demo-deleted", "pipe", folder(DEMO_FOLDERS[2].0))
        },
    ]
}

#[cfg(test)]
mod tests;
