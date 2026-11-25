//! Raw-mode key and SGR mouse decoder.
//!
//! The names, the allow-list and the modifier rule are the ones `model::State::action_at` takes.
//! A lone Escape is reported only after a short quiet gap, so it is not the first
//! byte of an arrow or a mouse sequence.
//!
//! The terminal's answers to the start-up probe (`caps.rs`) arrive on the same stream. They are
//! recognised here and handed over as [`Report`]s; none of them can become a key. While answers
//! are expected ([`Parser::expect_replies`]) an unfinished sequence, a lone Escape included, waits
//! for the rest of the answer instead of timing out after [`ESC_WAIT`]. A partial sequence that
//! is given up on still has its tail swallowed when it arrives, so `?62;4c` cannot press `?`.
//!
//! A bracketed paste (`ESC [ 200 ~` … `ESC [ 201 ~`, which the presenter asks the terminal for with
//! mode 2004) is dropped whole: everything between the markers is discarded unread, so pasted
//! text cannot press a key, however many reads it arrives in and whatever bytes it holds. The
//! original does the same. Where it waits for the end marker for as long as it takes, this parser
//! gives up after [`PASTE_QUIET`] without a byte, so a lost end marker cannot swallow the user's
//! input for good (see [`Parser::deadline`]).
use std::time::{Duration, Instant};

pub const ESC_WAIT: Duration = Duration::from_millis(25);

/// How long a paste may go without a byte before its end marker is given up on.
///
/// The original has no such limit: once it has seen the start marker it discards every byte until
/// the end marker, however long that takes, so a lost end marker leaves it deaf to the keyboard
/// (a quit key and Ctrl-C included) until it is killed. The bound is a deliberate difference.
/// Two seconds is far longer than the gap between two reads of a paste on any working link, and
/// short enough that someone who sees the dashboard stop answering has not yet given up on it.
/// It runs from the last byte received, so what is typed after the paste got lost counts as
/// nothing until the keyboard has been quiet that long.
pub const PASTE_QUIET: Duration = Duration::from_secs(2);

/// No XTVERSION answer is longer: a name of at most 64 bytes, a version of at most 32, and the
/// framing. A dropped DCS string that has gone on this long is not swallowed further.
const DCS_TAIL_MAX: usize = 128;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// What the terminal said back to a query. Anything it sends unasked is one of these or ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// `CSI row ; col R`, the answer to `CSI 6 n`.
    Cursor { row: i32, col: i32 },
    /// `CSI ? mode ; value $ y`, the answer to a mode query. 0 unknown, 1 set, 2 reset,
    /// 3 permanently set, 4 permanently reset.
    Mode { mode: i32, value: i32 },
    /// `CSI ? ... c`, the answer to `CSI c`. A terminal answers its queries in order, so this
    /// one means the others are not coming.
    Attributes,
    /// `DCS > | text ST`, the answer to `CSI > 0 q` (XTVERSION): the terminal's name and version,
    /// as `kitty(0.49.2)` or `ghostty 1.4.0`.
    Version(String),
    Truecolor,
    Palette { index: u8, rgb: u32 },
    /// `OSC 11 ; color ST`, the answer to `OSC 11 ; ?`: the terminal's background.
    Background(u32),
    /// `CSI ? 997 ; 1 n` (dark) or `; 2 n` (light): the terminal's color scheme changed, reported
    /// while mode 2031 is on.
    ColorScheme { dark: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// An allowed key name: `up`, `j`, `space`, `escape`, …
    Key(&'static str),
    /// Ctrl-C. The presenter quits, the same way the window does.
    Quit,
    /// Left button press, 0-based cell coordinates.
    Press { x: i32, y: i32 },
    /// Wheel. `down` is the same direction as `on_wheel`'s `dy < 0`.
    Wheel { x: i32, y: i32, down: bool },
    /// The terminal answered a query.
    Report(Report),
}

#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    esc_at: Option<Instant>,
    /// Until when an unfinished sequence may still be the start of an answer to a query.
    expect_until: Option<Instant>,
    /// A partial CSI was dropped: its parameter bytes and final byte are dropped when they come.
    tail: bool,
    /// A partial DCS string was dropped: everything up to its terminator is dropped when it comes.
    dcs_tail: bool,
    /// How many more bytes of a dropped DCS string are swallowed before giving up on its end.
    dcs_budget: usize,
    /// A bare `ESC P`, or `ESC P >`, was given up on: if the rest of `> |` follows, it was the head
    /// of the XTVERSION answer. How much of the head had come.
    after_esc_p: Option<usize>,
    /// An unfinished DCS string is waiting for its terminator.
    dcs: bool,
    /// Inside a bracketed paste: when the last byte of it arrived. Nothing is kept of the paste
    /// but, in `buf`, the end of what was read if that could be the start of the end marker.
    paste: Option<Instant>,
}

impl Parser {
    pub fn new() -> Self {
        Self::default()
    }

