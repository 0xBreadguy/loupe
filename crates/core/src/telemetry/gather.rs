//! Bounded, zero-secret local file gathering and cheap stat-based fingerprinting.
//!
//! This is the ONLY module in `telemetry` that touches the filesystem or process table.
//! Every path and system call is injected via [`Paths`], `var`, and [`System`] so tests
//! never open `~/.t3`, `/etc/t3code/policy.json`, or `/etc/hosts` on the host.
//!
//! Never opened: `userdata/secrets/`, `~/.codex/auth.json`, `~/.claude.json`,
//! `/proc/<pid>/environ`, `/proc/<pid>/cmdline`, `OTEL_EXPORTER_OTLP_HEADERS`, `statev2.sqlite`.

use super::{
    DEFAULT_POSTHOG_HOST, EnvKey, EnvSet, EnvValue, HostsMatch, HostsScan, Inputs, Launch,
    Liveness, LoadedPolicy, Platform, Policy, Read, ReadOutcome, ReadRecord, Runtime, Server,
    Settings, Signal, Source, Stamp, is_sinkhole_ip, parse_http_endpoint, parse_timestamp_ms,
};
use crate::platform::{ProcStat, ProcessAlive};
use crate::product::{self, Environment, PLATFORM_HOME_RULES};
use serde::Deserialize;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const POLICY_CAP: u64 = 256 * 1024;
const RUNTIME_CAP: u64 = 64 * 1024;
const SETTINGS_CAP: u64 = 4 * 1024 * 1024;
const SERVICE_ENV_CAP: u64 = 64 * 1024;
const HOSTS_CAP: u64 = 64 * 1024 * 1024;
const NSSWITCH_CAP: u64 = 64 * 1024;
const PID_REUSE_TOLERANCE_MS: f64 = 30_000.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    pub data_folder: PathBuf,
    pub policy: PathBuf,
    /// macOS only: `/Library/Managed Preferences/com.t3tools.t3code.plist` (stat-only, never opened).
    pub macos_managed_prefs: Option<PathBuf>,
    pub hosts: PathBuf,
    pub nsswitch: Option<PathBuf>,
    pub uwsm_dir: Option<PathBuf>,
    pub omarchy_hypr_lua: Option<PathBuf>,
    pub container_markers: Vec<PathBuf>,
}

impl Paths {
    /// The ONLY place host default paths are assembled.
    pub fn current(env: &Environment) -> Self {
        let cwd = std::env::current_dir().ok();
        let data_folder = product::data_folder(
            env.t3code_home.as_deref(),
            env.home.as_deref(),
            cwd.as_deref(),
            PLATFORM_HOME_RULES,
        );
        let policy = if cfg!(target_vendor = "apple") {
            PathBuf::from("/Library/Application Support/T3Code/policy.json")
        } else if cfg!(windows) {
            PathBuf::from(r"C:\Program Files\T3Code\policy.json")
        } else {
            PathBuf::from("/etc/t3code/policy.json")
        };
        let macos_managed_prefs = cfg!(target_vendor = "apple")
            .then(|| PathBuf::from("/Library/Managed Preferences/com.t3tools.t3code.plist"));
        let hosts = if cfg!(windows) {
            PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
        } else {
            PathBuf::from("/etc/hosts")
        };
        let nsswitch = cfg!(target_os = "linux").then(|| PathBuf::from("/etc/nsswitch.conf"));
        let config_dir = if cfg!(target_os = "linux") {
            std::env::var_os("XDG_CONFIG_HOME")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .or_else(|| env.home.as_ref().map(|h| PathBuf::from(h).join(".config")))
        } else {
            None
        };
        let uwsm_dir = config_dir.as_ref().map(|c| c.join("uwsm"));
        let omarchy_hypr_lua = config_dir.map(|c| c.join("hypr").join("hyprland.lua"));
        let container_markers = if cfg!(target_os = "linux") {
            vec![
                PathBuf::from("/.flatpak-info"),
                PathBuf::from("/run/.containerenv"),
            ]
        } else {
            Vec::new()
        };
        Self {
            data_folder,
            policy,
            macos_managed_prefs,
            hosts,
            nsswitch,
            uwsm_dir,
            omarchy_hypr_lua,
            container_markers,
        }
    }

    pub fn userdata(&self) -> PathBuf {
        self.data_folder.join("userdata")
    }

    pub fn settings(&self) -> PathBuf {
        self.userdata().join("settings.json")
    }

    pub fn runtime(&self) -> PathBuf {
        self.userdata().join("server-runtime.json")
    }

    pub fn service_env(&self) -> PathBuf {
        self.data_folder.join("service.env")
    }

    pub fn anonymous_id(&self) -> PathBuf {
        self.userdata().join("anonymous-id")
    }
}

pub trait System {
    fn alive(&self, pid: u32) -> ProcessAlive;
    fn stat(&self, pid: u32) -> Option<ProcStat>;
    fn loupe_ancestors(&self) -> Vec<u32>;
}

pub struct HostSystem;

impl System for HostSystem {
    fn alive(&self, pid: u32) -> ProcessAlive {
        crate::platform::process_alive(pid)
    }

    fn stat(&self, pid: u32) -> Option<ProcStat> {
        crate::platform::process_stat(pid)
    }

    fn loupe_ancestors(&self) -> Vec<u32> {
        crate::platform::loupe_ancestors()
    }
}

/// Cheap metadata fingerprint for polling without reopening files (especially large `/etc/hosts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    files: Vec<Option<(u64, u64, i128, u64)>>,
    pid_alive: Option<(u32, bool)>,
}

