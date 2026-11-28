//! The Insights screen: one cached body, scrolled and clipped into the Live frame.
//!
//! The view does not read, and it does not keep a copy of a [`Recap`] or [`Tools`]. The term
//! layer borrows the report it wants on screen and passes a `revision` that changes when that
//! report changes. A Live wake redraws the header row only.
//!
//! The view labels the report it was given. Dates, coverage
//! and the canvas come from that report, so a requested period cannot paint its dates over a
//! different one. After [`InsightsView::paint`], adopt [`InsightsView::displayed_period`] before
//! the next arrow, or the following step starts from the period that was asked for rather than
//! the one on screen.
//!
//! [`InsightsView::close`] drops the canvas, the scroll and the anchors. The caller drops its
//! snapshot too: the next open is [`Body::Loading`] with no previous report.

use std::hash::{Hash, Hasher};

use crate::clock;
use crate::model::State;
use crate::overview::{self, HeaderBadge};
use crate::recap::{self, Period, Recap, Span, Which};
use crate::recap_view::{self, Source};
use crate::terminal::{self, Canvas, Palette, Rgb};
use crate::tools::Tools;
use crate::tools_view;

pub const MIN_COLUMNS: usize = 52;
pub const MIN_ROWS: usize = 14;

const PERIODS: [Which; 4] = [
    Which::ThisWeek,
    Which::LastWeek,
    Which::ThisMonth,
    Which::LastMonth,
];
const NOTE_TIERS: [&str; 3] = [
    "made-up data, not the agents on Live",
    "made up · not Live's agents",
    "not Live's agents",
];
const LOAD_TIPS: [&str; 2] = [
    "Nothing is shown until the period is counted.",
    "i returns to Live. A read may finish in the background.",
];
const INSIGHTS_KEYS: &[(&str, &str)] = &[
    ("?", "help"),
    ("i", "back"),
    ("tab", "report"),
    ("←→", "period"),
    ("r", "refresh"),
    ("↑↓", "scroll"),
    ("q", "quit"),
];
const HELP_KEYS: &[(&str, &str)] = &[("?", "close help"), ("i", "back"), ("q", "quit")];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Recap,
    Tools,
}

/// The report on screen. The view borrows it for the paint and does not store it.
#[derive(Clone, Copy)]
pub enum Report<'a> {
    Recap(&'a Recap),
    Tools(&'a Tools),
}

/// What the body is this paint. A finished report replaces `previous`; loading or unreadable
/// keeps `previous` up and changes the footer only.
pub enum Body<'a> {
    Loading,
    Report(Report<'a>),
    Unreadable {
        message: &'a str,
        tips: &'a [String],
    },
}

/// The last good report, kept while a refresh or a failed read is in progress.
pub struct Kept<'a> {
    pub report: Report<'a>,
    pub read_at: f64,
    pub revision: u64,
}

pub struct Input<'a> {
    pub state: &'a State,
    pub palette: &'a Palette,
    pub local: recap::Local<'a>,
    pub tab: Tab,
    pub requested: Which,
    pub source: Source,
    pub body: Body<'a>,
    pub previous: Option<Kept<'a>>,
    pub started: Option<f64>,
    pub read_at: Option<f64>,
    pub help: bool,
    pub explicit_db: bool,
    pub revision: u64,
    pub cols: usize,
    pub rows: usize,
}

/// Scroll requested since the last paint. Row steps add up; a page or an end replaces them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Rows(isize),
    Page(isize),
    Start,
    End,
}

/// The selector value a report's own span and `so_far` flag name.
pub fn which_of(span: Span, so_far: bool) -> Which {
    match (span, so_far) {
        (Span::Month, true) => Which::ThisMonth,
        (Span::Month, false) => Which::LastMonth,
        (Span::Week, true) => Which::ThisWeek,
        (Span::Week, false) => Which::LastWeek,
    }
}

