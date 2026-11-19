//! Linux and macOS, and the other Unixes: libc.
use super::LocalTime;
use std::{io, process::Command, process::Stdio};

/// Calendar fields for epoch seconds in the process's local zone, from `localtime_r`.
/// `None` when the C library cannot convert the value.
pub fn local_time(epoch_seconds: i64) -> Option<LocalTime> {
    let seconds = epoch_seconds as libc::time_t;
    let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
    // SAFETY: both pointers point to correctly sized live values; libc initializes tm on success.
    let out = unsafe { libc::localtime_r(&seconds, tm.as_mut_ptr()) };
    if out.is_null() {
        return None;
    }
    // SAFETY: localtime_r returned non-null, so it filled `tm`.
    let tm = unsafe { tm.assume_init() };
    Some(LocalTime {
        year: tm.tm_year + 1900,
        month0: tm.tm_mon,
        day: tm.tm_mday,
        hour: tm.tm_hour,
        minute: tm.tm_min,
        second: tm.tm_sec,
    })
}

/// Seconds since the epoch for a civil time in the process's local zone, from `mktime` with
/// the daylight-saving flag left for the C library to decide. Fields past their range roll over.
pub fn local_to_epoch(
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
) -> Option<i64> {
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

/// The home folder in the passwd entry of the user this process runs as, or `None` when the
/// account has no entry. Asked by effective user id, as libuv does for Node's `os.homedir()`.
pub fn passwd_home() -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    let mut buffer: Vec<libc::c_char> = vec![0; 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `entry`, `buffer` and `found` are live for the call and `buffer.len()` is the
        // buffer's real size; getpwuid_r writes only inside them.
        let code = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if code == libc::ERANGE && buffer.len() < (1 << 20) {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if code != 0 || found.is_null() {
            return None;
        }
        // SAFETY: a non-null `found` means getpwuid_r filled `entry`, and `pw_dir` is either
        // null or a NUL-terminated string inside `buffer`, which is still alive here.
        let folder = unsafe { (*found).pw_dir };
        if folder.is_null() {
            return None;
        }
        // SAFETY: checked non-null just above; NUL-terminated by getpwuid_r.
        let bytes = unsafe { std::ffi::CStr::from_ptr(folder) }.to_bytes().to_vec();
        return Some(std::ffi::OsString::from_vec(bytes));
    }
}

/// The parent's process id. It changes to 1 (or a subreaper) when the parent exits.
pub fn parent_pid() -> i32 {
    // SAFETY: getppid takes no arguments and cannot fail.
    unsafe { libc::getppid() }
}

/// Ask the desktop to open a link: `xdg-open` on Linux and the other Unixes, `open` on macOS.
/// The program is started in its own process group and reaped on a thread, so the caller never
/// waits for the handler and keeps none of its output. A missing program is the error.
pub fn open_url(url: &str) -> io::Result<()> {
    use std::os::unix::process::CommandExt;
    let program = if cfg!(target_vendor = "apple") {
        "open"
    } else {
        "xdg-open"
    };
    let mut command = Command::new(program);
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Hand freed heap back to the system.
///
/// A full read of a real database allocates tens of megabytes for a moment: every row it
/// decodes, before the few fields that are kept. glibc holds on to that as free heap, so a
/// process that keeps only a status line still shows the size of its largest read. Only glibc
/// does this; elsewhere it does nothing.
pub fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only returns free pages at the top of the heap and in free chunks.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// The terminal's columns and its rows, when standard output is one. Each is known on its own:
/// a terminal that gives its width and no height is still that wide.
pub fn terminal_size() -> (Option<usize>, Option<usize>) {
    // SAFETY: `winsize` is plain data and the ioctl only writes into it.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ as _, &mut size) } == 0;
    let known = |cells: u16| (ok && cells > 0).then_some(cells as usize);
    (known(size.ws_col), known(size.ws_row))
}
