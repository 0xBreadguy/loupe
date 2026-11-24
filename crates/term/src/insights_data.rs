//! Visit-scoped typed Insights snapshots and a single background reader.
//!
//! Call `open`, paint the initial loading frame if desired, then call `dispatch`.
//! Selection and refresh also stage work until dispatch. The wake callback signals
//! the terminal loop, which calls `poll` before borrowing a snapshot for drawing.
//!
//! Replacing a request drops queued work and discards a superseded result. The
//! existing report readers own their SQLite connections and can use multiple
//! connections. A running read, including SQL, is allowed to finish.

use std::{
    fmt, io,
    sync::{Arc, Condvar, Mutex},
    thread,
};

use loupe_core::{
    clock, platform,
    product::{self, Database},
    recap::{Recap, Which},
    source::SourceError,
    tools::Tools,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    Recap,
    Tools,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub report: Report,
    pub period: Which,
}

impl Selection {
    pub const ENTRY: Self = Self {
        report: Report::Recap,
        period: Which::ThisWeek,
    };

    fn index(self) -> usize {
        let report = match self.report {
            Report::Recap => 0,
            Report::Tools => 4,
        };
        report
            + match self.period {
                Which::ThisWeek => 0,
                Which::LastWeek => 1,
                Which::ThisMonth => 2,
                Which::LastMonth => 3,
            }
    }
}

#[derive(Debug)]
pub enum Value {
    Recap(Recap),
    Tools(Tools),
}

#[derive(Debug)]
pub struct Loaded {
    pub returned: Which,
    pub value: Value,
}

#[derive(Debug)]
pub enum ReadError {
    Source(SourceError),
    StartWorker(io::Error),
}

impl fmt::Display for ReadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(error) => error.fmt(formatter),
            Self::StartWorker(error) => {
                write!(formatter, "Could not start the Insights reader: {error}")
            }
        }
    }
}

impl std::error::Error for ReadError {}

#[derive(Debug)]
pub struct Snapshot {
    pub selected: Selection,
    pub cutoff: f64,
    pub finished_at: f64,
    pub outcome: Result<Loaded, ReadError>,
}

#[derive(Clone, Copy)]
struct Request {
    id: u64,
    selected: Selection,
    cutoff: f64,
}

fn read(database: &Database, request: Request) -> Result<Loaded, SourceError> {
    let local = &platform::local_time;
    let cutoff = request.cutoff;
    let period = request.selected.period;
    match request.selected.report {
        Report::Recap => {
            product::load_recap(database, cutoff, period, local).map(|report| Loaded {
                returned: report.returned,
                value: Value::Recap(report.value),
            })
        }
        Report::Tools => {
            product::load_tools(database, cutoff, period, local).map(|report| Loaded {
                returned: report.returned,
                value: Value::Tools(report.value),
            })
        }
    }
}

#[derive(Default)]
struct Mailbox {
    running: bool,
    retire: bool,
    wanted: Option<u64>,
    pending: Option<Request>,
    completed: Option<Box<Snapshot>>,
}

#[derive(Default)]
struct Worker {
    mailbox: Mutex<Mailbox>,
    changed: Condvar,
}

impl Worker {
    fn reject(&self, retire: bool) {
        let completed = {
            let mut mailbox = self.mailbox.lock().unwrap();
            mailbox.retire |= retire;
            mailbox.wanted = None;
            mailbox.pending = None;
            mailbox.completed.take()
        };
        if retire {
            self.changed.notify_one();
        }
        drop(completed);
    }

    fn run(&self, load: impl Fn(Request) -> Result<Loaded, SourceError>, wake: impl Fn()) {
        loop {
            let request = {
                let mut mailbox = self.mailbox.lock().unwrap();
                loop {
                    if let Some(request) = mailbox.pending.take() {
                        break request;
                    }
                    if mailbox.retire {
                        mailbox.running = false;
                        return;
                    }
                    mailbox = self.changed.wait(mailbox).unwrap();
                }
            };
            let outcome = load(request).map_err(ReadError::Source);
            let finished_at = clock::wall_now();
            let mut mailbox = self.mailbox.lock().unwrap();
            if mailbox.wanted == Some(request.id) {
                mailbox.completed = Some(Box::new(Snapshot {
                    selected: request.selected,
                    cutoff: request.cutoff,
                    finished_at,
                    outcome,
                }));
                drop(mailbox);
                wake();
            }
        }
    }
}

