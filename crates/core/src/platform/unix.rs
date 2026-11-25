//! Linux and macOS, and the other Unixes: libc.
use super::{LocalTime, ProcStat, ProcessAlive};
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

/// Whether `pid` is alive according to `kill(pid, 0)`. `EPERM` means the process exists under
/// another uid; `ESRCH` means no such process.
pub fn process_alive(pid: u32) -> ProcessAlive {
    let Ok(pid) = i32::try_from(pid) else {
        return ProcessAlive::Stopped;
    };
    if pid <= 0 {
        return ProcessAlive::Stopped;
    }
    // SAFETY: signal 0 tests process existence without delivering a signal.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return ProcessAlive::Alive;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::EPERM) => ProcessAlive::Alive,
        Some(libc::ESRCH) => ProcessAlive::Stopped,
        _ => ProcessAlive::Unknown,
    }
}

/// Non-secret metadata (`comm`, `ppid`, approximate start epoch ms) from `/proc/<pid>/stat` on
/// Linux. Never opens `environ` or `cmdline`. Returns `None` on other Unixes or on read error.
pub fn process_stat(pid: u32) -> Option<ProcStat> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        if pid == 0 {
            return None;
        }
        let file = std::fs::File::open(format!("/proc/{pid}/stat")).ok()?;
        let mut buf = String::new();
        file.take(4096).read_to_string(&mut buf).ok()?;
        let (comm, rest) = parse_proc_stat_comm(&buf)?;
        let fields: Vec<&str> = rest.split_whitespace().collect();
        // After ") ": field 0 is state, field 1 is ppid, field 19 is starttime (22nd overall).
        let ppid = fields.get(1)?.parse::<u32>().ok()?;
        let start_ticks = fields.get(19).and_then(|s| s.parse::<u64>().ok());
        let started_ms = start_ticks.and_then(|ticks| {
            let btime = linux_boot_time_secs()?;
            // SAFETY: sysconf(_SC_CLK_TCK) is thread-safe and takes a constant selector.
            let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
            if hz <= 0 {
                return None;
            }
            Some((btime as f64 + (ticks as f64 / hz as f64)) * 1000.0)
        });
        Some(ProcStat {
            comm,
            ppid,
            started_ms,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

#[cfg(target_os = "linux")]
fn parse_proc_stat_comm(raw: &str) -> Option<(String, &str)> {
    let open = raw.find('(')?;
    let close = raw.rfind(')')?;
    if close <= open {
        return None;
    }
    let comm = raw[open + 1..close].to_owned();
    let rest = raw.get(close + 1..)?;
    Some((comm, rest))
}

#[cfg(target_os = "linux")]
fn linux_boot_time_secs() -> Option<u64> {
    use std::io::Read;
    let file = std::fs::File::open("/proc/stat").ok()?;
    let mut buf = String::new();
    file.take(65_536).read_to_string(&mut buf).ok()?;
    for line in buf.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse::<u64>().ok();
        }
    }
    None
}

/// Ancestor PIDs of the current Loupe process, walking `ppid` up to 16 hops on Linux.
pub fn loupe_ancestors() -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        let mut out = Vec::new();
        let mut cur = u32::try_from(parent_pid()).unwrap_or(0);
        for _ in 0..16 {
            if cur <= 1 || out.contains(&cur) {
                break;
            }
            out.push(cur);
            match process_stat(cur) {
                Some(stat) if stat.ppid > 1 => cur = stat.ppid,
                _ => break,
            }
        }
        out
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}
