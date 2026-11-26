//! Presenter proof under a pseudo-terminal. No emulator and no window.
//!
//! The child is `loupe-term`. Its bytes go through the same virtual screen
//! the unit tests use, and that screen is compared cell for cell with
//! `loupe-core --frame-json`.
//!
//! Unix only: `openpty` and the controlling-terminal ioctl do not exist on Windows, where the
//! presenter is a stub for now. Waits use `pselect`, as the presenter does, because `poll` on a
//! terminal is unreliable on macOS. The ioctl numbers come from `libc` (their integer types
//! differ between Linux and Apple, hence `as _`).
#![cfg(unix)]

use std::fs::File;
use std::io::{self, ErrorKind};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use loupe_core::terminal::{Canvas, Cell};
use loupe_term::{LEAVE_TERM, Screen, cells_match};

const COLS: u16 = 233;
const ROWS: u16 = 55;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../checks/fixtures/demo.json")
}

fn core_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_loupe-term")).with_file_name("loupe-core")
}

fn term_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_loupe-term"))
}

struct Pty {
    master: RawFd,
    slave: RawFd,
    child: Option<Child>,
    buf: Vec<u8>,
    orig: libc::termios,
}

impl Pty {
    fn spawn(cols: u16, rows: u16, extra_env: &[(&str, &str)]) -> Self {
        let fixture = fixture();
        let args = ["--snapshot", fixture.to_str().unwrap()];
        Self::spawn_args(cols, rows, &args, extra_env, &[])
    }

    /// Any command line, with variables set and others taken out of the child's environment.
    fn spawn_args(cols: u16, rows: u16, args: &[&str], extra_env: &[(&str, &str)], remove: &[&str]) -> Self {
        let mut master = 0;
        let mut slave = 0;
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Linux declares the last two parameters as const pointers and macOS as mutable ones.
        // A mutable pointer is accepted for both.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        assert_eq!(rc, 0, "openpty: {}", io::Error::last_os_error());
        let mut orig: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(slave, &mut orig) },
            0,
            "tcgetattr: {}",
            io::Error::last_os_error()
        );
        for fd in [master, slave] {
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) };
        let mut cmd = Command::new(term_bin());
        cmd.args(args)
            .stdin(Stdio::from(dup_file(slave)))
            .stdout(Stdio::from(dup_file(slave)))
            .stderr(Stdio::from(dup_file(slave)))
            .env_remove("T3_TERM_PANIC")
            .env_remove("T3_EXPERIMENT_INTERACTION")
            .env_remove("T3_EXPERIMENT_METRICS")
            .env_remove("T3_PRESENT_TRACE");
        for key in remove {
            cmd.env_remove(key);
        }
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().unwrap_or_else(|e| panic!("spawn term: {e}"));
        Self {
            master,
            slave,
            child: Some(child),
            buf: Vec::new(),
            orig,
        }
    }

    fn pump(&mut self) -> bool {
        let mut tmp = [0u8; 8192];
        loop {
            let n = unsafe {
                libc::read(
                    self.master,
                    tmp.as_mut_ptr() as *mut libc::c_void,
                    tmp.len(),
                )
            };
            if n > 0 {
                self.buf.extend_from_slice(&tmp[..n as usize]);
                continue;
            }
            if n == 0 {
                return true;
            }
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == ErrorKind::WouldBlock {
                return false;
            }
            if err.raw_os_error() == Some(libc::EIO) {
                return true;
            }
            panic!("pty read: {err}");
        }
    }

    fn wait_syncs(&mut self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let eof = self.pump();
            if sync_ends(&self.buf) >= n {
                return;
            }
            if eof {
                break;
            }
            select_fd(self.master, false, 30);
        }
        let status = self
            .child
            .as_mut()
            .and_then(|c| c.try_wait().ok())
            .flatten();
        panic!(
            "timed out waiting for {n} frames (have {}), child {status:?}, output:\n{}",
            sync_ends(&self.buf),
            String::from_utf8_lossy(&self.buf)
        );
    }

    /// Reads until `needle` has appeared `count` times in everything read so far.
    fn wait_for(&mut self, needle: &[u8], count: usize) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let eof = self.pump();
            if self.buf.windows(needle.len()).filter(|w| *w == needle).count() >= count {
                return;
            }
            if eof {
                break;
            }
            select_fd(self.master, false, 30);
        }
        panic!(
            "timed out waiting for {count} of {:?}, output:\n{}",
            String::from_utf8_lossy(needle),
            String::from_utf8_lossy(&self.buf)
        );
    }

    fn send(&self, bytes: &[u8]) {
        let mut rest = bytes;
        let deadline = Instant::now() + Duration::from_secs(2);
        while !rest.is_empty() {
            let n = unsafe {
                libc::write(
                    self.master,
                    rest.as_ptr() as *const libc::c_void,
                    rest.len(),
                )
            };
            if n > 0 {
                rest = &rest[n as usize..];
                continue;
            }
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == ErrorKind::WouldBlock && Instant::now() < deadline {
                select_fd(self.master, true, 30);
                continue;
            }
            panic!("pty write: {err}");
        }
    }

    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.master, libc::TIOCSWINSZ as _, &size) };
        assert_eq!(rc, 0, "TIOCSWINSZ: {}", io::Error::last_os_error());
        let pid = self.child.as_ref().unwrap().id() as i32;
        unsafe { libc::kill(pid, libc::SIGWINCH) };
    }

    fn signal(&self, sig: i32) {
        let pid = self.child.as_ref().unwrap().id() as i32;
        assert_eq!(unsafe { libc::kill(pid, sig) }, 0);
    }

    /// Reap the child while draining the master. Reading first matters on macOS: the kernel
    /// holds an exiting session leader until its controlling terminal's output is read, so a
    /// blocking `wait` here would never return (see `wait_reaps_a_child_whose_last_bytes_are_unread`).
    fn wait(&mut self) -> std::process::ExitStatus {
        let mut child = self.child.take().unwrap();
        let limit = Instant::now() + Duration::from_secs(20);
        let status = loop {
            self.pump();
            if let Some(status) = child.try_wait().unwrap_or_else(|e| panic!("wait: {e}")) {
                break status;
            }
            if Instant::now() >= limit {
                let _ = child.kill();
                self.pump();
                panic!(
                    "child did not exit within 20s, output:\n{}",
                    String::from_utf8_lossy(&self.buf)
                );
            }
            select_fd(self.master, false, 20);
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.pump() {
                break;
            }
            select_fd(self.master, false, 20);
        }
        status
    }

    fn assert_restored(&self) {
        // Which descriptor shows the attributes the child left behind. Linux: the slave (its
        // master has termios of its own). macOS: when the session leader exits, the kernel
        // revokes its controlling terminal, so this side's slave descriptor answers ENOTTY; the
        // master still reaches the same tty there, and shows a raw mode left set.
        #[cfg(target_os = "macos")]
        let fd = self.master;
        #[cfg(not(target_os = "macos"))]
        let fd = self.slave;
        let mut now: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(fd, &mut now) },
            0,
            "tcgetattr: {}",
            io::Error::last_os_error()
        );
        assert_eq!(now.c_iflag, self.orig.c_iflag, "input flags not restored");
        assert_eq!(now.c_oflag, self.orig.c_oflag, "output flags not restored");
        assert_eq!(now.c_cflag, self.orig.c_cflag, "control flags not restored");
        assert_eq!(now.c_lflag, self.orig.c_lflag, "local flags not restored");
        let cooked = libc::ICANON | libc::ECHO | libc::ISIG;
        assert_eq!(
            now.c_lflag & cooked,
            self.orig.c_lflag & cooked,
            "cooked mode bits changed"
        );
        assert!(
            self.orig.c_lflag & cooked == cooked,
            "the pty did not start in cooked mode: {:#x}",
            self.orig.c_lflag
        );
        assert!(
            self.buf.windows(LEAVE_TERM.len()).any(|w| w == LEAVE_TERM),
            "leave sequence missing from output"
        );
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // Close the master before reaping: on macOS a killed child still waits in exit for its
        // unread output, and a closed master releases it.
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            unsafe { libc::close(self.master) };
            let _ = child.wait();
        } else {
            unsafe { libc::close(self.master) };
        }
        unsafe { libc::close(self.slave) };
    }
}

