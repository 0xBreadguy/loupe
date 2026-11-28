//! Windows platform boundary: Win32 implementations on Windows, plus `std`-only
//! fallback stubs when compiled on non-Windows hosts during cross-host tests.
use super::{LocalTime, ProcStat, ProcessAlive, utc_seconds, utc_time};
#[cfg(not(windows))]
use super::not_supported;
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

#[cfg(windows)]
mod win32 {
    use super::{LocalTime, ProcessAlive, utc_seconds, utc_time};
    use std::ffi::c_void;
    use std::io;

    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
    const INVALID_HANDLE_VALUE: isize = -1;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    const SW_SHOWNORMAL: i32 = 1;
    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const COINIT_DISABLE_OLE1DDE: u32 = 0x4;
    const S_OK: i32 = 0;
    const S_FALSE: i32 = 1;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    unsafe extern "system" {
        fn SystemTimeToTzSpecificLocalTime(
            tz: *const c_void,
            universal: *const SystemTime,
            local: *mut SystemTime,
        ) -> i32;
        fn TzSpecificLocalTimeToSystemTime(
            tz: *const c_void,
            local: *const SystemTime,
            universal: *mut SystemTime,
        ) -> i32;
        fn GetStdHandle(std_handle: u32) -> isize;
        fn GetConsoleScreenBufferInfo(
            console_output: isize,
            info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
        fn OpenProcess(access: u32, inherit_handles: i32, process_id: u32) -> isize;
        fn GetExitCodeProcess(process: isize, exit_code: *mut u32) -> i32;
        fn CloseHandle(object: isize) -> i32;
        fn GetLastError() -> u32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> i32;
        fn CoUninitialize();
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }

    fn to_system_time(t: LocalTime) -> Option<SystemTime> {
        if !(1601..=9999).contains(&t.year) {
            return None;
        }
        Some(SystemTime {
            year: u16::try_from(t.year).ok()?,
            month: u16::try_from(t.month0 + 1).ok()?,
            day_of_week: 0,
            day: u16::try_from(t.day).ok()?,
            hour: u16::try_from(t.hour).ok()?,
            minute: u16::try_from(t.minute).ok()?,
            second: u16::try_from(t.second).ok()?,
            milliseconds: 0,
        })
    }

    fn from_system_time(st: SystemTime) -> LocalTime {
        LocalTime {
            year: i32::from(st.year),
            month0: i32::from(st.month) - 1,
            day: i32::from(st.day),
            hour: i32::from(st.hour),
            minute: i32::from(st.minute),
            second: i32::from(st.second),
        }
    }

    pub fn local_time(epoch_seconds: i64) -> Option<LocalTime> {
        let utc = utc_time(epoch_seconds);
        let Some(utc_st) = to_system_time(utc) else {
            return Some(utc);
        };
        let mut local_st = SystemTime::default();
        // SAFETY: both pointers reference live initialized `SystemTime` structs.
        let ok = unsafe {
            SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc_st, &mut local_st)
        };
        if ok != 0 {
            Some(from_system_time(local_st))
        } else {
            Some(utc)
        }
    }

    pub fn local_to_epoch(
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) -> Option<i64> {
        // Roll over out-of-range fields first (matching mktime behavior) before Win32 TZ conversion.
        let normalized = utc_seconds(year, month, day, hour, minute, second);
        let civil = utc_time(normalized);
        let Some(local_st) = to_system_time(civil) else {
            return Some(normalized);
        };
        let mut utc_st = SystemTime::default();
        // SAFETY: both pointers reference live initialized `SystemTime` structs.
        let ok = unsafe {
            TzSpecificLocalTimeToSystemTime(std::ptr::null(), &local_st, &mut utc_st)
        };
        if ok != 0 {
            Some(utc_seconds(
                i64::from(utc_st.year),
                i64::from(utc_st.month),
                i64::from(utc_st.day),
                i64::from(utc_st.hour),
                i64::from(utc_st.minute),
                i64::from(utc_st.second),
            ))
        } else {
            Some(normalized)
        }
    }

