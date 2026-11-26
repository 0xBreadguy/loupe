//! Read-only T3 Code telemetry check: model, parsers, evaluator, alerts, and JSON output.
//!
//! Pure logic only — zero filesystem, environment, or process I/O lives in this file.
//! All reads happen in [`gather`], which takes explicit [`gather::Paths`], an env lookup
//! closure, and a [`gather::System`] implementation so tests never touch the host.
//!
//! Upstream T3 Code rules cited from `pingdotgg/t3code` `main` (24 Nov 2026):
//! - `packages/shared/src/otelEnvironment.ts`: `T3CODE_TRUE`/`T3CODE_FALSE`, `OTEL_SDK_DISABLED`,
//!   `T3CODE_OTEL_SDK_DISABLED`, `isRelayTracingEnabled`, `resolveSignalEndpoint`.
//! - `apps/server/src/telemetry/AnalyticsService.ts`: `T3CODE_TELEMETRY_ENABLED` (default `true`),
//!   `T3CODE_POSTHOG_HOST` (default `https://ingest.t3.codes`), policy override.
//! - `apps/server/src/policy/ManagedPolicy.ts` & `packages/shared/src/managedPolicyFile.ts`:
//!   strict schema (`onExcessProperty: "error"`), OS policy paths, `limits.telemetry === false`.
//! - `apps/server/src/serverRuntimeState.ts`: `userdata/server-runtime.json` (`version: 1`, `pid`,
//!   `startedAt` ISO string, `serviceManaged`, `serverVersion`, `managedPolicy`).
//! - `apps/server/src/cli/service.ts`: `service.env` parsed via Node `util.parseEnv`.
//! - `apps/server/src/telemetry/Identify.ts` (PR #42299, `v0.1.12-nightly.20261124.5209`):
//!   removed `~/.codex/auth.json` and `~/.claude.json` hashing.

pub mod demo;
pub mod gather;

use crate::model;
use serde::Serialize;
use serde_json::{Value, json};
use std::path::PathBuf;

pub const DEFAULT_POSTHOG_HOST: &str = "ingest.t3.codes";
const RESTART_GRACE_MS: f64 = 2_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Machine,
    Demo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    Linux,
    #[serde(rename = "macos")]
    MacOs,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Signal {
    Traces,
    Metrics,
    Logs,
}

impl Signal {
    pub const ALL: [Signal; 3] = [Signal::Traces, Signal::Metrics, Signal::Logs];