pub fn fingerprint(paths: &Paths, runtime_pid: Option<u32>, sys: &dyn System) -> Fingerprint {
    // Follow symlinks only on OS-managed system files (with canonical target verification);
    // walk user-controlled T3 files component-by-component with O_NOFOLLOW / AT_SYMLINK_NOFOLLOW.
    let mut files = vec![
        system_file_sig(&paths.policy),
        sig_under_root_nofollow(&paths.data_folder, &["userdata", "server-runtime.json"]),
        sig_under_root_nofollow(&paths.data_folder, &["userdata", "settings.json"]),
        sig_under_root_nofollow(&paths.data_folder, &["service.env"]),
        sig_under_root_nofollow(&paths.data_folder, &["userdata", "anonymous-id"]),
        system_file_sig(&paths.hosts),
    ];
    if let Some(nss) = &paths.nsswitch {
        files.push(system_file_sig(nss));
    }
    if let Some(prefs) = &paths.macos_managed_prefs {
        files.push(file_sig(prefs, false));
    }
    let pid_alive = runtime_pid.map(|pid| (pid, sys.alive(pid) == ProcessAlive::Alive));
    Fingerprint { files, pid_alive }
}

fn system_file_sig(path: &Path) -> Option<(u64, u64, i128, u64)> {
    #[cfg(unix)]
    if is_os_config_path(path) {
        let canon = fs::canonicalize(path).ok()?;
        if !is_trusted_system_target(&canon) {
            return None;
        }
        return file_sig(&canon, false);
    }
    file_sig(path, true)
}

fn file_sig(path: &Path, follow_symlinks: bool) -> Option<(u64, u64, i128, u64)> {
    let meta = if follow_symlinks {
        fs::metadata(path).ok()?
    } else {
        fs::symlink_metadata(path).ok()?
    };
    sig_from_meta(&meta)
}

fn sig_from_meta(meta: &fs::Metadata) -> Option<(u64, u64, i128, u64)> {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    #[cfg(unix)]
    let (dev, ino) = {
        use std::os::unix::fs::MetadataExt;
        (meta.dev(), meta.ino())
    };
    #[cfg(not(unix))]
    let (dev, ino) = (0u64, 0u64);
    Some((dev, ino, mtime, meta.len()))
}

fn stamp_from_meta(meta: &fs::Metadata) -> Stamp {
    let modified_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    Stamp {
        modified_ms,
        len: meta.len(),
    }
}

fn shorten(path: &Path, home: Option<&str>) -> String {
    let raw = path.to_string_lossy();
    if let Some(h) = home.filter(|h| h.len() > 1) {
        let prefix = format!("{h}/");
        if let Some(rest) = raw.strip_prefix(&prefix) {
            return format!("~/{rest}");
        }
        if raw == h {
            return "~".into();
        }
    }
    raw.into_owned()
}

pub fn current_platform() -> Platform {
    if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_vendor = "apple") {
        Platform::MacOs
    } else {
        Platform::Other
    }
}

