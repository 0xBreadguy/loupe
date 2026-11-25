//! Trace while the dashboard runs: which task is open, what is selected, and a reader that
//! rereads that one task on its own thread.
//!
//! The product's own, like the theme keys: nothing here runs unless the product's options are
//! on, and the original has no counterpart. The reader is `trace::read_from` on one read-only
//! connection kept open while Trace is, polled as often as the dashboard's own reader. The
//! interface thread only takes finished traces from a mailbox, so a slow read never delays a
//! frame, as `live::spawn` does for the dashboard.
use crate::{
    source::SourceError,
    trace::{self, Kind, Target, Trace},
};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

type Read = Result<Option<Trace>, SourceError>;

/// A background reader of one task. Dropping it stops the thread after its current read.
pub struct Watch {
    latest: Arc<Mutex<Option<Read>>>,
    stop: Option<mpsc::Sender<()>>,
}

impl Watch {
    /// The newest finished read, if one arrived since the last call.
    pub fn take(&self) -> Option<Read> {
        self.latest.lock().unwrap().take()
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.take();
    }
}

/// Read `target` from `path` now and then every `interval`, until the watch is dropped.
/// `wake` is called from the reader's thread when a read is ready; it must only notify.
pub fn watch(path: &Path, target: Target, interval: Duration, wake: impl Fn() + Send + 'static) -> Watch {
    let path: PathBuf = path.to_owned();
    let latest = Arc::new(Mutex::new(None));
    let mailbox = latest.clone();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut db = None;
        // Steps kept between refreshes: only what T3 rewrote is parsed again.
        let mut cache = trace::ReadCache::default();
        // SQLite's change counter when the last read found the task ended. An ended task does
        // not depend on the clock, so while no other connection has committed since
        // (`PRAGMA data_version` changes when one does) there is nothing to read again and
        // nothing to redraw.
        let mut ended_at: Option<i64> = None;
        loop {
            if stopped.try_recv() != Err(mpsc::TryRecvError::Empty) {
                break;
            }
            if db.is_none() {
                match trace::open(&path) {
                    Ok(opened) => db = Some(opened),
                    Err(e) => {
                        *mailbox.lock().unwrap() = Some(Err(e));
                        wake();
                    }
                }
            }
            let version = db.as_ref().and_then(|c| c.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0)).ok());
            if let Some(connection) = db.as_ref().filter(|_| version.is_none() || version != ended_at) {
                let result = trace::read_cached(connection, &path, &target, crate::clock::wall_now(), &mut cache);
                ended_at = match &result {
                    Ok(Some(t)) if t.ended.is_some() => version,
                    _ => None,
                };
                if result.is_err() {
                    // Open again next time: the file may have been replaced.
                    db = None;
                }
                if stopped.try_recv() != Err(mpsc::TryRecvError::Empty) {
                    break;
                }
                *mailbox.lock().unwrap() = Some(result);
                wake();
            }
            if stopped.recv_timeout(interval.max(Duration::from_millis(1))) != Err(mpsc::RecvTimeoutError::Timeout) {
                break;
            }
        }
    });
    Watch { latest, stop: Some(stop) }
}

/// What a key asks of the presenter.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// Handled here, or not a Trace key: draw again.
    Stay,
    /// Back to the dashboard.
    Close,
    /// Open this thread in T3, as Enter does on the dashboard.
    OpenThread(String),
    /// Say this on the dashboard's notice line.
    Say(String),
    /// Read a different task: the presenter starts a new watch on `Screen::target()`.
    Read,
    /// Start a T3 Code thread to investigate this traced task (`f`).
    InvestigateInT3,
}

/// Trace while it is open.
pub struct Screen {
    /// The task shown last; earlier entries are the traces a delegate was opened from.
    pub stack: Vec<Target>,
    /// The latest read of the task on top of the stack.
    pub data: Option<Read>,
    /// The selected row's key (a span's `key`, or "model" or "more").
    pub selected: Option<String>,
    pub scroll: usize,
    /// The rows of the last frame, in screen order, for ↑↓ and clicks.
    pub rows: Vec<(String, i32)>,
    /// Rows that did not fit in the last frame.
    pub more_rows: usize,
    pub watch: Option<Watch>,
    /// The last frame drawn, and what it was drawn from.
    memo: Option<(FrameKey, crate::trace_view::Drawn)>,
}

/// What a frame depends on, apart from the spinner: the read (its thread, when it was read,
/// its size), the screen, the palette, the selection, and the whole second.
#[derive(PartialEq)]
struct FrameKey {
    read: (String, u64, usize, usize),
    size: (usize, usize),
    palette: crate::terminal::Palette,
    selected: Option<String>,
    scroll: usize,
    flags: (bool, bool, bool),
    second: u64,
}

impl Screen {
    pub fn new(target: Target) -> Screen {
        Screen { stack: vec![target], data: None, selected: None, scroll: 0, rows: vec![], more_rows: 0, watch: None, memo: None }
    }

