//! Pure, zero-I/O synthetic telemetry fixture for `--demo`.
//!
//! Showcases six real-world findings in one self-contained check:
//! - Linux desktop launch (Omarchy / uwsm), T3 0.1.11 running
//! - `/etc/hosts` sinkholing `ingest.t3.codes -> 0.0.0.0` (blocked, with delivery-only caveat)
//! - `OTEL_SDK_DISABLED=1` (opt-out ignored per OTEL spec -> `OPT-OUT NOT WORKING`)
//! - Pre-PR-#42299 version warning (`0.1.11` hashes `~/.codex/auth.json` & `~/.claude.json`)
//! - User-configured local OTLP traces receiver (`localhost:4318`)
//! - `agentFeedback: ask`, `install_id` present since Oct 3

use super::{
    DEFAULT_POSTHOG_HOST, Endpoint, EnvKey, EnvSet, HostsMatch, HostsScan, Inputs, Launch,
    Liveness, Platform, Read, ReadOutcome, ReadRecord, Report, Runtime, Server, Settings, Signal,
    Source, Stamp, evaluate, format_iso_utc,
};
use std::path::PathBuf;

pub fn inputs(now: f64) -> Inputs {
    let started_at = now - 64.0 * 60_000.0;
    let hosts_mtime = now - 3.0 * 86_400_000.0;
    let settings_mtime = now - 5.0 * 86_400_000.0;
    // Oct 3, 2026 12:00:00 UTC
    let oct_3_ms = 1_791_028_800_000.0;

    let mut shell = EnvSet::default();
    shell.set_raw(EnvKey::OtelSdkDisabled, "1");

    let reads = vec![
        ReadRecord {
            name: "hosts".into(),
            path: "/etc/hosts".into(),
            outcome: ReadOutcome::Read,
            modified_ms: Some(hosts_mtime),
            detail: "1 matching entry".into(),
        },
        ReadRecord {
            name: "policy".into(),
            path: "/etc/t3code/policy.json".into(),
            outcome: ReadOutcome::Absent,
            modified_ms: None,
            detail: "absent".into(),
        },
        ReadRecord {
            name: "runtime".into(),
            path: "~/.t3/userdata/server-runtime.json".into(),
            outcome: ReadOutcome::Read,
            modified_ms: Some(started_at),
            detail: "T3 0.1.11 · running".into(),
        },
        ReadRecord {
            name: "settings".into(),
            path: "~/.t3/userdata/settings.json".into(),
            outcome: ReadOutcome::Read,
            modified_ms: Some(settings_mtime),
            detail: "2 fields used".into(),
        },
        ReadRecord {
            name: "service_env".into(),
            path: "~/.t3/service.env".into(),
            outcome: ReadOutcome::Absent,
            modified_ms: None,
            detail: "absent".into(),
        },
        ReadRecord {
            name: "install_id".into(),
            path: "~/.t3/userdata/anonymous-id".into(),
            outcome: ReadOutcome::StatOnly,
            modified_ms: Some(oct_3_ms),
            detail: "stat only".into(),
        },
        ReadRecord {
            name: "nsswitch".into(),
            path: "/etc/nsswitch.conf".into(),
            outcome: ReadOutcome::Read,
            modified_ms: Some(hosts_mtime),
            detail: "hosts uses files".into(),
        },
        ReadRecord {
            name: "environment".into(),
            path: "this shell · T3's process tree".into(),
            outcome: ReadOutcome::Read,
            modified_ms: None,
            detail: "allowlisted only".into(),
        },
        ReadRecord {
            name: "never_opened".into(),
            path: "secrets/, agent logins, environ".into(),
            outcome: ReadOutcome::Never,
            modified_ms: None,
            detail: "excluded by design".into(),
        },
    ];

    Inputs {
        source: Source::Demo,
        platform: Platform::Linux,
        checked_at: now,
        data_folder_exists: true,
        uwsm_detected: true,
        omarchy_hypr_detected: false,
        in_container: false,
        shell,
        service_env: Read::Absent,
        policy: Read::Absent,
        macos_managed_prefs_present: false,
        runtime: Read::Ok {
            value: Runtime {
                version: 1,
                pid: 4242,
                started_at,
                started_at_raw: format_iso_utc(started_at),
                service_managed: Some(false),
                server_version: Some("0.1.11".into()),
                managed_policy: Some(None),
            },
            stamp: Stamp {
                modified_ms: started_at,
                len: 214,
            },
        },
        server: Server {
            liveness: Liveness::Running,
            launch: Launch::Desktop("uwsm".into()),
        },
        settings: Read::Ok {
            value: Settings {
                agent_feedback: Some("ask".into()),
                exporters: vec![(
                    Signal::Traces,
                    Endpoint {
                        scheme: "http".into(),
                        host: "localhost".into(),
                        port: Some(4318),
                    },
                )],
                provider_update_checks: None,
                share_usage_data: None,
            },
            stamp: Stamp {
                modified_ms: settings_mtime,
                len: 842,
            },
        },
        hosts: Read::Ok {
            value: HostsScan {
                target_host: DEFAULT_POSTHOG_HOST.into(),
                matches: vec![HostsMatch {
                    ip: "0.0.0.0".into(),
                    host: DEFAULT_POSTHOG_HOST.into(),
                    sinkhole: true,
                }],
                bare_parent_only: false,
            },
            stamp: Stamp {
                modified_ms: hosts_mtime,
                len: 312,
            },
        },
        nsswitch: Read::Ok {
            value: true,
            stamp: Stamp {
                modified_ms: hosts_mtime,
                len: 164,
            },
        },
        install_id: Some(Stamp {
            modified_ms: oct_3_ms,
            len: 37,
        }),
        data_folder: PathBuf::from("~/.t3"),
        home_display: None,
        reads,
    }
}

pub fn report(now: f64) -> Report {
    evaluate(inputs(now))
}