/// Wait up to `ms` for `fd` to be readable (or writable). `pselect`, never `poll`.
fn select_fd(fd: RawFd, for_write: bool, ms: i64) {
    let mut set: libc::fd_set = unsafe { std::mem::zeroed() };
    unsafe {
        libc::FD_ZERO(&mut set);
        libc::FD_SET(fd, &mut set);
    }
    let limit = libc::timespec {
        tv_sec: (ms / 1000) as libc::time_t,
        tv_nsec: ((ms % 1000) * 1_000_000) as _,
    };
    let (read, write) = if for_write {
        (std::ptr::null_mut(), &mut set as *mut libc::fd_set)
    } else {
        (&mut set as *mut libc::fd_set, std::ptr::null_mut())
    };
    unsafe { libc::pselect(fd + 1, read, write, std::ptr::null_mut(), &limit, std::ptr::null()) };
}

fn dup_file(fd: RawFd) -> File {
    let duped = unsafe { libc::dup(fd) };
    assert!(duped >= 0, "dup: {}", io::Error::last_os_error());
    unsafe { File::from_raw_fd(duped) }
}

const FRAME_END: &[u8] = b"\x1b[?2026l";

fn sync_ends(buf: &[u8]) -> usize {
    buf.windows(FRAME_END.len()).filter(|w| *w == FRAME_END).count()
}

fn nth_sync_end(buf: &[u8], n: usize) -> usize {
    let mut seen = 0;
    for (i, w) in buf.windows(FRAME_END.len()).enumerate() {
        if w == FRAME_END {
            seen += 1;
            if seen == n {
                return i + FRAME_END.len();
            }
        }
    }
    panic!("only {seen} frames in the stream");
}

fn frame_json(grid: &str, actions: Option<&str>) -> Canvas {
    let mut cmd = Command::new(core_bin());
    cmd.arg("--snapshot")
        .arg(fixture())
        .arg("--grid")
        .arg(grid)
        .arg("--frame-json");
    if let Some(actions) = actions {
        cmd.arg("--actions").arg(actions);
    }
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("spawn {}: {e}", core_bin().display()));
    assert!(
        out.status.success(),
        "loupe-core --frame-json failed ({:?}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("frame-json: {e}: {}", String::from_utf8_lossy(&out.stdout)));
    let w = value["w"].as_u64().unwrap() as usize;
    let h = value["h"].as_u64().unwrap() as usize;
    let ch = value["ch"].as_array().unwrap();
    let fg = value["fg"].as_array().unwrap();
    let bg = value["bg"].as_array().unwrap();
    let bold = value["at"].as_array().unwrap();
    let mut canvas = Canvas::new(w, h);
    assert_eq!(ch.len(), w * h);
    for i in 0..w * h {
        canvas.cells[i] = Cell {
            ch: char::from_u32(ch[i].as_u64().unwrap() as u32).unwrap(),
            fg: fg[i].as_u64().unwrap() as u32,
            bg: bg[i].as_u64().unwrap() as u32,
            bold: bold[i].as_u64().unwrap() != 0,
        };
    }
    canvas
}

fn show(screen: &Screen, canvas: &Canvas, what: &str) {
    cells_match(screen, canvas).unwrap_or_else(|e| panic!("{what}: {e}"));
}

fn sgr_press(x: i32, y: i32) -> Vec<u8> {
    format!("\x1b[<0;{};{}M", x + 1, y + 1).into_bytes()
}

fn sgr_wheel(x: i32, y: i32, down: bool) -> Vec<u8> {
    let button = if down { 65 } else { 64 };
    format!("\x1b[<{button};{};{}M", x + 1, y + 1).into_bytes()
}

fn layout() -> (loupe_core::ui::Surface, String) {
    let snap: loupe_core::model::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let mut state = loupe_core::model::State::new(snap);
    let (_, surface) =
        loupe_core::ui::draw_grid(&state, COLS as usize, ROWS as usize, &state.palette);
    let selected = state.selected.clone().unwrap_or_default();
    // A press on the selected row does not move. Pick another row the click can reach.
    let _ = &mut state;
    (surface, selected)
}

// These comparisons use the full-color cells from --frame-json.
const FRAME_TERMINAL: &[(&str, &str)] = &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

#[test]
fn keys_match_frame_json_and_resize_repaints() {
    let mut pty = Pty::spawn(COLS, ROWS, FRAME_TERMINAL);
    pty.wait_syncs(1);
    let quiet = pty.buf.len();
    std::thread::sleep(Duration::from_millis(200));
    pty.pump();
    assert_eq!(
        pty.buf.len(),
        quiet,
        "a calm fixed snapshot kept writing after the first frame"
    );
    let mut screen = Screen::new(COLS as usize, ROWS as usize);
    screen.apply(&pty.buf);
    show(&screen, &frame_json("233x55", None), "first frame");

    pty.send(b"j");
    pty.wait_syncs(2);
    screen.apply(&pty.buf[nth_sync_end(&pty.buf, 1)..]);
    show(&screen, &frame_json("233x55", Some("j")), "after j");

    let split = pty.buf.len();
    pty.resize(80, 24);
    pty.wait_syncs(3);
    let mut resized = Screen::new(80, 24);
    resized.apply(&pty.buf[split..]);
    show(&resized, &frame_json("80x24", Some("j")), "after SIGWINCH");

    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "quit status {status}");
    pty.assert_restored();
}

#[test]
fn mouse_press_and_wheel_reach_the_model() {
    let (surface, selected) = layout();
    let hit = surface
        .hits
        .iter()
        .rev()
        .find(|hit| {
            !hit.earlier
                && hit.rect.w > 0
                && hit.id != selected
                && !surface.needs_you.contains(hit.rect.x, hit.rect.y)
        })
        .unwrap_or_else(|| panic!("demo fixture has no clickable row besides {selected}"));
    let press_at = (hit.rect.x, hit.rect.y);
    let pick = format!("pick:{}", hit.id);

    let mut pty = Pty::spawn(COLS, ROWS, FRAME_TERMINAL);
    pty.wait_syncs(1);
    pty.send(&sgr_press(press_at.0, press_at.1));
    pty.wait_syncs(2);
    let mut screen = Screen::new(COLS as usize, ROWS as usize);
    screen.apply(&pty.buf);
    show(&screen, &frame_json("233x55", Some(&pick)), "mouse press");
    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "{status}");
    pty.assert_restored();

    let mut outside = (0, 0);
    if surface.detail.contains(0, 0) {
        'scan: for y in 0..ROWS as i32 {
            for x in 0..COLS as i32 {
                if !surface.detail.contains(x, y) {
                    outside = (x, y);
                    break 'scan;
                }
            }
        }
    }
    let mut pty = Pty::spawn(COLS, ROWS, FRAME_TERMINAL);
    pty.wait_syncs(1);
    pty.send(&sgr_wheel(outside.0, outside.1, true));
    pty.wait_syncs(2);
    let mut screen = Screen::new(COLS as usize, ROWS as usize);
    screen.apply(&pty.buf);
    show(&screen, &frame_json("233x55", Some("down")), "wheel down");
    pty.send(b"q");
    assert!(pty.wait().success());

    // Help covers the window. At 233×55 the page fits, so this uses a shorter
    // grid where three rows of wheel scroll actually move the text.
    let (hcols, hrows) = help_size();
    let mut pty = Pty::spawn(hcols, hrows, FRAME_TERMINAL);
    pty.wait_syncs(1);
    pty.send(b"?");
    pty.wait_syncs(2);
    let mut opened = Screen::new(hcols as usize, hrows as usize);
    opened.apply(&pty.buf);
    show(
        &opened,
        &frame_json(&format!("{hcols}x{hrows}"), Some("?")),
        "help opened",
    );
    pty.send(&sgr_wheel(1, 1, true));
    pty.wait_syncs(3);
    let mut screen = Screen::new(hcols as usize, hrows as usize);
    screen.apply(&pty.buf);
    show(
        &screen,
        &help_scrolled(hcols as usize, hrows as usize),
        "help wheel",
    );
    pty.send(b"q");
    assert!(pty.wait().success());

    // The demo's detail fits at 233×55. A shorter window makes it scroll.
    let (cols, rows, dx, dy) = detail_point();
    let mut pty = Pty::spawn(cols, rows, FRAME_TERMINAL);
    pty.wait_syncs(1);
    let before = pty.buf.len();
    pty.send(&sgr_wheel(dx, dy, true));
    pty.wait_syncs(2);
    assert!(pty.buf.len() > before, "detail wheel wrote no bytes");
    let mut screen = Screen::new(cols as usize, rows as usize);
    screen.apply(&pty.buf);
    show(
        &screen,
        &scrolled_detail(cols as usize, rows as usize, 3),
        "detail wheel",
    );
    pty.send(b"q");
    assert!(pty.wait().success());
}

