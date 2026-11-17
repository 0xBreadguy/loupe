//! Linux and macOS. The same code runs on both: there is no `eventfd`, no `poll` on the
//! terminal, and no ioctl constant of our own (the locked `libc` exports the window-size and
//! controlling-terminal numbers for Apple too; their integer types differ, hence `as _`).
use super::{Ready, Signals};
use crate::{ENTER_TERM, LEAVE_TERM};
use std::io::{self, ErrorKind};
use std::os::fd::RawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::time::Duration;

const GOT_WINCH: u32 = 1;
const GOT_QUIT: u32 = 2;

static WAKE_FD: AtomicI32 = AtomicI32::new(-1);
static PENDING: AtomicU32 = AtomicU32::new(0);
static RESTORE: Mutex<Option<Saved>> = Mutex::new(None);
/// Mode 2027 was found off and this program switched it on: leaving puts it back.
static GRAPHEME_TO_RESTORE: AtomicBool = AtomicBool::new(false);

struct Saved {
    fd: i32,
    out: i32,
    orig: libc::termios,
}

// ── The wake channel ───────────────────────────────────────────────────────

/// A pipe the loop waits on beside the terminal. Anything can wake the loop by writing a byte
/// to the write end, a signal handler included. Both ends are non-blocking and close-on-exec.
pub struct Wake {
    read: RawFd,
    write: RawFd,
}

/// The write end of a [`Wake`], as a plain number. `Copy` and `Send`, so a reader thread can hold
/// one. A signal handler may call [`Waker::wake`]: it is one `write`.
#[derive(Clone, Copy, Debug)]
pub struct Waker(RawFd);