struct Visit {
    selected: Selection,
    cutoff: f64,
    cache: [Option<Box<Snapshot>>; 8],
    previous: Option<Box<Snapshot>>,
    loading_since: Option<f64>,
    dispatched: bool,
}

impl Visit {
    fn current(&self) -> Option<&Snapshot> {
        self.cache[self.selected.index()].as_deref()
    }

    fn accept(&mut self, snapshot: Box<Snapshot>) {
        if snapshot.outcome.is_ok() {
            self.previous = None;
        }
        self.cache[self.selected.index()] = Some(snapshot);
        self.loading_since = None;
    }
}

/// Keep one instance across visits so a retiring read cannot overlap a new read.
#[derive(Default)]
pub struct InsightsData {
    visit: Option<Visit>,
    worker: Option<Arc<Worker>>,
    next_id: u64,
}

impl InsightsData {
    /// Pin the supplied epoch milliseconds for this visit. The caller chooses
    /// how to obtain the clock and when to dispatch after painting.
    pub fn open(&mut self, now: f64) {
        self.close();
        self.visit = Some(Visit {
            selected: Selection::ENTRY,
            cutoff: now,
            cache: Default::default(),
            previous: None,
            loading_since: Some(now),
            dispatched: false,
        });
    }

    pub fn close(&mut self) {
        if let Some(worker) = &self.worker {
            worker.reject(true);
        }
        self.visit = None;
    }

    pub fn selection(&self) -> Option<Selection> {
        self.visit.as_ref().map(|visit| visit.selected)
    }

    pub fn loading_since(&self) -> Option<f64> {
        self.visit.as_ref()?.loading_since
    }

    /// Most recent accepted outcome for the selection, including empty reports
    /// and errors. CLI JSON prints the full typed empty report. A source error
    /// instead yields stderr advice and exit 1, with no JSON on stdout.
    pub fn outcome(&self) -> Option<&Snapshot> {
        self.visit.as_ref()?.current()
    }

    /// Borrow this exact snapshot for drawing. During refresh or after a failed
    /// refresh it may be older than `outcome`; inspect `outcome` for the error.
    pub fn visible(&self) -> Option<&Snapshot> {
        let visit = self.visit.as_ref()?;
        visit.previous.as_deref().or_else(|| visit.current())
    }

    pub fn select(&mut self, selection: Selection, now: f64) -> bool {
        let Some(visit) = &mut self.visit else {
            return false;
        };
        if visit.selected == selection {
            return false;
        }
        if let Some(worker) = &self.worker {
            worker.reject(false);
        }
        visit.selected = selection;
        visit.previous = None;
        visit.loading_since = visit.current().is_none().then_some(now);
        visit.dispatched = false;
        true
    }

    pub fn refresh(&mut self, now: f64) -> bool {
        let Some(visit) = &mut self.visit else {
            return false;
        };
        if let Some(worker) = &self.worker {
            worker.reject(false);
        }
        if let Some(snapshot) = visit.cache[visit.selected.index()].take() {
            if snapshot.outcome.is_ok() {
                visit.previous = Some(snapshot);
            }
        }
        visit.cache = Default::default();
        visit.cutoff = now;
        visit.loading_since = Some(now);
        visit.dispatched = false;
        true
    }

    /// Dispatch staged work, if any. Supply the same dashboard database and
    /// wake destination throughout this controller's lifetime. Keep the wake
    /// destination alive in the callback and signal it without blocking.
    ///
    /// Returns true if a request was dispatched. A thread creation failure
    /// immediately sets `outcome` and clears `loading_since`, without a callback.
    pub fn dispatch(&mut self, database: &Database, wake: impl Fn() + Send + 'static) -> bool {
        self.dispatch_with(matches!(database, Database::Path { .. }), || {
            let database = database.clone();
            (move |request| read(&database, request), wake)
        })
    }

