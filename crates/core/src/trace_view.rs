//! Trace on a true time axis. Pure composition: the presenter owns time and selection.
use crate::{
    model,
    recap::Local,
    terminal::{self, Canvas, Palette, Rgb, mix},
    trace::{Kind, MarkKind, Span, State, Trace},
};

#[derive(Clone, Debug, Default)]
pub struct View {
    pub selected: Option<String>,
    pub scroll: usize,
    pub calm: bool,
    pub mono: bool,
    pub spin: usize,
    pub demo: bool,
    pub now: f64,
}
#[derive(Clone)]
pub struct Drawn {
    pub canvas: Canvas,
    pub rows: Vec<(String, i32)>,
    pub selected: Option<String>,
    pub more_rows: usize,
    /// Where the running spinner is, the only cell that changes with `View::spin`.
    pub spinner: Option<(i32, i32)>,
}
struct Pen<'a> {
    c: Canvas,
    p: &'a Palette,
}
impl Pen<'_> {
    fn text(&mut self, x: i32, y: i32, s: &str, ink: Rgb, bold: bool) -> i32 {
        self.c.text_with(x, y, s, ink, None, bold, self.c.w.saturating_sub(x.max(0) as usize)) + x
    }
    fn runs(&mut self, mut x: i32, y: i32, pieces: &[(String, Rgb, bool)]) {
        for (s, ink, bold) in pieces {
            let room = self.c.w.saturating_sub(1 + x.max(0) as usize);
            let text = terminal::fit(s, room);
            self.text(x, y, &text, *ink, *bold);
            if terminal::width(s) > room {
                break;
            }
            x += terminal::width(s) as i32;
        }
    }
}
fn duration(ms: f64) -> String {
    let s = (ms.max(0.) / 1000.).round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 600 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else if s < 3600 {
        format!("{}m", (s as f64 / 60.).round() as u64)
    } else {
        let m = (s as f64 / 60.).round() as u64;
        format!("{}h {:02}m", m / 60, m % 60)
    }
}
/// `terminal::fit`, without a space left before the ellipsis: `Find the…`, not `Find the …`.
fn cut(s: &str, max: usize) -> String {
    let fitted = terminal::fit(s, max);
    match fitted.strip_suffix('…') {
        Some(head) if head.ends_with(' ') => format!("{}…", head.trim_end()),
        _ => fitted,
    }
}
/// The marks of a step's row and the model's: a bar, its live edge, a failure.
const BARS: [char; 9] = ['━', '▌', '✗', '×', '▁', '▂', '▃', '▄', '░'];
/// `ink` mixed toward `toward` just far enough to reach 3:1 on `on`, in twentieths.
fn lift(ink: Rgb, on: Rgb, toward: Rgb) -> Rgb {
    (0..=20).map(|k| mix(ink, toward, k as f64 / 20.)).find(|c| crate::integration::contrast(*c, on) >= 3.).unwrap_or(toward)
}
/// A fold's label: `14h`, `2h`, `45m`; whole hours from an hour.
fn fold_label(ms: f64) -> String {
    let minutes = (ms / 60_000.).round() as u64;
    if minutes >= 60 { format!("{}h", (minutes as f64 / 60.).round() as u64) } else { format!("{minutes}m") }
}
/// A length rounded up to a round duration: 1, 2, 5, 10, 15 or 30 seconds or minutes, or
/// whole hours.
fn round_step(ms: f64) -> f64 {
    const STEPS: [f64; 12] = [1., 2., 5., 10., 15., 30., 60., 120., 300., 600., 900., 1800.];
    let seconds = ms.max(1.) / 1000.;
    match STEPS.iter().find(|&&s| s >= seconds) {
        Some(s) => s * 1000.,
        None => (seconds / 3600.).ceil() * 3_600_000.,
    }
}
/// A total, rounded: `45s`, `8m`, `1h 52m`. Seconds are kept only under a minute, so totals
/// read at a glance and do not flicker while the task runs.
fn rough(ms: f64) -> String {
    let s = (ms.max(0.) / 1000.).round() as u64;
    if s < 60 {
        return format!("{s}s");
    }
    let m = (s as f64 / 60.).round() as u64;
    if m < 60 { format!("{m}m") } else { format!("{}h {:02}m", m / 60, m % 60) }
}
fn clock(ms: f64, local: Local) -> String {
    let t = local((ms / 1000.).floor() as i64).unwrap_or_default();
    format!("{:02}:{:02}", t.hour, t.minute)
}
// Recap's shade rule (recap_view.rs, Pen::shade_amount), checked on all surfaces used here.
fn shade(p: &Palette) -> Rgb {
    let mut amount = 0.55;
    while amount > 0.15 && [p.bg, p.panel, p.sel].iter().any(|on| crate::integration::contrast(mix(p.accent, p.bg, amount), *on) < 3.) {
        amount -= 0.05;
    }
    mix(p.accent, p.bg, amount)
}
fn allot(w: usize, values: &[f64]) -> Vec<usize> {
    let total: f64 = values.iter().sum();
    if total <= 0. {
        return vec![0; values.len()];
    }
    let raw: Vec<_> = values.iter().map(|v| v.max(0.) / total * w as f64).collect();
    let mut n: Vec<_> = raw.iter().zip(values).map(|(r, v)| if *v > 0. { (*r as usize).max(1) } else { 0 }).collect();
    while n.iter().sum::<usize>() < w {
        let i = (0..n.len())
            .filter(|&i| values[i] > 0.)
            .max_by(|&a, &b| (raw[a] - n[a] as f64).total_cmp(&(raw[b] - n[b] as f64)).then(b.cmp(&a)))
            .unwrap();
        n[i] += 1;
    }
    while n.iter().sum::<usize>() > w {
        let Some(i) = (0..n.len()).filter(|&i| n[i] > 1).max_by(|&a, &b| (n[a] as f64 - raw[a]).total_cmp(&(n[b] as f64 - raw[b]))) else { break };
        n[i] -= 1;
    }
    n
}
fn is_step(s: &Span) -> bool {
    matches!(s.kind, Kind::Tool | Kind::Ask | Kind::Delegate)
}
fn must(s: &Span) -> bool {
    s.end.is_none() || matches!(s.kind, Kind::Ask | Kind::Delegate)
}
fn end(s: &Span, now: f64) -> f64 {
    s.end.unwrap_or(now).max(s.start)
}
/// A row's name: a delegate's title (or model), a tool's name. An MCP tool's recorded name
/// (`mcp__t3-code__task_status`) is shown as the tool alone, `task_status`.
fn name(s: &Span) -> &str {
    match &s.delegate {
        Some(d) => d.title.as_deref().unwrap_or(&d.model),
        None => s.label.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")).map_or(s.label.as_str(), |(_, tool)| tool),
    }
}
/// The same, in full for the selected row's words: `t3-code task_status`.
fn full_name(s: &Span) -> String {
    match &s.delegate {
        Some(d) => d.model.clone(),
        None => s.label.strip_prefix("mcp__").and_then(|rest| rest.split_once("__")).map_or(s.label.clone(), |(server, tool)| format!("{server} {tool}")),
    }
}
fn glyph(s: &Span) -> char {
    // The reader normally supplies normalized categories. Raw file_search also means read.
    model::glyph(if s.category == "file_search" {
        "read"
    } else if s.category == "command_execution" {
        model::category(&s.category, None)
    } else {
        &s.category
    })
}
fn ink(s: &Span, p: &Palette) -> Rgb {
    if s.kind == Kind::Ask { p.hot } else { s.delegate.as_ref().map_or(p.accent, |d| p.provider_of(&d.model)) }
}
/// The time axis: true time, except stretches in which nothing was in flight for over half an
/// hour (`Trace::quiet`), each folded to a few cells that say how long it was.
/// Folds take at most half the width; the largest are folded first.
struct Axis {
    x: usize,
    w: usize,
    lo: f64,
    hi: f64,
    pieces: Vec<Piece>,
}
/// A stretch of the axis: from and to in time, its first column and width in cells (as
/// fractions), and the label of a fold.
struct Piece {
    from: f64,
    to: f64,
    x: f64,
    cells: f64,
    fold: Option<String>,
}
impl Axis {
    fn new(x: usize, w: usize, lo: f64, hi: f64, quiet: &[(f64, f64)]) -> Axis {
        let mut folds: Vec<(f64, f64, String)> =
            quiet.iter().map(|&(a, b)| (a.max(lo), b.min(hi))).filter(|(a, b)| b > a).map(|(a, b)| (a, b, fold_label(b - a))).collect();
        folds.sort_by(|p, q| (q.1 - q.0).total_cmp(&(p.1 - p.0)));
        let mut kept: Vec<(f64, f64, String)> = vec![];
        let mut used = 0;
        for f in folds {
            let width = f.2.chars().count() + 2;
            if used + width <= w / 2 {
                used += width;
                kept.push(f);
            }
        }
        kept.sort_by(|p, q| p.0.total_cmp(&q.0));
        let active = (hi - lo) - kept.iter().map(|(a, b, _)| b - a).sum::<f64>();
        let room = (w - used) as f64;
        let (mut pieces, mut t, mut col) = (vec![], lo, x as f64);
        let push = |pieces: &mut Vec<Piece>, from: f64, to: f64, cells: f64, fold: Option<String>, col: &mut f64| {
            pieces.push(Piece { from, to, x: *col, cells, fold });
            *col += cells;
        };
        for (a, b, label) in kept {
            if a > t {
                push(&mut pieces, t, a, (a - t) / active * room, None, &mut col);
            }
            let width = label.chars().count() as f64 + 2.;
            push(&mut pieces, a, b, width, Some(label), &mut col);
            t = b;
        }
        push(&mut pieces, t, hi, (hi - t) / active.max(1.) * room, None, &mut col);
        Axis { x, w, lo, hi, pieces }
    }
    fn at(&self, t: f64) -> f64 {
        let t = t.clamp(self.lo, self.hi);
        let piece = self.pieces.iter().find(|p| t <= p.to).unwrap_or(self.pieces.last().unwrap());
        let share = if piece.to > piece.from { (t - piece.from) / (piece.to - piece.from) } else { 0. };
        (piece.x + share * piece.cells).min((self.x + self.w) as f64)
    }
    fn cell(&self, t: f64) -> usize {
        (self.at(t).floor() as usize).min(self.x + self.w - 1)
    }
    /// The time a column covers, or None for a fold's columns.
    fn time_of(&self, column: usize) -> Option<(f64, f64)> {
        let (left, right) = (column as f64, column as f64 + 1.);
        let piece = self.pieces.iter().find(|p| left < p.x + p.cells - 1e-9)?;
        if piece.fold.is_some() || piece.cells <= 0. {
            return None;
        }
        let time = |c: f64| piece.from + ((c - piece.x) / piece.cells).clamp(0., 1.) * (piece.to - piece.from);
        Some((time(left), time(right.min(piece.x + piece.cells))))
    }
    /// The instant at a column's left edge.
    fn time_at(&self, column: usize) -> f64 {
        let c = column as f64;
        let piece = self.pieces.iter().find(|p| c < p.x + p.cells - 1e-9).unwrap_or(self.pieces.last().unwrap());
        piece.from + ((c - piece.x) / piece.cells.max(1e-9)).clamp(0., 1.) * (piece.to - piece.from)
    }
    /// Whether an instant falls inside a fold.
    fn folded(&self, t: f64) -> bool {
        self.pieces.iter().any(|p| p.fold.is_some() && t > p.from && t < p.to)
    }
    /// Each fold: its first column, its width and its label.
    fn folds(&self) -> impl Iterator<Item = (usize, usize, &str)> {
        self.pieces.iter().filter_map(|p| p.fold.as_deref().map(|label| (p.x.round() as usize, p.cells.round() as usize, label)))
    }
    fn bounds(&self, s: &Span, now: f64) -> (usize, usize) {
        let a = self.cell(s.start);
        (a, (self.at(end(s, now)).ceil() as usize).max(a + 1).min(self.x + self.w))
    }
}
fn union(spans: &[&Span], now: f64) -> f64 {
    let mut ranges: Vec<_> = spans.iter().map(|s| (s.start, end(s, now))).collect();
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (mut total, mut until) = (0., f64::NEG_INFINITY);
    for (a, b) in ranges {
        total += (b - a.max(until)).max(0.);
        until = until.max(b);
    }
    total
}
fn detail(t: &Trace, key: &str, folded: &[&Span], now: f64, p: &Palette, local: Local) -> Vec<(String, Rgb, bool)> {
    if key == "model" {
        return vec![
            ("∴ ".into(), shade(p), false),
            ("model ".into(), p.fg, true),
            (rough(t.split.seen + t.split.model), p.bright, true),
            (format!(" · {} seen thinking, {} between steps, inferred", rough(t.split.seen), rough(t.split.model)), p.dim, false),
        ];
    }
    if key == "more" {
        let together = folded.iter().filter(|s| s.closed_together).count();
        let timed: Vec<&Span> = folded.iter().copied().filter(|s| !s.closed_together).collect();
        let quick = timed.iter().filter(|s| end(s, now) - s.start < 1000.).count();
        let longest = timed.iter().map(|s| end(s, now) - s.start).fold(0., f64::max);
        return vec![
            ("┄ ".into(), p.faint, false),
            (format!("{} more steps ", crate::recap::count_text(folded.len() as u64)), p.fg, true),
            (rough(union(&timed, now)), p.bright, true),
            (format!(" · {quick} under a second · the longest {}", duration(longest)), p.dim, false),
            (
                if together > 0 { format!(" · {together} closed together at one instant, their own ends not recorded") } else { String::new() },
                p.dim,
                false,
            ),
        ];
    }
    let Some(s) = t.spans.iter().find(|s| s.key == key) else { return vec![] };
    let d = end(s, now) - s.start;
    let label = full_name(s);
    let mut out = vec![(format!("{} ", glyph(s)), ink(s, p), false), (format!("{label} "), p.fg, true)];
    if s.end.is_none() {
        let (word, word_ink) = match s.kind {
            Kind::Ask if s.by_message => ("open ", p.text2),
            Kind::Ask => ("waiting on you ", p.hot),
            _ => ("running ", p.text2),
        };
        out.push((word.into(), word_ink, false));
    }
    out.push((duration(d), p.bright, true));
    if s.status == "failed" {
        out.push((" ✗ failed".into(), p.hot, false));
    }
    if s.status == "cancelled" {
        out.push((" ⊘ cancelled".into(), p.dim, false));
    }
    out.push((format!(" · from {}", clock(s.start, local)), p.dim, false));
    let tail = if let Some(dl) = &s.delegate {
        if dl.remote { " · on another machine" } else { " · ⏎ opens its trace" }.into()
    } else if s.by_message {
        " · the run works past it, so it is not counted as waiting".into()
    } else if s.kind == Kind::Ask && s.label == "link" {
        " · waiting only while no run is working".into()
    } else if s.kind == Kind::Ask && s.end.is_none() {
        " · ⏎ answer it in T3".into()
    } else if s.programs.len() > 1 {
        format!(" · {}", s.programs.join(" → "))
    } else if s.kind == Kind::Tool
        && s.end.is_none()
        && d > 300_000.
        && !t.spans.iter().any(|other| other.kind == Kind::Tool && end(other, now) - other.start > d)
    {
        " · the longest command in this task".into()
    } else {
        String::new()
    };
    out.push((tail, p.dim, false));
    out
}

