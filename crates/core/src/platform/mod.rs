//! The platform boundary. Everything in this crate that depends on the operating system
//! lives behind this module: local time, the parent process, opening a link, the Hyprland
//! focus watcher's stand-in, and handing freed heap back.
//!
//! `unix.rs` is Linux and macOS (and the other Unixes). `windows.rs` has the same functions
//! with stubs that compile and say "not supported yet" where there is nothing real yet. The
//! Windows file uses only `std`, so test builds on every host compile it too and prove the
//! stubs still build and still answer as documented, even though nothing here can
//! cross-compile for Windows.
//!
//! Nothing outside this module may call `libc` for these. Linux behavior is the reference.
#[cfg(any(windows, test))]
use std::io;
use std::ffi::OsString;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(any(windows, test))]
#[cfg_attr(not(windows), allow(dead_code))]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// The message every Windows stub gives.
pub const NOT_SUPPORTED_YET: &str = "not supported on this platform yet";

#[cfg(any(windows, test))]
pub(crate) fn not_supported(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{what}: {NOT_SUPPORTED_YET}"),
    )
}

/// Calendar fields for epoch seconds in some zone: the fields of `struct tm` the program reads.
///
/// `year` is the full year, `month0` runs 0 to 11, `day` from 1. `Default` is all zero, which
/// is what the program shows when a conversion fails.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i32,
    pub month0: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
    pub second: i32,
}

/// Liveness of a recorded process id, from `kill(pid, 0)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessAlive {
    Alive,
    Stopped,
    Unknown,
}

/// Non-secret metadata from `/proc/<pid>/stat` (Linux only). Never reads `environ` or `cmdline`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcStat {
    pub comm: String,
    pub ppid: u32,
    pub started_ms: Option<f64>,
}

/// The user's home directory, found the way the original finds it (Node's `os.homedir()`).
///
/// Unix: `HOME` whenever it is set, even to nothing; with no `HOME` at all, the home folder in
/// the account's passwd entry, so a process started with a bare environment still finds
/// `~/.t3`. Windows: `USERPROFILE`, then `HOMEDRIVE` joined with `HOMEPATH`; neither is expected
/// to be set on Unix. The Windows lookup is a pure function over a variable reader so it can be
/// tested on any host.
///
/// Where there is no passwd entry either, Node throws and the original never starts; this
/// returns `None` and paths under the home folder become relative.
pub fn home_dir() -> Option<OsString> {
    #[cfg(unix)]
    {
        home_or(std::env::var_os("HOME"), unix::passwd_home)
    }
    #[cfg(not(unix))]
    {
        windows::home_from(&|name| std::env::var_os(name))
    }
}

/// `HOME` wins whenever the variable exists, empty included. The account record is asked only
/// when there is no variable.
#[cfg(any(unix, test))]
pub(crate) fn home_or(
    variable: Option<OsString>,
    account: impl FnOnce() -> Option<OsString>,
) -> Option<OsString> {
    variable.or_else(account)
}

#[cfg(any(windows, test))]
/// Seconds since the epoch for a UTC calendar time. Out-of-range days and hours roll over.
/// (Hinnant's days-from-civil.)
pub(crate) fn utc_seconds(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468 + day - 1;
    days * 86400 + hour * 3600 + minute * 60 + second
}

