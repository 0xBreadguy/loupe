//! Recap, drawn: the 80-column card, the wide card for a large terminal, their quiet states,
//! and the short list for a narrow terminal. A month is the same two cards with a calendar where
//! a week has its days.
//!
//! The card is the dashboard's own parts: its tiles and half-block digits, its section rules,
//! its palette roles. Color means what it means there. `hot` is waiting on you and nothing
//! else; `warn` is a plan limit and nothing else; a provider's hue appears only on the model
//! bar and never beside the accent, so the two cannot be mistaken in a colorful theme.
//! Without color, parts are told apart by their fill.
use crate::{
    recap::{self, Recap, Span},
    terminal::{self, Canvas, Palette, Rgb, mix},
};

pub const CARD_COLUMNS: usize = 80;
/// The short list. One column short of a 40-column screen, so no row is exactly as wide as
/// the screen: that is the one width where a terminal may add a blank row.
pub const LIST_COLUMNS: usize = 39;
/// Columns 1 to 37 hold the list's text. Column 0 is a finding's mark and column 38 is page.
const LIST_TEXT: usize = 37;
/// The wide card is drawn when the terminal has this many columns and rows: the card's own
/// 39 rows and the prompt after it.
pub const WIDE_COLUMNS: usize = 120;
pub const WIDE_ROWS: usize = 40;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_NAMES: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = recap::DAYS;
const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// The command a shared card names, so a screenshot says where it came from.
const COMMAND: &str = "t3-agents --recap";

/// How a card is written out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// 24-bit color.
    True,
    /// The 256-color palette.
    Indexed,
    /// No color: plain text, parts told apart by fill.
    None,
}

/// Which drawing `--recap` uses. Without one asked for, the terminal's size decides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// `--compact`: the 80-column card.
    Compact,
    /// `--wide`: the 120-column card.
    Wide,
    /// `--narrow`: the short list, whatever the terminal's size.
    Narrow,
}

/// What to say in the header where the coverage goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Records,
    /// `--demo`: the card says the week is made up.
    Demo,
}

pub(crate) struct Pen<'a> {
    pub(crate) c: Canvas,
    pub(crate) p: &'a Palette,
    pub(crate) mono: bool,
}

impl Pen<'_> {
    pub(crate) fn new(p: &Palette, columns: usize, rows: usize, mono: bool) -> Pen<'_> {
        let mut c = Canvas::new(columns, rows);
        c.clear(p.bg, p.fg);
        Pen { c, p, mono }
    }
    /// Draw runs of text one after another. Returns the column after the last.
    pub(crate) fn runs(&mut self, mut x: i32, y: i32, runs: &[(&str, Rgb, bool)]) -> i32 {
        for (text, color, bold) in runs {
            x += if *bold { self.c.text_bold(x, y, text, *color) } else { self.c.text(x, y, text, *color) };
        }
        x
    }
    /// Runs of text kept to `width` cells: what does not fit is cut with an ellipsis, so a
    /// line with large numbers in it never runs into the column beside it.
    fn runs_in(&mut self, x: i32, y: i32, width: usize, runs: &[(&str, Rgb, bool)]) -> i32 {
        let mut at = x;
        for (text, color, bold) in runs {
            let left = (x + width as i32 - at).max(0) as usize;
            if terminal::width(text) > left {
                return at + self.c.text_with(at, y, &terminal::fit(text, left), *color, None, *bold, usize::MAX);
            }
            at += self.c.text_with(at, y, text, *color, None, *bold, usize::MAX);
        }
        at
    }
    /// A color mixed toward the background, to tell a part from its neighbor, but never so
    /// far that the mark stops reading: a bar or a swatch keeps 3:1 against what it sits on.
    pub(crate) fn shade(&self, color: Rgb, toward_bg: f64, on: Rgb) -> Rgb {
        mix(color, self.p.bg, self.shade_amount(color, toward_bg, on))
    }
    /// How far `shade` goes toward the background: as far as asked, less what 3:1 needs.
    fn shade_amount(&self, color: Rgb, toward_bg: f64, on: Rgb) -> f64 {
        let mut amount = toward_bg;
        while amount > 0.15 && crate::integration::contrast(mix(color, self.p.bg, amount), on) < 3. {
            amount -= 0.05;
        }
        amount
    }
    /// The dashboard's section head: a bold label, a rule, and dim text at the right.
    pub(crate) fn section(&mut self, x: i32, y: i32, w: i32, label: &str, right: &str) {
        terminal::section_with(&mut self.c, x, y, w, label, self.p, right, self.p.dim);
    }
    fn fill_run(&mut self, x: i32, y: i32, cells: usize, fill: char, color: Rgb) {
        self.c.text(x, y, &fill.to_string().repeat(cells), color);
    }
}

/// The month and day of a `YYYY-MM-DD` date, the month counted from 0.
fn month_day(date: &str) -> Option<(usize, u32)> {
    Some((date.get(5..7)?.parse::<usize>().ok()?.checked_sub(1).filter(|m| *m < 12)?, date.get(8..10)?.parse().ok()?))
}

/// A week as `Nov 9 – 15`, or `Oct 27 – Nov 2` across months; a month as `October 2026`.
/// From the `YYYY-MM-DD` dates of the period's first and last day.
pub fn period_text(span: Span, from: &str, to: &str) -> String {
    match (span, month_day(from), month_day(to)) {
        (Span::Month, Some((month, _)), _) => format!("{} {}", MONTH_NAMES[month], from.get(..4).unwrap_or_default()),
        (Span::Week, Some((m1, d1)), Some((m2, d2))) => {
            if m1 == m2 { format!("{} {d1} – {d2}", MONTHS[m1]) } else { format!("{} {d1} – {} {d2}", MONTHS[m1], MONTHS[m2]) }
        }
        _ => format!("{from} – {to}"),
    }
}

/// The period with its year, `Nov 9 – 15, 2026`: a card that is shared is read long after.
/// A month names its year already.
pub(crate) fn period_with_year(span: Span, from: &str, to: &str) -> String {
    match to.get(..4) {
        Some(year) if span == Span::Week && year.bytes().all(|b| b.is_ascii_digit()) => format!("{}, {year}", period_text(span, from, to)),
        _ => period_text(span, from, to),
    }
}

/// `week` or `month`, for a sentence.
fn span_word(r: &Recap) -> &'static str {
    if r.period == Span::Month { "month" } else { "week" }
}

/// How much of the period the records cover. The wide card has room to say where a month so
/// far stops.
fn coverage(r: &Recap, source: Source, roomy: bool) -> String {
    let recorded = format!("{} {} recorded", r.days_recorded, if r.days_recorded == 1 { "day" } else { "days" });
    match source {
        Source::Demo => "demo data · made up".into(),
        Source::Records if r.so_far && roomy && r.period == Span::Month => match month_day(&r.from) {
            Some((month, _)) => format!("so far, to {} {} · {recorded}", MONTHS[month], r.days_elapsed),
            None => format!("so far · {recorded}"),
        },
        Source::Records if r.so_far => format!("so far · {recorded}"),
        Source::Records => format!("{} of {} days recorded", r.days_recorded, r.days),
    }
}

fn header(pen: &mut Pen, period: &str, state: &str, state_color: Rgb) {
    let p = pen.p;
    let x = pen.runs(1, 0, &[("T3 Agents", p.fg, true), ("  ", p.dim, false), ("RECAP", p.text2, true), ("  ", p.dim, false), (period, p.fg, false), ("  ", p.dim, false)]);
    pen.c.text(x, 0, state, state_color);
    pen.c.right(pen.c.w as i32 - 1, 0, COMMAND, p.dim);
}

/// Whole cells for each value: every non-zero value gets at least one, by largest remainder.
fn allot(width: usize, values: &[f64]) -> Vec<usize> {
    let total: f64 = values.iter().sum();
    if total <= 0. || width == 0 {
        return vec![0; values.len()];
    }
    let raw: Vec<f64> = values.iter().map(|v| v * width as f64 / total).collect();
    let mut cells: Vec<usize> = raw.iter().zip(values).map(|(r, v)| if *v > 0. { (*r as usize).max(1) } else { 0 }).collect();
    while cells.iter().sum::<usize>() < width {
        let i = (0..cells.len()).filter(|&i| values[i] > 0.).max_by(|&a, &b| (raw[a] - cells[a] as f64).total_cmp(&(raw[b] - cells[b] as f64))).unwrap_or(0);
        cells[i] += 1;
    }
    while cells.iter().sum::<usize>() > width {
        let Some(i) = (0..cells.len()).filter(|&i| cells[i] > 1).max_by(|&a, &b| (cells[a] as f64 - raw[a]).total_cmp(&(cells[b] as f64 - raw[b]))) else { break };
        cells[i] -= 1;
    }
    cells
}

/// Whole percents that add to 100, by largest remainder.
fn percents(values: &[f64]) -> Vec<u32> {
    allot(100, values).into_iter().map(|n| n as u32).collect()
}

/// A count for a big tile: digits up to 999, then `1.2K`. There is no big comma.
pub(crate) fn tile_count(n: u64) -> String {
    if n < 1000 { n.to_string() } else { recap::short_text(n as f64, 10.) }
}

/// The dashboard's tile: a panel, a dim bold label, half-block digits and a dim caption. A
/// zero stays small and dim, as it does on the dashboard.
pub(crate) fn tile(pen: &mut Pen, x: i32, w: i32, label: &str, value: &str, quiet: bool, note: &str) {
    let p = pen.p;
    pen.c.fill(x, 2, w, 5, p.panel);
    pen.c.text_bold(x + 2, 2, label, p.dim);
    if quiet {
        pen.c.text(x + 2, 4, value, p.dim);
    } else {
        terminal::draw_big(&mut pen.c, x + 2, 3, value, p.bright, p.bright, false);
    }
    pen.c.text(x + 2, 6, &terminal::fit(note, (w - 3).max(0) as usize), p.dim);
}

/// The period's hours for a big tile: `114h`, and `1.2Kh` from a thousand.
fn big_hours(r: &Recap) -> String {
    if r.hours >= 1000. { format!("{}h", recap::short_text(r.hours, 10.)) } else { recap::hours_text(r.hours) }
}

/// The day with the most agent hours; the earliest of them when several have as many.
fn busiest(r: &Recap) -> Option<usize> {
    (0..r.by_day.len()).filter(|&i| r.by_day[i] > 0.).max_by(|&a, &b| r.by_day[a].total_cmp(&r.by_day[b]).then(b.cmp(&a)))
}

/// A day of the period by name: `Thu` in a week, `Thu 12` in a month.
fn day_name(r: &Recap, day: usize) -> String {
    let weekday = DAYS[(r.first_weekday as usize + day) % 7];
    if r.period == Span::Month { format!("{weekday} {}", day + 1) } else { weekday.to_owned() }
}

/// `busiest Thu · 33h`, or in a month `busiest Thu 12 · 33h`.
fn busiest_text(r: &Recap) -> Option<String> {
    busiest(r).map(|day| format!("busiest {} · {}", day_name(r, day), recap::hours_text(r.by_day[day])))
}

/// Whether a day can hold records: it is not before they begin, and it has come.
fn recorded(r: &Recap, day: usize) -> bool {
    day as u32 >= r.first_recorded_day && (day as u32) < r.days_elapsed
}

// ── A month's days ──────────────────────────────────────────────────────────

/// An amount of hours as `hours_text` writes it, so a cut point in a key is the number a day
/// was sorted by.
fn as_written(hours: f64) -> f64 {
    if hours >= 9.95 {
        hours.round()
    } else if hours >= 1. {
        (hours * 10.).round() / 10.
    } else {
        (hours * 60.).round() / 60.
    }
}

/// How a month's days are shaded: against its busiest day, in three steps cut at a third and
/// two thirds of it. Day totals differ too much between people for fixed cut points, and the
/// wide card writes the cut points out, so the scale travels with the card.
struct Shades {
    cuts: [f64; 2],
    busiest: Option<usize>,
}

impl Shades {
    fn new(r: &Recap) -> Shades {
        let busiest = busiest(r);
        let peak = busiest.map_or(0., |day| r.by_day[day]);
        Shades { cuts: [as_written(peak / 3.), as_written(peak * 2. / 3.)], busiest }
    }
    /// 0 for a day with no agent time, then 1 to 3; `None` for a day that holds no records.
    fn level(&self, r: &Recap, day: usize) -> Option<usize> {
        if !recorded(r, day) {
            return None;
        }
        let hours = r.by_day[day];
        Some(if hours <= 0. {
            0
        } else if self.busiest == Some(day) || hours >= self.cuts[1] {
            3
        } else if hours >= self.cuts[0] {
            2
        } else {
            1
        })
    }
    /// The key's entries, lightest first: the step and what it stands for. A month so quiet
    /// that a cut point is written as nothing, or two as the same, has fewer steps to name.
    fn key(&self) -> Vec<(usize, String)> {
        let [low, high] = self.cuts.map(recap::hours_text);
        if self.cuts[1] <= 0. {
            return vec![(3, "agents at work".to_owned())];
        }
        let mut key = vec![];
        if self.cuts[0] > 0. {
            key.push((1, format!("under {low}")));
        }
        if self.cuts[1] > self.cuts[0] {
            key.push((2, if self.cuts[0] > 0. { format!("{low} to {high}") } else { format!("under {high}") }));
        }
        key.push((3, format!("{high} or more")));
        key
    }
}

/// One Monday-to-Sunday week of a month, as both cards show it.
struct Week {
    /// Agent hours on its days inside the month.
    hours: f64,
    /// The date it begins on inside the month: the 1st, or its Monday.
    first: usize,
    /// Whether any of its days holds records, and whether any has come.
    recorded: bool,
    come: bool,
}

/// A month's weeks, 4 to 6 of them. The first and the last are as short as the calendar
/// makes them.
fn weeks(r: &Recap) -> Vec<Week> {
    let mut weeks: Vec<Week> = vec![];
    for day in 0..r.by_day.len() {
        let index = (r.first_weekday as usize + day) / 7;
        if weeks.len() <= index {
            weeks.push(Week { hours: 0., first: day + 1, recorded: false, come: false });
        }
        let week = &mut weeks[index];
        if recorded(r, day) {
            week.hours += r.by_day[day];
            week.recorded = true;
        }
        week.come |= (day as u32) < r.days_elapsed;
    }
    weeks
}

/// The week with the most agent hours; the earliest of them when several have as many.
fn busiest_week(weeks: &[Week]) -> Option<usize> {
    (0..weeks.len()).filter(|&w| weeks[w].hours > 0.).max_by(|&a, &b| weeks[a].hours.total_cmp(&weeks[b].hours).then(b.cmp(&a)))
}

/// One column of the hours tile, two cells wide: `part` of the tallest, which stops level
/// with the top of the digits, 20 of 24 eighths. The tallest is bright. The shortest is two
/// eighths: at one, a day or a week with a little in it drew the same glyph as the flat
/// track of one with nothing, and without color the two were the same.
fn column(pen: &mut Pen, x: i32, part: f64, is_top: bool) {
    let eighths = ((part * 20.).round() as i32).max(2);
    let color = if is_top || pen.mono { pen.p.bright } else { pen.shade(pen.p.accent, 0.4, pen.p.panel) };
    for row in 0..3 {
        let fill = (eighths - 8 * row).clamp(0, 8);
        if fill > 0 {
            pen.fill_run(x, 5 - row, 2, LEVELS[fill as usize - 1], color);
        }
    }
}

/// The hours tile for a month: the number, and the month beside it as a column a week,
/// drawn as the week card draws a column a day. Under each is the date the week begins on,
/// which is also why the first and last can be short: a month seldom starts on a Monday.
/// A small calendar was tried here first, the weeks as half rows under the weekday letters.
/// Its rows touched, so it read as seven bars whose height meant only which dates exist.
fn month_tile(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    pen.c.fill(0, 2, 41, 5, p.panel);
    pen.c.text_bold(2, 2, "AGENT HOURS", p.dim);
    let weeks = weeks(r);
    let top = busiest_week(&weeks);
    if let Some(week) = top {
        pen.c.right(40, 2, &format!("busiest week · {}", recap::hours_text(weeks[week].hours)), p.dim);
    }
    terminal::draw_big(&mut pen.c, 2, 3, &big_hours(r), p.bright, p.bright, false);
    pen.c.text(2, 6, &if r.most_at_once <= 1 { "one at a time".to_owned() } else { format!("up to {} at once", r.most_at_once) }, p.dim);
    let peak = top.map_or(0., |week| weeks[week].hours);
    for (i, week) in weeks.iter().enumerate() {
        let x = 20 + 3 * i as i32;
        let date = week.first.to_string();
        // A week before the records begin, or one that has not come yet, is not a zero.
        if !week.come || !week.recorded {
            if week.come {
                pen.c.text(x, 5, "·", p.faint);
            }
            pen.c.right(x + 2, 6, &date, p.faint);
            continue;
        }
        let is_top = top == Some(i);
        if week.hours <= 0. {
            pen.c.text(x, 5, "▁▁", p.track);
        } else {
            column(pen, x, week.hours / peak, is_top);
        }
        pen.c.right_with(x + 2, 6, &date, if is_top { p.fg } else { p.dim }, None, is_top);
    }
}

