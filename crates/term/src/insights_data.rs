//! Visit-scoped typed Insights snapshots and bounded background reads.
//!
//! Call `open`, paint the initial loading frame if desired, then call `dispatch`.
//! Selection and refresh also stage work until dispatch. The wake callback signals
//! the terminal loop, which calls `poll` before borrowing a snapshot for drawing.
//!
//! Replacing a request drops queued work and discards a superseded result. The
//! existing report readers own their SQLite connections and can use multiple
//! connections. A running read, including SQL, is allowed to finish.

use std::{
    cell::Cell,
    fmt, io,
    sync::{Arc, Condvar, Mutex},
    thread,
};

use loupe_core::{
    clock,
    comparison::Comparison,
    platform,
    product::{self, Database},
    recap::{InsightsUsage, Recap, Which},
    source::SourceError,
    tools::Tools,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    Recap,
    Tools,
    Compare(u8),
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
            Report::Tools => 5,
            Report::Compare(_) => 10,
        };
        report
            + match self.period {
                Which::ThisWeek => 0,
                Which::LastWeek => 1,
                Which::ThisMonth => 2,
                Which::LastMonth => 3,
                Which::Last7Days => 4,
            }
    }
}

#[derive(Debug)]
pub enum Value {
    Recap(Recap),
    Tools(Tools),
    Compare(Comparison),
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
    id: u64,
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
            product::load_insights_recap(database, cutoff, period, local).map(|report| Loaded {
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
        Report::Compare(older) => product::load_comparison(database, cutoff, period, older, local)
            .map(|report| Loaded {
                returned: period,
                value: Value::Compare(report),
            }),
    }
}

struct Job<T> {
    id: u64,
    input: T,
}

struct Mailbox<T> {
    running: bool,
    retire: bool,
    wanted: Option<u64>,
    pending: Option<Job<T>>,
    completed: Option<Box<Snapshot>>,
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self {
            running: false,
            retire: false,
            wanted: None,
            pending: None,
            completed: None,
        }
    }
}

struct Worker<T> {
    mailbox: Mutex<Mailbox<T>>,
    changed: Condvar,
}

impl<T> Default for Worker<T> {
    fn default() -> Self {
        Self {
            mailbox: Mutex::default(),
            changed: Condvar::new(),
        }
    }
}

impl<T: Send + 'static> Worker<T> {
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

    fn submit<L, W>(
        self: &Arc<Self>,
        job: Job<T>,
        name: &str,
        trim_on_retire: bool,
        start: impl FnOnce() -> (L, W),
    ) -> io::Result<()>
    where
        L: Fn(T) -> Option<Box<Snapshot>> + Send + 'static,
        W: Fn() + Send + 'static,
    {
        let mut mailbox = self.mailbox.lock().unwrap();
        if mailbox.wanted == Some(job.id) {
            return Ok(());
        }
        mailbox.retire = false;
        mailbox.wanted = Some(job.id);
        mailbox.pending = Some(job);
        mailbox.completed = None;
        if mailbox.running {
            self.changed.notify_one();
            return Ok(());
        }
        mailbox.running = true;
        let worker_thread = Arc::clone(self);
        let (load, wake) = start();
        thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                worker_thread.run(load, wake);
                if trim_on_retire {
                    clock::release_memory();
                }
            })
            .map(|_| ())
            .map_err(|error| {
                *mailbox = Mailbox::default();
                error
            })
    }

    fn take(&self) -> Option<Box<Snapshot>> {
        self.mailbox.lock().unwrap().completed.take()
    }

    fn wants(&self, id: u64) -> bool {
        self.mailbox.lock().unwrap().wanted == Some(id)
    }

    fn run(&self, load: impl Fn(T) -> Option<Box<Snapshot>>, wake: impl Fn()) {
        loop {
            let job = {
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
            let Some(completed) = load(job.input) else {
                continue;
            };
            let mut mailbox = self.mailbox.lock().unwrap();
            if mailbox.wanted == Some(job.id) {
                mailbox.completed = Some(completed);
                drop(mailbox);
                wake();
            }
        }
    }
}

struct UsageRequest {
    request: Request,
    report: product::ReportRead<Recap>,
}

