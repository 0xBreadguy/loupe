//! The shared host loop takes T3's database from the background reader.
mod support;
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};
use support::Db;
use loupe_core::{
    host::{self, Clock, Host},
    model::{Snapshot, State},
};

struct Still;
impl Clock for Still {
    fn instant(&mut self) {}
    fn wake_after(&mut self, _: Duration) {}
    fn clock(&self) -> f64 {
        0.
    }
}
fn ids(s: &State) -> Vec<String> {
    s.snap.agents.iter().map(|a| a.id.clone()).collect()
}
/// Polls the host each time the reader signals, until `done` holds or two seconds pass.
fn until(
    s: &mut State,
    h: &mut Host,
    woke: &mpsc::Receiver<()>,
    done: impl Fn(&State) -> bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let _ = woke.recv_timeout(Duration::from_millis(50));
        host::poll_host(s, 0., &mut Still, h);
        if done(s) {
            return true;
        }
    }
    false
}

#[test]
fn snapshots_arrive_from_the_background_and_wait_while_paused() {
    let mut db = Db::new();
    db.write("first", 1);
    let mut s = State::new(Snapshot::default());
    s.demo = false;
    let mut h = Host::live(db.path.clone(), 0.);
    h.usage = None;
    h.theme = None;
    let (tx, woke) = mpsc::channel();
    host::start_live(&mut h, 0.5, move || {
        let _ = tx.send(());
    });
    assert!(h.live.is_some());
    assert!(until(&mut s, &mut h, &woke, |s| ids(s) == ["first"]));

    // Paused: a write is read in the background but not shown.
    s.paused = true;
    db.write("second", 2);
    assert!(!until(&mut s, &mut h, &woke, |s| ids(s).len() == 2));
    assert_eq!(ids(&s), ["first"]);

    // Resumed: the newest snapshot was kept and appears without another write.
    s.paused = false;
    assert!(until(&mut s, &mut h, &woke, |s| ids(s).len() == 2));
    assert!(s.error.is_none());
}