/// One step along ThisWeek, LastWeek, ThisMonth, LastMonth. The ends do not wrap.
pub fn step_period(which: Which, delta: i32) -> Which {
    let index = PERIODS.iter().position(|item| *item == which).unwrap_or(0) as i32;
    PERIODS[(index + delta).clamp(0, PERIODS.len() as i32 - 1) as usize]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Section {
    HowLong,
    ByKind,
    Programs,
    WhyFailed,
    Tools,
    Providers,
    Longest,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Section(Section),
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    Wide,
    Card,
    List,
    Notice,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    None,
    Rows(isize),
    Page(isize),
    Start,
    End,
}

#[derive(Clone, Copy, PartialEq)]
struct BodyKey {
    kind: u8,
    tab: u8,
    which: u8,
    shape: u8,
    width: u16,
    revision: u64,
    demo: bool,
    live_demo: bool,
    explicit_db: bool,
    extra: u64,
    palette: u64,
}

#[derive(Clone, PartialEq)]
struct ChromeKey {
    cols: usize,
    rows: usize,
    help: bool,
    scroll: usize,
    which: Which,
    tab: Tab,
    calm: bool,
    paused: bool,
    error: bool,
    snap_at: u64,
    interval: u64,
    demo: bool,
    window: usize,
    scope: String,
    palette: u64,
    body: BodyKey,
    status: String,
    note: String,
}

struct Place {
    at_end: bool,
    scroll: usize,
    anchor: Option<Section>,
    period: Option<Which>,
    shape: Option<Shape>,
}

impl Default for Place {
    fn default() -> Self {
        Self {
            at_end: false,
            scroll: 0,
            anchor: None,
            period: None,
            shape: None,
        }
    }
}

enum Phase<'a> {
    Loading,
    Refreshing {
        report: Report<'a>,
        revision: u64,
    },
    Ready(Report<'a>),
    Empty(Report<'a>),
    Failed {
        report: Report<'a>,
        read_at: f64,
        revision: u64,
    },
    Unreadable {
        message: &'a str,
        tips: &'a [String],
    },
}

pub struct InsightsView {
    frame: Canvas,
    body: Canvas,
    body_key: Option<BodyKey>,
    chrome_key: Option<ChromeKey>,
    places: [Place; 2],
    pending: Pending,
    shown: Option<Which>,
    body_builds: u64,
}

impl Default for InsightsView {
    fn default() -> Self {
        Self::new()
    }
}

impl InsightsView {
    pub fn new() -> Self {
        Self {
            frame: Canvas::new(1, 1),
            body: Canvas::new(1, 1),
            body_key: None,
            chrome_key: None,
            places: [Place::default(), Place::default()],
            pending: Pending::None,
            shown: None,
            body_builds: 0,
        }
    }

    /// Drop the cached canvas, the scroll and the anchors. The next open paints loading.
    pub fn close(&mut self) {
        *self = Self::new();
    }

    pub fn scroll(&mut self, motion: Motion) {
        self.pending = match (self.pending, motion) {
            (Pending::Rows(have), Motion::Rows(more)) => Pending::Rows(have.saturating_add(more)),
            (_, Motion::Rows(more)) => Pending::Rows(more),
            (_, Motion::Page(pages)) => Pending::Page(pages),
            (_, Motion::Start) => Pending::Start,
            (_, Motion::End) => Pending::End,
        };
    }

    /// The period whose data and dates are on screen, or ThisWeek before the first paint
    /// above the size floor.
    pub fn displayed_period(&self) -> Which {
        self.shown.unwrap_or(Which::ThisWeek)
    }

    /// How many times the report body was rendered. A Live wake does not increment it.
    pub fn body_builds(&self) -> u64 {
        self.body_builds
    }

    pub fn paint<'a>(&'a mut self, input: &Input<'_>) -> &'a Canvas {
        self.render(input);
        &self.frame
    }

    /// What a click at `(x, y)` on the last frame asks for: a report by its tab name in the
    /// header, or a period on the line below it (a name when all four are shown, else an arrow).
    pub fn click(&self, x: i32, y: i32) -> Option<Click> {
        let frame = &self.frame;
        let at = |label: &str, row: i32| terminal::find_in_row(frame, row, label).is_some_and(|(from, to)| (from..to).contains(&x));
        match y {
            0 => [(" RECAP ", Tab::Recap), (" TOOLS ", Tab::Tools)]
                .into_iter()
                .find(|(label, _)| at(label, 0))
                .map(|(_, tab)| Click::Tab(tab)),
            1 if frame.w >= 120 => PERIODS
                .into_iter()
                .find(|period| at(&format!(" {} ", period_word(*period)), 1))
                .map(Click::Period),
            1 => {
                let arrow = |col: i32, ch: char| (col - 1..=col + 1).contains(&x) && frame.get(col, 1).is_some_and(|cell| cell.ch == ch);
                let right = compact_selector_width() as i32;
                if arrow(1, '◂') {
                    Some(Click::Step(-1))
                } else if arrow(right, '▸') {
                    Some(Click::Step(1))
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

/// A click on Insights' own controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Click {
    Tab(Tab),
    Period(Which),
    /// The previous (-1) or next (1) period.
    Step(i32),
}

impl InsightsView {
    fn render(&mut self, input: &Input<'_>) {
        if input.cols < MIN_COLUMNS || input.rows < MIN_ROWS {
            self.pending = Pending::None;
            self.compose_floor(input);
            return;
        }
        let phase = phase_of(input);
        let which = phase_which(&phase, input.requested);
        let shape = phase_shape(&phase, input.cols);
        let index = tab_index(input.tab);
        let period_changed = self.places[index].period != Some(which);
        if period_changed {
            self.places[index].at_end = false;
            self.places[index].scroll = 0;
            self.places[index].anchor = None;
        }
        let key = body_key(input, &phase, shape, which);
        let rebuilt = self.body_key != Some(key);
        if rebuilt {
            self.body = build_body(input, &phase, shape);
            self.body_key = Some(key);
            self.body_builds += 1;
        }
        let shape_changed = self.places[index].shape != Some(shape);
        if shape_changed {
            let anchor = self.places[index].anchor;
            self.places[index].scroll = anchor
                .and_then(|section| section_row(&self.body, section))
                .unwrap_or(0);
        }
        let before = self.places[index].scroll;
        self.apply_motion(index, input.rows);
        if rebuilt || shape_changed || period_changed || self.places[index].scroll != before {
            let scroll = self.places[index].scroll;
            self.places[index].anchor = anchor_at(&self.body, scroll);
        }
        self.places[index].period = Some(which);
        self.places[index].shape = Some(shape);
        self.shown = Some(which);

        let (dates, coverage) = dates_of(input, &phase, shape);
        let note = if which != input.requested {
            format!("{} empty · {dates}", period_word(input.requested))
        } else {
            pick_note(input.cols, input.state.demo, &dates, coverage.as_deref())
        };
        let status = footer_status(input, &phase);
        let chrome = ChromeKey {
            cols: input.cols,
            rows: input.rows,
            help: input.help,
            scroll: self.places[index].scroll,
            which,
            tab: input.tab,
            calm: input.state.calm,
            paused: input.state.paused,
            error: input.state.error.is_some(),
            snap_at: input.state.snap.at.to_bits(),
            interval: input.state.interval.to_bits(),
            demo: input.state.demo,
            window: input.state.window_index,
            scope: input.state.scope.clone(),
            palette: palette_tag(input.palette),
            body: key,
            status: status.clone(),
            note: note.clone(),
        };
        if self.chrome_key.as_ref() == Some(&chrome)
            && self.frame.w == input.cols
            && self.frame.h == input.rows
        {
            draw_header(&mut self.frame, input);
            return;
        }
        self.compose(input, which, &note, &status);
        self.chrome_key = Some(chrome);
    }

    fn apply_motion(&mut self, index: usize, rows: usize) {
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        let total = self.body.h;
        let view = rows.saturating_sub(3);
        let scrolls = total > view;
        let content = if scrolls {
            view.saturating_sub(1)
        } else {
            view
        };
        let max = total.saturating_sub(content);
        let mut scroll = if self.places[index].at_end { max } else { self.places[index].scroll.min(max) };
        match pending {
            Pending::None => {}
            Pending::Rows(delta) => scroll = add_scroll(scroll, delta),
            Pending::Page(pages) => {
                scroll = add_scroll(scroll, pages.saturating_mul(content as isize))
            }
            Pending::Start => scroll = 0,
            Pending::End => scroll = max,
        }
        self.places[index].scroll = scroll.min(max);
        if pending != Pending::None {
            self.places[index].at_end = matches!(pending, Pending::End)
                || (max > 0 && self.places[index].scroll == max);
        }
    }

    fn compose_floor(&mut self, input: &Input<'_>) {
        let cols = input.cols.max(1);
        let rows = input.rows.max(1);
        if self.frame.w != cols || self.frame.h != rows {
            self.frame = Canvas::new(cols, rows);
        }
        let p = input.palette;
        self.frame.clear(p.bg, p.fg);
        self.frame.text_bold(1, 0, "Loupe · Insights", p.bright);
        let notice = if cols < 17 {
            "Enlarge terminal".to_owned()
        } else if cols < 42 {
            format!("Minimum: {MIN_COLUMNS} × {MIN_ROWS}")
        } else {
            format!("Enlarge the terminal to at least {MIN_COLUMNS} × {MIN_ROWS}.")
        };
        self.frame.text(1, 2, &notice, p.dim);
        self.frame.text(1, 3, "i back   q quit", p.accent);
        self.chrome_key = None;
    }

    fn compose(&mut self, input: &Input<'_>, which: Which, note: &str, status: &str) {
        let cols = input.cols.max(1);
        let rows = input.rows.max(1);
        if self.frame.w != cols || self.frame.h != rows {
            self.frame = Canvas::new(cols, rows);
        }
        let p = input.palette;
        self.frame.clear(p.bg, p.fg);
        draw_header(&mut self.frame, input);
        draw_period(&mut self.frame, which, input.requested, note, p);
        let view = rows.saturating_sub(3);
        if input.help {
            self.frame
                .fill_with(0, 2, cols as i32, view as i32, p.bg, ' ', p.fg);
            draw_help(&mut self.frame, p, input.state.demo);
        } else {
            blit_body(
                &mut self.frame,
                &self.body,
                self.places[tab_index(input.tab)].scroll,
                view,
                p,
            );
        }
        draw_footer(
            &mut self.frame,
            if input.help { HELP_KEYS } else { INSIGHTS_KEYS },
            status,
            p,
        );
    }
}

fn tab_index(tab: Tab) -> usize {
    match tab {
        Tab::Recap => 0,
        Tab::Tools => 1,
    }
}

fn add_scroll(scroll: usize, delta: isize) -> usize {
    if delta >= 0 {
        scroll.saturating_add(delta as usize)
    } else {
        scroll.saturating_sub(delta.unsigned_abs())
    }
}

fn phase_of<'a>(input: &'a Input<'a>) -> Phase<'a> {
    match &input.body {
        Body::Report(report) if report.is_empty() => Phase::Empty(*report),
        Body::Report(report) => Phase::Ready(*report),
        Body::Loading => match input.previous {
            Some(Kept {
                report, revision, ..
            }) => Phase::Refreshing { report, revision },
            None => Phase::Loading,
        },
        Body::Unreadable { message, tips } => match input.previous {
            Some(Kept {
                report,
                read_at,
                revision,
            }) => Phase::Failed {
                report,
                read_at,
                revision,
            },
            None => Phase::Unreadable { message, tips },
        },
    }
}

fn phase_which(phase: &Phase<'_>, requested: Which) -> Which {
    match phase {
        Phase::Loading | Phase::Unreadable { .. } => requested,
        Phase::Refreshing { report, .. }
        | Phase::Ready(report)
        | Phase::Empty(report)
        | Phase::Failed { report, .. } => report.which(),
    }
}

fn phase_shape(phase: &Phase<'_>, cols: usize) -> Shape {
    let ready = match phase {
        Phase::Ready(report) | Phase::Refreshing { report, .. } | Phase::Failed { report, .. } => {
            !report.is_empty()
        }
        Phase::Empty(_) | Phase::Loading | Phase::Unreadable { .. } => false,
    };
    if ready { form_for(cols) } else { Shape::Notice }
}

fn form_for(cols: usize) -> Shape {
    if cols >= 120 {
        Shape::Wide
    } else if cols >= 80 {
        Shape::Card
    } else {
        Shape::List
    }
}

fn body_key(input: &Input<'_>, phase: &Phase<'_>, shape: Shape, which: Which) -> BodyKey {
    let kind = match phase {
        Phase::Loading => 1,
        Phase::Unreadable { .. } => 2,
        Phase::Empty(_) => 3,
        Phase::Ready(report) | Phase::Refreshing { report, .. } | Phase::Failed { report, .. }
            if report.is_empty() =>
        {
            3
        }
        _ => 4,
    };
    let width = if kind == 1
        || ((kind == 2 || kind == 3) && input.cols < 80)
        || (kind == 4 && shape == Shape::List && input.tab == Tab::Tools)
    {
        input.cols as u16
    } else {
        0
    };
    let revision = match phase {
        Phase::Refreshing { revision, .. } | Phase::Failed { revision, .. } => *revision,
        Phase::Ready(_) | Phase::Empty(_) => input.revision,
        Phase::Loading | Phase::Unreadable { .. } => 0,
    };
    let extra = match phase {
        Phase::Loading => input.started.map(f64::to_bits).unwrap_or(0),
        Phase::Unreadable { message, tips } => {
            hash_text(std::iter::once(*message).chain(tips.iter().map(String::as_str)))
        }
        _ => 0,
    };
    BodyKey {
        kind,
        tab: tab_index(input.tab) as u8,
        which: which_tag(which),
        shape: shape_tag(shape),
        width,
        revision,
        demo: input.source == Source::Demo,
        live_demo: input.state.demo,
        explicit_db: input.explicit_db,
        extra,
        palette: palette_tag(input.palette),
    }
}

fn which_tag(which: Which) -> u8 {
    match which {
        Which::ThisWeek => 0,
        Which::LastWeek => 1,
        Which::ThisMonth => 2,
        Which::LastMonth => 3,
        Which::Last7Days => 4,
    }
}

fn shape_tag(shape: Shape) -> u8 {
    match shape {
        Shape::Wide => 0,
        Shape::Card => 1,
        Shape::List => 2,
        Shape::Notice => 3,
    }
}

fn palette_tag(p: &Palette) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    p.dark.hash(&mut hasher);
    for color in [
        p.bg, p.panel, p.fg, p.text2, p.dim, p.faint, p.track, p.rule, p.accent, p.bright, p.hot,
        p.warn, p.sel, p.busy,
    ] {
        color.hash(&mut hasher);
    }
    p.floor.to_bits().hash(&mut hasher);
    p.providers.hash(&mut hasher);
    hasher.finish()
}

fn hash_text<'a>(parts: impl Iterator<Item = &'a str>) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for part in parts {
        part.hash(&mut hasher);
    }
    hasher.finish()
}

fn build_body(input: &Input<'_>, phase: &Phase<'_>, shape: Shape) -> Canvas {
    match phase {
        Phase::Loading => loading_canvas(input),
        Phase::Unreadable { message, tips } => unreadable_canvas(input, message, tips),
        Phase::Ready(report)
        | Phase::Empty(report)
        | Phase::Refreshing { report, .. }
        | Phase::Failed { report, .. } => {
            if report.is_empty() {
                empty_canvas(input, *report)
            } else {
                report_canvas(input, *report, shape)
            }
        }
    }
}

fn report_canvas(input: &Input<'_>, report: Report<'_>, shape: Shape) -> Canvas {
    let p = input.palette;
    let source = input.source;
    match (report, shape) {
        (Report::Recap(report), Shape::Wide) => {
            crop(&recap_view::wide(report, p, source, false), 2)
        }
        (Report::Recap(report), Shape::Card) => {
            with_recap_total(crop(&recap_view::card(report, p, source, false), 2), report.insights_total(), p, input.cols)
        }
        (Report::Recap(report), Shape::List) => {
            with_recap_total(crop(&recap_view::list(report, p, source, false), 3), report.insights_total(), p, input.cols)
        }
        (Report::Tools(report), Shape::Wide) => {
            crop(&tools_view::wide(report, p, source, false), 2)
        }
        (Report::Tools(report), Shape::Card) => {
            crop(&tools_view::report(report, p, source, false), 2)
        }
        (Report::Tools(report), Shape::List) => tools_list(report, source, input.cols, p),
        (_, Shape::Notice) => empty_canvas(input, report),
    }
}

fn with_recap_total(body: Canvas, totals: Option<(Option<f64>, &str)>, p: &Palette, cols: usize) -> Canvas {
    let Some((total, source)) = totals else { return body };
    let value = total.map(terminal::tokens).unwrap_or_else(|| "—".into());
    let main = format!("TOTAL TOKENS   {value}");
    let label = format!("{main}   ·   {source}");
    let width = body.w.min(cols).saturating_sub(2);
    let mut canvas = Canvas::new(body.w, body.h + 1);
    canvas.fill(0, 0, body.w as i32, 1, p.panel);
    let shown = if terminal::width(&label) <= width { &label } else { &main };
    canvas.text_bold(1, 0, &terminal::fit(shown, width), p.fg);
    canvas.cells[body.w..].clone_from_slice(&body.cells);
    canvas
}

fn tools_list(tools: &Tools, source: Source, cols: usize, p: &Palette) -> Canvas {
    let text = tools_view::list(tools, source, cols.saturating_sub(1));
    let lines: Vec<&str> = text.lines().skip(2).collect();
    let mut canvas = Canvas::new(cols.max(1), lines.len().max(1));
    canvas.clear(p.bg, p.fg);
    for (y, line) in lines.iter().enumerate() {
        canvas.text(1, y as i32, line, p.fg);
    }
    canvas
}

fn empty_canvas(input: &Input<'_>, report: Report<'_>) -> Canvas {
    let (message, state, tips) = empty_copy(report);
    notice_canvas(
        input,
        report_tab(report),
        &report_dates(report),
        state,
        false,
        &message,
        &tips,
    )
}

fn unreadable_canvas(input: &Input<'_>, message: &str, tips: &[String]) -> Canvas {
    let dates = requested_dates(input);
    notice_canvas(
        input,
        input.tab,
        &dates,
        "can't read T3",
        true,
        message,
        tips,
    )
}

fn notice_canvas(
    input: &Input<'_>,
    tab: Tab,
    dates: &str,
    state: &str,
    alarm: bool,
    message: &str,
    tips: &[String],
) -> Canvas {
    let tips = screen_tips(tips, input.explicit_db);
    let p = input.palette;
    if input.cols >= 80 {
        let card = match tab {
            Tab::Recap => recap_view::notice(p, dates, state, alarm, message, &tips),
            Tab::Tools => tools_view::notice(p, dates, state, alarm, message, &tips),
        };
        crop(&card, 2)
    } else {
        narrow_notice(p, message, &tips, input.cols)
    }
}

fn loading_canvas(input: &Input<'_>) -> Canvas {
    let message = loading_line(input.state.demo, input.tab, input.requested, input.started);
    let tips = [LOAD_TIPS[0].to_owned(), LOAD_TIPS[1].to_owned()];
    let p = input.palette;
    if input.state.themes && input.tab == Tab::Recap {
        let body = paint_rows(p, input.cols, &[(format!("  {message}"), p.text2)]);
        return with_recap_total(body, Some((None, "Loading Recap")), p, input.cols);
    }
    let mut rows = vec![(String::new(), p.fg)];
    for line in terminal::wrap(&message, input.cols.saturating_sub(4)) {
        rows.push((format!("  {line}"), p.text2));
    }
    rows.push((String::new(), p.fg));
    for tip in &tips {
        for line in terminal::wrap(tip, input.cols.saturating_sub(4)) {
            rows.push((format!("  {line}"), p.dim));
        }
    }
    paint_rows(input.palette, input.cols, &rows)
}

fn narrow_notice(p: &Palette, message: &str, tips: &[String], cols: usize) -> Canvas {
    let mut rows = vec![(String::new(), p.fg)];
    for line in terminal::wrap(message, cols.saturating_sub(4)) {
        rows.push((format!("  {line}"), p.fg));
    }
    rows.push((String::new(), p.fg));
    for tip in tips {
        let wrapped = terminal::wrap(tip, cols.saturating_sub(6));
        for (n, line) in wrapped.iter().enumerate() {
            let prefix = if n == 0 { "  · " } else { "    " };
            rows.push((format!("{prefix}{line}"), p.dim));
        }
    }
    paint_rows(p, cols, &rows)
}

fn paint_rows(p: &Palette, cols: usize, rows: &[(String, Rgb)]) -> Canvas {
    let mut canvas = Canvas::new(cols.max(1), rows.len().max(1));
    canvas.clear(p.bg, p.fg);
    for (y, (line, color)) in rows.iter().enumerate() {
        canvas.text(0, y as i32, line, *color);
    }
    canvas
}

fn crop(src: &Canvas, drop: usize) -> Canvas {
    let start = drop.min(src.h);
    let height = (src.h - start).max(1);
    let mut out = Canvas::new(src.w.max(1), height);
    if start == src.h {
        return out;
    }
    let count = (src.h - start) * src.w;
    out.cells[..count].copy_from_slice(&src.cells[start * src.w..start * src.w + count]);
    out
}

fn screen_tips(tips: &[String], explicit_db: bool) -> Vec<String> {
    tips.iter()
        .filter(|tip| (explicit_db || !tip.contains("--db")) && !tip.starts_with("Just looking"))
        .cloned()
        .collect()
}

fn empty_copy(report: Report<'_>) -> (String, &'static str, Vec<String>) {
    let dates = report_dates(report);
    let month = report.span() == Span::Month;
    let within = if month { format!("in {dates}") } else { dates };
    let (noun, state, reads) = match report {
        Report::Recap(_) => ("agent runs", "no records", "Recap reads the runs"),
        Report::Tools(_) => ("tool calls", "no tool calls", "Tools reads the tool calls"),
    };
    let span = if month { "month" } else { "week" };
    let message = if report.which().so_far() {
        format!("No {noun} recorded {within} so far.")
    } else {
        format!("No {noun} recorded {within}, or this {span} so far.")
    };
    let mut tips = vec![format!("{reads} T3 Code has recorded on this machine.")];
    match report.which() {
        Which::ThisWeek => tips.push("→ try last week".into()),
        Which::ThisMonth => tips.push("→ try last month".into()),
        _ => {}
    }
    (message, state, tips)
}

fn loading_line(demo: bool, tab: Tab, which: Which, started: Option<f64>) -> String {
    let report = match tab {
        Tab::Recap => "Recap",
        Tab::Tools => "Tools",
    };
    let period = period_word(which);
    let when = match started {
        Some(stamp) => format!(", started {}.", clock::clock(stamp)),
        None => ".".to_owned(),
    };
    if demo {
        format!("Making up {report} for {period}{when}")
    } else {
        format!("Reading {report} for {period} from T3{when}")
    }
}

fn dates_of(input: &Input<'_>, phase: &Phase<'_>, shape: Shape) -> (String, Option<String>) {
    match phase {
        Phase::Loading => (requested_dates(input), None),
        Phase::Unreadable { .. } => (requested_dates(input), Some("can't read T3".into())),
        Phase::Ready(report)
        | Phase::Empty(report)
        | Phase::Refreshing { report, .. }
        | Phase::Failed { report, .. } => {
            let wide = shape == Shape::Wide && !report.is_empty();
            let dates = if wide {
                recap_view::period_with_year(report.span(), report.from(), report.to())
            } else {
                report_dates(*report)
            };
            let coverage = if report.is_empty() {
                Some(match report {
                    Report::Recap(_) => "no records".into(),
                    Report::Tools(_) => "no tool calls".into(),
                })
            } else {
                Some(match report {
                    Report::Recap(report) => recap_view::coverage(report, input.source, wide),
                    Report::Tools(report) => tools_view::coverage(report, input.source),
                })
            };
            (dates, coverage)
        }
    }
}

fn requested_dates(input: &Input<'_>) -> String {
    let period = Period::new(input.state.now, input.requested, input.local);
    recap_view::period_text(
        period.span,
        &recap::local_date(input.local, period.start()),
        &recap::local_date(input.local, period.last_day()),
    )
}

fn report_dates(report: Report<'_>) -> String {
    recap_view::period_text(report.span(), report.from(), report.to())
}

fn pick_note(cols: usize, demo: bool, dates: &str, coverage: Option<&str>) -> String {
    let mut candidates = Vec::new();
    if demo {
        for note in NOTE_TIERS {
            candidates.push(format!("{dates} · {note}"));
        }
        candidates.push(dates.to_owned());
    } else if let Some(coverage) = coverage.filter(|text| !text.is_empty()) {
        candidates.push(format!("{dates} · {coverage}"));
        candidates.push(dates.to_owned());
    } else {
        candidates.push(dates.to_owned());
    }
    let size = if cols >= 120 {
        full_selector_width()
    } else {
        compact_selector_width()
    };
    let room = cols.saturating_sub(1);
    candidates
        .into_iter()
        .find(|text| 1 + size + 2 + terminal::width(text) <= room)
        .unwrap_or_default()
}

fn footer_status(input: &Input<'_>, phase: &Phase<'_>) -> String {
    let width = input.cols;
    let demo = input.state.demo;
    let in_progress = matches!(phase, Phase::Loading | Phase::Refreshing { .. });
    if let Phase::Failed { read_at, .. } = phase {
        return format!("can't read T3 · showing {}", clock::clock(*read_at));
    }
    if demo && in_progress {
        return if width < 60 {
            "making up"
        } else {
            "making up the demo"
        }
        .into();
    }
    if demo {
        return if width < 60 {
            "made up"
        } else {
            "demo data · made up"
        }
        .into();
    }
    match phase {
        Phase::Loading | Phase::Refreshing { .. } => match input.started {
            Some(stamp) => format!("reading since {}", clock::clock(stamp)),
            None => "reading".into(),
        },
        Phase::Unreadable { .. } => "can't read T3".into(),
        Phase::Ready(_) | Phase::Empty(_) | Phase::Failed { .. } => match input.read_at {
            Some(stamp) => format!("read at {}", clock::clock(stamp)),
            None => "read at".into(),
        },
    }
}

fn draw_header(canvas: &mut Canvas, input: &Input<'_>) {
    let tabs = [
        ("RECAP", input.tab == Tab::Recap),
        ("TOOLS", input.tab == Tab::Tools),
    ];
    overview::draw_header_badge(
        canvas,
        input.state,
        input.palette,
        None,
        Some(HeaderBadge::Tabs(&tabs)),
    );
}

fn draw_period(canvas: &mut Canvas, which: Which, requested: Which, note: &str, p: &Palette) {
    let width = canvas.w as i32;
    if width >= 120 {
        let mut x = 1;
        for period in PERIODS {
            let word = format!(" {} ", period_word(period));
            if period == which {
                x += canvas.text_bg(x, 1, &word, p.fg, p.sel, true);
            } else {
                x += canvas.text(x, 1, &word, p.dim);
            }
            x += 1;
        }
    } else {
        let index = PERIODS
            .iter()
            .position(|period| *period == requested)
            .unwrap_or(0);
        let left = if index == 0 { p.faint } else { p.text2 };
        let right = if index + 1 == PERIODS.len() {
            p.faint
        } else {
            p.text2
        };
        canvas.text(1, 1, "◂", left);
        canvas.text_bold(3, 1, period_word(which), p.fg);
        canvas.text((1 + compact_selector_width() as i32) - 1, 1, "▸", right);
    }
    if !note.is_empty() {
        canvas.right(width - 1, 1, note, p.dim);
    }
}

fn full_selector_width() -> usize {
    PERIODS
        .iter()
        .map(|period| terminal::width(period_word(*period)) + 2)
        .sum::<usize>()
        + PERIODS.len()
        - 1
}

fn compact_selector_width() -> usize {
    2 + terminal::width("this month") + 2
}

fn blit_body(dest: &mut Canvas, src: &Canvas, scroll: usize, view: usize, p: &Palette) {
    let total = src.h;
    let scrolls = total > view;
    let content = if scrolls {
        view.saturating_sub(1)
    } else {
        view
    };
    let max = total.saturating_sub(content);
    let offset = scroll.min(max);
    for row in 0..content {
        copy_row(dest, src, offset + row, 2 + row);
    }
    if scrolls {
        let y = 2 + view as i32 - 1;
        dest.fill_with(0, y, dest.w as i32, 1, p.bg, ' ', p.fg);
        if offset > 0 {
            dest.text(1, y, &format!("▲ {offset} above"), p.text2);
        }
        let below = total - offset - content;
        if below > 0 {
            dest.right(dest.w as i32 - 1, y, &format!("▼ {below} more"), p.text2);
        }
    }
}

fn copy_row(dest: &mut Canvas, src: &Canvas, src_row: usize, dest_row: usize) {
    if src_row >= src.h || dest_row >= dest.h {
        return;
    }
    let count = src.w.min(dest.w);
    let from = src_row * src.w;
    let to = dest_row * dest.w;
    dest.cells[to..to + count].copy_from_slice(&src.cells[from..from + count]);
}

fn draw_help(canvas: &mut Canvas, p: &Palette, demo: bool) {
    let last = canvas.h as i32 - 2;
    let width = canvas.w;
    if last < 2 {
        return;
    }
    let x = 1 + canvas.text_bold(1, 2, "INSIGHTS HELP", p.text2) + 1;
    let rules = (width as i32 - 1 - x).max(0) as usize;
    canvas.text(x, 2, &"─".repeat(rules), p.rule);
    let mut y = 2;
    let lines = crate::keymap::lines(crate::keymap::Screen::Insights, false);
    y = crate::keymap::draw(canvas, y + 1, last + 1, &lines, p, p.text2) - 1;
    if y >= last {
        return;
    }
    let mut notes = vec![
        "Opens on Recap for this week, read again each time.",
        "Scrolling and resizing redraw what was read.",
    ];
    if demo {
        notes.push("Demo: made-up weeks and months, not Live's agents.");
    }
    notes.push("Reads T3's records on this machine; sends nothing.");
    for note in notes {
        y += 1;
        if y > last {
            return;
        }
        canvas.text(2, y, &terminal::fit(note, width.saturating_sub(3)), p.dim);
    }
}

fn draw_footer(canvas: &mut Canvas, keys: &[(&str, &str)], status: &str, p: &Palette) {
    let y = canvas.h as i32 - 1;
    let width = canvas.w as i32;
    canvas.fill_with(0, y, width, 1, p.panel, ' ', p.fg);
    let mut x = 1;
    for (key, label) in keys {
        if x + terminal::width(key) as i32 + terminal::width(label) as i32 + 3 > width - 24 {
            break;
        }
        x += canvas.text_bold(x, y, key, p.accent) + 1;
        x += canvas.text(x, y, label, p.dim) + 2;
    }
    if !status.is_empty() {
        canvas.right(width - 1, y, status, p.dim);
    }
}

fn period_word(which: Which) -> &'static str {
    match which {
        Which::ThisWeek => "this week",
        Which::LastWeek => "last week",
        Which::ThisMonth => "this month",
        Which::LastMonth => "last month",
        Which::Last7Days => "last 7 days",
    }
}

fn report_tab(report: Report<'_>) -> Tab {
    match report {
        Report::Recap(_) => Tab::Recap,
        Report::Tools(_) => Tab::Tools,
    }
}

impl<'a> Report<'a> {
    fn which(self) -> Which {
        which_of(self.span(), self.so_far())
    }
    fn span(self) -> Span {
        match self {
            Report::Recap(report) => report.period,
            Report::Tools(report) => report.period,
        }
    }
    fn so_far(self) -> bool {
        match self {
            Report::Recap(report) => report.so_far,
            Report::Tools(report) => report.so_far,
        }
    }
    fn from(self) -> &'a str {
        match self {
            Report::Recap(report) => report.from.as_str(),
            Report::Tools(report) => report.from.as_str(),
        }
    }
    fn to(self) -> &'a str {
        match self {
            Report::Recap(report) => report.to.as_str(),
            Report::Tools(report) => report.to.as_str(),
        }
    }
    fn is_empty(self) -> bool {
        match self {
            Report::Recap(report) => report.is_empty()
                && !report.has_usage_history()
                && report.insights_usage != Some(recap::InsightsUsage::Pending),
            Report::Tools(report) => report.is_empty(),
        }
    }
}