pub fn gather(
    paths: &Paths,
    var: &dyn Fn(&str) -> Option<OsString>,
    sys: &dyn System,
    now: f64,
    home_display: Option<String>,
) -> Inputs {
    let home_ref = home_display.as_deref();
    let mut reads = Vec::new();

    // 1. Allowlisted env vars from Loupe's own environment (never iterates `std::env::vars()`).
    let mut shell = EnvSet::default();
    for key in EnvKey::ALLOWLIST {
        if let Some(v) = var(key.name()) {
            if let Some(s) = v.to_str() {
                shell.set_raw(key, s);
            }
        }
    }

    // 2. Managed policy (+ macOS Managed Preferences stat-only check)
    let policy = read_policy(&paths.policy);
    reads.push(record_for(
        "policy",
        &paths.policy,
        home_ref,
        &policy,
        |p| match p.telemetry {
            Some(false) => format!("telemetry: false ({})", p.organization),
            Some(true) => format!("telemetry: true ({})", p.organization),
            None => format!("telemetry unset ({})", p.organization),
        },
    ));
    let macos_managed_prefs_present = if let Some(prefs) = &paths.macos_managed_prefs {
        if let Ok(meta) = fs::symlink_metadata(prefs) {
            reads.push(ReadRecord {
                name: "macos_managed_prefs".into(),
                path: shorten(prefs, home_ref),
                outcome: ReadOutcome::StatOnly,
                modified_ms: Some(stamp_from_meta(&meta).modified_ms),
                detail: "plist present, not opened".into(),
            });
            true
        } else {
            false
        }
    } else {
        false
    };

    // 3. Server runtime note (component-by-component O_NOFOLLOW from data_folder)
    let runtime_path = paths.runtime();
    let runtime = read_runtime(&paths.data_folder);
    let server = classify_server(&runtime, sys);
    reads.push(record_for("runtime", &runtime_path, home_ref, &runtime, |r| {
        let ver = r.server_version.as_deref().unwrap_or("unknown");
        let st = match server.liveness {
            Liveness::Running => "running",
            Liveness::Stopped => "stopped",
            Liveness::PidReused => "pid reused",
            Liveness::Unknown => "unknown",
        };
        format!("T3 {ver} · {st}")
    }));

    // 4. Settings (component-by-component O_NOFOLLOW from data_folder)
    let settings_path = paths.settings();
    let settings = read_settings(&paths.data_folder);
    reads.push(record_for("settings", &settings_path, home_ref, &settings, |s| {
        let mut n = 0usize;
        if s.agent_feedback.is_some() {
            n += 1;
        }
        if s.provider_update_checks.is_some() {
            n += 1;
        }
        if s.share_usage_data.is_some() {
            n += 1;
        }
        n += s.exporters.len();
        if n == 0 {
            "defaults only".into()
        } else if n == 1 {
            "1 field used".into()
        } else {
            format!("{n} fields used")
        }
    }));

    // 5. service.env (allowlisted keys only, O_NOFOLLOW from data_folder)
    let service_env_path = paths.service_env();
    let service_env = read_service_env(&paths.data_folder);
    reads.push(record_for(
        "service_env",
        &service_env_path,
        home_ref,
        &service_env,
        |e| {
            if e.entries.is_empty() {
                "no telemetry keys".into()
            } else if e.entries.len() == 1 {
                "1 allowlisted key".into()
            } else {
                format!("{} allowlisted keys", e.entries.len())
            }
        },
    ));

    // 6. anonymous-id (component-by-component O_NOFOLLOW stat ONLY — never opened!)
    let anon_path = paths.anonymous_id();
    let install_id = stamp_under_root_nofollow(&paths.data_folder, &["userdata", "anonymous-id"]);
    reads.push(ReadRecord {
        name: "install_id".into(),
        path: shorten(&anon_path, home_ref),
        outcome: if install_id.is_some() {
            ReadOutcome::StatOnly
        } else {
            ReadOutcome::Absent
        },
        modified_ms: install_id.as_ref().map(|s| s.modified_ms),
        detail: if install_id.is_some() {
            "stat only".into()
        } else {
            "absent".into()
        },
    });

    // 7. Determine effective PostHog host before scanning /etc/hosts
    let effective_posthog_host = {
        let svc_host = match &service_env {
            Read::Ok { value, .. }
                if server.liveness == Liveness::Running
                    && matches!(&runtime, Read::Ok { value: rt, .. } if rt.service_managed == Some(true)) =>
            {
                value.get(EnvKey::PosthogHost)
            }
            _ => None,
        };
        match svc_host.or_else(|| shell.get(EnvKey::PosthogHost)) {
            Some(EnvValue::Endpoint(ep)) => ep.host.clone(),
            _ => DEFAULT_POSTHOG_HOST.to_owned(),
        }
    };

    let hosts = read_hosts(&paths.hosts, &effective_posthog_host);
    reads.push(record_for("hosts", &paths.hosts, home_ref, &hosts, |h| {
        if !h.matches.is_empty() {
            format!("{} matching {}", h.matches.len(), if h.matches.len() == 1 { "entry" } else { "entries" })
        } else if h.bare_parent_only {
            "t3.codes only".into()
        } else {
            "no matching entry".into()
        }
    }));

    // 8. nsswitch.conf (Linux)
    let nsswitch = match &paths.nsswitch {
        Some(p) => {
            let r = read_nsswitch(p);
            reads.push(record_for("nsswitch", p, home_ref, &r, |uses_files| {
                if *uses_files {
                    "hosts uses files".into()
                } else {
                    "hosts omits files".into()
                }
            }));
            r
        }
        None => Read::Absent,
    };

    // 9. Shell & process tree summary + explicit "never opened" audit rows
    reads.push(ReadRecord {
        name: "environment".into(),
        path: "this shell · T3's process tree".into(),
        outcome: ReadOutcome::Read,
        modified_ms: None,
        detail: "allowlisted only".into(),
    });
    reads.push(ReadRecord {
        name: "never_opened".into(),
        path: "secrets/, agent logins, environ".into(),
        outcome: ReadOutcome::Never,
        modified_ms: None,
        detail: "excluded by design".into(),
    });

    let data_folder_exists = fs::symlink_metadata(paths.userdata()).is_ok();
    let uwsm_detected = paths
        .uwsm_dir
        .as_deref()
        .is_some_and(|p| fs::symlink_metadata(p).is_ok());
    let omarchy_hypr_detected = paths
        .omarchy_hypr_lua
        .as_deref()
        .is_some_and(|p| fs::symlink_metadata(p).is_ok());
    let in_container = paths
        .container_markers
        .iter()
        .any(|p| fs::symlink_metadata(p).is_ok());

    Inputs {
        source: Source::Machine,
        platform: current_platform(),
        checked_at: now,
        data_folder_exists,
        uwsm_detected,
        omarchy_hypr_detected,
        in_container,
        shell,
        service_env,
        policy,
        macos_managed_prefs_present,
        runtime,
        server,
        settings,
        hosts,
        nsswitch,
        install_id,
        data_folder: paths.data_folder.clone(),
        home_display,
        reads,
    }
}

fn record_for<T>(
    name: &str,
    path: &Path,
    home: Option<&str>,
    read: &Read<T>,
    describe: impl FnOnce(&T) -> String,
) -> ReadRecord {
    let short = shorten(path, home);
    match read {
        Read::Absent => ReadRecord {
            name: name.into(),
            path: short,
            outcome: ReadOutcome::Absent,
            modified_ms: None,
            detail: "absent".into(),
        },
        Read::Unreadable(kind) => ReadRecord {
            name: name.into(),
            path: short,
            outcome: ReadOutcome::Unreadable,
            modified_ms: None,
            detail: format!("unreadable ({kind})"),
        },
        Read::Invalid { reason, .. } => ReadRecord {
            name: name.into(),
            path: short,
            outcome: ReadOutcome::Invalid,
            modified_ms: None,
            detail: reason.clone(),
        },
        Read::Ok { value, stamp } => ReadRecord {
            name: name.into(),
            path: short,
            outcome: ReadOutcome::Read,
            modified_ms: Some(stamp.modified_ms),
            detail: describe(value),
        },
    }
}

pub(crate) fn is_os_config_path(path: &Path) -> bool {
    path.starts_with("/etc") || path.starts_with("/Library")
}

