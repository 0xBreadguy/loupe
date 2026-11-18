//! Presenter proof under a pseudo-terminal. No emulator and no window.
//!
//! The child is `t3-agents-term`. Its bytes go through the same virtual screen
//! the unit tests use, and that screen is compared cell for cell with
//! `t3-agents-core --frame-json`.
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
use t3_agents_core::terminal::{Canvas, Cell};
use t3_agents_term::{LEAVE_TERM, Screen, cells_match};

const COLS: u16 = 233;
const ROWS: u16 = 55;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../checks/fixtures/demo.json")
}

fn core_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_t3-agents-term")).with_file_name("t3-agents-core")
}

fn term_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_t3-agents-term"))
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

fn sync_ends(buf: &[u8]) -> usize {
    buf.windows(8).filter(|w| *w == b"\x1b[?2026l").count()
}

fn nth_sync_end(buf: &[u8], n: usize) -> usize {
    let mut seen = 0;
    for (i, w) in buf.windows(8).enumerate() {
        if w == b"\x1b[?2026l" {
            seen += 1;
            if seen == n {
                return i + 8;
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
        "t3-agents-core --frame-json failed ({:?}): {}",
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

fn layout() -> (t3_agents_core::ui::Surface, String) {
    let snap: t3_agents_core::model::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    let mut state = t3_agents_core::model::State::new(snap);
    let (_, surface) =
        t3_agents_core::ui::draw_grid(&state, COLS as usize, ROWS as usize, &state.palette);
    let selected = state.selected.clone().unwrap_or_default();
    // A press on the selected row does not move. Pick another row the click can reach.
    let _ = &mut state;
    (surface, selected)
}

#[test]
fn keys_match_frame_json_and_resize_repaints() {
    let mut pty = Pty::spawn(COLS, ROWS, &[]);
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

    let mut pty = Pty::spawn(COLS, ROWS, &[]);
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
    let mut pty = Pty::spawn(COLS, ROWS, &[]);
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
    let mut pty = Pty::spawn(hcols, hrows, &[]);
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
    let mut pty = Pty::spawn(cols, rows, &[]);
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

fn demo_state() -> t3_agents_core::model::State {
    let snap: t3_agents_core::model::Snapshot =
        serde_json::from_str(&std::fs::read_to_string(fixture()).unwrap()).unwrap();
    t3_agents_core::model::State::new(snap)
}

fn detail_point() -> (u16, u16, i32, i32) {
    for rows in [55u16, 40, 30, 24, 18, 12] {
        let mut state = demo_state();
        let (_, surface) =
            t3_agents_core::ui::draw_grid(&state, 233, rows as usize, &state.palette);
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
    let (_, surface) = t3_agents_core::ui::draw_grid(&state, cols, rows, &state.palette);
    state.detail_scroll = (state.detail_scroll + by).min(surface.detail_max);
    let (canvas, _) = t3_agents_core::ui::draw_grid(&state, cols, rows, &state.palette);
    canvas
}

fn help_size() -> (u16, u16) {
    for rows in [24u16, 20, 16, 12] {
        let mut state = demo_state();
        state.action("?");
        let (_, surface) = t3_agents_core::ui::draw_grid(&state, 80, rows as usize, &state.palette);
        if surface.help_max >= 3 {
            return (80, rows);
        }
    }
    panic!("help page fits every tried grid");
}

fn help_scrolled(cols: usize, rows: usize) -> Canvas {
    let mut state = demo_state();
    state.action("?");
    let (_, surface) = t3_agents_core::ui::draw_grid(&state, cols, rows, &state.palette);
    assert!(surface.help_max > 0, "help has nothing to scroll");
    state.help_scroll = 3.min(surface.help_max);
    let (canvas, _) = t3_agents_core::ui::draw_grid(&state, cols, rows, &state.palette);
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
        let path = std::env::temp_dir().join(format!("t3-agents-pty-{tag}-{}", std::process::id()));
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
const OUTSIDE: &[&str] = &["XDG_CONFIG_HOME", "OMARCHY_PATH", "T3_AGENTS_THEME", "T3_AGENTS_NO_THEMES", "TMUX", "STY"];

fn background(rgb: u32) -> Vec<u8> {
    format!("48;2;{};{};{}", rgb >> 16, (rgb >> 8) & 255, rgb & 255).into_bytes()
}

/// With no Omarchy and nothing chosen, the dashboard asks the terminal for its background and
/// draws Paper on a light one and Frontier on a dark one, and follows a color scheme change. This
/// is a deliberate difference from the original, which draws its gray fallback here; under
/// `T3_AGENTS_NO_THEMES` it asks nothing (below).
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
    let config = std::fs::read_to_string(home.0.join(".config/t3-agents/config.toml")).unwrap();
    assert_eq!(config, "theme = \"dark:paper,light:paper\"\n");
}

/// The original's behavior, which the parity checks compare: no background query, no theme key.
#[test]
fn without_themes_nothing_is_asked_and_c_is_not_a_key() {
    let home = Home::new("off");
    let h = home.0.to_str().unwrap();
    let t3 = home.0.join("t3");
    let env = [("HOME", h), ("T3CODE_HOME", t3.to_str().unwrap()), ("TERM", "xterm-256color"), ("COLORTERM", "truecolor"), ("T3_AGENTS_NO_THEMES", "1")];
    let mut pty = Pty::spawn_args(160, 45, &["--demo", "--calm"], &env, &OUTSIDE[..3]);
    pty.wait_for(b"\x1b[c", 1);
    pty.send(b"\x1b[?62;22c");
    pty.wait_for(b"T3 Agents", 1);
    pty.send(b"c");
    pty.send(b"q");
    let status = pty.wait();
    assert!(status.success(), "{status:?}");
    let out = String::from_utf8_lossy(&pty.buf);
    assert!(!out.contains("\x1b[?2031h") && !out.contains("\x1b]11;?"), "{out}");
    assert!(!out.contains("theme:"), "{out}");
    assert!(!home.0.join(".config/t3-agents").exists());
}
