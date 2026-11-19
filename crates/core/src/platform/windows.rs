//! Windows: stubs that compile with `std` alone and say "not supported yet" where there is
//! nothing real. The only things that work are the pure ones: UTC time, the home directory
//! lookup, and a link that fails cleanly (the UI turns that into a note, never a crash).
//!
//! Nothing here has run on Windows. Test builds on other hosts compile this file so the stubs
//! cannot rot unseen.
use super::{LocalTime, not_supported, utc_seconds, utc_time};
use std::{ffi::OsString, io, path::PathBuf, sync::mpsc::Sender};

/// Windows has no `HOME`: `USERPROFILE` first, then `HOMEDRIVE` joined with `HOMEPATH`.
/// Empty values count as unset.
pub fn home_from(var: &dyn Fn(&str) -> Option<OsString>) -> Option<OsString> {
    let non_empty = |name: &str| var(name).filter(|value| !value.is_empty());
    if let Some(profile) = non_empty("USERPROFILE") {
        return Some(profile);
    }
    let mut joined = non_empty("HOMEDRIVE")?;
    joined.push(non_empty("HOMEPATH")?);
    Some(joined)
}

/// UTC until a C runtime binding exists, which is what the headless clock already did off Unix.
/// The labels are right on a machine set to UTC and off by the zone offset elsewhere.
pub fn local_time(epoch_seconds: i64) -> Option<LocalTime> {
    Some(utc_time(epoch_seconds))
}

/// UTC, for the same reason as [`local_time`].
pub fn local_to_epoch(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Option<i64> {
    Some(utc_seconds(year, month, day, hour, minute, second))
}

/// Zero never equals a live process id, so the status watcher's parent check never fires.
/// Its loop still ends when a write to stdout fails.
pub fn parent_pid() -> i32 {
    0
}

/// Opening a `t3code://` link needs `ShellExecuteW`, which is not bound yet.
pub fn open_url(_url: &str) -> io::Result<()> {
    Err(not_supported("opening a T3 Code link"))
}

/// Nothing to hand back: only glibc holds freed heap this way.
pub fn release_memory() {}

/// The Hyprland focus watcher exists only on Unix. This stand-in lets `Host` name the type.
pub struct FocusWatcher(());

impl FocusWatcher {
    pub fn start(_dir: PathBuf, _sender: Sender<Option<String>>) -> io::Result<Self> {
        Err(not_supported("following the focused T3 window"))
    }

    pub fn stop(&mut self) {}
}

/// The terminal's columns and its rows. Not read on Windows yet.
pub fn terminal_size() -> (Option<usize>, Option<usize>) {
    (None, None)
}