pub(crate) fn is_trusted_system_target(canon: &Path) -> bool {
    canon.starts_with("/etc")
        || canon.starts_with("/private/etc")
        || canon.starts_with("/nix/store")
        || canon.starts_with("/run")
        || canon.starts_with("/usr")
        || canon.starts_with("/Library")
        || canon.starts_with("/System")
}

/// Walk `components` under `root` component-by-component with `openat(..., O_NOFOLLOW)` so
/// neither intermediate directories (e.g. `userdata -> /attacker`) nor the final file can
/// follow a symlink. Opens the final regular file with `O_NONBLOCK` so FIFOs never block.
fn open_under_root_nofollow(
    root: &Path,
    components: &[&str],
) -> Result<(File, fs::Metadata), ReadError> {
    if components.is_empty() {
        return Err(ReadError::Absent);
    }
    #[cfg(unix)]
    {
        use std::ffi::CString;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

        let root_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(root)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    ReadError::Absent
                } else {
                    ReadError::Unreadable(io_kind_label(e.kind()))
                }
            })?;
        let mut dir_fd: OwnedFd = root_file.into();

        for &dir_name in &components[..components.len() - 1] {
            let c_name = CString::new(dir_name)
                .map_err(|_| ReadError::Unreadable("invalid path".into()))?;
            let next_raw = unsafe {
                libc::openat(
                    dir_fd.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if next_raw < 0 {
                let err = io::Error::last_os_error();
                return Err(map_nofollow_open_err(err));
            }
            dir_fd = unsafe { OwnedFd::from_raw_fd(next_raw) };
        }

        let last = *components.last().unwrap();
        let c_last =
            CString::new(last).map_err(|_| ReadError::Unreadable("invalid path".into()))?;
        let file_raw = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                c_last.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if file_raw < 0 {
            let err = io::Error::last_os_error();
            return Err(map_nofollow_open_err(err));
        }
        let file = unsafe { File::from_raw_fd(file_raw) };
        let meta = file
            .metadata()
            .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
        if !meta.is_file() {
            return Err(ReadError::Unreadable("not a regular file".into()));
        }
        Ok((file, meta))
    }
    #[cfg(not(unix))]
    {
        let mut cur = root.to_path_buf();
        for &comp in components {
            cur.push(comp);
            if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(ReadError::Unreadable("symlink not followed".into()));
            }
        }
        let file = OpenOptions::new().read(true).open(&cur).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ReadError::Absent
            } else {
                ReadError::Unreadable(io_kind_label(e.kind()))
            }
        })?;
        let meta = file
            .metadata()
            .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
        if !meta.is_file() {
            return Err(ReadError::Unreadable("not a regular file".into()));
        }
        Ok((file, meta))
    }
}

#[cfg(unix)]
fn map_nofollow_open_err(err: io::Error) -> ReadError {
    if err.kind() == io::ErrorKind::NotFound {
        ReadError::Absent
    } else if err.raw_os_error() == Some(libc::ELOOP)
        || err.raw_os_error() == Some(libc::ENOTDIR)
    {
        // Note: on Linux `openat(..., O_DIRECTORY | O_NOFOLLOW)` returns ELOOP on a symlink;
        // on macOS/BSD it can return ENOTDIR when a symlink is opened with O_DIRECTORY | O_NOFOLLOW.
        ReadError::Unreadable("symlink not followed".into())
    } else {
        ReadError::Unreadable(io_kind_label(err.kind()))
    }
}

#[cfg(unix)]
fn stat_under_root_nofollow(root: &Path, components: &[&str]) -> Option<libc::stat> {
    use std::ffi::CString;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

    if components.is_empty() {
        return None;
    }
    let root_file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
        .open(root)
        .ok()?;
    let mut dir_fd: OwnedFd = root_file.into();
    for &dir_name in &components[..components.len() - 1] {
        let c_name = CString::new(dir_name).ok()?;
        let next_raw = unsafe {
            libc::openat(
                dir_fd.as_raw_fd(),
                c_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if next_raw < 0 {
            return None;
        }
        dir_fd = unsafe { OwnedFd::from_raw_fd(next_raw) };
    }
    let last = *components.last()?;
    let c_last = CString::new(last).ok()?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir_fd.as_raw_fd(),
            c_last.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 { Some(st) } else { None }
}

fn sig_under_root_nofollow(root: &Path, components: &[&str]) -> Option<(u64, u64, i128, u64)> {
    #[cfg(unix)]
    {
        let st = stat_under_root_nofollow(root, components)?;
        let mtime_ns =
            (st.st_mtime as i128) * 1_000_000_000i128 + (st.st_mtime_nsec as i128);
        Some((st.st_dev as u64, st.st_ino as u64, mtime_ns, st.st_size.max(0) as u64))
    }
    #[cfg(not(unix))]
    {
        let mut cur = root.to_path_buf();
        for &comp in components {
            cur.push(comp);
            if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                return None;
            }
        }
        file_sig(&cur, false)
    }
}

fn stamp_under_root_nofollow(root: &Path, components: &[&str]) -> Option<Stamp> {
    #[cfg(unix)]
    {
        let st = stat_under_root_nofollow(root, components)?;
        let modified_ms =
            (st.st_mtime as f64) * 1000.0 + (st.st_mtime_nsec as f64) / 1_000_000.0;
        Some(Stamp {
            modified_ms,
            len: st.st_size.max(0) as u64,
        })
    }
    #[cfg(not(unix))]
    {
        let mut cur = root.to_path_buf();
        for &comp in components {
            cur.push(comp);
            if fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
                return None;
            }
        }
        fs::symlink_metadata(&cur).ok().map(|m| stamp_from_meta(&m))
    }
}

