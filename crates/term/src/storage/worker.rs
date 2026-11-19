use super::OwnedInput;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use t3_agents_core::storage::{Cancelled, Inventory};

#[derive(Debug)]
pub enum ScanState {
    Idle,
    /// Indeterminate. Inventory only reports coverage after the walk returns.
    Scanning,
    Ready(Inventory),
    /// The worker exited unexpectedly.
    Failed,
}

struct Job {
    input: Arc<OwnedInput>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Default)]
struct Mailbox {
    pending: Option<Job>,
    ready: Option<Inventory>,
    closed: bool,
}

/// A single scanner with one replaceable pending request. All filesystem work takes place
/// outside the mailbox lock. The presenter owns the visible state and only advances it in poll().
pub struct StorageWorker {
    mailbox: Arc<Mutex<Mailbox>>,
    thread: JoinHandle<()>,
    current: Option<Job>,
    state: ScanState,
}

impl StorageWorker {
    /// `notify` wakes the presenter when a result is ready. It runs on this worker and must
    /// return promptly. If it uses a wake channel, capture an owned handle: drop never joins.
    pub fn new(notify: impl Fn() + Send + 'static) -> io::Result<Self> {
        Self::start(notify, |input, cancelled| input.scan(cancelled))
    }

    pub(super) fn start(
        notify: impl Fn() + Send + 'static,
        scan: impl Fn(&OwnedInput, &dyn Fn() -> bool) -> Result<Inventory, Cancelled> + Send + 'static,
    ) -> io::Result<Self> {
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let shared = Arc::clone(&mailbox);
        let thread = thread::Builder::new()
            .name("t3-storage".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut mail = shared.lock().unwrap();
                        if mail.closed {
                            break;
                        }
                        mail.pending.take()
                    };
                    let Some(job) = job else {
                        thread::park();
                        continue;
                    };

                    let cancelled = || job.cancelled.load(Ordering::Acquire);
                    let Ok(result) = scan(&job.input, &cancelled) else {
                        continue;
                    };
                    {
                        let mut mail = shared.lock().unwrap();
                        if cancelled() {
                            continue;
                        }
                        mail.ready = Some(result);
                    }
                    notify();
                }
            })?;
        Ok(Self {
            mailbox,
            thread,
            current: None,
            state: ScanState::Idle,
        })
    }

    /// Start if idle or if the input differs. Identical requests reuse both in-flight and
    /// completed work. Call on Storage entry and after relevant snapshot changes while open.
    pub fn request(&mut self, input: OwnedInput) {
        if self.input() != Some(&input) {
            self.rescan(input);
        }
    }

    /// Force a fresh walk, superseding the current one even when the input is unchanged.
    pub fn rescan(&mut self, input: OwnedInput) {
        self.cancel();
        if self.thread.is_finished() {
            self.state = ScanState::Failed;
            return;
        }
        let input = Arc::new(input);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.current = Some(Job {
            input: Arc::clone(&input),
            cancelled: Arc::clone(&cancelled),
        });
        self.mailbox.lock().unwrap().pending = Some(Job { input, cancelled });
        self.state = ScanState::Scanning;
        self.thread.thread().unpark();
    }

    /// Hide and invalidate all results immediately. An in-flight scan stops at its next
    /// cooperative cancellation check, including after returning from a filesystem syscall.
    pub fn cancel(&mut self) {
        self.clear(false);
    }

    fn clear(&mut self, close: bool) {
        let discarded = {
            let mut mail = self.mailbox.lock().unwrap();
            if let Some(job) = &self.current {
                job.cancelled.store(true, Ordering::Release);
            }
            mail.closed = close;
            (mail.pending.take(), mail.ready.take())
        };
        drop(discarded);
        self.current = None;
        self.state = ScanState::Idle;
    }

    /// Take a completed result if available. Poll after a wake and before composing Storage.
    /// Returns whether the visible state changed.
    pub fn poll(&mut self) -> bool {
        let ready = self.mailbox.lock().unwrap().ready.take();
        if let Some(result) = ready {
            self.state = ScanState::Ready(result);
            return true;
        }
        if matches!(self.state, ScanState::Scanning) && self.thread.is_finished() {
            self.state = ScanState::Failed;
            return true;
        }
        false
    }

    pub fn state(&self) -> &ScanState {
        &self.state
    }

    /// Metadata for the same request as the visible state.
    pub fn input(&self) -> Option<&OwnedInput> {
        self.current.as_ref().map(|job| job.input.as_ref())
    }
}

impl Drop for StorageWorker {
    fn drop(&mut self) {
        self.clear(true);
        self.thread.thread().unpark();
    }
}