    pub fn open_url(url: &str) -> io::Result<()> {
        if !url.starts_with("t3code://")
            || url.chars().any(|c| c == '\0' || c.is_control())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid T3 Code link",
            ));
        }
        let op: Vec<u16> = "open\0".encode_utf16().collect();
        let target: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
        // SAFETY: COM apartment init for ShellExecuteW on this thread, paired with CoUninitialize.
        let com_hr = unsafe {
            CoInitializeEx(
                std::ptr::null_mut(),
                COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
            )
        };
        // SAFETY: op and target are NUL-terminated UTF-16 buffers valid for the call.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                op.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        if com_hr == S_OK || com_hr == S_FALSE {
            // SAFETY: matches successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
        if result > 32 {
            Ok(())
        } else {
            let err = io::Error::last_os_error();
            Err(if err.raw_os_error().is_some_and(|c| c != 0) {
                err
            } else {
                io::Error::other(format!("ShellExecuteW failed with code {result}"))
            })
        }
    }

    pub fn terminal_size() -> (Option<usize>, Option<usize>) {
        // SAFETY: GetStdHandle and GetConsoleScreenBufferInfo inspect stdout's console window.
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            if handle == 0 || handle == INVALID_HANDLE_VALUE {
                return (None, None);
            }
            let mut info = ConsoleScreenBufferInfo::default();
            if GetConsoleScreenBufferInfo(handle, &mut info) == 0 {
                return (None, None);
            }
            let cols = i32::from(info.window.right) - i32::from(info.window.left) + 1;
            let rows = i32::from(info.window.bottom) - i32::from(info.window.top) + 1;
            (
                usize::try_from(cols).ok().filter(|&n| n > 0),
                usize::try_from(rows).ok().filter(|&n| n > 0),
            )
        }
    }

    pub fn process_alive(pid: u32) -> ProcessAlive {
        if pid == 0 {
            return ProcessAlive::Stopped;
        }
        // SAFETY: OpenProcess/GetExitCodeProcess/CloseHandle on the queried pid.
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle == 0 {
                return match GetLastError() {
                    ERROR_ACCESS_DENIED => ProcessAlive::Alive,
                    ERROR_INVALID_PARAMETER => ProcessAlive::Stopped,
                    _ => ProcessAlive::Unknown,
                };
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            if ok == 0 {
                ProcessAlive::Unknown
            } else if code == STILL_ACTIVE {
                ProcessAlive::Alive
            } else {
                ProcessAlive::Stopped
            }
        }
    }
}

/// Calendar fields for epoch seconds in the process's local zone.
pub fn local_time(epoch_seconds: i64) -> Option<LocalTime> {
    #[cfg(windows)]
    {
        win32::local_time(epoch_seconds)
    }
    #[cfg(not(windows))]
    {
        Some(utc_time(epoch_seconds))
    }
}

/// Seconds since the epoch for a civil time in the process's local zone.
pub fn local_to_epoch(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> Option<i64> {
    #[cfg(windows)]
    {
        win32::local_to_epoch(year, month, day, hour, minute, second)
    }
    #[cfg(not(windows))]
    {
        Some(utc_seconds(year, month, day, hour, minute, second))
    }
}

/// Zero never equals a live process id, so the status watcher's parent check never fires.
/// Its loop still ends when a write to stdout fails.
pub fn parent_pid() -> i32 {
    0
}

/// Ask Windows Shell to open a `t3code://` link via `ShellExecuteW`.
pub fn open_url(url: &str) -> io::Result<()> {
    #[cfg(windows)]
    {
        win32::open_url(url)
    }
    #[cfg(not(windows))]
    {
        let _ = url;
        Err(not_supported("opening a T3 Code link"))
    }
}

/// Nothing to hand back: only glibc holds freed heap this way.
pub fn release_memory() {}

/// The Hyprland focus watcher exists only on Linux/Hyprland.
pub struct FocusWatcher(());

impl FocusWatcher {
    pub fn start(_dir: PathBuf, _sender: Sender<Option<String>>) -> io::Result<Self> {
        #[cfg(windows)]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "following the focused T3 window: not supported on this platform yet",
            ))
        }
        #[cfg(not(windows))]
        {
            Err(not_supported("following the focused T3 window"))
        }
    }

    pub fn stop(&mut self) {}
}

/// The terminal's columns and its rows, when standard output is a Windows console.
pub fn terminal_size() -> (Option<usize>, Option<usize>) {
    #[cfg(windows)]
    {
        win32::terminal_size()
    }
    #[cfg(not(windows))]
    {
        (None, None)
    }
}

/// Whether `pid` is alive on Windows.
pub fn process_alive(pid: u32) -> ProcessAlive {
    #[cfg(windows)]
    {
        win32::process_alive(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        ProcessAlive::Unknown
    }
}

/// Process ancestry metadata is Linux-only.
pub fn process_stat(_pid: u32) -> Option<ProcStat> {
    None
}

/// Ancestor PIDs are Linux-only.
pub fn loupe_ancestors() -> Vec<u32> {
    Vec::new()
}