/// Opens an OS-managed config file (`/etc/t3code/policy.json`, `/etc/hosts`, `/etc/nsswitch.conf`)
/// without blocking on FIFOs (`O_NONBLOCK`), validating the canonical target on OS paths so
/// neither parent-directory symlinks nor final-component symlinks can escape into user directories,
/// and verifying the opened descriptor is a regular file.
fn open_system_regular(path: &Path) -> Result<(File, fs::Metadata), ReadError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        let target_path: PathBuf = if is_os_config_path(path) {
            let canon = fs::canonicalize(path).map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    ReadError::Absent
                } else {
                    ReadError::Unreadable(io_kind_label(e.kind()))
                }
            })?;
            if !is_trusted_system_target(&canon) {
                return Err(ReadError::Unreadable("symlink outside system path".into()));
            }
            canon
        } else {
            path.to_path_buf()
        };

        let mut flags = libc::O_NONBLOCK | libc::O_CLOEXEC;
        if is_os_config_path(path) {
            flags |= libc::O_NOFOLLOW;
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(flags)
            .open(&target_path)
            .map_err(|e| {
                if e.kind() == io::ErrorKind::NotFound {
                    ReadError::Absent
                } else if e.raw_os_error() == Some(libc::ELOOP) {
                    ReadError::Unreadable("symlink not followed".into())
                } else {
                    ReadError::Unreadable(io_kind_label(e.kind()))
                }
            })?;
        let meta = file
            .metadata()
            .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
        if !meta.is_file() {
            return Err(ReadError::Unreadable("not a regular file".into()));
        }
        Ok((file, meta))
    }
    #[cfg(not(unix))]
    {
        let file = OpenOptions::new().read(true).open(path).map_err(|e| {
            if e.kind() == io::ErrorKind::NotFound {
                ReadError::Absent
            } else {
                ReadError::Unreadable(io_kind_label(e.kind()))
            }
        })?;
        let meta = file
            .metadata()
            .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
        if !meta.is_file() {
            return Err(ReadError::Unreadable("not a regular file".into()));
        }
        Ok((file, meta))
    }
}

fn read_bounded_from_file(file: File, meta: fs::Metadata, cap: u64) -> Result<(String, Stamp), ReadError> {
    if meta.len() > cap {
        return Err(ReadError::TooLarge(cap));
    }
    let stamp = stamp_from_meta(&meta);
    let mut buf = String::new();
    file.take(cap + 1)
        .read_to_string(&mut buf)
        .map_err(|e| ReadError::Unreadable(io_kind_label(e.kind())))?;
    if buf.len() as u64 > cap {
        return Err(ReadError::TooLarge(cap));
    }
    Ok((buf, stamp))
}

fn read_bounded_system_string(path: &Path, cap: u64) -> Result<(String, Stamp), ReadError> {
    let (file, meta) = open_system_regular(path)?;
    read_bounded_from_file(file, meta, cap)
}

fn read_bounded_root_string(
    root: &Path,
    components: &[&str],
    cap: u64,
) -> Result<(String, Stamp), ReadError> {
    let (file, meta) = open_under_root_nofollow(root, components)?;
    read_bounded_from_file(file, meta, cap)
}

enum ReadError {
    Absent,
    Unreadable(String),
    TooLarge(u64),
}

fn io_kind_label(kind: io::ErrorKind) -> String {
    match kind {
        io::ErrorKind::PermissionDenied => "permission denied".into(),
        io::ErrorKind::InvalidData => "not valid UTF-8".into(),
        other => format!("{other:?}").to_ascii_lowercase(),
    }
}

// ── Strict policy.json reader (ManagedPolicyDocument schema) ────────────────

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawPolicy {
    schema_version: u32,
    organization: String,
    #[serde(default)]
    #[allow(dead_code)]
    support_url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    settings: Option<serde_json::Value>,
    #[serde(default)]
    limits: Option<RawPolicyLimits>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RawPolicyLimits {
    #[serde(default)]
    #[allow(dead_code)]
    max_runtime_mode: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    clients: Option<Vec<String>>,
    #[serde(default)]
    #[allow(dead_code)]
    connect: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    webhook_tasks: Option<bool>,
    #[serde(default)]
    telemetry: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    browser_cookie_import: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    preview_tunnel: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    worktree_layout_commands: Option<bool>,
    #[serde(default)]
    #[allow(dead_code)]
    providers: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)]
    updates: Option<serde_json::Value>,
}

pub(crate) fn read_policy(path: &Path) -> Read<Policy> {
    let (raw, stamp) = match read_bounded_system_string(path, POLICY_CAP) {
        Ok(pair) => pair,
        Err(ReadError::Absent) => return Read::Absent,
        Err(ReadError::Unreadable(k)) => return Read::Unreadable(k),
        Err(ReadError::TooLarge(cap)) => {
            return Read::Invalid {
                reason: format!("exceeds {} KiB limit", cap / 1024),
                attempted_telemetry_off: false,
            };
        }
    };
    let attempted_off = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get("limits")?.get("telemetry")?.as_bool())
        == Some(false);

    match serde_json::from_str::<RawPolicy>(&raw) {
        Ok(parsed) => {
            if parsed.schema_version != 1 {
                return Read::Invalid {
                    reason: format!("unsupported schemaVersion {}", parsed.schema_version),
                    attempted_telemetry_off: attempted_off,
                };
            }
            let org = parsed.organization.trim();
            if org.is_empty() {
                return Read::Invalid {
                    reason: "organization must not be empty".into(),
                    attempted_telemetry_off: attempted_off,
                };
            }
            Read::Ok {
                value: Policy {
                    organization: org.to_owned(),
                    telemetry: parsed.limits.and_then(|l| l.telemetry),
                },
                stamp,
            }
        }
        Err(err) => Read::Invalid {
            reason: sanitize_serde_reason(&err.to_string()),
            attempted_telemetry_off: attempted_off,
        },
    }
}