    fn dispatch_with<L, W>(&mut self, trim_on_retire: bool, start: impl FnOnce() -> (L, W)) -> bool
    where
        L: Fn(Request) -> Result<Loaded, SourceError> + Send + 'static,
        W: Fn() + Send + 'static,
    {
        let Some(visit) = &mut self.visit else {
            return false;
        };
        if visit.loading_since.is_none() || visit.dispatched {
            return false;
        }
        self.next_id += 1;
        let request = Request {
            id: self.next_id,
            selected: visit.selected,
            cutoff: visit.cutoff,
        };
        let worker = self
            .worker
            .get_or_insert_with(|| Arc::new(Worker::default()));
        let mut mailbox = worker.mailbox.lock().unwrap();
        visit.dispatched = true;
        mailbox.retire = false;
        mailbox.wanted = Some(request.id);
        mailbox.pending = Some(request);
        if mailbox.running {
            worker.changed.notify_one();
            return true;
        }

        mailbox.running = true;
        let worker_thread = Arc::clone(worker);
        let (load, wake) = start();
        if let Err(error) = thread::Builder::new()
            .name("t3-insights".into())
            .spawn(move || {
                worker_thread.run(load, wake);
                // The read scratch is gone and a new visit can start using the mailbox.
                if trim_on_retire {
                    clock::release_memory();
                }
            })
        {
            *mailbox = Mailbox::default();
            visit.accept(Box::new(Snapshot {
                selected: request.selected,
                cutoff: request.cutoff,
                finished_at: clock::wall_now(),
                outcome: Err(ReadError::StartWorker(error)),
            }));
        }
        true
    }

    /// Take at most one result on the event loop. The mailbox rejects results
    /// superseded by navigation, refresh or closing, even across reopening.
    pub fn poll(&mut self) -> bool {
        let Some(visit) = &mut self.visit else {
            return false;
        };
        let Some(worker) = &self.worker else {
            return false;
        };
        let completed = worker.mailbox.lock().unwrap().completed.take();
        if let Some(snapshot) = completed {
            visit.accept(snapshot);
            true
        } else {
            false
        }
    }
}

impl Drop for InsightsData {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    const NOW: f64 = 1_794_810_000_000.0;
    const TOOLS: Selection = Selection {
        report: Report::Tools,
        period: Which::ThisWeek,
    };
    const LATER: f64 = NOW + 7.0 * 86_400_000.0;
    const TIMEOUT: Duration = Duration::from_secs(10);

    #[test]
    fn worker_returns_the_typed_demo_used_by_the_report_loaders() {
        let (wake, ready) = mpsc::channel();
        let mut data = InsightsData::default();
        data.open(NOW);
        assert_eq!(data.selection(), Some(Selection::ENTRY));
        assert_eq!(data.loading_since(), Some(NOW));
        assert!(data.visible().is_none());
        assert!(ready.try_recv().is_err());
        assert!(data.dispatch(&Database::Demo, move || wake.send(()).unwrap()));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        let recap = data.visible().unwrap();
        assert_eq!(recap.selected, Selection::ENTRY);
        assert_eq!(recap.cutoff, NOW);
        let loaded = recap.outcome.as_ref().unwrap();
        let expected =
            product::load_recap(&Database::Demo, NOW, Which::ThisWeek, &platform::local_time)
                .unwrap();
        assert_eq!(loaded.returned, expected.returned);
        assert!(matches!(&loaded.value, Value::Recap(value) if *value == expected.value));

        data.select(TOOLS, LATER);
        assert!(data.dispatch(&Database::Demo, || panic!("worker should be reused")));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        let tools = data.visible().unwrap();
        assert_eq!(tools.selected, TOOLS);
        assert_eq!(tools.cutoff, NOW);
        let loaded = tools.outcome.as_ref().unwrap();
        let expected =
            product::load_tools(&Database::Demo, NOW, Which::ThisWeek, &platform::local_time)
                .unwrap();
        assert_eq!(loaded.returned, expected.returned);
        assert!(matches!(&loaded.value, Value::Tools(value) if *value == expected.value));
        data.select(Selection::ENTRY, LATER);
        assert!(!data.dispatch(&Database::Demo, || panic!("cached selection")));
        assert_eq!(data.visible().unwrap().cutoff, NOW);

        assert!(data.refresh(LATER));
        assert!(data.outcome().is_none());
        assert_eq!(data.visible().unwrap().cutoff, NOW);
        assert!(data.dispatch(&Database::Demo, || panic!("worker should be reused")));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        assert_eq!(data.visible().unwrap().cutoff, LATER);
        assert!(data.select(TOOLS, LATER));
        assert!(data.visible().is_none());
        assert!(data.dispatch(&Database::Demo, || panic!("worker should be reused")));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        assert_eq!(data.visible().unwrap().cutoff, LATER);
    }