    /// The trace's frame, when there is a trace to draw. It is drawn at the whole second, and
    /// at most once for each read, size, palette, selection and second: everything on it but
    /// the running spinner changes only with those (clocks show whole seconds). Frames in
    /// between are the last one with the spinner turned, which spares the full draw on nine
    /// frames in ten while the dashboard animates at 100 ms (checks/bench/trace-read).
    pub fn frame(&mut self, p: &crate::terminal::Palette, cols: usize, rows: usize, view: &crate::trace_view::View, local: crate::recap::Local) -> Option<crate::terminal::Canvas> {
        let Some(Ok(Some(task))) = &self.data else { return None };
        let second = (view.now / 1000.).floor() * 1000.;
        let key = FrameKey {
            read: (task.thread.clone(), task.now.to_bits(), task.spans.len(), task.runs.len()),
            size: (cols, rows),
            palette: *p,
            selected: view.selected.clone(),
            scroll: view.scroll,
            flags: (view.calm, view.mono, view.demo),
            second: second.to_bits(),
        };
        if self.memo.as_ref().is_none_or(|(k, _)| *k != key) {
            let at_second = crate::trace_view::View { now: second, selected: view.selected.clone(), ..*view };
            self.memo = Some((key, crate::trace_view::draw(task, p, cols, rows, &at_second, local)));
        }
        let drawn = &self.memo.as_ref().unwrap().1;
        let mut canvas = drawn.canvas.clone();
        if let Some((x, y)) = drawn.spinner {
            if let Some(cell) = canvas.cells.get_mut(y as usize * canvas.w + x as usize) {
                cell.ch = crate::terminal::spinner((view.spin % 10) as f64 * 100.);
            }
        }
        self.rows = drawn.rows.clone();
        self.selected = drawn.selected.clone();
        self.more_rows = drawn.more_rows;
        Some(canvas)
    }

    pub fn target(&self) -> &Target {
        self.stack.last().expect("a trace screen always has a target")
    }

    /// The trace on screen, when the last read found one.
    pub fn trace(&self) -> Option<&Trace> {
        match &self.data {
            Some(Ok(Some(t))) => Some(t),
            _ => None,
        }
    }

    /// Take a finished read from the watch. Returns whether anything changed.
    pub fn poll(&mut self) -> bool {
        match self.watch.as_ref().and_then(Watch::take) {
            Some(read) => {
                self.data = Some(read);
                true
            }
            None => false,
        }
    }

    fn go(&mut self, target: Target, push: bool) -> Outcome {
        if push {
            self.stack.push(target);
        } else {
            *self.stack.last_mut().unwrap() = target;
        }
        self.data = None;
        self.selected = None;
        self.scroll = 0;
        self.watch = None;
        Outcome::Read
    }

    /// Apply a key. `w` and Esc go back one trace, then close.
    pub fn key(&mut self, name: &str) -> Outcome {
        match name {
            "w" | "escape" => {
                if self.stack.len() > 1 {
                    self.stack.pop();
                    self.data = None;
                    self.selected = None;
                    self.scroll = 0;
                    self.watch = None;
                    Outcome::Read
                } else {
                    Outcome::Close
                }
            }
            "j" | "down" | "k" | "up" | "g" | "home" | "G" | "end" => {
                if self.rows.is_empty() {
                    return Outcome::Stay;
                }
                let index = self.rows.iter().position(|(key, _)| Some(key) == self.selected.as_ref()).unwrap_or(0);
                let last = self.rows.len() - 1;
                let next = match name {
                    "k" | "up" => index.saturating_sub(1),
                    "g" | "home" => 0,
                    "G" | "end" => last,
                    _ => (index + 1).min(last),
                };
                self.selected = Some(self.rows[next].0.clone());
                Outcome::Stay
            }
            "J" | "pagedown" => {
                self.scroll = (self.scroll + 5).min(self.more_rows.max(self.scroll));
                Outcome::Stay
            }
            "K" | "pageup" => {
                self.scroll = self.scroll.saturating_sub(5);
                Outcome::Stay
            }
            "left" | "right" => {
                let Some(t) = self.trace() else { return Outcome::Stay };
                let next = if name == "left" { t.previous.clone() } else { t.next.clone() };
                match next {
                    Some(run) => self.go(Target::Run(run), false),
                    None => Outcome::Say(if name == "left" { "No earlier task in this thread".into() } else { "No later task in this thread".into() }),
                }
            }
            "enter" | "o" => {
                let Some(t) = self.trace() else { return Outcome::Stay };
                let span = self.selected.as_ref().and_then(|key| t.spans.iter().find(|s| &s.key == key));
                match span.and_then(|s| s.delegate.as_ref().filter(|_| s.kind == Kind::Delegate)) {
                    Some(d) if d.remote => Outcome::Say(format!("{} ran on another machine: no trace here", d.model)),
                    Some(d) => match &d.child_thread {
                        Some(child) => self.go(Target::Thread(child.clone()), true),
                        None => Outcome::Say(format!("{} left no thread to trace", d.model)),
                    },
                    None => Outcome::OpenThread(t.thread.clone()),
                }
            }
            "f" => {
                if self.trace().is_some() {
                    Outcome::InvestigateInT3
                } else {
                    Outcome::Stay
                }
            }
            _ => Outcome::Stay,
        }
    }

