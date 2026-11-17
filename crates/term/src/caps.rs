//! What the terminal can do beyond the basics, found out once at start-up the way the original's
//! renderer does it, and put back when the program leaves.
//!
//! The probe is four queries written together, right after entering the alternate screen:
//!
//! - `CSI > 0 q`: XTVERSION, the terminal's name and version. The original asks first of all, and
//!   judges by the answer (kitty, iTerm2, XTerm and Ghostty are known by it where the
//!   environment says nothing, or something else).
//! - `CSI ? 2027 $ p`: is grapheme-cluster mode (2027) known, and is it on?
//! - `CSI H`, `OSC 66 ; w=1 ; <space> ST`, `CSI 6 n`: write one space with an explicit width of 1
//!   at home and ask where the cursor is. A terminal that speaks the protocol has moved it to
//!   column 2. One that does not has ignored the sequence (or drawn nothing) and is still at
//!   column 1. This is the original's own probe, and its rule: an answer `1;N` with N other
//!   than 1.
//! - `CSI c`: primary device attributes. Terminals answer in order, so this answer means no more
//!   answers are coming. Nearly every terminal gives it.
//!
//! The loop holds the first frame until the last answer or WINDOW. Unambiguous color, version
//! and mode answers also count later and can force one full repaint.
use crate::keys::Report;
use std::time::{Duration, Instant};

/// The probe. It is written once, after `ENTER_TERM`; the first frame clears the screen.
pub const PROBE: &[u8] = b"\x1b[>0q\x1b[?2027$p\x1b[H\x1b]66;w=1; \x1b\\\x1b[6n\x1b[c";

/// Switch grapheme-cluster mode on, as the original does when the terminal can.
pub const GRAPHEME_ON: &[u8] = b"\x1b[?2027h";
/// Put it back. Written on exit, and only if it was found off and we switched it on.
pub const GRAPHEME_OFF: &[u8] = b"\x1b[?2027l";

/// How long after the probe an answer still counts. Past it, and with no `CSI c` answer, the
/// terminal is treated as not answering. A local terminal answers within a millisecond or two.
pub const WINDOW: Duration = Duration::from_millis(250);

/// Whether to probe at all: not on a terminal that calls itself dumb.
pub fn wanted(term: Option<&str>) -> bool {
    term != Some("dumb")
}

/// What an answer changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The terminal takes explicit widths: write non-ASCII cells with OSC 66 and repaint.
    pub explicit_width: bool,
    pub truecolor: bool,
    pub palette: Option<(u8, u32)>,
    /// Mode 2027 was found off and can be switched on. It is put back on exit.
    pub grapheme_on: bool,
    /// The terminal's report on mode 2027 (0 to 4), which the original's choice of width method
    /// follows (see `t3_agents_core::cluster::select_width_method`).
    pub mode_2027: Option<u8>,
    /// The terminal's XTVERSION answer, as sent (`kitty(0.49.2)`, `ghostty 1.4.0`).
    pub xtversion: Option<String>,
}

/// The start-up probe's state.
pub struct Probe {
    until: Instant,
    done: bool,
}

impl Probe {
    pub fn started(now: Instant) -> Self {
        Self {
            until: now + WINDOW,
            done: false,
        }
    }

    /// When the probe stops counting answers.
    pub fn until(&self) -> Instant {
        self.until
    }

    /// Whether answers may still come: the terminal has not yet answered the last question
    /// (`CSI c`, which every terminal answers) and the wait is not over. The first frame is held
    /// until this is false, so it is drawn once, the way the answers say, as the original's is.
    pub fn waiting(&self, now: Instant) -> bool {
        !self.done && now < self.until
    }