    #[test]
    fn concurrent_navigation_keeps_only_the_latest_request_and_rejects_stale_results() {
        let (started, reads) = mpsc::channel();
        let (release, barrier) = mpsc::channel();
        let (wake, ready) = mpsc::channel();
        let mut data = InsightsData::default();
        data.open(NOW);
        assert!(data.dispatch_with(false, || {
            let load = move |request: Request| {
                started.send(request).unwrap();
                let fail = barrier.recv_timeout(TIMEOUT).unwrap();
                if fail {
                    return Err(SourceError {
                        kind: "missing",
                        path: "synthetic.db".into(),
                        detail: String::new(),
                    });
                }
                Ok(Loaded {
                    returned: request.selected.period,
                    value: Value::Recap(Recap::default()),
                })
            };
            (load, move || wake.send(()).unwrap())
        }));
        let first = reads.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(first.selected, Selection::ENTRY);
        let unused = || panic!("another worker started");
        assert!(!data.dispatch(&Database::Demo, unused));
        for _ in 0..128 {
            assert!(data.select(TOOLS, LATER));
            assert!(data.dispatch(&Database::Demo, unused));
            assert!(data.select(Selection::ENTRY, LATER));
            assert!(data.dispatch(&Database::Demo, unused));
        }
        data.refresh(LATER);
        assert!(data.dispatch(&Database::Demo, unused));
        data.close();
        assert!(data.visible().is_none());
        assert!(!data.poll());
        data.open(LATER);
        assert!(data.dispatch(&Database::Demo, unused));
        assert!(reads.try_recv().is_err());
        release.send(false).unwrap();
        let reopened = reads.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(reopened.selected, Selection::ENTRY);
        assert_eq!(reopened.cutoff, LATER);
        assert!(reopened.id > first.id);
        assert!(ready.try_recv().is_err());
        assert!(!data.poll());
        assert!(data.visible().is_none());

        // Replace a read that is already running, this time on refresh.
        assert!(data.refresh(NOW));
        assert!(data.dispatch(&Database::Demo, unused));
        release.send(false).unwrap();
        let refreshed = reads.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(refreshed.cutoff, NOW);
        assert!(!data.poll());
        assert!(ready.try_recv().is_err());
        release.send(false).unwrap();
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        assert_eq!(data.visible().unwrap().cutoff, NOW);

        assert!(data.select(TOOLS, NOW));
        assert!(data.dispatch(&Database::Demo, unused));
        let switched = reads.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(switched.selected, TOOLS);
        // Discard an active Tools read on switching back to the cached Recap.
        assert!(data.select(Selection::ENTRY, NOW));
        assert!(!data.dispatch(&Database::Demo, unused));
        assert_eq!(data.visible().unwrap().cutoff, NOW);
        release.send(false).unwrap();
        assert!(data.refresh(LATER));
        assert!(data.dispatch(&Database::Demo, unused));
        let failed = reads.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(failed.selected, Selection::ENTRY);
        assert_eq!(failed.cutoff, LATER);
        assert!(ready.try_recv().is_err());
        assert!(!data.poll());
        assert_eq!(data.visible().unwrap().cutoff, NOW);
        release.send(true).unwrap();
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        assert!(matches!(
            data.outcome().unwrap().outcome,
            Err(ReadError::Source(_))
        ));
        assert_eq!(data.visible().unwrap().cutoff, NOW);
        assert!(data.select(TOOLS, LATER));
        assert!(data.visible().is_none());
        assert!(data.dispatch(&Database::Demo, unused));
        let tools_again = reads.recv_timeout(TIMEOUT).unwrap();
        assert_eq!(tools_again.selected, TOOLS);
        assert_eq!(tools_again.cutoff, LATER);

        data.close();
        release.send(false).unwrap();
        // Take the receiver to disconnection to verify retirement and that the
        // closing read produced no wake, without sleeping or joining the worker.
        assert_eq!(
            ready.recv_timeout(TIMEOUT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
        assert_eq!(
            reads.recv_timeout(TIMEOUT).err(),
            Some(mpsc::RecvTimeoutError::Disconnected)
        );
    }
}