    /// A click on a row selects it.
    pub fn press(&mut self, y: i32) {
        if let Some((key, _)) = self.rows.iter().find(|(_, row)| *row == y) {
            self.selected = Some(key.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Screen {
        let mut s = Screen::new(Target::Thread("t".into()));
        s.data = Some(trace::demo(1_795_000_000_000.).map(Some));
        s.rows = vec![("model".into(), 8), ("more".into(), 9), ("a".into(), 10), ("b".into(), 11)];
        s
    }

    #[test]
    fn a_cached_frame_is_the_frame_a_full_draw_makes() {
        // The demo task is running, so its spinner turns. Through one second of 100 ms frames,
        // after a selection change, and after a new read, every frame equals a full draw at
        // the whole second with that frame's spinner.
        use crate::{terminal::Palette, trace_view};
        let p = Palette::default();
        let local = crate::platform::local_time;
        let mut s = Screen::new(Target::Thread("t".into()));
        let mut task = trace::demo(1_795_000_000_000.).unwrap();
        assert_eq!(task.state, trace::State::Running);
        s.data = Some(Ok(Some(task.clone())));
        let check = |s: &mut Screen, now: f64, spin: usize| {
            let view = trace_view::View { selected: s.selected.clone(), scroll: s.scroll, calm: false, mono: false, spin, demo: true, now };
            let frame = s.frame(&p, 120, 40, &view, &local).unwrap();
            let task = s.trace().unwrap().clone();
            let full = trace_view::draw(&task, &p, 120, 40, &trace_view::View { now: (now / 1000.).floor() * 1000., ..view }, &local);
            assert_eq!(frame.cells, full.canvas.cells, "at {now} with spin {spin}");
            assert_eq!(s.rows, full.rows);
        };
        let second = 1_795_000_000_000.;
        for k in 0..10 {
            check(&mut s, second + k as f64 * 100., k);
        }
        s.selected = s.rows.get(3).map(|(key, _)| key.clone());
        check(&mut s, second + 1000., 10);
        task.now += 2000.;
        task.spans.pop();
        s.data = Some(Ok(Some(task)));
        check(&mut s, second + 1100., 11);
    }

    #[test]
    fn keys_move_the_selection_and_close() {
        let mut s = screen();
        assert_eq!(s.key("down"), Outcome::Stay);
        assert_eq!(s.selected.as_deref(), Some("more"));
        s.key("G");
        assert_eq!(s.selected.as_deref(), Some("b"));
        s.key("up");
        assert_eq!(s.selected.as_deref(), Some("a"));
        s.press(8);
        assert_eq!(s.selected.as_deref(), Some("model"));
        assert_eq!(s.key("w"), Outcome::Close);
    }

    #[test]
    fn enter_opens_a_delegate_and_w_comes_back() {
        let mut s = screen();
        let t = s.trace().unwrap().clone();
        let local = t.spans.iter().find(|s| s.delegate.as_ref().is_some_and(|d| d.child_thread.is_some() && !d.remote)).unwrap();
        s.selected = Some(local.key.clone());
        assert_eq!(s.key("enter"), Outcome::Read);
        assert_eq!((s.stack.len(), s.target().clone()), (2, Target::Thread(local.delegate.as_ref().unwrap().child_thread.clone().unwrap())));
        assert_eq!(s.key("escape"), Outcome::Read);
        assert_eq!(s.stack.len(), 1);
        // A remote delegate cannot be opened here, and says so.
        let mut s = screen();
        let remote = t.spans.iter().find(|s| s.delegate.as_ref().is_some_and(|d| d.remote)).unwrap();
        s.selected = Some(remote.key.clone());
        assert!(matches!(s.key("enter"), Outcome::Say(_)));
        // Anything else opens the thread in T3.
        s.selected = Some("model".into());
        assert_eq!(s.key("enter"), Outcome::OpenThread(t.thread.clone()));
    }

    #[test]
    fn the_watch_reads_the_task_and_stops_when_dropped() {
        let dir = std::env::temp_dir().join(format!("loupe-trace-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("synthetic.sqlite");
        let _ = std::fs::remove_file(&path);
        trace::demo_database(crate::clock::wall_now()).unwrap().execute("VACUUM INTO ?1", [path.to_str().unwrap()]).unwrap();
        let (tx, rx) = mpsc::channel();
        let w = watch(&path, Target::Thread("t".into()), Duration::from_millis(20), move || {
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(5)).expect("a read within five seconds");
        let read = w.take().expect("the mailbox holds the read").unwrap().unwrap();
        assert_eq!(read.thread, "t");
        // It keeps reading.
        rx.recv_timeout(Duration::from_secs(5)).expect("a second read");
        drop(w);
        // After the drop the thread ends: at most the read in flight still arrives.
        let _ = rx.recv_timeout(Duration::from_millis(200));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn left_and_right_step_through_the_threads_tasks() {
        let mut s = screen();
        // The demo task is the thread's first: nothing before it.
        assert!(matches!(s.key("left"), Outcome::Say(_)));
    }
}