fn sanitize_serde_reason(msg: &str) -> String {
    // Keep structural key names ("unknown field `foo`", "missing field `organization`"), strip raw line noise.
    let first = msg.lines().next().unwrap_or("invalid JSON");
    if let Some(idx) = first.find(" at line ") {
        first[..idx].to_owned()
    } else {
        first.chars().take(80).collect()
    }
}

// ── server-runtime.json reader ──────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRuntime {
    version: u32,
    pid: u32,
    started_at: serde_json::Value,
    #[serde(default)]
    service_managed: Option<bool>,
    #[serde(default)]
    server_version: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional_nullable")]
    managed_policy: Option<Option<RawLoadedPolicy>>,
}

fn deserialize_optional_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLoadedPolicy {
    source: String,
    organization: String,
    fingerprint: String,
}

fn read_runtime(data_folder: &Path) -> Read<Runtime> {
    let (raw, stamp) =
        match read_bounded_root_string(data_folder, &["userdata", "server-runtime.json"], RUNTIME_CAP) {
        Ok(pair) => pair,
        Err(ReadError::Absent) => return Read::Absent,
        Err(ReadError::Unreadable(k)) => return Read::Unreadable(k),
        Err(ReadError::TooLarge(cap)) => {
            return Read::Invalid {
                reason: format!("exceeds {} KiB limit", cap / 1024),
                attempted_telemetry_off: false,
            };
        }
    };
    let parsed: RawRuntime = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            return Read::Invalid {
                reason: sanitize_serde_reason(&err.to_string()),
                attempted_telemetry_off: false,
            };
        }
    };
    let (started_at, started_at_raw) = match &parsed.started_at {
        serde_json::Value::String(s) => match parse_timestamp_ms(s) {
            Some(ms) => (ms, s.clone()),
            None => {
                return Read::Invalid {
                    reason: "invalid startedAt timestamp".into(),
                    attempted_telemetry_off: false,
                };
            }
        },
        serde_json::Value::Number(n) => match n.as_f64() {
            Some(ms) if ms.is_finite() => (ms, super::format_iso_utc(ms)),
            _ => {
                return Read::Invalid {
                    reason: "invalid startedAt number".into(),
                    attempted_telemetry_off: false,
                };
            }
        },
        _ => {
            return Read::Invalid {
                reason: "missing startedAt".into(),
                attempted_telemetry_off: false,
            };
        }
    };
    Read::Ok {
        value: Runtime {
            version: parsed.version,
            pid: parsed.pid,
            started_at,
            started_at_raw,
            service_managed: parsed.service_managed,
            server_version: parsed.server_version,
            managed_policy: parsed.managed_policy.map(|opt| {
                opt.map(|p| LoadedPolicy {
                    source: p.source,
                    organization: p.organization,
                    fingerprint: p.fingerprint,
                })
            }),
        },
        stamp,
    }
}

fn classify_server(runtime: &Read<Runtime>, sys: &dyn System) -> Server {
    let Read::Ok { value: rt, .. } = runtime else {
        return Server {
            liveness: Liveness::Unknown,
            launch: Launch::Unknown,
        };
    };
    if rt.version != 1 {
        return Server {
            liveness: Liveness::Unknown,
            launch: Launch::Unknown,
        };
    }
    match sys.alive(rt.pid) {
        ProcessAlive::Stopped => {
            return Server {
                liveness: Liveness::Stopped,
                launch: Launch::Unknown,
            };
        }
        ProcessAlive::Unknown => {
            return Server {
                liveness: Liveness::Unknown,
                launch: Launch::Unknown,
            };
        }
        ProcessAlive::Alive => {}
    }

    // Linux: verify start time to catch PID reuse, and walk ancestry to classify launch.
    if let Some(stat) = sys.stat(rt.pid) {
        if let Some(proc_start) = stat.started_ms {
            if (proc_start - rt.started_at).abs() > PID_REUSE_TOLERANCE_MS {
                return Server {
                    liveness: Liveness::PidReused,
                    launch: Launch::Unknown,
                };
            }
        }
        if rt.service_managed == Some(true) {
            return Server {
                liveness: Liveness::Running,
                launch: Launch::Service,
            };
        }
        let loupe_ancestors = sys.loupe_ancestors();
        let launch = classify_ancestry(stat.ppid, &loupe_ancestors, sys);
        return Server {
            liveness: Liveness::Running,
            launch,
        };
    }

    Server {
        liveness: Liveness::Running,
        launch: if rt.service_managed == Some(true) {
            Launch::Service
        } else {
            Launch::Unknown
        },
    }
}

fn classify_ancestry(start_ppid: u32, loupe_ancestors: &[u32], sys: &dyn System) -> Launch {
    let mut cur = start_ppid;
    let mut seen = Vec::new();
    let mut desktop_comm: Option<String> = None;
    for _ in 0..16 {
        if cur <= 1 || seen.contains(&cur) {
            break;
        }
        seen.push(cur);
        let Some(st) = sys.stat(cur) else {
            break;
        };
        let comm_lower = st.comm.to_ascii_lowercase();
        if matches!(
            comm_lower.as_str(),
            "bash" | "zsh" | "fish" | "sh" | "dash" | "nu" | "xonsh" | "elvish"
        ) {
            let same = loupe_ancestors.contains(&cur);
            return Launch::Shell {
                comm: st.comm,
                same_as_loupe: same,
            };
        }
        if desktop_comm.is_none()
            && (matches!(
                comm_lower.as_str(),
                "systemd"
                    | "uwsm"
                    | "hyprland"
                    | "sway"
                    | "niri"
                    | "gnome-shell"
                    | "kwin_wayland"
                    | "kwin_x11"
                    | "electron"
                    | "t3code"
            ) || comm_lower.contains("t3-code")
                || comm_lower.contains("launcher"))
        {
            desktop_comm = Some(st.comm.clone());
        }
        if st.ppid <= 1 {
            break;
        }
        cur = st.ppid;
    }
    match desktop_comm {
        Some(comm) => Launch::Desktop(comm),
        None => Launch::Unknown,
    }
}

