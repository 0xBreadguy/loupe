//! Wall-clock helpers shared by every presenter.
use std::time::{SystemTime, UNIX_EPOCH};
pub fn clock(now: f64) -> String {
    if let Some(tm) = crate::platform::local_time((now / 1000.).floor() as i64) {
        return format!("{:02}:{:02}:{:02}", tm.hour, tm.minute, tm.second);
    }
    let sec = ((now / 1000.).floor() as i64).rem_euclid(86400);
    format!("{:02}:{:02}:{:02}", sec / 3600, (sec % 3600) / 60, sec % 60)
}
pub fn wall_now() -> f64 {
    // The terminal parity gate pins both sides. Node does it with fixed-clock.mjs;
    // this is the same instant for every presenter that asks for the wall clock.
    // Absent or not a finite number, the real clock is used and nothing else changes.
    if let Some(ms) = fixed_now() {
        return ms;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
        * 1000.
}
/// Hand freed heap back to the system.
///
/// A full read of a real database allocates tens of megabytes for a moment: every row it
/// decodes, before the few fields that are kept. glibc holds on to that as free heap, so a
/// process that keeps only a status line still shows the size of its largest read. Call
/// this after a full read. Elsewhere it does nothing.
pub fn release_memory() {
    crate::platform::release_memory();
}

fn fixed_now() -> Option<f64> {
    let raw = std::env::var("T3_FIXED_NOW").ok()?;
    let ms = raw.trim().parse::<f64>().ok()?;
    ms.is_finite().then_some(ms)
}