impl Wake {
    pub fn new() -> io::Result<Self> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` has room for the two descriptors pipe writes.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let wake = Self {
            read: fds[0],
            write: fds[1],
        };
        // `pipe2` does not exist on macOS, so set the flags after the fact on both systems.
        for fd in fds {
            set_flags(fd)?;
        }
        Ok(wake)
    }

    pub fn waker(&self) -> Waker {
        Waker(self.write)
    }

    /// Empty the pipe so the next wait blocks again.
    pub fn drain(&self) {
        let mut buf = [0u8; 256];
        loop {
            // SAFETY: `buf` is writable for its whole length.
            let n = unsafe { libc::read(self.read, buf.as_mut_ptr() as *mut _, buf.len()) };
            if n <= 0 {
                break;
            }
        }
    }
}

impl Drop for Wake {
    fn drop(&mut self) {
        // SAFETY: both descriptors are this value's own.
        unsafe {
            libc::close(self.read);
            libc::close(self.write);
        }
    }
}

fn set_flags(fd: RawFd) -> io::Result<()> {
    // SAFETY: plain fcntl calls on a descriptor this module just opened.
    unsafe {
        if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) < 0 {
            return Err(io::Error::last_os_error());
        }
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

impl Waker {
    /// Make the loop's next wait return. A full pipe already has a wake pending, so a failed
    /// write loses nothing.
    pub fn wake(self) {
        let one = 1u8;
        // SAFETY: one readable byte. `write` is async-signal-safe.
        unsafe { libc::write(self.0, &one as *const u8 as *const libc::c_void, 1) };
    }
}

// ── The wait ───────────────────────────────────────────────────────────────

/// Block until the terminal has input, the wake pipe has a byte, the timeout passes, or a signal
/// interrupts. `None` waits for as long as it takes: a calm screen does not wake by itself.
///
/// This is `pselect`, not `poll`. macOS reports a terminal as ready to `poll` whether or not it
/// is, so a `poll` wait spins there; `select` on a terminal is the portable wait. Linux runs the
/// same call so the checks here cover the code a Mac runs.
pub fn wait(wake: &Wake, timeout: Option<Duration>) -> io::Result<Ready> {
    wait_on(libc::STDIN_FILENO, wake, timeout)
}

/// [`wait`] on any descriptor for input, so tests can use a pipe where the terminal would be.
pub fn wait_on(input: RawFd, wake: &Wake, timeout: Option<Duration>) -> io::Result<Ready> {
    let top = input.max(wake.read);
    if top as usize >= libc::FD_SETSIZE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "descriptor beyond FD_SETSIZE",
        ));
    }
    // SAFETY: `set` is zeroed through FD_ZERO before use; both descriptors are below FD_SETSIZE.
    let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::FD_ZERO(&mut set);
        libc::FD_SET(input, &mut set);
        libc::FD_SET(wake.read, &mut set);
    }
    let limit = timeout.map(timespec);
    let limit = limit.as_ref().map_or(std::ptr::null(), |t| t as *const _);
    // SAFETY: `set` and `limit` outlive the call; no write or error sets, no signal mask change.
    let rc = unsafe {
        libc::pselect(
            top + 1,
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            limit,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted {
            // The handler already left a byte in the pipe and set its flag; the sets are
            // undefined after EINTR, so report nothing ready and let the loop look again.
            return Ok(Ready::default());
        }
        return Err(err);
    }
    // SAFETY: `set` was filled in by a successful pselect.
    Ok(Ready {
        input: unsafe { libc::FD_ISSET(input, &set) },
        wake: unsafe { libc::FD_ISSET(wake.read, &set) },
    })
}

fn timespec(after: Duration) -> libc::timespec {
    libc::timespec {
        tv_sec: after.as_secs().min(i32::MAX as u64) as libc::time_t,
        tv_nsec: after.subsec_nanos() as _,
    }
}

/// Wait until `fd` can be written, for as long as it takes.
fn wait_writable(fd: RawFd) -> io::Result<()> {
    if fd as usize >= libc::FD_SETSIZE {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "descriptor beyond FD_SETSIZE",
        ));
    }
    // SAFETY: as in `wait`; only the write set is used.
    let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::FD_ZERO(&mut set);
        libc::FD_SET(fd, &mut set);
    }
    let rc = unsafe {
        libc::pselect(
            fd + 1,
            std::ptr::null_mut(),
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() != ErrorKind::Interrupted {
            return Err(err);
        }
    }
    Ok(())
}

/// Write every byte, retrying short writes and `EAGAIN`.
pub fn write_all_fd(fd: i32, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        // SAFETY: `data` is readable for its whole length.
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n > 0 {
            data = &data[n as usize..];
            continue;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            ErrorKind::Interrupted => continue,
            ErrorKind::WouldBlock => wait_writable(fd)?,
            _ => return Err(err),
        }
    }
    Ok(())
}

/// Write to the terminal.
pub fn write_stdout(data: &[u8]) -> io::Result<()> {
    write_all_fd(libc::STDOUT_FILENO, data)
}

// ── Signals ────────────────────────────────────────────────────────────────

extern "C" fn on_signal(sig: libc::c_int) {
    let bit = if sig == libc::SIGWINCH {
        GOT_WINCH
    } else {
        GOT_QUIT
    };
    PENDING.fetch_or(bit, Ordering::Relaxed);
    let fd = WAKE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        Waker(fd).wake();
    }
}

/// Catch resize, interrupt, terminate and hangup, and wake the loop through `waker` when one
/// arrives.
pub fn install_signals(waker: Waker) -> io::Result<()> {
    WAKE_FD.store(waker.0, Ordering::Relaxed);
    // SAFETY: standard sigaction setup with a handler that only touches atomics and one `write`.
    unsafe {
        // The parent may have blocked these. A blocked SIGWINCH never reaches the handler.
        let mut unblock: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut unblock);
        for sig in [libc::SIGWINCH, libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            libc::sigaddset(&mut unblock, sig);
        }
        libc::sigprocmask(libc::SIG_UNBLOCK, &unblock, std::ptr::null_mut());
        let mut sa: libc::sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as *const () as libc::sighandler_t;
        libc::sigemptyset(&mut sa.sa_mask);
        for sig in [libc::SIGWINCH, libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            if libc::sigaction(sig, &sa, std::ptr::null_mut()) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// The signals that arrived since the last call.
pub fn take_signals() -> Signals {
    let pending = PENDING.swap(0, Ordering::SeqCst);
    Signals {
        resized: pending & GOT_WINCH != 0,
        quit: pending & GOT_QUIT != 0,
    }
}

// ── The terminal ───────────────────────────────────────────────────────────

/// Puts the terminal back when dropped.
pub struct Restorer;

impl Drop for Restorer {
    fn drop(&mut self) {
        restore_now();
    }
}

fn restore_now() {
    let saved = RESTORE.lock().unwrap_or_else(|e| e.into_inner()).take();
    let Some(saved) = saved else { return };
    if GRAPHEME_TO_RESTORE.swap(false, Ordering::SeqCst) {
        let _ = write_all_fd(saved.out, crate::GRAPHEME_OFF);
    }
    let _ = write_all_fd(saved.out, LEAVE_TERM);
    // SAFETY: `orig` is the attribute block `tcgetattr` filled in.
    unsafe { libc::tcsetattr(saved.fd, libc::TCSANOW, &saved.orig) };
}

/// Raw mode on the terminal, the alternate screen, and a panic hook that restores both. The
/// returned guard restores them on drop.
pub fn enter_raw() -> io::Result<Restorer> {
    let fd = libc::STDIN_FILENO;
    let out = libc::STDOUT_FILENO;
    // SAFETY: termios is plain data; tcgetattr fills it in.
    let mut orig: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut orig) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut raw = orig;
    raw.c_iflag &= !(libc::BRKINT | libc::ICRNL | libc::INPCK | libc::ISTRIP | libc::IXON);
    raw.c_oflag &= !libc::OPOST;
    raw.c_cflag |= libc::CS8;
    raw.c_lflag &= !(libc::ECHO | libc::ICANON | libc::IEXTEN | libc::ISIG);
    // VMIN 0 makes a no-data read return 0, which is indistinguishable from hangup.
    raw.c_cc[libc::VMIN] = 1;
    raw.c_cc[libc::VTIME] = 0;
    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } < 0 {
        return Err(io::Error::last_os_error());
    }
    for fd in [fd, out] {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags >= 0 {
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        }
    }
    *RESTORE.lock().unwrap_or_else(|e| e.into_inner()) = Some(Saved { fd, out, orig });
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_now();
        prev(info);
    }));
    write_all_fd(out, ENTER_TERM)?;
    Ok(Restorer)
}

/// Record that mode 2027 was found off and has been switched on, so every way out of the program
/// (quit, a signal, a panic) switches it off again.
pub fn restore_grapheme_mode_on_exit() {
    GRAPHEME_TO_RESTORE.store(true, Ordering::SeqCst);
}

/// Columns and rows of the terminal on standard output, each at least 1.
pub fn window_size() -> io::Result<(usize, usize)> {
    // SAFETY: winsize is plain data; the ioctl fills it in.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ as _, &mut size) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((size.ws_col.max(1) as usize, size.ws_row.max(1) as usize))
}

/// Everything waiting on standard input, and whether the terminal hung up.
pub fn read_input() -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        // SAFETY: `tmp` is writable for its whole length.
        let n = unsafe { libc::read(libc::STDIN_FILENO, tmp.as_mut_ptr() as *mut _, tmp.len()) };
        if n > 0 {
            buf.extend_from_slice(&tmp[..n as usize]);
            continue;
        }
        if n == 0 {
            // A closed master reports EIO on Linux. A zero read is a hangup too:
            // with VMIN 1 it is not the "no data yet" result.
            return Ok((buf, true));
        }
        let err = io::Error::last_os_error();
        if err.kind() == ErrorKind::Interrupted {
            continue;
        }
        if err.kind() == ErrorKind::WouldBlock {
            return Ok((buf, false));
        }
        if err.raw_os_error() == Some(libc::EIO) {
            return Ok((buf, true));
        }
        return Err(err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn pipe() -> (RawFd, RawFd) {
        let mut fds = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        (fds[0], fds[1])
    }

    #[test]
    fn wait_sleeps_for_the_whole_timeout_when_nothing_happens() {
        let wake = Wake::new().unwrap();
        let (input, out) = pipe();
        let started = Instant::now();
        let ready = wait_on(input, &wake, Some(Duration::from_millis(120))).unwrap();
        let slept = started.elapsed();
        assert_eq!(ready, Ready::default());
        assert!(slept >= Duration::from_millis(100), "returned after {slept:?}");
        unsafe {
            libc::close(input);
            libc::close(out);
        }
    }

    #[test]
    fn a_wake_byte_returns_the_wait_and_drain_makes_it_block_again() {
        let wake = Wake::new().unwrap();
        let (input, out) = pipe();
        let waker = wake.waker();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            waker.wake();
        });
        // No timeout: only the wake can end this wait.
        let ready = wait_on(input, &wake, None).unwrap();
        handle.join().unwrap();
        assert_eq!(ready, Ready { input: false, wake: true });
        // Still undrained: the next wait returns at once.
        assert!(wait_on(input, &wake, Some(Duration::ZERO)).unwrap().wake);
        wake.drain();
        let started = Instant::now();
        let ready = wait_on(input, &wake, Some(Duration::from_millis(60))).unwrap();
        assert_eq!(ready, Ready::default());
        assert!(started.elapsed() >= Duration::from_millis(50));
        unsafe {
            libc::close(input);
            libc::close(out);
        }
    }

    #[test]
    fn input_is_reported_apart_from_wake() {
        let wake = Wake::new().unwrap();
        let (input, out) = pipe();
        assert_eq!(unsafe { libc::write(out, b"k".as_ptr() as *const _, 1) }, 1);
        let ready = wait_on(input, &wake, Some(Duration::from_millis(500))).unwrap();
        assert_eq!(ready, Ready { input: true, wake: false });
        unsafe {
            libc::close(input);
            libc::close(out);
        }
    }

    #[test]
    fn many_wakes_do_not_block_the_writer_and_one_drain_clears_them() {
        let wake = Wake::new().unwrap();
        let waker = wake.waker();
        // Far more bytes than a pipe holds: the extra writes fail with EAGAIN and are dropped.
        for _ in 0..200_000 {
            waker.wake();
        }
        let (input, out) = pipe();
        assert!(wait_on(input, &wake, Some(Duration::ZERO)).unwrap().wake);
        wake.drain();
        assert!(!wait_on(input, &wake, Some(Duration::ZERO)).unwrap().wake);
        unsafe {
            libc::close(input);
            libc::close(out);
        }
    }

    #[test]
    fn both_pipe_ends_are_nonblocking_and_close_on_exec() {
        let wake = Wake::new().unwrap();
        for fd in [wake.read, wake.write] {
            let fd_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert!(fd_flags & libc::FD_CLOEXEC != 0, "fd {fd} leaks across exec");
            let fl = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert!(fl & libc::O_NONBLOCK != 0, "fd {fd} would block");
        }
    }
}