/// A month day by day on the wide card: a calendar with its dates, each day shaded by its
/// agent hours, and each week's hours at the right. A day before the records begin, or one
/// that has not come, keeps its date and gets no mark: it is not a day without work.
fn month_grid(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    pen.section(1, 8, 57, "AGENTS AT WORK, BY DAY", &busiest_text(r).unwrap_or_default());
    for (weekday, name) in DAYS.iter().enumerate() {
        pen.c.text(1 + 7 * weekday as i32, 9, name, p.dim);
    }
    pen.c.right(58, 9, "week", p.dim);
    let (shades, inks) = (Shades::new(r), grid_inks(pen));
    const FILLS: [char; 4] = ['·', '░', '▄', '█'];
    let mark = |pen: &mut Pen, x: i32, y: i32, cells: usize, level: usize| {
        if pen.mono { pen.fill_run(x, y, cells, FILLS[level], p.fg) } else { pen.fill_run(x, y, cells, '▆', inks[level]) }
    };
    let weeks = weeks(r);
    let top_week = busiest_week(&weeks);
    for day in 0..r.by_day.len() {
        let slot = r.first_weekday as usize + day;
        let (x, y) = (1 + 7 * (slot % 7) as i32, 10 + (slot / 7) as i32);
        let date = (day + 1).to_string();
        match shades.level(r, day) {
            None => drop(pen.c.right(x + 2, y, &date, p.faint)),
            Some(level) => {
                let is_top = shades.busiest == Some(day);
                pen.c.right_with(x + 2, y, &date, if is_top { p.fg } else { p.text2 }, None, is_top);
                mark(pen, x + 3, y, 3, level);
            }
        }
    }
    // The same hours as the 80-column card's columns, so the two cards agree.
    for (i, week) in weeks.iter().enumerate() {
        let y = 10 + i as i32;
        if !week.come {
            continue;
        }
        if !week.recorded {
            pen.c.right(58, y, "—", p.dim);
        } else if week.hours <= 0. {
            pen.c.right(58, y, "none", p.dim);
        } else {
            let is_top = top_week == Some(i);
            pen.c.right_with(58, y, &recap::hours_text(week.hours), if is_top { p.fg } else { p.text2 }, None, is_top);
        }
    }
    // What a month can say that a week cannot: how many of its days had agents at work, and
    // what such a day is like. Typical is the median, as it is for a wait.
    if r.days_active > 0 {
        let days = r.days_recorded.max(r.days_active);
        let mut x = pen.runs(1, 16, &[("agents ran on ", p.text2, false), (&r.days_active.to_string(), p.fg, true), (&format!(" of {days} {}", noun(u64::from(days), "day", "days")), p.text2, false)]);
        if let Some(typical) = r.typical_day.filter(|_| r.days_active >= 2) {
            x = pen.runs(x, 16, &[(" · ", p.faint, false), (&recap::hours_text(typical), p.fg, true), (" on a typical one", p.text2, false)]);
        }
        let _ = x;
    }
    // The key writes the cut points out in hours, so a shade means the same to whoever is
    // shown the card. The shorter lead where the long one would not fit.
    let key = shades.key();
    let wide: usize = key.iter().map(|(_, label)| 3 + terminal::width(label)).sum::<usize>() + 2 * key.len().saturating_sub(1);
    let lead = if "hours a day: ".len() + wide <= 57 { "hours a day: " } else { "a day: " };
    let mut x = 1 + pen.c.text(1, 17, lead, p.dim);
    for (i, (level, label)) in key.iter().enumerate() {
        if i > 0 {
            x += 2;
        }
        mark(pen, x, 17, 2, *level);
        x += 3 + pen.c.text(x + 3, 17, label, p.dim);
    }
}

/// The hours tile, two tiles wide: the number, and the week beside it as seven columns.
fn hours_tile(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    pen.c.fill(0, 2, 41, 5, p.panel);
    pen.c.text_bold(2, 2, "AGENT HOURS", p.dim);
    let top = busiest(r);
    if let Some(text) = busiest_text(r) {
        pen.c.right(40, 2, &text, p.dim);
    }
    terminal::draw_big(&mut pen.c, 2, 3, &big_hours(r), p.bright, p.bright, false);
    pen.c.text(2, 6, &if r.most_at_once <= 1 { "one at a time".to_owned() } else { format!("up to {} at once", r.most_at_once) }, p.dim);
    let peak = top.map_or(0., |day| r.by_day[day]);
    for day in 0..r.by_day.len().min(7) {
        let x = 20 + 3 * day as i32;
        let letter = &DAYS[day][..1];
        // A day before the records begin, or one that has not come yet, is not a zero.
        if !recorded(r, day) {
            pen.c.text(x, 5, "·", p.faint);
            pen.c.text(x, 6, letter, p.faint);
            continue;
        }
        let is_top = top == Some(day);
        if r.by_day[day] <= 0. {
            pen.c.text(x, 5, "▁▁", p.track);
        } else {
            column(pen, x, r.by_day[day] / peak, is_top);
        }
        if is_top {
            pen.c.text_bold(x, 6, letter, p.fg);
        } else {
            pen.c.text(x, 6, letter, p.dim);
        }
    }
}

/// One part of the hours: its name, its ink, the fill that stands for the ink where there is
/// no color, and what it is.
struct Part {
    name: &'static str,
    hours: f64,
    ink: Rgb,
    fill: char,
    what: &'static str,
}

fn parts(pen: &Pen, r: &Recap) -> [Part; 3] {
    let p = pen.p;
    [
        Part { name: "tools", hours: r.split.tools, ink: p.accent, fill: '▆', what: "a tool call running" },
        Part { name: "model", hours: r.split.model, ink: pen.shade(p.accent, 0.55, p.bg), fill: '░', what: "thinking and writing" },
        Part { name: "waiting on you", hours: r.split.waiting, ink: p.hot, fill: '▄', what: "from asked to answered" },
    ]
}

/// The parts as one bar, `width` cells from `x`, with a blank cell between them. Returns
/// each part's whole percent.
fn split_bar(pen: &mut Pen, x: i32, y: i32, width: usize, parts: &[Part; 3]) -> Vec<String> {
    let hours: Vec<f64> = parts.iter().map(|part| part.hours).collect();
    let shown = hours.iter().filter(|h| **h > 0.).count();
    let cells = allot(width.saturating_sub(shown.saturating_sub(1)), &hours);
    let mut x = x;
    for (part, cells) in parts.iter().zip(cells) {
        if part.hours > 0. {
            pen.fill_run(x, y, cells, if pen.mono { part.fill } else { '▆' }, part.ink);
            x += cells as i32 + 1;
        }
    }
    percents(&hours).into_iter().map(|share| if share == 0 { "<1%".to_owned() } else { format!("{share}%") }).collect()
}

fn split(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    pen.section(1, 8, 78, "WHERE THE HOURS WENT", &format!("of {}", recap::hours_text(r.hours)));
    let parts = parts(pen, r);
    let share = split_bar(pen, 1, 9, 78, &parts);
    let mut legend = 1;
    for (part, share) in parts.iter().zip(share) {
        let mark = if pen.mono { part.fill.to_string() } else { "■".to_owned() };
        legend = if part.hours <= 0. {
            pen.runs(legend, 10, &[(&mark, p.track, false), (&format!(" {} none", part.name), p.dim, false)])
        } else {
            pen.runs(legend, 10, &[(&mark, part.ink, false), (&format!(" {} ", part.name), p.text2, false), (&share, p.fg, true), (&format!(" {}", recap::hours_text(part.hours)), p.dim, false)])
        } + 3;
    }
}

fn family_color(p: &Palette, family: &str) -> Rgb {
    p.providers[match family {
        "claude" => 0,
        "openai" => 1,
        "grok" => 2,
        _ => 3,
    }]
}

/// The models a legend names, and the rest folded into "N more". The folded item has no family.
fn folded_models(r: &Recap, named: usize) -> Vec<(String, Option<&'static str>, f64)> {
    let mut items: Vec<_> = r.models.iter().take(named).map(|m| (terminal::fit(&m.name, 16), Some(m.family), m.hours)).collect();
    if r.models.len() > named {
        let hours = r.models[named..].iter().map(|m| m.hours).sum();
        items.push((format!("{} more", r.models.len() - named), None, hours));
    }
    items
}