fn demo_state() -> loupe_core::model::State {
    let snap: loupe_core::model::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    loupe_core::model::State::new(snap)
}

fn detail_point() -> (u16, u16, i32, i32) {
    for rows in [55u16, 40, 30, 24, 18, 12] {
        let mut state = demo_state();
        let (_, surface) =
            loupe_core::ui::draw_grid(&state, 233, rows as usize, &state.palette);
        if surface.detail_max > 0 && surface.detail.w > 0 && surface.detail.h > 0 {
            let x = surface.detail.x + surface.detail.w / 2;
            let y = surface.detail.y;
            assert!(surface.detail.contains(x, y));
            let _ = &mut state;
            return (233, rows, x, y);
        }
    }
    panic!("no demo grid makes the detail panel scroll");
}

/// The same transition `on_wheel` applies over the detail rect: three rows,
/// clamped to the surface the first paint published.
fn scrolled_detail(cols: usize, rows: usize, by: usize) -> Canvas {
    let mut state = demo_state();
    let (_, surface) = loupe_core::ui::draw_grid(&state, cols, rows, &state.palette);
    state.detail_scroll = (state.detail_scroll + by).min(surface.detail_max);
    let (canvas, _) = loupe_core::ui::draw_grid(&state, cols, rows, &state.palette);
    canvas
}

fn help_size() -> (u16, u16) {
    for rows in [24u16, 20, 16, 12] {
        let mut state = demo_state();
        state.action("?");
        let (_, surface) = loupe_core::ui::draw_grid(&state, 80, rows as usize, &state.palette);
        if surface.help_max >= 3 {
            return (80, rows);
        }
    }
    panic!("help page fits every tried grid");
}

fn help_scrolled(cols: usize, rows: usize) -> Canvas {
    let mut state = demo_state();
    state.action("?");
    let (_, surface) = loupe_core::ui::draw_grid(&state, cols, rows, &state.palette);
    assert!(surface.help_max > 0, "help has nothing to scroll");
    state.help_scroll = 3.min(surface.help_max);
    let (canvas, _) = loupe_core::ui::draw_grid(&state, cols, rows, &state.palette);
    canvas
}

#[test]
fn quit_signal_and_panic_restore_the_terminal() {
    let mut pty = Pty::spawn(COLS, ROWS, &[]);
    pty.wait_syncs(1);
    pty.send(&[0x03]);
    let status = pty.wait();
    assert!(status.success(), "ctrl-c status {status}");
    pty.assert_restored();

    let mut pty = Pty::spawn(COLS, ROWS, &[]);
    pty.wait_syncs(1);
    pty.signal(libc::SIGTERM);
    let status = pty.wait();
    assert!(!status.success() || status.code() == Some(0) || status.code().is_none());
    // SIGTERM is caught and unwinds through the same restore as a normal quit,
    // so the process exits 0. A killed process would not have written LEAVE_TERM.
    assert!(
        status.success(),
        "SIGTERM should be caught and exit cleanly, got {status}"
    );
    pty.assert_restored();

    let mut pty = Pty::spawn(COLS, ROWS, &[]);
    pty.wait_syncs(1);
    pty.signal(libc::SIGINT);
    let status = pty.wait();
    assert!(status.success(), "SIGINT status {status}");
    pty.assert_restored();

    let mut pty = Pty::spawn(COLS, ROWS, &[("T3_TERM_PANIC", "1")]);
    let status = pty.wait();
    assert_eq!(status.code(), Some(101), "panic status {status}");
    pty.assert_restored();
}

/// `Pty::wait` must reap a child whose last bytes nobody has read yet. On macOS the kernel holds
/// an exiting session leader until its controlling terminal's output has drained, so a harness
/// that reaps before it reads waits for the child while the child waits for the harness (Linux
/// does not drain there, which is why this only ever hung on a Mac). The panicking presenter
/// writes its enter and leave sequences and exits at once, before this side reads anything.
/// A rescue thread drains the master after a few seconds so a regression fails here instead of
/// hanging the run.
#[test]
fn wait_reaps_a_child_whose_last_bytes_are_unread() {
    let mut pty = Pty::spawn(COLS, ROWS, &[("T3_TERM_PANIC", "1")]);
    let master = pty.master;
    let (done, wait_done) = std::sync::mpsc::channel::<()>();
    let rescue = std::thread::spawn(move || {
        if wait_done.recv_timeout(Duration::from_secs(5)).is_ok() {
            return false;
        }
        let mut tmp = [0u8; 8192];
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            let n = unsafe { libc::read(master, tmp.as_mut_ptr() as *mut libc::c_void, tmp.len()) };
            if n <= 0 {
                select_fd(master, false, 20);
            }
        }
        true
    });
    let started = Instant::now();
    let status = pty.wait();
    let _ = done.send(());
    let rescued = rescue.join().unwrap();
    assert!(
        !rescued,
        "Pty::wait did not reap the child until another thread read the pty ({:?})",
        started.elapsed()
    );
    assert_eq!(status.code(), Some(101), "panic status {status}");
    pty.assert_restored();
}

/// CPU time a presenter used from launch to exit, from `wait4` on that one child. The child is
/// reaped here, so it never touches `RUSAGE_CHILDREN`, which other tests share.
fn cpu_used(idle: Duration) -> Duration {
    let mut pty = Pty::spawn(COLS, ROWS, &[]);
    pty.wait_syncs(1);
    std::thread::sleep(idle);
    pty.send(b"q");
    let child = pty.child.take().unwrap();
    let pid = child.id() as libc::pid_t;
    // Reap without blocking and drain the pty meanwhile: on macOS the presenter cannot finish
    // exiting until its leave sequence is read (see `Pty::wait`). The limit makes a presenter that
    // never quits fail the test instead of hanging it.
    let limit = Instant::now() + Duration::from_secs(20);
    let mut status = 0;
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = loop {
        pty.pump();
        let rc = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut usage) };
        if rc != 0 {
            break rc;
        }
        if Instant::now() >= limit {
            unsafe { libc::kill(pid, libc::SIGKILL) };
            pty.pump();
            panic!("presenter did not quit within 20s of q");
        }
        select_fd(pty.master, false, 20);
    };
    assert_eq!(rc, pid, "wait4: {}", io::Error::last_os_error());
    assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0, "exit status {status:#x}");
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, t.tv_usec as u32 * 1000);
    tv(usage.ru_utime) + tv(usage.ru_stime)
}