struct UsageLoader {
    worker: Arc<Worker<UsageRequest>>,
    load: Arc<ReadUsage>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

type ReadUsage =
    dyn Fn(UsageRequest, &dyn Fn() -> bool) -> Option<product::ReportRead<Recap>> + Send + Sync;

struct Visit {
    selected: Selection,
    cutoff: f64,
    cache: [Option<Box<Snapshot>>; 15],
    previous: Option<Box<Snapshot>>,
    loading_since: Option<f64>,
    dispatched: bool,
}

impl Visit {
    fn current(&self) -> Option<&Snapshot> {
        self.cache[self.selected.index()]
            .as_deref()
            .filter(|snapshot| snapshot.selected == self.selected)
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
    worker: Option<Arc<Worker<Request>>>,
    usage: Option<UsageLoader>,
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
        if let Some(usage) = &self.usage {
            usage.worker.reject(true);
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
        self.schedule_usage();
        true
    }

    pub fn refresh(&mut self, now: f64) -> bool {
        let Some(visit) = &mut self.visit else {
            return false;
        };
        if let Some(worker) = &self.worker {
            worker.reject(false);
        }
        if let Some(usage) = &self.usage {
            usage.worker.reject(false);
        }
        if let Some(snapshot) = visit.cache[visit.selected.index()].take() {
            if snapshot.selected == visit.selected && snapshot.outcome.is_ok() {
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
    pub fn dispatch(
        &mut self,
        database: &Database,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> bool {
        let wake = Arc::new(wake);
        if self.usage.is_none()
            && matches!(database, Database::Path { .. })
            && !loupe_core::theme::disabled()
        {
            let database = database.clone();
            self.usage = Some(UsageLoader {
                worker: Arc::new(Worker::default()),
                load: Arc::new(move |job, keep_reading| {
                    product::finish_insights_recap(
                        &database,
                        job.request.cutoff,
                        &platform::local_time,
                        job.report,
                        keep_reading,
                    )
                }),
                wake: wake.clone(),
            });
        }
        self.dispatch_with(matches!(database, Database::Path { .. }), || {
            let database = database.clone();
            (move |request| read(&database, request), move || wake())
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
        visit.dispatched = true;
        if let Err(error) = worker.submit(
            Job {
                id: request.id,
                input: request,
            },
            "t3-insights",
            trim_on_retire,
            || {
                let (load, wake) = start();
                let load = move |request: Request| {
                    let outcome = load(request).map_err(ReadError::Source);
                    Some(snapshot(request, outcome))
                };
                (load, wake)
            },
        ) {
            visit.accept(Box::new(Snapshot {
                id: request.id,
                selected: request.selected,
                cutoff: request.cutoff,
                finished_at: clock::wall_now(),
                outcome: Err(ReadError::StartWorker(error)),
            }));
        }
        true
    }

    /// Publish the base report before scheduling any provider history scan.
    pub fn poll(&mut self) -> bool {
        let Some(visit) = &mut self.visit else {
            return false;
        };
        let mut changed = false;
        if let Some(snapshot) = self.worker.as_ref().and_then(|worker| worker.take()) {
            visit.accept(snapshot);
            changed = true;
        }
        if let Some(snapshot) = self.usage.as_ref().and_then(|usage| usage.worker.take()) {
            let selected = snapshot.selected;
            let slot = &mut visit.cache[selected.index()];
            if slot.as_ref().is_some_and(|base| base.id == snapshot.id) {
                *slot = Some(snapshot);
                changed |= selected == visit.selected;
            }
        }
        self.schedule_usage();
        changed
    }

    fn schedule_usage(&mut self) {
        let Some(usage) = &self.usage else { return };
        let Some(visit) = &mut self.visit else { return };
        let Some(base) = visit.cache[visit.selected.index()]
            .as_mut()
            .filter(|base| base.selected == visit.selected)
        else {
            return;
        };
        let Ok(Loaded {
            returned,
            value: Value::Recap(report),
        }) = &mut base.outcome
        else {
            return;
        };
        if report.insights_usage != Some(InsightsUsage::Pending) || usage.worker.wants(base.id) {
            return;
        }
        let request = Request {
            id: base.id,
            selected: base.selected,
            cutoff: base.cutoff,
        };
        let job = Job {
            id: base.id,
            input: UsageRequest {
                request,
                report: product::ReportRead {
                    returned: *returned,
                    value: report.clone(),
                },
            },
        };
        if usage
            .worker
            .submit(job, "t3-insights-usage", true, || {
                let load_report = Arc::clone(&usage.load);
                let wake = Arc::clone(&usage.wake);
                let worker = Arc::clone(&usage.worker);
                let load = move |job: UsageRequest| {
                    let request = job.request;
                    let canceled = Cell::new(false);
                    let report = load_report(job, &|| {
                        if !canceled.get() && !worker.wants(request.id) {
                            canceled.set(true);
                        }
                        !canceled.get()
                    })?;
                    Some(snapshot(
                        request,
                        Ok(Loaded {
                            returned: report.returned,
                            value: Value::Recap(report.value),
                        }),
                    ))
                };
                (load, move || wake())
            })
            .is_err()
        {
            report.insights_usage = Some(InsightsUsage::Turns);
        }
    }
}

fn snapshot(request: Request, outcome: Result<Loaded, ReadError>) -> Box<Snapshot> {
    Box::new(Snapshot {
        id: request.id,
        selected: request.selected,
        cutoff: request.cutoff,
        finished_at: clock::wall_now(),
        outcome,
    })
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
        let expected = product::load_insights_recap(
            &Database::Demo,
            NOW,
            Which::ThisWeek,
            &platform::local_time,
        )
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
    fn recap_and_tools_are_visible_while_usage_is_reading() {
        let (wake, ready) = mpsc::channel();
        let (started, scanning) = mpsc::channel();
        let (release, barrier) = mpsc::channel();
        let barrier = Mutex::new(barrier);
        let token_wake = wake.clone();
        let mut data = InsightsData::default();
        data.usage = Some(UsageLoader {
            worker: Arc::new(Worker::default()),
            wake: Arc::new(move || token_wake.send(()).unwrap()),
            load: Arc::new(move |job, _| {
                started.send(()).unwrap();
                barrier.lock().unwrap().recv_timeout(TIMEOUT).unwrap();
                let mut report = job.report;
                report.value.insights_usage = Some(InsightsUsage::History(Box::new(
                    loupe_core::comparison::UsageWindow {
                        tokens: loupe_core::comparison::Tokens {
                            fresh: 83.,
                            turns: 1,
                            ..Default::default()
                        },
                        models: Default::default(),
                        providers: Default::default(),
                        days: Vec::new(),
                        partial: false,
                    },
                )));
                Some(report)
            }),
        });
        data.open(NOW);
        assert!(data.dispatch_with(false, || {
            let load = |request: Request| {
                let value = match request.selected.report {
                    Report::Recap => Value::Recap(Recap {
                        runs: 1,
                        insights_usage: Some(InsightsUsage::Pending),
                        ..Recap::default()
                    }),
                    Report::Tools => Value::Tools(Tools::default()),
                    _ => unreachable!(),
                };
                Ok(Loaded {
                    returned: request.selected.period,
                    value,
                })
            };
            (load, move || wake.send(()).unwrap())
        }));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        scanning.recv_timeout(TIMEOUT).unwrap();
        let visible = || data.visible().unwrap().outcome.as_ref().unwrap();
        assert!(
            matches!(&visible().value, Value::Recap(r) if r.insights_usage == Some(InsightsUsage::Pending))
        );
        assert_eq!(data.loading_since(), None);

        assert!(data.select(TOOLS, NOW));
        assert!(data.dispatch(&Database::Demo, || panic!("worker should be reused")));
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(data.poll());
        assert!(matches!(
            &data.visible().unwrap().outcome.as_ref().unwrap().value,
            Value::Tools(_)
        ));

        release.send(()).unwrap();
        ready.recv_timeout(TIMEOUT).unwrap();
        assert!(!data.poll(), "an offscreen Usage result needs no repaint");
        assert!(data.select(Selection::ENTRY, NOW));
        assert!(
            matches!(&data.visible().unwrap().outcome.as_ref().unwrap().value,
            Value::Recap(r) if matches!(&r.insights_usage, Some(InsightsUsage::History(usage)) if usage.tokens.total() == 83.))
        );
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