/// How wide that legend is, counting the two spaces between its items.
fn legend_width(items: &[(String, Option<&'static str>, f64)]) -> usize {
    items.iter().map(|(name, family, hours)| terminal::width(name) + recap::hours_text(*hours).len() + if family.is_some() { 3 } else { 1 } + 2).sum::<usize>().saturating_sub(2)
}

/// One bar in provider hues, ordered by run time. The legend names as many models as fit and
/// folds the rest into "N more".
fn models(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    let count = r.models.len();
    pen.section(1, 12, 78, "MODELS", &if count == 1 { "one model".to_owned() } else { format!("{count} models · by run time") });
    // (legend name, family, hours); the folded item has no family.
    let mut named = count.min(4);
    while named > 1 && legend_width(&folded_models(r, named)) > 78 {
        named -= 1;
    }
    let items = folded_models(r, named);
    let cells = allot(78usize.saturating_sub(items.len().saturating_sub(1)), &items.iter().map(|i| i.2).collect::<Vec<_>>());
    let (mut x, mut legend) = (1, 1);
    for (i, (name, family, hours)) in items.iter().enumerate() {
        // The folded models are no one provider, so they wear no provider's color.
        let color = family.map_or(p.dim, |f| family_color(p, f));
        pen.fill_run(x, 13, cells[i], if pen.mono && i % 2 == 1 { '░' } else { '▆' }, color);
        x += cells[i] as i32 + 1;
        let time = recap::hours_text(*hours);
        legend = match family {
            Some(_) => pen.runs(legend, 14, &[("●", color, false), (&format!(" {name} "), p.text2, false), (&time, p.fg, true)]),
            None => pen.runs(legend, 14, &[(&format!("{name} "), p.dim, false), (&time, p.text2, true)]),
        } + 2;
    }
}

fn ledger(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    let lines_up = r.prs_merged == 0; // lines moved up into the fourth tile
    for (x, head) in [(1, "TASKS"), (21, if lines_up { "PULL REQUESTS" } else { "LINES CHANGED" }), (41, "HANDED TO AGENTS"), (61, "TOKENS")] {
        pen.c.text_bold(x, 16, head, p.dim);
    }
    // Each column keeps to its 19 cells.
    let dot = (" · ", p.faint, false);
    pen.runs_in(1, 17, 19, &[(&recap::count_text(u64::from(r.tasks.from_you)), p.fg, true), (" from you", p.text2, false)]);
    if r.tasks.from_agents > 0 {
        pen.runs_in(1, 18, 19, &[(&recap::count_text(u64::from(r.tasks.from_agents)), p.fg, true), (" from agents", p.text2, false)]);
    } else {
        pen.c.text(1, 18, "none from agents", p.dim);
    }
    if lines_up {
        if r.prs_open + r.prs_closed > 0 {
            pen.runs_in(21, 17, 19, &[(&r.prs_open.to_string(), p.fg, true), (" open", p.text2, false), dot, ("0 merged", p.dim, false)]);
        } else {
            pen.c.text(21, 17, "none linked", p.dim);
        }
    } else {
        pen.runs_in(21, 17, 19, &[(&recap::lines_text('+', r.lines_added), p.fg, true), ("  ", p.dim, false), (&recap::lines_text('-', r.lines_removed), p.fg, true)]);
    }
    pen.runs_in(21, 18, 19, &[(&recap::count_text(r.file_changes), p.fg, true), (if r.file_changes == 1 { " file change" } else { " file changes" }, p.text2, false)]);
    if r.handed.count > 0 {
        let (count, hours) = (recap::count_text(u64::from(r.handed.count)), recap::hours_text(r.handed.hours));
        // "times" goes where a month's numbers leave it no room.
        let times = if r.handed.count == 1 { " time" } else if count.len() + hours.len() + 9 <= 19 { " times" } else { "" };
        pen.runs_in(41, 17, 19, &[(&count, p.fg, true), (times, p.text2, false), dot, (&hours, p.fg, true)]);
        // Failed, stopped or cancelled: what is true of all three is that they did not finish.
        if r.handed.failed > 0 {
            pen.runs_in(41, 18, 19, &[(&recap::count_text(u64::from(r.handed.failed)), p.fg, true), (" did not finish", p.text2, false)]);
        } else {
            pen.c.text(41, 18, "all finished", p.dim);
        }
    } else {
        pen.c.text(41, 17, "none", p.dim);
    }
    let t = &r.tokens;
    if !t.reported {
        pen.c.text(61, 17, "none reported", p.dim);
        return;
    }
    pen.runs_in(61, 17, 19, &[(&recap::short_text(t.input, 10.), p.fg, true), (" in · ", p.text2, false), (&recap::short_text(t.output, 10.), p.fg, true), (" out", p.text2, false)]);
    // One provider that reports none is named with the hours left out, where the name and
    // the hours leave room for the words; otherwise the hours the totals do cover.
    let named = match t.left_out.as_slice() {
        [(name, hours)] => Some((name, recap::hours_text(*hours))).filter(|(name, hours)| terminal::width(name) + hours.len() + 12 <= 19),
        _ => None,
    };
    match (named, t.left_out.is_empty()) {
        (_, true) => {
            let cached = if t.input > 0. { (t.cached * 100. / t.input).round() } else { 0. };
            pen.runs_in(61, 18, 19, &[(&format!("{cached}%"), p.fg, true), (" read from cache", p.text2, false)]);
        }
        (Some((name, hours)), _) => {
            let color = p.family_color(terminal::family(name, None));
            pen.runs_in(61, 18, 19, &[("●", color, false), (&format!(" {name} "), p.text2, false), (&hours, p.fg, true), (" left out", p.dim, false)]);
        }
        (None, _) => {
            let missing: f64 = t.left_out.iter().map(|(_, hours)| hours).sum();
            let (covered, all) = (recap::hours_text((r.hours - missing).max(0.)), recap::hours_text(r.hours));
            if covered.len() + all.len() + 11 <= 19 {
                pen.runs_in(61, 18, 19, &[("covers ", p.dim, false), (&covered, p.fg, true), (&format!(" of {all}"), p.dim, false)]);
            } else {
                // Four-digit hours: the share, which always fits. Never 100%, since some is left out.
                let share = if r.hours > 0. { ((r.hours - missing).max(0.) * 100. / r.hours).round().min(99.) } else { 0. };
                pen.runs_in(61, 18, 19, &[("covers ", p.dim, false), (&format!("{share}%"), p.fg, true), (" of hours", p.dim, false)]);
            }
        }
    }
}

/// What a quiet period can say for itself, in the order the card says it.
fn quiet_notes(r: &Recap) -> Vec<&'static str> {
    let mut notes = Vec::new();
    if r.tasks.failed_limit == 0 {
        notes.push("no plan limits");
    }
    if r.tasks.failed == 0 {
        notes.push("no failures");
    }
    if r.asks.hours < 0.5 {
        notes.push("no long waits");
    }
    notes
}

fn findings(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    if r.findings.is_empty() {
        let quiet = quiet_notes(r);
        let x = pen.c.text(2, 20, &format!("No findings this {}", span_word(r)), p.text2) + 2;
        if !quiet.is_empty() {
            pen.runs(x, 20, &[(" · ", p.faint, false), (&quiet.join(" · "), p.dim, false)]);
        }
        return;
    }
    for (i, finding) in r.findings.iter().enumerate() {
        let y = 20 + i as i32;
        match finding.kind {
            "limit" => drop(pen.c.text(0, y, "▌", p.warn)),
            "waiting" => drop(pen.c.text(0, y, "▌", p.hot)),
            _ => {}
        }
        pen.c.text_bold(2, y, &finding.number, p.bright);
        let x = 8 + pen.c.text_with(8, y, &finding.text, p.fg, None, false, CARD_COLUMNS - 8);
        // What a finding rests on is shown whole or not at all.
        if !finding.rests_on.is_empty() && x as usize + 3 + terminal::width(&finding.rests_on) <= CARD_COLUMNS {
            pen.runs(x, y, &[(" · ", p.faint, false), (&finding.rests_on, p.dim, false)]);
        }
    }
}

/// Pull requests merged; with none merged, the lines added, so the tile is never a dead zero.
fn fourth_tile(pen: &mut Pen, r: &Recap, x: i32, w: i32, closed: bool) {
    if r.prs_merged > 0 {
        let note = if closed && r.prs_closed > 0 { format!("{} open · {} closed", r.prs_open, r.prs_closed) } else { format!("{} open", r.prs_open) };
        tile(pen, x, w, "PRS MERGED", &tile_count(u64::from(r.prs_merged)), false, &note);
    } else {
        let added = if r.lines_added < 1000 { r.lines_added.to_string() } else { recap::short_text(r.lines_added as f64, 10.) };
        tile(pen, x, w, "LINES ADDED", &added, r.lines_added == 0, &format!("{} removed", recap::lines_text(' ', r.lines_removed).trim()));
    }
}

/// The 80-column card: 23 rows, so it and the prompt after it fit a 24-row terminal.
pub fn card(r: &Recap, p: &Palette, source: Source, mono: bool) -> Canvas {
    let mut pen = Pen::new(p, CARD_COLUMNS, 23, mono);
    header(&mut pen, &period_text(r.period, &r.from, &r.to), &coverage(r, source, false), p.dim);
    if r.period == Span::Month { month_tile(&mut pen, r) } else { hours_tile(&mut pen, r) }
    let t = &r.tasks;
    let done = if t.total > 0 && t.finished == t.total { "all finished".to_owned() } else { format!("{} finished", recap::count_text(u64::from(t.finished))) };
    tile(&mut pen, 42, 18, "TASKS", &tile_count(u64::from(t.total)), t.total == 0, &done);
    fourth_tile(&mut pen, r, 61, 19, false);
    split(&mut pen, r);
    models(&mut pen, r);
    ledger(&mut pen, r);
    findings(&mut pen, r);
    pen.c
}

/// `word` or `words`, for a count.
fn noun<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// The hour grid's inks: an hour with no agent, then three steps that read as more. They run
/// the way the dashboard's own ramp does (`terminal::ramp`: half the accent, the accent,
/// bright), because the accent alone has too little room: where it sits near the page, the
/// lightest step that still reads 3:1 is already most of the way to it. So the top step is
/// whichever of the accent and bright stands further from the page, and the middle one is
/// halfway to it.
fn grid_inks(pen: &Pen) -> [Rgb; 4] {
    let p = pen.p;
    let far = |color: Rgb| crate::integration::contrast(color, p.bg);
    let light = pen.shade(p.accent, 0.5, p.bg);
    let top = if far(p.bright) > far(p.accent) { p.bright } else { p.accent };
    [p.track, light, mix(light, top, 0.5), top]
}

/// The week hour by hour: seven rows of 24 hours, two cells an hour, shaded by how many
/// agents were running. The 80-column card has room only for the seven days.
fn hour_grid(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    pen.section(1, 8, 57, "AGENTS AT WORK, BY HOUR", "hours a day");
    let inks = grid_inks(pen);
    const FILLS: [char; 4] = ['·', '░', '▄', '█'];
    let cell = |pen: &mut Pen, x: i32, y: i32, level: usize| {
        if pen.mono { pen.fill_run(x, y, 2, FILLS[level], p.fg) } else { pen.fill_run(x, y, 2, '▆', inks[level]) }
    };
    let top = busiest(r);
    for day in 0..r.by_day.len().min(7) {
        let y = 9 + day as i32;
        if day as u32 >= r.days_elapsed {
            // A day that has not come yet is not a day without work.
            pen.c.text(1, y, DAYS[day], p.faint);
            continue;
        }
        if (day as u32) < r.first_recorded_day {
            pen.c.text(1, y, DAYS[day], p.dim);
            let from = if day == 0 { 5 + pen.c.text(5, y, "before the records begin", p.dim) + 1 } else { 5 };
            pen.fill_run(from, y, (53 - from) as usize, '┄', p.faint);
            pen.c.right(58, y, "—", p.dim);
            continue;
        }
        let is_top = top == Some(day);
        pen.c.text_with(1, y, DAYS[day], if is_top { p.fg } else { p.text2 }, None, is_top, usize::MAX);
        for hour in 0..24 {
            // An hour's number is the agents running through it, on average: one agent for
            // the whole hour is 1, and so are two for half of it.
            let agents = r.by_hour.get(day).map_or(0., |hours| hours[hour]);
            let level = if agents <= 0. { 0 } else if agents < 0.995 { 1 } else if agents < 1.995 { 2 } else { 3 };
            cell(pen, 5 + 2 * hour as i32, y, level);
        }
        if r.by_day[day] > 0. {
            pen.c.right_with(58, y, &recap::hours_text(r.by_day[day]), if is_top { p.fg } else { p.text2 }, None, is_top);
        } else {
            pen.c.right(58, y, "none", p.dim);
        }
    }
    for (hour, x) in [("0", 5), ("6", 17), ("12", 29), ("18", 41), ("24", 51)] {
        pen.c.text(x, 16, hour, p.dim);
    }
    // The key says what a shade is: the hour's average, not a head count at one moment.
    let mut x = 1 + pen.c.text(1, 17, "agents on average: ", p.dim);
    for (level, label) in [(1, " under 1  "), (2, " 1 to 2  "), (3, " 2 or more")] {
        cell(pen, x, 17, level);
        x += 2 + pen.c.text(x + 2, 17, label, p.dim);
    }
}

/// Where the hours went, each part with what it is, and the waiting in detail.
fn split_wide(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    let x = 62;
    pen.section(x, 8, 57, "WHERE THE HOURS WENT", &format!("of {}", recap::hours_text(r.hours)));
    let parts = parts(pen, r);
    let share = split_bar(pen, x, 9, 57, &parts);
    for (i, (part, share)) in parts.iter().zip(share).enumerate() {
        let y = 11 + i as i32;
        let mark = if pen.mono { part.fill.to_string() } else { "■".to_owned() };
        if part.hours <= 0. {
            pen.runs(x, y, &[(&mark, p.track, false), (&format!(" {}", part.name), p.dim, false)]);
            pen.c.right(x + 29, y, "none", p.dim);
            continue;
        }
        pen.runs(x, y, &[(&mark, part.ink, false), (&format!(" {}", part.name), p.text2, false)]);
        pen.c.right_with(x + 22, y, &share, p.fg, None, true);
        pen.c.right(x + 29, y, &recap::hours_text(part.hours), p.dim);
        pen.c.text(x + 31, y, part.what, p.dim);
    }
    pen.section(x, 15, 57, "WAITING ON YOU", "");
    let asks = &r.asks;
    if asks.count == 0 {
        pen.c.text(x, 16, &format!("none this {}", span_word(r)), p.dim);
        return;
    }
    let dot = (" · ", p.faint, false);
    // The hours are in the legend two rows up; this row is what they were made of.
    let mut at = pen.runs(x, 16, &[(&recap::count_text(u64::from(asks.count)), p.fg, true), (noun(u64::from(asks.count), " ask", " asks"), p.text2, false)]);
    for (label, wait) in [("typical ", asks.typical_ms), ("longest ", asks.longest_ms)] {
        if let Some(wait) = wait {
            at = pen.runs(at, 16, &[dot, (label, p.text2, false), (&terminal::span(Some(wait / 1000.)), p.fg, true)]);
        }
    }
}

/// The models as a table: run time as a bar, then hours, runs and the tokens they read.
fn models_wide(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    let count = r.models.len();
    pen.section(1, 19, 76, "MODELS", &if count == 1 { "one model".to_owned() } else { format!("{count} models · by run time") });
    for (right, head) in [(48, "hours"), (57, "runs"), (71, "tokens in")] {
        pen.c.right(right, 20, head, p.dim);
    }
    // Five rows: every model when there are five or fewer, else four and the rest as one.
    let named = if count <= 5 { count } else { 4 };
    // (name, color, hours, runs, tokens read, whether every model in the row reports them)
    let mut rows: Vec<(String, Rgb, f64, u32, Option<f64>, bool)> =
        r.models.iter().take(named).map(|m| (terminal::fit(&m.name, 16), family_color(p, m.family), m.hours, m.runs, m.tokens_in, m.tokens_in.is_some())).collect();
    let rest = &r.models[named..];
    if !rest.is_empty() {
        let reporting = rest.iter().filter(|m| m.tokens_in.is_some()).count();
        let tokens = (reporting > 0).then(|| rest.iter().filter_map(|m| m.tokens_in).sum());
        // The folded models are no one provider, so the row wears no provider's color.
        rows.push((format!("{} more", rest.len()), p.dim, rest.iter().map(|m| m.hours).sum(), rest.iter().map(|m| m.runs).sum(), tokens, reporting == rest.len()));
    }
    let longest = rows.iter().map(|row| row.2).fold(0., f64::max);
    for (i, (name, color, hours, runs, tokens, all)) in rows.iter().enumerate() {
        let y = 21 + i as i32;
        if i < named {
            pen.runs(1, y, &[("●", *color, false), (&format!(" {name}"), p.text2, false)]);
        } else {
            pen.c.text(3, y, name, p.dim);
        }
        if longest > 0. && *hours > 0. {
            let cells = ((hours / longest * 20.).round() as usize).clamp(1, 20);
            pen.fill_run(20, y, cells, if pen.mono && i % 2 == 1 { '░' } else { '▆' }, *color);
        }
        pen.c.right_with(48, y, &recap::hours_text(*hours), p.fg, None, true);
        pen.c.right(57, y, &recap::count_text(u64::from(*runs)), p.text2);
        match tokens {
            Some(read) if *all => pen.c.right(71, y, &recap::short_text(*read, 10.), p.text2),
            // Some of the folded models report none: at least this many.
            Some(read) => pen.c.right(71, y, &format!("{}+", recap::short_text(*read, 10.)), p.dim),
            None => pen.c.right(71, y, "not reported", p.dim),
        };
    }
}

/// Which model handed work to which: the three commonest pairs.
fn handed_wide(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    let (x, right) = (80, WIDE_COLUMNS as i32 - 1);
    let h = &r.handed;
    if h.count == 0 {
        pen.section(x, 19, right - x, "HANDED TO AGENTS", "");
        pen.c.text(x, 20, &format!("none this {}", span_word(r)), p.dim);
        return;
    }
    // "Handoffs", so the count is not read as the tasks another agent started: a handoff
    // inside a run, to a provider's own subagent, is not a task.
    // The count and the hours alone where a month's numbers leave the word no room beside
    // the heading.
    let (count, hours) = (recap::count_text(u64::from(h.count)), recap::hours_text(h.hours));
    let total = Some(format!("{count} {} · {hours}", noun(u64::from(h.count), "handoff", "handoffs"))).filter(|text| terminal::width(text) + 19 <= (right - x) as usize).unwrap_or_else(|| format!("{count} · {hours}"));
    pen.section(x, 19, right - x, "HANDED TO AGENTS", &total);
    pen.c.text(x, 20, "from → to", p.dim);
    // A model that ran this week wears the color its runs wear; one that only took work
    // handed to it has no run here, so its name decides.
    let color = |name: &str| r.models.iter().find(|m| m.name == name).map_or_else(|| p.family_color(terminal::family(name, None)), |m| family_color(p, m.family));
    for (i, (from, to, times)) in h.pairs.iter().take(3).enumerate() {
        let y = 21 + i as i32;
        let times = recap::count_text(u64::from(*times));
        // Two dots, an arrow and a gap before the count leave the two names the rest.
        let room = ((right - x) as usize).saturating_sub(times.len() + 8) / 2;
        pen.runs(x, y, &[("●", color(from), false), (&format!(" {}", terminal::fit(from, room)), p.text2, false), (" → ", p.faint, false),
            ("●", color(to), false), (&format!(" {}", terminal::fit(to, room)), p.text2, false)]);
        pen.c.right_with(right, y, &times, p.fg, None, true);
    }
    // The rest are handoffs, not pairs: some have no run on record to pair them with.
    let shown: u32 = h.pairs.iter().take(3).map(|pair| pair.2).sum();
    if h.count > shown {
        pen.c.text(x, 24, "other handoffs", p.dim);
        pen.c.right(right, 24, &recap::count_text(u64::from(h.count - shown)), p.text2);
    }
    // Failed, stopped or cancelled: what is true of all three is that they did not finish.
    if h.failed > 0 {
        pen.runs(x, 25, &[(&recap::count_text(u64::from(h.failed)), p.fg, true), (" did not finish", p.text2, false)]);
    } else {
        pen.c.text(x, 25, "all finished", p.dim);
    }
}

/// Two rows of three sections: what the 80-column card keeps to a line each, or leaves out.
fn ledger_wide(pen: &mut Pen, r: &Recap) {
    let p = pen.p;
    let dot = (" · ", p.faint, false);
    let n = |count: u32| recap::count_text(u64::from(count));
    let (a, b, c, w) = (1, 41, 81, 38);
    let t = &r.tasks;

    // Every line keeps to its column's 38 cells, whatever the numbers come to.
    let cells = w as usize;
    pen.section(a, 27, w, "TASKS", "");
    let (from_you, from_phone) = (n(t.from_you), n(t.from_phone));
    // "Of them", so the two are not added up; without it where four digits leave no room.
    let by_phone = if from_you.len() + from_phone.len() + 29 <= cells { " of them by phone" } else { " by phone" };
    let mut line = vec![(from_you.as_str(), p.fg, true), (" from you", p.text2, false)];
    if t.from_phone > 0 {
        line.extend([dot, (from_phone.as_str(), p.fg, true), (by_phone, p.text2, false)]);
    }
    pen.runs_in(a, 28, cells, &line);
    if t.from_agents > 0 {
        pen.runs_in(a, 29, cells, &[(&n(t.from_agents), p.fg, true), (" started by another agent", p.text2, false)]);
    } else {
        pen.c.text(a, 29, "none started by another agent", p.dim);
    }
    pen.runs_in(a, 30, cells, &[("across ", p.text2, false), (&n(r.projects), p.fg, true), (noun(u64::from(r.projects), " project", " projects"), p.text2, false)]);

    pen.section(b, 27, w, "HOW THEY ENDED", "");
    let (finished, failed) = (n(t.finished), n(t.failed));
    let mut line = vec![("✓ ", p.text2, false), (finished.as_str(), p.fg, true), (" finished", p.text2, false), dot];
    if t.failed > 0 {
        line.extend([("✗ ", p.text2, false), (failed.as_str(), p.fg, true), (" failed", p.text2, false)]);
    } else {
        line.push(("none failed", p.dim, false));
    }
    pen.runs_in(b, 28, cells, &line);
    // Why they failed, with "plan limit" as "limit" where three large counts leave no room.
    let causes = |limit: &str| -> String {
        [("setup", t.failed_setup), (limit, t.failed_limit), ("provider", t.failed_provider)].iter().filter(|(_, count)| *count > 0).map(|(cause, count)| format!("{cause} {}", n(*count))).collect::<Vec<_>>().join(" · ")
    };
    let why = Some(causes("plan limit")).filter(|text| terminal::width(text) <= cells).unwrap_or_else(|| causes("limit"));
    pen.runs_in(b, 29, cells, &[(&why, p.dim, false)]);
    // T3 records that a run was stopped, not who stopped it.
    let ends: Vec<(String, &str)> = [(t.stopped, " stopped"), (t.cancelled, " cancelled"), (t.running, " running")].into_iter().filter(|end| end.0 > 0).map(|(count, words)| (n(count), words)).collect();
    let mut line = vec![];
    for (i, (count, words)) in ends.iter().enumerate() {
        if i > 0 {
            line.push(dot);
        }
        line.extend([(count.as_str(), p.fg, true), (*words, p.text2, false)]);
    }
    pen.runs_in(b, 30, cells, &line);

    pen.section(c, 27, w, if r.prs_merged > 0 { "LINES CHANGED" } else { "LINES AND PRS" }, "");
    pen.runs_in(c, 28, cells, &[(&recap::lines_text('+', r.lines_added), p.fg, true), ("  ", p.dim, false), (&recap::lines_text('-', r.lines_removed), p.fg, true), (" lines", p.text2, false)]);
    pen.runs_in(c, 29, cells, &[(&recap::count_text(r.file_changes), p.fg, true), (noun(r.file_changes, " file change in ", " file changes in "), p.text2, false),
        (&n(r.runs_changing_files), p.fg, true), (noun(u64::from(r.runs_changing_files), " run", " runs"), p.text2, false)]);
    if r.prs_merged + r.prs_open + r.prs_closed > 0 {
        let pulls: Vec<(String, &str)> = [(r.prs_merged, " merged"), (r.prs_open, " open"), (r.prs_closed, " closed")].into_iter().filter(|pr| pr.0 > 0).map(|(count, state)| (n(count), state)).collect();
        let mut line = vec![("PRs", p.text2, false)];
        for (i, (count, state)) in pulls.iter().enumerate() {
            line.extend([if i == 0 { (" ", p.text2, false) } else { dot }, (count.as_str(), p.fg, true), (*state, p.text2, false)]);
        }
        pen.runs_in(c, 30, cells, &line);
    } else {
        pen.c.text(c, 30, "no pull requests linked", p.dim);
    }

    pen.section(a, 32, w, "TOOLS", "");
    pen.runs_in(a, 33, cells, &[(&n(r.tool_calls), p.fg, true), (noun(u64::from(r.tool_calls), " call", " calls"), p.text2, false), dot, (&n(r.commands), p.fg, true), (noun(u64::from(r.commands), " of them a command", " of them commands"), p.text2, false)]);
    if r.commands_failed > 0 {
        // A command fails on any exit code but 0, and a search that finds nothing has one.
        // The reader never sees what a command was, so the card cannot tell the two apart.
        // The line above has just said "commands", so the word goes first where five digits
        // leave no room for it.
        let failed = n(r.commands_failed);
        let words = if r.commands_failed == 1 { " command failed or found nothing" } else if failed.len() + 33 <= cells { " commands failed or found nothing" } else { " failed or found nothing" };
        pen.runs_in(a, 34, cells, &[(&failed, p.fg, true), (words, p.text2, false)]);
    } else {
        pen.c.text(a, 34, "no command failed", p.dim);
    }

    pen.section(b, 32, w, "CONTEXT", "");
    if r.compactions > 0 {
        pen.runs_in(b, 33, cells, &[(&n(r.compactions), p.fg, true), (noun(u64::from(r.compactions), " time a thread was compacted", " times a thread was compacted"), p.text2, false)]);
    } else {
        pen.c.text(b, 33, "no thread was compacted", p.dim);
    }
    if r.turns_past_90 > 0 {
        pen.runs_in(b, 34, cells, &[(&n(r.turns_past_90), p.fg, true), (noun(u64::from(r.turns_past_90), " turn ran past 90% full", " turns ran past 90% full"), p.text2, false)]);
    } else {
        pen.c.text(b, 34, "no turn ran past 90% full", p.dim);
    }

    pen.section(c, 32, w, "TOKENS", "");
    let tokens = &r.tokens;
    if !tokens.reported {
        pen.c.text(c, 33, "none reported", p.dim);
        return;
    }
    let cached = if tokens.input > 0. { (tokens.cached * 100. / tokens.input).round() } else { 0. };
    pen.runs_in(c, 33, cells, &[(&recap::short_text(tokens.input, 10.), p.fg, true), (" in", p.text2, false), dot, (&format!("{cached}%"), p.fg, true), (" from cache", p.text2, false), dot,
        (&recap::short_text(tokens.output, 10.), p.fg, true), (" out", p.text2, false)]);
    // Each provider that reports none, with the hours the totals leave out, while they fit.
    let named: usize = tokens.left_out.iter().map(|(name, hours)| terminal::width(name) + recap::hours_text(*hours).len() + 5).sum();
    if tokens.left_out.is_empty() {
    } else if named + 18 <= cells {
        let mut at = c;
        for (name, hours) in &tokens.left_out {
            at = pen.runs(at, 34, &[("●", p.family_color(terminal::family(name, None)), false), (&format!(" {name} "), p.text2, false), (&recap::hours_text(*hours), p.fg, true), ("  ", p.dim, false)]);
        }
        pen.c.text(at, 34, "reported no tokens", p.dim);
    } else {
        let missing: f64 = tokens.left_out.iter().map(|(_, hours)| hours).sum();
        pen.runs_in(c, 34, cells, &[("covers ", p.dim, false), (&recap::hours_text((r.hours - missing).max(0.)), p.fg, true), (&format!(" of {}", recap::hours_text(r.hours)), p.dim, false)]);
    }
}

/// What a shared card needs said under it: how its numbers are counted. The last line is the
/// same in every mode. "May be cached here" keeps it true with the cache on, with
/// T3_AGENTS_NO_CACHE=1 and for --demo, which save nothing; nothing is sent in any of them.
const COUNTED: [&str; 2] = [
    "Agent hours: each run from start to end, plus waits on you after a run ended. Five agents for an hour count as 5h.",
    "Lines include generated files. Times are local. Read from T3's records. Numbers may be cached here; nothing is sent.",
];

/// The wide card, for a terminal of 120 columns by 40 rows or more: the same week with room
/// for when the agents worked, hour by hour, and for what the 80-column card folds away. It
/// ends with how the numbers are counted where the smaller card has its findings: every
/// finding would repeat a block that is already above it.
pub fn wide(r: &Recap, p: &Palette, source: Source, mono: bool) -> Canvas {
    let mut pen = Pen::new(p, WIDE_COLUMNS, WIDE_ROWS - 1, mono);
    header(&mut pen, &period_with_year(r.period, &r.from, &r.to), &coverage(r, source, true), p.dim);
    let t = &r.tasks;
    let n = |count: u32| recap::count_text(u64::from(count));
    // The runs alone where a month's counts leave the threads no room: a note is never cut.
    let runs = format!("{} {}", n(r.runs), noun(u64::from(r.runs), "run", "runs"));
    let work = Some(format!("{runs} · {} {}", n(r.threads), noun(u64::from(r.threads), "thread", "threads"))).filter(|note| terminal::width(note) <= 26).unwrap_or(runs);
    tile(&mut pen, 0, 29, "AGENT HOURS", &big_hours(r), false, &work);
    // The note adds up to the number above it. A task still running has not failed to
    // finish, so "did not" is said only when none is. The longer note where it fits the tile.
    let rest = t.total.saturating_sub(t.finished);
    let others = if t.running == 0 { "did not" } else if t.running == rest { "running" } else { "others" };
    // A month's four digits still say how many did not: "2,842 of 2,931 finished".
    let done = [
        if rest == 0 { "all finished".to_owned() } else { format!("{} finished · {} {others}", n(t.finished), n(rest)) },
        format!("{} of {} finished", n(t.finished), n(t.total)),
        format!("{} finished", n(t.finished)),
    ];
    let done = done.iter().find(|note| terminal::width(note) <= 26).unwrap_or(&done[2]);
    tile(&mut pen, 30, 29, "TASKS", &tile_count(u64::from(t.total)), t.total == 0, done);
    tile(&mut pen, 60, 29, "MOST AT ONCE", &tile_count(u64::from(r.most_at_once)), r.most_at_once == 0, r.most_at_once_when.as_deref().unwrap_or(""));
    fourth_tile(&mut pen, r, 90, 30, true);
    if r.period == Span::Month { month_grid(&mut pen, r) } else { hour_grid(&mut pen, r) }
    split_wide(&mut pen, r);
    models_wide(&mut pen, r);
    handed_wide(&mut pen, r);
    ledger_wide(&mut pen, r);
    pen.section(1, 36, WIDE_COLUMNS as i32 - 2, "HOW THIS IS COUNTED", "");
    for (i, line) in COUNTED.iter().enumerate() {
        pen.c.text(1, 37 + i as i32, line, p.dim);
    }
    pen.c
}

/// A card that says why there is nothing to show: a state in the header, a message, and
/// things to try, wrapped as the dashboard wraps them.
pub fn notice(p: &Palette, period: &str, state: &str, alarm: bool, message: &str, tips: &[String]) -> Canvas {
    let mut body: Vec<(String, u8)> = terminal::wrap(message, CARD_COLUMNS - 4).into_iter().map(|line| (line, 0)).collect();
    body.push((String::new(), 3));
    for tip in tips {
        for (i, line) in terminal::wrap(tip, CARD_COLUMNS - 6).into_iter().enumerate() {
            body.push((line, if i == 0 { 1 } else { 2 }));
        }
    }
    let mut pen = Pen::new(p, CARD_COLUMNS, body.len() + 5, false);
    header(&mut pen, period, state, if alarm { p.hot } else { p.dim });
    pen.c.fill(0, 2, CARD_COLUMNS as i32, body.len() as i32 + 2, p.panel);
    for (i, (line, kind)) in body.iter().enumerate() {
        let y = 3 + i as i32;
        match kind {
            0 => drop(pen.c.text_bold(2, y, line, p.fg)),
            1 | 2 => {
                if *kind == 1 {
                    pen.c.text(2, y, "·", p.dim);
                }
                pen.c.text(4, y, line, p.text2);
            }
            _ => {}
        }
    }
    pen.c
}

/// One row of the short list. A blank row is page. The bar is the hours. Text starts at
/// column 1, and a finding may mark column 0.
enum ListRow {
    /// Nothing drawn. The painted page shows through.
    Blank,
    /// Where the hours went, across the text columns.
    Bar,
    /// Runs of text, and the ink of a finding's mark when this row begins one.
    Text { mark: Option<Rgb>, runs: Vec<(String, Rgb, bool)> },
}

/// A run of the list: the text, its ink, and whether it is bold.
fn painted(text: impl Into<String>, ink: Rgb, bold: bool) -> (String, Rgb, bool) {
    (text.into(), ink, bold)
}

/// ` · ` in `faint`, the list's separator.
fn faint_dot(p: &Palette) -> (String, Rgb, bool) {
    painted(" · ", p.faint, false)
}

/// How many cells a row's text takes, from column 1.
fn row_width(runs: &[(String, Rgb, bool)]) -> usize {
    runs.iter().map(|(text, _, _)| terminal::width(text)).sum()
}

/// The first variant that fits the text columns, or the last when none does.
fn pick(variants: Vec<Vec<(String, Rgb, bool)>>) -> Vec<(String, Rgb, bool)> {
    let last = variants.len().saturating_sub(1);
    let chosen = variants.iter().enumerate().find(|(i, runs)| *i == last || row_width(runs) <= LIST_TEXT).map(|(i, _)| i);
    match chosen {
        Some(i) => variants.into_iter().nth(i).unwrap_or_default(),
        None => Vec::new(),
    }
}

/// Text from column 1, with a finding's mark when `mark` is set.
fn text_row(mark: Option<Rgb>, runs: Vec<(String, Rgb, bool)>) -> ListRow {
    ListRow::Text { mark, runs }
}

/// Draw runs one after another. The canvas clips what passes its edge.
fn paint_runs(pen: &mut Pen, x: i32, y: i32, runs: &[(String, Rgb, bool)]) {
    let borrowed: Vec<(&str, Rgb, bool)> = runs.iter().map(|(text, ink, bold)| (text.as_str(), *ink, *bold)).collect();
    pen.runs(x, y, &borrowed);
}

/// The header's period: the year when it fits, and a month's short name when the long one does not.
fn list_period(r: &Recap) -> String {
    let room = LIST_TEXT.saturating_sub(terminal::width("T3 Agents") + terminal::width("RECAP") + 4);
    let with_year = period_with_year(r.period, &r.from, &r.to);
    if terminal::width(&with_year) <= room {
        return with_year;
    }
    if r.period == Span::Month {
        if let Some((month, _)) = month_day(&r.from) {
            let year = r.from.get(..4).unwrap_or_default();
            return format!("{} {year}", MONTHS[month]);
        }
    }
    period_text(r.period, &r.from, &r.to)
}

/// Row 0: the name, RECAP, and the period.
fn list_header(p: &Palette, r: &Recap) -> ListRow {
    text_row(None, vec![
        painted("T3 Agents", p.fg, true),
        painted("  ", p.dim, false),
        painted("RECAP", p.text2, true),
        painted("  ", p.dim, false),
        painted(list_period(r), p.fg, false),
    ])
}

/// Row 3: the hours, and how many agents ran at once when that fits.
fn hours_row(p: &Palette, r: &Recap) -> ListRow {
    let hours = painted(recap::hours_text(r.hours), p.bright, true);
    let words = painted(" agent hours", p.text2, false);
    let once = if r.most_at_once <= 1 { "one at a time".to_owned() } else { format!("up to {} at once", r.most_at_once) };
    let full = vec![hours.clone(), words.clone(), faint_dot(p), painted(once, p.dim, false)];
    let short = vec![hours, words];
    text_row(None, pick(vec![full, short]))
}

/// Row 5: one swatch, name and percent for each part. A part at none is left out when the line does not fit.
fn legend_row(p: &Palette, parts: &[Part; 3], mono: bool) -> ListRow {
    let hours = [parts[0].hours, parts[1].hours, parts[2].hours];
    let share = percents(&hours);
    // The cards call the third part "waiting on you". The list has room for "waiting".
    let names = ["tools", "model", "waiting"];
    let mut full = Vec::new();
    let mut short = Vec::new();
    for ((part, share), name) in parts.iter().zip(share).zip(names) {
        let swatch = if mono { part.fill.to_string() } else { "■".to_owned() };
        let item = if part.hours <= 0. {
            vec![painted(swatch, p.track, false), painted(format!(" {name} none"), p.dim, false)]
        } else {
            let percent = if share == 0 { "<1%".to_owned() } else { format!("{share}%") };
            vec![painted(swatch, part.ink, false), painted(format!(" {name} "), p.text2, false), painted(percent, p.fg, true)]
        };
        if !full.is_empty() {
            full.push(painted(" ", p.fg, false));
        }
        full.extend(item.iter().cloned());
        if part.hours > 0. {
            if !short.is_empty() {
                short.push(painted(" ", p.fg, false));
            }
            short.extend(item);
        }
    }
    text_row(None, pick(vec![full, short]))
}

/// How many eighths a day with hours gets: at least two, so it does not look like a zero.
fn eighths(hours: f64, peak: f64) -> char {
    let scaled = (hours / peak * 8.).round();
    let e = if scaled.is_finite() { scaled.clamp(2., 8.) as usize } else { 2 };
    LEVELS[e - 1]
}

/// One day's mark: `·` before the records or not yet come, `▁` for a recorded zero, else the eighths.
fn bar_runs(p: &Palette, mono: bool, hours_ink: Rgb, day: usize, hours: f64, cells: usize, known: bool, peak: f64, top: Option<usize>) -> Vec<(String, Rgb, bool)> {
    if !known {
        let mut runs = vec![painted("·", p.faint, false)];
        if cells > 1 {
            runs.push(painted(" ".repeat(cells - 1), p.fg, false));
        }
        return runs;
    }
    if hours <= 0. || peak <= 0. {
        let ink = if mono { p.fg } else { p.track };
        return vec![painted("▁".repeat(cells), ink, false)];
    }
    let color = if mono { p.fg } else if top == Some(day) { p.bright } else { hours_ink };
    vec![painted(eighths(hours, peak).to_string().repeat(cells), color, false)]
}

/// Rows 7 to 9: the day marks, their labels, and the busiest day when any day has hours.
fn day_rows(p: &Palette, r: &Recap, mono: bool, hours_ink: Rgb) -> Vec<ListRow> {
    let top = busiest(r);
    let peak = top.map(|day| r.by_day[day]).unwrap_or(0.);
    let (marks, labels) = if r.period == Span::Week { week_days(p, r, mono, hours_ink, peak, top) } else { month_days(p, r, mono, hours_ink, peak, top) };
    let mut rows = vec![text_row(None, marks), text_row(None, labels)];
    if let Some(text) = busiest_text(r) {
        let base = vec![painted(text, p.dim, false)];
        let runs = match r.typical_day {
            Some(typical) if r.days_active >= 2 => {
                let mut longer = base.clone();
                longer.push(faint_dot(p));
                longer.push(painted("typical ", p.dim, false));
                longer.push(painted(recap::hours_text(typical), p.text2, false));
                pick(vec![longer, base])
            }
            _ => base,
        };
        rows.push(text_row(None, runs));
    }
    rows
}

/// A week: four cells a day, a gap between days, and a letter under each.
fn week_days(p: &Palette, r: &Recap, mono: bool, hours_ink: Rgb, peak: f64, top: Option<usize>) -> (Vec<(String, Rgb, bool)>, Vec<(String, Rgb, bool)>) {
    let mut marks = Vec::new();
    let mut labels = Vec::new();
    for day in 0..r.by_day.len().min(7) {
        if day > 0 {
            marks.push(painted(" ", p.fg, false));
            labels.push(painted("    ", p.fg, false));
        }
        let known = recorded(r, day);
        marks.extend(bar_runs(p, mono, hours_ink, day, r.by_day[day], 4, known, peak, top));
        let letter = DAYS[day].chars().next().unwrap_or(' ').to_string();
        let (ink, bold) = if top == Some(day) { (p.fg, true) } else if known { (p.dim, false) } else { (p.faint, false) };
        labels.push(painted(letter, ink, bold));
    }
    (marks, labels)
}

/// A month: one cell a day, a gap after each Sunday, and the 1st and each Monday underneath.
fn month_days(p: &Palette, r: &Recap, mono: bool, hours_ink: Rgb, peak: f64, top: Option<usize>) -> (Vec<(String, Rgb, bool)>, Vec<(String, Rgb, bool)>) {
    let mut marks = Vec::new();
    let mut placed: Vec<(usize, String, bool)> = Vec::new();
    let mut col = 0usize;
    for (day, hours) in r.by_day.iter().enumerate() {
        let weekday = (r.first_weekday as usize + day) % 7;
        if day > 0 && weekday == 0 {
            marks.push(painted(" ", p.fg, false));
            col += 1;
        }
        if day == 0 || weekday == 0 {
            placed.push((col, (day + 1).to_string(), !recorded(r, day)));
        }
        marks.extend(bar_runs(p, mono, hours_ink, day, *hours, 1, recorded(r, day), peak, top));
        col += 1;
    }
    let mut labels = Vec::new();
    let mut at = 0usize;
    for (column, text, faint) in placed {
        if column > at {
            labels.push(painted(" ".repeat(column - at), p.fg, false));
        }
        labels.push(painted(text.clone(), if faint { p.faint } else { p.dim }, false));
        at = column + terminal::width(&text);
    }
    (marks, labels)
}

/// A count in fg bold and the word after it in text2.
fn count_words(p: &Palette, count: u64, one: &str, many: &str) -> Vec<(String, Rgb, bool)> {
    vec![painted(recap::count_text(count), p.fg, true), painted(format!(" {}", noun(count, one, many)), p.text2, false)]
}

/// Row 11: tasks, dropping finished before failed, and saying all finished when they are.
fn tasks_row(p: &Palette, r: &Recap) -> ListRow {
    let t = &r.tasks;
    let head = count_words(p, u64::from(t.total), "task", "tasks");
    if t.total > 0 && t.finished == t.total {
        let mut runs = head;
        runs.push(faint_dot(p));
        runs.push(painted("all finished", p.dim, false));
        return text_row(None, runs);
    }
    let mut finished = head.clone();
    finished.push(faint_dot(p));
    finished.extend(count_words(p, u64::from(t.finished), "finished", "finished"));
    let mut failed = head.clone();
    if t.failed > 0 {
        let mut tail = vec![faint_dot(p)];
        tail.extend(count_words(p, u64::from(t.failed), "failed", "failed"));
        finished.extend(tail.clone());
        failed.extend(tail);
    }
    text_row(None, pick(vec![finished, failed, head]))
}

/// Row 12: pull requests, dropping closed before open.
fn prs_row(p: &Palette, r: &Recap) -> ListRow {
    let (merged, open, closed) = (r.prs_merged, r.prs_open, r.prs_closed);
    if merged + open + closed == 0 {
        return text_row(None, vec![painted("no pull requests linked", p.dim, false)]);
    }
    let mut base = if merged == 0 {
        vec![painted("no PR merged", p.dim, false)]
    } else {
        count_words(p, u64::from(merged), "PR merged", "PRs merged")
    };
    let mut variants = vec![base.clone()];
    for (count, word) in [(open, "open"), (closed, "closed")] {
        if count > 0 {
            base.push(faint_dot(p));
            base.extend(count_words(p, u64::from(count), word, word));
            variants.insert(0, base.clone());
        }
    }
    text_row(None, pick(variants))
}

/// The several-model legend for these items: each named model, then the fold of the rest, with
/// its hours or without them.
fn model_runs(p: &Palette, items: &[(String, Option<&'static str>, f64)], fold_hours: bool) -> Vec<(String, Rgb, bool)> {
    let mut runs = Vec::new();
    for (i, (name, family, hours)) in items.iter().enumerate() {
        if i > 0 {
            runs.push(painted("  ", p.fg, false));
        }
        let time = recap::hours_text(*hours);
        match family {
            Some(family) => {
                runs.push(painted("●", family_color(p, family), false));
                runs.push(painted(format!(" {name} "), p.text2, false));
                runs.push(painted(time, p.fg, true));
            }
            None if fold_hours => {
                runs.push(painted(format!("{name} "), p.dim, false));
                runs.push(painted(time, p.text2, true));
            }
            None => runs.push(painted(name.as_str(), p.dim, false)),
        }
    }
    runs
}

/// Row 13: one model on its own, or as many of the 80-card legend as fit. None when there are none.
fn models_row(p: &Palette, r: &Recap) -> Option<ListRow> {
    match r.models.as_slice() {
        [] => None,
        [model] => {
            let name = terminal::fit(&model.name, 16);
            let runs = vec![
                painted("●", family_color(p, model.family), false),
                painted(format!(" {name} "), p.text2, false),
                painted(recap::hours_text(model.hours), p.fg, true),
                faint_dot(p),
                painted("one model", p.dim, false),
            ];
            Some(text_row(None, runs))
        }
        _ => {
            let most = r.models.len().min(4);
            let mut named = most;
            while named > 1 && legend_width(&folded_models(r, named)) > LIST_TEXT {
                named -= 1;
            }
            let items = folded_models(r, named);
            // One model named and still too long (a 16-cell name, four-digit hours and a
            // four-digit fold): the fold gives up its hours, and then goes.
            let with_hours = model_runs(p, &items, true);
            let bare_fold = model_runs(p, &items, false);
            let alone = model_runs(p, &items[..1], true);
            Some(text_row(None, pick(vec![with_hours, bare_fold, alone])))
        }
    }
}

/// Row 14: lines added and removed, and the file changes when they fit.
fn lines_row(p: &Palette, r: &Recap) -> ListRow {
    let base = vec![
        painted(recap::lines_text('+', r.lines_added), p.fg, true),
        painted(" ", p.fg, false),
        painted(recap::lines_text('-', r.lines_removed), p.fg, true),
        painted(" lines", p.text2, false),
    ];
    let mut with_files = base.clone();
    with_files.push(faint_dot(p));
    with_files.extend(count_words(p, r.file_changes, "file change", "file changes"));
    text_row(None, pick(vec![with_files, base]))
}

/// Row 15: handoffs, dropping the unfinished tail before the hours.
fn handoffs_row(p: &Palette, r: &Recap) -> ListRow {
    let handed = &r.handed;
    if handed.count == 0 {
        return text_row(None, vec![painted("nothing handed to agents", p.dim, false)]);
    }
    let head = count_words(p, u64::from(handed.count), "handoff", "handoffs");
    let mut with_hours = head.clone();
    with_hours.push(faint_dot(p));
    with_hours.push(painted(recap::hours_text(handed.hours), p.fg, true));
    let mut full = with_hours.clone();
    if handed.failed > 0 {
        full.push(faint_dot(p));
        full.extend(count_words(p, u64::from(handed.failed), "did not finish", "did not finish"));
    } else {
        full.push(faint_dot(p));
        full.push(painted("all finished", p.dim, false));
    }
    text_row(None, pick(vec![full, with_hours, head]))
}

/// What the token totals cover, as hours and then as a whole percent. Never 100%, since some
/// is left out: the cards' rule.
fn token_cover(p: &Palette, r: &Recap, head: &[(String, Rgb, bool)], missing: f64) -> (Vec<(String, Rgb, bool)>, Vec<(String, Rgb, bool)>) {
    let covered = recap::hours_text((r.hours - missing).max(0.));
    let all = recap::hours_text(r.hours);
    let share = if r.hours > 0. { ((r.hours - missing).max(0.) * 100. / r.hours).round().min(99.) } else { 0. };
    let mut hours = head.to_vec();
    hours.push(faint_dot(p));
    hours.push(painted("covers ", p.dim, false));
    hours.push(painted(covered, p.fg, true));
    hours.push(painted(format!(" of {all}"), p.dim, false));
    let mut percent = head.to_vec();
    percent.push(faint_dot(p));
    percent.push(painted("covers ", p.dim, false));
    percent.push(painted(format!("{share}%"), p.fg, true));
    percent.push(painted(" of hours", p.dim, false));
    (hours, percent)
}

/// Row 16: tokens, naming one provider left out and otherwise what the totals cover.
fn tokens_row(p: &Palette, r: &Recap) -> ListRow {
    let tokens = &r.tokens;
    if !tokens.reported {
        return text_row(None, vec![painted("no tokens reported", p.dim, false)]);
    }
    let head = vec![painted(recap::short_text(tokens.input, 10.), p.fg, true), painted(" tokens in", p.text2, false)];
    if tokens.left_out.is_empty() {
        let mut runs = head;
        runs.push(faint_dot(p));
        runs.push(painted(recap::short_text(tokens.output, 10.), p.fg, true));
        runs.push(painted(" out", p.text2, false));
        return text_row(None, runs);
    }
    let missing = tokens.left_out.iter().map(|(_, hours)| hours).sum();
    let (covers, percent) = token_cover(p, r, &head, missing);
    if let [(name, hours)] = tokens.left_out.as_slice() {
        let mut named = head;
        named.push(faint_dot(p));
        named.push(painted("●", p.family_color(terminal::family(name, None)), false));
        named.push(painted(format!(" {name} "), p.text2, false));
        named.push(painted(recap::hours_text(*hours), p.fg, true));
        named.push(painted(" left out", p.dim, false));
        return text_row(None, pick(vec![named, covers, percent]));
    }
    text_row(None, pick(vec![covers, percent]))
}

/// A finding's sentence, wrapped at words. A comma breaks the first two rows when both halves fit.
fn wrapped_finding(text: &str, start: usize) -> Vec<String> {
    let room = LIST_TEXT.saturating_sub(start).max(1);
    let mut lines = terminal::wrap(text, room);
    if lines.is_empty() {
        lines.push(String::new());
    }
    if lines.len() > 1 {
        if let Some((head, tail)) = text.split_once(',') {
            let tail = tail.trim();
            if start + terminal::width(head) + 1 <= LIST_TEXT && start + terminal::width(tail) <= LIST_TEXT {
                return vec![format!("{head},"), tail.to_owned()];
            }
        }
    }
    lines
}

/// Rows from 18: at most three findings, or the quiet line. A note that does not fit is skipped.
fn finding_rows(p: &Palette, r: &Recap, mono: bool) -> Vec<ListRow> {
    if r.findings.is_empty() {
        let mut runs = vec![painted(format!("No findings this {}", span_word(r)), p.text2, false)];
        for note in quiet_notes(r) {
            if row_width(&runs) + 3 + terminal::width(note) <= LIST_TEXT {
                runs.push(faint_dot(p));
                runs.push(painted(note, p.dim, false));
            }
        }
        return vec![text_row(None, runs)];
    }
    let mut rows = Vec::new();
    for finding in r.findings.iter().take(3) {
        let mark = match finding.kind {
            "limit" => Some(if mono { p.fg } else { p.warn }),
            "waiting" => Some(if mono { p.fg } else { p.hot }),
            _ => None,
        };
        let start = terminal::width(&finding.number) + 1;
        let lines = wrapped_finding(&finding.text, start);
        rows.push(text_row(mark, vec![painted(&finding.number, p.bright, true), painted(" ", p.fg, false), painted(&lines[0], p.fg, false)]));
        for more in lines.iter().skip(1) {
            rows.push(text_row(None, vec![painted(" ".repeat(start), p.fg, false), painted(more, p.fg, false)]));
        }
    }
    rows
}

/// The short list, top to bottom. Every cell is painted page, as a card is.
fn list_rows(r: &Recap, p: &Palette, source: Source, mono: bool) -> Vec<ListRow> {
    let scratch = Pen::new(p, 1, 1, mono);
    let bar_parts = parts(&scratch, r);
    let hours_ink = if mono { p.fg } else { scratch.shade(p.accent, 0.4, p.bg) };
    let mut rows = vec![
        list_header(p, r),
        text_row(None, vec![painted(coverage(r, source, false), p.dim, false)]),
        ListRow::Blank,
        hours_row(p, r),
        ListRow::Bar,
        legend_row(p, &bar_parts, mono),
        ListRow::Blank,
    ];
    rows.extend(day_rows(p, r, mono, hours_ink));
    rows.push(ListRow::Blank);
    rows.push(tasks_row(p, r));
    rows.push(prs_row(p, r));
    if let Some(row) = models_row(p, r) {
        rows.push(row);
    }
    rows.push(lines_row(p, r));
    rows.push(handoffs_row(p, r));
    rows.push(tokens_row(p, r));
    rows.push(ListRow::Blank);
    rows.extend(finding_rows(p, r, mono));
    rows
}

/// The short list a terminal under 80 columns gets, and `--narrow` asks for. 39 columns, every
/// cell painted, drawn with the cards' pen.
pub fn list(r: &Recap, p: &Palette, source: Source, mono: bool) -> Canvas {
    let rows = list_rows(r, p, source, mono);
    let mut pen = Pen::new(p, LIST_COLUMNS, rows.len().max(1), mono);
    for (y, row) in rows.iter().enumerate() {
        let y = y as i32;
        match row {
            ListRow::Blank => {}
            ListRow::Bar => {
                let bar_parts = parts(&pen, r);
                split_bar(&mut pen, 1, y, LIST_TEXT, &bar_parts);
            }
            ListRow::Text { mark, runs } => {
                if let Some(ink) = mark {
                    pen.c.text(0, y, "▌", *ink);
                }
                paint_runs(&mut pen, 1, y, runs);
            }
        }
    }
    pen.c
}

/// The nearest color in the 256-color palette, of the 6×6×6 cube and the gray ramp. Nearest is
/// the dashboard's rule too (`Colors::nearest` in the terminal crate, from OpenTUI 0.8.3's
/// palette search), so a theme's role is the same color on the dashboard and here: a hue the
/// palette has no near match for goes gray on both. The first 16 colors are left out, since
/// a terminal's own theme sets them and nothing here asks the terminal what they are.
fn indexed(rgb: Rgb) -> u8 {
    let (r, g, b) = ((rgb >> 16 & 255) as i32, (rgb >> 8 & 255) as i32, (rgb & 255) as i32);
    let step = |v: i32| -> (i32, i32) {
        let i = if v < 48 { 0 } else if v < 115 { 1 } else { (v - 35) / 40 };
        (i, if i == 0 { 0 } else { 55 + 40 * i })
    };
    let ((ri, rv), (gi, gv), (bi, bv)) = (step(r), step(g), step(b));
    // The ramp's 24 grays run from 8 in steps of 10. The nearest is the one nearest the mean
    // of the three, rounded: rounding down picked the gray a step too dark.
    let gray = (r + g + b - 24 + 15).div_euclid(30).clamp(0, 23);
    let gray_value = 8 + 10 * gray;
    let distance = |x: i32, y: i32, z: i32| (r - x).pow(2) + (g - y).pow(2) + (b - z).pow(2);
    if distance(gray_value, gray_value, gray_value) < distance(rv, gv, bv) { (232 + gray) as u8 } else { (16 + 36 * ri + 6 * gi + bi) as u8 }
}

/// A card as text for a terminal's scrollback: each row painted across the card's own width
/// and reset at its end, so the prompt after it keeps the terminal's colors.
pub fn paint(canvas: &Canvas, color: Color) -> String {
    if color == Color::None {
        return canvas.to_text();
    }
    let set = |out: &mut String, layer: u8, rgb: Rgb| match color {
        Color::True => out.push_str(&format!("\x1b[{layer}8;2;{};{};{}m", rgb >> 16 & 255, rgb >> 8 & 255, rgb & 255)),
        _ => out.push_str(&format!("\x1b[{layer}8;5;{}m", indexed(rgb))),
    };
    let mut out = String::new();
    for y in 0..canvas.h {
        let (mut fg, mut bg, mut bold) = (None, None, false);
        for cell in &canvas.cells[y * canvas.w..(y + 1) * canvas.w] {
            if cell.bold != bold {
                out.push_str(if cell.bold { "\x1b[1m" } else { "\x1b[22m" });
                bold = cell.bold;
            }
            if fg != Some(cell.fg) {
                set(&mut out, 3, cell.fg);
                fg = Some(cell.fg);
            }
            if bg != Some(cell.bg) {
                set(&mut out, 4, cell.bg);
                bg = Some(cell.bg);
            }
            if cell.ch != '\0' {
                out.push(cell.ch);
            }
        }
        out.push_str("\x1b[0m");
        if y + 1 < canvas.h {
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recap::{Period, Which};

    fn utc(seconds: i64) -> Option<crate::platform::LocalTime> {
        Some(crate::platform::utc_time(seconds))
    }
    /// A period of `days` days with nothing in it yet, for a test to fill in by hand.
    fn blank(days: usize) -> Recap {
        Recap { days: days as u32, by_day: recap::PerDay::new(days, 0., (0, days)), by_hour: recap::PerDay::new(days, [0.; 24], (0, days)), ..Recap::default() }
    }

    fn week() -> Recap {
        let period = Period::new(recap::ms("2026-11-17T12:00:00.000Z").unwrap(), Which::LastWeek, &utc);
        recap::demo(&period, &utc).unwrap()
    }

    #[test]
    fn cells_are_shared_out_whole() {
        assert_eq!(allot(10, &[1., 1.]), vec![5, 5]);
        assert_eq!(allot(10, &[98., 1., 1.]), vec![8, 1, 1]);
        assert_eq!(allot(10, &[5., 0., 5.]), vec![5, 0, 5]);
        assert_eq!(allot(10, &[0., 0.]), vec![0, 0]);
        assert_eq!(percents(&[46.4, 47.9, 5.7]).iter().sum::<u32>(), 100);
    }

    #[test]
    fn a_period_names_its_months_once() {
        assert_eq!(period_text(Span::Week, "2026-11-09", "2026-11-15"), "Nov 9 – 15");
        assert_eq!(period_text(Span::Week, "2026-10-26", "2026-11-01"), "Oct 26 – Nov 1");
        assert_eq!((period_with_year(Span::Week, "2026-12-28", "2027-01-03"), period_text(Span::Month, "2026-10-01", "2026-10-31")), ("Dec 28 – Jan 3, 2027".into(), "October 2026".into()));
        assert_eq!(period_with_year(Span::Month, "2026-10-01", "2026-10-31"), "October 2026");
    }

    #[test]
    fn the_card_is_eighty_by_twenty_three_and_says_what_it_shows() {
        let r = week();
        let c = card(&r, &Palette::default(), Source::Demo, false);
        assert_eq!((c.w, c.h), (80, 23));
        let text = c.to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        assert!(rows[0].starts_with(" T3 Agents  RECAP  Nov 9 – 15  demo data · made up"), "{}", rows[0]);
        assert!(rows[0].ends_with(COMMAND));
        assert!(rows[2].contains("AGENT HOURS") && rows[2].contains("busiest ") && rows[2].contains("TASKS"));
        assert!(rows[6].contains("at once") && rows[6].contains("M  T  W  T  F  S  S") && rows[6].contains("finished"));
        assert!(rows[8].starts_with(" WHERE THE HOURS WENT ─") && rows[8].contains("of "));
        assert!(rows[10].contains("■ tools ") && rows[10].contains("■ model ") && rows[10].contains("■ waiting on you "));
        assert!(rows[12].starts_with(" MODELS ─") && rows[12].ends_with("models · by run time"));
        assert!(rows[14].contains("● ") && rows[14].contains(" more "));
        assert!(rows[16].contains("TASKS") && rows[16].contains("HANDED TO AGENTS") && rows[16].contains("TOKENS"));
        assert!(rows[17].contains(" from you") && rows[18].contains(" from agents"));
        assert_eq!(rows.len(), 23);
        // Every row keeps to the card, and the three parts of the bar fill it.
        assert!(rows.iter().all(|row| terminal::width(row) <= 80));
        assert_eq!(rows[9].chars().filter(|ch| *ch == '▆').count(), 76);
    }

    #[test]
    fn without_color_the_parts_differ_by_fill_and_nothing_is_painted() {
        let r = week();
        let plain = paint(&card(&r, &Palette::default(), Source::Demo, true), Color::None);
        assert!(!plain.contains('\x1b'));
        let bar = plain.split('\n').nth(9).unwrap();
        assert!(bar.contains('▆') && bar.contains('░') && bar.contains('▄'));
        let painted = paint(&card(&r, &Palette::default(), Source::Demo, false), Color::True);
        assert!(painted.contains("\x1b[38;2;") && painted.contains("\x1b[48;2;") && painted.ends_with("\x1b[0m"));
        assert!(paint(&card(&r, &Palette::default(), Source::Demo, false), Color::Indexed).contains("\x1b[38;5;"));
    }

    #[test]
    fn the_256_color_palette_takes_the_nearest() {
        assert_eq!((indexed(0x000000), indexed(0xffffff), indexed(0xff0000)), (16, 231, 196));
        assert_eq!(indexed(0x808080), 244);
    }

    #[test]
    fn a_week_with_little_in_it_stays_honest() {
        let mut r = Recap { from: "2026-11-09".into(), to: "2026-11-15".into(), days_recorded: 7, days_elapsed: 7, hours: 0.67, ..blank(7) };
        r.split = recap::Split { tools: 0.35, model: 0.32, waiting: 0. };
        r.by_day[2] = 0.67;
        r.most_at_once = 1;
        r.tasks.total = 3;
        r.tasks.finished = 3;
        r.tasks.from_you = 3;
        r.lines_added = 412;
        r.lines_removed = 37;
        r.models = vec![recap::Model { name: "sonnet 5.5".into(), family: "claude", hours: 0.67, runs: 3, tokens_in: None }];
        let text = card(&r, &Palette::default(), Source::Records, false).to_text();
        for expected in ["7 of 7 days recorded", "busiest Wed · 40m", "one at a time", "all finished", "LINES ADDED", "37 removed", "waiting on you none",
            "one model", "● sonnet 5.5 40m", "PULL REQUESTS", "none linked", "none from agents", "none reported", "No findings this week · no plan limits · no failures · no long waits"] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
    }

    #[test]
    fn shaded_marks_keep_three_to_one_in_every_built_in_theme() {
        use crate::theme::{BUILTINS, Paths, Source as ThemeSource, palette};
        for theme in BUILTINS {
            let p = palette(&ThemeSource::Builtin(theme.name), &Paths::default());
            let pen = Pen::new(&p, CARD_COLUMNS, 1, false);
            for (toward, on) in [(0.55, p.bg), (0.4, p.panel)] {
                let mark = pen.shade(p.accent, toward, on);
                assert!(crate::integration::contrast(mark, on) >= 3., "{}: {mark:06x} on {on:06x}", theme.name);
                assert_ne!(mark, p.accent, "{}: the shade must still differ from the accent", theme.name);
            }
            // The hour grid and the month's calendar: every step that means an agent reads on
            // the page, and each step stands apart from the one before it. Mixed toward the
            // accent alone, two steps came as close as 1.21:1.
            let inks = grid_inks(&pen);
            let from_page = |ink: Rgb| crate::integration::contrast(ink, p.bg);
            for i in 1..4 {
                assert!(from_page(inks[i]) >= 3., "{}: step {i} {:06x} on {:06x}", theme.name, inks[i], p.bg);
                let apart = crate::integration::contrast(inks[i], inks[i - 1]);
                assert!(apart >= 1.6, "{}: steps {} and {i} are {apart:.2}:1 apart", theme.name, i - 1);
                // More agents is further from the page, never back toward it.
                assert!(from_page(inks[i]) > from_page(inks[i - 1]), "{}: step {i} is no further from the page than step {}", theme.name, i - 1);
            }
        }
    }

    #[test]
    fn the_wide_card_is_120_by_39_and_ends_with_how_it_is_counted() {
        let r = week();
        let c = wide(&r, &Palette::default(), Source::Demo, false);
        assert_eq!((c.w, c.h), (WIDE_COLUMNS, WIDE_ROWS - 1));
        let text = c.to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        assert!(rows[0].starts_with(" T3 Agents  RECAP  Nov 9 – 15, 2026  demo data · made up") && rows[0].ends_with(COMMAND));
        assert!(rows[2].contains("AGENT HOURS") && rows[2].contains("TASKS") && rows[2].contains("MOST AT ONCE") && rows[2].contains("PRS MERGED"));
        assert!(rows[6].contains(" runs · ") && rows[6].contains(" did not") && rows[6].contains(r.most_at_once_when.as_deref().unwrap()));
        assert!(rows[8].starts_with(" AGENTS AT WORK, BY HOUR ─") && rows[8].contains("hours a day    WHERE THE HOURS WENT ─"));
        // Seven days of 24 hours, two cells an hour, and each day's hours after them.
        for (day, row) in rows[9..16].iter().enumerate() {
            let cells: String = row.chars().skip(5).take(48).collect();
            assert!(row.starts_with(&format!(" {} ", DAYS[day])) && cells.chars().all(|ch| ch == '▆'), "{row}");
            assert!(row.chars().skip(53).take(5).collect::<String>().trim_start().ends_with(['h', 'm']), "{row}");
        }
        assert!(rows[16].starts_with("     0           6           12          18        24"));
        assert!(rows[17].starts_with(" agents on average: ▆▆ under 1  ▆▆ 1 to 2  ▆▆ 2 or more"));
        assert!(rows[11].contains("■ tools") && rows[11].contains("a tool call running") && rows[13].contains("from asked to answered"));
        assert!(rows[15].contains("WAITING ON YOU ─") && rows[16].contains(" asks · typical ") && rows[16].contains(" · longest "));
        assert!(rows[19].starts_with(" MODELS ─") && rows[19].contains("HANDED TO AGENTS ─"));
        assert!(rows[20].contains("hours     runs     tokens in") && rows[20].contains("from → to"));
        // Four models and the rest as one, whose tokens are known only in part.
        // Four models, then the rest as one row with no provider's dot: some of those report
        // no tokens, so theirs is "at least".
        assert!(rows[21].starts_with(" ● ") && rows[25].starts_with("   2 more ") && rows[25].contains("M+  "), "{}", rows[25]);
        assert!(rows[19].contains(" handoffs · ") && rows[21].contains(" → ● ") && rows[24].contains("other handoffs") && rows[25].contains(" did not finish"));
        assert!(rows[27].starts_with(" TASKS ─") && rows[27].contains("HOW THEY ENDED ─") && rows[27].contains("LINES CHANGED ─"));
        assert!(rows[28].contains(" of them by phone") && rows[29].contains(" started by another agent") && rows[30].contains("across ") && rows[30].contains("PRs "));
        assert!(!rows[30].contains(" 0 "), "a zero is not listed: {}", rows[30]);
        assert!(rows[32].starts_with(" TOOLS ─") && rows[32].contains("CONTEXT ─") && rows[32].contains("TOKENS ─"));
        assert!(rows[33].contains(" of them commands") && rows[34].contains(" commands failed or found nothing") && rows[34].contains("reported no tokens"));
        assert!(rows[36].starts_with(" HOW THIS IS COUNTED ─") && rows[37].starts_with(" Agent hours: ") && rows[38].ends_with(" Lines include generated files. Times are local. Read from T3's records. Numbers may be cached here; nothing is sent."));
        assert!(rows.len() == 39 && rows.iter().all(|row| terminal::width(row) <= 120));
        assert!(COUNTED.iter().all(|line| terminal::width(line) <= 118));
        assert!(COUNTED.iter().all(|line| !line.contains("saved") && !line.contains("stored")), "the cache may keep run summaries");
    }

    #[test]
    fn the_tasks_tile_says_what_adds_up_to_its_number() {
        let note = |running: u32| {
            let mut r = week();
            (r.tasks.total, r.tasks.finished, r.tasks.running) = (10, 6, running);
            wide(&r, &Palette::default(), Source::Demo, false).to_text().split('\n').nth(6).unwrap().to_owned()
        };
        // Nothing running: the rest did not finish. All of the rest running: say so. A mix:
        // neither word is true of all four, and the block below has the detail.
        assert!(note(0).contains("6 finished · 4 did not") && note(4).contains("6 finished · 4 running") && note(1).contains("6 finished · 4 others"));
    }

    #[test]
    fn the_wide_card_without_color_tells_its_parts_apart_by_fill() {
        let plain = paint(&wide(&week(), &Palette::default(), Source::Demo, true), Color::None);
        assert!(!plain.contains('\x1b'));
        let rows: Vec<&str> = plain.split('\n').collect();
        let grid: String = rows[9..16].concat();
        assert!(['·', '░', '▄', '█'].iter().all(|fill| grid.contains(*fill)), "{grid}");
        assert!(rows[9].contains('▆') && rows[9].contains('░') && rows[9].contains('▄'));
        assert!(rows[17].contains("░░ under 1  ▄▄ 1 to 2  ██ 2 or more"));
        assert!(rows[21].contains('▆') && rows[22].contains('░'));
    }

    #[test]
    fn the_wide_card_stays_honest_on_a_week_with_little_in_it() {
        let mut r = Recap { from: "2026-11-09".into(), to: "2026-11-15".into(), days_recorded: 3, days_elapsed: 5, first_recorded_day: 2, hours: 0.67, ..blank(7) };
        r.split = recap::Split { tools: 0.35, model: 0.32, waiting: 0. };
        r.by_day[2] = 0.67;
        r.by_hour[2][10] = 0.67;
        (r.most_at_once, r.most_at_once_when, r.runs, r.threads, r.projects) = (1, Some("Wed 10:05".into()), 3, 1, 1);
        (r.tasks.total, r.tasks.finished, r.tasks.from_you) = (3, 3, 3);
        (r.lines_added, r.lines_removed, r.file_changes, r.runs_changing_files) = (412, 37, 1, 1);
        r.models = vec![recap::Model { name: "sonnet 5.5".into(), family: "claude", hours: 0.67, runs: 3, tokens_in: None }];
        let text = wide(&r, &Palette::default(), Source::Records, false).to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        for expected in ["3 runs · 1 thread", "all finished", "Wed 10:05", "LINES ADDED", "37 removed", "one model", "not reported", "LINES AND PRS", "1 file change in 1 run",
            "no pull requests linked", "none started by another agent", "across 1 project", "✓ 3 finished · none failed", "no command failed", "no thread was compacted",
            "no turn ran past 90% full", "none reported"] {
            assert!(text.contains(expected), "missing {expected:?} in\n{text}");
        }
        // A day before the records begin says so once; a day that has not come says nothing.
        assert!(rows[9].starts_with(" Mon before the records begin ┄") && rows[10].starts_with(" Tue ┄┄┄") && rows[9].contains(" —") && !rows[10].contains("before"));
        assert!(rows[11].starts_with(" Wed ▆") && rows[11].contains(" 40m") && rows[12].starts_with(" Thu ▆") && rows[12].contains(" none"));
        assert_eq!((rows[14].trim_end(), rows[15].split("    ").next().unwrap()), (" Sat", " Sun"));
        // Nobody was asked and nothing was handed over: each block says so under its own name.
        assert!(rows[13].contains("waiting on you") && rows[13].contains("none") && rows[16].contains("none this week") && rows[20].contains("none this week"));
    }

    /// A month by hand: the weekday of its 1st (Monday is 0), each day's agent hours, the
    /// first day with records, and how many days have come.
    fn month(first_weekday: u32, hours: &[f64], first_recorded: u32, elapsed: u32) -> Recap {
        let days = hours.len();
        let mut r = Recap {
            period: Span::Month,
            from: "2026-10-01".into(),
            to: format!("2026-10-{days}"),
            so_far: (elapsed as usize) < days,
            days_recorded: elapsed - first_recorded,
            days_elapsed: elapsed,
            first_recorded_day: first_recorded,
            first_weekday,
            hours: hours.iter().sum(),
            ..blank(days)
        };
        // A day with no records holds nothing, as it would when read.
        for (day, hours) in hours.iter().enumerate() {
            r.by_day[day] = if recorded(&r, day) { *hours } else { 0. };
        }
        r.hours = r.by_day.iter().sum();
        let mut active: Vec<f64> = r.by_day.iter().copied().filter(|h| *h > 0.).collect();
        active.sort_by(f64::total_cmp);
        r.days_active = active.len() as u32;
        r.typical_day = active.get(active.len() / 2).copied();
        (r.runs, r.most_at_once, r.most_at_once_when) = (1, 2, Some("Fri 30, 15:21".into()));
        r
    }

    /// October 2026: 31 days from a Thursday, so five week rows. Quiet weekends, a busy 30th.
    fn october() -> Recap {
        let hours: Vec<f64> = (0..31).map(|day| match (day + 3) % 7 { 5 | 6 => 0., _ if day == 29 => 36., 3 => 24., 1 => 12.5, _ => 6. }).collect();
        month(3, &hours, 0, 31)
    }

    #[test]
    fn a_months_days_are_shaded_against_its_busiest() {
        let r = october();
        let shades = Shades::new(&r);
        // A third and two thirds of the busiest day's 36 hours.
        assert_eq!((shades.cuts, shades.busiest), ([12., 24.], Some(29)));
        // Sat 3 none, Fri 2 under 12h, Tue 6 12h to 24h, Thu 8 and Fri 30 24h or more.
        assert_eq!([2, 1, 5, 7, 29].map(|day| shades.level(&r, day)), [Some(0), Some(1), Some(2), Some(3), Some(3)]);
        assert_eq!(shades.key(), vec![(1, "under 12h".into()), (2, "12h to 24h".into()), (3, "24h or more".into())]);
        // A day is sorted by the cut points as the key writes them. A busiest day of 43h cuts
        // at 14.33h and 28.67h, written 14h and 29h: 14.1h is "14h to 29h", and so is 28.8h.
        let close = month(0, &[43., 14.1, 28.8, 29., 13.9], 0, 5);
        let shades = Shades::new(&close);
        assert_eq!((shades.cuts, [1, 2, 3, 4].map(|day| shades.level(&close, day))), ([14., 29.], [Some(2), Some(2), Some(3), Some(1)]));
        // Before the records begin and after today a day has no shade: it is not a zero.
        // What is left is shaded against its own busiest day, Thursday the 15th.
        let part = month(3, &r.by_day, 13, 18);
        assert_eq!([12, 13, 14, 17, 18].map(|day| Shades::new(&part).level(&part, day)), [None, Some(1), Some(3), Some(0), None]);
        // A month so quiet that the cut points are written alike names fewer steps, never "0m".
        let quiet = month(3, &[0.02, 0.01, 0.], 0, 3);
        assert_eq!(Shades::new(&quiet).key(), vec![(2, "under 1m".into()), (3, "1m or more".into())]);
        assert_eq!(Shades::new(&month(3, &[0.004], 0, 1)).key(), vec![(3, "agents at work".into())]);
    }

    #[test]
    fn the_month_card_has_a_column_for_each_week() {
        let r = october();
        let p = Palette::default();
        let c = card(&r, &p, Source::Records, false);
        assert_eq!((c.w, c.h), (80, 23));
        let text = c.to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        assert!(rows[0].starts_with(" T3 Agents  RECAP  October 2026  31 of 31 days recorded") && rows[0].ends_with(COMMAND), "{}", rows[0]);
        // Five weeks, each under the date it begins on. The last is the busiest: 85h.
        assert!(rows[2].contains("AGENT HOURS         busiest week · 85h    TASKS"), "{}", rows[2]);
        assert!(rows[6].starts_with("  up to 2 at once    1  5 12 19 26"), "{}", rows[6]);
        assert!(rows.iter().all(|row| terminal::width(row) <= 80));
        let columns = |r: &Recap, mono: bool, y: usize| card(r, &p, Source::Records, mono).to_text().split('\n').nth(y).unwrap().chars().skip(20).take(20).collect::<String>().trim_end().to_owned();
        // 30h, three weeks of 54.5h and 84.5h, the tallest 20 eighths high.
        assert_eq!((columns(&r, true, 3), columns(&r, true, 4), columns(&r, true, 5)), ("            ▄▄".into(), "   ▅▅ ▅▅ ▅▅ ██".into(), "▇▇ ██ ██ ██ ██".into()));
        // The busiest week is bright and its date bold; the others wear the accent's shade.
        let cell = |x: i32, y: i32| c.get(x, y).map(|cell| (cell.ch, cell.fg, cell.bold)).unwrap();
        assert_eq!((cell(32, 3), cell(33, 6)), (('▄', p.bright, false), ('6', p.fg, true)));
        assert!(cell(20, 5).1 != p.bright && cell(21, 6) == ('1', p.dim, false));

        // Four weeks (February 2027, from a Monday) and six (August 2026, from a Saturday).
        let february = month(0, &[5.; 28], 0, 28);
        assert_eq!((columns(&february, true, 3), columns(&february, true, 6)), ("▄▄ ▄▄ ▄▄ ▄▄".into(), " 1  8 15 22".into()));
        let august = month(5, &[5.; 31], 0, 31);
        assert_eq!((columns(&august, true, 5), columns(&august, true, 6)), ("▆▆ ██ ██ ██ ██ ▃▃".into(), " 1  3 10 17 24 31".into()));

        // A month so far, to the 18th, with records from the 14th. The weeks before the
        // records are a faint dot, the weeks not yet come are only their dates, and neither
        // is a zero. The week it has reached shows its hours so far.
        let part = month(3, &r.by_day, 13, 18);
        let c = card(&part, &p, Source::Records, false);
        let text = c.to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        assert!(rows[0].starts_with(" T3 Agents  RECAP  October 2026  so far · 5 days recorded") && rows[2].contains("busiest week · 36h"), "{}", rows[0]);
        assert_eq!((columns(&part, true, 3), columns(&part, true, 5), columns(&part, true, 6)), ("      ▄▄".into(), "·  ·  ██".into(), " 1  5 12 19 26".into()));
        let ink = |x: i32, y: i32| c.get(x, y).map(|cell| (cell.ch, cell.fg)).unwrap();
        assert_eq!((ink(20, 5), ink(21, 6), ink(30, 6), ink(29, 5).0), (('·', p.faint), ('1', p.faint), ('9', p.faint), ' '));
        assert!(rows[20].starts_with("  No findings this month"), "{}", rows[20]);
        // A recorded week with no agent time is a flat track, not a gap, and a week with
        // very little in it stands taller than that, so the two differ without color too.
        let mut quiet = october();
        for day in 4..18 {
            quiet.by_day[day] = 0.;
        }
        quiet.by_day[11] = 0.5;
        assert_eq!(columns(&quiet, true, 5), "▇▇ ▁▁ ▂▂ ██ ██");
    }

    #[test]
    fn the_wide_month_is_a_calendar_with_its_dates_and_each_weeks_hours() {
        let r = october();
        let c = wide(&r, &Palette::default(), Source::Records, false);
        let text = c.to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        let left = |y: usize| rows[y].chars().take(58).collect::<String>();
        assert!(rows[0].starts_with(" T3 Agents  RECAP  October 2026  31 of 31 days recorded"), "{}", rows[0]);
        assert!(rows[6].contains("Fri 30, 15:21"));
        // What did not finish is still said when the counts are too long for the first form.
        let mut many = october();
        (many.tasks.total, many.tasks.finished) = (2_931, 2_842);
        assert!(wide(&many, &Palette::default(), Source::Records, false).to_text().split('\n').nth(6).unwrap().contains("  2,842 of 2,931 finished  "));
        assert!(left(8).starts_with(" AGENTS AT WORK, BY DAY ─") && left(8).ends_with("  busiest Fri 30 · 36h"), "{}", left(8));
        assert_eq!(left(9), " Mon    Tue    Wed    Thu    Fri    Sat    Sun        week");
        assert_eq!(left(10), "                       1 ▆▆▆  2 ▆▆▆  3 ▆▆▆  4 ▆▆▆      30h");
        assert_eq!(left(11), "  5 ▆▆▆  6 ▆▆▆  7 ▆▆▆  8 ▆▆▆  9 ▆▆▆ 10 ▆▆▆ 11 ▆▆▆      55h");
        assert_eq!(left(14), " 26 ▆▆▆ 27 ▆▆▆ 28 ▆▆▆ 29 ▆▆▆ 30 ▆▆▆ 31 ▆▆▆             85h");
        assert_eq!((left(15).trim(), left(16).trim_end()), ("", " agents ran on 22 of 31 days · 6.0h on a typical one"));
        assert_eq!(left(17).trim_end(), " hours a day: ▆▆ under 12h  ▆▆ 12h to 24h  ▆▆ 24h or more");
        // The block keeps to its side of the card, and the busiest day and week are bold.
        assert!(rows[8].chars().skip(58).collect::<String>().starts_with("    WHERE THE HOURS WENT ─"), "{}", rows[8]);
        assert!(c.get(29, 14).is_some_and(|cell| cell.bold) && c.get(55, 14).is_some_and(|cell| cell.bold) && c.get(55, 11).is_some_and(|cell| !cell.bold));
        assert!(rows.len() == 39 && rows.iter().all(|row| terminal::width(row) <= 120));

        // Without color the four steps are four fills.
        let plain = wide(&r, &Palette::default(), Source::Records, true).to_text();
        let rows: Vec<&str> = plain.split('\n').collect();
        assert!(rows[11].starts_with("  5 ░░░  6 ▄▄▄  7 ░░░  8 ███  9 ░░░ 10 ··· 11 ···      55h"), "{}", rows[11]);
        assert!(rows[17].starts_with(" hours a day: ░░ under 12h  ▄▄ 12h to 24h  ██ 24h or more"), "{}", rows[17]);

        // Records from the 14th, and today the 18th: a date with no records keeps its place
        // and gets no mark. A week before the records is a dash, one not yet come is blank.
        let part = month(3, &r.by_day, 13, 18);
        let plain = wide(&part, &Palette::default(), Source::Records, true).to_text();
        let rows: Vec<&str> = plain.split('\n').collect();
        let left = |y: usize| rows[y].chars().take(58).collect::<String>();
        assert!(rows[0].starts_with(" T3 Agents  RECAP  October 2026  so far, to Oct 18 · 5 days recorded"), "{}", rows[0]);
        assert_eq!(left(10), "                       1      2      3      4            —");
        assert_eq!(left(11), "  5      6      7      8      9     10     11            —");
        assert_eq!(left(12), " 12     13     14 ░░░ 15 ███ 16 ░░░ 17 ··· 18 ···      36h");
        assert_eq!((left(13).trim_end(), left(14).trim_end()), (" 19     20     21     22     23     24     25", " 26     27     28     29     30     31"));
        assert_eq!(left(16).trim_end(), " agents ran on 3 of 5 days · 6.0h on a typical one");
        assert!(rows[16].contains("none this month") && rows[20].contains("none this month"));

        // Six week rows still leave the summary and the key their rows.
        let august = month(5, &[5.; 31], 0, 31);
        let plain = wide(&august, &Palette::default(), Source::Records, true).to_text();
        let rows: Vec<&str> = plain.split('\n').collect();
        assert!(rows[10].starts_with(&format!("{}1 ███  2 ███      10h", " ".repeat(37))) && rows[15].starts_with(" 31 ███ ") && rows[16].starts_with(" agents ran on 31 of 31 days"), "{}\n{}", rows[10], rows[15]);
    }

    #[test]
    fn a_heavy_month_keeps_every_number_in_its_place() {
        // Four-digit hours and five-digit counts: what a busy team's month comes to.
        let mut r = october();
        (r.hours, r.runs, r.threads, r.projects, r.most_at_once) = (12_345., 99_999, 9_999, 128, 112);
        r.split = recap::Split { tools: 5_555., model: 6_000., waiting: 790. };
        let t = &mut r.tasks;
        (t.total, t.from_you, t.from_phone, t.from_agents, t.finished, t.failed, t.stopped, t.cancelled) = (99_999, 60_000, 20_000, 39_999, 98_000, 999, 500, 500);
        (t.failed_setup, t.failed_limit, t.failed_provider) = (333, 333, 333);
        (r.tool_calls, r.commands, r.commands_failed, r.compactions, r.turns_past_90) = (99_999, 99_999, 9_999, 9_999, 9_999);
        (r.lines_added, r.lines_removed, r.file_changes, r.runs_changing_files) = (99_999_999, 9_999_999, 99_999, 99_999);
        (r.prs_merged, r.prs_open, r.prs_closed) = (999, 999, 999);
        r.handed = recap::Handed { count: 99_999, hours: 9_999., failed: 999, pairs: vec![("opus 5.5".into(), "gpt 6.1-sol".into(), 55_555)] };
        r.asks = recap::Asks { count: 9_999, hours: 790., typical_ms: Some(104_000.), longest_ms: Some(9_124_000.) };
        r.tokens = recap::Tokens { input: 9.9e11, output: 9.9e9, cached: 9.7e11, left_out: vec![("grok".into(), 1_234.)], reported: true };
        r.models = (0..7).map(|i| recap::Model { name: format!("model-{i}"), family: "claude", hours: 2_000. - 100. * f64::from(i), runs: 14_000, tokens_in: Some(1.4e11) }).collect();
        for (canvas, width) in [(card(&r, &Palette::default(), Source::Records, false), 80), (wide(&r, &Palette::default(), Source::Records, false), 120)] {
            let text = canvas.to_text();
            assert!(text.split('\n').all(|row| terminal::width(row) <= width));
            // The hours keep their unit past a thousand, and stop short of the calendar.
            assert!(text.split('\n').nth(5).unwrap().chars().skip(2).take(16).collect::<String>().trim_end().ends_with("K h"), "{text}");
        }
        let text = wide(&r, &Palette::default(), Source::Records, false).to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        let column = |y: usize, from: usize, cells: usize| rows[y].chars().skip(from).take(cells).collect::<String>();
        // Each ledger column ends before the next begins.
        for y in [28, 29, 30, 33, 34] {
            assert!(column(y, 39, 2).trim().is_empty() && column(y, 79, 2).trim().is_empty(), "row {y}: {}", rows[y]);
        }
        assert!(column(33, 1, 38).starts_with("99,999 calls · 99,999 of them commands") && column(28, 1, 38).contains("20,000 by phone"), "{}", rows[33]);
        assert!(column(29, 41, 38).starts_with("setup 333 · limit 333 · provider 333") && column(34, 1, 38).starts_with("9,999 commands failed or found nothing"), "{}\n{}", rows[29], rows[34]);
        assert!(rows[6].starts_with("  99,999 runs  ") && !text.contains('…') && rows[19].contains("HANDED TO AGENTS ─") && rows[19].ends_with(" 99,999 · 9999h"), "{}\n{}", rows[6], rows[19]);
        // The 80-column card: its columns too, and a provider with three-digit hours left
        // out says what the totals cover, since its name and hours no longer fit.
        let text = card(&r, &Palette::default(), Source::Records, false).to_text();
        let rows: Vec<&str> = text.split('\n').collect();
        for (y, gap) in [(17, 20), (18, 20), (17, 40), (18, 40), (17, 60), (18, 60)] {
            assert_eq!(rows[y].chars().nth(gap), Some(' '), "row {y}: {}", rows[y]);
        }
        assert!(rows[17].contains("99,999 · 9999h") && rows[18].ends_with("covers 90% of hours") && !text.contains('…'), "{}\n{}", rows[17], rows[18]);
    }

    #[test]
    fn a_notice_wraps_what_it_says() {
        let c = notice(&Palette::default(), "Nov 9 – 15", "can't read T3", true, "Can't read the T3 database at /somewhere/quite/long/userdata/statev2.sqlite: database is locked.",
            &["Is T3 Code installed, and has it been opened at least once?".into()]);
        let text = c.to_text();
        assert!(text.contains("can't read T3") && text.contains("  · Is T3 Code installed"));
        assert!(text.split('\n').all(|row| terminal::width(row) <= 80));
    }

    /// The thin week from the list's scenarios: one Wednesday, one model, nothing else.
    fn thin_week() -> Recap {
        let mut r = blank(7);
        r.from = "2026-11-09".into();
        r.to = "2026-11-15".into();
        (r.days_recorded, r.days_elapsed, r.first_recorded_day, r.first_weekday) = (7, 7, 0, 0);
        (r.hours, r.most_at_once, r.days_active, r.typical_day) = (0.67, 1, 1, Some(0.67));
        r.by_day[2] = 0.67;
        r.split = recap::Split { tools: 0.35, model: 0.32, waiting: 0. };
        (r.tasks.total, r.tasks.finished, r.tasks.failed) = (3, 3, 0);
        (r.lines_added, r.lines_removed, r.file_changes) = (412, 37, 1);
        r.models = vec![recap::Model { name: "sonnet 5.5".into(), family: "claude", hours: 0.67, runs: 3, tokens_in: None }];
        r
    }

    /// The heavy month from the list's scenarios: 12,345h, 99,999 tasks, seven models.
    fn heavy_month() -> Recap {
        let pattern: Vec<f64> = (0..31).map(|day| {
            let weekday = (day + 3) % 7;
            if weekday == 5 || weekday == 6 { 0. } else if day == 29 { 36. } else if weekday == 3 { 24. } else if weekday == 1 { 12.5 } else { 6. }
        }).collect();
        let scale = 12_345. / pattern.iter().sum::<f64>();
        let hours: Vec<f64> = pattern.iter().map(|hour| hour * scale).collect();
        let mut r = month(3, &hours, 0, 31);
        (r.hours, r.most_at_once, r.days_active, r.typical_day) = (12_345., 112, 22, Some(6. * scale));
        r.split = recap::Split { tools: 5_555., model: 6_000., waiting: 790. };
        (r.tasks.total, r.tasks.finished, r.tasks.failed) = (99_999, 98_000, 999);
        (r.prs_merged, r.prs_open, r.prs_closed) = (999, 999, 999);
        (r.lines_added, r.lines_removed, r.file_changes) = (99_999_999, 9_999_999, 99_999);
        r.handed = recap::Handed { count: 99_999, hours: 9_999., failed: 999, pairs: vec![] };
        r.tokens = recap::Tokens { input: 9.9e11, output: 9.9e9, cached: 0., left_out: vec![("grok".into(), 1_234.)], reported: true };
        r.models = (0..7).map(|i| recap::Model { name: format!("model-{i}"), family: "claude", hours: 2_000. - 100. * f64::from(i), runs: 0, tokens_in: None }).collect();
        r.findings = vec![
            recap::Finding { kind: "limit", number: "333".into(), text: "tasks stopped on a plan limit".into(), rests_on: "not shown".into() },
            recap::Finding { kind: "waiting", number: "790h".into(), text: "agents waited on you, over 9,999 asks".into(), rests_on: "not shown".into() },
            recap::Finding { kind: "failed", number: "666".into(), text: "other tasks failed, of 99,999".into(), rests_on: "not shown".into() },
        ];
        r
    }

    /// The list as plain rows. Trailing spaces are already gone, as a comparison with a frame ignores them.
    fn list_rows(r: &Recap, mono: bool) -> Vec<String> {
        list(r, &Palette::default(), Source::Records, mono).to_text().split('\n').map(str::to_owned).collect()
    }

    fn assert_list(r: &Recap, mono: bool, expected: &[&str]) {
        let canvas = list(r, &Palette::default(), Source::Records, mono);
        assert_eq!(canvas.w, LIST_COLUMNS);
        let rows = canvas.to_text().split('\n').map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(rows.len(), expected.len(), "{}", rows.join("\n"));
        for (i, (got, want)) in rows.iter().zip(expected).enumerate() {
            assert_eq!(got, want, "row {i}");
            assert!(terminal::width(got) <= LIST_COLUMNS, "row {i} is {} cells: {got}", terminal::width(got));
        }
    }

    #[test]
    fn the_thin_week_and_the_heavy_month_match_the_list() {
        let thin = [
            " T3 Agents  RECAP  Nov 9 – 15, 2026",
            " 7 of 7 days recorded",
            "",
            " 40m agent hours · one at a time",
            " ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆ ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆",
            " ■ tools 52% ■ model 48%",
            "",
            " ▁▁▁▁ ▁▁▁▁ ████ ▁▁▁▁ ▁▁▁▁ ▁▁▁▁ ▁▁▁▁",
            " M    T    W    T    F    S    S",
            " busiest Wed · 40m",
            "",
            " 3 tasks · all finished",
            " no pull requests linked",
            " ● sonnet 5.5 40m · one model",
            " +412 -37 lines · 1 file change",
            " nothing handed to agents",
            " no tokens reported",
            "",
            " No findings this week · no failures",
        ];
        let heavy = [
            " T3 Agents  RECAP  October 2026",
            " 31 of 31 days recorded",
            "",
            " 12345h agent hours",
            " ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆ ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆ ▆▆",
            " ■ tools 45% ■ model 49% ■ waiting 6%",
            "",
            " ▅▂▁▁ ▂▃▂▅▂▁▁ ▂▃▂▅▂▁▁ ▂▃▂▅▂▁▁ ▂▃▂▅█▁",
            " 1    5       12      19      26",
            " busiest Fri 30 · 1599h · typical 266h",
            "",
            " 99,999 tasks · 999 failed",
            " 999 PRs merged · 999 open",
            " ● model-0 2000h  6 more 9900h",
            " +100M -10.0M lines",
            " 99,999 handoffs · 9999h",
            " 990B tokens in · covers 90% of hours",
            "",
            "▌333 tasks stopped on a plan limit",
            "▌790h agents waited on you,",
            "      over 9,999 asks",
            " 666 other tasks failed, of 99,999",
        ];
        let heavy_mono = [
            " T3 Agents  RECAP  October 2026",
            " 31 of 31 days recorded",
            "",
            " 12345h agent hours",
            " ▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆▆ ░░░░░░░░░░░░░░░░░ ▄▄",
            " ▆ tools 45% ░ model 49% ▄ waiting 6%",
            "",
            " ▅▂▁▁ ▂▃▂▅▂▁▁ ▂▃▂▅▂▁▁ ▂▃▂▅▂▁▁ ▂▃▂▅█▁",
            " 1    5       12      19      26",
            " busiest Fri 30 · 1599h · typical 266h",
            "",
            " 99,999 tasks · 999 failed",
            " 999 PRs merged · 999 open",
            " ● model-0 2000h  6 more 9900h",
            " +100M -10.0M lines",
            " 99,999 handoffs · 9999h",
            " 990B tokens in · covers 90% of hours",
            "",
            "▌333 tasks stopped on a plan limit",
            "▌790h agents waited on you,",
            "      over 9,999 asks",
            " 666 other tasks failed, of 99,999",
        ];
        assert_list(&thin_week(), false, &thin);
        assert_list(&heavy_month(), false, &heavy);
        assert_list(&heavy_month(), true, &heavy_mono);
        // The evidence a finding rests on is not on the list, and neither is a token total that was not reported.
        let heavy_rows = list_rows(&heavy_month(), false);
        assert!(heavy_rows.iter().all(|row| !row.contains("not shown")));
        assert!(thin.iter().any(|row| *row == " no tokens reported"));
        assert!(thin.iter().any(|row| row.contains("No findings")) && thin.iter().all(|row| !row.contains('▌')));
    }

    #[test]
    fn day_marks_and_finding_marks_use_the_cards_inks() {
        let p = Palette::default();
        let thin = list(&thin_week(), &p, Source::Records, false);
        let marks = 7;
        // Monday is a recorded zero. Wednesday is the only day with hours, so it is the busiest.
        assert_eq!((thin.cells[marks * thin.w + 1].ch, thin.cells[marks * thin.w + 1].fg), ('▁', p.track));
        assert_eq!((thin.cells[marks * thin.w + 11].ch, thin.cells[marks * thin.w + 11].fg), ('█', p.bright));
        // A day before the records, and one that has not come, are a faint dot. A recorded zero stays on the track.
        let mut gap = blank(7);
        gap.from = "2026-11-09".into();
        gap.to = "2026-11-15".into();
        (gap.days_elapsed, gap.first_recorded_day) = (5, 2);
        gap.by_day[3] = 8.;
        let canvas = list(&gap, &p, Source::Records, false);
        assert_eq!((canvas.cells[marks * canvas.w + 1].ch, canvas.cells[marks * canvas.w + 1].fg), ('·', p.faint));
        assert_eq!((canvas.cells[marks * canvas.w + 11].ch, canvas.cells[marks * canvas.w + 11].fg), ('▁', p.track));
        assert_eq!(canvas.cells[marks * canvas.w + 16].fg, p.bright);
        let heavy = list(&heavy_month(), &p, Source::Records, false);
        let mut marks = Vec::new();
        for y in 0..heavy.h {
            let cell = &heavy.cells[y * heavy.w];
            if cell.ch == '▌' {
                marks.push(cell.fg);
            }
        }
        assert_eq!(marks, vec![p.warn, p.hot]);
    }

    #[test]
    fn a_month_starting_on_sunday_gaps_before_monday() {
        let r = month(6, &[1.; 30], 0, 30);
        let rows = list_rows(&r, false);
        let labels = rows.iter().find(|row| row.contains('1') && row.contains('9')).expect("labels");
        assert!(labels.contains("1 2       9"), "{labels}");
        assert!(!labels.contains("12"), "{labels}");
    }

    #[test]
    fn a_february_of_28_days_keeps_one_cell_each() {
        let r = month(0, &[1.; 28], 0, 28);
        let rows = list_rows(&r, false);
        assert!(rows.iter().all(|row| terminal::width(row) <= LIST_COLUMNS));
        let bar = rows.iter().find(|row| row.contains('█')).expect("days");
        assert_eq!(bar.chars().filter(|ch| *ch != ' ').count(), 28, "{bar}");
        assert!(terminal::width(bar) <= 38, "{bar}");
    }

    #[test]
    fn a_small_day_is_at_least_two_eighths() {
        let mut r = blank(7);
        r.from = "2026-11-09".into();
        r.to = "2026-11-15".into();
        (r.days_elapsed, r.first_recorded_day) = (7, 0);
        (r.by_day[0], r.by_day[1]) = (1., 100.);
        let rows = list_rows(&r, false);
        let bar = rows.iter().find(|row| row.contains('█')).expect("days");
        assert!(bar.contains('▂'), "a day of 1h beside 100h must not look empty: {bar}");
        assert!(!bar.starts_with(" ▁"), "{bar}");
    }

    /// The first row that contains `needle`.
    fn row_with(rows: &[String], needle: &str) -> String {
        rows.iter().find(|row| row.contains(needle)).unwrap_or_else(|| panic!("no {needle:?} in {}", rows.join("\n"))).clone()
    }

    #[test]
    fn ledger_rows_give_way_in_the_order_the_list_allows() {
        let mut tasks = blank(7);
        (tasks.days_elapsed, tasks.from, tasks.to) = (7, "2026-11-09".into(), "2026-11-15".into());
        (tasks.tasks.total, tasks.tasks.finished, tasks.tasks.failed) = (20, 15, 2);
        assert_eq!(row_with(&list_rows(&tasks, false), "tasks"), " 20 tasks · 15 finished · 2 failed");
        (tasks.tasks.total, tasks.tasks.finished, tasks.tasks.failed) = (99_999, 98_000, 999);
        let dropped = row_with(&list_rows(&tasks, false), "tasks");
        assert_eq!(dropped, " 99,999 tasks · 999 failed");
        assert!(!dropped.contains("finished"));
        (tasks.tasks.total, tasks.tasks.finished, tasks.tasks.failed) = (1, 1, 0);
        assert_eq!(row_with(&list_rows(&tasks, false), "task"), " 1 task · all finished");

        let mut pulls = tasks.clone();
        (pulls.prs_merged, pulls.prs_open, pulls.prs_closed) = (8, 4, 1);
        assert_eq!(row_with(&list_rows(&pulls, false), "PR"), " 8 PRs merged · 4 open · 1 closed");
        (pulls.prs_merged, pulls.prs_open, pulls.prs_closed) = (999, 999, 999);
        let dropped = row_with(&list_rows(&pulls, false), "PR");
        assert_eq!(dropped, " 999 PRs merged · 999 open");
        assert!(!dropped.contains("closed"));
        (pulls.prs_merged, pulls.prs_open, pulls.prs_closed) = (1, 0, 0);
        assert_eq!(row_with(&list_rows(&pulls, false), "PR"), " 1 PR merged");
        (pulls.prs_merged, pulls.prs_open, pulls.prs_closed) = (0, 0, 0);
        assert_eq!(row_with(&list_rows(&pulls, false), "pull"), " no pull requests linked");

        let mut models = pulls.clone();
        models.models = vec![recap::Model { name: "a".into(), family: "claude", hours: 10., runs: 1, tokens_in: None }];
        assert_eq!(row_with(&list_rows(&models, false), "●"), " ● a 10h · one model");
        models.models = (0..5).map(|i| recap::Model { name: format!("{i}"), family: "claude", hours: 10., runs: 1, tokens_in: None }).collect();
        let named = row_with(&list_rows(&models, false), "●");
        assert_eq!(named.matches('●').count(), 3, "{named}");
        assert!(named.contains("2 more"), "{named}");
        models.models = (0..7).map(|i| recap::Model { name: format!("model-{i}"), family: "claude", hours: 2_000. - 100. * f64::from(i), runs: 1, tokens_in: None }).collect();
        let one = row_with(&list_rows(&models, false), "●");
        assert_eq!(one.matches('●').count(), 1, "{one}");
        assert!(one.contains("6 more"), "{one}");
        // A long name, four-digit hours and a four-digit fold: the fold gives up its hours.
        models.models = (0..13).map(|_| recap::Model { name: "deepseek/deepseek-v4.1-flash".into(), family: "other", hours: 1_234., runs: 1, tokens_in: None }).collect();
        assert_eq!(row_with(&list_rows(&models, false), "●"), " ● deepseek/deepse… 1234h  12 more");
        // And where even that is too long, the one model stands alone. No row is ever cut.
        models.models[0].hours = 1e9;
        let alone = row_with(&list_rows(&models, false), "●");
        assert_eq!(alone, " ● deepseek/deepse… 1000000000h");
        models.models.clear();
        assert!(list_rows(&models, false).iter().all(|row| !row.contains('●')));

        let mut lines = models.clone();
        (lines.lines_added, lines.lines_removed, lines.file_changes) = (412, 37, 1);
        assert_eq!(row_with(&list_rows(&lines, false), "lines"), " +412 -37 lines · 1 file change");
        (lines.lines_added, lines.lines_removed, lines.file_changes) = (99_999_999, 9_999_999, 99_999);
        let dropped = row_with(&list_rows(&lines, false), "lines");
        assert_eq!(dropped, " +100M -10.0M lines");
        assert!(!dropped.contains("file"));

        let mut hands = lines.clone();
        hands.handed = recap::Handed { count: 2, hours: 10., failed: 1, pairs: vec![] };
        assert_eq!(row_with(&list_rows(&hands, false), "handoff"), " 2 handoffs · 10h · 1 did not finish");
        hands.handed = recap::Handed { count: 1, hours: 10., failed: 0, pairs: vec![] };
        assert_eq!(row_with(&list_rows(&hands, false), "handoff"), " 1 handoff · 10h · all finished");
        hands.handed = recap::Handed { count: 99_999, hours: 9_999., failed: 999, pairs: vec![] };
        let dropped = row_with(&list_rows(&hands, false), "handoff");
        assert_eq!(dropped, " 99,999 handoffs · 9999h");
        assert!(!dropped.contains("finish"));
        hands.handed.count = 0;
        assert_eq!(row_with(&list_rows(&hands, false), "handed"), " nothing handed to agents");

        let mut tokens = hands.clone();
        tokens.hours = 128.;
        tokens.tokens = recap::Tokens { input: 3.1e9, output: 11e6, cached: 0., left_out: vec![], reported: true };
        assert_eq!(row_with(&list_rows(&tokens, false), "tokens"), " 3.1B tokens in · 11M out");
        tokens.tokens.left_out = vec![("grok".into(), 10.)];
        let named = row_with(&list_rows(&tokens, false), "tokens");
        assert_eq!(named, " 3.1B tokens in · ● grok 10h left out");
        assert!(!named.contains("11M"));
        tokens.hours = 12_345.;
        tokens.tokens.left_out = vec![("grok".into(), 12_345.)];
        assert_eq!(row_with(&list_rows(&tokens, false), "tokens"), " 3.1B tokens in · covers 0m of 12345h");
        tokens.hours = 1_234_567.;
        tokens.tokens.left_out = vec![("grok".into(), 123_456.7)];
        let percent = row_with(&list_rows(&tokens, false), "tokens");
        assert_eq!(percent, " 3.1B tokens in · covers 90% of hours");
        // A sliver left out is still left out: 99%, never 100%.
        tokens.tokens.left_out = vec![("grok".into(), 1.), ("claude".into(), 1.)];
        assert_eq!(row_with(&list_rows(&tokens, false), "tokens"), " 3.1B tokens in · covers 99% of hours");
        tokens.hours = 128.;
        tokens.tokens.left_out = vec![("grok".into(), 6.), ("claude".into(), 4.)];
        let covers = row_with(&list_rows(&tokens, false), "tokens");
        assert_eq!(covers, " 3.1B tokens in · covers 118h of 128h");
        assert!(!covers.contains("grok"));
        tokens.tokens.reported = false;
        assert_eq!(row_with(&list_rows(&tokens, false), "tokens"), " no tokens reported");
    }

    #[test]
    fn the_256_color_choice_is_the_nearest_of_the_cube_and_the_gray_ramp() {
        let level = [0, 95, 135, 175, 215, 255];
        let color = |i: i32| -> (i32, i32, i32) {
            if i < 232 {
                let n = (i - 16) as usize;
                (level[n / 36], level[n / 6 % 6], level[n % 6])
            } else {
                let gray = 8 + (i - 232) * 10;
                (gray, gray, gray)
            }
        };
        let distance = |(r, g, b): (i32, i32, i32), (x, y, z): (i32, i32, i32)| (r - x).pow(2) + (g - y).pow(2) + (b - z).pow(2);
        // Every third value of each channel, and the corners: about 600,000 colors.
        let values: Vec<i32> = (0..256).step_by(3).chain([254, 255]).collect();
        for &r in &values {
            for &g in &values {
                for &b in &values {
                    let chosen = distance((r, g, b), color(i32::from(indexed((r << 16 | g << 8 | b) as Rgb))));
                    let nearest = (16..256).map(|i| distance((r, g, b), color(i))).min().unwrap();
                    assert_eq!(chosen, nearest, "#{r:02x}{g:02x}{b:02x}");
                }
            }
        }
        // A gray between two of the ramp's takes the nearer one, not the darker: Frontier's dim.
        assert_eq!(indexed(0x7d7d7d), 244);
        // Paper's purple for Claude has no near match and goes gray, as the dashboard's rule has it.
        assert_eq!(indexed(0x5f3369), 240);
    }

    #[test]
    fn a_quiet_line_names_only_what_is_true_and_fits() {
        let rows = list_rows(&thin_week(), false);
        assert_eq!(row_with(&rows, "findings"), " No findings this week · no failures");
        assert!(rows.iter().all(|row| !row.contains("no plan limits") && !row.contains("no long waits")));
        let mut noisy = thin_week();
        (noisy.tasks.failed, noisy.tasks.failed_limit, noisy.asks.hours) = (4, 1, 2.);
        assert_eq!(row_with(&list_rows(&noisy, false), "findings"), " No findings this week");
        let mut month = heavy_month();
        month.findings.clear();
        month.tokens.reported = false;
        let rows = list_rows(&month, false);
        assert_eq!(row_with(&rows, "tokens"), " no tokens reported");
        assert_eq!(row_with(&rows, "findings"), " No findings this month");
    }
}