/// The presenter on a calm fixed snapshot sleeps in its wait. If the wait spun (the `poll`
/// trap on a macOS terminal), a second and a half of idling would add about a second and a half
/// of CPU. This compares a run that quits at once with one that sits idle first.
#[test]
fn an_idle_presenter_sleeps_instead_of_spinning() {
    let baseline = cpu_used(Duration::ZERO);
    let idle = Duration::from_millis(1500);
    let idled = cpu_used(idle);
    eprintln!("cpu: quit at once {baseline:?}, after {idle:?} idle {idled:?}");
    assert!(
        idled <= baseline + Duration::from_millis(300),
        "{idle:?} of idling cost {:?} more CPU than quitting at once ({baseline:?} -> {idled:?})",
        idled.saturating_sub(baseline)
    );
}

/// A folder for one test's home, removed when dropped.
struct Home(PathBuf);
impl Home {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("loupe-pty-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("t3")).unwrap();
        Self(path)
    }
}
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The variables that could lead the child to a real theme or config file.
const OUTSIDE: &[&str] = &["XDG_CONFIG_HOME", "OMARCHY_PATH", "LOUPE_THEME", "LOUPE_NO_THEMES", "TMUX", "STY"];

fn background(rgb: u32) -> Vec<u8> {
    format!("48;2;{};{};{}", rgb >> 16, (rgb >> 8) & 255, rgb & 255).into_bytes()
}