    /// When the parser next needs to be told the time: when a pending partial sequence is given
    /// up on, or when a paste that has gone quiet is, whichever is first.
    pub fn deadline(&self) -> Option<Instant> {
        let esc = self.esc_at.map(|t| self.due(t));
        let paste = self.paste.map(|t| t + PASTE_QUIET);
        match (esc, paste) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Whether the input is inside a bracketed paste, whose bytes are being dropped.
    pub fn pasting(&self) -> bool {
        self.paste.is_some()
    }

    /// Answers to queries are on their way until `until`, or until the terminal's answer to
    /// `CSI c` arrives, whichever is first.
    pub fn expect_replies(&mut self, until: Instant) {
        self.expect_until = Some(until);
    }

    fn due(&self, since: Instant) -> Instant {
        // Unambiguous color strings can arrive in pieces after the startup wait too.
        let color = self.buf.get(1) == Some(&b']') || (self.buf.len() >= 4 && matches!(&self.buf[2..4], b"1+" | b"0+"));
        let due = since + if color { crate::caps::WINDOW } else { ESC_WAIT };
        match self.expect_until {
            Some(until) => due.max(until),
            None => due,
        }
    }

    pub fn push(&mut self, input: &[u8], now: Instant) -> Vec<Event> {
        self.abandon_quiet_paste(now);
        self.buf.extend_from_slice(input);
        if self.paste.is_some() && !input.is_empty() {
            self.paste = Some(now);
        }
        self.drain(now)
    }

    pub fn timed_out(&mut self, now: Instant) -> Vec<Event> {
        self.abandon_quiet_paste(now);
        self.drain(now)
    }

    /// Give up on a paste whose end marker has not come and which has gone quiet. What was held
    /// back as a possible start of the end marker belonged to the paste and goes with it, and its
    /// remaining bytes are dropped when they arrive, as for any sequence given up on.
    fn abandon_quiet_paste(&mut self, now: Instant) {
        let Some(last) = self.paste else { return };
        if now < last + PASTE_QUIET {
            return;
        }
        self.paste = None;
        if self.buf.len() >= 2 {
            self.tail = true;
        }
        self.buf.clear();
        self.esc_at = None;
    }

    fn drain(&mut self, now: Instant) -> Vec<Event> {
        let mut out = Vec::new();
        loop {
            if self.paste.is_some() {
                // Everything up to the end marker is the paste, and is dropped unread.
                if let Some(at) = find(&self.buf, PASTE_END) {
                    self.buf.drain(..at + PASTE_END.len());
                    self.paste = None;
                    self.esc_at = None;
                    continue;
                }
                // The end marker may be cut between two reads: keep only a tail that could be
                // the start of it, so a paste of any size costs no memory.
                let keep = (1..PASTE_END.len())
                    .rev()
                    .find(|&n| self.buf.ends_with(&PASTE_END[..n]))
                    .unwrap_or(0);
                self.buf.drain(..self.buf.len() - keep);
                break;
            }
            if self.buf.is_empty() {
                self.esc_at = None;
                break;
            }
            if let Some(head) = self.after_esc_p {
                // A slow terminal sent `ESC P` and, after the Escape gap, `> | name version ST`.
                let rest: &[u8] = if head == 2 { b">|" } else { b"|" };
                if !self.buf.is_empty() && rest.starts_with(&self.buf) && self.buf.len() < rest.len() {
                    break;
                }
                self.after_esc_p = None;
                if self.buf.starts_with(rest) {
                    self.dcs_tail = true;
                    self.dcs_budget = DCS_TAIL_MAX;
                }
            }
            if self.dcs_tail {
                // The rest of a string that was given up on, up to its terminator or, if that never
                // comes, a bound no answer is longer than.
                match find(&self.buf, b"\x1b\\").map(|n| (n, 2)).or_else(|| self.buf.iter().position(|b| *b == 7).map(|n| (n, 1))) {
                    Some((at, len)) => {
                        self.buf.drain(..at + len);
                        self.dcs_tail = false;
                        continue;
                    }
                    None => {
                        // Keep a trailing ESC: it may be the first half of the terminator.
                        let keep = usize::from(self.buf.last() == Some(&0x1b));
                        let dropped = self.buf.len() - keep;
                        self.buf.drain(..dropped);
                        self.dcs_budget = self.dcs_budget.saturating_sub(dropped);
                        if self.dcs_budget == 0 {
                            self.dcs_tail = false;
                        }
                        break;
                    }
                }
            }
            if self.tail {
                // The rest of a sequence that was given up on: parameters, then a final byte.
                match self.buf[0] {
                    0x20..=0x3f => {
                        self.buf.remove(0);
                        continue;
                    }
                    0x40..=0x7e => {
                        self.buf.remove(0);
                        self.tail = false;
                        continue;
                    }
                    _ => self.tail = false,
                }
            }
            if self.buf[0] != 0x1b {
                match take_plain(&mut self.buf) {
                    Plain::Event(ev) => out.push(ev),
                    Plain::Ignore => {}
                    Plain::Need(n) if self.buf.len() < n => break,
                    Plain::Need(_) => {
                        // A broken UTF-8 lead. Drop one byte and resync.
                        self.buf.remove(0);
                    }
                }
                continue;
            }
            let replies = self.dcs || self.expect_until.is_some_and(|until| now < until);
            match classify(&self.buf, replies) {
                Class::Done { len, event } => {
                    if self.buf[..len] == *PASTE_START {
                        self.paste = Some(now);
                    }
                    self.buf.drain(..len);
                    self.esc_at = None;
                    self.dcs = false;
                    if let Some(event) = event {
                        if event == Event::Report(Report::Attributes) {
                            // The last answer to the start-up probe: nothing more is coming.
                            self.expect_until = None;
                        }
                        out.push(event);
                    }
                }
                Class::Wait => {
                    // Whether what waits is certainly a DCS string, as against a bare ESC P.
                    self.dcs = self.buf.get(1) == Some(&b']') || self.buf.get(1) == Some(&b'P')
                        && (replies || (self.buf.len() >= 4 && (self.buf[2..4] == *b">|" || self.buf[2..4] == *b"1+" || self.buf[2..4] == *b"0+")));
                    if self.esc_at.is_none() {
                        self.esc_at = Some(now);
                    }
                    let due = self.due(self.esc_at.unwrap());
                    if now >= due {
                        if self.buf.len() == 1 {
                            out.push(Event::Key("escape"));
                        } else if self.buf[1] == b'[' {
                            self.tail = true;
                        } else if matches!(self.buf[1], b'P' | b']') && self.dcs {
                            self.dcs_tail = true;
                            self.dcs_budget = DCS_TAIL_MAX;
                            self.dcs = false;
                            // An Escape at the very end may be the first half of the terminator.
                            if self.buf.last() == Some(&0x1b) {
                                self.buf.clear();
                                self.buf.push(0x1b);
                                self.esc_at = None;
                                break;
                            }
                        } else if self.buf == b"\x1bP" || self.buf == b"\x1bP>" {
                            self.after_esc_p = Some(self.buf.len());
                        }
                        // A partial CSI or SS3 that never finished is discarded.
                        self.buf.clear();
                        self.esc_at = None;
                    }
                    break;
                }
            }
        }
        out
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

enum Plain {
    Event(Event),
    Ignore,
    Need(usize),
}

fn take_plain(buf: &mut Vec<u8>) -> Plain {
    let b = buf[0];
    if b == 0x03 {
        buf.remove(0);
        return Plain::Event(Event::Quit);
    }
    // Enter is CR in raw mode. LF is what some stacks send for the same key.
    if b == b'\r' || b == b'\n' {
        buf.remove(0);
        return Plain::Event(Event::Key("enter"));
    }
    if b == b'\t' {
        buf.remove(0);
        return Plain::Event(Event::Key("tab"));
    }
    if b < 0x20 || b == 0x7f {
        buf.remove(0);
        return Plain::Ignore;
    }
    if b < 0x80 {
        buf.remove(0);
        return match allowed_char(b as char) {
            Some(name) => Plain::Event(Event::Key(name)),
            None => Plain::Ignore,
        };
    }
    let need = match b {
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => return Plain::Need(1),
    };
    if buf.len() < need {
        return Plain::Need(need);
    }
    // The allow-list is ASCII. Consume the scalar so the next key stays aligned.
    buf.drain(..need);
    Plain::Ignore
}

fn allowed_char(c: char) -> Option<&'static str> {
    Some(match c {
        ' ' => "space",
        'j' => "j",
        'k' => "k",
        'J' => "J",
        'K' => "K",
        'g' => "g",
        'G' => "G",
        '?' => "?",
        'h' => "h",
        'q' => "q",
        'o' => "o",
        'a' => "a",
        'n' => "n",
        't' => "t",
        'p' => "p",
        'r' => "r",
        's' => "s",
        'i' => "i",
        'e' => "e",
        'm' => "m",
        'x' => "x",
        'f' => "f",
        'c' => "c",
        'C' => "C",
        'w' => "w",
        _ => return None,
    })
}

enum Class {
    Done { len: usize, event: Option<Event> },
    Wait,
}

fn classify(buf: &[u8], replies: bool) -> Class {
    if buf.len() == 1 {
        return Class::Wait;
    }
    match buf[1] {
        b'[' => match csi_end(buf) {
            Some(len) => Class::Done {
                len,
                event: parse_csi(&buf[..len]),
            },
            None if buf.len() > 64 => Class::Done {
                len: buf.len(),
                event: None,
            },
            None => Class::Wait,
        },
        // A DCS string is an answer to a query: while answers are expected, and later too when it
        // starts `ESC P > |` (the XTVERSION answer, which a slow terminal sends after the wait). Any
        // other ESC P is Alt+Shift+P and must not hold back the keys that follow.
        b']' => {
            let end = buf[2..].iter().position(|b| *b == 7).map(|n| (n,1))
                .or_else(|| find(&buf[2..], b"\x1b\\").map(|n| (n,2)));
            match end {
                Some((n,t)) => {
                    let body = &buf[2..2+n];
                    let event = crate::color::osc_palette(body).map(|(index,rgb)| Event::Report(Report::Palette {index,rgb}))
                        .or_else(|| crate::color::osc_background(body).map(|rgb| Event::Report(Report::Background(rgb))));
                    Class::Done { len: n+2+t, event }
                }
                None if buf.len() > 512 => Class::Done {len: buf.len(), event: None},
                None => Class::Wait,
            }
        }
        b'P' => {
            let version = buf.len() >= 4 && matches!(&buf[2..4], b">|" | b"1+" | b"0+");
            if replies || version {
                match find(&buf[2..], b"\x1b\\") {
                    Some(at) => Class::Done {
                        len: at + 4,
                        event: parse_dcs(&buf[2..2 + at]),
                    },
                    // No reply is this long: give up on the string and drop what has come of it.
                    None if buf.len() > 512 => Class::Done {
                        len: buf.len(),
                        event: None,
                    },
                    None => Class::Wait,
                }
            } else if buf.len() < 5 && [b">|".as_slice(), b"1+r", b"0+r"].iter().any(|head| head.starts_with(&buf[2..])) {
                // Could still become `ESC P > |`: a moment to see.
                Class::Wait
            } else {
                Class::Done {
                    len: 2,
                    event: None,
                }
            }
        }
        b'O' => {
            if buf.len() < 3 {
                Class::Wait
            } else {
                Class::Done {
                    len: 3,
                    event: parse_ss3(buf[2]),
                }
            }
        }
        _ => Class::Done {
            // ESC then a non-introducer is Alt+key. Modified printables are ignored.
            len: 2,
            event: None,
        },
    }
}

/// The terminal's answers to the queries in `caps.rs` that come as a DCS string. Others are
/// dropped whole.
fn parse_dcs(body: &[u8]) -> Option<Event> {
    if crate::color::xtgettcap(body) { return Some(Event::Report(Report::Truecolor)); }
    let text = body.strip_prefix(b">|")?;
    Some(Event::Report(Report::Version(String::from_utf8_lossy(text).into_owned())))
}

fn csi_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 3 {
        return None;
    }
    for (i, b) in buf.iter().enumerate().skip(2) {
        if (0x40..=0x7e).contains(b) {
            return Some(i + 1);
        }
        if !(0x20..=0x3f).contains(b) {
            return Some(i);
        }
    }
    None
}

fn parse_ss3(b: u8) -> Option<Event> {
    Some(Event::Key(match b {
        b'A' => "up",
        b'B' => "down",
        b'C' => "right",
        b'D' => "left",
        b'H' => "home",
        b'F' => "end",
        b'M' => "enter",
        _ => return None,
    }))
}

fn parse_csi(seq: &[u8]) -> Option<Event> {
    let final_b = *seq.last()?;
    let body = &seq[2..seq.len() - 1];
    if body.first() == Some(&b'<') {
        return parse_mouse(body, final_b);
    }
    if let Some(report) = parse_report(body, final_b) {
        return Some(Event::Report(report));
    }
    // Arrows, Home, End, PageUp and PageDown stay active with Ctrl/Alt/Shift.
    let name = match final_b {
        b'A' => "up",
        b'B' => "down",
        b'C' => "right",
        b'D' => "left",
        b'H' => "home",
        b'F' => "end",
        // OpenTUI's src/lib/parse.keypress.ts names CSI Z Tab and marks it shifted.
        b'Z' if body.is_empty() => "shift-tab",
        b'~' => match numbers(body).first().copied().unwrap_or(0) {
            1 | 7 => "home",
            4 | 8 => "end",
            5 => "pageup",
            6 => "pagedown",
            _ => return None,
        },
        _ => return None,
    };
    Some(Event::Key(name))
}

/// The terminal's answers to the queries in `caps.rs`. Other answers fall through and are ignored.
fn parse_report(body: &[u8], final_b: u8) -> Option<Report> {
    match (body.first(), final_b) {
        (Some(b'?'), b'c') => Some(Report::Attributes),
        (Some(b'?'), b'y') if body.last() == Some(&b'$') => {
            match numbers(&body[1..body.len() - 1])[..] {
                [mode, value] => Some(Report::Mode { mode, value }),
                _ => None,
            }
        }
        (Some(b'0'..=b'9'), b'R') => match numbers(body)[..] {
            [row, col] => Some(Report::Cursor { row, col }),
            _ => None,
        },
        (Some(b'?'), b'n') => match numbers(&body[1..])[..] {
            [997, 1] => Some(Report::ColorScheme { dark: true }),
            [997, 2] => Some(Report::ColorScheme { dark: false }),
            _ => None,
        },
        _ => None,
    }
}

fn parse_mouse(body: &[u8], final_b: u8) -> Option<Event> {
    let nums = numbers(body);
    if nums.len() < 3 {
        return None;
    }
    let button = nums[0];
    let x = nums[1].saturating_sub(1);
    let y = nums[2].saturating_sub(1);
    // Motion reports (bit 5) are not clicks. Shift/Alt/Ctrl are bits 2..4.
    if button & 32 != 0 {
        return None;
    }
    let base = button & !0b0001_1100;
    if base == 64 {
        return Some(Event::Wheel { x, y, down: false });
    }
    if base == 65 {
        return Some(Event::Wheel { x, y, down: true });
    }
    if final_b == b'M' && base == 0 {
        return Some(Event::Press { x, y });
    }
    None
}

fn numbers(body: &[u8]) -> Vec<i32> {
    let mut out = Vec::new();
    let mut cur: Option<i32> = None;
    let mut any = false;
    for &b in body {
        if b.is_ascii_digit() {
            cur = Some(cur.unwrap_or(0).saturating_mul(10) + i32::from(b - b'0'));
            any = true;
        } else if b == b';' {
            out.push(cur.unwrap_or(0));
            cur = None;
            any = true;
        }
    }
    if cur.is_some() || any {
        out.push(cur.unwrap_or(0));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::new();
        let now = Instant::now();
        p.push(bytes, now)
    }

    #[test]
    fn background_and_color_scheme_answers_are_reports_and_c_is_a_key() {
        assert_eq!(parse(b"\x1b]11;rgb:fafa/fafa/fafa\x1b\\"), vec![Event::Report(Report::Background(0xfafafa))]);
        assert_eq!(parse(b"\x1b]11;rgb:1a1a/1b1b/2626\x07"), vec![Event::Report(Report::Background(0x1a1b26))]);
        assert_eq!(parse(b"\x1b[?997;1n"), vec![Event::Report(Report::ColorScheme { dark: true })]);
        assert_eq!(parse(b"\x1b[?997;2n"), vec![Event::Report(Report::ColorScheme { dark: false })]);
        assert_eq!(parse(b"cC"), vec![Event::Key("c"), Event::Key("C")]);
    }

    #[test]
    fn arrows_home_paging_and_the_allow_list() {
        assert_eq!(parse(b"\x1b[A"), vec![Event::Key("up")]);
        assert_eq!(parse(b"\x1b[B"), vec![Event::Key("down")]);
        assert_eq!(parse(b"\x1b[C"), vec![Event::Key("right")]);
        assert_eq!(parse(b"\x1b[D"), vec![Event::Key("left")]);
        assert_eq!(parse(b"\x1bOA"), vec![Event::Key("up")]);
        assert_eq!(parse(b"\x1b[H"), vec![Event::Key("home")]);
        assert_eq!(parse(b"\x1b[F"), vec![Event::Key("end")]);
        assert_eq!(parse(b"\x1b[1~"), vec![Event::Key("home")]);
        assert_eq!(parse(b"\x1b[4~"), vec![Event::Key("end")]);
        assert_eq!(parse(b"\x1b[5~"), vec![Event::Key("pageup")]);
        assert_eq!(parse(b"\x1b[6~"), vec![Event::Key("pagedown")]);
        assert_eq!(parse(b"\x1b[7~"), vec![Event::Key("home")]);
        assert_eq!(parse(b"\x1b[8~"), vec![Event::Key("end")]);
        assert_eq!(parse(b"\r"), vec![Event::Key("enter")]);
        assert_eq!(
            parse(b"jJ? gq"),
            vec![
                Event::Key("j"),
                Event::Key("J"),
                Event::Key("?"),
                Event::Key("space"),
                Event::Key("g"),
                Event::Key("q"),
            ]
        );
        assert_eq!(parse(b"z1\x7f"), Vec::<Event>::new());
    }

    #[test]
    fn modifiers_keep_navigation_and_drop_other_keys() {
        assert_eq!(parse(b"\x1b[1;5A"), vec![Event::Key("up")]);
        assert_eq!(parse(b"\x1b[1;3B"), vec![Event::Key("down")]);
        assert_eq!(parse(b"\x1b[5;5~"), vec![Event::Key("pageup")]);
        assert_eq!(parse(b"\x1b[1;2H"), vec![Event::Key("home")]);
        assert_eq!(parse(b"\x03"), vec![Event::Quit]);
        assert_eq!(parse(b"\x1bj"), Vec::<Event>::new(), "alt-j is ignored");
        assert!(parse(b"\x01").is_empty(), "ctrl-a is ignored");
    }

    #[test]
    fn lone_escape_waits_and_a_sequence_does_not() {
        let mut p = Parser::new();
        let now = Instant::now();
        assert!(p.push(b"\x1b", now).is_empty());
        assert!(p.deadline().is_some());
        assert_eq!(p.timed_out(now + ESC_WAIT), vec![Event::Key("escape")]);
        let mut p = Parser::new();
        assert!(p.push(b"\x1b", now).is_empty());
        assert_eq!(
            p.push(b"[A", now + Duration::from_millis(5)),
            vec![Event::Key("up")]
        );
        assert!(p.deadline().is_none());
    }

    #[test]
    fn sgr_press_wheel_and_release() {
        assert_eq!(parse(b"\x1b[<0;12;4M"), vec![Event::Press { x: 11, y: 3 }]);
        assert_eq!(
            parse(b"\x1b[<0;12;4m"),
            Vec::<Event>::new(),
            "release is not a press"
        );
        assert_eq!(
            parse(b"\x1b[<65;3;8M"),
            vec![Event::Wheel {
                x: 2,
                y: 7,
                down: true
            }]
        );
        assert_eq!(
            parse(b"\x1b[<64;3;8M"),
            vec![Event::Wheel {
                x: 2,
                y: 7,
                down: false
            }]
        );
        assert_eq!(
            parse(b"\x1b[<69;3;8M"),
            vec![Event::Wheel {
                x: 2,
                y: 7,
                down: true
            }],
            "shift does not change the wheel"
        );
        assert!(parse(b"\x1b[<32;3;8M").is_empty(), "motion is not a click");
    }

    // ── The terminal's answers to the start-up probe ─────────────────────────

    const CPR: &[u8] = b"\x1b[1;2R";
    const MODE: &[u8] = b"\x1b[?2027;2$y";
    const DA1: &[u8] = b"\x1b[?62;4c";

    #[test]
    fn answers_are_reports_and_never_keys() {
        assert_eq!(
            parse(CPR),
            vec![Event::Report(Report::Cursor { row: 1, col: 2 })]
        );
        assert_eq!(
            parse(MODE),
            vec![Event::Report(Report::Mode {
                mode: 2027,
                value: 2
            })]
        );
        assert_eq!(parse(DA1), vec![Event::Report(Report::Attributes)]);
        assert_eq!(parse(b"\x1b[?1;2c"), vec![Event::Report(Report::Attributes)]);
        // Other answers a terminal can give are dropped without a key.
        for other in [
            &b"\x1b[?0u"[..],
            b"\x1b[4;800;1600t",
            b"\x1b[0n",
            b"\x1b[?997;1n",
            b"\x1b[I",
            b"\x1b[O",
            b"\x1b[200~",
            b"\x1b[201~",
        ] {
            assert!(
                !parse(other).iter().any(|e| matches!(e, Event::Key(_))),
                "{other:?} made a key"
            );
        }
        // Between keys, in the order they arrived.
        let mut all = b"j".to_vec();
        all.extend_from_slice(MODE);
        all.extend_from_slice(CPR);
        all.extend_from_slice(b"k");
        all.extend_from_slice(DA1);
        assert_eq!(
            parse(&all),
            vec![
                Event::Key("j"),
                Event::Report(Report::Mode {
                    mode: 2027,
                    value: 2
                }),
                Event::Report(Report::Cursor { row: 1, col: 2 }),
                Event::Key("k"),
                Event::Report(Report::Attributes),
            ]
        );
    }

    #[test]
    fn an_answer_in_pieces_waits_for_the_rest_while_answers_are_expected() {
        let t0 = Instant::now();
        // Every split point of every answer, with a gap far longer than ESC_WAIT between the
        // pieces: no key, and exactly one report.
        for answer in [CPR, MODE, DA1] {
            for cut in 1..answer.len() {
                let mut p = Parser::new();
                p.expect_replies(t0 + Duration::from_millis(250));
                assert!(p.push(&answer[..cut], t0).is_empty());
                let due = p.deadline().unwrap();
                assert!(due >= t0 + Duration::from_millis(250), "waits for the window");
                // Nothing happens at the usual Escape gap.
                assert!(p.timed_out(t0 + Duration::from_millis(100)).is_empty()
                    || p.deadline().is_none());
                let got = p.push(&answer[cut..], t0 + Duration::from_millis(120));
                assert_eq!(got.len(), 1, "cut {cut} of {answer:?}: {got:?}");
                assert!(matches!(got[0], Event::Report(_)));
            }
        }
    }

    /// The XTVERSION answer is a DCS string. Its letters (`kitty(0.49.2)` holds `k` and `t`, both
    /// keys) must never be read as typing, whole, in pieces, or given up on and then finished.
    #[test]
    fn the_version_answer_is_a_report_and_never_a_key() {
        let t0 = Instant::now();
        let answer: &[u8] = b"\x1bP>|kitty(0.49.2)\x1b\\";
        let mut p = Parser::new();
        p.expect_replies(t0 + Duration::from_millis(250));
        assert_eq!(
            p.push(answer, t0),
            vec![Event::Report(Report::Version("kitty(0.49.2)".into()))]
        );
        // Another DCS reply (XTGETTCAP, say) is dropped whole.
        let mut p = Parser::new();
        p.expect_replies(t0 + Duration::from_millis(250));
        assert!(p.push(b"\x1bP1+r4d73=\x1b\\j", t0).len() == 1);
        // Every split point, with a long gap between the pieces.
        for cut in 1..answer.len() {
            let mut p = Parser::new();
            p.expect_replies(t0 + Duration::from_millis(250));
            assert!(p.push(&answer[..cut], t0).is_empty());
            let got = p.push(&answer[cut..], t0 + Duration::from_millis(120));
            assert_eq!(got.len(), 1, "cut {cut}: {got:?}");
            assert!(matches!(got[0], Event::Report(Report::Version(_))));
        }
        // Given up on (the terminal went quiet past the window), then the rest arrives: no key, and
        // the next key is a key.
        for cut in 2..answer.len() - 1 {
            let mut p = Parser::new();
            p.expect_replies(t0 + Duration::from_millis(250));
            assert!(p.push(&answer[..cut], t0).is_empty());
            assert!(p.timed_out(t0 + Duration::from_millis(300)).is_empty());
            let late = t0 + Duration::from_secs(1);
            assert!(p.push(&answer[cut..], late).is_empty(), "cut {cut}");
            assert_eq!(p.push(b"j", late), vec![Event::Key("j")]);
        }
        // A terminal slower than the wait: the whole answer, and every split of it with a gap
        // longer than the Escape gap, arrives after the window and is still no key. (Cut after the
        // Escape alone is not a case: an Escape that nothing follows is the Escape key.)
        let late = t0 + Duration::from_secs(1);
        for cut in std::iter::once(0).chain(2..=answer.len()) {
            let mut p = Parser::new();
            p.expect_replies(t0 + Duration::from_millis(250));
            let mut now = late;
            let mut got = Vec::new();
            for piece in [&answer[..cut], &answer[cut..]] {
                if !piece.is_empty() {
                    got.extend(p.push(piece, now));
                }
                now += Duration::from_millis(80);
                got.extend(p.timed_out(now));
            }
            assert!(!got.iter().any(|e| matches!(e, Event::Key(_))), "cut {cut}: {got:?}");
            assert_eq!(p.push(b"j", now), vec![Event::Key("j")], "cut {cut}");
        }
        // Once answers are no longer expected, ESC P is Alt+Shift+P and holds nothing back.
        let mut p = Parser::new();
        assert_eq!(p.push(b"\x1bPj", t0), vec![Event::Key("j")]);
    }

    #[test]
    fn a_lone_escape_waits_for_the_window_and_then_is_a_key() {
        let t0 = Instant::now();
        let mut p = Parser::new();
        p.expect_replies(t0 + Duration::from_millis(250));
        assert!(p.push(b"\x1b", t0).is_empty());
        assert!(p.deadline().unwrap() >= t0 + Duration::from_millis(250));
        assert_eq!(
            p.timed_out(t0 + Duration::from_millis(250)),
            vec![Event::Key("escape")]
        );
        // The last answer ends the wait: a lone Escape is a key after the usual gap again.
        let mut p = Parser::new();
        p.expect_replies(t0 + Duration::from_millis(250));
        assert_eq!(p.push(DA1, t0).len(), 1);
        assert!(p.push(b"\x1b", t0).is_empty());
        assert_eq!(p.deadline().unwrap(), t0 + ESC_WAIT);
        assert_eq!(p.timed_out(t0 + ESC_WAIT), vec![Event::Key("escape")]);
    }

    #[test]
    fn the_tail_of_a_late_answer_is_not_a_key() {
        let t0 = Instant::now();
        // The terminal was slow: the head of the answer was given up on, then the rest comes.
        // `?62;4c` would be Help (`?`) if it were read as typing.
        for (head, tail) in [
            (&b"\x1b["[..], &b"?62;4c"[..]),
            (b"\x1b[?62", b";4c"),
            (b"\x1b[?2027;", b"2$y"),
            (b"\x1b[1;", b"2R"),
        ] {
            let mut p = Parser::new();
            assert!(p.push(head, t0).is_empty());
            assert!(p.timed_out(t0 + ESC_WAIT).is_empty());
            assert!(
                p.push(tail, t0 + Duration::from_secs(1)).is_empty(),
                "{tail:?} after {head:?}"
            );
            // And the next key is a key.
            assert_eq!(
                p.push(b"j", t0 + Duration::from_secs(1)),
                vec![Event::Key("j")]
            );
        }
        // A lone Escape that timed out is a key press and leaves no tail behind.
        let mut p = Parser::new();
        assert!(p.push(b"\x1b", t0).is_empty());
        assert_eq!(p.timed_out(t0 + ESC_WAIT), vec![Event::Key("escape")]);
        assert_eq!(p.push(b"?", t0 + ESC_WAIT), vec![Event::Key("?")]);
    }

    // ── Bracketed paste ──────────────────────────────────────────────────────

    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";

    fn pasted(body: &[u8]) -> Vec<u8> {
        [START, body, END].concat()
    }

    /// Every key the allow-list has, an Enter, a Ctrl-C, an Escape, arrows, all three probe
    /// answers, mouse reports, a focus report and an OSC: none may come out of a paste.
    fn hostile_paste() -> Vec<u8> {
        let mut body = b"qqqjjkk?xnapt \r\n\x03\x1b".to_vec();
        for part in [
            &b"\x1b[A\x1b[B\x1bOA\x1b[5~"[..],
            CPR,
            MODE,
            DA1,
            b"\x1b[<0;12;4M\x1b[<65;3;8M\x1b[<64;3;8m",
            b"\x1b[I\x1b[O\x1b]52;c;AAAA\x07",
            b"\x1b[200~",
            "q\u{e9}\u{1f35e}q".as_bytes(),
        ] {
            body.extend_from_slice(part);
        }
        body
    }

    #[test]
    fn a_paste_is_dropped_whatever_it_holds() {
        assert!(parse(&pasted(b"qqqq")).is_empty());
        assert!(parse(&pasted(b"")).is_empty());
        assert!(parse(&pasted(b"q\nq\nj\n")).is_empty());
        assert!(parse(&pasted(&hostile_paste())).is_empty());
        // A start marker inside a paste is data: the first end marker ends it.
        assert_eq!(
            parse(&[START, b"qq", START, b"qq", END, b"j"].concat()),
            vec![Event::Key("j")]
        );
    }

    #[test]
    fn keys_around_a_paste_are_keys() {
        let mut bytes = b"j".to_vec();
        bytes.extend_from_slice(&pasted(b"qqq"));
        bytes.extend_from_slice(b"k");
        bytes.extend_from_slice(&pasted(b"q"));
        bytes.extend_from_slice(&pasted(b"q"));
        bytes.extend_from_slice(b"\x1b[B");
        assert_eq!(
            parse(&bytes),
            vec![Event::Key("j"), Event::Key("k"), Event::Key("down")]
        );
        // A stray end marker outside a paste is nothing.
        assert_eq!(parse(&[END, b"j"].concat()), vec![Event::Key("j")]);
        // Text after the first end marker is typing, not paste.
        assert_eq!(
            parse(&[START, b"a", END, b"q"].concat()),
            vec![Event::Key("q")]
        );
    }

    #[test]
    fn a_paste_in_pieces_is_dropped_at_every_split_point() {
        let t0 = Instant::now();
        let tail_key = b"j";
        for whole in [
            [pasted(&hostile_paste()), tail_key.to_vec()].concat(),
            [pasted(b"qq"), tail_key.to_vec()].concat(),
            [pasted(b""), tail_key.to_vec()].concat(),
        ] {
            // One cut anywhere, with a gap far longer than ESC_WAIT but inside PASTE_QUIET.
            for cut in 0..=whole.len() {
                let mut p = Parser::new();
                let mut got = p.push(&whole[..cut], t0);
                got.extend(p.timed_out(t0 + Duration::from_millis(100)));
                got.extend(p.push(&whole[cut..], t0 + Duration::from_millis(200)));
                // A start marker cut in two with 100 ms between the pieces is past ESC_WAIT, so
                // the paste is not recognised, here and in the original (its wait is 20 ms):
                // `a_start_marker_cut_and_given_up_on_leaks_the_paste_as_the_original_does`.
                if cut > 0 && cut < START.len() {
                    continue;
                }
                assert_eq!(got, vec![Event::Key("j")], "cut at {cut} of {} bytes", whole.len());
            }
            // Every byte on its own, as slowly as ESC_WAIT allows inside the paste.
            let mut p = Parser::new();
            let mut got = Vec::new();
            for (i, b) in whole.iter().enumerate() {
                got.extend(p.push(&[*b], t0 + Duration::from_millis(i as u64)));
            }
            assert_eq!(got, vec![Event::Key("j")], "one byte at a time");
        }
    }

    #[test]
    fn the_end_marker_cut_between_reads_ends_the_paste() {
        let t0 = Instant::now();
        for cut in 1..END.len() {
            let mut p = Parser::new();
            assert!(p.push(&[START, b"qq", &END[..cut]].concat(), t0).is_empty());
            assert!(p.pasting());
            // The piece of the marker is held, not typed, however long the gap (inside the bound).
            assert!(p.timed_out(t0 + Duration::from_secs(1)).is_empty());
            assert_eq!(
                p.push(&[&END[cut..], b"j"].concat(), t0 + Duration::from_secs(1)),
                vec![Event::Key("j")],
                "cut {cut}"
            );
            assert!(!p.pasting());
        }
        // Bytes that look like the start of the end marker but are not do not hold anything back,
        // and a marker that begins again after them still counts.
        let mut p = Parser::new();
        assert!(p.push(&[START, b"\x1b[20", b"x\x1b[2"].concat(), t0).is_empty());
        assert_eq!(
            p.push(&[&b"01~"[..], b"j"].concat(), t0),
            vec![Event::Key("j")]
        );
    }

    #[test]
    fn a_split_start_marker_is_a_paste_when_the_pieces_are_close() {
        let t0 = Instant::now();
        for cut in 1..START.len() {
            let mut p = Parser::new();
            assert!(p.push(&START[..cut], t0).is_empty());
            let got = p.push(&[&START[cut..], b"qqqq", END, b"j"].concat(), t0 + Duration::from_millis(5));
            assert_eq!(got, vec![Event::Key("j")], "cut {cut}");
        }
    }

    #[test]
    fn a_start_marker_cut_and_given_up_on_leaks_the_paste_as_the_original_does() {
        // Both programs give up on a partial sequence after a short wait (20 ms in the original,
        // ESC_WAIT here), so a start marker that arrives in two pieces further apart than that is
        // not a paste any more. The original quits on the `q` that follows, and so does this.
        let t0 = Instant::now();
        let mut p = Parser::new();
        assert!(p.push(b"\x1b[20", t0).is_empty());
        assert!(p.timed_out(t0 + ESC_WAIT).is_empty());
        let got = p.push(b"0~qq\x1b[201~", t0 + Duration::from_millis(150));
        assert_eq!(got, vec![Event::Key("q"), Event::Key("q")]);
    }

    #[test]
    fn a_paste_costs_no_memory() {
        let t0 = Instant::now();
        let mut p = Parser::new();
        p.push(START, t0);
        let chunk = vec![b'q'; 64 * 1024];
        for i in 0..64 {
            assert!(p.push(&chunk, t0 + Duration::from_millis(i)).is_empty());
            assert!(p.buf.len() < PASTE_END.len(), "{} bytes held", p.buf.len());
        }
        assert!(p.buf.capacity() < 1 << 20, "{} bytes reserved for 4 MiB", p.buf.capacity());
        assert_eq!(p.push(END, t0 + Duration::from_millis(70)), vec![]);
        assert_eq!(p.push(b"j", t0 + Duration::from_millis(71)), vec![Event::Key("j")]);
    }

    #[test]
    fn a_lost_end_marker_is_given_up_on_after_the_quiet_bound() {
        let t0 = Instant::now();
        let mut p = Parser::new();
        assert!(p.push(&[START, b"qqq"].concat(), t0).is_empty());
        assert_eq!(p.deadline(), Some(t0 + PASTE_QUIET));
        // Typing during the wait is swallowed, and counts as the paste going on.
        assert!(p.push(b"q", t0 + Duration::from_secs(1)).is_empty());
        assert_eq!(p.deadline(), Some(t0 + Duration::from_secs(1) + PASTE_QUIET));
        assert!(p.timed_out(t0 + Duration::from_secs(2)).is_empty());
        assert!(p.pasting());
        // The bound reached by the clock: the paste is over, and nothing comes out of it.
        assert!(p.timed_out(t0 + Duration::from_secs(1) + PASTE_QUIET).is_empty());
        assert!(!p.pasting());
        assert!(p.deadline().is_none());
        assert_eq!(p.push(b"j", t0 + Duration::from_secs(4)), vec![Event::Key("j")]);
        // The bound is also seen by a byte that arrives after it, with no timer in between.
        let mut p = Parser::new();
        assert!(p.push(&[START, b"qqq"].concat(), t0).is_empty());
        assert_eq!(
            p.push(b"jq", t0 + PASTE_QUIET),
            vec![Event::Key("j"), Event::Key("q")]
        );
        // Part of an end marker held at that moment went with the paste, and the rest of it is
        // not a key.
        let mut p = Parser::new();
        assert!(p.push(&[START, b"qq\x1b[20"].concat(), t0).is_empty());
        assert!(p.timed_out(t0 + PASTE_QUIET).is_empty());
        assert_eq!(p.push(b"1~j", t0 + PASTE_QUIET), vec![Event::Key("j")]);
    }

    #[test]
    fn the_paste_bound_does_not_cut_a_slow_paste_that_keeps_arriving() {
        let t0 = Instant::now();
        let mut p = Parser::new();
        p.push(START, t0);
        // A gap just under the bound, many times over: ten seconds in all, still one paste.
        for i in 1..=10u64 {
            let now = t0 + Duration::from_millis(i * 1_000);
            assert!(p.timed_out(now - Duration::from_millis(1)).is_empty());
            assert!(p.push(b"qqq", now).is_empty());
        }
        assert_eq!(
            p.push(&[END, b"j"].concat(), t0 + Duration::from_secs(11)),
            vec![Event::Key("j")]
        );
    }
}