    /// Take one answer. A cursor answer outside the probe's window is not ours: `1;2R` is also
    /// what Shift-F3 looks like. The terminal's name and its answer about a mode can be nothing
    /// else, so they count whenever they come, as they do in the original
    /// (`processCapabilitySequence`): a slow terminal, or a busy machine, is still told apart.
    pub fn answer(&mut self, report: Report, now: Instant) -> Change {
        let open = !self.done && now < self.until;
        match report {
            Report::Truecolor => Change { truecolor: true, ..Change::default() },
            Report::Palette { index, rgb } => Change { palette: Some((index,rgb)), ..Change::default() },
            Report::Attributes => {
                self.done = true;
                Change::default()
            }
            Report::Cursor { row: 1, col } if col != 1 && open => Change {
                explicit_width: true,
                ..Change::default()
            },
            // 2 is "reset, and can be set". 1 is already on: nothing to switch and nothing to
            // put back. 0, 3 and 4 say it is unknown or fixed.
            Report::Mode { mode: 2027, value } => Change {
                grapheme_on: value == 2,
                mode_2027: u8::try_from(value).ok(),
                ..Change::default()
            },
            Report::Version(text) => Change {
                xtversion: Some(text),
                ..Change::default()
            },
            _ => Change::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_probe_is_the_original_s_four_queries() {
        // The original asks for XTVERSION first of all.
        assert!(PROBE.starts_with(b"\x1b[>0q\x1b[?2027$p"));
        assert!(PROBE.ends_with(b"\x1b[c"));
        let sizing: &[u8] = b"\x1b]66;w=1; \x1b\\\x1b[6n";
        assert!(PROBE.windows(sizing.len()).any(|w| w == sizing));
    }

    #[test]
    fn the_probe_waits_until_the_last_answer_or_the_end_of_the_window() {
        let now = Instant::now();
        let mut p = Probe::started(now);
        assert!(p.waiting(now));
        p.answer(Report::Mode { mode: 2027, value: 2 }, now);
        assert!(p.waiting(now), "an answer that is not the last does not end the wait");
        p.answer(Report::Attributes, now);
        assert!(!p.waiting(now));
        // A terminal that answers nothing is waited for only as long as the window.
        let silent = Probe::started(now);
        assert!(silent.waiting(now + WINDOW - Duration::from_millis(1)));
        assert!(!silent.waiting(now + WINDOW));
    }

    #[test]
    fn the_version_answer_is_handed_on_as_sent() {
        let now = Instant::now();
        let mut p = Probe::started(now);
        let change = p.answer(Report::Version("ghostty 1.4.0+omarchy.1".into()), now);
        assert_eq!(change.xtversion.as_deref(), Some("ghostty 1.4.0+omarchy.1"));
        assert!(!change.explicit_width && !change.grapheme_on && change.mode_2027.is_none());
        // Past the window, and after the last answer, it still counts: nothing else looks like it.
        let mut p = Probe::started(now);
        p.answer(Report::Attributes, now);
        let late = p.answer(Report::Version("kitty(0.49.2)".into()), now + WINDOW);
        assert_eq!(late.xtversion.as_deref(), Some("kitty(0.49.2)"));
        let late = p.answer(Report::Mode { mode: 2027, value: 2 }, now + WINDOW);
        assert_eq!(late.mode_2027, Some(2));
        // A cursor answer does not: by then it is a key.
        assert_eq!(p.answer(Report::Cursor { row: 1, col: 2 }, now + WINDOW), Change::default());
    }

    #[test]
    fn a_moved_cursor_means_explicit_width_and_an_unmoved_one_does_not() {
        let now = Instant::now();
        let mut p = Probe::started(now);
        assert_eq!(
            p.answer(Report::Cursor { row: 1, col: 1 }, now),
            Change::default()
        );
        assert!(
            p.answer(Report::Cursor { row: 1, col: 2 }, now)
                .explicit_width
        );
        // The row has to be 1: the probe is written at home.
        assert!(
            !p.answer(Report::Cursor { row: 5, col: 2 }, now)
                .explicit_width
        );
    }

    #[test]
    fn mode_2027_is_switched_on_only_when_the_terminal_says_it_is_off_and_can_be_set() {
        let now = Instant::now();
        for (value, on) in [(0, false), (1, false), (2, true), (3, false), (4, false)] {
            let mut p = Probe::started(now);
            let change = p.answer(Report::Mode { mode: 2027, value }, now);
            assert_eq!(change.grapheme_on, on, "reply {value}");
        }
        let mut p = Probe::started(now);
        assert_eq!(
            p.answer(
                Report::Mode {
                    mode: 2031,
                    value: 2
                },
                now
            ),
            Change::default()
        );
    }

    #[test]
    fn answers_after_the_window_or_after_the_last_answer_are_not_the_probe_s() {
        let now = Instant::now();
        let mut p = Probe::started(now);
        let late = now + WINDOW;
        assert_eq!(
            p.answer(Report::Cursor { row: 1, col: 2 }, late),
            Change::default()
        );
        let mut p = Probe::started(now);
        p.answer(Report::Attributes, now);
        assert_eq!(
            p.answer(Report::Cursor { row: 1, col: 2 }, now),
            Change::default()
        );
    }

    #[test]
    fn a_dumb_terminal_is_not_asked() {
        assert!(!wanted(Some("dumb")));
        assert!(wanted(Some("xterm-256color")));
        assert!(wanted(None));
    }
}