/// With no Omarchy and nothing chosen, the dashboard asks the terminal for its background and
/// draws Paper on a light one and Frontier on a dark one, and follows a color scheme change. This
/// is a deliberate difference from the original, which draws its gray fallback here; under
/// `LOUPE_NO_THEMES` it asks nothing (below).
#[test]
fn the_default_theme_follows_the_terminals_light_or_dark_background() {
    let home = Home::new("mode");
    let h = home.0.to_str().unwrap();
    let t3 = home.0.join("t3");
    let env = [("HOME", h), ("T3CODE_HOME", t3.to_str().unwrap()), ("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("TZ", "UTC")];
    let mut pty = Pty::spawn_args(160, 45, &["--demo", "--calm"], &env, OUTSIDE);
    pty.wait_for(b"\x1b[?2031h\x1b]11;?\x07", 1);
    pty.send(b"\x1b]11;rgb:fafa/fafa/fafa\x1b\\\x1b[?62;22c");
    let paper = 0xf4f1e8;
    pty.wait_for(&background(paper), 1);
    // The terminal says its scheme changed: the dashboard asks again and follows the answer.
    pty.send(b"\x1b[?997;1n");
    pty.wait_for(b"\x1b]11;?\x07", 2);
    pty.send(b"\x1b]11;rgb:1a1a/1b1b/2626\x07");
    pty.wait_for(&background(0x0a0a0b), 1);
    // c steps to the next theme for this mode, and remembers it.
    pty.send(b"c");
    pty.wait_for(b"Dark theme: Paper", 1);
    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "{status:?}");
    assert!(pty.buf.windows(8).any(|w| w == b"\x1b[?2031l"), "color scheme reports are switched off on the way out");
    let config = std::fs::read_to_string(home.0.join(".config/loupe/config.toml")).unwrap();
    assert_eq!(config, "theme = \"dark:paper,light:paper\"\n");
}

/// The original's behavior, which the parity checks compare: no background query, no theme key.
#[test]
fn without_themes_nothing_is_asked_and_c_is_not_a_key() {
    let home = Home::new("off");
    let h = home.0.to_str().unwrap();
    let t3 = home.0.join("t3");
    let env = [("HOME", h), ("T3CODE_HOME", t3.to_str().unwrap()), ("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("LOUPE_NO_THEMES", "1")];
    let mut pty = Pty::spawn_args(160, 45, &["--demo", "--calm"], &env, &OUTSIDE[..3]);
    pty.wait_for(b"\x1b[c", 1);
    pty.send(b"\x1b[?62;22c");
    pty.wait_for(b"Loupe", 1);
    pty.send(b"c");
    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "{status:?}");
    let out = String::from_utf8_lossy(&pty.buf);
    assert!(!out.contains("\x1b[?2031h") && !out.contains("\x1b]11;?"), "{out}");
    assert!(!out.contains("theme:"), "{out}");
    assert!(!home.0.join(".config/loupe").exists());
}

#[test]
fn storage_demo_opens_rescans_and_returns_through_the_product_entry() {
    let home = Home::new("storage");
    let t3 = home.0.join("t3");
    let env = [
        ("HOME", home.0.to_str().unwrap()),
        ("T3CODE_HOME", t3.to_str().unwrap()),
        ("TERM", "dumb"),
        ("T3_FIXED_NOW", "1795026000000"),
    ];
    let mut pty = Pty::spawn_args(
        80,
        28,
        &["--demo", "--calm", "--theme", "frontier"],
        &env,
        OUTSIDE,
    );
    pty.wait_syncs(1);
    pty.send(b"?");
    pty.wait_for(b"storage: disk use by project and thread", 1);
    pty.send(b"s");
    pty.wait_for(b"folders \xc2\xb7 scan", 1);
    // Made-up settings: every auto-delete setting off, one project with its own rule.
    pty.wait_for(b"T3 CLEANUP", 1);
    pty.wait_for(b"off \xc2\xb7 no auto-delete on", 1);
    // `c` opens the cleanup settings panel instead of changing the theme, and Esc returns.
    pty.send(b"c");
    pty.wait_for(b"DELETE WORKTREES", 1);
    pty.wait_for(b"oldest 12d", 1);
    pty.send(b"j");
    pty.wait_for(b"no activity.", 1);
    pty.send(b"\x1b");
    pty.wait_for(b"folders \xc2\xb7 scan", 2);
    pty.send(b"r");
    pty.wait_for(b"folders \xc2\xb7 scan", 3);
    pty.send(b"h");
    pty.wait_for(b"STORAGE HELP", 1);
    pty.send(b"\x1b");
    pty.wait_for(b"folders \xc2\xb7 scan", 4);
    pty.send(b"\x1b");
    pty.wait_for(b"AGENTS", 2);
    pty.send(b"sq");
    assert!(pty.wait().success());
    pty.assert_restored();
}

#[test]
fn storage_reopen_keeps_the_last_measurement_until_rescan() {
    let home = Home::new("storage-reopen");
    let t3 = home.0.join("t3");
    let env = [
        ("HOME", home.0.to_str().unwrap()),
        ("T3CODE_HOME", t3.to_str().unwrap()),
        ("TERM", "dumb"),
        ("T3_FIXED_NOW", "1795026000000"),
    ];
    let mut pty = Pty::spawn_args(
        80,
        28,
        &["--demo", "--calm", "--theme", "frontier"],
        &env,
        OUTSIDE,
    );
    pty.wait_syncs(1);
    pty.send(b"s");
    // The diff can split the finished word. The same prefix the product-entry
    // test uses is contiguous and does not match "scanning…" or "last scan".
    pty.wait_for(b"folders \xc2\xb7 scan", 1);
    pty.send(b"\x1b");
    pty.wait_for(b"AGENTS", 2);
    pty.send(b"s");
    pty.wait_for(b"last scan", 1);
    // A PTY read can end after the heading but before the measured rows.
    let heading = pty.buf.windows(9).position(|w| w == b"last scan").unwrap();
    pty.wait_syncs(sync_ends(&pty.buf[..heading]) + 1);
    let rest = String::from_utf8_lossy(&pty.buf[heading..]);
    assert!(rest.contains("MB"), "{rest}");
    pty.send(b"r");
    pty.wait_for(b"folders \xc2\xb7 scan", 2);
    pty.send(b"q");
    assert!(pty.wait().success());
    pty.assert_restored();
}

/// A PTY read can stop mid-frame, even in the closing escape. A terminal presents the
/// synchronized frame only once that escape is complete. Keep the suffix for the next read.
fn apply_complete_frames(screen: &mut Screen, buf: &[u8]) -> usize {
    let Some(last) = buf.windows(FRAME_END.len()).rposition(|w| w == FRAME_END) else {
        return 0;
    };
    let end = last + FRAME_END.len();
    screen.apply(&buf[..end]);
    end
}

#[test]
fn split_pty_reads_publish_only_complete_frames() {
    let live = b"\x1b[?2026hLive\x1b[?2026l";
    let help = b"\x1b[?2026h\x1b[1;1HHelp\x1b[?2026l";
    let stream = [live.as_slice(), help.as_slice()].concat();
    let row = |screen: &Screen| {
        screen.cells.iter().map(|cell| cell.text.as_str()).collect::<String>()
    };

    for split in 0..=stream.len() {
        let mut screen = Screen::new(4, 1);
        let fed = apply_complete_frames(&mut screen, &stream[..split]);
        let expected = if split < live.len() {
            "    "
        } else if split < stream.len() {
            "Live"
        } else {
            "Help"
        };
        assert_eq!(row(&screen), expected, "first read ends at byte {split}");
        assert_eq!(fed + apply_complete_frames(&mut screen, &stream[fed..]), stream.len());
        assert_eq!(row(&screen), "Help");
    }
}

/// The demo with every product view, checked against the completed frames on screen.
struct Shown {
    pty: Pty,
    screen: Screen,
    fed: usize,
    _home: Home,
}

impl Shown {
    fn open(tag: &str, cols: u16, rows: u16) -> Self {
        Self::with_args(Home::new(tag), cols, rows, &["--demo", "--calm", "--theme", "frontier"])
    }

    fn with_args(home: Home, cols: u16, rows: u16, args: &[&str]) -> Self {
        let t3 = home.0.join("t3");
        let env = [
            ("HOME", home.0.to_str().unwrap()),
            ("T3CODE_HOME", t3.to_str().unwrap()),
            ("TERM", "dumb"),
            ("TZ", "America/Los_Angeles"),
            ("T3_FIXED_NOW", "1795026000000"),
            ("LOUPE_NO_CACHE", "1"),
            ("HYPRLAND_INSTANCE_SIGNATURE", ""), // No desktop focus watcher for synthetic databases.
        ];
        let pty = Pty::spawn_args(cols, rows, args, &env, OUTSIDE);
        Self { pty, screen: Screen::new(cols as usize, rows as usize), fed: 0, _home: home }
    }

    fn feed(&mut self) -> bool {
        let eof = self.pty.pump();
        self.fed += apply_complete_frames(&mut self.screen, &self.pty.buf[self.fed..]);
        eof
    }

    fn rows(&self) -> Vec<String> {
        self.screen.cells.chunks(self.screen.w).map(|row| row.iter().map(|c| c.text.as_str()).collect()).collect()
    }

    fn dump(&self) -> String {
        self.rows().join("\n")
    }

    /// Wait until the screen shows `what`.
    fn until(&mut self, what: &str, ok: impl Fn(&[String]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            let eof = self.feed();
            if ok(&self.rows()) {
                return;
            }
            assert!(!eof, "the program ended before {what}; screen:\n{}", self.dump());
            select_fd(self.pty.master, false, 30);
        }
        panic!("{}x{}: timed out waiting for {what}; screen:\n{}", self.screen.w, self.screen.h, self.dump());
    }

    /// Read until nothing more arrives for a moment, then check the screen still shows `what`:
    /// for keys a view ignores, which draw nothing.
    fn still(&mut self, what: &str, ok: impl Fn(&[String]) -> bool) {
        let mut quiet = Instant::now() + Duration::from_millis(250);
        while Instant::now() < quiet {
            let before = self.pty.buf.len();
            assert!(!self.feed(), "the program ended; expected {what}");
            if self.pty.buf.len() != before {
                quiet = Instant::now() + Duration::from_millis(250);
            }
            select_fd(self.pty.master, false, 20);
        }
        assert!(ok(&self.rows()), "{}x{}: expected {what} to stay; screen:\n{}", self.screen.w, self.screen.h, self.dump());
    }

    fn key(&mut self, bytes: &[u8], what: &str, ok: impl Fn(&[String]) -> bool) {
        self.pty.send(bytes);
        self.until(what, ok);
    }

    /// Read successive completed frames even if the PTY coalesces loading and readiness.
    fn key_frames(&mut self, bytes: &[u8], count: usize) -> Vec<Vec<String>> {
        let before = sync_ends(&self.pty.buf[..self.fed]);
        self.pty.send(bytes);
        self.pty.wait_syncs(before + count);
        (before + 1..=before + count).map(|n| {
            let end = nth_sync_end(&self.pty.buf, n);
            self.screen.apply(&self.pty.buf[self.fed..end]);
            self.fed = end;
            self.rows()
        }).collect()
    }

    /// Resize after the screen has settled; the program repaints everything at the new size.
    fn resize(&mut self, cols: u16, rows: u16, what: &str, ok: impl Fn(&[String]) -> bool) {
        self.still("the screen before a resize", |_| true);
        self.pty.resize(cols, rows);
        self.screen.resize(cols as usize, rows as usize);
        self.until(what, ok);
    }

    fn quit(mut self, key: &[u8]) {
        self.pty.send(key);
        let status = self.pty.wait();
        assert!(status.success(), "quit status {status}");
        self.pty.assert_restored();
    }
}

fn header(rows: &[String]) -> &str {
    rows.first().map_or("", String::as_str)
}
fn footer(rows: &[String]) -> &str {
    rows.iter().rev().find(|r| !r.trim().is_empty()).map_or("", String::as_str)
}
fn on_dashboard(rows: &[String]) -> bool {
    header(rows).contains("DEMO")
        && !header(rows).contains("STORAGE")
        && rows.iter().any(|r| r.starts_with(" AGENTS"))
        && !rows.iter().any(|r| r.contains("Loupe · help"))
        && footer(rows).contains("demo data · made up")
}
fn on_help(rows: &[String]) -> bool {
    rows.iter().any(|r| r.contains("╭─ Loupe · help")) && !header(rows).contains("TRACE")
}
fn on_storage(rows: &[String]) -> bool {
    header(rows).contains("STORAGE") && rows.iter().any(|r| r.starts_with(" BY PROJECT")) && footer(rows).contains("s back")
}
fn on_storage_help(rows: &[String]) -> bool {
    rows.iter().any(|r| r.starts_with(" STORAGE HELP")) && footer(rows).contains("s back")
}
fn on_trace_help(rows: &[String]) -> bool {
    header(rows).contains("TRACE") && rows.iter().any(|r| r.contains("TRACE HELP"))
        && rows.iter().any(|r| r.contains("task")) && rows.iter().any(|r| r.contains("delegate"))
        && footer(rows).split_whitespace().eq(["q", "quit"])
}
fn on_trace(rows: &[String]) -> bool {
    header(rows).contains("TRACE") && header(rows).contains("made up")
        && !rows.iter().any(|r| r.contains("TRACE HELP")) && footer(rows).contains("w back")
}
#[test]
fn partial_trace_help_is_not_a_completed_navigation_frame() {
    let rows = ["Loupe  TRACE  Example · made up", "TRACE HELP", "previous task", "open a delegate", "w back"]
        .map(str::to_owned);
    assert!(!on_trace(&rows));
    assert!(!on_trace_help(&rows));
}

/// A size notice, whole: the full minimum with its full stop, so a clipped "60 × 1" fails.
fn notice(rows: &[String], title: &str, size: &str, keys: &str) -> bool {
    header(rows).trim_end() == format!(" {title}")
        && rows.get(2).is_some_and(|r| r.trim() == format!("Enlarge the terminal to at least {size}."))
        && rows.get(3).is_some_and(|r| r.trim() == keys)
}
fn trace_small(rows: &[String]) -> bool {
    notice(rows, "Loupe", "60 × 16", "w back   q quit")
}

#[test]
fn demo_trace_shows_the_selected_waiting_task() {
    let mut v = Shown::open("demo-row-trace", 120, 40);
    v.until("dashboard", on_dashboard);
    v.key(b"w", "search trace with its pending question", |rows| {
        on_trace(rows)
            && header(rows).contains("Plan the search index rebuild")
            && header(rows).contains("needs you 27m")
            && rows[1].contains("opus 5.5")
            && rows.iter().any(|r| r.contains("question"))
    });
    v.quit(b"q");
}

#[test]
fn demo_trace_shows_a_different_selected_running_task() {
    let mut v = Shown::open("demo-running-trace", 120, 40);
    v.until("dashboard", on_dashboard);
    v.pty.send(b"jj");
    v.key(b"w", "streaming export trace", |rows| {
        on_trace(rows)
            && header(rows).contains("Add streaming export to the CLI")
            && header(rows).contains("running 29m")
            && rows[1].contains("grok-4.7")
    });
    v.quit(b"q");
}

#[test]
fn demo_trace_shows_no_history_for_a_delegate_without_its_own_run() {
    let mut v = Shown::open("demo-unavailable-trace", 120, 40);
    v.until("dashboard", on_dashboard);
    v.pty.send(b"j");
    v.key(b"w", "no task history for Survey index sizes", |rows| {
        rows.iter().any(|r| r.trim() == "Nothing to trace")
    });
    v.key(b"w", "dashboard", on_dashboard);
    v.quit(b"q");
}

#[test]
fn storage_demo_shows_sample_headroom_at_narrow_and_wide_sizes() {
    let mut v = Shown::open("storage-headroom", 55, 24);
    v.until("dashboard", on_dashboard);
    v.key(b"s", "sample headroom", |rows| {
        on_storage(rows)
            && rows.iter().any(|r| r.contains("demo data"))
            && rows.iter().any(|r| r.contains("free of"))
    });
    v.resize(120, 40, "sample headroom wide", |rows| {
        rows.iter().any(|r| r.contains("demo data"))
            && rows.iter().any(|r| r.contains("free of"))
            && !rows.iter().any(|r| r.contains("from the file system"))
    });
    v.quit(b"q");
}

/// Dashboard, help, Storage and Trace reach each other and come back by their keys at 55 × 24,
/// 60 × 20, 80 × 24 and 120 × 40, with each view's footer. At 55 columns Trace does not fit,
/// and its notice must still go back with Esc or w rather than leave the user only `q`.
#[test]
fn combined_navigation_returns_to_each_view_at_every_size() {
    for (cols, rows) in [(80u16, 24u16), (55, 24), (60, 20), (120, 40)] {
        let fits = cols >= 60 && rows >= 16;
        let traced = move |r: &[String]| if fits { on_trace(r) } else { trace_small(r) };
        let mut v = Shown::open(&format!("nav-{cols}x{rows}"), cols, rows);
        v.until("the dashboard", on_dashboard);

        v.key(b"?", "help", on_help);
        v.key(b"\x1b", "Esc closing help, not quitting", on_dashboard);

        v.key(b"s", "Storage", on_storage);
        v.key(b"?", "Storage's help", on_storage_help);
        v.key(b"\x1b", "Esc closing Storage's help", on_storage);
        v.key(b"\x1b", "Esc back to the dashboard", on_dashboard);
        v.key(b"?", "help", on_help);
        v.key(b"s", "s from help opening Storage with help closed", on_storage);
        v.key(b"s", "s back to the dashboard", on_dashboard);

        v.key(b"w", "Trace", traced);
        v.key(b"?", "help over Trace", |r| {
            if fits {
                on_trace_help(r)
            } else {
                r.iter().any(|line| line.contains("TRACE HELP"))
                    && r.iter().any(|line| line.contains("Esc") && line.contains("close help"))
            }
        });
        v.key(b"\x1b", "Esc closing help back to Trace", traced);
        v.pty.send(b"s");
        v.still("Trace: s is not a key there", traced);
        v.key(b"w", "w back to the dashboard (not Storage)", on_dashboard);
        v.key(b"w", "Trace again", traced);
        v.key(b"\x1b", "Esc back to the dashboard", on_dashboard);

        v.key(b"w", "Trace before quitting", traced);
        v.quit(b"q");
    }
}

/// The size floors, crossed in place: the dashboard and Storage need 48 × 14, Trace 60 × 16.
/// Each view repaints whole on the way down and on the way back, keeps its place, and its
/// notice says the full minimum and the way back.
#[test]
fn view_size_floors_hold_across_resizes() {
    let dash_small = |r: &[String]| notice(r, "Loupe", "48 × 14", "q quit");
    let storage_small = |r: &[String]| notice(r, "Loupe · Storage", "48 × 14", "s back   q quit");
    let mut v = Shown::open("floors", 60, 16);
    v.until("the dashboard at 60 × 16", on_dashboard);
    v.key(b"w", "Trace at 60 × 16", on_trace);
    v.resize(59, 16, "Trace's notice at 59 × 16", trace_small);
    v.resize(60, 16, "Trace again at 60 × 16", on_trace);
    v.resize(60, 15, "Trace's notice at 60 × 15", trace_small);
    v.key(b"\x1b", "Esc from Trace's notice to the dashboard at 60 × 15", on_dashboard);

    v.key(b"s", "Storage at 60 × 15", on_storage);
    v.resize(48, 14, "Storage at 48 × 14", on_storage);
    v.resize(47, 14, "Storage's notice at 47 × 14", storage_small);
    v.resize(48, 13, "Storage's notice at 48 × 13", storage_small);
    v.key(b"s", "s from Storage's notice to the dashboard's", dash_small);
    v.resize(48, 14, "the dashboard at 48 × 14", on_dashboard);
    v.resize(47, 14, "the dashboard's notice at 47 × 14", dash_small);
    v.key(b"w", "Trace's notice opened below its floor", trace_small);
    v.resize(120, 40, "Trace at 120 × 40", on_trace);
    v.key(b"w", "the dashboard at 120 × 40", on_dashboard);
    v.quit(b"q");
}

// Deliberate product differences from the oracle: contextual return keys and complete minima.
#[test]
fn contextual_notices_and_demo_storage_are_honest() {
    let mut v = Shown::open("context-notices", 120, 40);
    v.until("dashboard", on_dashboard);
    v.key(b"s", "demo Storage", |r| on_storage(r) && !r.iter().any(|line| line.contains("from the file system")));
    v.resize(30, 14, "narrow Storage minimum", |r| r.iter().any(|line| line.contains("48 × 14")));
    v.key(b"s", "narrow dashboard", |r| r.iter().any(|line| line.contains("48 × 14")));
    v.key(b"w", "narrow Trace minimum", |r| r.iter().any(|line| line.contains("60 × 16")) && r.iter().any(|line| line.contains("w back")));
    v.resize(10, 14, "very narrow Trace notice", |r| r.get(2).is_some_and(|line| !line.chars().any(|ch| ch.is_ascii_digit())));
    v.quit(b"q");
}

fn insights_ready(rows: &[String]) -> bool {
    header(rows).contains("RECAP")
        && header(rows).contains("TOOLS")
        && footer(rows).contains("i back")
        && footer(rows).contains("made up")
        && !rows.iter().any(|row| row.contains("INSIGHTS HELP"))
}

#[test]
fn insights_demo_loads_refreshes_and_returns_to_the_same_board() {
    let mut v = Shown::open("insights", 55, 24);
    v.until("Live", on_dashboard);
    v.key(b"j", "a different board selection", on_dashboard);
    v.still("the selected Live row", on_dashboard);
    let live = v.rows();

    v.key(b"?", "Insights in 55-column Live help", |rows| {
        on_help(rows) && rows.iter().any(|row| row.contains("insights: Recap and Tools"))
    });
    let entry = v.key_frames(b"i", 2);
    assert!(entry[0].iter().any(|row| row.contains("Making up Recap for this week, started")));
    assert!(insights_ready(&entry[1]));

    v.key(b"\x1b[C", "last week", |rows| insights_ready(rows) && rows[1].contains("last week"));
    let tools = v.key_frames(b"\t", 2);
    assert!(tools[0].iter().any(|row| row.contains("Making up Tools for last week")));
    assert!(insights_ready(&tools[1]) && tools[1][1].contains("last week"));

    v.key(b"\x1b[F", "the end of Tools", |rows| {
        insights_ready(rows) && rows.iter().any(|row| row.contains("above") && !row.contains("more"))
    });
    v.key(b"\x1b[<64;20;12M", "wheel scrolling up", |rows| {
        insights_ready(rows) && rows.iter().any(|row| row.contains("above") && row.contains("more"))
    });
    let before = v.rows();
    let refresh = v.key_frames(b"r", 2);
    assert!(footer(&refresh[0]).contains("making up"));
    assert_eq!(refresh[0][2..23], before[2..23], "refresh keeps the visible report");
    assert_eq!(refresh[1], before, "fixed-clock refresh restores the ready frame");

    v.key(b"\x1b[Z", "Shift-Tab returning to Recap", |rows| {
        insights_ready(rows) && rows[1].contains("last week")
            && !rows.iter().any(|row| row.contains("above"))
    });
    v.key(b"?", "Insights help at 55 columns", |rows| {
        rows.iter().any(|row| row.contains("INSIGHTS HELP"))
            && rows.iter().any(|row| row.contains("Esc closes help first"))
    });
    v.key(b"\x1b", "Esc closing Insights help", insights_ready);
    v.key(b"\x1b", "Esc returning to the same Live selection", |rows| rows == live);

    v.key(b"i", "reopening at Recap this week", |rows| insights_ready(rows) && rows[1].contains("this week"));
    v.resize(40, 12, "Insights size notice", |rows| {
        rows.iter().any(|row| row.contains("Loupe · Insights"))
            && rows.iter().any(|row| row.contains("i back"))
    });
    v.key(b"i", "back to the tiny Live board", |rows| !header(rows).contains("Insights"));
    v.key(b"i", "reopening below the minimum", |rows| header(rows).contains("Insights"));
    v.quit(b"q");
}

#[test]
fn insights_fallback_labels_the_data_without_moving_the_period_cursor() {
    let home = Home::new("insights-fallback");
    let path = home.0.join("reports.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../../../checks/fixtures/t3-schema.sql")).unwrap();
    db.execute_batch(
        "INSERT INTO orchestration_v2_projection_threads
           (thread_id, project_id, title, default_provider, runtime_mode, interaction_mode,
            created_at, updated_at, payload_json)
         VALUES ('t', 'p', 'Synthetic thread', 'claudeAgent', 'local', 'default',
                 '2026-11-01T00:00:00Z', '2026-11-17T19:00:00Z',
                 '{\"projectId\":\"p\",\"createdAt\":\"2026-11-01T00:00:00Z\"}');
         INSERT INTO orchestration_v2_projection_runs
           (run_id, thread_id, ordinal, provider, status, requested_at, completed_at, payload_json)
         VALUES ('r', 't', 1, 'claudeAgent', 'completed',
                 '2026-11-17T18:00:00Z', '2026-11-17T19:00:00Z',
                 '{\"status\":\"completed\",\"startedAt\":\"2026-11-17T18:00:00Z\"}');
         INSERT INTO orchestration_v2_projection_turn_items
           (turn_item_id, thread_id, run_id, ordinal, type, status, updated_at, payload_json)
         VALUES ('u', 't', 'r', 1, 'user_message', 'completed', '2026-11-17T18:00:00Z',
                 '{\"createdBy\":\"user\",\"creationSource\":\"web\"}'),
                ('c', 't', 'r', 2, 'command_execution', 'completed', '2026-11-17T19:00:00Z',
                 '{\"startedAt\":\"2026-11-17T18:00:00Z\",\"completedAt\":\"2026-11-17T19:00:00Z\",
                   \"input\":\"git status\"}');",
    ).unwrap();
    drop(db);

    let mut v = Shown::with_args(
        home, 80, 24,
        &["--db", path.to_str().unwrap(), "--calm", "--theme", "frontier"],
    );
    v.until("Live", |rows| header(rows).contains("LIVE"));
    let ready = |rows: &[String], period: &str, dates: &str| {
        assert!(footer(rows).contains("read at 10:20:00"), "{}", rows.join("\n"));
        assert!(rows[1].contains(period) && rows[1].contains(dates), "{}", rows[1]);
    };

    ready(&v.key_frames(b"i", 2)[1], "this week", "Nov 16");
    let last_week = v.key_frames(b"\x1b[C", 2);
    assert!(last_week[0][1].contains("last week"));
    ready(&last_week[1], "this week", "Nov 16");

    let tools = v.key_frames(b"\t", 2);
    assert!(tools[0].iter().any(|row| row.contains("Reading Tools for last week")));
    ready(&tools[1], "this week", "Nov 16");
    ready(&v.key_frames(b"\x1b[C", 2)[1], "this month", "November 2026");

    let last_month = v.key_frames(b"\x1b[C", 2);
    assert!(last_month[0][1].contains("last month"));
    ready(&last_month[1], "this month", "November 2026");
    v.pty.send(b"\x1b[C");
    v.still("Right stays at the fourth requested slot", |rows| rows == last_month[1]);

    // The cached Tools results paint immediately while Left traverses requested slots.
    v.key(b"\x1b[D", "this month", |rows| {
        rows[1].contains("this month") && !rows[1].contains("last month empty")
    });
    v.key(b"\x1b[D", "last week fallback after moving left", |rows| rows[1].contains("this week"));
    let this_week = v.key_frames(b"\x1b[D", 2);
    assert!(this_week[0].iter().any(|row| row.contains("Reading Tools for this week")));
    ready(&this_week[1], "this week", "Nov 16");

    // Live sees the source disappear while the report stays pinned to its successful read.
    std::fs::rename(&path, path.with_file_name("parked.sqlite")).unwrap();
    v.until("Live retrying while Insights keeps the report", |rows| {
        header(rows).contains("RETRYING") && footer(rows).contains("read at 10:20:00")
    });
    assert_eq!(v.rows()[1..23], this_week[1][1..23]);
    v.key(b"r", "failed refresh retaining the visible report", |rows| {
        footer(rows).contains("showing 10:20:00")
    });
    assert_eq!(v.rows()[1..23], this_week[1][1..23]);
    v.quit(b"q");
}

#[test]
fn insights_unreadable_and_snapshot_routes_preserve_terminal_shutdown() {
    let home = Home::new("insights-missing");
    let missing = home.0.join("missing.sqlite");
    let mut v = Shown::with_args(home, 80, 24, &["--db", missing.to_str().unwrap(), "--calm"]);
    v.until("Live retrying with a missing database", |rows| header(rows).contains("RETRYING"));
    let failed = |rows: &[String]| header(rows).contains("RECAP") && footer(rows).contains("can't read T3");
    v.key(b"i", "the Insights read error", failed);
    v.key(b"i", "back from the read error", |rows| !header(rows).contains("RECAP"));
    v.key(b"i", "the error after reopening", failed);
    v.pty.signal(libc::SIGTERM);
    assert!(v.pty.wait().success());
    v.pty.assert_restored();

    let path = fixture();
    let mut v = Shown::with_args(
        Home::new("insights-standalone"), 80, 24,
        &["--snapshot", path.to_str().unwrap()],
    );
    v.until("a standalone board", |rows| header(rows).contains("Loupe"));
    v.pty.send(b"i\t\x1b[Z");
    v.still("the standalone board never opens a report", |rows| !header(rows).contains("RECAP"));
    v.quit(b"q");
}

#[test]
fn demo_telemetry_board_navigation_and_no_themes_gating() {
    let on_telemetry = |rows: &[String]| {
        header(rows).contains("TELEMETRY")
            && header(rows).contains("SOURCES")
            && rows.iter().any(|r| r.contains("WHAT T3 CODE SENDS"))
            && footer(rows).contains("e back")
    };
    let on_sources = |rows: &[String]| {
        header(rows).contains("SOURCES")
            && rows.iter().any(|r| r.contains("LOCAL SOURCES INSPECTED"))
            && footer(rows).contains("e back")
    };
    let on_tel_help = |rows: &[String]| {
        rows.iter().any(|r| r.contains("TELEMETRY HELP"))
            && footer(rows).contains("close help")
    };

    let mut v = Shown::open("telemetry-nav", 120, 40);
    v.until("dashboard", on_dashboard);
    v.key(b"e", "Telemetry board", on_telemetry);
    v.key(b"\t", "Sources tab", on_sources);
    v.key(b"\t", "back to Telemetry tab", on_telemetry);
    v.key(b"?", "Telemetry help", on_tel_help);
    v.key(b"\x1b", "Esc closes help first", on_telemetry);
    v.key(b"e", "e returns to dashboard", on_dashboard);
    v.quit(b"q");

    // Under LOUPE_NO_THEMES=1, `e` must be a no-op (oracle parity).
    let home = Home::new("telemetry-no-themes");
    let t3 = home.0.join("t3");
    let env = [
        ("HOME", home.0.to_str().unwrap()),
        ("T3CODE_HOME", t3.to_str().unwrap()),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("LOUPE_NO_THEMES", "1"),
    ];
    let mut pty = Pty::spawn_args(120, 40, &["--demo", "--calm"], &env, &OUTSIDE[..3]);
    pty.wait_for(b"\x1b[c", 1);
    pty.send(b"\x1b[?62;22c");
    pty.wait_for(b"Loupe", 1);
    pty.send(b"e");
    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "{status:?}");
    let out = String::from_utf8_lossy(&pty.buf);
    assert!(!out.contains("WHAT T3 CODE SENDS"), "e opened Telemetry under LOUPE_NO_THEMES");
}

#[test]
fn demo_t3_handoffs_across_all_boards_and_no_themes_gating() {
    let mut v = Shown::open("handoff-all", 120, 40);
    v.until("dashboard", on_dashboard);

    // 1. Live `M` quick-reply / nudge shows demo preview and dismisses on Esc.
    v.key(b"M", "Live M demo preview", |rows| {
        rows.iter().any(|r| r.contains("Demo: would queue a message"))
    });
    v.key(b"\x1b", "Esc dismisses Live M banner", |rows| {
        on_dashboard(rows) && !rows.iter().any(|r| r.contains("Demo: would queue a message"))
    });

    // 2. Trace `f` shows demo preview and dismisses on Esc.
    v.key(b"w", "open Trace", |rows| {
        header(rows).contains("TRACE") && footer(rows).contains("f diagnose")
    });
    v.key(b"f", "Trace f demo preview", |rows| {
        rows.iter().any(|r| r.contains("Demo: would start a T3 thread (diagnose trace)"))
    });
    v.key(b"\x1b", "Esc dismisses Trace handoff banner", |rows| {
        header(rows).contains("TRACE")
            && !rows.iter().any(|r| r.contains("Demo: would start a T3 thread"))
    });
    v.key(b"w", "close Trace", on_dashboard);

    // 3. Storage `f` shows demo preview and dismisses on Esc.
    v.key(b"s", "open Storage", |rows| {
        header(rows).contains("STORAGE") && footer(rows).contains("f clean")
    });
    v.key(b"f", "Storage f demo preview", |rows| {
        rows.iter().any(|r| r.contains("Demo: would start a T3 thread (clean up storage)"))
    });
    v.key(b"\x1b", "Esc dismisses Storage handoff banner", |rows| {
        header(rows).contains("STORAGE")
            && !rows.iter().any(|r| r.contains("Demo: would start a T3 thread"))
    });
    v.key(b"s", "close Storage", on_dashboard);

    // 4. Insights Recap `f` and Tools `f` show demo previews and dismiss on Esc.
    v.key(b"i", "open Insights Recap", |rows| {
        header(rows).contains("RECAP") && footer(rows).contains("f optimize")
    });
    v.key(b"f", "Recap f demo preview", |rows| {
        rows.iter()
            .any(|r| r.contains("Demo: would start a T3 thread (optimize workflow from Recap)"))
    });
    v.key(b"\x1b", "Esc dismisses Recap handoff banner", |rows| {
        header(rows).contains("RECAP")
            && !rows.iter().any(|r| r.contains("Demo: would start a T3 thread"))
    });
    v.key(b"\t", "switch to Insights Tools", |rows| {
        header(rows).contains("TOOLS") && footer(rows).contains("f optimize tools")
    });
    v.key(b"f", "Tools f demo preview", |rows| {
        rows.iter()
            .any(|r| r.contains("Demo: would start a T3 thread (optimize slow & failing tools)"))
    });
    v.key(b"\x1b", "Esc dismisses Tools handoff banner", |rows| {
        header(rows).contains("TOOLS")
            && !rows.iter().any(|r| r.contains("Demo: would start a T3 thread"))
    });
    v.key(b"i", "close Insights", on_dashboard);

    // 5. Telemetry `f` shows demo preview.
    v.key(b"e", "open Telemetry", |rows| header(rows).contains("TELEMETRY"));
    v.key(b"f", "Telemetry f demo preview", |rows| {
        rows.iter()
            .any(|r| r.contains("Demo: would start a T3 thread (turn off telemetry)"))
    });
    v.key(b"e", "close Telemetry", on_dashboard);
    v.quit(b"q");

    // Under LOUPE_NO_THEMES=1, `M` must be a complete no-op (oracle parity).
    let home = Home::new("handoff-no-themes");
    let t3 = home.0.join("t3");
    let env = [
        ("HOME", home.0.to_str().unwrap()),
        ("T3CODE_HOME", t3.to_str().unwrap()),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("LOUPE_NO_THEMES", "1"),
    ];
    let mut pty = Pty::spawn_args(120, 40, &["--demo", "--calm"], &env, &OUTSIDE[..3]);
    pty.wait_for(b"\x1b[c", 1);
    pty.send(b"\x1b[?62;22c");
    pty.wait_for(b"Loupe", 1);
    pty.send(b"M");
    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "{status:?}");
    let out = String::from_utf8_lossy(&pty.buf);
    assert!(!out.contains("Demo: would queue"), "M opened handoff under LOUPE_NO_THEMES");
}