// ── settings.json reader (narrow projection, drops all unknown fields) ──────

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct RawSettings {
    #[serde(default)]
    agent_feedback: Option<RawAgentFeedback>,
    #[serde(default)]
    observability: Option<RawObservability>,
    #[serde(default)]
    enable_provider_update_checks: Option<bool>,
    #[serde(default)]
    share_usage_data: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawAgentFeedback {
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawObservability {
    #[serde(default)]
    otlp_traces_url: Option<String>,
    #[serde(default)]
    otlp_metrics_url: Option<String>,
    #[serde(default)]
    otlp_logs_url: Option<String>,
}

fn read_settings(data_folder: &Path) -> Read<Settings> {
    let (raw, stamp) =
        match read_bounded_root_string(data_folder, &["userdata", "settings.json"], SETTINGS_CAP) {
        Ok(pair) => pair,
        Err(ReadError::Absent) => return Read::Absent,
        Err(ReadError::Unreadable(k)) => return Read::Unreadable(k),
        Err(ReadError::TooLarge(cap)) => {
            return Read::Invalid {
                reason: format!("exceeds {} MiB limit", cap / (1024 * 1024)),
                attempted_telemetry_off: false,
            };
        }
    };
    let parsed: RawSettings = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(err) => {
            return Read::Invalid {
                reason: sanitize_serde_reason(&err.to_string()),
                attempted_telemetry_off: false,
            };
        }
    };
    let mut exporters = Vec::new();
    if let Some(obs) = parsed.observability {
        for (sig, url) in [
            (Signal::Traces, obs.otlp_traces_url),
            (Signal::Metrics, obs.otlp_metrics_url),
            (Signal::Logs, obs.otlp_logs_url),
        ] {
            if let Some(raw_url) = url {
                if let Some(ep) = parse_http_endpoint(&raw_url) {
                    exporters.push((sig, ep));
                }
            }
        }
    }
    let agent_feedback = parsed
        .agent_feedback
        .and_then(|af| af.mode)
        .filter(|m| matches!(m.as_str(), "off" | "ask" | "auto"));
    Read::Ok {
        value: Settings {
            agent_feedback,
            exporters,
            provider_update_checks: parsed.enable_provider_update_checks,
            share_usage_data: parsed.share_usage_data,
        },
        stamp,
    }
}

// ── service.env reader (line-by-line allowlist; drops all other keys immediately)

fn read_service_env(data_folder: &Path) -> Read<EnvSet> {
    let (file, meta) = match open_under_root_nofollow(data_folder, &["service.env"]) {
        Ok(pair) => pair,
        Err(ReadError::Absent) => return Read::Absent,
        Err(ReadError::Unreadable(k)) => return Read::Unreadable(k),
        Err(ReadError::TooLarge(cap)) => {
            return Read::Invalid {
                reason: format!("exceeds {} KiB limit", cap / 1024),
                attempted_telemetry_off: false,
            };
        }
    };
    if meta.len() > SERVICE_ENV_CAP {
        return Read::Invalid {
            reason: format!("exceeds {} KiB limit", SERVICE_ENV_CAP / 1024),
            attempted_telemetry_off: false,
        };
    }
    let stamp = stamp_from_meta(&meta);
    let mut reader = BufReader::new(file.take(SERVICE_ENV_CAP + 1));
    let mut env = EnvSet::default();
    let mut line = String::new();
    // Track an in-progress multiline quoted value so we stay aligned with Node `util.parseEnv`
    // (`apps/server/src/cli/service.ts:57`) without retaining values of non-allowlisted keys.
    let mut open_quote: Option<(Option<EnvKey>, char, String)> = None;
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Read::Unreadable(io_kind_label(e.kind())),
        }
        if let Some((key_opt, quote, mut acc)) = open_quote.take() {
            let stripped = line.trim_end_matches(['\r', '\n']);
            if let Some((chunk, closed)) = consume_quoted_body(stripped, quote) {
                if key_opt.is_some() {
                    acc.push('\n');
                    acc.push_str(&chunk);
                }
                if closed {
                    if let Some(key) = key_opt {
                        env.set_raw(key, &acc);
                    }
                } else {
                    open_quote = Some((key_opt, quote, acc));
                }
            }
            continue;
        }
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Matches `apps/server/src/cli/service.ts` (`util.parseEnv` + optional `export `).
        let rest = trimmed
            .strip_prefix("export ")
            .or_else(|| trimmed.strip_prefix("export\t"))
            .map(str::trim_start)
            .unwrap_or(trimmed);
        let Some((key_raw, val_raw)) = rest.split_once('=') else {
            continue;
        };
        let key_name = key_raw.trim();
        let key_opt = EnvKey::from_name(key_name);
        let val_start = val_raw.trim_start();
        if let Some(quote) = val_start.chars().next().filter(|c| matches!(c, '"' | '\'' | '`')) {
            let after_open = &val_start[quote.len_utf8()..];
            // Strip only line ending (preserve spaces inside quotes)
            let body = after_open.trim_end_matches(['\r', '\n']);
            if let Some((parsed_chunk, closed)) = consume_quoted_body(body, quote) {
                if closed {
                    if let Some(key) = key_opt {
                        env.set_raw(key, &parsed_chunk);
                    }
                } else {
                    // Multiline quoted value: only buffer content if key is allowlisted.
                    let acc = if key_opt.is_some() {
                        parsed_chunk
                    } else {
                        String::new()
                    };
                    open_quote = Some((key_opt, quote, acc));
                }
            }
        } else if let Some(key) = key_opt {
            // Unquoted value: strip inline `# comment` and trim trailing whitespace.
            let unquoted = val_start.split('#').next().unwrap_or(val_start).trim_end();
            env.set_raw(key, unquoted);
        }
    }
    Read::Ok { value: env, stamp }
}