fn line_text(canvas: &Canvas, y: usize) -> String {
    canvas.cells[y * canvas.w..(y + 1) * canvas.w]
        .iter()
        .filter(|cell| cell.ch != '\0')
        .map(|cell| cell.ch)
        .collect()
}

fn mark(line: &str) -> Option<Mark> {
    if let Some(index) = line.find('─') {
        let name = line[..index].trim();
        if name.len() >= 2
            && name.chars().all(|ch| ch.is_ascii_uppercase() || ch == ' ')
            && name.chars().any(|ch| ch.is_ascii_uppercase())
        {
            return Some(match name {
                "HOW LONG THEY TOOK" => Mark::Section(Section::HowLong),
                "BY KIND" => Mark::Section(Section::ByKind),
                "COMMANDS BY PROGRAM" => Mark::Section(Section::Programs),
                "WHY COMMANDS FAILED" => Mark::Section(Section::WhyFailed),
                "TOOLS BY NAME" => Mark::Section(Section::Tools),
                "PROVIDERS" => Mark::Section(Section::Providers),
                "LONGEST CALLS" => Mark::Section(Section::Longest),
                _ => Mark::Other,
            });
        }
    }
    let rest = line.strip_prefix(' ').unwrap_or(line);
    let Some(start) = rest.chars().next() else {
        return None;
    };
    if !start.is_ascii_lowercase() {
        return None;
    }
    let Some(index) = rest.find("  ") else {
        return None;
    };
    let label = &rest[..index];
    if label.len() < 2
        || !label.chars().all(|ch| ch.is_ascii_lowercase() || ch == ' ')
        || !label.ends_with(|ch: char| ch.is_ascii_lowercase())
    {
        return None;
    }
    Some(match label {
        "how long" => Mark::Section(Section::HowLong),
        "by kind" => Mark::Section(Section::ByKind),
        "programs" => Mark::Section(Section::Programs),
        "why failed" => Mark::Section(Section::WhyFailed),
        "tools" => Mark::Section(Section::Tools),
        "providers" => Mark::Section(Section::Providers),
        "longest" => Mark::Section(Section::Longest),
        _ => Mark::Other,
    })
}