pub fn draw(t: &Trace, p: &Palette, cols: usize, rows: usize, view: &View, local: Local) -> Drawn {
    let mut pen = Pen { c: Canvas::new(cols, rows), p };
    pen.c.clear(p.bg, p.fg);
    let mut drawn = vec![];
    let model_ink = shade(p);
    if cols < 60 || rows < 16 {
        return finish(small(p, cols, rows), drawn, None, 0, view.mono, p);
    }
    let now = view.now;
    let hi = t.ended.unwrap_or(now).max(t.requested + 1.);
    let elapsed = hi - t.requested;
    let (mark, word, state_ink) = match t.state {
        State::Running => (if view.calm { '◉' } else { terminal::spinner((view.spin % 10) as f64 * 100.) }, "running", p.accent),
        State::Waiting => ('◆', "needs you", p.hot),
        State::Delegated => ('⇉', "delegates working", p.text2),
        State::Queued => ('◔', "queued", p.dim),
        State::Completed => ('✓', "finished", p.dim),
        State::Failed => ('✗', "failed", p.hot),
        State::Interrupted | State::Cancelled => ('⊘', "stopped", p.dim),
    };
    let right = format!("{mark} {word} {}", duration(elapsed));
    let rx = cols.saturating_sub(1 + terminal::width(&right));
    let x = pen.text(1, 0, "T3 Agents  ", p.fg, true);
    let x = pen.text(x, 0, "TRACE  ", p.text2, true);
    let demo = if view.demo { " · made up" } else { "" };
    let room = rx.saturating_sub(x as usize + 1);
    let title = terminal::fit(t.title.as_deref().unwrap_or("Untitled task"), room.saturating_sub(terminal::width(demo)));
    let x = pen.text(x, 0, &title, p.fg, true);
    pen.text(x, 0, &terminal::fit(demo, room.saturating_sub(terminal::width(&title))), p.dim, false);
    let spinner = (t.state == State::Running && !view.calm).then_some((rx as i32, 0));
    let x = pen.text(rx as i32, 0, &format!("{mark} "), state_ink, false);
    let x = pen.text(x, 0, word, if matches!(t.state, State::Waiting | State::Failed) { p.hot } else { p.text2 }, false);
    pen.text(x, 0, &format!(" {}", duration(elapsed)), p.fg, true);
    let x = pen.text(1, 1, "● ", p.family_color(terminal::family(&t.model, Some(&t.provider))), false);
    let x = pen.text(x, 1, &terminal::fit(&t.model, cols.saturating_sub(5)), p.text2, false);
    let meta = format!(
        "  asked {} · {} {} · {} steps{}",
        clock(t.requested, local),
        t.runs.len(),
        if t.runs.len() == 1 { "run" } else { "runs" },
        crate::recap::count_text(u64::from(t.steps.tools)),
        if t.steps.failed > 0 { format!(" · {} failed", crate::recap::count_text(u64::from(t.steps.failed))) } else { String::new() }
    );
    let x = pen.text(x, 1, &terminal::fit(&meta, cols.saturating_sub(x as usize + 1)), p.dim, false);
    if x + 7 < cols as i32 {
        pen.c.right(cols as i32 - 1, 1, &clock(now, local), p.dim);
    }
    terminal::section_with(&mut pen.c, 1, 3, cols as i32 - 2, "WHERE THE TIME WENT", p, &format!("of {}", rough(elapsed)), p.dim);
    let sp = &t.split;
    let values = [sp.tools, sp.seen, sp.model, sp.waiting, sp.between, sp.queue];
    let colors = [p.accent, model_ink, model_ink, p.hot, p.faint, p.faint];
    let fills = ['█', '█', '░', '█', '┄', '┄'];
    let mut x = 1;
    for (i, n) in allot(cols - 2, &values).into_iter().enumerate() {
        x = pen.text(x, 4, &fills[i].to_string().repeat(n), colors[i], false);
    }
    let mut x = 1;
    // Tools and model are always named, but not before anything has run; the queue is named
    // when it is at least a hundredth of the task, or all of it.
    let started = t.runs.iter().any(|r| r.started.is_some());
    for (i, label, v, sw) in [
        (0, "tools", sp.tools, '■'),
        (1, "model", sp.seen + sp.model, '■'),
        (3, "you", sp.waiting, '■'),
        (4, "no run", sp.between, '┄'),
        (5, "queued", sp.queue, '┄'),
    ] {
        let named = match i {
            0 | 1 => started,
            5 => v > 0. && (!started || v >= sp.total / 100.),
            _ => v > 0.,
        };
        if !named {
            continue;
        }
        let percent = if sp.total > 0. { format!("{:.0}%", 100. * v / sp.total) } else { "—".into() };
        let tail = if cols < 76 {
            String::new()
        } else {
            format!(" {}{}", rough(v), if i == 1 { format!(" · {} seen", rough(sp.seen)) } else { String::new() })
        };
        let piece = format!("{sw} {label} {percent}{tail}");
        if x as usize + terminal::width(&piece) > cols - 1 {
            break;
        }
        let a = pen.text(x, 5, &format!("{sw} "), colors[i], false);
        let a = pen.text(a, 5, &format!("{label} "), p.text2, false);
        let a = pen.text(a, 5, &percent, p.fg, true);
        x = pen.text(a, 5, &tail, p.dim, false) + 2;
    }
    let lw = if cols >= 100 {
        30
    } else if cols >= 72 {
        21
    } else {
        19
    };
    // A finished task's axis ends where it ended. A live task's ends at most one step ahead of
    // now and moves a whole step at a time (a step is a cell's length, rounded up to a round
    // duration), so the cells stay put between steps instead of shifting under every frame.
    let w = cols - 2 - lw;
    let axis_hi = match t.ended {
        Some(_) => hi,
        None => {
            let step = round_step((hi - t.requested) / w as f64);
            t.requested + ((hi - t.requested) / step).ceil().max(1.) * step
        }
    };
    let axis = Axis::new(1 + lw, w, t.requested, axis_hi, &t.quiet);
    pen.text(1, 7, "STEPS", p.text2, true);
    pen.text(axis.x as i32, 7, &"─".repeat(axis.w), p.rule, false);
    for (x, _, label) in axis.folds() {
        pen.text(x as i32, 7, &format!("┊{label}┊"), p.dim, false);
    }
    let marks: Vec<_> = t.marks.iter().map(|m| (axis.at(m.at).round() as usize).min(axis.x + axis.w - 1)).collect();
    pen.text(axis.x as i32, 7, &clock(t.requested, local), p.dim, false);
    let interval = if elapsed > 3_000_000. {
        30.
    } else if elapsed > 600_000. {
        5.
    } else {
        1.
    } * 60_000.;
    let lt = local((t.requested / 1000.).floor() as i64).unwrap_or_default();
    let offset = (lt.minute as f64 * 60. + lt.second as f64) * 1000.;
    let mut tick = t.requested + (interval - offset.rem_euclid(interval));
    let mut last = axis.x + 5;
    while tick < axis.hi {
        let xx = axis.at(tick).round() as usize;
        let on_a_fold = axis.folds().any(|(x, width, _)| xx + 6 >= x && xx <= x + width);
        if !axis.folded(tick) && !on_a_fold && xx > last + 3 && xx + 6 < cols - 6 && !marks.iter().any(|m| *m >= xx && *m <= xx + 6) {
            pen.text(xx as i32, 7, "┴", p.rule, false);
            pen.text(xx as i32 + 1, 7, &clock(tick, local), p.dim, false);
            last = xx + 6;
        }
        tick += interval;
    }
    let end_label = if t.ended.is_none() { "now".into() } else { clock(hi, local) };
    pen.c.right(cols as i32 - 1, 7, &end_label, if t.ended.is_none() { p.text2 } else { p.dim });
    for (m, x) in t.marks.iter().zip(marks) {
        pen.text(
            x as i32,
            7,
            match m.kind {
                MarkKind::Steer => "»",
                MarkKind::StopAsked => "⊘",
                MarkKind::Restart => "↻",
                MarkKind::Error => "✗",
            },
            if m.kind == MarkKind::Error { p.hot } else { p.fg },
            false,
        );
    }
    if t.state == State::Queued && t.runs.iter().all(|r| r.started.is_none()) {
        let position = t.runs.last().and_then(|r| r.queue_position).map_or(String::new(), |n| format!(" · position {n}"));
        pen.text(1, 8, &format!("◔ queued for {}{position}", duration(elapsed)), p.text2, false);
        keys(&mut pen, rows);
        return finish(pen.c, drawn, None, 0, view.mono, p);
    }
    let room = rows.saturating_sub(13);
    let steps: Vec<_> = t.spans.iter().filter(|s| is_step(s)).collect();
    let mut keep: Vec<_> = steps.iter().copied().filter(|s| must(s)).collect();
    // A call closed together with others has no length of its own, so it never ranks as long.
    let mut long: Vec<_> = steps.iter().copied().filter(|s| !must(s) && !s.closed_together && end(s, now) - s.start >= 1000.).collect();
    long.sort_by(|a, b| (end(b, now) - b.start).total_cmp(&(end(a, now) - a.start)).then(a.start.total_cmp(&b.start)));
    keep.extend(long.into_iter().take(room.saturating_sub(keep.len())));
    keep.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    // Overflow is navigation, not folding: always-kept rows stay distinct from short steps.
    let more_rows = keep.len().saturating_sub(room);
    let folded: Vec<_> = steps.iter().copied().filter(|s| !keep.iter().any(|k| k.key == s.key)).collect();
    let default = keep
        .iter()
        .rev()
        .find(|s| s.end.is_none())
        .copied()
        .or_else(|| keep.iter().copied().max_by(|a, b| (end(a, now) - a.start).total_cmp(&(end(b, now) - b.start))));
    let requested =
        view.selected.as_deref().filter(|key| *key == "model" || (*key == "more" && !folded.is_empty()) || keep.iter().any(|s| s.key == *key));
    let mut selected = requested.unwrap_or_else(|| default.map_or("model", |s| s.key.as_str()));
    let mut start = more_rows.saturating_sub(view.scroll.min(more_rows));
    if let Some(i) = keep.iter().position(|s| s.key == selected).filter(|_| requested.is_some()) {
        if i < start {
            start = i;
        }
        if i >= start + room {
            start = i + 1 - room;
        }
    }
    if requested.is_none() && !keep.iter().skip(start).take(room).any(|s| s.key == selected) && selected != "model" {
        selected = keep.iter().skip(start).take(room).last().map_or("model", |s| s.key.as_str());
    }
    let mut y = 8;
    pen.text(1, y, "∴ ", model_ink, false);
    pen.text(3, y, "model", p.text2, false);
    pen.c.right(lw as i32 - 1, y, &rough(sp.seen + sp.model), p.dim);
    for i in 0..axis.w {
        let Some((a, b)) = axis.time_of(axis.x + i) else { continue };
        if b <= a {
            continue;
        }
        let overlap = |kind| t.spans.iter().filter(|s| s.kind == kind).map(|s| (b.min(end(s, now)) - a.max(s.start)).max(0.)).sum::<f64>();
        let (seen, rest) = (overlap(Kind::Seen), overlap(Kind::Model));
        let share = ((seen + rest) / (b - a)).min(1.);
        if share > 0.04 {
            let ch = if seen >= rest { ['▁', '▂', '▃', '▄'][(share * 4.).round().clamp(1., 4.) as usize - 1] } else { '░' };
            pen.c.set((axis.x + i) as i32, y, ch, model_ink, None, false);
        }
    }
    drawn.push(("model".into(), y));
    y += 1;
    if !folded.is_empty() {
        pen.text(1, y, "┄ ", p.faint, false);
        pen.text(3, y, &format!("{} more", crate::recap::count_text(folded.len() as u64)), p.text2, false);
        let timed: Vec<&Span> = folded.iter().copied().filter(|s| !s.closed_together).collect();
        pen.c.right(lw as i32 - 1, y, &rough(union(&timed, now)), p.dim);
        let mut counts = vec![0usize; axis.w];
        for s in &folded {
            counts[axis.cell(s.start) - axis.x] += 1;
        }
        let top = *counts.iter().max().unwrap_or(&1);
        for (i, n) in counts.iter().enumerate() {
            if *n > 0 {
                pen.c.set(
                    (axis.x + i) as i32,
                    y,
                    ['▁', '▂', '▃', '▄'][(4. * (*n as f64 / top as f64).sqrt()).round().clamp(1., 4.) as usize - 1],
                    p.dim,
                    None,
                    false,
                );
            }
        }
        for s in &folded {
            if s.status == "failed" {
                pen.text(axis.cell(s.start) as i32, y, "×", p.hot, false);
            }
        }
        drawn.push(("more".into(), y));
        y += 1;
    }
    for s in keep.iter().skip(start).take(room) {
        let color = ink(s, p);
        let open = s.end.is_none();
        pen.text(1, y, &format!("{} ", glyph(s)), color, false);
        pen.text(3, y, &cut(name(s), lw - 11), if open { p.fg } else { p.text2 }, false);
        let d = duration(end(s, now) - s.start);
        pen.c.text_with((lw - 1).saturating_sub(terminal::width(&d)) as i32, y, &d, if open { p.bright } else { p.dim }, None, open, lw - 1);
        let (a, b) = axis.bounds(s, now);
        pen.text(a as i32, y, &"━".repeat(b - a), color, false);
        // An ask's bar is hot only where this ask held the person (its `holds`, from the shared
        // rule in the core); where a run was working past it, or it holds nobody, it is dim.
        if s.kind == Kind::Ask {
            for column in a..b {
                let (from, to) = (axis.time_at(column), axis.time_at(column + 1));
                if !s.holds.iter().any(|(w0, w1)| *w0 < to && *w1 > from) {
                    pen.c.cells[y as usize * cols + column].fg = p.dim;
                }
            }
        }
        if open {
            pen.text(b as i32 - 1, y, if view.calm { "━" } else { "▌" }, p.bright, false);
        }
        if s.status == "failed" {
            pen.text(b as i32 - 1, y, "✗", p.hot, false);
        } else if b < axis.x + axis.w {
            if s.status == "cancelled" {
                pen.text(b as i32, y, "⊘", p.dim, false);
            } else if s.delegate.as_ref().is_some_and(|d| d.remote) {
                pen.text(b as i32 + 1, y, &terminal::fit("remote", (axis.x + axis.w).saturating_sub(b + 1)), p.dim, false);
            }
        }
        // An open bar reaches the axis edge; its warning uses the one-cell margin.
        if open && s.kind == Kind::Tool && end(s, now) - s.start > 300_000. {
            pen.text(b as i32, y, "△", p.warn, false);
        }
        drawn.push((s.key.clone(), y));
        y += 1;
    }
    for s in t.spans.iter().filter(|s| s.kind == Kind::Between) {
        let (a, b) = axis.bounds(s, now);
        for yy in 8..y {
            for xx in a..b {
                pen.c.cells[yy as usize * cols + xx].bg = p.panel;
            }
        }
    }
    // A fold runs down the chart: its edges are marked on every row, so the break reads.
    for (x, width, _) in axis.folds() {
        for yy in 8..y {
            for edge in [x, x + width.saturating_sub(1)] {
                let cell = &mut pen.c.cells[yy as usize * cols + edge];
                if cell.ch == ' ' || cell.ch == '\0' {
                    cell.ch = '┊';
                    cell.fg = p.faint;
                }
            }
        }
    }
    if let Some((_, yy)) = drawn.iter().find(|(key, _)| key == selected) {
        for cell in &mut pen.c.cells[*yy as usize * cols..(*yy as usize + 1) * cols] {
            cell.bg = p.sel;
            cell.bold |= view.mono;
        }
        pen.c.set(0, *yy, '▎', p.accent, Some(p.sel), true);
    }
    // A bar keeps 3:1 against what it sits on. A provider's hue or the model's shade can fall
    // to 2:1 on the selection or the no-run band in some themes; there it is lifted toward the
    // foreground until it reads, keeping as much of its hue as that allows.
    for yy in 8..y as usize {
        for cell in &mut pen.c.cells[yy * cols..(yy + 1) * cols] {
            if BARS.contains(&cell.ch) && cell.bg != p.bg {
                cell.fg = lift(cell.fg, cell.bg, p.fg);
            }
        }
    }
    pen.runs(1, rows as i32 - 2, &detail(t, selected, &folded, now, p, local));
    keys(&mut pen, rows);
    Drawn { spinner, ..finish(pen.c, drawn, Some(selected.into()), more_rows, view.mono, p) }
}
fn keys(pen: &mut Pen, rows: usize) {
    let p = pen.p;
    let mut x = 1;
    for (key, word) in [("↑↓", "step"), ("←→", "task"), ("⏎", "open"), ("w", "back"), ("?", "help")] {
        x = pen.text(x, rows as i32 - 1, key, p.accent, true);
        x = pen.text(x, rows as i32 - 1, &format!(" {word}  "), p.dim, false);
    }
}
fn finish(mut canvas: Canvas, rows: Vec<(String, i32)>, selected: Option<String>, more_rows: usize, mono: bool, p: &Palette) -> Drawn {
    if mono {
        for c in &mut canvas.cells {
            c.fg = p.fg;
            c.bg = p.bg;
        }
    }
    Drawn { canvas, rows, selected, more_rows, spinner: None }
}
pub fn notice(p: &Palette, cols: usize, rows: usize, title: &str, message: &str, tips: &[String]) -> Canvas {
    let mut pen = Pen { c: Canvas::new(cols, rows), p };
    pen.c.clear(p.bg, p.fg);
    pen.text(1, 0, "T3 Agents  TRACE", p.fg, true);
    pen.text(1, 2, &terminal::fit(title, cols.saturating_sub(2)), p.text2, true);
    let mut y = 4;
    for text in std::iter::once(message).chain(tips.iter().map(String::as_str)) {
        for line in terminal::wrap(text, cols.saturating_sub(4).max(1)) {
            if y >= rows as i32 {
                break;
            }
            pen.text(2, y, &line, p.dim, false);
            y += 1;
        }
        y += 1;
    }
    pen.c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        recap,
        recap_view::{Color, paint},
        trace,
    };
    fn utc(s: i64) -> Option<crate::platform::LocalTime> {
        Some(crate::platform::utc_time(s))
    }
    fn demo() -> Trace {
        trace::demo(recap::ms("2026-11-18T14:53:00Z").unwrap()).unwrap()
    }
    fn view(t: &Trace) -> View {
        View { now: t.now, spin: 2, ..View::default() }
    }
    fn row(c: &Canvas, y: usize) -> String {
        c.cells[y * c.w..(y + 1) * c.w].iter().map(|c| c.ch).collect()
    }
    fn hand() -> Trace {
        let mut t = demo();
        t.requested = 0.;
        t.now = 600_000.;
        t.ended = Some(t.now);
        t.state = State::Completed;
        t.spans.clear();
        t.marks.clear();
        t.runs.clear();
        t.split =
            trace::Split { total: 600_000., tools: 120_000., seen: 120_000., model: 120_000., waiting: 120_000., between: 60_000., queue: 60_000. };
        for (key, kind, start, end, status) in [
            ("fail", Kind::Tool, 0., Some(120_000.), "failed"),
            ("band", Kind::Between, 240_000., Some(300_000.), "completed"),
            ("seen", Kind::Seen, 120_000., Some(240_000.), "completed"),
            ("model-span", Kind::Model, 300_000., Some(420_000.), "completed"),
        ] {
            t.spans.push(Span {
                key: key.into(),
                kind,
                category: "shell".into(),
                label: "cargo".into(),
                start,
                end,
                status: status.into(),
                measured: true,
                run: None,
                delegate: None,
                programs: vec![],
                closed_together: false,
                by_message: false,
                holds: vec![],
            });
        }
        t
    }
    #[test]
    fn sizes_selection_open_steps_and_legend() {
        for t in [demo(), hand()] {
            for (w, h) in (60..=200).map(|w| (w, 24)).chain([(60, 16), (60, 24), (80, 24), (120, 40), (190, 52)]) {
                let d = draw(&t, &Palette::default(), w, h, &view(&t), &utc);
                assert_eq!(d.canvas.cells.len(), w * h);
                assert!(d.canvas.to_text().lines().all(|s| terminal::width(s) <= w));
                assert!(row(&d.canvas, 0).contains("T3 Agents  TRACE"));
                assert!(row(&d.canvas, h - 1).contains("↑↓ step  ←→ task  ⏎ open  w back  ? help"));
                assert!(d.rows.iter().all(|(_, y)| *y >= 8 && *y < h as i32 - 2));
                assert!(d.rows.iter().any(|(k, _)| Some(k) == d.selected.as_ref()));
                for s in t.spans.iter().filter(|s| is_step(s) && s.end.is_none()) {
                    assert!(d.rows.iter().any(|(k, _)| k == &s.key));
                }
                assert_eq!(row(&d.canvas, 4).chars().filter(|c| "█░┄".contains(*c)).count(), w - 2, "split at {w}");
                assert_eq!(d.canvas.get(0, 5).unwrap().ch, ' ');
                assert_eq!(d.canvas.get(w as i32 - 1, 5).unwrap().ch, ' ');
            }
        }
    }
    #[test]
    fn help_keeps_a_gap_after_page_keys_at_the_minimum_width() {
        let mut canvas = Canvas::new(60, 16);
        help(&mut canvas, &Palette::default());
        assert!(row(&canvas, 3).contains("PgUp PgDn scroll steps"));
    }
    #[test]
    fn split_failed_band_and_words_cell_by_cell() {
        let t = hand();
        let p = Palette::default();
        let d = draw(&t, &p, 62, 24, &view(&t), &utc);
        let fills = [('█', p.accent, 12), ('█', shade(&p), 12), ('░', shade(&p), 12), ('█', p.hot, 12), ('┄', p.faint, 6), ('┄', p.faint, 6)];
        let mut x = 1;
        for (ch, fg, n) in fills {
            for _ in 0..n {
                let c = d.canvas.get(x, 4).unwrap();
                assert_eq!((c.ch, c.fg, c.bg), (ch, fg, p.bg));
                x += 1;
            }
        }
        let y = d.rows.iter().find(|(k, _)| k == "fail").unwrap().1;
        // Axis [20,61), two minutes of a ten minute task ends at ceil(28.2).
        assert_eq!((d.canvas.get(28, y).unwrap().ch, d.canvas.get(28, y).unwrap().fg), ('✗', p.hot));
        assert_eq!(d.canvas.get(37, 8).unwrap().bg, p.panel);
        assert_eq!(d.canvas.get(0, y).unwrap().ch, '▎');
        assert!(row(&d.canvas, 22).starts_with(" ❯ cargo 2m 00s ✗ failed · from 00:00"));
        assert!(d.canvas.get(3, 22).unwrap().bold);
        assert_eq!(d.canvas.get(21, 8).unwrap().ch, ' ');
        assert!(row(&d.canvas, 8).contains('░'));
    }
    #[test]
    fn mono_calm_clock_and_stable_selection() {
        let mut t = demo();
        let p = Palette::default();
        let mut v = view(&t);
        v.mono = true;
        v.calm = true;
        let a = draw(&t, &p, 80, 24, &v, &utc);
        assert!(a.canvas.cells.iter().all(|c| c.fg == p.fg && c.bg == p.bg));
        assert!(!paint(&a.canvas, Color::None).contains('\x1b'));
        assert!(row(&a.canvas, 0).contains('◉'));
        assert!(!a.canvas.cells.iter().any(|c| c.ch == '▌'));
        v.selected = a.selected;
        v.now += 1000.;
        t.spans.reverse();
        let b = draw(&t, &p, 80, 24, &v, &utc);
        assert_eq!(b.selected, v.selected);
        assert_ne!(row(&a.canvas, 22), row(&b.canvas, 22));
    }
    #[test]
    fn overflow_scroll_and_folded_selection() {
        let mut t = hand();
        let s = t.spans[0].clone();
        t.spans.clear();
        for i in 0..20 {
            let mut s = s.clone();
            s.key = format!("open-{i}");
            s.start = i as f64;
            s.end = None;
            t.spans.push(s);
        }
        let mut v = view(&t);
        let a = draw(&t, &Palette::default(), 80, 16, &v, &utc);
        assert_eq!(a.more_rows, 17);
        assert!(a.rows.iter().any(|(k, _)| k == "open-19"));
        v.scroll = 17;
        v.selected = Some("open-0".into());
        let b = draw(&t, &Palette::default(), 80, 16, &v, &utc);
        assert!(b.rows.iter().any(|(k, _)| k == "open-0"));
        let t = demo();
        v = view(&t);
        v.selected = Some("more".into());
        let d = draw(&t, &Palette::default(), 80, 24, &v, &utc);
        assert_eq!(d.selected.as_deref(), Some("more"));
        assert!(row(&d.canvas, 22).contains("more steps"));
        assert!(row(&d.canvas, 22).contains("under a second"));
    }
    #[test]
    fn queue_notice_and_too_small() {
        let mut t = hand();
        t.ended = None;
        t.state = State::Queued;
        t.split = trace::Split { total: 600_000., queue: 600_000., ..trace::Split::default() };
        let d = draw(&t, &Palette::default(), 80, 24, &view(&t), &utc);
        assert!(row(&d.canvas, 8).contains("◔ queued for 10m"));
        assert!(d.rows.is_empty());
        assert_eq!(row(&d.canvas, 4).chars().filter(|c| *c == '┄').count(), 78);
        let d = draw(&t, &Palette::default(), 59, 15, &view(&t), &utc);
        assert!(row(&d.canvas, 2).contains("Enlarge the terminal"));
        let c = notice(&Palette::default(), 60, 16, "Nothing to trace yet.", "Can't read the synthetic source.", &["Open a task in T3.".into()]);
        assert!(row(&c, 2).contains("Nothing to trace yet."));
        assert!(c.to_text().contains("Open a task in T3."));
    }
    #[test]
    fn states_marks_details_and_priority() {
        let p = Palette::default();
        let mut t = hand();
        for (state, mark, word) in [
            (State::Waiting, '◆', "needs you"),
            (State::Delegated, '⇉', "delegates working"),
            (State::Failed, '✗', "failed"),
            (State::Cancelled, '⊘', "stopped"),
            (State::Interrupted, '⊘', "stopped"),
        ] {
            t.state = state;
            let d = draw(&t, &p, 80, 24, &view(&t), &utc);
            assert!(row(&d.canvas, 0).contains(mark));
            assert!(row(&d.canvas, 0).contains(word));
        }
        let mut ask = t.spans[0].clone();
        ask.key = "ask".into();
        ask.kind = Kind::Ask;
        ask.category = "ask".into();
        ask.end = None;
        ask.status = "running".into();
        t.spans.push(ask);
        let mut v = view(&t);
        v.selected = Some("ask".into());
        let d = draw(&t, &p, 120, 40, &v, &utc);
        assert!(row(&d.canvas, 38).contains("waiting on you"));
        assert!(row(&d.canvas, 38).contains("⏎ answer it in T3"));
        let s = t.spans.last_mut().unwrap();
        s.kind = Kind::Delegate;
        s.category = "agent".into();
        s.delegate = Some(trace::Delegate {
            native: false,
            remote: false,
            model: "gpt-6".into(),
            title: Some("Review the task".into()),
            child_thread: Some("child".into()),
        });
        let d = draw(&t, &p, 120, 40, &v, &utc);
        assert!(row(&d.canvas, 38).contains("⇉ gpt-6"));
        assert!(row(&d.canvas, 38).contains("⏎ opens its trace"));
        t.spans.last_mut().unwrap().delegate.as_mut().unwrap().remote = true;
        assert!(row(&draw(&t, &p, 120, 40, &v, &utc).canvas, 38).contains("on another machine"));
        let mut tiny = t.spans[0].clone();
        tiny.key = "tiny".into();
        tiny.end = Some(100.);
        t.spans.push(tiny);
        let d = draw(&t, &p, 80, 24, &view(&t), &utc);
        assert!(!d.rows.iter().any(|(k, _)| k == "tiny"));
        assert!(row(&d.canvas, 9).contains('×'));
        let mut v = view(&t);
        v.selected = Some("model".into());
        assert!(row(&draw(&t, &p, 120, 40, &v, &utc).canvas, 38).contains("between steps, inferred"));
        assert_eq!((rough(45_000.), rough(372_000.), rough(6_720_000.)), ("45s".into(), "6m".into(), "1h 52m".into()));
        let mut tool = trace::demo(0.).unwrap().spans.into_iter().find(|s| s.kind == Kind::Tool).unwrap();
        tool.label = "mcp__t3-code__task_status".into();
        assert_eq!((name(&tool), full_name(&tool)), ("task_status", "t3-code task_status".to_string()));
        assert_eq!((cut("Find the flaky assertion", 10), cut("Run the integration suite", 10)), ("Find the…".to_string(), "Run the i…".to_string()));
        assert_eq!((round_step(1_900.), round_step(110_000.), round_step(7_000_000.)), (2_000., 120_000., 7_200_000.));
        // A live trace drawn a second later keeps every cell of its folded row where it was.
        let task = trace::demo(1_795_000_000_000.).unwrap();
        let at = |now: f64| {
            let view = View { selected: None, scroll: 0, calm: true, mono: true, spin: 0, demo: true, now };
            row(&draw(&task, &Palette::default(), 80, 24, &view, &utc).canvas, 9)
        };
        assert_eq!(at(task.now + 1000.), at(task.now + 2000.));
        assert_eq!(duration(45_000.), "45s");
        assert_eq!(duration(372_000.), "6m 12s");
        assert_eq!(duration(1_500_000.), "25m");
        assert_eq!(duration(6_720_000.), "1h 52m");
        assert_eq!(allot(10, &[9999., 1., 1., 1., 1., 1.]).iter().sum::<usize>(), 10);
        assert!(allot(10, &[9999., 1., 1., 1., 1., 1.]).iter().all(|n| *n >= 1));
    }
    #[test]
    #[ignore = "writes review evidence and measures pure draw; run explicitly in release"]
    fn evidence_and_measure() {
        let t = demo();
        let v = View { demo: true, ..view(&t) };
        let p = crate::integration::derive_themed(crate::theme::builtin("frontier").unwrap().text);
        let dir = std::path::Path::new("../../checks/generated/trace");
        std::fs::create_dir_all(dir).unwrap();
        for (w, h) in [(80, 24), (120, 40), (60, 24), (190, 52)] {
            let c = draw(&t, &p, w, h, &v, &utc).canvas;
            std::fs::write(dir.join(format!("demo-{w}x{h}.ansi")), paint(&c, Color::True)).unwrap();
        }
        for (name, key) in [("model", "model"), ("more", "more")] {
            let v = View { selected: Some(key.into()), ..v.clone() };
            std::fs::write(dir.join(format!("{name}.ansi")), paint(&draw(&t, &p, 80, 24, &v, &utc).canvas, Color::True)).unwrap();
        }
        let mut samples = [vec![], vec![]];
        for _ in 0..7 {
            for (i, (w, h)) in [(190, 52), (80, 24)].into_iter().enumerate() {
                let start = std::time::Instant::now();
                for _ in 0..1000 {
                    let d = draw(std::hint::black_box(&t), &p, w, h, &v, &utc);
                    assert!(d.selected.is_some());
                    std::hint::black_box(d);
                }
                samples[i].push(start.elapsed().as_secs_f64() * 1_000_000. / 1000.);
            }
        }
        for (i, values) in samples.iter_mut().enumerate() {
            println!("{} draw us per call, trial order: {values:?}", if i == 0 { "190x52" } else { "80x24" });
            values.sort_by(f64::total_cmp);
            println!("median {:.3}, range {:.3}..{:.3}", values[3], values[0], values[6]);
        }
    }
    #[test]
    fn a_long_quiet_stretch_folds_to_a_labelled_marker() {
        // Two hours of work, fourteen quiet hours, two more hours of work.
        let mut t = trace::demo(1_795_000_000_000.).unwrap();
        let hour = 3_600_000.;
        let (lo, gap) = (t.requested, t.requested + 2. * hour);
        t.quiet = vec![(gap, gap + 14. * hour)];
        t.ended = Some(lo + 18. * hour);
        t.split.total = 18. * hour;
        let view = View { selected: None, scroll: 0, calm: true, mono: true, spin: 0, demo: true, now: lo + 18. * hour };
        let d = draw(&t, &Palette::default(), 80, 24, &view, &utc);
        let axis = row(&d.canvas, 7);
        assert!(axis.contains("┊14h┊"), "{axis}");
        // The fold is narrow: the four hours of work keep almost all of the axis.
        let a = Axis::new(22, 57, lo, lo + 18. * hour, &t.quiet);
        assert_eq!(a.folds().map(|(_, w, l)| (w, l.to_string())).collect::<Vec<_>>(), [(5, "14h".to_string())]);
        assert!((a.at(gap) - (22. + 26.)).abs() < 0.01, "{}", a.at(gap));
        assert!((a.at(gap + 14. * hour) - (22. + 31.)).abs() < 0.01);
        assert!(a.folded(gap + hour) && !a.folded(gap - hour));
        assert!(a.time_of(48).is_none() && a.time_of(47).is_some());
        // Without quiet stretches the axis is true time from end to end.
        let plain = Axis::new(22, 57, lo, lo + 18. * hour, &[]);
        assert!((plain.at(lo + 9. * hour) - (22. + 28.5)).abs() < 0.01);
        assert_eq!(plain.folds().count(), 0);
    }
    #[test]
    fn an_ask_that_holds_nobody_is_drawn_dim_and_says_why() {
        // The demo's question, made answerable by message and open: not waiting.
        let mut t = trace::demo(1_795_000_000_000.).unwrap();
        let q = t.spans.iter().position(|s| s.label == "question").unwrap();
        let (start, end) = (t.spans[q].start, t.spans[q].end.unwrap());
        t.spans[q].by_message = true;
        t.spans[q].end = None;
        t.spans[q].holds.clear();
        t.waiting.retain(|(a, b)| *b <= start || *a >= end);
        let key = t.spans[q].key.clone();
        let view = View { selected: Some(key.clone()), scroll: 0, calm: true, mono: false, spin: 0, demo: true, now: t.now };
        let p = Palette::default();
        let d = draw(&t, &p, 120, 40, &view, &utc);
        let y = d.rows.iter().find(|(k, _)| *k == key).unwrap().1;
        let words = row(&d.canvas, 38);
        assert!(words.contains("question open") && words.contains("not counted as waiting"), "{words}");
        // Its bar: dim (lifted only where the selection needs it), never the hot of waiting.
        let bar: Vec<_> = (31..118).filter_map(|x| d.canvas.get(x, y)).filter(|c| c.ch == '━').collect();
        assert!(!bar.is_empty() && bar.iter().all(|c| c.fg != p.hot));
        // The same question holding the run is hot where it waits.
        let t2 = trace::demo(1_795_000_000_000.).unwrap();
        let view2 = View { selected: None, ..view };
        let d2 = draw(&t2, &p, 120, 40, &view2, &utc);
        let y2 = d2.rows.iter().find(|(k, _)| *k == key).unwrap().1;
        assert!((31..118).filter_map(|x| d2.canvas.get(x, y2)).any(|c| c.ch == '━' && c.fg == p.hot));
    }
    #[test]
    fn every_bar_reads_at_3_to_1_in_every_builtin_theme() {
        // The demo drawn with a delegate selected, so bars sit on the page, the no-run band and
        // the selection: every bar mark keeps 3:1 against the cell it is drawn on.
        let task = trace::demo(1_795_000_000_000.).unwrap();
        let delegate = task.spans.iter().find(|s| s.kind == Kind::Delegate && s.delegate.as_ref().is_some_and(|d| d.remote)).unwrap().key.clone();
        let mut checked = 0;
        for b in crate::theme::BUILTINS {
            let p = crate::integration::derive_themed(b.text);
            let view = View { selected: Some(delegate.clone()), scroll: 0, calm: true, mono: false, spin: 0, demo: true, now: task.now };
            let d = draw(&task, &p, 120, 40, &view, &utc);
            for y in 8..36 {
                for x in 0..120 {
                    let c = d.canvas.get(x, y).unwrap();
                    if BARS.contains(&c.ch) && c.ch != '░' && c.ch != '▁' && c.ch != '▂' && c.ch != '▃' && c.ch != '▄' {
                        let ratio = crate::integration::contrast(c.fg, c.bg);
                        assert!(ratio >= 3., "{}: {:?} at {x},{y} reads {ratio:.2}:1", b.name, c.ch);
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 1000, "{checked} bar cells checked");
    }

}

/// Trace's size floor applies even while its task is loading or unavailable.
pub fn small(p: &Palette, cols: usize, rows: usize) -> Canvas {
    let mut c = Canvas::new(cols, rows);
    c.clear(p.bg, p.fg);
    c.text_bold(1, 0, "T3 Agents", p.bright);
    c.text(1, 2, if cols < 17 { "Enlarge terminal" } else if cols < 42 { "Minimum: 60 × 16" } else { "Enlarge the terminal to at least 60 × 16." }, p.dim);
    c.text(1, 3, "w back   q quit", p.accent);
    c
}

/// Contextual help over the current Trace, keeping its header visible.
pub fn help(c: &mut Canvas, p: &Palette) {
    c.fill(0, 1, c.w as i32, c.h as i32 - 1, p.bg);
    terminal::section(c, 1, 1, c.w as i32 - 2, "TRACE HELP", p, "");
    if c.w < 60 || c.h < 16 {
        c.text(1, 2, "Esc / ?  close help", p.accent);
        c.text(1, 3, "w / Esc  back from Trace", p.text2);
        c.text(1, 4, "q        quit", p.text2);
        return;
    }
    for (i, (key, label)) in [
        ("↑↓ / j k", "select a step · mouse wheel selects"),
        ("J K / PgUp PgDn", "scroll steps"),
        ("g G / Home End", "select first / last step"),
        ("←→", "previous / next task"),
        ("⏎ / o", "open a delegate trace or its thread in T3"),
        ("w / Esc", "back · Esc closes help first"),
        ("? / h", "close help"),
        ("m p r c C", "calm · pause · refresh · themes"),
        ("q", "quit"),
    ].into_iter().enumerate() {
        c.text_bold(2, 2 + i as i32, key, p.accent);
        c.text(18, 2 + i as i32, label, p.text2);
    }
}