/// Scans the body of a quoted `service.env` value until the matching closing quote.
/// Any trailing `# comment` or whitespace after the closing quote is ignored, matching
/// Node's `util.parseEnv`. Returns `Some((text, closed))`.
fn consume_quoted_body(body: &str, quote: char) -> Option<(String, bool)> {
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' && quote == '"' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(esc) => out.push(esc),
                None => out.push('\\'),
            }
            continue;
        }
        if ch == quote {
            return Some((out, true));
        }
        out.push(ch);
    }
    Some((out, false))
}

// ── /etc/hosts streamed scanner with byte prefilter ──────────────────────────

pub(crate) fn read_hosts(path: &Path, target_host: &str) -> Read<HostsScan> {
    let (file, meta) = match open_system_regular(path) {
        Ok(pair) => pair,
        Err(ReadError::Absent) => return Read::Absent,
        Err(ReadError::Unreadable(k)) => return Read::Unreadable(k),
        Err(ReadError::TooLarge(cap)) => {
            return Read::Invalid {
                reason: format!("exceeds {} MiB limit", cap / (1024 * 1024)),
                attempted_telemetry_off: false,
            };
        }
    };
    if meta.len() > HOSTS_CAP {
        return Read::Invalid {
            reason: format!("exceeds {} MiB limit", HOSTS_CAP / (1024 * 1024)),
            attempted_telemetry_off: false,
        };
    }
    let stamp = stamp_from_meta(&meta);
    let target_lower = target_host.trim().trim_end_matches('.').to_ascii_lowercase();
    let parent_domain = target_lower
        .split_once('.')
        .map(|(_, rest)| rest.to_owned())
        .filter(|rest| rest.contains('.'));

    // Fast byte prefilter needle (lowercased suffix) to skip 99.9% of blocklist lines cleanly.
    let needle: &[u8] = parent_domain
        .as_deref()
        .map(str::as_bytes)
        .unwrap_or_else(|| target_lower.as_bytes());

    let mut reader = BufReader::with_capacity(64 * 1024, file.take(HOSTS_CAP + 1));
    let mut buf = Vec::with_capacity(512);
    let mut matches = Vec::new();
    let mut bare_parent_seen = false;

    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return Read::Unreadable(io_kind_label(e.kind())),
        }
        if !contains_ascii_icase(&buf, needle) {
            continue;
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            continue;
        };
        let active = line.split('#').next().unwrap_or("");
        let mut tokens = active.split_whitespace();
        let Some(ip) = tokens.next() else {
            continue;
        };
        for host in tokens {
            let clean = host.trim_end_matches('.').to_ascii_lowercase();
            if clean == target_lower {
                matches.push(HostsMatch {
                    ip: ip.to_owned(),
                    host: target_lower.clone(),
                    sinkhole: is_sinkhole_ip(ip),
                });
            } else if parent_domain.as_deref() == Some(clean.as_str()) {
                bare_parent_seen = true;
            }
        }
    }

    Read::Ok {
        value: HostsScan {
            target_host: target_lower,
            bare_parent_only: matches.is_empty() && bare_parent_seen,
            matches,
        },
        stamp,
    }
}

fn contains_ascii_icase(haystack: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if haystack.len() < needle_lower.len() {
        return false;
    }
    let first = needle_lower[0];
    let first_upper = first.to_ascii_uppercase();
    let last_start = haystack.len() - needle_lower.len();
    for i in 0..=last_start {
        let b = haystack[i];
        if b != first && b != first_upper {
            continue;
        }
        if haystack[i..i + needle_lower.len()]
            .iter()
            .zip(needle_lower)
            .all(|(h, n)| h.to_ascii_lowercase() == *n)
        {
            return true;
        }
    }
    false
}

// ── /etc/nsswitch.conf reader (hosts: line only) ────────────────────────────

fn read_nsswitch(path: &Path) -> Read<bool> {
    let (raw, stamp) = match read_bounded_system_string(path, NSSWITCH_CAP) {
        Ok(pair) => pair,
        Err(ReadError::Absent) => return Read::Absent,
        Err(ReadError::Unreadable(k)) => return Read::Unreadable(k),
        Err(ReadError::TooLarge(cap)) => {
            return Read::Invalid {
                reason: format!("exceeds {} KiB limit", cap / 1024),
                attempted_telemetry_off: false,
            };
        }
    };
    for line in raw.lines() {
        let active = line.split('#').next().unwrap_or("").trim();
        if let Some(rest) = active.strip_prefix("hosts:") {
            // Both glibc `files` and `systemd-resolved` (`resolve`) consult `/etc/hosts`.
            let uses_files = rest
                .split_whitespace()
                .any(|tok| tok.eq_ignore_ascii_case("files") || tok.eq_ignore_ascii_case("resolve"));
            return Read::Ok {
                value: uses_files,
                stamp,
            };
        }
    }
    // No explicit hosts: line -> glibc defaults to consulting files.
    Read::Ok { value: true, stamp }
}