fn section_row(body: &Canvas, section: Section) -> Option<usize> {
    (0..body.h).find(|&y| mark(&line_text(body, y)) == Some(Mark::Section(section)))
}

fn anchor_at(body: &Canvas, offset: usize) -> Option<Section> {
    if body.h == 0 {
        return None;
    }
    let last = offset.min(body.h - 1);
    if last + 1 < body.h && line_text(body, last).trim().is_empty() {
        if let Some(Mark::Section(section)) = mark(&line_text(body, last + 1)) {
            return Some(section);
        }
    }
    match (0..=last).rev().find_map(|y| mark(&line_text(body, y))) {
        Some(Mark::Section(section)) => Some(section),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Snapshot;
    use crate::platform::LocalTime;
    use std::sync::OnceLock;

    /// 2026-11-18 10:20:00 in Los Angeles, the instant the design frames use.
    const NOW: f64 = 1_795_026_000_000.0;
    const UNREADABLE: &str =
        "Can't read the synthetic database: not a SQLite database.";

    /// Los Angeles without touching the process zone. Cargo runs this crate's tests in
    /// parallel, and other tests call the process clock.
    fn la(seconds: i64) -> Option<LocalTime> {
        let shifted = seconds - if seconds < 1_793_523_600 { 7 } else { 8 } * 3600;
        let text = recap::iso(shifted as f64 * 1000.0);
        let n = |range: std::ops::Range<usize>| text[range].parse::<i32>().unwrap_or(0);
        Some(LocalTime {
            year: n(0..4),
            month0: n(5..7) - 1,
            day: n(8..10),
            hour: n(11..13),
            minute: n(14..16),
            second: n(17..19),
        })
    }

    fn palette(name: &str) -> &'static Palette {
        static FRONTIER: OnceLock<Palette> = OnceLock::new();
        static PAPER: OnceLock<Palette> = OnceLock::new();
        let cell = if name == "paper" { &PAPER } else { &FRONTIER };
        cell.get_or_init(|| {
            crate::integration::derive_themed(crate::theme::builtin(name).expect(name).text)
        })
    }

    fn demo_recap(which: Which) -> &'static Recap {
        static LAST_7_DAYS: OnceLock<Recap> = OnceLock::new();
        static THIS_WEEK: OnceLock<Recap> = OnceLock::new();
        static LAST_WEEK: OnceLock<Recap> = OnceLock::new();
        static THIS_MONTH: OnceLock<Recap> = OnceLock::new();
        static LAST_MONTH: OnceLock<Recap> = OnceLock::new();
        let cell = match which {
            Which::Last7Days => &LAST_7_DAYS,
            Which::ThisWeek => &THIS_WEEK,
            Which::LastWeek => &LAST_WEEK,
            Which::ThisMonth => &THIS_MONTH,
            Which::LastMonth => &LAST_MONTH,
        };
        cell.get_or_init(|| {
            let period = Period::new(NOW, which, &la);
            recap::demo(&period, &la).unwrap()
        })
    }

    fn demo_tools(which: Which) -> &'static Tools {
        static LAST_7_DAYS: OnceLock<Tools> = OnceLock::new();
        static THIS_WEEK: OnceLock<Tools> = OnceLock::new();
        static LAST_WEEK: OnceLock<Tools> = OnceLock::new();
        static THIS_MONTH: OnceLock<Tools> = OnceLock::new();
        static LAST_MONTH: OnceLock<Tools> = OnceLock::new();
        let cell = match which {
            Which::Last7Days => &LAST_7_DAYS,
            Which::ThisWeek => &THIS_WEEK,
            Which::LastWeek => &LAST_WEEK,
            Which::ThisMonth => &THIS_MONTH,
            Which::LastMonth => &LAST_MONTH,
        };
        cell.get_or_init(|| {
            let period = Period::new(NOW, which, &la);
            crate::tools::demo(&period, &la).unwrap()
        })
    }

    fn state_of(demo: bool, calm: bool, snap_at: f64) -> State {
        let mut snap = Snapshot::default();
        snap.at = snap_at;
        let mut state = State::new(snap);
        state.demo = demo;
        state.calm = calm;
        state.now = NOW;
        state.interval = 2.0;
        state.window_index = 1;
        state.scope = "all".into();
        state.error = None;
        state.paused = false;
        state.home.clear();
        state
    }

    fn frame_json(name: &str) -> serde_json::Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../checks/fixtures/insights-screen")
            .join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing fixture {}: {err}", path.display()));
        serde_json::from_str(&text)
            .unwrap_or_else(|err| panic!("bad fixture {}: {err}", path.display()))
    }

    fn expect_rows(value: &serde_json::Value) -> Vec<String> {
        value["text"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row.as_str().unwrap().to_owned())
            .collect()
    }

    fn frame_rows(canvas: &Canvas) -> Vec<String> {
        (0..canvas.h)
            .map(|y| {
                canvas.cells[y * canvas.w..(y + 1) * canvas.w]
                    .iter()
                    .filter(|cell| cell.ch != '\0')
                    .map(|cell| cell.ch)
                    .collect()
            })
            .collect()
    }

    fn mask_clocks(rows: &[String]) -> Vec<String> {
        rows.iter().map(|row| mask_clock(row)).collect()
    }

    fn mask_clock(row: &str) -> String {
        let mut chars: Vec<char> = row.chars().collect();
        let mut index = 0;
        while index + 8 <= chars.len() {
            if is_clock(&chars[index..index + 8]) {
                for (offset, ch) in "10:20:00".chars().enumerate() {
                    chars[index + offset] = ch;
                }
                index += 8;
            } else {
                index += 1;
            }
        }
        chars.into_iter().collect()
    }

    fn is_clock(chars: &[char]) -> bool {
        chars.len() == 8
            && chars[2] == ':'
            && chars[5] == ':'
            && chars
                .iter()
                .enumerate()
                .all(|(i, ch)| i == 2 || i == 5 || ch.is_ascii_digit())
    }

    fn diff(name: &str, got: &[String], expect: &[String]) -> Option<String> {
        if got == expect {
            return None;
        }
        let mut message = format!("{name}: {} rows vs {}", got.len(), expect.len());
        let mut shown = 0;
        for (index, pair) in got.iter().zip(expect.iter()).enumerate() {
            if pair.0 != pair.1 {
                message.push_str(&format!(
                    "\nrow {index}\n got {:?}\n exp {:?}",
                    pair.0, pair.1
                ));
                shown += 1;
                if shown == 4 {
                    break;
                }
            }
        }
        Some(message)
    }

    /// The period row names the period on screen and its dates, and does not also carry
    /// another period's dates. A row that names the right period and then paints the other
    /// period's dates fails.
    fn period_row_is_actual(
        row: &str,
        period: &str,
        actual_dates: &str,
        foreign_dates: &str,
    ) -> bool {
        !period.is_empty()
            && !actual_dates.is_empty()
            && !foreign_dates.is_empty()
            && row.contains(period)
            && row.contains(actual_dates)
            && !row.contains(foreign_dates)
    }

    struct Sample {
        name: &'static str,
        tab: Tab,
        which: Which,
        cols: usize,
        rows: usize,
        theme: &'static str,
        calm: bool,
        help: bool,
        scroll: Option<usize>,
        end: bool,
    }

    fn paint_sample(sample: &Sample) -> InsightsView {
        let report = match sample.tab {
            Tab::Recap => Report::Recap(demo_recap(sample.which)),
            Tab::Tools => Report::Tools(demo_tools(sample.which)),
        };
        let snap = if sample.calm { NOW } else { NOW - 1000.0 };
        let state = state_of(true, sample.calm, snap);
        let palette = palette(sample.theme);
        let mut view = InsightsView::new();
        let input = Input {
            state: &state,
            palette,
            local: &la,
            tab: sample.tab,
            requested: sample.which,
            source: Source::Demo,
            body: Body::Report(report),
            previous: None,
            started: None,
            read_at: Some(NOW),
            help: sample.help,
            explicit_db: false,
            revision: 7,
            cols: sample.cols,
            rows: sample.rows,
        };
        view.paint(&input);
        if let Some(scroll) = sample.scroll {
            view.places[tab_index(sample.tab)].scroll = scroll;
        }
        if sample.end {
            view.scroll(Motion::End);
        }
        if sample.scroll.is_some() || sample.end {
            view.paint(&input);
        }
        view
    }

    fn samples() -> Vec<Sample> {
        vec![
            Sample {
                name: "demo-recap-this-week-55x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 55,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-this-week-80x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-this-week-120x40",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 120,
                rows: 40,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-last-week-80x24",
                tab: Tab::Recap,
                which: Which::LastWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-this-month-80x24",
                tab: Tab::Recap,
                which: Which::ThisMonth,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-this-month-120x40",
                tab: Tab::Recap,
                which: Which::ThisMonth,
                cols: 120,
                rows: 40,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-last-month-80x24",
                tab: Tab::Recap,
                which: Which::LastMonth,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-recap-last-month-55x24",
                tab: Tab::Recap,
                which: Which::LastMonth,
                cols: 55,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-tools-this-week-55x24",
                tab: Tab::Tools,
                which: Which::ThisWeek,
                cols: 55,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-tools-this-week-80x24",
                tab: Tab::Tools,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-tools-this-week-120x40",
                tab: Tab::Tools,
                which: Which::ThisWeek,
                cols: 120,
                rows: 40,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-tools-last-week-80x24-programs",
                tab: Tab::Tools,
                which: Which::LastWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: Some(20),
                end: false,
            },
            Sample {
                name: "demo-tools-last-week-80x24-end",
                tab: Tab::Tools,
                which: Which::LastWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: None,
                end: true,
            },
            Sample {
                name: "demo-tools-this-week-55x24-scrolled",
                tab: Tab::Tools,
                which: Which::ThisWeek,
                cols: 55,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: false,
                scroll: Some(20),
                end: false,
            },
            Sample {
                name: "demo-help-55x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 55,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: true,
                scroll: None,
                end: false,
            },
            Sample {
                name: "demo-help-80x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: true,
                help: true,
                scroll: None,
                end: false,
            },
            Sample {
                name: "light-recap-this-week-80x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "paper",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "light-tools-this-week-80x24",
                tab: Tab::Tools,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "paper",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "light-recap-this-week-55x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 55,
                rows: 24,
                theme: "paper",
                calm: true,
                help: false,
                scroll: None,
                end: false,
            },
            Sample {
                name: "light-help-80x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "paper",
                calm: true,
                help: true,
                scroll: None,
                end: false,
            },
            Sample {
                name: "notcalm-recap-this-week-80x24",
                tab: Tab::Recap,
                which: Which::ThisWeek,
                cols: 80,
                rows: 24,
                theme: "frontier",
                calm: false,
                help: false,
                scroll: None,
                end: false,
            },
        ]
    }

    #[test]
    fn period_selection_and_steps() {
        assert_eq!(which_of(Span::Week, true), Which::ThisWeek);
        assert_eq!(which_of(Span::Month, false), Which::LastMonth);
        assert_eq!(step_period(Which::ThisWeek, -1), Which::ThisWeek);
        assert_eq!(step_period(Which::ThisWeek, 1), Which::LastWeek);
        assert_eq!(step_period(Which::LastWeek, 1), Which::ThisMonth);
        assert_eq!(step_period(Which::LastMonth, 1), Which::LastMonth);
    }

    #[test]
    fn period_row_checker_rejects_previous_dates_on_a_current_label() {
        assert!(period_row_is_actual(
            " ◂ this week  ▸   Nov 16 – 22 · no records",
            "this week",
            "Nov 16 – 22",
            "Nov 9 – 15"
        ));
        assert!(!period_row_is_actual(
            " ◂ this week  ▸   Nov 16 – 22 and Nov 9 – 15",
            "this week",
            "Nov 16 – 22",
            "Nov 9 – 15",
        ));
        assert!(!period_row_is_actual(
            " ◂ last week  ▸   Nov 9 – 15",
            "this week",
            "Nov 16 – 22",
            "Nov 9 – 15"
        ));
    }

    /// Clicks reach the tabs and the periods where the frame drew them.
    #[test]
    fn clicks_name_a_report_or_a_period() {
        let sample = |cols, rows| Sample {
            name: "",
            tab: Tab::Recap,
            which: Which::ThisWeek,
            cols,
            rows,
            theme: "frontier",
            calm: true,
            help: false,
            scroll: None,
            end: false,
        };
        let view = paint_sample(&sample(80, 24));
        let (tools, _) = terminal::find_in_row(&view.frame, 0, " TOOLS ").unwrap();
        let (recap, _) = terminal::find_in_row(&view.frame, 0, " RECAP ").unwrap();
        assert_eq!(view.click(tools + 3, 0), Some(Click::Tab(Tab::Tools)));
        assert_eq!(view.click(recap, 0), Some(Click::Tab(Tab::Recap)));
        assert_eq!(view.click(2, 0), None, "Loupe's name");
        assert_eq!(view.click(1, 1), Some(Click::Step(-1)));
        let right = compact_selector_width() as i32;
        assert_eq!(view.click(right, 1), Some(Click::Step(1)));
        assert_eq!(view.click(7, 1), None, "the period's name between the arrows");
        assert_eq!(view.click(10, 8), None, "the report body");
        // Wide, all four periods are named and each is a target.
        let view = paint_sample(&sample(120, 40));
        let (last_month, _) = terminal::find_in_row(&view.frame, 1, " last month ").unwrap();
        assert_eq!(view.click(last_month + 2, 1), Some(Click::Period(Which::LastMonth)));
        assert_eq!(view.click(2, 1), Some(Click::Period(Which::ThisWeek)));
    }

    #[test]
    fn ready_frames_match_the_design_text() {
        let mut failures = Vec::new();
        for sample in samples() {
            let view = paint_sample(&sample);
            let got = mask_clocks(&frame_rows(&view.frame));
            let expect = mask_clocks(&expect_rows(&frame_json(sample.name)));
            if let Some(message) = diff(sample.name, &got, &expect) {
                failures.push(message);
                if failures.len() == 3 {
                    break;
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    fn expand_style(runs: &serde_json::Value, cols: usize) -> Vec<(u32, u32, bool)> {
        let mut out = vec![(0, 0, false); cols];
        for run in runs.as_array().unwrap() {
            let start = run[0].as_u64().unwrap() as usize;
            let len = run[1].as_u64().unwrap() as usize;
            let fg =
                u32::from_str_radix(run[2].as_str().unwrap().trim_start_matches('#'), 16).unwrap();
            let bg =
                u32::from_str_radix(run[3].as_str().unwrap().trim_start_matches('#'), 16).unwrap();
            let bold = run[4].as_bool().unwrap();
            for cell in &mut out[start..start + len] {
                *cell = (fg, bg, bold);
            }
        }
        out
    }

    fn style_deltas(canvas: &Canvas, styles: &serde_json::Value) -> Vec<String> {
        let mut bad = Vec::new();
        for (y, runs) in styles.as_array().unwrap().iter().enumerate() {
            let painted = expand_style(runs, canvas.w);
            for x in 0..canvas.w {
                let cell = canvas.cells[y * canvas.w + x];
                let (fg, bg, bold) = painted[x];
                if y == 0 && cell.ch == ' ' {
                    if cell.bg != bg || cell.bold != bold {
                        bad.push(format!("header space {x},{y}"));
                    }
                    continue;
                }
                if cell.fg != fg || cell.bg != bg || cell.bold != bold {
                    bad.push(format!(
                        "{x},{y} {:?} fg #{:06x}/#{fg:06x} bg #{:06x}/#{bg:06x} bold {}/{bold}",
                        cell.ch, cell.fg, cell.bg, cell.bold
                    ));
                    if bad.len() == 6 {
                        return bad;
                    }
                }
            }
        }
        bad
    }

    #[test]
    fn help_and_card_styles_match_and_a_flipped_color_fails() {
        let sample = Sample {
            name: "demo-help-55x24",
            tab: Tab::Recap,
            which: Which::ThisWeek,
            cols: 55,
            rows: 24,
            theme: "frontier",
            calm: true,
            help: true,
            scroll: None,
            end: false,
        };
        let view = paint_sample(&sample);
        let styles = frame_json(sample.name);
        let bad = style_deltas(&view.frame, &styles["style"]);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        let card = Sample {
            name: "demo-recap-this-week-80x24",
            tab: Tab::Recap,
            which: Which::ThisWeek,
            cols: 80,
            rows: 24,
            theme: "frontier",
            calm: true,
            help: false,
            scroll: None,
            end: false,
        };
        let mut card_view = paint_sample(&card);
        let card_styles = frame_json(card.name);
        let bad = style_deltas(&card_view.frame, &card_styles["style"]);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        let restless = Sample {
            name: "notcalm-recap-this-week-80x24",
            tab: Tab::Recap,
            which: Which::ThisWeek,
            cols: 80,
            rows: 24,
            theme: "frontier",
            calm: false,
            help: false,
            scroll: None,
            end: false,
        };
        let restless_view = paint_sample(&restless);
        let restless_styles = frame_json(restless.name);
        let bad = style_deltas(&restless_view.frame, &restless_styles["style"]);
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        let index = card_view
            .frame
            .cells
            .iter()
            .position(|cell| cell.ch == 'T')
            .unwrap();
        card_view.frame.cells[index].fg ^= 0x00ff00;
        assert!(!style_deltas(&card_view.frame, &card_styles["style"]).is_empty());
        let _ = view.frame.w;
    }

    #[test]
    fn resize_keeps_the_programs_section_and_the_floor_keeps_the_anchor() {
        let tools = demo_tools(Which::LastWeek);
        let state = state_of(true, true, NOW);
        let palette = palette("frontier");
        let mut view = InsightsView::new();
        let paint_at = |view: &mut InsightsView, cols: usize, rows: usize| {
            let input = Input {
                state: &state,
                palette,
                local: &la,
                tab: Tab::Tools,
                requested: Which::LastWeek,
                source: Source::Demo,
                body: Body::Report(Report::Tools(tools)),
                previous: None,
                started: None,
                read_at: Some(NOW),
                help: false,
                explicit_db: false,
                revision: 3,
                cols,
                rows,
            };
            view.paint(&input);
        };
        paint_at(&mut view, 80, 24);
        let programs = (0..view.body.h)
            .find(|&y| line_text(&view.body, y).contains("COMMANDS BY PROGRAM"))
            .expect("programs heading");
        view.scroll(Motion::Rows(programs as isize));
        paint_at(&mut view, 80, 24);
        assert_eq!(view.places[1].anchor, Some(Section::Programs));
        let got = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("resize-0-80x24")));
        assert!(
            diff("resize-0", &got, &expect).is_none(),
            "{}",
            diff("resize-0", &got, &expect).unwrap_or_default()
        );
        paint_at(&mut view, 120, 40);
        let got = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("resize-1-120x40")));
        assert!(
            diff("resize-1", &got, &expect).is_none(),
            "{}",
            diff("resize-1", &got, &expect).unwrap_or_default()
        );
        paint_at(&mut view, 55, 24);
        let got = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("resize-2-55x24")));
        assert!(
            diff("resize-2", &got, &expect).is_none(),
            "{}",
            diff("resize-2", &got, &expect).unwrap_or_default()
        );
        let builds_at_list = view.body_builds();
        paint_at(&mut view, 40, 12);
        let got = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("resize-3-40x12")));
        assert!(
            diff("resize-3", &got, &expect).is_none(),
            "{}",
            diff("resize-3", &got, &expect).unwrap_or_default()
        );
        assert_eq!(view.places[1].anchor, Some(Section::Programs));
        assert_eq!(
            view.body_builds(),
            builds_at_list,
            "the floor does not rebuild the body"
        );
        paint_at(&mut view, 80, 24);
        let got = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("resize-4-80x24")));
        assert!(
            diff("resize-4", &got, &expect).is_none(),
            "{}",
            diff("resize-4", &got, &expect).unwrap_or_default()
        );
        assert_eq!(
            view.body_builds(),
            builds_at_list + 1,
            "the cache holds one body, so the card is built again"
        );
        assert_eq!(
            builds_at_list, 3,
            "the first card, the wide form, and the list"
        );
        view.scroll(Motion::Rows(-1));
        paint_at(&mut view, 80, 24);
        assert_eq!(view.places[1].anchor, Some(Section::Programs));
        for (w, h) in [(55, 24), (120, 40), (80, 24)] {
            paint_at(&mut view, w, h);
        }
        assert_eq!(view.places[1].scroll, programs);

        view.scroll(Motion::End);
        paint_at(&mut view, 80, 24);
        let end = view.places[1].scroll;
        for (w, h) in [(120, 40), (55, 24), (40, 12), (80, 24)] {
            paint_at(&mut view, w, h);
        }
        assert_eq!(view.places[1].scroll, end);
    }

    fn empty_recap(which: Which) -> Recap {
        let period = Period::new(NOW, which, &la);
        Recap {
            period: which.span(),
            so_far: which.so_far(),
            from: recap::local_date(&la, period.start()),
            to: recap::local_date(&la, period.last_day()),
            days: period.len() as u32,
            ..Recap::default()
        }
    }

    fn empty_tools(which: Which) -> Tools {
        let period = Period::new(NOW, which, &la);
        Tools {
            period: which.span(),
            so_far: which.so_far(),
            from: recap::local_date(&la, period.start()),
            to: recap::local_date(&la, period.last_day()),
            days: period.len() as u32,
            ..Tools::default()
        }
    }

    fn paint_body(
        state: &State,
        tab: Tab,
        requested: Which,
        body: Body<'_>,
        cols: usize,
        rows: usize,
    ) -> InsightsView {
        let palette = palette("frontier");
        let mut view = InsightsView::new();
        let input = Input {
            state,
            palette,
            local: &la,
            tab,
            requested,
            source: if state.demo {
                Source::Demo
            } else {
                Source::Records
            },
            body,
            previous: None,
            started: Some(NOW),
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 1,
            cols,
            rows,
        };
        view.paint(&input);
        view
    }

    #[test]
    fn fallback_labels_the_report_on_screen_at_55_and_80() {
        let state = state_of(true, true, NOW);
        let cases = [
            (
                Tab::Recap,
                Which::LastWeek,
                Which::ThisWeek,
                "Nov 16 – 22",
                "Nov 9 – 15",
                "this week",
                "demo-recap-this-week-55x24",
                "demo-recap-this-week-80x24",
            ),
            (
                Tab::Tools,
                Which::LastWeek,
                Which::ThisWeek,
                "Nov 16 – 22",
                "Nov 9 – 15",
                "this week",
                "demo-tools-this-week-55x24",
                "demo-tools-this-week-80x24",
            ),
        ];
        for (tab, requested, actual, dates, foreign, word, narrow, card) in cases {
            for (cols, rows, fixture) in [(55, 24, narrow), (80, 24, card)] {
                let report = match tab {
                    Tab::Recap => Report::Recap(demo_recap(actual)),
                    Tab::Tools => Report::Tools(demo_tools(actual)),
                };
                let view = paint_body(&state, tab, requested, Body::Report(report), cols, rows);
                let rows = mask_clocks(&frame_rows(&view.frame));
                assert_eq!(view.displayed_period(), actual);
                assert!(
                    period_row_is_actual(&rows[1], word, dates, foreign),
                    "{}",
                    rows[1]
                );
                if cols < 120 {
                    assert!(rows[1].contains("last week empty"), "{}", rows[1]);
                }
                let expect = mask_clocks(&expect_rows(&frame_json(fixture)));
                assert!(
                    diff(
                        fixture,
                        &rows[2..rows.len() - 1],
                        &expect[2..expect.len() - 1]
                    )
                    .is_none(),
                    "{}",
                    diff(
                        fixture,
                        &rows[2..rows.len() - 1],
                        &expect[2..expect.len() - 1]
                    )
                    .unwrap_or_default()
                );
            }
        }
        for tab in [Tab::Recap, Tab::Tools] {
            for cols in [55, 80] {
                let report = match tab {
                    Tab::Recap => Report::Recap(demo_recap(Which::ThisMonth)),
                    Tab::Tools => Report::Tools(demo_tools(Which::ThisMonth)),
                };
                let view = paint_body(
                    &state,
                    tab,
                    Which::LastMonth,
                    Body::Report(report),
                    cols,
                    24,
                );
                let rows = frame_rows(&view.frame);
                assert_eq!(view.displayed_period(), Which::ThisMonth);
                assert!(
                    period_row_is_actual(&rows[1], "this month", "November 2026", "October 2026"),
                    "{}",
                    rows[1]
                );
                assert!(rows[1].contains("last month empty"), "{}", rows[1]);
            }
        }
        let report = Report::Recap(demo_recap(Which::ThisMonth));
        let view = paint_body(
            &state,
            Tab::Recap,
            Which::LastMonth,
            Body::Report(report),
            80,
            24,
        );
        let rows = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("demo-recap-this-month-80x24")));
        assert!(
            diff(
                "month body",
                &rows[2..rows.len() - 1],
                &expect[2..expect.len() - 1]
            )
            .is_none(),
            "{}",
            diff(
                "month body",
                &rows[2..rows.len() - 1],
                &expect[2..expect.len() - 1]
            )
            .unwrap_or_default()
        );

        let report = Report::Recap(demo_recap(Which::ThisWeek));
        let view = paint_body(
            &state,
            Tab::Recap,
            Which::LastWeek,
            Body::Report(report),
            120,
            40,
        );
        let rows = frame_rows(&view.frame);
        assert!(
            period_row_is_actual(&rows[1], "this week", "Nov 16 – 22, 2026", "Nov 9 – 15"),
            "{}",
            rows[1]
        );
        let palette = palette("frontier");
        let active = view.frame.cells[1 * view.frame.w + 2];
        assert_eq!(active.ch, 't');
        assert!(
            active.bold && active.bg == palette.sel,
            "the current period is the selected one"
        );
        let inactive = view.frame.cells[1 * view.frame.w + 14];
        assert_eq!(inactive.ch, 'l');
        assert!(!inactive.bold && inactive.fg == palette.dim && inactive.bg == palette.bg);
    }

    fn words(text: &str) -> String {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn both_empty_keeps_the_requested_period_and_this_week_hints() {
        let state = state_of(false, true, NOW);
        for cols in [55, 80] {
            let recap = empty_recap(Which::LastWeek);
            let tools = empty_tools(Which::LastWeek);
            for (tab, body) in [
                (Tab::Recap, Body::Report(Report::Recap(&recap))),
                (Tab::Tools, Body::Report(Report::Tools(&tools))),
            ] {
                let view = paint_body(&state, tab, Which::LastWeek, body, cols, 24);
                let text = words(&frame_rows(&view.frame).join("\n"));
                assert_eq!(view.displayed_period(), Which::LastWeek);
                assert!(
                    text.contains("last week") && text.contains("Nov 9 – 15"),
                    "{text}"
                );
                assert!(text.contains("or this week so far"), "{text}");
                assert!(!text.contains("→ try last week"), "{text}");
                assert!(!text.contains("Nov 16 – 22"), "{text}");
                assert!(period_row_is_actual(
                    &frame_rows(&view.frame)[1],
                    "last week",
                    "Nov 9 – 15",
                    "Nov 16 – 22"
                ));
            }
            let recap = empty_recap(Which::LastMonth);
            let tools = empty_tools(Which::LastMonth);
            for (tab, body) in [
                (Tab::Recap, Body::Report(Report::Recap(&recap))),
                (Tab::Tools, Body::Report(Report::Tools(&tools))),
            ] {
                let view = paint_body(&state, tab, Which::LastMonth, body, cols, 24);
                let text = words(&frame_rows(&view.frame).join("\n"));
                assert_eq!(view.displayed_period(), Which::LastMonth);
                assert!(
                    text.contains("last month") && text.contains("October 2026"),
                    "{text}"
                );
                assert!(text.contains("or this month so far"), "{text}");
                assert!(!text.contains("→ try last month"), "{text}");
                assert!(!text.contains("November 2026"), "{text}");
            }
        }
        for (tab, cols) in [
            (Tab::Recap, 55),
            (Tab::Recap, 80),
            (Tab::Recap, 120),
            (Tab::Tools, 55),
            (Tab::Tools, 80),
            (Tab::Tools, 120),
        ] {
            let recap = empty_recap(Which::ThisWeek);
            let tools = empty_tools(Which::ThisWeek);
            let body = match tab {
                Tab::Recap => Body::Report(Report::Recap(&recap)),
                Tab::Tools => Body::Report(Report::Tools(&tools)),
            };
            let view = paint_body(&state, tab, Which::ThisWeek, body, cols, 24);
            let rows = frame_rows(&view.frame);
            let text = words(&rows.join("\n"));
            assert_eq!(view.displayed_period(), Which::ThisWeek);
            assert!(text.contains("→ try last week"), "{text}");
            assert!(
                period_row_is_actual(&rows[1], "this week", "Nov 16 – 22", "Nov 9 – 15"),
                "{}",
                rows[1]
            );
            assert!(!text.contains("Nov 9 – 15"), "{text}");
        }
        let recap = empty_recap(Which::ThisMonth);
        let tools = empty_tools(Which::ThisMonth);
        for cols in [55, 80, 120] {
            for (tab, body) in [
                (Tab::Recap, Body::Report(Report::Recap(&recap))),
                (Tab::Tools, Body::Report(Report::Tools(&tools))),
            ] {
                let view = paint_body(&state, tab, Which::ThisMonth, body, cols, 24);
                let text = words(&frame_rows(&view.frame).join("\n"));
                assert!(text.contains("→ try last month"), "{text}");
                assert!(text.contains("November 2026"), "{text}");
                assert!(text.contains("this month"), "{text}");
                assert!(!text.contains("October 2026"), "{text}");
            }
        }
    }

    #[test]
    fn loading_names_the_report_and_does_not_claim_the_read_stopped() {
        let state = state_of(false, true, NOW);
        let view = paint_body(&state, Tab::Recap, Which::ThisWeek, Body::Loading, 80, 24);
        let rows = mask_clocks(&frame_rows(&view.frame));
        let expect = mask_clocks(&expect_rows(&frame_json("db-loading-80x24")));
        assert_eq!(rows[0], expect[0]);
        assert_eq!(rows[1], expect[1]);
        assert_eq!(rows.last().unwrap(), expect.last().unwrap());
        let body = rows[2..rows.len() - 1].join("\n");
        assert!(
            body.contains("Reading Recap for this week from T3, started 10:20:00."),
            "{body}"
        );
        assert!(
            body.contains("i returns to Live. A read may finish in the background."),
            "{body}"
        );
        assert!(!body.contains("stops the read"), "{body}");
        assert!(
            !body
                .chars()
                .any(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch)),
            "{body}"
        );
        let tools = state_of(false, true, NOW);
        let view = paint_body(&tools, Tab::Tools, Which::LastMonth, Body::Loading, 55, 24);
        let text = frame_rows(&view.frame).join("\n");
        assert!(
            text.contains("Reading Tools for last month from T3"),
            "{text}"
        );
        let demo = state_of(true, true, NOW);
        let view = paint_body(&demo, Tab::Recap, Which::LastMonth, Body::Loading, 80, 24);
        let text = frame_rows(&view.frame).join("\n");
        assert!(text.contains("Making up Recap for last month"), "{text}");
        assert!(text.contains("making up the demo"), "{text}");
    }

    #[test]
    fn unreadable_notice_and_dropped_tips() {
        let state = state_of(false, true, NOW);
        let tips = vec![
            "Check that it is T3 Code's statev2.sqlite and that your user can read it.".to_owned(),
            "Check the path given to --db.".to_owned(),
            "Just looking? loupe --recap --demo shows a made-up week.".to_owned(),
        ];
        for (name, cols, rows) in [
            ("db-unreadable-55x24", 55, 24),
            ("db-unreadable-80x24", 80, 24),
            ("db-unreadable-120x40", 120, 40),
        ] {
            let palette = palette("frontier");
            let mut view = InsightsView::new();
            let input = Input {
                state: &state,
                palette,
                local: &la,
                tab: Tab::Recap,
                requested: Which::ThisWeek,
                source: Source::Records,
                body: Body::Unreadable {
                    message: UNREADABLE,
                    tips: &tips,
                },
                previous: None,
                started: None,
                read_at: None,
                help: false,
                explicit_db: false,
                revision: 1,
                cols,
                rows,
            };
            view.paint(&input);
            let got = mask_clocks(&frame_rows(&view.frame));
            let text = got.join("\n");
            assert!(text.contains("Can't read the synthetic database"), "{name}: {text}");
            assert!(text.contains("can't read T3"), "{name}: {text}");
            assert!(!text.contains("--db"), "{name} kept the --db tip");
            assert!(
                !text.contains("Just looking"),
                "{name} kept the command tip"
            );
        }
        let palette = palette("frontier");
        let mut view = InsightsView::new();
        let input = Input {
            state: &state,
            palette,
            local: &la,
            tab: Tab::Recap,
            requested: Which::ThisWeek,
            source: Source::Records,
            body: Body::Unreadable {
                message: UNREADABLE,
                tips: &tips,
            },
            previous: None,
            started: None,
            read_at: None,
            help: false,
            explicit_db: true,
            revision: 1,
            cols: 80,
            rows: 24,
        };
        let text = frame_rows(view.paint(&input)).join("\n");
        assert!(text.contains("--db"), "an explicit --db keeps its tip");
        assert!(!text.contains("Just looking"));
    }

    #[test]
    fn refresh_and_failure_keep_the_snapshot_and_close_discards_it() {
        let report = demo_recap(Which::ThisWeek);
        let demo = state_of(true, true, NOW);
        let mut view = InsightsView::new();
        let palette = palette("frontier");
        let ready = Input {
            state: &demo,
            palette,
            local: &la,
            tab: Tab::Recap,
            requested: Which::ThisWeek,
            source: Source::Demo,
            body: Body::Report(Report::Recap(report)),
            previous: None,
            started: None,
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 4,
            cols: 80,
            rows: 24,
        };
        let first = frame_rows(view.paint(&ready)).join("\n");
        assert!(first.contains("AGENT HOURS"), "{first}");
        let kept = Kept {
            report: Report::Recap(report),
            read_at: NOW,
            revision: 4,
        };
        let refreshing = Input {
            state: &demo,
            palette,
            local: &la,
            tab: Tab::Recap,
            requested: Which::ThisWeek,
            source: Source::Demo,
            body: Body::Loading,
            previous: Some(kept),
            started: Some(NOW + 65_000.0),
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 4,
            cols: 80,
            rows: 24,
        };
        let text = frame_rows(view.paint(&refreshing)).join("\n");
        assert!(
            text.contains("AGENT HOURS") && text.contains("making up the demo"),
            "{text}"
        );
        assert!(!text.contains("Making up Recap"), "{text}");
        assert_eq!(view.body_builds(), 1);

        let db = state_of(false, true, NOW);
        let failed = Input {
            state: &db,
            palette,
            local: &la,
            tab: Tab::Recap,
            requested: Which::ThisWeek,
            source: Source::Records,
            body: Body::Unreadable {
                message: UNREADABLE,
                tips: &[],
            },
            previous: Some(Kept {
                report: Report::Recap(report),
                read_at: NOW,
                revision: 4,
            }),
            started: Some(NOW),
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 9,
            cols: 80,
            rows: 24,
        };
        // The snapshot was rendered as demo. A records failure over a demo snapshot rebuilds
        // because the source changed. Use the demo state so the cached body stays.
        let failed_demo = Input {
            state: &demo,
            source: Source::Demo,
            ..failed
        };
        let text = frame_rows(view.paint(&failed_demo)).join("\n");
        assert!(text.contains("AGENT HOURS"), "{text}");
        assert!(text.contains("can't read T3 · showing"), "{text}");
        assert!(!text.contains("not a SQLite"), "{text}");

        view.close();
        let loading = Input {
            body: Body::Loading,
            previous: None,
            state: &db,
            source: Source::Records,
            ..ready
        };
        let text = frame_rows(view.paint(&loading)).join("\n");
        assert!(
            text.contains("Reading Recap for this week from T3"),
            "{text}"
        );
        assert!(!text.contains("AGENT HOURS"), "{text}");
        let _ = db;
    }

    #[test]
    fn live_wake_redraws_the_header_only_and_a_list_width_rebuilds() {
        let report = demo_recap(Which::ThisWeek);
        let mut state = state_of(true, true, NOW);
        let palette = palette("frontier");
        let mut view = InsightsView::new();
        let input = Input {
            state: &state,
            palette,
            local: &la,
            tab: Tab::Recap,
            requested: Which::ThisWeek,
            source: Source::Demo,
            body: Body::Report(Report::Recap(report)),
            previous: None,
            started: None,
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 1,
            cols: 80,
            rows: 24,
        };
        let first = frame_rows(view.paint(&input));
        drop(input);
        state.now = NOW + 1000.0;
        let input = Input {
            state: &state,
            palette,
            local: &la,
            tab: Tab::Recap,
            requested: Which::ThisWeek,
            source: Source::Demo,
            body: Body::Report(Report::Recap(report)),
            previous: None,
            started: None,
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 1,
            cols: 80,
            rows: 24,
        };
        let second = frame_rows(view.paint(&input));
        assert_eq!(view.body_builds(), 1);
        assert_ne!(first[0], second[0], "the clock moved");
        assert_eq!(first[2], second[2]);
        let wider = Input { cols: 100, ..input };
        view.paint(&wider);
        assert_eq!(
            view.body_builds(),
            1,
            "a recap card does not rebuild between 80 and 119"
        );
        // This week's recap card fits in 24 rows, so it cannot show a scroll. Last week's tools card does.
        let tall_report = demo_tools(Which::LastWeek);
        let mut tall = InsightsView::new();
        let tall_input = Input {
            state: &state,
            palette,
            local: &la,
            tab: Tab::Tools,
            requested: Which::LastWeek,
            source: Source::Demo,
            body: Body::Report(Report::Tools(tall_report)),
            previous: None,
            started: None,
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 1,
            cols: 80,
            rows: 24,
        };
        let top = frame_rows(tall.paint(&tall_input));
        tall.scroll(Motion::Rows(4));
        let scrolled = frame_rows(tall.paint(&tall_input));
        assert_eq!(tall.body_builds(), 1);
        assert_ne!(scrolled[2], top[2]);
        let tools = demo_tools(Which::ThisWeek);
        let mut tools_view = InsightsView::new();
        let narrow = Input {
            state: &state,
            palette,
            local: &la,
            tab: Tab::Tools,
            requested: Which::ThisWeek,
            source: Source::Demo,
            body: Body::Report(Report::Tools(tools)),
            previous: None,
            started: None,
            read_at: Some(NOW),
            help: false,
            explicit_db: false,
            revision: 1,
            cols: 55,
            rows: 24,
        };
        tools_view.paint(&narrow);
        let again = Input { cols: 70, ..narrow };
        tools_view.paint(&again);
        assert_eq!(
            tools_view.body_builds(),
            2,
            "a tools list depends on the width"
        );
    }
}