    pub fn label(self) -> &'static str {
        match self {
            Signal::Traces => "traces",
            Signal::Metrics => "metrics",
            Signal::Logs => "logs",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Stamp {
    pub modified_ms: f64,
    pub len: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Read<T> {
    Absent,
    /// `io::ErrorKind` description only — never file contents.
    Unreadable(String),
    /// Structural/schema reason only — never secret values.
    Invalid {
        reason: String,
        /// True when a rejected `policy.json` appeared to set `limits.telemetry: false`.
        attempted_telemetry_off: bool,
    },
    Ok {
        value: T,
        stamp: Stamp,
    },
}

/// Sanitized HTTP(S) endpoint keeping only `scheme://host[:port]` (no userinfo, path, or query).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
}

impl Endpoint {
    pub fn display_url(&self) -> String {
        match self.port {
            Some(port) => format!("{}://{}:{port}", self.scheme, self.host),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }

    pub fn host_port(&self) -> String {
        match self.port {
            Some(port) => format!("{}:{port}", self.host),
            None => self.host.clone(),
        }
    }

    pub fn is_loopback(&self) -> bool {
        is_sinkhole_or_loopback_host(&self.host)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bool3 {
    True,
    False,
    Unset,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EnvKey {
    TelemetryEnabled,
    T3OtelSdkDisabled,
    OtelSdkDisabled,
    PosthogHost,
    T3OtlpUrl(Signal),
    OtlpEndpoint(Option<Signal>),
    OtlpProtocol(Option<Signal>),
    OtelExporter(Signal),
}

impl EnvKey {
    pub fn name(self) -> &'static str {
        match self {
            EnvKey::TelemetryEnabled => "T3CODE_TELEMETRY_ENABLED",
            EnvKey::T3OtelSdkDisabled => "T3CODE_OTEL_SDK_DISABLED",
            EnvKey::OtelSdkDisabled => "OTEL_SDK_DISABLED",
            EnvKey::PosthogHost => "T3CODE_POSTHOG_HOST",
            EnvKey::T3OtlpUrl(Signal::Traces) => "T3CODE_OTLP_TRACES_URL",
            EnvKey::T3OtlpUrl(Signal::Metrics) => "T3CODE_OTLP_METRICS_URL",
            EnvKey::T3OtlpUrl(Signal::Logs) => "T3CODE_OTLP_LOGS_URL",
            EnvKey::OtlpEndpoint(None) => "OTEL_EXPORTER_OTLP_ENDPOINT",
            EnvKey::OtlpEndpoint(Some(Signal::Traces)) => "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            EnvKey::OtlpEndpoint(Some(Signal::Metrics)) => "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
            EnvKey::OtlpEndpoint(Some(Signal::Logs)) => "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
            EnvKey::OtlpProtocol(None) => "OTEL_EXPORTER_OTLP_PROTOCOL",
            EnvKey::OtlpProtocol(Some(Signal::Traces)) => "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL",
            EnvKey::OtlpProtocol(Some(Signal::Metrics)) => "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL",
            EnvKey::OtlpProtocol(Some(Signal::Logs)) => "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL",
            EnvKey::OtelExporter(Signal::Traces) => "OTEL_TRACES_EXPORTER",
            EnvKey::OtelExporter(Signal::Metrics) => "OTEL_METRICS_EXPORTER",
            EnvKey::OtelExporter(Signal::Logs) => "OTEL_LOGS_EXPORTER",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "T3CODE_TELEMETRY_ENABLED" => EnvKey::TelemetryEnabled,
            "T3CODE_OTEL_SDK_DISABLED" => EnvKey::T3OtelSdkDisabled,
            "OTEL_SDK_DISABLED" => EnvKey::OtelSdkDisabled,
            "T3CODE_POSTHOG_HOST" => EnvKey::PosthogHost,
            "T3CODE_OTLP_TRACES_URL" => EnvKey::T3OtlpUrl(Signal::Traces),
            "T3CODE_OTLP_METRICS_URL" => EnvKey::T3OtlpUrl(Signal::Metrics),
            "T3CODE_OTLP_LOGS_URL" => EnvKey::T3OtlpUrl(Signal::Logs),
            "OTEL_EXPORTER_OTLP_ENDPOINT" => EnvKey::OtlpEndpoint(None),
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT" => EnvKey::OtlpEndpoint(Some(Signal::Traces)),
            "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT" => EnvKey::OtlpEndpoint(Some(Signal::Metrics)),
            "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT" => EnvKey::OtlpEndpoint(Some(Signal::Logs)),
            "OTEL_EXPORTER_OTLP_PROTOCOL" => EnvKey::OtlpProtocol(None),
            "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL" => EnvKey::OtlpProtocol(Some(Signal::Traces)),
            "OTEL_EXPORTER_OTLP_METRICS_PROTOCOL" => EnvKey::OtlpProtocol(Some(Signal::Metrics)),
            "OTEL_EXPORTER_OTLP_LOGS_PROTOCOL" => EnvKey::OtlpProtocol(Some(Signal::Logs)),
            "OTEL_TRACES_EXPORTER" => EnvKey::OtelExporter(Signal::Traces),
            "OTEL_METRICS_EXPORTER" => EnvKey::OtelExporter(Signal::Metrics),
            "OTEL_LOGS_EXPORTER" => EnvKey::OtelExporter(Signal::Logs),
            _ => return None,
        })
    }

    pub const ALLOWLIST: [EnvKey; 18] = [
        EnvKey::TelemetryEnabled,
        EnvKey::T3OtelSdkDisabled,
        EnvKey::OtelSdkDisabled,
        EnvKey::PosthogHost,
        EnvKey::T3OtlpUrl(Signal::Traces),
        EnvKey::T3OtlpUrl(Signal::Metrics),
        EnvKey::T3OtlpUrl(Signal::Logs),
        EnvKey::OtlpEndpoint(None),
        EnvKey::OtlpEndpoint(Some(Signal::Traces)),
        EnvKey::OtlpEndpoint(Some(Signal::Metrics)),
        EnvKey::OtlpEndpoint(Some(Signal::Logs)),
        EnvKey::OtlpProtocol(None),
        EnvKey::OtlpProtocol(Some(Signal::Traces)),
        EnvKey::OtlpProtocol(Some(Signal::Metrics)),
        EnvKey::OtlpProtocol(Some(Signal::Logs)),
        EnvKey::OtelExporter(Signal::Traces),
        EnvKey::OtelExporter(Signal::Metrics),
        EnvKey::OtelExporter(Signal::Logs),
    ];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvValue {
    /// `T3CODE_TELEMETRY_ENABLED` is parsed differently by AnalyticsService (strict Effect
    /// `Config.Boolean`, exact lowercase only, no trim) vs relayTracing (`otelEnvironment.ts`,
    /// trimmed & lowercased).
    TelemetryBool {
        raw: String,
        product_parsed: Bool3,
        relay_parsed: Bool3,
    },
    Bool { raw: String, parsed: Bool3 },
    Endpoint(Endpoint),
    ProtocolSupported,
    ExporterNone,
    ExporterOtlp,
    Invalid { raw: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvSet {
    pub entries: Vec<(EnvKey, EnvValue)>,
}

impl EnvSet {
    pub fn get(&self, key: EnvKey) -> Option<&EnvValue> {
        self.entries
            .iter()
            .rev()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    pub fn set_raw(&mut self, key: EnvKey, raw: &str) {
        if key == EnvKey::TelemetryEnabled {
            if raw.is_empty() {
                return;
            }
            let val = EnvValue::TelemetryBool {
                raw: sanitize_short(raw, 16),
                product_parsed: parse_effect_bool_strict(raw),
                relay_parsed: parse_t3_bool(Some(raw)),
            };
            self.entries.push((key, val));
            return;
        }
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        let val = match key {
            EnvKey::TelemetryEnabled => unreachable!(),
            EnvKey::T3OtelSdkDisabled => EnvValue::Bool {
                raw: sanitize_short(trimmed, 16),
                parsed: parse_t3_bool(Some(trimmed)),
            },
            EnvKey::OtelSdkDisabled => EnvValue::Bool {
                raw: sanitize_short(trimmed, 16),
                parsed: parse_otel_bool(Some(trimmed)),
            },
            EnvKey::PosthogHost | EnvKey::T3OtlpUrl(_) | EnvKey::OtlpEndpoint(_) => {
                match parse_http_endpoint(trimmed) {
                    Some(ep) => EnvValue::Endpoint(ep),
                    None => EnvValue::Invalid {
                        // Never retain query strings or userinfo in error display.
                        raw: sanitize_short("not an http(s) URL", 24),
                    },
                }
            }
            EnvKey::OtlpProtocol(_) => {
                let lower = trimmed.to_ascii_lowercase();
                if matches!(lower.as_str(), "http/protobuf" | "http/json") {
                    EnvValue::ProtocolSupported
                } else {
                    EnvValue::Invalid {
                        raw: sanitize_short(trimmed, 16),
                    }
                }
            }
            EnvKey::OtelExporter(_) => {
                let mut has_otlp = false;
                let mut has_none = false;
                for part in trimmed.split(',').map(|s| s.trim().to_ascii_lowercase()) {
                    if part == "otlp" {
                        has_otlp = true;
                    } else if part == "none" {
                        has_none = true;
                    }
                }
                if has_otlp {
                    EnvValue::ExporterOtlp
                } else if has_none {
                    EnvValue::ExporterNone
                } else {
                    EnvValue::Invalid {
                        raw: sanitize_short(trimmed, 16),
                    }
                }
            }
        };
        self.entries.push((key, val));
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Policy {
    pub organization: String,
    pub telemetry: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedPolicy {
    pub source: String,
    pub organization: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Runtime {
    pub version: u32,
    pub pid: u32,
    pub started_at: f64,
    pub started_at_raw: String,
    pub service_managed: Option<bool>,
    pub server_version: Option<String>,
    /// `None` = field absent (older server did not record policy);
    /// `Some(None)` = field explicitly `null` (server started with no policy);
    /// `Some(Some(p))` = server loaded managed policy `p`.
    pub managed_policy: Option<Option<LoadedPolicy>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    Running,
    Stopped,
    PidReused,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    Service,
    Desktop(String),
    Shell { comm: String, same_as_loupe: bool },
    Unknown,
}

impl Launch {
    pub fn kind_str(&self) -> &'static str {
        match self {
            Launch::Service => "service",
            Launch::Desktop(_) => "desktop",
            Launch::Shell { .. } => "shell",
            Launch::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub liveness: Liveness,
    pub launch: Launch,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settings {
    pub agent_feedback: Option<String>,
    pub exporters: Vec<(Signal, Endpoint)>,
    pub provider_update_checks: Option<bool>,
    pub share_usage_data: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostsMatch {
    pub ip: String,
    pub host: String,
    pub sinkhole: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostsScan {
    pub target_host: String,
    /// Entries matching the effective PostHog host (e.g. `ingest.t3.codes`).
    pub matches: Vec<HostsMatch>,
    /// True if `/etc/hosts` lists `t3.codes` without the target subdomain.
    pub bare_parent_only: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadOutcome {
    Read,
    StatOnly,
    Absent,
    Unreadable,
    Invalid,
    Never,
}

impl ReadOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            ReadOutcome::Read => "read",
            ReadOutcome::StatOnly => "stat_only",
            ReadOutcome::Absent => "absent",
            ReadOutcome::Unreadable => "unreadable",
            ReadOutcome::Invalid => "invalid",
            ReadOutcome::Never => "never",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReadRecord {
    pub name: String,
    pub path: String,
    pub outcome: ReadOutcome,
    pub modified_ms: Option<f64>,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DesktopEnv {
    pub env: EnvSet,
    /// Short display path of the file providing the active telemetry setting(s), e.g.
    /// `~/.config/hypr/hyprland.lua`, `~/.config/uwsm/env`, `~/.config/environment.d/t3code.conf`,
    /// or `/etc/environment`.
    pub source_label: String,
    /// Latest modification stamp among the contributing desktop env file(s), used for restart checks.
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellRcSource {
    /// Short display path of the shell file declaring the opt-out, e.g. `~/.bashrc` or `~/.profile`.
    pub path: String,
    /// True for interactive-only shell rc files (`~/.bashrc`, `~/.zshrc`) that Linux desktop
    /// launchers normally skip.
    pub interactive_only: bool,
}

/// Everything a telemetry check inspected. Pure data — no handles, no I/O.
#[derive(Clone, Debug, PartialEq)]
pub struct Inputs {
    pub source: Source,
    pub platform: Platform,
    pub checked_at: f64,
    pub data_folder_exists: bool,
    pub uwsm_detected: bool,
    pub omarchy_hypr_detected: bool,
    pub in_container: bool,
    pub shell: EnvSet,
    pub desktop_env: Option<DesktopEnv>,
    pub shell_rc_source: Option<ShellRcSource>,
    pub service_env: Read<EnvSet>,
    pub policy: Read<Policy>,
    /// macOS only: `/Library/Managed Preferences/com.t3tools.t3code.plist` exists (stat-only, never opened).
    pub macos_managed_prefs_present: bool,
    pub runtime: Read<Runtime>,
    pub server: Server,
    pub settings: Read<Settings>,
    pub hosts: Read<HostsScan>,
    pub nsswitch: Read<bool>,
    pub install_id: Option<Stamp>,
    pub data_folder: PathBuf,
    pub home_display: Option<String>,
    pub reads: Vec<ReadRecord>,
}

// ── Output model ─────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Off,
    Blocked,
    Partial,
    On,
    Misconfigured,
    RestartNeeded,
    Unverified,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Off => "off",
            Verdict::Blocked => "blocked",
            Verdict::Partial => "partial",
            Verdict::On => "on",
            Verdict::Misconfigured => "misconfigured",
            Verdict::RestartNeeded => "restart_needed",
            Verdict::Unverified => "unverified",
        }
    }

    pub fn heading(self) -> &'static str {
        match self {
            Verdict::Off => "TELEMETRY OFF",
            Verdict::Blocked => "TELEMETRY BLOCKED",
            Verdict::Partial => "PARTLY ON",
            Verdict::On => "TELEMETRY ON",
            Verdict::Misconfigured => "OPT-OUT NOT WORKING",
            Verdict::RestartNeeded => "RESTART T3 TO APPLY",
            Verdict::Unverified => "UNVERIFIED",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Verdict::Off => "Telemetry off",
            Verdict::Blocked => "Telemetry blocked",
            Verdict::Partial => "Partly on",
            Verdict::On => "Telemetry on",
            Verdict::Misconfigured => "Opt-out not working",
            Verdict::RestartNeeded => "Restart T3 to apply",
            Verdict::Unverified => "Unverified",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamId {
    ProductEvents,
    RelayTraces,
    CustomExporters,
}

impl StreamId {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamId::ProductEvents => "product_events",
            StreamId::RelayTraces => "relay_traces",
            StreamId::CustomExporters => "custom_exporters",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StreamId::ProductEvents => "product usage events",
            StreamId::RelayTraces => "relay traces",
            StreamId::CustomExporters => "exporters you set",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            StreamId::ProductEvents => "product events",
            StreamId::RelayTraces => "relay traces",
            StreamId::CustomExporters => "your exporters",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamState {
    Off,
    Blocked,
    On,
    OptOutIgnored,
    RestartNeeded,
    Unverified,
    None,
}

impl StreamState {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamState::Off => "off",
            StreamState::Blocked => "blocked",
            StreamState::On | StreamState::OptOutIgnored => "on",
            StreamState::RestartNeeded => "restart_needed",
            StreamState::Unverified => "unverified",
            StreamState::None => "none",
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            StreamState::Off => "off",
            StreamState::Blocked => "blocked",
            StreamState::On | StreamState::OptOutIgnored => "on",
            StreamState::RestartNeeded => "restart",
            StreamState::Unverified => "unknown",
            StreamState::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    Verified,
    Likely,
    Unverified,
    Read,
}

impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Confidence::Verified => "verified",
            Confidence::Likely => "likely",
            Confidence::Unverified => "unverified",
            Confidence::Read => "read",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Confidence::Verified => "verified",
            Confidence::Likely => "likely",
            Confidence::Unverified => "unverified",
            Confidence::Read => "read",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LadderStep {
    pub step: String,
    pub value: String,
    pub parsed: Option<String>,
    pub decisive: bool,
    pub state_hint: Option<StreamState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExporterEntry {
    pub signal: Signal,
    pub endpoint: Endpoint,
    pub from: String,
    pub silenced_by_sdk_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub id: StreamId,
    pub state: StreamState,
    pub confidence: Confidence,
    pub counts_toward_verdict: bool,
    pub destination: String,
    pub destination_host: Option<String>,
    pub destination_overridden: bool,
    pub decided_step: String,
    pub decided_detail: String,
    pub ladder: Vec<LadderStep>,
    pub caveat: Option<String>,
    pub exporters: Vec<ExporterEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtherItem {
    pub id: &'static str,
    pub label: &'static str,
    pub value: String,
    pub note: Option<String>,
    pub is_default: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    OptOutIgnored,
    Restart,
    Warn,
    Caveat,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::OptOutIgnored => "opt_out_ignored",
            Severity::Restart => "restart",
            Severity::Warn => "warn",
            Severity::Caveat => "caveat",
            Severity::Info => "info",
        }
    }

    pub fn is_to_fix(self) -> bool {
        matches!(self, Severity::OptOutIgnored | Severity::Restart | Severity::Warn)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub id: &'static str,
    pub severity: Severity,
    pub stream: Option<StreamId>,
    pub variable: Option<&'static str>,
    pub value: Option<String>,
    pub where_set: Option<&'static str>,
    pub pr: Option<u32>,
    pub message: String,
    pub fix: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotChecked {
    pub id: &'static str,
    pub label: &'static str,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub for_kind: &'static str,
    pub heading: String,
    pub command: String,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthFilesHashed {
    Yes,
    No,
    Unknown,
}

impl AuthFilesHashed {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthFilesHashed::Yes => "yes",
            AuthFilesHashed::No => "no",
            AuthFilesHashed::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerView {
    pub state: Liveness,
    pub launch: Launch,
    pub service_managed: Option<bool>,
    pub version: Option<String>,
    pub started_at: Option<String>,
    pub policy_loaded: Option<LoadedPolicy>,
    pub auth_files_hashed: AuthFilesHashed,
}

/// Live warning-bar alert. Fires ONLY on `Misconfigured` or `RestartNeeded`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert {
    pub kind: AlertKind,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    Misconfigured,
    RestartNeeded,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub verdict: Verdict,
    pub summary: String,
    pub streams: Vec<Stream>,
    pub other: Vec<OtherItem>,
    pub findings: Vec<Finding>,
    pub not_checked: Vec<NotChecked>,
    pub server: ServerView,
    pub fix: Vec<Snippet>,
    pub inputs: Inputs,
}

impl Report {
    /// Contextual heading for CLI banners. Gives specific, graceful readouts for
    /// `Verdict::Unverified` instead of a blunt generic "can't tell".
    pub fn verdict_heading(&self) -> &'static str {
        if self.verdict != Verdict::Unverified {
            return self.verdict.heading();
        }
        if matches!(self.inputs.platform, Platform::Other) {
            return "NOT SUPPORTED ON THIS OS";
        }
        if self.inputs.macos_managed_prefs_present && !matches!(&self.inputs.policy, Read::Absent)
        {
            return "POLICY CONFLICT";
        }
        if matches!(&self.inputs.policy, Read::Invalid { .. }) {
            return "INVALID POLICY";
        }
        if matches!(&self.inputs.policy, Read::Unreadable(_)) {
            return "POLICY UNREADABLE";
        }
        if self.inputs.macos_managed_prefs_present
            || self
                .findings
                .iter()
                .any(|f| f.id == "macos_managed_profile")
        {
            return "MACOS PROFILE";
        }
        if self
            .findings
            .iter()
            .any(|f| f.id == "invalid_telemetry_enabled")
        {
            return "INVALID SETTING";
        }
        if self
            .findings
            .iter()
            .any(|f| f.id == "shell_env_unverified" || f.id == "shell_otel_unverified")
        {
            return "OFF IN THIS SHELL";
        }
        if !self.inputs.data_folder_exists && matches!(&self.inputs.policy, Read::Absent) {
            return "NO T3 DATA HERE";
        }
        "UNVERIFIED"
    }

    /// Short contextual label for the interactive board verdict strip.
    pub fn verdict_short(&self) -> &'static str {
        if self.verdict != Verdict::Unverified {
            return self.verdict.short_label();
        }
        if matches!(self.inputs.platform, Platform::Other) {
            return "Unsupported OS";
        }
        if self.inputs.macos_managed_prefs_present && !matches!(&self.inputs.policy, Read::Absent)
        {
            return "Policy conflict";
        }
        if matches!(&self.inputs.policy, Read::Invalid { .. }) {
            return "Invalid policy";
        }
        if matches!(&self.inputs.policy, Read::Unreadable(_)) {
            return "Policy unreadable";
        }
        if self.inputs.macos_managed_prefs_present
            || self
                .findings
                .iter()
                .any(|f| f.id == "macos_managed_profile")
        {
            return "macOS profile";
        }
        if self
            .findings
            .iter()
            .any(|f| f.id == "invalid_telemetry_enabled")
        {
            return "Invalid setting";
        }
        if self
            .findings
            .iter()
            .any(|f| f.id == "shell_env_unverified" || f.id == "shell_otel_unverified")
        {
            return "Off in shell";
        }
        if !self.inputs.data_folder_exists && matches!(&self.inputs.policy, Read::Absent) {
            return "No T3 data here";
        }
        "Unverified"
    }

    /// Returns a Live dashboard warning alert only when a user-set opt-out is ignored
    /// or waiting on a T3 restart. Plain default-on never fires an alert.
    pub fn alert(&self) -> Option<Alert> {
        match self.verdict {
            Verdict::Misconfigured => {
                let detail = self
                    .findings
                    .iter()
                    .find(|f| f.severity == Severity::OptOutIgnored)
                    .map(|f| match f.id {
                        "otel_sdk_disabled_not_true" => {
                            let v = f.value.as_deref().unwrap_or("");
                            format!("OTEL_SDK_DISABLED={v} reads as false")
                        }
                        "t3_otel_overrides_spec" => {
                            "T3CODE_OTEL_SDK_DISABLED overrides OTEL_SDK_DISABLED=true".into()
                        }
                        _ => f.message.clone(),
                    })
                    .unwrap_or_else(|| self.summary.clone());
                Some(Alert {
                    kind: AlertKind::Misconfigured,
                    detail,
                })
            }
            Verdict::RestartNeeded => Some(Alert {
                kind: AlertKind::RestartNeeded,
                detail: self.summary.clone(),
            }),
            _ => None,
        }
    }

    pub fn json(&self) -> Value {
        let streams: Vec<Value> = self
            .streams
            .iter()
            .map(|s| {
                let mut obj = json!({
                    "id": s.id.as_str(),
                    "state": s.state.as_str(),
                    "confidence": s.confidence.as_str(),
                    "counts_toward_verdict": s.counts_toward_verdict,
                });
                if let Some(map) = obj.as_object_mut() {
                    if let Some(host) = &s.destination_host {
                        map.insert(
                            "destination".into(),
                            json!({
                                "host": host,
                                "overridden": s.destination_overridden,
                            }),
                        );
                    }
                    if s.id == StreamId::CustomExporters {
                        let eps: Vec<Value> = s
                            .exporters
                            .iter()
                            .map(|e| {
                                json!({
                                    "signal": e.signal.label(),
                                    "endpoint": e.endpoint.display_url(),
                                    "loopback": e.endpoint.is_loopback(),
                                    "from": e.from,
                                    "silenced": e.silenced_by_sdk_disabled,
                                })
                            })
                            .collect();
                        map.insert("endpoints".into(), Value::Array(eps));
                    } else {
                        map.insert(
                            "decided_by".into(),
                            json!({
                                "step": s.decided_step,
                                "detail": s.decided_detail,
                            }),
                        );
                        let ladder: Vec<Value> = s
                            .ladder
                            .iter()
                            .map(|step| {
                                let mut row = json!({
                                    "step": step.step,
                                    "value": step.value,
                                    "decisive": step.decisive,
                                });
                                if let (Some(parsed), Some(m)) = (&step.parsed, row.as_object_mut())
                                {
                                    m.insert("parsed".into(), Value::String(parsed.clone()));
                                }
                                row
                            })
                            .collect();
                        map.insert("ladder".into(), Value::Array(ladder));
                    }
                }
                obj
            })
            .collect();

        let other: Vec<Value> = self
            .other
            .iter()
            .map(|o| {
                let mut obj = json!({
                    "id": o.id,
                    "value": o.value,
                });
                if let Some(m) = obj.as_object_mut() {
                    if o.is_default {
                        m.insert("default".into(), Value::Bool(true));
                    }
                    if let Some(note) = &o.note {
                        m.insert("note".into(), Value::String(note.clone()));
                    }
                }
                obj
            })
            .collect();

        let findings: Vec<Value> = self
            .findings
            .iter()
            .map(|f| {
                let mut obj = json!({
                    "id": f.id,
                    "severity": f.severity.as_str(),
                    "message": f.message,
                });
                if let Some(m) = obj.as_object_mut() {
                    if let Some(st) = f.stream {
                        m.insert("stream".into(), Value::String(st.as_str().into()));
                    }
                    if let Some(var) = f.variable {
                        m.insert("variable".into(), Value::String(var.into()));
                    }
                    if let Some(val) = &f.value {
                        m.insert("value".into(), Value::String(val.clone()));
                    }
                    if let Some(w) = f.where_set {
                        m.insert("where".into(), Value::String(w.into()));
                    }
                    if let Some(pr) = f.pr {
                        m.insert("pr".into(), Value::from(pr));
                    }
                    if let Some(fix) = &f.fix {
                        m.insert("fix".into(), Value::String(fix.clone()));
                    }
                }
                obj
            })
            .collect();

        let not_checked: Vec<Value> = self
            .not_checked
            .iter()
            .map(|n| json!({ "id": n.id, "label": n.label, "detail": n.detail }))
            .collect();

        let fix: Vec<Value> = self
            .fix
            .iter()
            .map(|s| json!({ "for": s.for_kind, "text": s.command, "note": s.note }))
            .collect();

        let inputs_log: Vec<Value> = self
            .inputs
            .reads
            .iter()
            .map(|r| {
                let mut obj = json!({
                    "name": r.name,
                    "path": r.path,
                    "state": r.outcome.as_str(),
                });
                if let Some(m) = obj.as_object_mut() {
                    if let Some(ms) = r.modified_ms {
                        m.insert("modified_at".into(), Value::String(format_iso_utc(ms)));
                    }
                    if !r.detail.is_empty() {
                        m.insert("used".into(), Value::String(r.detail.clone()));
                    }
                }
                obj
            })
            .collect();

        let policy_loaded = self.server.policy_loaded.as_ref().map(|p| {
            json!({
                "source": p.source,
                "organization": p.organization,
                "fingerprint": p.fingerprint,
            })
        });

        json!({
            "schema": 1,
            "kind": "telemetry_check",
            "checked_at": format_iso_utc(self.inputs.checked_at),
            "source": self.inputs.source,
            "platform": self.inputs.platform,
            "verdict": self.verdict.as_str(),
            "summary": self.summary,
            "network": "none",
            "writes": "none",
            "server": {
                "state": self.server.state,
                "launch": self.server.launch.kind_str(),
                "service_managed": self.server.service_managed,
                "version": self.server.version,
                "started_at": self.server.started_at,
                "policy_loaded": policy_loaded,
                "auth_files_hashed": self.server.auth_files_hashed.as_str(),
            },
            "streams": streams,
            "other": other,
            "findings": findings,
            "not_checked": not_checked,
            "fix": fix,
            "inputs": inputs_log,
        })
    }
}

// ── Upstream-matched parsers ─────────────────────────────────────────────────

/// Matches Effect 4.1.2 `Config.Boolean` (`apps/server/src/telemetry/AnalyticsService.ts:76`,
/// `Schema.ts:5730`): exact lowercase literals only, no trimming or case-folding. Any other
/// non-empty value fails config validation at server startup.
pub fn parse_effect_bool_strict(raw: &str) -> Bool3 {
    if raw.is_empty() {
        return Bool3::Unset;
    }
    match raw {
        "true" | "yes" | "on" | "1" | "y" => Bool3::True,
        "false" | "no" | "off" | "0" | "n" => Bool3::False,
        _ => Bool3::Invalid,
    }
}

/// Matches T3 Code's trimmed case-insensitive boolean parser
/// (`packages/shared/src/otelEnvironment.ts`: `T3CODE_TRUE`/`T3CODE_FALSE`, `TrimmedLowercase`).
pub fn parse_t3_bool(raw: Option<&str>) -> Bool3 {
    match raw.map(str::trim) {
        None | Some("") => Bool3::Unset,
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" | "y" => Bool3::True,
            "false" | "no" | "off" | "0" | "n" => Bool3::False,
            _ => Bool3::Invalid,
        },
    }
}

/// Matches T3 Code's `OTEL_SDK_DISABLED` flag (`packages/shared/src/otelEnvironment.ts:261-269`):
/// trimmed & case-insensitive, ONLY `"true"` disables; `"false"` is false; any other non-empty
/// string is read as `false` with a warning (`Bool3::Invalid`).
pub fn parse_otel_bool(raw: Option<&str>) -> Bool3 {
    match raw.map(str::trim) {
        None | Some("") => Bool3::Unset,
        Some(v) => match v.to_ascii_lowercase().as_str() {
            "true" => Bool3::True,
            "false" => Bool3::False,
            _ => Bool3::Invalid,
        },
    }
}

/// Parses an `http://` or `https://` URL and discards userinfo, path, query, and fragment so
/// credentials embedded in URLs can never enter `Inputs` or `Report`.
pub fn parse_http_endpoint(raw: &str) -> Option<Endpoint> {
    let s = raw.trim();
    let (scheme, rest) = if let Some(r) = s.strip_prefix("https://") {
        ("https", r)
    } else if let Some(r) = s.strip_prefix("http://") {
        ("http", r)
    } else {
        // Case-insensitive scheme check
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("https://") {
            ("https", &s[8..])
        } else if lower.starts_with("http://") {
            ("http", &s[7..])
        } else {
            return None;
        }
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    // Strip userinfo (`user:pass@`) — never retain it.
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, hp)| hp);
    if host_port.is_empty() {
        return None;
    }
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let (addr, tail) = bracketed.split_once(']')?;
        if addr.is_empty() {
            return None;
        }
        let port = if let Some(p) = tail.strip_prefix(':') {
            Some(p.parse::<u16>().ok()?)
        } else if tail.is_empty() {
            None
        } else {
            return None;
        };
        (format!("[{}]", addr.to_ascii_lowercase()), port)
    } else if let Some((h, p)) = host_port.rsplit_once(':') {
        if h.is_empty() || h.contains(':') {
            return None;
        }
        (h.to_ascii_lowercase(), Some(p.parse::<u16>().ok()?))
    } else {
        (host_port.to_ascii_lowercase(), None)
    };
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(Endpoint {
        scheme: scheme.to_owned(),
        host,
        port,
    })
}

pub fn is_sinkhole_ip(ip: &str) -> bool {
    let ip = ip.trim();
    if ip == "0.0.0.0" || ip == "::" || ip == "::0" || ip == "::1" || ip == "0:0:0:0:0:0:0:0" || ip == "0:0:0:0:0:0:0:1" {
        return true;
    }
    if let Some(rest) = ip.strip_prefix("127.") {
        // Any valid 127.0.0.0/8 loopback address
        let parts: Vec<&str> = rest.split('.').collect();
        return parts.len() == 3 && parts.iter().all(|p| p.parse::<u8>().is_ok());
    }
    false
}

pub fn is_sinkhole_or_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']').trim_end_matches('.');
    h.eq_ignore_ascii_case("localhost") || is_sinkhole_ip(h)
}

pub fn sanitize_short(raw: &str, max_chars: usize) -> String {
    let cleaned = model::clean(raw);
    cleaned.chars().take(max_chars).collect()
}

/// Checks whether a T3 Code `serverVersion` string falls inside the auth-file hashing window:
/// introduced in PR #21506 (20 Oct 2026, 0.1.x series) and removed in PR #42299
/// (`0.1.12-nightly.20261124.5209`).
///
/// Format: `MAJOR.MINOR.PATCH[-nightly.YYYYMMDD.N]` (leading `v` tolerated).
/// - `< 0.1.0` -> `Unknown` (predates PR #21506 introduction)
/// - `0.1.0 .. 0.1.11` -> `Yes` (unless `-nightly.YYYYMMDD.*` predates `20261020`, which is `No`)
/// - `0.1.12-nightly.YYYYMMDD.N` where `(YYYYMMDD, N) < (20261124, 5209)` -> `Yes`
/// - `0.1.12-nightly.YYYYMMDD.N` where `(YYYYMMDD, N) >= (20261124, 5209)` -> `No`
/// - `>= 0.1.13` -> `No`
/// - `0.1.12` stable (no `-nightly`) -> `Unknown` until confirmed.
pub fn auth_files_hashed_for_version(version: &str) -> AuthFilesHashed {
    let v = version.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut parts = core.split('.');
    let (Some(maj), Some(min), Some(pat)) = (
        parts.next().and_then(|s| s.parse::<u32>().ok()),
        parts.next().and_then(|s| s.parse::<u32>().ok()),
        parts.next().and_then(|s| s.parse::<u32>().ok()),
    ) else {
        return AuthFilesHashed::Unknown;
    };
    if parts.next().is_some() {
        return AuthFilesHashed::Unknown;
    }
    if (maj, min, pat) < (0, 1, 0) {
        return AuthFilesHashed::Unknown;
    }
    if (maj, min, pat) < (0, 1, 12) {
        if let Some(rest) = pre.and_then(|p| p.strip_prefix("nightly.")) {
            if let Some(date) = rest.split('.').next().and_then(|s| s.parse::<u32>().ok()) {
                if date < 20261020 {
                    return AuthFilesHashed::No;
                }
            }
        }
        return AuthFilesHashed::Yes;
    }
    if (maj, min, pat) > (0, 1, 12) {
        return AuthFilesHashed::No;
    }
    // Exactly 0.1.12
    let Some(pre) = pre else {
        // Stable 0.1.12 not yet confirmed in FIXED_IN table.
        return AuthFilesHashed::Unknown;
    };
    let Some(rest) = pre.strip_prefix("nightly.") else {
        return AuthFilesHashed::Unknown;
    };
    let mut np = rest.split('.');
    let (Some(date), Some(build)) = (
        np.next().and_then(|s| s.parse::<u32>().ok()),
        np.next().and_then(|s| s.parse::<u32>().ok()),
    ) else {
        return AuthFilesHashed::Unknown;
    };
    if (date, build) < (20261124, 5209) {
        AuthFilesHashed::Yes
    } else {
        AuthFilesHashed::No
    }
}

/// Parse ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SS[.fff]Z`) or numeric epoch ms.
pub fn parse_timestamp_ms(raw: &str) -> Option<f64> {
    let s = raw.trim();
    if let Ok(num) = s.parse::<f64>() {
        if num.is_finite() && num > 0.0 {
            return Some(num);
        }
    }
    // Delegate ISO parsing to model::ms (handles standard ISO-8601 dates).
    model::ms(Some(s))
}

pub fn format_iso_utc(ms: f64) -> String {
    let secs = (ms / 1000.0).floor() as i64;
    let rem_ms = (ms - (secs as f64) * 1000.0).round().clamp(0.0, 999.0) as i64;
    let t = utc_civil(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.0, t.1, t.2, t.3, t.4, t.5, rem_ms
    )
}

pub fn format_short_date(ms: f64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = (ms / 1000.0).floor() as i64;
    let t = crate::platform::local_time(secs);
    match t {
        Some(lt) if (0..12).contains(&lt.month0) => format!("{} {}", MONTHS[lt.month0 as usize], lt.day),
        _ => {
            let (_, m, d, _, _, _) = utc_civil(secs);
            format!("{} {}", MONTHS[(m - 1).clamp(0, 11) as usize], d)
        }
    }
}

pub fn format_header_time(ms: f64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let secs = (ms / 1000.0).floor() as i64;
    match crate::platform::local_time(secs) {
        Some(lt) if (0..12).contains(&lt.month0) => format!(
            "{} {} {:02}:{:02}",
            MONTHS[lt.month0 as usize], lt.day, lt.hour, lt.minute
        ),
        _ => {
            let (_, m, d, h, min, _) = utc_civil(secs);
            format!(
                "{} {} {:02}:{:02}",
                MONTHS[(m - 1).clamp(0, 11) as usize],
                d,
                h,
                min
            )
        }
    }
}

fn utc_civil(seconds: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rest / 3600, (rest % 3600) / 60, rest % 60)
}

// ── Evaluator ────────────────────────────────────────────────────────────────

enum EffectiveEnv<'a> {
    Service(&'a EnvSet, Option<&'a Stamp>),
    DesktopConfig(&'a EnvSet, &'a str, &'a Stamp),
    SameShell(&'a EnvSet),
    OtherLaunch(&'a EnvSet),
    StoppedShell(&'a EnvSet),
}

impl<'a> EffectiveEnv<'a> {
    fn set(&self) -> &'a EnvSet {
        match self {
            EffectiveEnv::Service(s, _)
            | EffectiveEnv::DesktopConfig(s, _, _)
            | EffectiveEnv::SameShell(s)
            | EffectiveEnv::OtherLaunch(s)
            | EffectiveEnv::StoppedShell(s) => s,
        }
    }

    fn confidence(&self) -> Confidence {
        match self {
            EffectiveEnv::Service(_, _)
            | EffectiveEnv::DesktopConfig(_, _, _)
            | EffectiveEnv::SameShell(_) => Confidence::Likely,
            EffectiveEnv::OtherLaunch(_) | EffectiveEnv::StoppedShell(_) => Confidence::Unverified,
        }
    }

    fn where_label(&self) -> &'static str {
        match self {
            EffectiveEnv::Service(_, _) => "service.env",
            EffectiveEnv::DesktopConfig(_, _, _) => "desktop env",
            _ => "shell",
        }
    }

    fn detail_source(&self) -> &str {
        match self {
            EffectiveEnv::Service(_, _) => "service.env",
            EffectiveEnv::DesktopConfig(_, src, _) => src,
            _ => "shell",
        }
    }

    fn is_unverified_for_running(&self) -> bool {
        matches!(self, EffectiveEnv::OtherLaunch(_))
    }
}

pub fn evaluate(inputs: Inputs) -> Report {
    let mut findings: Vec<Finding> = Vec::new();
    let mut not_checked: Vec<NotChecked> = Vec::new();

    // 1. Runtime & server state
    let runtime_ok = match &inputs.runtime {
        Read::Ok { value, .. } if value.version == 1 => Some(value),
        Read::Ok { value, .. } => {
            findings.push(Finding {
                id: "unsupported_runtime_version",
                severity: Severity::Caveat,
                stream: None,
                variable: None,
                value: Some(value.version.to_string()),
                where_set: None,
                pr: None,
                message: format!(
                    "Unsupported server-runtime.json (version {}): update Loupe.",
                    value.version
                ),
                fix: None,
            });
            None
        }
        Read::Invalid { reason, .. } => {
            findings.push(Finding {
                id: "invalid_runtime_file",
                severity: Severity::Caveat,
                stream: None,
                variable: None,
                value: None,
                where_set: None,
                pr: None,
                message: format!("Could not parse server-runtime.json ({reason})."),
                fix: None,
            });
            None
        }
        _ => None,
    };

    let server_alive = inputs.server.liveness == Liveness::Running;
    let service_managed = runtime_ok.and_then(|r| r.service_managed);
    let server_version = runtime_ok.and_then(|r| r.server_version.clone());
    let auth_files_hashed = server_version
        .as_deref()
        .map(auth_files_hashed_for_version)
        .unwrap_or(AuthFilesHashed::Unknown);

    let server_view = ServerView {
        state: inputs.server.liveness,
        launch: inputs.server.launch.clone(),
        service_managed,
        version: server_version.clone(),
        started_at: runtime_ok.map(|r| r.started_at_raw.clone()),
        policy_loaded: runtime_ok.and_then(|r| r.managed_policy.clone().flatten()),
        auth_files_hashed,
    };

    // 2. Choose effective environment per design §5.2
    let empty_env = EnvSet::default();
    let eff_env = if server_alive && service_managed == Some(true) {
        match &inputs.service_env {
            Read::Ok { value, stamp } => EffectiveEnv::Service(value, Some(stamp)),
            _ => EffectiveEnv::Service(&empty_env, None),
        }
    } else if server_alive {
        match &inputs.server.launch {
            Launch::Shell {
                same_as_loupe: true,
                ..
            } => EffectiveEnv::SameShell(&inputs.shell),
            Launch::Desktop(_) => match &inputs.desktop_env {
                Some(de) if !de.env.entries.is_empty() => {
                    EffectiveEnv::DesktopConfig(&de.env, &de.source_label, &de.stamp)
                }
                _ => EffectiveEnv::OtherLaunch(&inputs.shell),
            },
            _ => EffectiveEnv::OtherLaunch(&inputs.shell),
        }
    } else {
        match &inputs.desktop_env {
            Some(de) if !de.env.entries.is_empty() => {
                EffectiveEnv::DesktopConfig(&de.env, &de.source_label, &de.stamp)
            }
            _ => EffectiveEnv::StoppedShell(&inputs.shell),
        }
    };

    // Detect shell vs service.env / desktop mismatch (opt-out set in shell)
    let shell_opts_out_telemetry = matches!(
        inputs.shell.get(EnvKey::TelemetryEnabled),
        Some(EnvValue::TelemetryBool {
            product_parsed: Bool3::False,
            ..
        }) | Some(EnvValue::TelemetryBool {
            relay_parsed: Bool3::False,
            ..
        })
    );
    let shell_otel_opt_out_var: Option<(&'static str, String)> = match (
        inputs.shell.get(EnvKey::T3OtelSdkDisabled),
        inputs.shell.get(EnvKey::OtelSdkDisabled),
    ) {
        (
            Some(EnvValue::Bool {
                raw,
                parsed: Bool3::True,
            }),
            _,
        ) => Some(("T3CODE_OTEL_SDK_DISABLED", raw.clone())),
        (
            _,
            Some(EnvValue::Bool {
                raw,
                parsed: Bool3::True,
            }),
        ) => Some(("OTEL_SDK_DISABLED", raw.clone())),
        _ => None,
    };
    let service_env_opts_out = match &inputs.service_env {
        Read::Ok { value, .. } => matches!(
            value.get(EnvKey::TelemetryEnabled),
            Some(EnvValue::TelemetryBool {
                product_parsed: Bool3::False,
                ..
            }) | Some(EnvValue::TelemetryBool {
                relay_parsed: Bool3::False,
                ..
            })
        ),
        _ => false,
    };
    let service_env_opts_out_otel = match &inputs.service_env {
        Read::Ok { value, .. } => {
            matches!(
                value.get(EnvKey::T3OtelSdkDisabled),
                Some(EnvValue::Bool {
                    parsed: Bool3::True,
                    ..
                })
            ) || matches!(
                value.get(EnvKey::OtelSdkDisabled),
                Some(EnvValue::Bool {
                    parsed: Bool3::True,
                    ..
                })
            )
        }
        _ => false,
    };

    let service_env_display = display_service_env_path(&inputs);

    // 3. Managed policy evaluation, macOS MDM profile check & staleness check
    let is_plist_source = |src: &str| {
        let lower = src.to_ascii_lowercase();
        lower.ends_with(".plist") || lower.contains("managed preferences")
    };
    let macos_dual_policy =
        inputs.macos_managed_prefs_present && !matches!(&inputs.policy, Read::Absent);
    let macos_profile_active = inputs.macos_managed_prefs_present
        || server_view
            .policy_loaded
            .as_ref()
            .is_some_and(|p| is_plist_source(&p.source));

    if macos_dual_policy {
        findings.push(Finding {
            id: "macos_dual_policy_conflict",
            severity: Severity::Warn,
            stream: None,
            variable: None,
            value: None,
            where_set: Some("policy.json"),
            pr: None,
            message: "Both macOS Managed Preferences (com.t3tools.t3code.plist) and policy.json exist; T3 Code refuses to start when both are present.".into(),
            fix: Some("Remove one policy source, then restart T3 Code.".into()),
        });
    } else if macos_profile_active {
        findings.push(Finding {
            id: "macos_managed_profile",
            severity: Severity::Caveat,
            stream: None,
            variable: None,
            value: None,
            where_set: Some("Managed Preferences"),
            pr: None,
            message: "A macOS Managed Preferences profile (com.t3tools.t3code.plist) is active. Loupe does not open managed plists, so it cannot verify limits.telemetry.".into(),
            fix: None,
        });
    }

    let policy_denies_telemetry = !macos_dual_policy
        && !macos_profile_active
        && matches!(
            &inputs.policy,
            Read::Ok { value, .. } if value.telemetry == Some(false)
        );
    let policy_unreadable = !macos_dual_policy
        && !macos_profile_active
        && matches!(&inputs.policy, Read::Unreadable(_));
    let policy_invalid = !macos_dual_policy
        && !macos_profile_active
        && matches!(&inputs.policy, Read::Invalid { .. });

    if !macos_dual_policy && !macos_profile_active {
        if let Read::Invalid { reason, .. } = &inputs.policy {
            let msg = if server_alive {
                format!(
                    "Managed policy is invalid ({reason}). The running T3 server keeps what it loaded, but T3 will not start again until policy.json is fixed."
                )
            } else {
                format!(
                    "Managed policy is invalid ({reason}); T3 Code will not start until policy.json is fixed."
                )
            };
            findings.push(Finding {
                id: "policy_invalid",
                severity: Severity::Warn,
                stream: None,
                variable: None,
                value: None,
                where_set: Some("policy.json"),
                pr: None,
                message: msg,
                fix: Some("Run `t3 policy check` to fix policy.json, then restart T3 Code.".into()),
            });
        } else if let Read::Ok { value, .. } = &inputs.policy {
            if value.telemetry != Some(false) {
                findings.push(Finding {
                    id: "policy_does_not_disable_telemetry",
                    severity: Severity::Info,
                    stream: None,
                    variable: None,
                    value: None,
                    where_set: Some("policy.json"),
                    pr: None,
                    message: "Managed policy does not disable telemetry (only limits.telemetry: false turns it off)."
                        .into(),
                    fix: None,
                });
            }
        }
    }

    // Check whether running server actually loaded on-disk policy (§5.4)
    // Distinguish `Some(None)` (server explicitly recorded null = no policy) from `None`
    // (older server that did not record `managedPolicy` in server-runtime.json).
    let policy_restart_reason: Option<String> = if policy_denies_telemetry && server_alive {
        if let (Read::Ok { value: pol, stamp }, Some(rt)) = (&inputs.policy, runtime_ok) {
            match &rt.managed_policy {
                Some(None) => Some("T3 started without policy.json".into()),
                Some(Some(loaded))
                    if !is_plist_source(&loaded.source) && loaded.organization != pol.organization =>
                {
                    Some("policy organization changed after T3 started".into())
                }
                Some(Some(loaded))
                    if !is_plist_source(&loaded.source)
                        && stamp.modified_ms > rt.started_at + RESTART_GRACE_MS =>
                {
                    Some("policy.json changed after T3 started".into())
                }
                Some(Some(_)) => None,
                None if stamp.modified_ms > rt.started_at + RESTART_GRACE_MS => {
                    Some("policy.json changed after T3 started".into())
                }
                None => None,
            }
        } else {
            None
        }
    } else {
        None
    };

    // Detect file policy removed on disk while still loaded in running server (skip macOS MDM profiles)
    let policy_removed_but_loaded = matches!(&inputs.policy, Read::Absent)
        && !macos_profile_active
        && server_alive
        && server_view
            .policy_loaded
            .as_ref()
            .is_some_and(|p| !is_plist_source(&p.source));
    if policy_removed_but_loaded {
        findings.push(Finding {
            id: "policy_removed_on_disk",
            severity: Severity::Caveat,
            stream: None,
            variable: None,
            value: None,
            where_set: Some("policy.json"),
            pr: None,
            message: "Managed policy was removed on disk; telemetry will follow defaults after T3 restarts."
                .into(),
            fix: None,
        });
    }

    // Check service.env or desktop launcher config modified after server started
    let env_config_stale_source: Option<&str> = match (&eff_env, runtime_ok) {
        (EffectiveEnv::Service(_, Some(stamp)), Some(rt))
            if server_alive && stamp.modified_ms > rt.started_at + RESTART_GRACE_MS =>
        {
            Some("service.env")
        }
        (EffectiveEnv::DesktopConfig(_, src, stamp), Some(rt))
            if server_alive && stamp.modified_ms > rt.started_at + RESTART_GRACE_MS =>
        {
            Some(src)
        }
        _ => None,
    };

    // Check service.env opting out when T3 is NOT service-managed
    if service_env_opts_out && server_alive && service_managed != Some(true) && !policy_denies_telemetry {
        findings.push(Finding {
            id: "service_env_unused",
            severity: Severity::OptOutIgnored,
            stream: Some(StreamId::ProductEvents),
            variable: Some("T3CODE_TELEMETRY_ENABLED"),
            value: Some("false".into()),
            where_set: Some("service.env"),
            pr: None,
            message: "service.env turns telemetry off, but T3 Code is not running as a service."
                .into(),
            fix: None,
        });
    }

    // 4. Effective PostHog host
    let (posthog_host, posthog_overridden, posthog_invalid) =
        match eff_env.set().get(EnvKey::PosthogHost) {
            Some(EnvValue::Endpoint(ep)) => (
                ep.host.clone(),
                !ep.host.eq_ignore_ascii_case(DEFAULT_POSTHOG_HOST),
                false,
            ),
            Some(EnvValue::Invalid { .. }) => (DEFAULT_POSTHOG_HOST.to_owned(), true, true),
            _ => (DEFAULT_POSTHOG_HOST.to_owned(), false, false),
        };
    if posthog_invalid {
        findings.push(Finding {
            id: "invalid_posthog_host",
            severity: Severity::Warn,
            stream: Some(StreamId::ProductEvents),
            variable: Some("T3CODE_POSTHOG_HOST"),
            value: None,
            where_set: Some(eff_env.where_label()),
            pr: None,
            message: "T3CODE_POSTHOG_HOST is not a valid http(s) URL.".into(),
            fix: None,
        });
    }

    // 5. Evaluate Stream 1: Product usage events
    let product_stream = evaluate_product_events(
        &inputs,
        &eff_env,
        policy_denies_telemetry,
        policy_unreadable,
        policy_invalid,
        macos_profile_active,
        macos_dual_policy,
        policy_restart_reason.as_deref(),
        env_config_stale_source,
        server_alive,
        runtime_ok,
        &posthog_host,
        posthog_overridden,
        posthog_invalid,
        &mut findings,
    );

    // 6. Evaluate Stream 2: Relay traces (Axiom / OTEL)
    // Compute `sdk_disabled` independently of policy/T3CODE_TELEMETRY_ENABLED early returns,
    // matching `packages/shared/src/otelEnvironment.ts:287`, so custom OTLP exporters know
    // whether the OTEL SDK itself is disabled.
    let sdk_disabled = is_otel_sdk_disabled(eff_env.set());
    let relay_stream = evaluate_relay_traces(
        &inputs,
        &eff_env,
        policy_denies_telemetry,
        policy_unreadable,
        policy_invalid,
        macos_profile_active,
        macos_dual_policy,
        policy_restart_reason.as_deref(),
        env_config_stale_source,
        server_alive,
        runtime_ok,
        &mut findings,
    );

    // 7. Shell-vs-launch mismatch checks
    if server_alive
        && service_managed == Some(true)
        && !policy_denies_telemetry
        && !macos_profile_active
        && !macos_dual_policy
    {
        if shell_opts_out_telemetry && !service_env_opts_out {
            findings.push(Finding {
                id: "shell_env_ignored_by_service",
                severity: Severity::OptOutIgnored,
                stream: Some(StreamId::ProductEvents),
                variable: Some("T3CODE_TELEMETRY_ENABLED"),
                value: Some("false".into()),
                where_set: Some("shell"),
                pr: None,
                message: "T3CODE_TELEMETRY_ENABLED=false is set in this shell, but T3 runs as a service and reads service.env."
                    .into(),
                fix: Some(format!(
                    "Add T3CODE_TELEMETRY_ENABLED=false to {service_env_display} and restart the service."
                )),
            });
        } else if let Some((var_name, var_val)) = &shell_otel_opt_out_var {
            if !service_env_opts_out && !service_env_opts_out_otel {
                findings.push(Finding {
                    id: "shell_otel_ignored_by_service",
                    severity: Severity::OptOutIgnored,
                    stream: Some(StreamId::RelayTraces),
                    variable: Some(var_name),
                    value: Some(var_val.clone()),
                    where_set: Some("shell"),
                    pr: None,
                    message: format!(
                        "{var_name}={var_val} is set in this shell, but T3 runs as a service and reads service.env."
                    ),
                    fix: Some(format!(
                        "Add {var_name}={var_val} to {service_env_display} and restart the service."
                    )),
                });
            }
        }
    } else if server_alive
        && eff_env.is_unverified_for_running()
        && !policy_denies_telemetry
        && !policy_invalid
        && !macos_profile_active
        && !macos_dual_policy
    {
        let desktop_target = if inputs.omarchy_hypr_detected {
            "~/.config/hypr/hyprland.lua"
        } else if inputs.uwsm_detected {
            "~/.config/uwsm/env"
        } else {
            "~/.config/environment.d/t3code.conf"
        };
        if shell_opts_out_telemetry {
            let (severity, msg, fix) = match (&inputs.server.launch, &inputs.shell_rc_source) {
                (Launch::Desktop(_), Some(rc))
                    if rc.interactive_only && matches!(inputs.platform, Platform::Linux) =>
                {
                    (
                        Severity::Warn,
                        format!(
                            "T3CODE_TELEMETRY_ENABLED=false is only in {} (interactive shells). Desktop T3 launched by your compositor usually skips {}, so telemetry may still be on.",
                            rc.path, rc.path
                        ),
                        Some(format!(
                            "Move it to {desktop_target} (no sudo, tracked by Loupe) or create optional /etc/t3code/policy.json."
                        )),
                    )
                }
                (Launch::Desktop(_), Some(rc)) => (
                    Severity::Caveat,
                    format!(
                        "T3CODE_TELEMETRY_ENABLED=false was found in {}, not in a desktop launcher env file or policy.json. Loupe never reads the running app's private env.",
                        rc.path
                    ),
                    Some(format!(
                        "Set it in {desktop_target} or optional /etc/t3code/policy.json so Loupe can track it."
                    )),
                ),
                (Launch::Desktop(_), None) => (
                    Severity::Caveat,
                    "T3CODE_TELEMETRY_ENABLED=false is set in this shell, not in a desktop launcher env file or policy.json. Loupe never reads the running app's private env.".into(),
                    None,
                ),
                _ => (
                    Severity::Caveat,
                    "T3CODE_TELEMETRY_ENABLED=false is set in this shell, but T3 Code was started from a different shell.".into(),
                    None,
                ),
            };
            findings.push(Finding {
                id: "shell_env_unverified",
                severity,
                stream: Some(StreamId::ProductEvents),
                variable: Some("T3CODE_TELEMETRY_ENABLED"),
                value: Some("false".into()),
                where_set: Some("shell"),
                pr: None,
                message: msg,
                fix,
            });
        } else if let Some((var_name, var_val)) = &shell_otel_opt_out_var {
            let (severity, msg, fix) = match (&inputs.server.launch, &inputs.shell_rc_source) {
                (Launch::Desktop(_), Some(rc))
                    if rc.interactive_only && matches!(inputs.platform, Platform::Linux) =>
                {
                    (
                        Severity::Warn,
                        format!(
                            "{var_name}={var_val} is only in {} (interactive shells). Desktop T3 usually skips {}, so relay traces may still be on.",
                            rc.path, rc.path
                        ),
                        Some(format!(
                            "Move it to {desktop_target} (no sudo, tracked by Loupe) or create optional /etc/t3code/policy.json."
                        )),
                    )
                }
                (Launch::Desktop(_), _) => (
                    Severity::Caveat,
                    format!(
                        "{var_name}={var_val} is set in this shell, not in a desktop launcher env file or policy.json. Loupe never reads the running app's private env."
                    ),
                    None,
                ),
                _ => (
                    Severity::Caveat,
                    format!(
                        "{var_name}={var_val} is set in this shell, but T3 Code was started from a different shell."
                    ),
                    None,
                ),
            };
            findings.push(Finding {
                id: "shell_otel_unverified",
                severity,
                stream: Some(StreamId::RelayTraces),
                variable: Some(var_name),
                value: Some(var_val.clone()),
                where_set: Some("shell"),
                pr: None,
                message: msg,
                fix,
            });
        }
    }

    // 8. Evaluate Stream 3: Exporters you set (informational, does not affect verdict)
    let custom_stream = evaluate_custom_exporters(&inputs, &eff_env, sdk_disabled, &mut findings);

    // 9. PR #42299 auth-file hashing finding (if running version is known pre-fix)
    if auth_files_hashed == AuthFilesHashed::Yes {
        let ver = server_version.as_deref().unwrap_or("this build");
        let severity = if matches!(
            product_stream.state,
            StreamState::On | StreamState::OptOutIgnored | StreamState::Blocked | StreamState::RestartNeeded
        ) {
            Severity::Warn
        } else {
            Severity::Caveat
        };
        findings.push(Finding {
            id: "auth_files_hashed",
            severity,
            stream: Some(StreamId::ProductEvents),
            variable: None,
            value: None,
            where_set: None,
            pr: Some(42299),
            message: format!(
                "T3 {ver} hashes ~/.codex/auth.json and ~/.claude.json into its install ID. Builds from nightly 2026-11-24.5209 (#42299) don't. Loupe never opens them."
            ),
            fix: Some("Update T3 Code to v0.1.12-nightly.20261124.5209 or newer.".into()),
        });
    }

    // 10. Downgrade OptOutIgnored findings whose target stream ended up Off anyway (§5.5)
    for f in &mut findings {
        if f.severity == Severity::OptOutIgnored {
            let target_off = match f.stream {
                Some(StreamId::ProductEvents) => product_stream.state == StreamState::Off,
                Some(StreamId::RelayTraces) => relay_stream.state == StreamState::Off,
                _ => false,
            };
            if target_off {
                f.severity = Severity::Caveat;
            }
        }
    }

    // Also update stream state to OptOutIgnored if an active OptOutIgnored finding targets it
    let mut product_stream = product_stream;
    let mut relay_stream = relay_stream;
    if findings
        .iter()
        .any(|f| f.severity == Severity::OptOutIgnored && f.stream == Some(StreamId::ProductEvents))
        && product_stream.state == StreamState::On
    {
        product_stream.state = StreamState::OptOutIgnored;
    }
    if findings
        .iter()
        .any(|f| f.severity == Severity::OptOutIgnored && f.stream == Some(StreamId::RelayTraces))
        && relay_stream.state == StreamState::On
    {
        relay_stream.state = StreamState::OptOutIgnored;
    }

    // 11. Other settings (informational only; never count toward verdict)
    let mut other = Vec::new();
    match &inputs.settings {
        Read::Ok { value, .. } => {
            let fb = value.agent_feedback.as_deref().unwrap_or("off");
            other.push(OtherItem {
                id: "agent_feedback",
                label: "agent feedback",
                value: fb.to_owned(),
                note: (fb == "off").then(|| "default".into()),
                is_default: value.agent_feedback.is_none(),
            });
            let upd = value.provider_update_checks.unwrap_or(true);
            other.push(OtherItem {
                id: "provider_update_checks",
                label: "update checks",
                value: if upd { "on".into() } else { "off".into() },
                note: value.provider_update_checks.is_none().then(|| "default".into()),
                is_default: value.provider_update_checks.is_none(),
            });
            if let Some(share) = value.share_usage_data {
                other.push(OtherItem {
                    id: "share_usage_data",
                    label: "shareUsageData",
                    value: if share { "true".into() } else { "false".into() },
                    note: Some("reserved; not read by T3 yet".into()),
                    is_default: false,
                });
            }
        }
        Read::Invalid { .. } | Read::Unreadable(_) => {
            other.push(OtherItem {
                id: "settings",
                label: "other settings",
                value: "unknown".into(),
                note: Some("settings.json unreadable or invalid".into()),
                is_default: false,
            });
        }
        Read::Absent => {
            other.push(OtherItem {
                id: "agent_feedback",
                label: "agent feedback",
                value: "off".into(),
                note: Some("default".into()),
                is_default: true,
            });
            other.push(OtherItem {
                id: "provider_update_checks",
                label: "update checks",
                value: "on".into(),
                note: Some("default".into()),
                is_default: true,
            });
        }
    }
    if let Some(stamp) = &inputs.install_id {
        other.push(OtherItem {
            id: "install_id",
            label: "install ID",
            value: format!("since {}", format_short_date(stamp.modified_ms)),
            note: Some("date only, not opened".into()),
            is_default: false,
        });
    }

    // 12. Honest "Not checked here" list
    not_checked.push(NotChecked {
        id: "activity_sharing",
        label: "activity sharing",
        detail: "in T3's secrets (Settings › Sharing)".into(),
    });
    if eff_env.is_unverified_for_running() && !policy_denies_telemetry {
        let reason = match &inputs.server.launch {
            Launch::Desktop(_) => "desktop app env is private (never read)",
            Launch::Shell { .. } => "other shell env is private (never read)",
            _ => "running T3 env is private (never read)",
        };
        not_checked.push(NotChecked {
            id: "app_environment",
            label: "the app's own env",
            detail: reason.into(),
        });
    }
    if inputs.in_container {
        not_checked.push(NotChecked {
            id: "container_etc",
            label: "host /etc",
            detail: "Loupe is in a container and may see a different /etc than T3".into(),
        });
    }
    if matches!(inputs.platform, Platform::Other) {
        not_checked.push(NotChecked {
            id: "platform",
            label: "this platform",
            detail: "telemetry checks are supported on Linux and macOS only".into(),
        });
    }

    // 13. Overall verdict (§5.5)
    let has_opt_out_ignored = findings.iter().any(|f| f.severity == Severity::OptOutIgnored);
    let any_restart = product_stream.state == StreamState::RestartNeeded
        || relay_stream.state == StreamState::RestartNeeded;
    let any_unverified_env_off = [(&product_stream), (&relay_stream)].iter().any(|s| {
        s.state == StreamState::Off && s.confidence == Confidence::Unverified && server_alive
    });
    let no_t3_or_policy = !inputs.data_folder_exists
        && matches!(&inputs.policy, Read::Absent)
        && !inputs.macos_managed_prefs_present;

    let verdict = if matches!(inputs.platform, Platform::Other) {
        Verdict::Unverified
    } else if has_opt_out_ignored {
        Verdict::Misconfigured
    } else if any_restart {
        Verdict::RestartNeeded
    } else if any_unverified_env_off
        || product_stream.state == StreamState::Unverified
        || relay_stream.state == StreamState::Unverified
    {
        Verdict::Unverified
    } else if no_t3_or_policy
        && product_stream.state == StreamState::On
        && relay_stream.state == StreamState::On
    {
        Verdict::Unverified
    } else {
        match (product_stream.state, relay_stream.state) {
            (StreamState::Off, StreamState::Off) => Verdict::Off,
            (StreamState::Off | StreamState::Blocked, StreamState::Off | StreamState::Blocked) => {
                Verdict::Blocked
            }
            (StreamState::On | StreamState::OptOutIgnored, StreamState::On | StreamState::OptOutIgnored) => {
                Verdict::On
            }
            _ => Verdict::Partial,
        }
    };

    let summary = build_summary(verdict, &product_stream, &relay_stream, no_t3_or_policy, &inputs);

    // 14. Remediation snippets (chosen by launch path, only when not already Off)
    let fix = build_fix_snippets(verdict, &inputs, service_managed);

    // Sort findings: OptOutIgnored -> Restart -> Warn -> Caveat -> Info
    findings.sort_by_key(|f| match f.severity {
        Severity::OptOutIgnored => 0,
        Severity::Restart => 1,
        Severity::Warn => 2,
        Severity::Caveat => 3,
        Severity::Info => 4,
    });

    Report {
        verdict,
        summary,
        streams: vec![product_stream, relay_stream, custom_stream],
        other,
        findings,
        not_checked,
        server: server_view,
        fix,
        inputs,
    }
}

fn is_otel_sdk_disabled(env: &EnvSet) -> bool {
    match env.get(EnvKey::T3OtelSdkDisabled) {
        Some(EnvValue::Bool {
            parsed: Bool3::True,
            ..
        }) => true,
        Some(EnvValue::Bool {
            parsed: Bool3::False,
            ..
        }) => false,
        _ => matches!(
            env.get(EnvKey::OtelSdkDisabled),
            Some(EnvValue::Bool {
                parsed: Bool3::True,
                ..
            })
        ),
    }
}

fn display_service_env_path(inputs: &Inputs) -> String {
    let raw = inputs.data_folder.join("service.env");
    let s = raw.to_string_lossy();
    if let Some(h) = inputs.home_display.as_deref().filter(|h| h.len() > 1) {
        let prefix = format!("{h}/");
        if let Some(rest) = s.strip_prefix(&prefix) {
            return format!("~/{rest}");
        }
    }
    s.into_owned()
}

/// Quotes a filesystem path for safe inclusion in a POSIX shell command.
/// Preserves tilde expansion (`~/...`) while single-quoting spaces, quotes, newlines,
/// `$()`, backticks, `;`, and any other shell metacharacters.
pub(crate) fn shell_quote_path(path: &str) -> String {
    let safe_unquoted = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'+' | b'-'))
    };
    let single_quote = |s: &str| {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for ch in s.chars() {
            if ch == '\0' {
                continue;
            }
            if ch == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(ch);
            }
        }
        out.push('\'');
        out
    };
    let cleaned: String = path.chars().filter(|&c| c != '\0').collect();
    if let Some(rest) = cleaned.strip_prefix("~/") {
        if safe_unquoted(rest) {
            return cleaned;
        }
        return format!("~/{}", single_quote(rest));
    }
    if safe_unquoted(&cleaned) {
        cleaned
    } else {
        single_quote(&cleaned)
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_product_events(
    inputs: &Inputs,
    eff_env: &EffectiveEnv<'_>,
    policy_denies: bool,
    policy_unreadable: bool,
    policy_invalid: bool,
    macos_profile_active: bool,
    macos_dual_policy: bool,
    policy_restart_reason: Option<&str>,
    env_config_stale_source: Option<&str>,
    server_alive: bool,
    runtime_ok: Option<&Runtime>,
    posthog_host: &str,
    posthog_overridden: bool,
    posthog_invalid: bool,
    findings: &mut Vec<Finding>,
) -> Stream {
    let mut ladder = Vec::new();
    let dest = if posthog_overridden {
        format!("{posthog_host} (custom)")
    } else {
        format!("{DEFAULT_POSTHOG_HOST} · PostHog")
    };

    if macos_dual_policy {
        ladder.push(LadderStep {
            step: "macOS policy sources".into(),
            value: "both plist & policy.json".into(),
            parsed: Some("conflict".into()),
            decisive: true,
            state_hint: Some(StreamState::Unverified),
        });
        return Stream {
            id: StreamId::ProductEvents,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: posthog_overridden,
            decided_step: "policy".into(),
            decided_detail: "both macOS plist & policy.json exist (T3 won't start)".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    if macos_profile_active {
        ladder.push(LadderStep {
            step: "macOS Managed Prefs".into(),
            value: "plist present (not opened)".into(),
            parsed: Some("unverified".into()),
            decisive: true,
            state_hint: Some(StreamState::Unverified),
        });
        return Stream {
            id: StreamId::ProductEvents,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: posthog_overridden,
            decided_step: "policy".into(),
            decided_detail: "macOS managed profile (not opened)".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    // Step 1: managed policy (optional system-wide override; not created by T3 by default)
    let policy_val = match &inputs.policy {
        Read::Absent => "not set (optional)".to_owned(),
        Read::Unreadable(k) => format!("unreadable ({k})"),
        Read::Invalid { .. } => "invalid (blocks startup)".to_owned(),
        Read::Ok { value, .. } => match value.telemetry {
            Some(false) => "telemetry: false".to_owned(),
            Some(true) => "telemetry: true".to_owned(),
            None => "telemetry unset".to_owned(),
        },
    };
    if policy_invalid {
        ladder.push(LadderStep {
            step: "policy.json (optional)".into(),
            value: policy_val,
            parsed: Some("invalid".into()),
            decisive: true,
            state_hint: Some(StreamState::Unverified),
        });
        return Stream {
            id: StreamId::ProductEvents,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: posthog_overridden,
            decided_step: "policy".into(),
            decided_detail: "policy.json invalid (T3 won't start)".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    if policy_denies {
        let (state, conf, detail) = match policy_restart_reason {
            Some(reason) => (
                StreamState::RestartNeeded,
                Confidence::Verified,
                reason.to_owned(),
            ),
            None if server_alive
                && runtime_ok.is_some_and(|rt| matches!(rt.managed_policy, Some(Some(_)))) =>
            {
                (
                    StreamState::Off,
                    Confidence::Verified,
                    "managed policy · loaded by T3".to_owned(),
                )
            }
            None => (
                StreamState::Off,
                Confidence::Likely,
                "managed policy · applies when T3 starts".to_owned(),
            ),
        };
        ladder.push(LadderStep {
            step: "policy.json (optional)".into(),
            value: policy_val,
            parsed: Some("false".into()),
            decisive: true,
            state_hint: Some(state),
        });
        return Stream {
            id: StreamId::ProductEvents,
            state,
            confidence: conf,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: posthog_overridden,
            decided_step: "policy".into(),
            decided_detail: detail,
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    ladder.push(LadderStep {
        step: "policy.json (optional)".into(),
        value: policy_val,
        parsed: None,
        decisive: false,
        state_hint: None,
    });

    // Optional transparency step on Linux desktop: show that desktop launcher env had no opt-out
    if matches!(inputs.platform, Platform::Linux)
        && matches!(inputs.server.launch, Launch::Desktop(_))
        && !matches!(eff_env, EffectiveEnv::DesktopConfig(_, _, _))
    {
        let hint = if inputs.omarchy_hypr_detected {
            "none in hyprland.lua / env.d"
        } else if inputs.uwsm_detected {
            "none in uwsm/env / env.d"
        } else {
            "none in environment.d / /etc/environment"
        };
        ladder.push(LadderStep {
            step: "desktop session env".into(),
            value: hint.into(),
            parsed: None,
            decisive: false,
            state_hint: None,
        });
    }

    // Step 2: T3CODE_TELEMETRY_ENABLED (strict Effect Config.Boolean for product events)
    let env_entry = eff_env.set().get(EnvKey::TelemetryEnabled);
    match env_entry {
        Some(EnvValue::TelemetryBool {
            raw,
            product_parsed: Bool3::False,
            ..
        }) => {
            let (state, detail) = if let Some(stale_src) = env_config_stale_source {
                (
                    StreamState::RestartNeeded,
                    format!("{stale_src} changed after T3 started"),
                )
            } else if eff_env.is_unverified_for_running() {
                let d = match &inputs.shell_rc_source {
                    Some(rc) if rc.interactive_only && matches!(inputs.platform, Platform::Linux) => {
                        format!("only in {} · desktop skips it", rc.path)
                    }
                    Some(rc) => format!("in {} · app env private", rc.path),
                    None => "set in this shell · app env private".to_owned(),
                };
                (StreamState::Unverified, d)
            } else {
                (
                    StreamState::Off,
                    format!("T3CODE_TELEMETRY_ENABLED={raw} ({})", eff_env.detail_source()),
                )
            };
            let ladder_value = if eff_env.is_unverified_for_running() {
                match &inputs.shell_rc_source {
                    Some(rc) => format!("\"{raw}\" ({} only)", rc.path),
                    None => format!("\"{raw}\" (this shell only)"),
                }
            } else if let EffectiveEnv::DesktopConfig(_, src, _) = eff_env {
                format!("\"{raw}\" ({src})")
            } else {
                format!("\"{raw}\" → false")
            };
            ladder.push(LadderStep {
                step: "T3CODE_TELEMETRY_ENABLED".into(),
                value: ladder_value,
                parsed: Some("false".into()),
                decisive: true,
                state_hint: Some(state),
            });
            return Stream {
                id: StreamId::ProductEvents,
                state,
                confidence: eff_env.confidence(),
                counts_toward_verdict: true,
                destination: dest,
                destination_host: Some(posthog_host.to_owned()),
                destination_overridden: posthog_overridden,
                decided_step: "T3CODE_TELEMETRY_ENABLED".into(),
                decided_detail: detail,
                ladder,
                caveat: None,
                exporters: Vec::new(),
            };
        }
        Some(EnvValue::TelemetryBool {
            raw,
            product_parsed: Bool3::True,
            ..
        }) => {
            ladder.push(LadderStep {
                step: "T3CODE_TELEMETRY_ENABLED".into(),
                value: format!("\"{raw}\" → true"),
                parsed: Some("true".into()),
                decisive: false,
                state_hint: None,
            });
        }
        Some(EnvValue::TelemetryBool {
            raw,
            product_parsed: Bool3::Invalid,
            relay_parsed,
        }) => {
            ladder.push(LadderStep {
                step: "T3CODE_TELEMETRY_ENABLED".into(),
                value: format!("\"{raw}\" → config error"),
                parsed: Some("invalid".into()),
                decisive: true,
                state_hint: Some(StreamState::Unverified),
            });
            let msg = if *relay_parsed == Bool3::False {
                format!(
                    "T3CODE_TELEMETRY_ENABLED=\"{raw}\" is rejected by T3's analytics config (Effect Config.Boolean requires exact lowercase like \"false\" with no surrounding spaces) and can stop the server from starting."
                )
            } else {
                format!(
                    "T3CODE_TELEMETRY_ENABLED=\"{raw}\" is not a valid lowercase boolean (true/false/yes/no/on/off/1/0/y/n); T3's analytics config rejects it on startup."
                )
            };
            findings.push(Finding {
                id: "invalid_telemetry_enabled",
                severity: Severity::Warn,
                stream: Some(StreamId::ProductEvents),
                variable: Some("T3CODE_TELEMETRY_ENABLED"),
                value: Some(raw.clone()),
                where_set: Some(eff_env.where_label()),
                pr: None,
                message: msg,
                fix: Some("Set T3CODE_TELEMETRY_ENABLED=false (exact lowercase)".into()),
            });
            return Stream {
                id: StreamId::ProductEvents,
                state: StreamState::Unverified,
                confidence: Confidence::Unverified,
                counts_toward_verdict: true,
                destination: dest,
                destination_host: Some(posthog_host.to_owned()),
                destination_overridden: posthog_overridden,
                decided_step: "T3CODE_TELEMETRY_ENABLED".into(),
                decided_detail: format!("invalid value \"{raw}\" (must be lowercase)"),
                ladder,
                caveat: None,
                exporters: Vec::new(),
            };
        }
        _ => {
            ladder.push(LadderStep {
                step: "T3CODE_TELEMETRY_ENABLED".into(),
                value: "unset, so on (T3's default)".into(),
                parsed: Some("unset".into()),
                decisive: false,
                state_hint: None,
            });
        }
    }

    if posthog_invalid {
        return Stream {
            id: StreamId::ProductEvents,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: true,
            decided_step: "T3CODE_POSTHOG_HOST".into(),
            decided_detail: "invalid URL".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    // Step 3: delivery blocks (loopback PostHog host or /etc/hosts sinkhole)
    if is_sinkhole_or_loopback_host(posthog_host) {
        ladder.push(LadderStep {
            step: "T3CODE_POSTHOG_HOST".into(),
            value: format!("{posthog_host} (loopback)"),
            parsed: Some("blocked".into()),
            decisive: true,
            state_hint: Some(StreamState::Blocked),
        });
        return Stream {
            id: StreamId::ProductEvents,
            state: StreamState::Blocked,
            confidence: eff_env.confidence(),
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: true,
            decided_step: "T3CODE_POSTHOG_HOST".into(),
            decided_detail: format!("loopback host: {posthog_host}"),
            ladder,
            caveat: Some("T3 still builds events; the loopback host only stops delivery.".into()),
            exporters: Vec::new(),
        };
    }

    let nss_uses_files = match &inputs.nsswitch {
        Read::Ok { value, .. } => *value,
        _ => true,
    };

    if let Read::Ok { value: scan, .. } = &inputs.hosts {
        if let Some(m) = scan.matches.first() {
            if m.sinkhole {
                if nss_uses_files {
                    ladder.push(LadderStep {
                        step: format!("hosts file, {posthog_host}"),
                        value: m.ip.clone(),
                        parsed: Some("blocked".into()),
                        decisive: true,
                        state_hint: Some(StreamState::Blocked),
                    });
                    let caveat = "T3 still builds these events; the hosts entry only stops delivery. A VPN, DNS over HTTPS or a container can get round it.".to_owned();
                    findings.push(Finding {
                        id: "hosts_block_is_delivery_only",
                        severity: Severity::Caveat,
                        stream: Some(StreamId::ProductEvents),
                        variable: None,
                        value: None,
                        where_set: Some("/etc/hosts"),
                        pr: None,
                        message: "The hosts entry stops delivery only. A VPN or DNS over HTTPS can get round it.".into(),
                        fix: None,
                    });
                    return Stream {
                        id: StreamId::ProductEvents,
                        state: StreamState::Blocked,
                        confidence: Confidence::Likely,
                        counts_toward_verdict: true,
                        destination: dest,
                        destination_host: Some(posthog_host.to_owned()),
                        destination_overridden: posthog_overridden,
                        decided_step: "hosts".into(),
                        decided_detail: format!("hosts: {posthog_host} → {}", m.ip),
                        ladder,
                        caveat: Some(caveat),
                        exporters: Vec::new(),
                    };
                } else {
                    ladder.push(LadderStep {
                        step: format!("hosts file, {posthog_host}"),
                        value: format!("{} (nsswitch skips files)", m.ip),
                        parsed: None,
                        decisive: false,
                        state_hint: Some(StreamState::OptOutIgnored),
                    });
                    findings.push(Finding {
                        id: "nsswitch_skips_files",
                        severity: Severity::OptOutIgnored,
                        stream: Some(StreamId::ProductEvents),
                        variable: None,
                        value: None,
                        where_set: Some("/etc/nsswitch.conf"),
                        pr: None,
                        message: "/etc/nsswitch.conf omits `files` for hosts, so the /etc/hosts block is not consulted.".into(),
                        fix: None,
                    });
                }
            } else {
                ladder.push(LadderStep {
                    step: format!("hosts file, {posthog_host}"),
                    value: format!("redirected to {}", m.ip),
                    parsed: None,
                    decisive: false,
                    state_hint: Some(StreamState::On),
                });
                findings.push(Finding {
                    id: "hosts_non_sinkhole",
                    severity: Severity::Warn,
                    stream: Some(StreamId::ProductEvents),
                    variable: None,
                    value: Some(m.ip.clone()),
                    where_set: Some("/etc/hosts"),
                    pr: None,
                    message: format!(
                        "/etc/hosts points {posthog_host} to {} (not a local sinkhole like 0.0.0.0 or 127.0.0.1).",
                        m.ip
                    ),
                    fix: None,
                });
            }
        } else if scan.bare_parent_only {
            ladder.push(LadderStep {
                step: format!("hosts file, {posthog_host}"),
                value: "only t3.codes listed".into(),
                parsed: None,
                decisive: false,
                state_hint: Some(StreamState::On),
            });
            findings.push(Finding {
                id: "hosts_parent_domain_only",
                severity: Severity::Warn,
                stream: Some(StreamId::ProductEvents),
                variable: None,
                value: Some("t3.codes".into()),
                where_set: Some("/etc/hosts"),
                pr: None,
                message: format!(
                    "/etc/hosts blocks t3.codes, not {posthog_host} (hosts entries do not cover subdomains)."
                ),
                fix: Some(format!("Add `0.0.0.0 {posthog_host}` or set T3CODE_TELEMETRY_ENABLED=false.")),
            });
        } else {
            ladder.push(LadderStep {
                step: format!("hosts file, {posthog_host}"),
                value: "no entry".into(),
                parsed: None,
                decisive: false,
                state_hint: None,
            });
        }
    }

    // If policy.json was unreadable and runtime didn't prove no policy loaded, state is Unverified (§5.6).
    let runtime_proved_no_policy =
        server_alive && runtime_ok.is_some_and(|rt| matches!(rt.managed_policy, Some(None)));
    if policy_unreadable && !runtime_proved_no_policy {
        return Stream {
            id: StreamId::ProductEvents,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: Some(posthog_host.to_owned()),
            destination_overridden: posthog_overridden,
            decided_step: "policy".into(),
            decided_detail: "policy.json unreadable".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    // Default ON
    let explicit_on = matches!(
        env_entry,
        Some(EnvValue::TelemetryBool {
            product_parsed: Bool3::True,
            ..
        })
    );
    let detail = if explicit_on {
        "T3CODE_TELEMETRY_ENABLED=true".to_owned()
    } else {
        "T3's default (nothing turns it off)".to_owned()
    };
    let conf = if eff_env.is_unverified_for_running() {
        Confidence::Likely
    } else {
        eff_env.confidence()
    };

    Stream {
        id: StreamId::ProductEvents,
        state: StreamState::On,
        confidence: conf,
        counts_toward_verdict: true,
        destination: dest,
        destination_host: Some(posthog_host.to_owned()),
        destination_overridden: posthog_overridden,
        decided_step: if explicit_on {
            "T3CODE_TELEMETRY_ENABLED".into()
        } else {
            "default".into()
        },
        decided_detail: detail,
        ladder,
        caveat: None,
        exporters: Vec::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_relay_traces(
    inputs: &Inputs,
    eff_env: &EffectiveEnv<'_>,
    policy_denies: bool,
    policy_unreadable: bool,
    policy_invalid: bool,
    macos_profile_active: bool,
    macos_dual_policy: bool,
    policy_restart_reason: Option<&str>,
    env_config_stale_source: Option<&str>,
    server_alive: bool,
    runtime_ok: Option<&Runtime>,
    findings: &mut Vec<Finding>,
) -> Stream {
    let mut ladder = Vec::new();
    let dest = "Axiom · T3 Connect".to_owned();

    if macos_dual_policy {
        ladder.push(LadderStep {
            step: "macOS policy sources".into(),
            value: "both plist & policy.json".into(),
            parsed: Some("conflict".into()),
            decisive: true,
            state_hint: Some(StreamState::Unverified),
        });
        return Stream {
            id: StreamId::RelayTraces,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "policy".into(),
            decided_detail: "both macOS plist & policy.json exist (T3 won't start)".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    if macos_profile_active {
        ladder.push(LadderStep {
            step: "macOS Managed Prefs".into(),
            value: "plist present (not opened)".into(),
            parsed: Some("unverified".into()),
            decisive: true,
            state_hint: Some(StreamState::Unverified),
        });
        return Stream {
            id: StreamId::RelayTraces,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "policy".into(),
            decided_detail: "macOS managed profile (not opened)".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    // Step 1: managed policy (optional)
    let policy_val = match &inputs.policy {
        Read::Absent => "not set (optional)".to_owned(),
        Read::Unreadable(k) => format!("unreadable ({k})"),
        Read::Invalid { .. } => "invalid (blocks startup)".to_owned(),
        Read::Ok { value, .. } => match value.telemetry {
            Some(false) => "telemetry: false".to_owned(),
            Some(true) => "telemetry: true".to_owned(),
            None => "telemetry unset".to_owned(),
        },
    };
    if policy_invalid {
        ladder.push(LadderStep {
            step: "policy.json (optional)".into(),
            value: policy_val,
            parsed: Some("invalid".into()),
            decisive: true,
            state_hint: Some(StreamState::Unverified),
        });
        return Stream {
            id: StreamId::RelayTraces,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "policy".into(),
            decided_detail: "policy.json invalid (T3 won't start)".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    if policy_denies {
        let (state, conf, detail) = match policy_restart_reason {
            Some(reason) => (
                StreamState::RestartNeeded,
                Confidence::Verified,
                reason.to_owned(),
            ),
            None if server_alive
                && runtime_ok.is_some_and(|rt| matches!(rt.managed_policy, Some(Some(_)))) =>
            {
                (
                    StreamState::Off,
                    Confidence::Verified,
                    "managed policy · loaded by T3".to_owned(),
                )
            }
            None => (
                StreamState::Off,
                Confidence::Likely,
                "managed policy · applies when T3 starts".to_owned(),
            ),
        };
        ladder.push(LadderStep {
            step: "policy.json (optional)".into(),
            value: policy_val,
            parsed: Some("false".into()),
            decisive: true,
            state_hint: Some(state),
        });
        return Stream {
            id: StreamId::RelayTraces,
            state,
            confidence: conf,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "policy".into(),
            decided_detail: detail,
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    ladder.push(LadderStep {
        step: "policy.json (optional)".into(),
        value: policy_val,
        parsed: None,
        decisive: false,
        state_hint: None,
    });

    if matches!(inputs.platform, Platform::Linux)
        && matches!(inputs.server.launch, Launch::Desktop(_))
        && !matches!(eff_env, EffectiveEnv::DesktopConfig(_, _, _))
    {
        let hint = if inputs.omarchy_hypr_detected {
            "none in hyprland.lua / env.d"
        } else if inputs.uwsm_detected {
            "none in uwsm/env / env.d"
        } else {
            "none in environment.d / /etc/environment"
        };
        ladder.push(LadderStep {
            step: "desktop session env".into(),
            value: hint.into(),
            parsed: None,
            decisive: false,
            state_hint: None,
        });
    }

    // Step 2: T3CODE_TELEMETRY_ENABLED=false also disables relay tracing
    // (`packages/shared/src/otelEnvironment.ts:325`, trimmed + lowercased)
    let tel_env = eff_env.set().get(EnvKey::TelemetryEnabled);
    if let Some(EnvValue::TelemetryBool {
        raw,
        relay_parsed: Bool3::False,
        ..
    }) = tel_env
    {
        let (state, detail) = if let Some(stale_src) = env_config_stale_source {
            (
                StreamState::RestartNeeded,
                format!("{stale_src} changed after T3 started"),
            )
        } else if eff_env.is_unverified_for_running() {
            let d = match &inputs.shell_rc_source {
                Some(rc) if rc.interactive_only && matches!(inputs.platform, Platform::Linux) => {
                    format!("only in {} · desktop skips it", rc.path)
                }
                Some(rc) => format!("in {} · app env private", rc.path),
                None => "set in this shell · app env private".to_owned(),
            };
            (StreamState::Unverified, d)
        } else {
            (
                StreamState::Off,
                format!("T3CODE_TELEMETRY_ENABLED={raw} ({})", eff_env.detail_source()),
            )
        };
        let ladder_value = if eff_env.is_unverified_for_running() {
            match &inputs.shell_rc_source {
                Some(rc) => format!("\"{raw}\" ({} only)", rc.path),
                None => format!("\"{raw}\" (this shell only)"),
            }
        } else if let EffectiveEnv::DesktopConfig(_, src, _) = eff_env {
            format!("\"{raw}\" ({src})")
        } else {
            format!("\"{raw}\" → false")
        };
        ladder.push(LadderStep {
            step: "T3CODE_TELEMETRY_ENABLED".into(),
            value: ladder_value,
            parsed: Some("false".into()),
            decisive: true,
            state_hint: Some(state),
        });
        return Stream {
            id: StreamId::RelayTraces,
            state,
            confidence: eff_env.confidence(),
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "T3CODE_TELEMETRY_ENABLED".into(),
            decided_detail: detail,
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }
    ladder.push(LadderStep {
        step: "T3CODE_TELEMETRY_ENABLED".into(),
        value: match tel_env {
            Some(EnvValue::TelemetryBool {
                raw,
                relay_parsed: Bool3::True,
                ..
            }) => format!("\"{raw}\" → true"),
            Some(EnvValue::TelemetryBool {
                raw,
                relay_parsed: Bool3::Invalid,
                ..
            }) => format!("\"{raw}\" → invalid"),
            _ => "unset".into(),
        },
        parsed: None,
        decisive: false,
        state_hint: None,
    });

    // Step 3 & 4: T3CODE_OTEL_SDK_DISABLED ?? OTEL_SDK_DISABLED ?? false
    // (`packages/shared/src/otelEnvironment.ts:287`)
    let t3_otel = eff_env.set().get(EnvKey::T3OtelSdkDisabled);
    let spec_otel = eff_env.set().get(EnvKey::OtelSdkDisabled);

    let t3_val = match t3_otel {
        Some(EnvValue::Bool { raw, parsed: Bool3::True }) => {
            let state = if env_config_stale_source.is_some() {
                StreamState::RestartNeeded
            } else if eff_env.is_unverified_for_running() {
                StreamState::Unverified
            } else {
                StreamState::Off
            };
            let ladder_value = if eff_env.is_unverified_for_running() {
                match &inputs.shell_rc_source {
                    Some(rc) => format!("\"{raw}\" ({} only)", rc.path),
                    None => format!("\"{raw}\" (this shell only)"),
                }
            } else if let EffectiveEnv::DesktopConfig(_, src, _) = eff_env {
                format!("\"{raw}\" ({src})")
            } else {
                format!("\"{raw}\" → true")
            };
            ladder.push(LadderStep {
                step: "T3CODE_OTEL_SDK_DISABLED".into(),
                value: ladder_value,
                parsed: Some("true".into()),
                decisive: true,
                state_hint: Some(state),
            });
            Some(true)
        }
        Some(EnvValue::Bool { raw, parsed: Bool3::False }) => {
            let overrides_spec = matches!(
                spec_otel,
                Some(EnvValue::Bool {
                    parsed: Bool3::True,
                    ..
                })
            );
            ladder.push(LadderStep {
                step: "T3CODE_OTEL_SDK_DISABLED".into(),
                value: format!("\"{raw}\" → false"),
                parsed: Some("false".into()),
                decisive: overrides_spec,
                state_hint: overrides_spec.then_some(StreamState::OptOutIgnored),
            });
            if overrides_spec {
                findings.push(Finding {
                    id: "t3_otel_overrides_spec",
                    severity: Severity::OptOutIgnored,
                    stream: Some(StreamId::RelayTraces),
                    variable: Some("T3CODE_OTEL_SDK_DISABLED"),
                    value: Some(raw.clone()),
                    where_set: Some(eff_env.where_label()),
                    pr: None,
                    message: format!(
                        "T3CODE_OTEL_SDK_DISABLED={raw} overrides OTEL_SDK_DISABLED=true, leaving relay traces on."
                    ),
                    fix: Some("Unset T3CODE_OTEL_SDK_DISABLED or set it to true.".into()),
                });
            }
            Some(false)
        }
        Some(EnvValue::Bool { raw, parsed: Bool3::Invalid }) => {
            ladder.push(LadderStep {
                step: "T3CODE_OTEL_SDK_DISABLED".into(),
                value: format!("\"{raw}\" → ignored"),
                parsed: Some("invalid".into()),
                decisive: false,
                state_hint: Some(StreamState::OptOutIgnored),
            });
            findings.push(Finding {
                id: "invalid_t3_otel_sdk_disabled",
                severity: Severity::OptOutIgnored,
                stream: Some(StreamId::RelayTraces),
                variable: Some("T3CODE_OTEL_SDK_DISABLED"),
                value: Some(raw.clone()),
                where_set: Some(eff_env.where_label()),
                pr: None,
                message: format!(
                    "T3CODE_OTEL_SDK_DISABLED={raw} is not a yes/no value and was ignored."
                ),
                fix: Some("Set T3CODE_OTEL_SDK_DISABLED=true".into()),
            });
            None
        }
        _ => {
            ladder.push(LadderStep {
                step: "T3CODE_OTEL_SDK_DISABLED".into(),
                value: "unset".into(),
                parsed: None,
                decisive: false,
                state_hint: None,
            });
            None
        }
    };

    if t3_val == Some(true) {
        let (state, detail) = if let Some(stale_src) = env_config_stale_source {
            (
                StreamState::RestartNeeded,
                format!("{stale_src} changed after T3 started"),
            )
        } else if eff_env.is_unverified_for_running() {
            let d = match &inputs.shell_rc_source {
                Some(rc) if rc.interactive_only && matches!(inputs.platform, Platform::Linux) => {
                    format!("only in {} · desktop skips it", rc.path)
                }
                Some(rc) => format!("in {} · app env private", rc.path),
                None => "set in this shell · app env private".into(),
            };
            (StreamState::Unverified, d)
        } else {
            (
                StreamState::Off,
                format!("T3CODE_OTEL_SDK_DISABLED=true ({})", eff_env.detail_source()),
            )
        };
        return Stream {
            id: StreamId::RelayTraces,
            state,
            confidence: eff_env.confidence(),
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "T3CODE_OTEL_SDK_DISABLED".into(),
            decided_detail: detail,
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    // Step 4: OTEL_SDK_DISABLED
    let mut otel_misconfigured: Option<String> = None;
    let spec_disabled = match spec_otel {
        Some(EnvValue::Bool { raw, parsed: Bool3::True }) if t3_val.is_none() => {
            let state = if env_config_stale_source.is_some() {
                StreamState::RestartNeeded
            } else if eff_env.is_unverified_for_running() {
                StreamState::Unverified
            } else {
                StreamState::Off
            };
            let ladder_value = if eff_env.is_unverified_for_running() {
                match &inputs.shell_rc_source {
                    Some(rc) => format!("\"{raw}\" ({} only)", rc.path),
                    None => format!("\"{raw}\" (this shell only)"),
                }
            } else if let EffectiveEnv::DesktopConfig(_, src, _) = eff_env {
                format!("\"{raw}\" ({src})")
            } else {
                format!("\"{raw}\" → true")
            };
            ladder.push(LadderStep {
                step: "OTEL_SDK_DISABLED".into(),
                value: ladder_value,
                parsed: Some("true".into()),
                decisive: true,
                state_hint: Some(state),
            });
            true
        }
        Some(EnvValue::Bool { raw, parsed: Bool3::True }) => {
            // Overridden by T3CODE_OTEL_SDK_DISABLED=false above
            ladder.push(LadderStep {
                step: "OTEL_SDK_DISABLED".into(),
                value: format!("\"{raw}\" (overridden)"),
                parsed: Some("true".into()),
                decisive: false,
                state_hint: None,
            });
            false
        }
        Some(EnvValue::Bool { raw, parsed: Bool3::Invalid }) => {
            let looks_truthy = matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "yes" | "on" | "y"
            );
            ladder.push(LadderStep {
                step: "OTEL_SDK_DISABLED".into(),
                value: format!("\"{raw}\" → false"),
                parsed: Some("false".into()),
                decisive: true,
                state_hint: Some(if looks_truthy {
                    StreamState::OptOutIgnored
                } else {
                    StreamState::On
                }),
            });
            if looks_truthy {
                otel_misconfigured = Some(raw.clone());
                findings.push(Finding {
                    id: "otel_sdk_disabled_not_true",
                    severity: Severity::OptOutIgnored,
                    stream: Some(StreamId::RelayTraces),
                    variable: Some("OTEL_SDK_DISABLED"),
                    value: Some(raw.clone()),
                    where_set: Some(eff_env.where_label()),
                    pr: None,
                    message: format!(
                        "OTEL_SDK_DISABLED takes only \"true\", so \"{raw}\" counts as false. T3's own switch, T3CODE_OTEL_SDK_DISABLED, takes 1, yes and on."
                    ),
                    fix: Some("T3CODE_OTEL_SDK_DISABLED=true".into()),
                });
            }
            false
        }
        Some(EnvValue::Bool { raw, parsed: Bool3::False }) => {
            ladder.push(LadderStep {
                step: "OTEL_SDK_DISABLED".into(),
                value: format!("\"{raw}\" → false"),
                parsed: Some("false".into()),
                decisive: false,
                state_hint: None,
            });
            false
        }
        _ => {
            ladder.push(LadderStep {
                step: "OTEL_SDK_DISABLED".into(),
                value: "unset".into(),
                parsed: None,
                decisive: false,
                state_hint: None,
            });
            false
        }
    };

    if spec_disabled {
        let (state, detail) = if let Some(stale_src) = env_config_stale_source {
            (
                StreamState::RestartNeeded,
                format!("{stale_src} changed after T3 started"),
            )
        } else if eff_env.is_unverified_for_running() {
            let d = match &inputs.shell_rc_source {
                Some(rc) if rc.interactive_only && matches!(inputs.platform, Platform::Linux) => {
                    format!("only in {} · desktop skips it", rc.path)
                }
                Some(rc) => format!("in {} · app env private", rc.path),
                None => "set in this shell · app env private".into(),
            };
            (StreamState::Unverified, d)
        } else {
            (
                StreamState::Off,
                format!("OTEL_SDK_DISABLED=true ({})", eff_env.detail_source()),
            )
        };
        return Stream {
            id: StreamId::RelayTraces,
            state,
            confidence: eff_env.confidence(),
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "OTEL_SDK_DISABLED".into(),
            decided_detail: detail,
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    let runtime_proved_no_policy =
        server_alive && runtime_ok.is_some_and(|rt| matches!(rt.managed_policy, Some(None)));
    if policy_unreadable && !runtime_proved_no_policy {
        return Stream {
            id: StreamId::RelayTraces,
            state: StreamState::Unverified,
            confidence: Confidence::Unverified,
            counts_toward_verdict: true,
            destination: dest,
            destination_host: None,
            destination_overridden: false,
            decided_step: "policy".into(),
            decided_detail: "policy.json unreadable".into(),
            ladder,
            caveat: None,
            exporters: Vec::new(),
        };
    }

    let (decided_step, decided_detail, state) = if let Some(raw) = otel_misconfigured {
        (
            "OTEL_SDK_DISABLED".into(),
            format!("OTEL_SDK_DISABLED={raw} is false"),
            StreamState::OptOutIgnored,
        )
    } else if t3_val == Some(false)
        && matches!(
            spec_otel,
            Some(EnvValue::Bool {
                parsed: Bool3::True,
                ..
            })
        )
    {
        (
            "T3CODE_OTEL_SDK_DISABLED".into(),
            "T3CODE_OTEL_SDK_DISABLED=false overrides OTEL_SDK_DISABLED=true".into(),
            StreamState::OptOutIgnored,
        )
    } else {
        (
            "default".into(),
            "T3's default (when using T3 Connect)".into(),
            StreamState::On,
        )
    };

    let conf = if eff_env.is_unverified_for_running() {
        Confidence::Likely
    } else {
        eff_env.confidence()
    };

    Stream {
        id: StreamId::RelayTraces,
        state,
        confidence: conf,
        counts_toward_verdict: true,
        destination: dest,
        destination_host: None,
        destination_overridden: false,
        decided_step,
        decided_detail,
        ladder,
        caveat: Some("Hosted web app and mobile clients have their own Axiom toggles.".into()),
        exporters: Vec::new(),
    }
}

/// Evaluates user-configured OTLP exporters following `resolveSignalEndpoint` in
/// `packages/shared/src/otelEnvironment.ts:197-252`.
fn evaluate_custom_exporters(
    inputs: &Inputs,
    eff_env: &EffectiveEnv<'_>,
    sdk_disabled: bool,
    findings: &mut Vec<Finding>,
) -> Stream {
    let mut entries = Vec::new();
    let mut ladder = Vec::new();
    let env = eff_env.set();

    let settings_exporters = match &inputs.settings {
        Read::Ok { value, .. } => &value.exporters[..],
        _ => &[],
    };

    for signal in Signal::ALL {
        // 1. T3CODE_OTLP_<SIGNAL>_URL takes precedence (even if invalid -> turns signal off)
        let resolved: Option<(Endpoint, String)> = match env.get(EnvKey::T3OtlpUrl(signal)) {
            Some(EnvValue::Endpoint(ep)) => {
                Some((ep.clone(), EnvKey::T3OtlpUrl(signal).name().to_owned()))
            }
            Some(EnvValue::Invalid { .. }) => None,
            _ => {
                // 2. OTEL_<SIGNAL>_EXPORTER=none disables the signal
                if matches!(
                    env.get(EnvKey::OtelExporter(signal)),
                    Some(EnvValue::ExporterNone)
                ) {
                    None
                } else {
                    // 3. Protocol check: per-signal or general OTEL_EXPORTER_OTLP_PROTOCOL
                    let proto_invalid = match env
                        .get(EnvKey::OtlpProtocol(Some(signal)))
                        .or_else(|| env.get(EnvKey::OtlpProtocol(None)))
                    {
                        Some(EnvValue::Invalid { .. }) => true,
                        _ => false,
                    };
                    if proto_invalid {
                        None
                    } else {
                        // 4. Per-signal OTEL_EXPORTER_OTLP_<SIGNAL>_ENDPOINT (invalid -> off, no fallback)
                        match env.get(EnvKey::OtlpEndpoint(Some(signal))) {
                            Some(EnvValue::Endpoint(ep)) => Some((
                                ep.clone(),
                                EnvKey::OtlpEndpoint(Some(signal)).name().to_owned(),
                            )),
                            Some(EnvValue::Invalid { .. }) => None,
                            _ => {
                                // 5. General OTEL_EXPORTER_OTLP_ENDPOINT (invalid -> off, no fallback to settings)
                                match env.get(EnvKey::OtlpEndpoint(None)) {
                                    Some(EnvValue::Endpoint(ep)) => Some((
                                        ep.clone(),
                                        EnvKey::OtlpEndpoint(None).name().to_owned(),
                                    )),
                                    Some(EnvValue::Invalid { .. }) => None,
                                    _ => {
                                        // 6. settings.json observability fallback
                                        settings_exporters
                                            .iter()
                                            .find(|(s, _)| *s == signal)
                                            .map(|(_, ep)| (ep.clone(), "settings".to_owned()))
                                    }
                                }
                            }
                        }
                    }
                }
            }
        };

        if let Some((ep, from)) = resolved {
            ladder.push(LadderStep {
                step: format!("{} ({from})", signal.label()),
                value: if ep.is_loopback() {
                    format!("{} · this machine", ep.host_port())
                } else {
                    ep.display_url()
                },
                parsed: Some(if sdk_disabled { "silenced".into() } else { "on".into() }),
                decisive: true,
                state_hint: Some(if sdk_disabled { StreamState::Off } else { StreamState::On }),
            });
            entries.push(ExporterEntry {
                signal,
                endpoint: ep,
                from,
                silenced_by_sdk_disabled: sdk_disabled,
            });
        }
    }

    if sdk_disabled && !entries.is_empty() {
        findings.push(Finding {
            id: "custom_exporters_silenced",
            severity: Severity::Info,
            stream: Some(StreamId::CustomExporters),
            variable: None,
            value: None,
            where_set: None,
            pr: None,
            message: "OTEL SDK is disabled, so your own OTLP exporters are off too.".into(),
            fix: None,
        });
    }

    let (state, detail, dest) = if entries.is_empty() {
        (
            StreamState::None,
            "none configured".to_owned(),
            "your own OTLP receiver".to_owned(),
        )
    } else {
        let first = &entries[0];
        let host_text = if first.endpoint.is_loopback() {
            format!("{} · local", first.endpoint.host_port())
        } else {
            first.endpoint.display_url()
        };
        let extra = if entries.len() > 1 {
            format!(" (+{})", entries.len() - 1)
        } else {
            String::new()
        };
        let sig_word = first.signal.label();
        let detail = if sdk_disabled {
            format!("{sig_word} → {host_text}{extra} (SDK off)")
        } else {
            format!("{sig_word} → {host_text}{extra}")
        };
        (
            if sdk_disabled { StreamState::Off } else { StreamState::On },
            detail,
            host_text,
        )
    };

    Stream {
        id: StreamId::CustomExporters,
        state,
        confidence: Confidence::Read,
        counts_toward_verdict: false,
        destination: dest,
        destination_host: entries.first().map(|e| e.endpoint.host.clone()),
        destination_overridden: false,
        decided_step: entries
            .first()
            .map(|e| e.from.clone())
            .unwrap_or_else(|| "none".into()),
        decided_detail: detail,
        ladder,
        caveat: None,
        exporters: entries,
    }
}

fn build_summary(
    verdict: Verdict,
    product: &Stream,
    relay: &Stream,
    no_t3_or_policy: bool,
    inputs: &Inputs,
) -> String {
    match verdict {
        Verdict::Off => "Product usage events and relay traces are both off.".into(),
        Verdict::Blocked => {
            if relay.state == StreamState::Off {
                "Product usage events are blocked by hosts; relay traces are off.".into()
            } else {
                "Product usage events are blocked at delivery; relay traces are not blocked.".into()
            }
        }
        Verdict::On => "T3 Code's default telemetry is on (usage events and relay traces).".into(),
        Verdict::Partial => {
            if product.state == StreamState::Blocked && relay.state == StreamState::On {
                "Product usage events are blocked; relay traces are still on.".into()
            } else if product.state == StreamState::Off && relay.state == StreamState::On {
                "Product usage events are off; relay traces are still on.".into()
            } else if product.state == StreamState::On && relay.state == StreamState::Off {
                "Relay traces are off; product usage events are still on.".into()
            } else {
                "Some telemetry streams are still on.".into()
            }
        }
        Verdict::Misconfigured => {
            if relay.state == StreamState::OptOutIgnored && product.state == StreamState::Blocked {
                "An opt-out you set is not taking effect: relay traces are still on.".into()
            } else if product.state == StreamState::OptOutIgnored {
                "An opt-out you set is not taking effect: product usage events are still on.".into()
            } else if relay.state == StreamState::OptOutIgnored {
                "An opt-out you set is not taking effect: relay traces are still on.".into()
            } else {
                "An opt-out you set is not taking effect.".into()
            }
        }
        Verdict::RestartNeeded => {
            "Your opt-out is saved on disk, but the running T3 Code server has not loaded it yet."
                .into()
        }
        Verdict::Unverified => {
            if matches!(inputs.platform, Platform::Other) {
                "Telemetry checks are supported on Linux and macOS.".into()
            } else if inputs.macos_managed_prefs_present && !matches!(&inputs.policy, Read::Absent)
            {
                "Both macOS Managed Preferences and policy.json exist; T3 won't start.".into()
            } else if matches!(&inputs.policy, Read::Invalid { .. }) {
                "Managed policy is invalid; T3 Code will not start until policy.json is fixed."
                    .into()
            } else if matches!(&inputs.policy, Read::Unreadable(_)) {
                "Can't read policy.json (check permissions) to verify limits.telemetry.".into()
            } else if inputs.macos_managed_prefs_present {
                "A macOS Managed Preferences plist is active; Loupe does not open plists.".into()
            } else if product.decided_step == "T3CODE_TELEMETRY_ENABLED"
                && product.decided_detail.contains("invalid")
            {
                "T3CODE_TELEMETRY_ENABLED must be exact lowercase (like \"false\"); T3 rejects it."
                    .into()
            } else if no_t3_or_policy {
                "No T3 data folder or policy found here — set T3CODE_HOME if it's elsewhere.".into()
            } else if let Some(rc) = &inputs.shell_rc_source {
                if rc.interactive_only && matches!(inputs.platform, Platform::Linux) {
                    let target = if inputs.omarchy_hypr_detected {
                        "hyprland.lua"
                    } else if inputs.uwsm_detected {
                        "uwsm/env"
                    } else {
                        "environment.d"
                    };
                    format!(
                        "Only in {} — desktop T3 likely skips it. Move to {} or policy.json.",
                        rc.path, target
                    )
                } else {
                    format!(
                        "Set in {}; desktop T3's env is private — use desktop env or policy.json.",
                        rc.path
                    )
                }
            } else {
                "Off in this shell; desktop T3's env is private — use desktop env or policy.json."
                    .into()
            }
        }
    }
}

fn build_fix_snippets(
    verdict: Verdict,
    inputs: &Inputs,
    service_managed: Option<bool>,
) -> Vec<Snippet> {
    if verdict == Verdict::Off {
        return Vec::new();
    }
    if matches!(&inputs.policy, Read::Invalid { .. }) {
        return vec![Snippet {
            for_kind: "policy_fix",
            heading: "TO FIX POLICY".into(),
            command: "t3 policy check".into(),
            note: "T3 Code refuses to start while policy.json is invalid.".into(),
        }];
    }
    if verdict == Verdict::RestartNeeded {
        return vec![Snippet {
            for_kind: "restart",
            heading: "TO APPLY IT".into(),
            command: if service_managed == Some(true) {
                "t3 service restart".into()
            } else {
                "Quit and reopen T3 Code".into()
            },
            note: "T3 reads managed policy and env vars only when its server starts.".into(),
        }];
    }
    let service_env_path = display_service_env_path(inputs);
    let quoted_service_env = shell_quote_path(&service_env_path);

    let (policy_dir, policy_path) = match inputs.platform {
        Platform::MacOs => (
            "\"/Library/Application Support/T3Code\"",
            "\"/Library/Application Support/T3Code/policy.json\"",
        ),
        _ => ("/etc/t3code", "/etc/t3code/policy.json"),
    };
    let policy_snippet = Snippet {
        for_kind: "managed_policy",
        heading: "OPTIONAL MACHINE-WIDE POLICY".into(),
        command: format!(
            "sudo install -d -m 755 {policy_dir} && printf '%s%s%s\\n' '{{\"schemaVersion\":1,' '\"organization\":\"Local\",' '\"limits\":{{\"telemetry\":false}}}}' | sudo tee {policy_path}"
        ),
        note: "optional admin file (T3 never creates one); covers all launch paths & T3 logs it so Loupe marks it verified"
            .into(),
    };

    let session_snippet = if service_managed == Some(true) {
        Snippet {
            for_kind: "service",
            heading: "TO TURN IT ALL OFF".into(),
            command: format!(
                "echo 'T3CODE_TELEMETRY_ENABLED=false' >> {quoted_service_env} && t3 service restart"
            ),
            note: format!("T3 Code is running as a service and reads {service_env_path}."),
        }
    } else if matches!(inputs.platform, Platform::Linux)
        && matches!(inputs.server.launch, Launch::Desktop(_))
    {
        if inputs.omarchy_hypr_detected {
            Snippet {
                for_kind: "desktop_omarchy",
                heading: "TO TURN IT ALL OFF".into(),
                command: "echo 'hl.env(\"T3CODE_TELEMETRY_ENABLED\", \"false\")' >> ~/.config/hypr/hyprland.lua && hyprctl reload".into(),
                note: "no sudo, no logout; restart T3 Code after reload — Loupe reads hyprland.lua and shows ✓ OFF".into(),
            }
        } else if inputs.uwsm_detected {
            Snippet {
                for_kind: "desktop_uwsm",
                heading: "TO TURN IT ALL OFF".into(),
                command: "echo 'export T3CODE_TELEMETRY_ENABLED=false' >> ~/.config/uwsm/env".into(),
                note: "no sudo; log out and back in so T3 starts with it — Loupe reads uwsm/env and shows ✓ OFF".into(),
            }
        } else {
            Snippet {
                for_kind: "desktop_linux",
                heading: "TO TURN IT ALL OFF".into(),
                command: "mkdir -p ~/.config/environment.d && printf 'T3CODE_TELEMETRY_ENABLED=false\\n' > ~/.config/environment.d/t3code.conf".into(),
                note: "no sudo; log out and back in so T3 starts with it — Loupe reads environment.d and shows ✓ OFF".into(),
            }
        }
    } else if matches!(inputs.platform, Platform::MacOs) {
        Snippet {
            for_kind: "shell_macos",
            heading: "TO TURN IT ALL OFF".into(),
            command: "echo 'export T3CODE_TELEMETRY_ENABLED=false' >> ~/.zshrc".into(),
            note: "no sudo; restart T3 Code (desktop app reads your login shell profile)".into(),
        }
    } else {
        Snippet {
            for_kind: "shell",
            heading: "TO TURN IT ALL OFF".into(),
            command: "export T3CODE_TELEMETRY_ENABLED=false".into(),
            note: "add to your shell profile (~/.zshrc or ~/.bashrc), then restart T3 Code".into(),
        }
    };

    // Put the no-sudo native launcher/service option first (Loupe reads & tracks it directly),
    // with the optional machine-wide policy.json right below it for verified all-launch enforcement.
    vec![session_snippet, policy_snippet]
}

/// Builds a self-contained, non-secret `(title, prompt)` pair for `t3 thread start`
/// so a T3 Code agent can turn off or fix telemetry on this machine when the user
/// explicitly asks Loupe to hand it off (`f` on the Telemetry board).
///
/// All variable metadata and recommended options are serialized inside a length-bounded
/// JSON block labeled as untrusted observational data; nothing is interpolated into
/// instruction prose or shell fences.
pub fn t3_handoff_prompt(report: &Report) -> (String, String) {
    let title = if matches!(&report.inputs.policy, Read::Invalid { .. }) {
        "Fix T3 Code telemetry policy".to_owned()
    } else if report.verdict == Verdict::RestartNeeded {
        "Apply T3 Code telemetry opt-out".to_owned()
    } else {
        "Turn off T3 Code telemetry".to_owned()
    };

    let intro = if matches!(&report.inputs.policy, Read::Invalid { .. }) {
        "Loupe's read-only Telemetry Check found an invalid T3 Code managed policy file on this machine (which can prevent T3 Code from starting). Please help me fix it cleanly."
    } else if report.verdict == Verdict::RestartNeeded {
        "Loupe's read-only Telemetry Check found a telemetry opt-out on disk that T3 Code hasn't picked up yet. Please help me verify the configuration and get it applied cleanly."
    } else if report.verdict == Verdict::Unverified {
        "Loupe's read-only Telemetry Check could not verify whether T3 Code's running telemetry is actually off on this machine. Please help me make the opt-out deterministic and verifiable."
    } else {
        "Please help me turn off T3 Code telemetry on this machine based on what Loupe's read-only Telemetry Check found."
    };

    let platform = match report.inputs.platform {
        Platform::Linux => "Linux",
        Platform::MacOs => "macOS",
        Platform::Other => "Other",
    };
    let server_state = match report.server.state {
        Liveness::Running => "running",
        Liveness::Stopped => "stopped",
        Liveness::PidReused => "pid_reused",
        Liveness::Unknown => "unknown",
    };
    let streams: Vec<serde_json::Value> = report
        .streams
        .iter()
        .map(|s| {
            serde_json::json!({
                "stream": s.id.label(),
                "state": s.state.word(),
                "detail": crate::handoff::bound_text(&s.decided_detail, 200),
            })
        })
        .collect();
    let findings: Vec<String> = report
        .findings
        .iter()
        .take(12)
        .map(|f| crate::handoff::bound_text(&f.message, 240))
        .collect();
    let recommended_options: Vec<serde_json::Value> = report
        .fix
        .iter()
        .take(6)
        .map(|snip| {
            serde_json::json!({
                "kind": snip.for_kind,
                "heading": crate::handoff::bound_text(&snip.heading, 80),
                "command": crate::handoff::bound_text(&snip.command, 260),
                "note": crate::handoff::bound_text(&snip.note, 200),
            })
        })
        .collect();
    let payload = serde_json::json!({
        "kind": "telemetry_check",
        "status": report.verdict_heading(),
        "summary": crate::handoff::bound_text(&report.summary, 240),
        "platform": platform,
        "server": {
            "state": server_state,
            "launch": report.server.launch.kind_str(),
        },
        "streams": streams,
        "findings": findings,
        "recommendedOptions": recommended_options,
    });

    let mut body = String::new();
    body.push_str(intro);
    body.push_str("\n\n**Scope:** Machine-wide T3 Code configuration on this host. Do not edit files or make commits in the project repository where this thread was opened.\n\n");
    body.push_str(&crate::handoff::render_untrusted_json(&payload));
    body.push_str("\n## What I'd like you to do\n");
    body.push_str("1. Confirm with me which option to apply (e.g., my desktop/service/shell environment vs machine-wide `policy.json`) before editing system files or running `sudo`.\n");
    body.push_str("2. Apply the change cleanly (and fix any broken opt-out like `OTEL_SDK_DISABLED=1` or invalid `policy.json`).\n");
    body.push_str("3. Run `loupe --telemetry-check` afterward to verify the result, and tell me if I need to restart T3 Code or log out/in for it to take effect.\n");

    (title, body)
}

/// Extracts and validates `threadId` (or `id`) strictly from `t3 thread start --json` output.
/// Rejects non-JSON output, empty values, leading `-`, whitespace/controls, and non-ASCII text.
pub fn parse_t3_thread_id(stdout: &str) -> Option<String> {
    let v = serde_json::from_str::<Value>(stdout.trim()).ok()?;
    let id = v
        .get("threadId")
        .or_else(|| v.get("id"))
        .and_then(|s| s.as_str())?;
    if is_valid_thread_id(id) {
        Some(id.to_owned())
    } else {
        None
    }
}

pub(crate) fn is_valid_thread_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && !id.starts_with('-')
        && id.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;

    fn base_inputs() -> Inputs {
        Inputs {
            source: Source::Machine,
            platform: Platform::Linux,
            checked_at: 1_795_532_700_000.0,
            data_folder_exists: true,
            uwsm_detected: false,
            omarchy_hypr_detected: false,
            in_container: false,
            shell: EnvSet::default(),
            desktop_env: None,
            shell_rc_source: None,
            service_env: Read::Absent,
            policy: Read::Absent,
            macos_managed_prefs_present: false,
            runtime: Read::Absent,
            server: Server {
                liveness: Liveness::Stopped,
                launch: Launch::Unknown,
            },
            settings: Read::Absent,
            hosts: Read::Ok {
                value: HostsScan {
                    target_host: DEFAULT_POSTHOG_HOST.into(),
                    matches: Vec::new(),
                    bare_parent_only: false,
                },
                stamp: Stamp {
                    modified_ms: 1_795_000_000_000.0,
                    len: 200,
                },
            },
            nsswitch: Read::Ok {
                value: true,
                stamp: Stamp {
                    modified_ms: 1_795_000_000_000.0,
                    len: 100,
                },
            },
            install_id: None,
            data_folder: PathBuf::from("~/.t3"),
            home_display: Some("/home/user".into()),
            reads: Vec::new(),
        }
    }

    #[test]
    fn parse_bool_matches_t3_otel_environment() {
        // packages/shared/src/otelEnvironment.ts:91-92, 261-269
        for t in ["true", "TRUE", " yes ", "On", "1", "y", "\tY\n"] {
            assert_eq!(parse_t3_bool(Some(t)), Bool3::True, "{t:?}");
        }
        for f in ["false", "FALSE", " no ", "Off", "0", "n"] {
            assert_eq!(parse_t3_bool(Some(f)), Bool3::False, "{f:?}");
        }
        for u in [None, Some(""), Some("   ")] {
            assert_eq!(parse_t3_bool(u), Bool3::Unset, "{u:?}");
        }
        for bad in ["maybe", "2", "nope", "tru"] {
            assert_eq!(parse_t3_bool(Some(bad)), Bool3::Invalid, "{bad:?}");
        }
        // OTEL_SDK_DISABLED: only literal "true" disables
        assert_eq!(parse_otel_bool(Some("true")), Bool3::True);
        assert_eq!(parse_otel_bool(Some(" TRUE ")), Bool3::True);
        assert_eq!(parse_otel_bool(Some("false")), Bool3::False);
        assert_eq!(parse_otel_bool(Some("1")), Bool3::Invalid);
        assert_eq!(parse_otel_bool(Some("yes")), Bool3::Invalid);
        assert_eq!(parse_otel_bool(Some("")), Bool3::Unset);
        assert_eq!(parse_otel_bool(None), Bool3::Unset);
    }

    #[test]
    fn endpoint_strips_userinfo_path_and_query() {
        let ep = parse_http_endpoint("https://user:CANARY@otlp.example.com:4318/v1/traces?k=CANARY#frag")
            .unwrap();
        assert_eq!(ep.display_url(), "https://otlp.example.com:4318");
        assert!(!ep.is_loopback());
        let local = parse_http_endpoint("http://localhost:4318/v1/traces").unwrap();
        assert!(local.is_loopback());
        assert!(parse_http_endpoint("ftp://example.com").is_none());
        assert!(parse_http_endpoint("not-a-url").is_none());
    }

    #[test]
    fn version_gate_for_pr_42299() {
        assert_eq!(auth_files_hashed_for_version("0.1.11"), AuthFilesHashed::Yes);
        assert_eq!(
            auth_files_hashed_for_version("v0.1.12-nightly.20261123.9999"),
            AuthFilesHashed::Yes
        );
        assert_eq!(
            auth_files_hashed_for_version("0.1.12-nightly.20261124.5208"),
            AuthFilesHashed::Yes
        );
        assert_eq!(
            auth_files_hashed_for_version("0.1.12-nightly.20261124.5209"),
            AuthFilesHashed::No
        );
        assert_eq!(auth_files_hashed_for_version("0.1.12"), AuthFilesHashed::Unknown);
        assert_eq!(auth_files_hashed_for_version("0.1.13"), AuthFilesHashed::No);
        assert_eq!(auth_files_hashed_for_version("garbage"), AuthFilesHashed::Unknown);
    }

    #[test]
    fn precedence_matrix_and_policy_dominance() {
        // Exhaustive property: valid policy with telemetry:false forces both streams Off
        // across every env combination when server is stopped.
        for tel in ["", "true", "false", "bogus"] {
            for t3_otel in ["", "true", "false", "bogus"] {
                for spec in ["", "true", "false", "1"] {
                    let mut inp = base_inputs();
                    inp.policy = Read::Ok {
                        value: Policy {
                            organization: "Local".into(),
                            telemetry: Some(false),
                        },
                        stamp: Stamp {
                            modified_ms: 1_795_000_000_000.0,
                            len: 64,
                        },
                    };
                    inp.shell.set_raw(EnvKey::TelemetryEnabled, tel);
                    inp.shell.set_raw(EnvKey::T3OtelSdkDisabled, t3_otel);
                    inp.shell.set_raw(EnvKey::OtelSdkDisabled, spec);
                    let rep = evaluate(inp);
                    assert_eq!(rep.verdict, Verdict::Off, "tel={tel} t3={t3_otel} spec={spec}");
                    assert_eq!(rep.streams[0].state, StreamState::Off);
                    assert_eq!(rep.streams[1].state, StreamState::Off);
                }
            }
        }

        // OTEL_SDK_DISABLED=1 is Misconfigured when telemetry is not off
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::OtelSdkDisabled, "1");
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::Misconfigured);
        assert!(rep.alert().is_some());

        // T3CODE_OTEL_SDK_DISABLED=false overrides OTEL_SDK_DISABLED=true -> Misconfigured
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::OtelSdkDisabled, "true");
        inp.shell.set_raw(EnvKey::T3OtelSdkDisabled, "false");
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::Misconfigured);
        assert_eq!(rep.streams[1].state, StreamState::OptOutIgnored);

        // T3CODE_OTEL_SDK_DISABLED=true alone -> Partial (relay off, product on)
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::T3OtelSdkDisabled, "true");
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::Partial);
        assert_eq!(rep.streams[0].state, StreamState::On);
        assert_eq!(rep.streams[1].state, StreamState::Off);

        // Default nothing set -> On, and NO live alert
        let rep = evaluate(base_inputs());
        assert_eq!(rep.verdict, Verdict::On);
        assert!(rep.alert().is_none());
    }

    #[test]
    fn restart_staleness_for_policy_and_service_env() {
        let mut inp = base_inputs();
        inp.server.liveness = Liveness::Running;
        inp.policy = Read::Ok {
            value: Policy {
                organization: "Acme".into(),
                telemetry: Some(false),
            },
            stamp: Stamp {
                modified_ms: 1_795_530_000_000.0,
                len: 64,
            },
        };
        // Running server started without policy (explicit managedPolicy: null) -> RestartNeeded
        inp.runtime = Read::Ok {
            value: Runtime {
                version: 1,
                pid: 4242,
                started_at: 1_795_529_000_000.0,
                started_at_raw: "2026-11-24T14:00:00.000Z".into(),
                service_managed: Some(false),
                server_version: Some("0.1.12-nightly.20261124.5209".into()),
                managed_policy: Some(None),
            },
            stamp: Stamp {
                modified_ms: 1_795_529_000_000.0,
                len: 120,
            },
        };
        let rep = evaluate(inp.clone());
        assert_eq!(rep.verdict, Verdict::RestartNeeded);
        assert_eq!(rep.alert().unwrap().kind, AlertKind::RestartNeeded);

        // Older server with absent managedPolicy field (None) and policy written before start
        // must NOT raise a false "T3 started without policy.json" restart warning.
        if let Read::Ok { value, .. } = &mut inp.runtime {
            value.managed_policy = None;
            value.started_at = 1_795_530_000_000.0;
        }
        let rep = evaluate(inp.clone());
        assert_eq!(rep.verdict, Verdict::Off);
        assert_eq!(rep.streams[0].confidence, Confidence::Likely);

        // Policy loaded and mtime <= startedAt + 2s -> Off (Verified)
        if let Read::Ok { value, .. } = &mut inp.runtime {
            value.managed_policy = Some(Some(LoadedPolicy {
                source: "/etc/t3code/policy.json".into(),
                organization: "Acme".into(),
                fingerprint: "abcdef012345".into(),
            }));
            value.started_at = 1_795_530_000_000.0;
        }
        let rep = evaluate(inp.clone());
        assert_eq!(rep.verdict, Verdict::Off);
        assert_eq!(rep.streams[0].confidence, Confidence::Verified);

        // Policy modified > 2s after start -> RestartNeeded
        if let Read::Ok { stamp, .. } = &mut inp.policy {
            stamp.modified_ms = 1_795_530_005_000.0;
        }
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::RestartNeeded);
    }

    #[test]
    fn privacy_canary_and_anonymous_id_never_opened() {
        let dir = std::env::temp_dir().join(format!(
            "loupe-tel-canary-{}-{}",
            std::process::id(),
            crate::clock::wall_now() as u64
        ));
        let userdata = dir.join("userdata");
        fs::create_dir_all(&userdata).unwrap();
        fs::write(
            dir.join("service.env"),
            "ANTHROPIC_API_KEY=CANARY_SECRET_1\nT3CODE_TELEMETRY_ENABLED=false\n",
        )
        .unwrap();
        fs::write(
            userdata.join("settings.json"),
            r#"{"secretField":"CANARY_SECRET_2","agentFeedback":{"mode":"ask"},"observability":{"otlpTracesUrl":"https://u:CANARY_SECRET_3@localhost:4318/v1/traces?tok=CANARY_SECRET_4"}}"#,
        )
        .unwrap();
        let anon = userdata.join("anonymous-id");
        fs::write(&anon, "CANARY_SECRET_5").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&anon, fs::Permissions::from_mode(0o000));
        }

        let hosts_path = dir.join("hosts");
        fs::write(&hosts_path, "0.0.0.0 ingest.t3.codes # CANARY_SECRET_6\n").unwrap();
        let nss_path = dir.join("nsswitch.conf");
        fs::write(&nss_path, "hosts: files dns\n").unwrap();

        let paths = gather::Paths {
            data_folder: dir.clone(),
            policy: dir.join("policy.json"),
            macos_managed_prefs: None,
            hosts: hosts_path,
            nsswitch: Some(nss_path),
            uwsm_dir: None,
            omarchy_hypr_lua: None,
            environment_d_dir: None,
            etc_environment: None,
            home_dir: None,
            container_markers: Vec::new(),
        };
        struct FakeSys;
        impl gather::System for FakeSys {
            fn alive(&self, _pid: u32) -> crate::platform::ProcessAlive {
                crate::platform::ProcessAlive::Stopped
            }
            fn stat(&self, _pid: u32) -> Option<crate::platform::ProcStat> {
                None
            }
            fn loupe_ancestors(&self) -> Vec<u32> {
                Vec::new()
            }
        }
        let var = |name: &str| -> Option<OsString> {
            match name {
                "OTEL_EXPORTER_OTLP_HEADERS" => Some("Authorization=CANARY_SECRET_7".into()),
                _ => None,
            }
        };
        let inputs = gather::gather(&paths, &var, &FakeSys, 1_795_532_700_000.0, None);
        assert!(inputs.install_id.is_some(), "mode 000 anonymous-id still statted");
        let report = evaluate(inputs);

        let debug_str = format!("{report:?}");
        let json_str = report.json().to_string();
        assert!(!debug_str.contains("CANARY"), "canary leaked into Debug: {debug_str}");
        assert!(!json_str.contains("CANARY"), "canary leaked into JSON: {json_str}");

        // Restore perms so temp dir cleans up cleanly
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&anon, fs::Permissions::from_mode(0o600));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn policy_strict_schema_rejects_unknown_keys() {
        let dir = std::env::temp_dir().join(format!(
            "loupe-tel-pol-{}-{}",
            std::process::id(),
            crate::clock::wall_now() as u64
        ));
        fs::create_dir_all(&dir).unwrap();
        let pol = dir.join("policy.json");
        fs::write(
            &pol,
            r#"{"schemaVersion":1,"organization":"Acme","unknownKey":123,"limits":{"telemetry":false}}"#,
        )
        .unwrap();
        let read = gather::read_policy(&pol);
        match read {
            Read::Invalid {
                reason,
                attempted_telemetry_off,
            } => {
                assert!(reason.contains("unknownKey"), "{reason}");
                assert!(attempted_telemetry_off);
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn demo_report_is_self_contained_and_valid() {
        let rep = demo::report(1_795_532_700_000.0);
        assert_eq!(rep.verdict, Verdict::Misconfigured);
        assert_eq!(rep.streams.len(), 3);
        assert_eq!(rep.streams[0].state, StreamState::Blocked);
        assert_eq!(rep.streams[1].state, StreamState::OptOutIgnored);
        assert_eq!(rep.streams[2].state, StreamState::On);
        assert!(rep.findings.iter().any(|f| f.id == "otel_sdk_disabled_not_true"));
        assert!(rep.findings.iter().any(|f| f.id == "auth_files_hashed"));
        assert_eq!(rep.json()["schema"], 1);
        assert_eq!(rep.json()["network"], "none");
    }

    #[test]
    fn hosts_parser_handles_comments_subdomains_and_redirects() {
        let dir = std::env::temp_dir().join(format!(
            "loupe-tel-hosts-{}-{}",
            std::process::id(),
            crate::clock::wall_now() as u64
        ));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hosts");
        fs::write(
            &p,
            "# 0.0.0.0 ingest.t3.codes\n10.0.0.5 INGEST.T3.CODES.\n:: t3.codes\n",
        )
        .unwrap();
        let Read::Ok { value, .. } = gather::read_hosts(&p, "ingest.t3.codes") else {
            panic!("expected Ok");
        };
        assert_eq!(value.matches.len(), 1);
        assert!(!value.matches[0].sinkhole);
        assert_eq!(value.matches[0].ip, "10.0.0.5");

        fs::write(&p, "0.0.0.0 t3.codes\n").unwrap();
        let Read::Ok { value, .. } = gather::read_hosts(&p, "ingest.t3.codes") else {
            panic!("expected Ok");
        };
        assert!(value.matches.is_empty());
        assert!(value.bare_parent_only);

        let _ = fs::remove_dir_all(Path::new(&dir));
    }

    #[test]
    fn review_regressions_policy_strict_bool_desktop_macos_exporters_and_service_env() {
        // 1. Invalid policy refuses startup (Unverified + Warn), NOT Misconfigured / alert
        let mut inp = base_inputs();
        inp.policy = Read::Invalid {
            reason: "unknown field `bogus`".into(),
            attempted_telemetry_off: true,
        };
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::Unverified);
        assert!(rep.alert().is_none(), "invalid policy should not raise opt-out-ignored alert");
        assert!(rep.summary.contains("will not start"));

        // 2. Product events require exact lowercase T3CODE_TELEMETRY_ENABLED (Effect Config.Boolean)
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::TelemetryEnabled, "False");
        let rep = evaluate(inp);
        assert_eq!(rep.streams[0].state, StreamState::Unverified);
        assert_eq!(rep.streams[1].state, StreamState::Off); // relay traces trim+lowercase
        assert!(rep.findings.iter().any(|f| f.id == "invalid_telemetry_enabled"));

        // 3. Desktop launch with shell opt-out -> row & verdict both Unverified with graceful wording
        let mut inp = base_inputs();
        inp.server.liveness = Liveness::Running;
        inp.server.launch = Launch::Desktop("uwsm".into());
        inp.shell.set_raw(EnvKey::TelemetryEnabled, "false");
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::Unverified);
        assert_eq!(rep.verdict_heading(), "OFF IN THIS SHELL");
        assert_eq!(rep.verdict_short(), "Off in shell");
        assert_eq!(rep.streams[0].state, StreamState::Unverified);
        assert_eq!(rep.streams[1].state, StreamState::Unverified);
        assert!(rep.findings.iter().any(|f| f.id == "shell_env_unverified"));

        // 4. t3_otel_overrides_spec alert does not say "T3CODE_OTEL_SDK_DISABLED=false ignored"
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::OtelSdkDisabled, "true");
        inp.shell.set_raw(EnvKey::T3OtelSdkDisabled, "false");
        let rep = evaluate(inp);
        let alert = rep.alert().expect("alert expected");
        assert!(alert.detail.contains("overrides"), "{}", alert.detail);
        assert!(!alert.detail.contains("ignored"), "{}", alert.detail);

        // 5. sdk_disabled still silences custom exporters when T3CODE_TELEMETRY_ENABLED=false
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::TelemetryEnabled, "false");
        inp.shell.set_raw(EnvKey::T3OtelSdkDisabled, "true");
        inp.settings = Read::Ok {
            value: Settings {
                exporters: vec![(
                    Signal::Traces,
                    Endpoint {
                        scheme: "http".into(),
                        host: "localhost".into(),
                        port: Some(4318),
                    },
                )],
                ..Settings::default()
            },
            stamp: Stamp {
                modified_ms: 1_795_000_000_000.0,
                len: 80,
            },
        };
        let rep = evaluate(inp);
        assert_eq!(rep.streams[2].state, StreamState::Off);
        assert!(rep.streams[2].exporters[0].silenced_by_sdk_disabled);
        assert_eq!(rep.json()["streams"][2]["endpoints"][0]["silenced"], true);

        // 6. Invalid per-signal endpoint or protocol turns signal off with NO fallback to settings
        let mut inp = base_inputs();
        inp.shell.set_raw(EnvKey::OtlpEndpoint(Some(Signal::Traces)), "not-a-url");
        inp.shell.set_raw(EnvKey::OtlpProtocol(Some(Signal::Metrics)), "grpc");
        inp.settings = Read::Ok {
            value: Settings {
                exporters: vec![
                    (
                        Signal::Traces,
                        Endpoint {
                            scheme: "http".into(),
                            host: "localhost".into(),
                            port: Some(4318),
                        },
                    ),
                    (
                        Signal::Metrics,
                        Endpoint {
                            scheme: "http".into(),
                            host: "localhost".into(),
                            port: Some(4318),
                        },
                    ),
                ],
                ..Settings::default()
            },
            stamp: Stamp {
                modified_ms: 1_795_000_000_000.0,
                len: 80,
            },
        };
        let rep = evaluate(inp);
        assert!(rep.streams[2].exporters.is_empty());

        // 7. macOS platform JSON literal + MDM plist + dual policy conflict
        let mut inp = base_inputs();
        inp.platform = Platform::MacOs;
        inp.macos_managed_prefs_present = true;
        let rep = evaluate(inp.clone());
        assert_eq!(rep.json()["platform"], "macos");
        assert_eq!(rep.verdict, Verdict::Unverified);
        assert!(rep.findings.iter().any(|f| f.id == "macos_managed_profile"));

        inp.policy = Read::Ok {
            value: Policy {
                organization: "Acme".into(),
                telemetry: Some(false),
            },
            stamp: Stamp {
                modified_ms: 1_795_000_000_000.0,
                len: 64,
            },
        };
        let rep = evaluate(inp);
        assert_eq!(rep.verdict, Verdict::Unverified);
        assert!(rep.findings.iter().any(|f| f.id == "macos_dual_policy_conflict"));

        // 8. service.env quoted value with inline `# comment` parses cleanly
        let dir = std::env::temp_dir().join(format!(
            "loupe-tel-svcenv-{}-{}",
            std::process::id(),
            crate::clock::wall_now() as u64
        ));
        fs::create_dir_all(&dir).unwrap();
        let svc = dir.join("service.env");
        fs::write(
            &svc,
            "T3CODE_TELEMETRY_ENABLED=\"false\" # disable telemetry\nOTHER_SECRET=\"line1\nline2\"\nOTEL_SDK_DISABLED='true' # off\n",
        )
        .unwrap();
        let paths = gather::Paths {
            data_folder: dir.clone(),
            policy: dir.join("policy.json"),
            macos_managed_prefs: None,
            hosts: dir.join("hosts"),
            nsswitch: None,
            uwsm_dir: None,
            omarchy_hypr_lua: None,
            environment_d_dir: None,
            etc_environment: None,
            home_dir: None,
            container_markers: Vec::new(),
        };
        struct FakeSys;
        impl gather::System for FakeSys {
            fn alive(&self, _pid: u32) -> crate::platform::ProcessAlive {
                crate::platform::ProcessAlive::Stopped
            }
            fn stat(&self, _pid: u32) -> Option<crate::platform::ProcStat> {
                None
            }
            fn loupe_ancestors(&self) -> Vec<u32> {
                Vec::new()
            }
        }
        let gathered = gather::gather(&paths, &|_| None, &FakeSys, 1_795_532_700_000.0, None);
        let Read::Ok { value: svc_env, .. } = gathered.service_env else {
            panic!("expected Ok service_env");
        };
        assert!(matches!(
            svc_env.get(EnvKey::TelemetryEnabled),
            Some(EnvValue::TelemetryBool {
                product_parsed: Bool3::False,
                ..
            })
        ));
        assert!(matches!(
            svc_env.get(EnvKey::OtelSdkDisabled),
            Some(EnvValue::Bool {
                parsed: Bool3::True,
                ..
            })
        ));
        let _ = fs::remove_dir_all(&dir);

        // 9. T3 handoff prompt is self-contained, wraps data in JSON, and contains zero secrets
        let demo_rep = demo::report(1_795_532_700_000.0);
        let (title, prompt) = t3_handoff_prompt(&demo_rep);
        assert_eq!(title, "Turn off T3 Code telemetry");
        assert!(prompt.contains("OTEL_SDK_DISABLED"));
        assert!(prompt.contains("loupe --telemetry-check"));
        assert!(prompt.contains("~/.config/uwsm/env"));
        assert!(prompt.contains("Untrusted observational data (JSON)"));
        assert!(!prompt.contains("```sh"));
    }

    #[test]
    fn shell_quote_path_prevents_command_injection() {
        assert_eq!(shell_quote_path("~/.t3/service.env"), "~/.t3/service.env");
        assert_eq!(
            shell_quote_path("/tmp/t3 data/service.env"),
            "'/tmp/t3 data/service.env'"
        );
        assert_eq!(
            shell_quote_path("~/.t3; rm -rf /; #/service.env"),
            "~/'/.t3; rm -rf /; #/service.env'".replace("~/'/", "~/'")
        );
        assert_eq!(
            shell_quote_path("/tmp/a'b$(id)`uname`\n/service.env"),
            "'/tmp/a'\\''b$(id)`uname`\n/service.env'"
        );
    }

    #[test]
    fn parse_t3_thread_id_requires_valid_json_id() {
        assert_eq!(
            parse_t3_thread_id(r#"{"threadId":"mcp:7832a1ed-7053-47c3-b183-a3ef46cccc85"}"#),
            Some("mcp:7832a1ed-7053-47c3-b183-a3ef46cccc85".into())
        );
        assert_eq!(parse_t3_thread_id(r#"{"id":"thread_123"}"#), Some("thread_123".into()));
        // Plain text, malformed JSON, leading dash, spaces, controls, and non-ASCII are rejected.
        assert_eq!(parse_t3_thread_id("plain-text-line\n"), None);
        assert_eq!(parse_t3_thread_id("not json"), None);
        assert_eq!(parse_t3_thread_id(r#"{"threadId":"--flag"}"#), None);
        assert_eq!(parse_t3_thread_id(r#"{"threadId":"id with space"}"#), None);
        assert_eq!(parse_t3_thread_id(r#"{"threadId":"🚀emoji-id"}"#), None);
    }

    #[cfg(unix)]
    #[test]
    fn gather_refuses_symlinks_on_user_configs_and_does_not_block_on_fifo() {
        use std::os::unix::fs::symlink;
        let dir = std::env::temp_dir().join(format!(
            "loupe-tel-sec-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let userdata = dir.join("userdata");
        fs::create_dir_all(&userdata).unwrap();

        let secret = dir.join("secret.json");
        fs::write(&secret, r#"{"shareUsageData": true}"#).unwrap();
        symlink(&secret, userdata.join("settings.json")).unwrap();
        symlink(&secret, dir.join("service.env")).unwrap();

        // Create a FIFO for server-runtime.json — opening without O_NONBLOCK would deadlock forever.
        let fifo = userdata.join("server-runtime.json");
        let c_fifo = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        let rc = unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) };
        assert_eq!(rc, 0, "mkfifo failed");

        let paths = gather::Paths {
            data_folder: dir.clone(),
            policy: dir.join("no-policy.json"),
            macos_managed_prefs: None,
            hosts: dir.join("no-hosts"),
            nsswitch: None,
            uwsm_dir: None,
            omarchy_hypr_lua: None,
            environment_d_dir: None,
            etc_environment: None,
            home_dir: None,
            container_markers: Vec::new(),
        };
        let gathered = gather::gather(
            &paths,
            &|_| None,
            &gather::HostSystem,
            1_795_532_700_000.0,
            None,
        );
        assert!(matches!(
            gathered.settings,
            Read::Unreadable(ref msg) if msg == "symlink not followed"
        ));
        assert!(matches!(
            gathered.service_env,
            Read::Unreadable(ref msg) if msg == "symlink not followed"
        ));
        assert!(matches!(
            gathered.runtime,
            Read::Unreadable(ref msg) if msg == "not a regular file"
        ));
        let _ = fs::remove_dir_all(&dir);

        // Also test symlinked intermediate `userdata/` directory (parent-component symlink bypass).
        let dir2 = std::env::temp_dir().join(format!(
            "loupe-tel-parent-sym-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir2);
        let outside = dir2.join("outside");
        let root2 = dir2.join("root");
        fs::create_dir_all(&outside).unwrap();
        fs::create_dir_all(&root2).unwrap();
        fs::write(outside.join("settings.json"), r#"{"shareUsageData": true}"#).unwrap();
        symlink(&outside, root2.join("userdata")).unwrap();
        let paths2 = gather::Paths {
            data_folder: root2,
            policy: dir2.join("no-policy.json"),
            macos_managed_prefs: None,
            hosts: dir2.join("no-hosts"),
            nsswitch: None,
            uwsm_dir: None,
            omarchy_hypr_lua: None,
            environment_d_dir: None,
            etc_environment: None,
            home_dir: None,
            container_markers: Vec::new(),
        };
        let g2 = gather::gather(
            &paths2,
            &|_| None,
            &gather::HostSystem,
            1_795_532_700_000.0,
            None,
        );
        assert!(matches!(
            g2.settings,
            Read::Unreadable(ref msg) if msg == "symlink not followed"
        ));
        assert!(gather::is_trusted_system_target(Path::new("/etc/t3code/policy.json")));
        assert!(gather::is_trusted_system_target(Path::new("/nix/store/abc-etc/hosts")));
        assert!(!gather::is_trusted_system_target(Path::new("/home/alice/.t3/userdata/secrets/key")));
        assert!(!gather::is_trusted_system_target(Path::new("/tmp/evil")));
        let _ = fs::remove_dir_all(&dir2);
    }

    #[test]
    fn omarchy_desktop_shell_opt_out_recommends_hyprland_lua_and_optional_policy() {
        let mut inp = base_inputs();
        inp.platform = Platform::Linux;
        inp.omarchy_hypr_detected = true;
        inp.server.liveness = Liveness::Running;
        inp.server.launch = Launch::Desktop("Hyprland".into());
        inp.shell.set_raw(EnvKey::TelemetryEnabled, "false");
        inp.shell_rc_source = Some(ShellRcSource {
            path: "~/.bashrc".into(),
            interactive_only: true,
        });
        let rep = evaluate(inp);

        assert_eq!(rep.verdict, Verdict::Unverified);
        assert_eq!(rep.streams[0].state, StreamState::Unverified);
        assert!(
            rep.streams[0].ladder[0].value.contains("optional"),
            "policy step should note it is optional: {:?}",
            rep.streams[0].ladder[0]
        );
        assert!(
            rep.streams[0]
                .ladder
                .iter()
                .any(|s| s.step == "desktop session env"),
            "desktop session env step expected: {:?}",
            rep.streams[0].ladder
        );
        assert!(
            rep.streams[0]
                .ladder
                .iter()
                .any(|s| s.value.contains("~/.bashrc only")),
            "ladder should cite ~/.bashrc: {:?}",
            rep.streams[0].ladder
        );
        assert!(
            rep.findings
                .iter()
                .any(|f| f.id == "shell_env_unverified" && f.severity == Severity::Warn),
            "interactive ~/.bashrc-only opt-out on Linux desktop should warn: {:?}",
            rep.findings
        );
        assert_eq!(rep.fix.len(), 2);
        assert_eq!(rep.fix[0].for_kind, "desktop_omarchy");
        assert!(rep.fix[0].command.contains("~/.config/hypr/hyprland.lua"));
        assert!(rep.fix[0].command.contains("hl.env"));
        assert_eq!(rep.fix[1].for_kind, "managed_policy");
        assert!(rep.fix[1].command.contains("install -d -m 755 /etc/t3code"));
        assert!(rep.fix[1].command.contains("/etc/t3code/policy.json"));
    }

    #[test]
    fn desktop_launcher_env_files_verify_off_detect_restart_and_never_leak_secrets() {
        let dir = std::env::temp_dir().join(format!(
            "loupe-tel-desktop-env-{}-{}",
            std::process::id(),
            crate::clock::wall_now() as u64
        ));
        let _ = fs::remove_dir_all(&dir);
        let userdata = dir.join("userdata");
        let hypr = dir.join("config/hypr");
        let envd = dir.join("config/environment.d");
        fs::create_dir_all(&userdata).unwrap();
        fs::create_dir_all(&hypr).unwrap();
        fs::create_dir_all(&envd).unwrap();

        // Put an unrelated secret file in environment.d and a secret in hyprland.lua & .bashrc
        fs::write(envd.join("60-cloudflare.conf"), "CF_API_TOKEN=CANARY_CF_SECRET\n").unwrap();
        fs::write(
            hypr.join("hyprland.lua"),
            "-- user hyprland config\nhl.env(\"SECRET_KEY\", \"CANARY_HYPR_SECRET\")\nhl.env(\"T3CODE_TELEMETRY_ENABLED\", \"false\")\n",
        )
        .unwrap();
        fs::write(
            dir.join(".bashrc"),
            "export OPENAI_API_KEY=CANARY_BASHRC_SECRET\nexport T3CODE_TELEMETRY_ENABLED=false\n",
        )
        .unwrap();

        let paths = gather::Paths {
            data_folder: dir.clone(),
            policy: dir.join("no-policy.json"),
            macos_managed_prefs: None,
            hosts: dir.join("no-hosts"),
            nsswitch: None,
            uwsm_dir: None,
            omarchy_hypr_lua: Some(hypr.join("hyprland.lua")),
            environment_d_dir: Some(envd),
            etc_environment: None,
            home_dir: Some(dir.clone()),
            container_markers: Vec::new(),
        };
        struct FakeSys;
        impl gather::System for FakeSys {
            fn alive(&self, _pid: u32) -> crate::platform::ProcessAlive {
                crate::platform::ProcessAlive::Stopped
            }
            fn stat(&self, _pid: u32) -> Option<crate::platform::ProcStat> {
                None
            }
            fn loupe_ancestors(&self) -> Vec<u32> {
                Vec::new()
            }
        }
        let mut inputs = gather::gather(
            &paths,
            &|name| (name == "T3CODE_TELEMETRY_ENABLED").then(|| OsString::from("false")),
            &FakeSys,
            1_795_532_700_000.0,
            Some(dir.to_string_lossy().into_owned()),
        );
        assert!(inputs.omarchy_hypr_detected);
        let de = inputs.desktop_env.as_ref().expect("hyprland.lua detected");
        assert!(de.source_label.ends_with("hyprland.lua"), "{}", de.source_label);
        let hypr_mtime = de.stamp.modified_ms;

        // 1. Running desktop server started AFTER hyprland.lua was written -> Verdict::Off (Likely)
        inputs.server.liveness = Liveness::Running;
        inputs.server.launch = Launch::Desktop("Hyprland".into());
        inputs.runtime = Read::Ok {
            value: Runtime {
                version: 1,
                pid: 4242,
                started_at: hypr_mtime + 10_000.0,
                started_at_raw: format_iso_utc(hypr_mtime + 10_000.0),
                service_managed: Some(false),
                server_version: Some("0.1.13".into()),
                managed_policy: Some(None),
            },
            stamp: Stamp {
                modified_ms: hypr_mtime + 10_000.0,
                len: 120,
            },
        };
        let rep_off = evaluate(inputs.clone());
        assert_eq!(rep_off.verdict, Verdict::Off);
        assert_eq!(rep_off.streams[0].state, StreamState::Off);
        assert_eq!(rep_off.streams[0].confidence, Confidence::Likely);
        assert_eq!(rep_off.streams[1].state, StreamState::Off);
        let json_str = rep_off.json().to_string();
        let dbg_str = format!("{rep_off:?}");
        assert!(!json_str.contains("CANARY"), "secret leaked in json: {json_str}");
        assert!(!dbg_str.contains("CANARY"), "secret leaked in debug: {dbg_str}");
        assert!(
            !json_str.contains("60-cloudflare") && !dbg_str.contains("60-cloudflare"),
            "unrelated environment.d filename leaked: {json_str}"
        );

        // 2. Running desktop server started BEFORE hyprland.lua was edited -> RestartNeeded
        if let Read::Ok { value, .. } = &mut inputs.runtime {
            value.started_at = hypr_mtime - 60_000.0;
        }
        let rep_restart = evaluate(inputs);
        assert_eq!(rep_restart.verdict, Verdict::RestartNeeded);
        assert_eq!(rep_restart.streams[0].state, StreamState::RestartNeeded);

        let _ = fs::remove_dir_all(&dir);
    }
}