#[cfg(any(windows, test))]
/// The UTC calendar time for seconds since the epoch. (Hinnant's civil-from-days.)
pub(crate) fn utc_time(seconds: i64) -> LocalTime {
    let days = seconds.div_euclid(86400);
    let rest = seconds.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    LocalTime {
        year: year as i32,
        month0: (month - 1) as i32,
        day: day as i32,
        hour: (rest / 3600) as i32,
        minute: (rest % 3600 / 60) as i32,
        second: (rest % 60) as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn gm(seconds: i64) -> LocalTime {
        let t = seconds as libc::time_t;
        // SAFETY: both pointers are live and correctly sized; gmtime_r fills `tm`.
        let tm = unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            assert!(!libc::gmtime_r(&t, &mut tm).is_null());
            tm
        };
        LocalTime {
            year: tm.tm_year + 1900,
            month0: tm.tm_mon,
            day: tm.tm_mday,
            hour: tm.tm_hour,
            minute: tm.tm_min,
            second: tm.tm_sec,
        }
    }

    /// The UTC arithmetic Windows falls back on matches the C library over a wide range,
    /// including negative seconds, leap days and century years.
    #[cfg(unix)]
    #[test]
    fn utc_arithmetic_matches_the_c_library() {
        let mut seconds = -5_000_000_000i64;
        while seconds < 8_000_000_000 {
            let got = utc_time(seconds);
            assert_eq!(got, gm(seconds), "utc_time({seconds})");
            let back = utc_seconds(
                i64::from(got.year),
                i64::from(got.month0) + 1,
                i64::from(got.day),
                i64::from(got.hour),
                i64::from(got.minute),
                i64::from(got.second),
            );
            assert_eq!(back, seconds, "round trip {seconds}");
            seconds += 604_803;
        }
        for edge in [0, -1, 86_399, 86_400, 951_782_399, 951_782_400, 1_709_251_199, 1_709_251_200] {
            assert_eq!(utc_time(edge), gm(edge), "edge {edge}");
        }
    }

    #[test]
    fn utc_seconds_rolls_over_out_of_range_fields() {
        assert_eq!(utc_seconds(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(utc_seconds(1970, 1, 1, 24, 0, 0), 86_400);
        assert_eq!(utc_seconds(1970, 1, 32, 0, 0, 0), 31 * 86_400);
    }

    /// The stubs compile on every host and say so plainly. On Linux and macOS the real
    /// functions run; this only exercises the Windows file.
    #[test]
    fn windows_stubs_answer_not_supported() {
        let err = windows::open_url("t3code://threads/a/b").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("not supported"), "{err}");
        let (tx, _rx) = std::sync::mpsc::channel();
        let err = windows::FocusWatcher::start(std::path::PathBuf::new(), tx)
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert_eq!(windows::parent_pid(), 0);
        let local = windows::local_time(86_400 + 3_661).unwrap();
        assert_eq!((local.day, local.hour, local.minute, local.second), (2, 1, 1, 1));
        assert_eq!(windows::local_to_epoch(1970, 1, 2, 1, 1, 1), Some(86_400 + 3_661));
        windows::release_memory();
    }

    /// `os.homedir()`'s order: the variable whenever it exists, the account record otherwise.
    #[test]
    fn home_is_the_variable_even_when_empty_then_the_account() {
        use std::ffi::OsStr;
        let account = || Some(OsString::from("/account"));
        assert_eq!(home_or(Some("/var".into()), account).as_deref(), Some(OsStr::new("/var")));
        assert_eq!(home_or(Some("".into()), account).as_deref(), Some(OsStr::new("")));
        assert_eq!(home_or(None, account).as_deref(), Some(OsStr::new("/account")));
        assert_eq!(home_or(None, || None), None);
        let asked = std::cell::Cell::new(false);
        home_or(Some("/var".into()), || {
            asked.set(true);
            None
        });
        assert!(!asked.get(), "the account record is not read when HOME is set");
    }

    /// The passwd lookup answers with this account's folder. Checked against `/etc/passwd`
    /// where the account is listed there; an account served from elsewhere only has to be
    /// absolute.
    #[cfg(unix)]
    #[test]
    fn passwd_home_is_this_accounts_folder() {
        let found = unix::passwd_home().expect("this account has a passwd entry");
        let found = found.to_str().expect("a UTF-8 home folder on the test host").to_owned();
        assert!(found.starts_with('/'), "{found}");
        // SAFETY: geteuid takes no arguments and cannot fail.
        let uid = unsafe { libc::geteuid() }.to_string();
        let listed = std::fs::read_to_string("/etc/passwd").ok().and_then(|text| {
            text.lines()
                .map(|line| line.split(':').collect::<Vec<_>>())
                .find(|fields| fields.len() >= 6 && fields[2] == uid)
                .map(|fields| fields[5].to_owned())
        });
        if let Some(listed) = listed {
            assert_eq!(found, listed);
        }
    }

    #[test]
    fn windows_home_is_userprofile_then_drive_and_path() {
        use std::ffi::OsStr;
        let pick = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(*value))
            }
        };
        let both = pick(&[("USERPROFILE", r"C:\Users\a"), ("HOMEDRIVE", "D:"), ("HOMEPATH", r"\b")]);
        assert_eq!(windows::home_from(&both).as_deref(), Some(OsStr::new(r"C:\Users\a")));
        let drive = pick(&[("HOMEDRIVE", "D:"), ("HOMEPATH", r"\b")]);
        assert_eq!(windows::home_from(&drive).as_deref(), Some(OsStr::new(r"D:\b")));
        let empty = pick(&[("USERPROFILE", ""), ("HOMEDRIVE", "D:")]);
        assert_eq!(windows::home_from(&empty), None);
        assert_eq!(windows::home_from(&pick(&[])), None);
    }
}
