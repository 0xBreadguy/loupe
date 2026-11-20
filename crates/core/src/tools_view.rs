//! Tools, drawn: the 80-column report, the 120-column report, their quiet states, and a plain
//! list for a narrow terminal, built from Recap's own parts. The frames it must equal, line
//! for line, are in `checks/fixtures/tools/`. `Source` and `Size` are Recap's: a report says where its numbers came from the same
//! way a card does, and `--wide` / `--compact` are the same two sizes.
use crate::{
    model, recap,
    recap_view::{self, Pen},
    terminal::{self, Canvas, Palette, Rgb},
    tools::{self, Exits, Kind, Long, Row, Tools},
};

/// Where the numbers came from. Recap's own enum: a report says it the same way a card does.
pub use recap_view::Source;

pub const REPORT_COLUMNS: usize = 80;
pub const WIDE_COLUMNS: usize = 120;

const COMMAND: &str = "t3-agents --tools";
const EXAMPLE: &str = "\"cd app && cargo test | tail\"";
const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
const BUCKET_WORDS: [&str; 5] = ["under 1s", "1–10s", "10–60s", "1–10 min", "over 10 min"];
/// Why a command was marked failed, in the order two equal counts keep. `no exit code` is always last.
const WHY: &[(&str, &str, &str)] = &[
    ("one", "exit 1", "an error, or a test that failed"),
    ("found_nothing", "found nothing", "rg or grep, exit 1"),
    ("files_differ", "files differ", "diff, exit 1"),
    ("two", "exit 2", "often a wrong option or a missing file"),
    ("timed_out", "timed out", "timeout stopped it, exit 124"),
    ("not_runnable", "could not run", "found but not runnable, exit 126"),
    ("not_found", "not installed", "no such program, exit 127"),
    ("signal", "stopped", "interrupted, killed or crashed"),
    ("other", "another code", "any code not listed here"),
    ("zero", "exit 0", "reported success, marked failed"),
];

struct WhyRow {
    key: &'static str,
    label: &'static str,
    count: u32,
    note: Option<&'static str>,
}

#[derive(Clone, Copy)]
struct Grid {
    glyph: i32,
    name: i32,
    name_w: usize,
    calls: i32,
    calls_bar: i32,
    bar_w: i32,
    time: i32,
    time_bar: i32,
    failed: i32,
    rate: i32,
    typical: Option<i32>,
    longest: Option<i32>,
}

const G80: Grid = Grid {
    glyph: 1, name: 3, name_w: 20, calls: 31, calls_bar: 32, bar_w: 7, time: 46, time_bar: 47, failed: 61, rate: 63, typical: None, longest: None,
};
const G120: Grid = Grid {
    glyph: 1, name: 3, name_w: 34, calls: 45, calls_bar: 46, bar_w: 12, time: 65, time_bar: 66, failed: 103, rate: 105, typical: Some(86), longest: Some(95),
};

/// The 80-column report.
pub fn report(tools: &Tools, palette: &Palette, source: Source, mono: bool) -> Canvas {
    draw(tools, palette, source, mono, false)
}

/// The 120-column report. Chosen by columns alone: the report is taller than a screen either way.
pub fn wide(tools: &Tools, palette: &Palette, source: Source, mono: bool) -> Canvas {
    draw(tools, palette, source, mono, true)
}

/// A notice in Recap's box, with this report's name in the header. It ends on the blank row under the panel.
pub fn notice(p: &Palette, period: &str, state: &str, alarm: bool, message: &str, tips: &[String]) -> Canvas {
    let mut canvas = recap_view::notice(p, period, state, alarm, message, tips);
    // Recap's header is the same width: `TOOLS` sits where `RECAP` did, and `tools` where `recap` did.
    canvas.text_bold(12, 0, "TOOLS", p.text2);
    canvas.text(74, 0, "tools", p.dim);
    canvas
}

fn draw(tools: &Tools, palette: &Palette, source: Source, mono: bool, wide: bool) -> Canvas {
    let columns = if wide { WIDE_COLUMNS } else { REPORT_COLUMNS };
    // Blank rows between the notes, and the `no name` line, sit past the round-2 height. `finish` crops.
    let rows = 110 + tools.kinds.len() + tools.programs.len() + tools.tools.len() + tools.providers.len() + tools.longest.len();
    let mut pen = Pen::new(palette, columns, rows, mono);
    let period = if wide { recap_view::period_with_year(tools.period, &tools.from, &tools.to) } else { recap_view::period_text(tools.period, &tools.from, &tools.to) };
    header(&mut pen, &period, &coverage(tools, source), palette.dim);
    tiles(&mut pen, tools, wide);
    let (x, w) = (1, columns as i32 - 2);
    let grid = if wide { G120 } else { G80 };
    let mut y = lengths_block(&mut pen, tools, x, 8, w, if wide { 22 } else { 14 }, if wide { 14 } else { 7 }) + 1;
    y = kinds_block(&mut pen, tools, &grid, x, y, w) + 1;
    y = programs_block(&mut pen, tools, &grid, x, y, w, if wide { 10 } else { 7 }, if wide { 5 } else { 4 }, if wide { 3 } else { 2 }) + 1;
    if tools.commands.calls > 0 {
        y = why_block(&mut pen, tools, x, y, w, wide) + 1;
    }
    y = tools_block(&mut pen, tools, &grid, x, y, w, if wide { 6 } else { 4 }, if wide { 4 } else { 3 }, wide) + 1;
    y = providers_block(&mut pen, tools, x, y, w, wide) + 1;
    y = longest_block(&mut pen, tools, x, y, w, if wide { 10 } else { 5 }, wide) + 1;
    y = counted_block(&mut pen, tools, x, y, w);
    finish(pen, y)
}

fn finish(mut pen: Pen<'_>, rows: i32) -> Canvas {
    let h = (rows.max(1) as usize).min(pen.c.h);
    pen.c.h = h;
    pen.c.cells.truncate(h * pen.c.w);
    pen.c.unclip();
    pen.c
}

fn header(pen: &mut Pen, period: &str, state: &str, state_color: Rgb) {
    let p = pen.p;
    let x = pen.runs(1, 0, &[("T3 Agents", p.fg, true), ("  ", p.dim, false), ("TOOLS", p.text2, true), ("  ", p.dim, false), (period, p.fg, false), ("  ", p.dim, false)]);
    pen.c.text(x, 0, state, state_color);
    pen.c.right(pen.c.w as i32 - 1, 0, COMMAND, p.dim);
}

/// Recap's coverage with `roomy` false: the wide month's "so far, to …" is not this report's.
pub(crate) fn coverage(tools: &Tools, source: Source) -> String {
    let days = tools.days_recorded;
    let recorded = format!("{days} {} recorded", if days == 1 { "day" } else { "days" });
    match source {
        Source::Demo => "demo data · made up".into(),
        Source::Records if tools.so_far => format!("so far · {recorded}"),
        Source::Records => format!("{} of {} days recorded", tools.days_recorded, tools.days),
    }
}

fn tiles(pen: &mut Pen, tools: &Tools, wide: bool) {
    recap_view::tile(pen, 0, if wide { 29 } else { 26 }, "CALLS", &recap_view::tile_count(u64::from(tools.calls)), false, &calls_note(tools));
    let (call_x, call_w) = if wide { (30, 29) } else { (27, 26) };
    let note = clock_note(tools);
    if u64::from(tools.calls) == untimed(u64::from(tools.open), u64::from(tools.closed_together)) {
        recap_view::tile(pen, call_x, call_w, "CALL TIME", "unknown", true, &note);
    } else if tools.ms <= 0.0 {
        recap_view::tile(pen, call_x, call_w, "CALL TIME", "<1s", true, &note);
    } else {
        let (digits, unit) = hours_big(tools.ms);
        recap_view::tile(pen, call_x, call_w, "CALL TIME", &format!("{digits}{unit}"), false, &note);
    }
    let (x3, w3) = if wide { (60, 29) } else { (54, 26) };
    if tools.failed == 0 {
        let word = if tools.calls == 1 { "call" } else { "calls" };
        recap_view::tile(pen, x3, w3, "FAILED", "none", true, &format!("of {} {word}", count(tools.calls)));
    } else {
        recap_view::tile(pen, x3, w3, "FAILED", &recap_view::tile_count(u64::from(tools.failed)), false, &failed_note(tools));
    }
    if !wide {
        return;
    }
    if tools.longest.is_empty() {
        recap_view::tile(pen, 90, 30, "LONGEST CALL", "unknown", true, "no length is known");
        return;
    }
    let call = &tools.longest[0];
    let who = if call.provider.is_empty() || call.provider == "unknown" { "not recorded".to_owned() } else { mid(&call.provider, 12) };
    let room = (30 - 3 - terminal::width(&format!(" · {who}")) as i32).max(0) as usize;
    let note = format!("{} · {who}", mid(&long_label(call.kind, call.name.as_deref()), room));
    recap_view::tile(pen, 90, 30, "LONGEST CALL", &span_ms(Some(call.ms)), false, &note);
}

fn calls_note(tools: &Tools) -> String {
    let cmd = tools.commands.calls;
    if cmd == tools.calls {
        "all of them commands".into()
    } else if cmd == 0 {
        "none of them commands".into()
    } else {
        format!("{} of them {}", count(cmd), if cmd == 1 { "a command" } else { "commands" })
    }
}

fn untimed(open: u64, closed: u64) -> u64 {
    open + closed
}

fn clock_note(tools: &Tools) -> String {
    let uncovered = untimed(u64::from(tools.open), u64::from(tools.closed_together));
    if u64::from(tools.calls) == uncovered {
        return "no length is known".into();
    }
    if tools.closed_together > 0 {
        let word = if uncovered == 1 { "call" } else { "calls" };
        return format!("leaves out {} {word}", count_u(uncovered));
    }
    if tools.busy_ms >= tools.ms {
        return "no calls overlapped".into();
    }
    if time_text(tools.busy_ms) == time_text(tools.ms) {
        return "the same on the clock".into();
    }
    format!("{} on the clock", time_text(tools.busy_ms))
}

fn failed_note(tools: &Tools) -> String {
    if tools.calls < tools.min_for_rate {
        format!("of {} {}", count(tools.calls), if tools.calls == 1 { "call" } else { "calls" })
    } else {
        format!("{} of calls", share(f64::from(tools.failed), f64::from(tools.calls)))
    }
}

fn lengths_block(pen: &mut Pen, tools: &Tools, x: i32, y: i32, w: i32, pitch: i32, bar: i32) -> i32 {
    pen.section(x, y, w, "HOW LONG THEY TOOK", &minute_text(tools));
    let x0 = x + 8;
    for (i, word) in BUCKET_WORDS.iter().enumerate() {
        pen.c.text(x0 + pitch * i as i32, y + 1, word, pen.p.dim);
    }
    let ended: u64 = tools.lengths.iter().map(|b| u64::from(b.calls)).sum();
    let calls_color = calls_ink(pen);
    for (row, (label, time, whole)) in [("calls", false, ended as f64), ("time", true, tools.ms)].into_iter().enumerate() {
        let yy = y + 2 + row as i32;
        pen.c.text(x, yy, label, pen.p.text2);
        for (i, bucket) in tools.lengths.iter().enumerate() {
            let bx = x0 + pitch * i as i32;
            let part = if time { bucket.ms } else { f64::from(bucket.calls) };
            let frac = if whole > 0.0 { part / whole } else { 0.0 };
            if part > 0.0 && whole > 0.0 {
                hbar(&mut pen.c, bx, yy, bar, frac, if time { pen.p.accent } else { calls_color });
            }
            let big = part > 0.0 && whole > 0.0 && frac >= 0.3;
            let ink = if part > 0.0 { pen.p.fg } else { pen.p.dim };
            pen.c.right_with(bx + bar + 5, yy, &share(part, whole), ink, None, big);
        }
    }
    y + 4
}

fn minute_text(tools: &Tools) -> String {
    let (ended, n, ms) = minutes(tools);
    if ended == 0 {
        "no call has a known length".into()
    } else if n == 0 {
        "no call took a minute or more".into()
    } else {
        format!("{} of calls took a minute or more: {} of the time", share(n as f64, ended as f64), share(ms, tools.ms))
    }
}

fn minute_list(tools: &Tools) -> String {
    let (ended, n, ms) = minutes(tools);
    if n == 0 {
        "no call took a minute or more".into()
    } else {
        format!("{} of calls a minute or more: {} of the time", share(n as f64, ended as f64), share(ms, tools.ms))
    }
}

/// Buckets 4 and 5 (indexes 3 and 4): calls of a minute or more, and the calls that ended.
fn minutes(tools: &Tools) -> (u64, u32, f64) {
    let ended = tools.lengths.iter().map(|b| u64::from(b.calls)).sum();
    let (c3, m3) = bucket(tools, 3);
    let (c4, m4) = bucket(tools, 4);
    (ended, c3.saturating_add(c4), m3 + m4)
}

fn bucket(tools: &Tools, index: usize) -> (u32, f64) {
    tools.lengths.get(index).map(|b| (b.calls, b.ms)).unwrap_or((0, 0.0))
}

fn kinds_block(pen: &mut Pen, tools: &Tools, g: &Grid, x: i32, y: i32, w: i32) -> i32 {
    pen.section(x, y, w, "BY KIND", "by time");
    table_heads(pen, g, y + 1);
    let (top_c, top_m) = tops(&tools.kinds);
    for (i, row) in tools.kinds.iter().enumerate() {
        let label = vec![(kind_word(row.kind).to_owned(), pen.p.fg)];
        table_row(pen, g, y + 2 + i as i32, row, &label, top_c, top_m, tools.min_for_rate, Some(kind_glyph(row.kind)));
    }
    y + 2 + tools.kinds.len() as i32
}

fn programs_block(pen: &mut Pen, tools: &Tools, g: &Grid, x: i32, y: i32, w: i32, by_time: usize, by_calls: usize, by_failed: usize) -> i32 {
    let rows: Vec<&Row> = tools.programs.iter().collect();
    let unnamed = rows.iter().copied().find(|row| row.name.is_none());
    let (head, rest) = shown(&rows, by_time, by_calls, unnamed, by_failed);
    pen.section(x, y, w, "COMMANDS BY PROGRAM", &format!("{} programs on the list · by time", count(tools.commands.listed)));
    if head.is_empty() {
        pen.c.text(x + 2, y + 1, "no commands", pen.p.dim);
        return y + 2;
    }
    table_heads(pen, g, y + 1);
    let (top_c, top_m) = tops_ref(&head);
    for (i, row) in head.iter().enumerate() {
        let label = match row.name.as_deref() {
            None => vec![("not on the list".to_owned(), pen.p.text2)],
            Some(name) => vec![(terminal::fit(name, g.name_w), pen.p.fg)],
        };
        table_row(pen, g, y + 2 + i as i32, row, &label, top_c, top_m, tools.min_for_rate, None);
    }
    let mut yy = y + 2 + head.len() as i32;
    if !rest.is_empty() {
        fold_row(pen, g, yy, &rest, tools.min_for_rate);
        yy += 1;
    }
    let inside = inside_others(tools);
    if !inside.is_empty() {
        let mut xx = pen.c.text(g.name, yy, "also inside other command lines", pen.p.dim) + g.name + 2;
        for (i, (name, n)) in inside.iter().enumerate() {
            let counted = format!(" {}", count(*n));
            let need = terminal::width(name) + terminal::width(&counted) + if i == 0 { 0 } else { 3 };
            if xx + need as i32 > x + w {
                break;
            }
            let (faint, text2, fg) = (pen.p.faint, pen.p.text2, pen.p.fg);
            if i > 0 {
                xx = pen.runs(xx, yy, &[(" · ", faint, false)]);
            }
            xx = pen.runs(xx, yy, &[(name, text2, false), (counted.as_str(), fg, false)]);
        }
        yy += 1;
    }
    yy
}

fn program_part(row: &Row, key: &str) -> u32 {
    let name = row.name.as_deref();
    let search = name.is_some_and(|n| tools::FOUND_NOTHING.contains(&n));
    let differ = name.is_some_and(|n| tools::FILES_DIFFER.contains(&n));
    match key {
        "no_code" => row.failed.saturating_sub(row.exits.failed_known),
        "found_nothing" => if search { row.exits.one } else { 0 },
        "files_differ" => if differ { row.exits.one } else { 0 },
        "one" => if search || differ { 0 } else { row.exits.one },
        _ => exit_count(&row.exits, key),
    }
}

fn exit_count(exits: &Exits, key: &str) -> u32 {
    match key {
        "zero" => exits.zero,
        "one" => exits.one,
        "two" => exits.two,
        "timed_out" => exits.timed_out,
        "not_runnable" => exits.not_runnable,
        "not_found" => exits.not_found,
        "signal" => exits.signal,
        "other" => exits.other,
        _ => 0,
    }
}

fn why_rows(tools: &Tools) -> Vec<WhyRow> {
    let exits = &tools.commands.exits;
    let found = tools.programs.iter().fold(0u32, |sum, row| sum.saturating_add(program_part(row, "found_nothing")));
    let differ = tools.programs.iter().fold(0u32, |sum, row| sum.saturating_add(program_part(row, "files_differ")));
    let mut rows: Vec<WhyRow> = WHY.iter().filter_map(|&(key, label, note)| {
        let count = if key == "one" { exits.one.saturating_sub(found).saturating_sub(differ) } else if key == "found_nothing" { found } else if key == "files_differ" { differ } else { exit_count(exits, key) };
        (count > 0).then_some(WhyRow { key, label, count, note: Some(note) })
    }).collect();
    rows.sort_by(|a, b| b.count.cmp(&a.count));
    let missing = tools.commands.failed.saturating_sub(exits.failed_known);
    if missing > 0 {
        rows.push(WhyRow { key: "no_code", label: "no exit code", count: missing, note: None });
    }
    rows
}

fn who_lacks(tools: &Tools) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = tools.providers.iter().filter_map(|provider| {
        let lack = provider.commands_failed.saturating_sub(provider.exits.failed_known);
        (lack > 0).then(|| (recorded_name(&provider.provider).to_owned(), lack))
    }).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

fn programs_for(tools: &Tools, key: &str) -> Vec<(String, u32)> {
    let mut out: Vec<(String, u32)> = tools.programs.iter().filter_map(|row| {
        let count = program_part(row, key);
        (count > 0).then(|| (row.name.clone().unwrap_or_else(|| "not on the list".into()), count))
    }).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then((a.0 == "not on the list").cmp(&(b.0 == "not on the list"))).then(a.0.cmp(&b.0)));
    out
}

fn draw_items(pen: &mut Pen, mut x: i32, y: i32, end: i32, pairs: &[(String, u32)]) {
    for (i, (name, n)) in pairs.iter().enumerate() {
        let counted = format!(" {}", count(*n));
        let need = terminal::width(name) + terminal::width(&counted) + if i == 0 { 0 } else { 3 };
        if x + need as i32 > end {
            break;
        }
        let (faint, text2, fg) = (pen.p.faint, pen.p.text2, pen.p.fg);
        if i > 0 {
            x = pen.runs(x, y, &[(" · ", faint, false)]);
        }
        x = pen.runs(x, y, &[(name.as_str(), text2, false), (counted.as_str(), fg, false)]);
    }
}

fn lacking_sentences(tools: &Tools) -> Vec<String> {
    let mut out = Vec::new();
    for provider in &tools.providers {
        let lack = provider.commands_failed.saturating_sub(provider.exits.failed_known);
        if provider.provider.is_empty() || provider.provider == "unknown" || lack == 0 || u64::from(lack) * 2 <= u64::from(provider.commands_failed) {
            continue;
        }
        let known = provider.exits.known;
        let have = if known == 0 { "none has".to_owned() } else if known == 1 { "1 has".to_owned() } else { format!("{} have", count(known)) };
        let word = if provider.commands == 1 { "command" } else { "commands" };
        out.push(format!("Of {}'s {} {word}, {have} an exit code.", provider.provider, count(provider.commands)));
    }
    out
}

fn fact_sentence(tools: &Tools) -> Option<String> {
    let exits = &tools.commands.exits;
    if exits.known == 0 {
        return None;
    }
    if exits.zero == 0 && exits.passed_nonzero == 0 {
        return Some("Where a code was recorded, a command was marked failed exactly when its code was not 0.".into());
    }
    let mut parts = Vec::new();
    if exits.zero > 0 {
        parts.push(format!("{} marked failed had code 0", count(exits.zero)));
    }
    if exits.passed_nonzero > 0 {
        parts.push(format!("{} not marked failed had another code", count(exits.passed_nonzero)));
    }
    Some(format!("Where a code was recorded, {}.", parts.join(" and ")))
}

fn why_paragraph(tools: &Tools) -> Vec<String> {
    let commands = &tools.commands;
    if commands.failed == 0 {
        return vec!["No command was marked failed.".into()];
    }
    if commands.exits.failed_known == 0 {
        let lead = if commands.failed == 1 {
            "The one command marked failed has no exit code, so why it failed is not known.".into()
        } else {
            format!("None of the {} commands marked failed has an exit code, so why they failed is not known.", count(commands.failed))
        };
        let mut lines = vec![lead];
        lines.extend(lacking_sentences(tools));
        return lines;
    }
    let mut lines = lacking_sentences(tools);
    if let Some(fact) = fact_sentence(tools) {
        lines.push(fact);
    }
    lines
}

fn why_block(pen: &mut Pen, tools: &Tools, x: i32, y: i32, w: i32, wide: bool) -> i32 {
    let commands = &tools.commands;
    if commands.calls == 0 {
        return y;
    }
    let rows = if commands.exits.failed_known > 0 { why_rows(tools) } else { Vec::new() };
    let right = if rows.is_empty() { String::new() } else { format!("{} of the {} have an exit code", count(commands.exits.failed_known), count(commands.failed)) };
    pen.section(x, y, w, "WHY COMMANDS FAILED", &right);
    let mut yy = y + 1;
    let bar = if wide { 12 } else { 7 };
    let top = rows.iter().map(|row| row.count).max().unwrap_or(1).max(1);
    let calls_color = calls_ink(pen);
    for row in &rows {
        pen.c.text(x + 2, yy, row.label, if row.key == "no_code" { pen.p.text2 } else { pen.p.fg });
        pen.c.right(31, yy, &count(row.count), pen.p.text2);
        hbar(&mut pen.c, 32, yy, bar, f64::from(row.count) / f64::from(top), calls_color);
        let nx = if wide { 46 } else { 41 };
        if row.key == "no_code" {
            draw_items(pen, nx, yy, nx + 38, &who_lacks(tools));
        } else if let Some(note) = row.note {
            let fitted = terminal::fit(note, 38);
            pen.c.text(nx, yy, &fitted, pen.p.dim);
        }
        if wide {
            draw_items(pen, 86, yy, x + w, &programs_for(tools, row.key));
        }
        yy += 1;
    }
    let paragraph = why_paragraph(tools).join(" ");
    for (line, _) in wrap_indexed(&paragraph, (w - 2) as usize) {
        pen.c.text(x + 2, yy, &line, pen.p.dim);
        yy += 1;
    }
    yy
}

fn tools_block(pen: &mut Pen, tools: &Tools, g: &Grid, x: i32, y: i32, w: i32, by_time: usize, by_calls: usize, with_server: bool) -> i32 {
    let (named, nameless) = tool_parts(tools);
    let (head, rest) = shown(&named, by_time, by_calls, None, 0);
    let n = named.iter().filter(|row| row.name.is_some()).count();
    let right = if n == 0 { String::new() } else { format!("{n} {} · by time", if n == 1 { "tool" } else { "tools" }) };
    pen.section(x, y, w, "TOOLS BY NAME", &right);
    if head.is_empty() && nameless.is_empty() {
        pen.c.text(x + 2, y + 1, "no other tools", pen.p.dim);
        return y + 2;
    }
    let mut yy = y + 1;
    if !head.is_empty() {
        table_heads(pen, g, yy);
        let (top_c, top_m) = tops_ref(&head);
        for (i, row) in head.iter().enumerate() {
            let label = label_of(row, &head, g.name_w, with_server, pen.p);
            table_row(pen, g, yy + 1 + i as i32, row, &label, top_c, top_m, tools.min_for_rate, Some(kind_glyph(row.kind)));
        }
        yy += 1 + head.len() as i32;
        if !rest.is_empty() {
            fold_row(pen, g, yy, &rest, tools.min_for_rate);
            yy += 1;
        }
    }
    if !nameless.is_empty() {
        let xx = pen.c.text(g.name, yy, "no name", pen.p.dim) + g.name + 2;
        let pairs: Vec<(String, u32)> = nameless.iter().map(|row| (unnamed_tool(row.kind).to_owned(), row.calls)).collect();
        draw_items(pen, xx, yy, x + w, &pairs);
        yy += 1;
    }
    yy
}

/// Rows the tools table may draw (a name, or `unnamed tools`), and the nameless kinds with a call.
fn tool_parts(tools: &Tools) -> (Vec<&Row>, Vec<&Row>) {
    let rows: Vec<&Row> = tools.tools.iter().filter(|row| row.calls > 0).collect();
    let named = rows.iter().copied().filter(|row| !(row.name.is_none() && nameless_kind(row.kind))).collect();
    let mut nameless: Vec<&Row> = rows.into_iter().filter(|row| row.name.is_none() && nameless_kind(row.kind)).collect();
    nameless.sort_by(|a, b| b.calls.cmp(&a.calls).then(nameless_order(a.kind).cmp(&nameless_order(b.kind))));
    (named, nameless)
}

fn nameless_kind(kind: Kind) -> bool {
    matches!(kind, Kind::Edit | Kind::Search | Kind::Web)
}

fn nameless_order(kind: Kind) -> u8 {
    match kind {
        Kind::Edit => 0,
        Kind::Search => 1,
        Kind::Web => 2,
        _ => 3,
    }
}

fn providers_block(pen: &mut Pen, tools: &Tools, x: i32, y: i32, w: i32, wide: bool) -> i32 {
    pen.section(x, y, w, "PROVIDERS", "each decides for itself what failed means");
    let (name_w, cmd_end, fail_end, all_end) = if wide { (24, 36, 53, 91) } else { (12, 24, 41, 79) };
    let mut extra = 0i32;
    for (i, provider) in tools.providers.iter().enumerate() {
        let yy = y + 1 + i as i32 + extra;
        provider_runs(pen, x, yy, &provider.provider, name_w);
        if provider.commands == 0 {
            pen.c.right(cmd_end, yy, "no commands", pen.p.dim);
        } else {
            pen.c.right_with(cmd_end, yy, &count(provider.commands), pen.p.fg, None, true);
            pen.c.text(cmd_end, yy, if provider.commands == 1 { " command" } else { " commands" }, pen.p.text2);
            let (failed, ink) = if provider.commands_failed == 0 { ("none".to_owned(), pen.p.dim) } else { (count(provider.commands_failed), pen.p.text2) };
            pen.c.right(fail_end, yy, &failed, ink);
            pen.c.text(fail_end, yy, " marked failed", ink);
        }
        let time = if provider.ms > 0.0 { time_text(provider.ms) } else { "unknown".into() };
        let summary = format!("{} {} · {time}", count(provider.calls), if provider.calls == 1 { "call" } else { "calls" });
        pen.c.right(all_end, yy, &summary, pen.p.dim);
        if provider.closed_together > 0 {
            let words = terminal::fit(&closed_words(provider.closed_together), (w - 4).max(0) as usize);
            let (rule, dim) = (pen.p.rule, pen.p.dim);
            pen.runs(x + 2, yy + 1, &[("└ ", rule, false), (words.as_str(), dim, false)]);
            extra += 1;
        }
    }
    y + 1 + tools.providers.len() as i32 + extra
}

fn closed_words(n: u32) -> String {
    if n == 1 {
        "1 call marked finished together with others: its time is not known".into()
    } else {
        format!("{} calls marked finished all at once: their time is not known", count(n))
    }
}

fn longest_block(pen: &mut Pen, tools: &Tools, x: i32, y: i32, w: i32, n: usize, with_server: bool) -> i32 {
    let (name_w, len_end, prov_x, when_x, failed_x, thread_end) = if with_server { (34, 45, 47, 63, 79, 119) } else { (18, 27, 28, 43, 57, 79) };
    let rows: Vec<&Long> = tools.longest.iter().take(n).collect();
    let word = if tools.period == recap::Span::Month { "month" } else { "week" };
    if rows.is_empty() {
        pen.section(x, y, w, "LONGEST CALLS", "");
        pen.c.text(x + 2, y + 1, "No call has a known length.", pen.p.dim);
        return y + 2;
    }
    let right = if rows.len() > 1 { format!("the {} longest of the {word}", rows.len()) } else { format!("the longest of the {word}") };
    pen.section(x, y, w, "LONGEST CALLS", &right);
    pen.c.right(len_end, y + 1, "length", pen.p.dim);
    pen.c.text(prov_x, y + 1, "provider", pen.p.dim);
    pen.c.text(when_x, y + 1, "started", pen.p.dim);
    pen.c.right(thread_end, y + 1, "thread", pen.p.dim);
    for (i, call) in rows.iter().enumerate() {
        let yy = y + 2 + i as i32;
        pen.c.set(x, yy, kind_glyph(call.kind), pen.p.text2, None, false);
        let label = long_runs(call, name_w, with_server, pen.p);
        let runs: Vec<(&str, Rgb, bool)> = label.iter().map(|(s, c)| (s.as_str(), *c, false)).collect();
        pen.runs(x + 2, yy, &runs);
        pen.c.right_with(len_end, yy, &span_ms(Some(call.ms)), pen.p.fg, None, true);
        provider_runs(pen, prov_x, yy, &call.provider, 12);
        pen.c.text(when_x, yy, &call.when, pen.p.dim);
        if call.failed {
            pen.c.text(failed_x, yy, &ended_word(call), pen.p.text2);
        }
        pen.c.right(thread_end, yy, &tail8(&call.thread), pen.p.dim);
    }
    y + 2 + rows.len() as i32
}

fn counted_block(pen: &mut Pen, tools: &Tools, x: i32, y: i32, w: i32) -> i32 {
    pen.section(x, y, w, "HOW THIS IS COUNTED", "");
    let mut yy = y + 1;
    for (n, para) in counted_lines(tools).into_iter().enumerate() {
        if n > 0 {
            yy += 1;
        }
        let (lo, hi) = para.find(EXAMPLE).map(|at| (at, at + EXAMPLE.len())).unwrap_or((usize::MAX, 0));
        for (line, start) in wrap_indexed(&para, w as usize) {
            for (i, ch) in line.chars().enumerate() {
                let k = start + i;
                let ink = if lo <= k && k < hi { pen.p.text2 } else { pen.p.dim };
                pen.c.set(x + i as i32, yy, ch, ink, None, false);
            }
            yy += 1;
        }
    }
    yy
}

/// The report's last line, true with the Insights cache on or off: the report does not know
/// whether this read used it, so "may". It names no folder, because the folder depends on
/// `XDG_CACHE_HOME` and on the name the program was installed under
/// (`cache_path::insights_file`): a path written here would be wrong on some machines, and
/// "your cache folder" is true on all of them. Nothing is sent in any mode. One row at 120
/// columns, two at 80, the second starting with a word, not `~` like a shell prompt.
const LAST_NOTE: &str = "Only program and tool names and exit codes are read. Numbers may be cached in your cache folder; nothing is sent.";

fn counted_lines(tools: &Tools) -> Vec<String> {
    let cmd = &tools.commands;
    let ended: u64 = tools.lengths.iter().map(|bucket| u64::from(bucket.calls)).sum();
    let mut lines = if ended == 0 {
        vec!["Time adds up each call's own length; no call here has one. Recap's 'tools' hours are counted another way.".to_owned()]
    } else {
        let clock = if tools.busy_ms > 0.0 { time_text(tools.busy_ms) } else { "<1s".into() };
        vec![format!("Time adds up each call's own length. Calls in a thread can overlap; counted once, they come to {clock} on the clock. Recap's 'tools' hours are counted another way.")]
    };
    let mut gone = Vec::new();
    let n = tools.closed_together;
    if n == 1 {
        gone.push("1 call was marked finished together with others, though they started at different times, most likely when a run stopped. When it really finished is not known, so its time is not counted and it is not among the longest: totals of time can only be too small.".to_owned());
    } else if n > 0 {
        gone.push(format!("{count} calls were marked finished all at once, though they started at different times, most likely when a run stopped. When each really finished is not known, so their time is not counted and they are not among the longest: totals of time can only be too small.", count = count(n)));
    }
    if tools.open > 0 {
        let open = tools.open;
        let more = if n > 0 { " more" } else { "" };
        let word = if open == 1 { "call" } else { "calls" };
        let verb = if open == 1 { "adds" } else { "add" };
        gone.push(format!("{}{more} {word} never ended and {verb} no time.", count(open)));
    }
    if !gone.is_empty() {
        lines.push(gone.join(" "));
    }
    if cmd.calls > 0 {
        let chains = if cmd.chained == 0 {
            "No command line ran two or more programs on the list.".to_owned()
        } else {
            format!("{} of command lines ran two or more programs on the list.", share(f64::from(cmd.chained), f64::from(cmd.calls)))
        };
        lines.push(format!("A line like {EXAMPLE} is one call, named for its main program, cargo: its time, failure and exit code are the whole line's. {chains} Programs are named from a list of {} common ones; a command with none of them is \"not on the list\".", cmd.listed));
    }
    lines.push(LAST_NOTE.into());
    lines
}

fn ended_word(call: &Long) -> String {
    if !call.failed {
        return String::new();
    }
    let Some(code) = call.exit else { return "failed".into() };
    let name = call.name.as_deref();
    match code {
        0 => "marked failed".into(),
        1 if name.is_some_and(|n| tools::FOUND_NOTHING.contains(&n)) => "found nothing".into(),
        1 if name.is_some_and(|n| tools::FILES_DIFFER.contains(&n)) => "files differ".into(),
        1 => "exit 1".into(),
        2 => "exit 2".into(),
        124 => "timed out".into(),
        126 => "could not run".into(),
        127 => "not installed".into(),
        129..=255 => "stopped".into(),
        _ => terminal::fit(&format!("exit {code}"), 13),
    }
}

fn table_heads(pen: &mut Pen, g: &Grid, y: i32) {
    pen.c.right(g.calls, y, "calls", pen.p.dim);
    pen.c.right(g.time, y, "time", pen.p.dim);
    if let Some(at) = g.typical {
        pen.c.right(at, y, "typical", pen.p.dim);
        pen.c.right(g.longest.unwrap_or(at), y, "longest", pen.p.dim);
    }
    pen.c.right(g.failed, y, "failed", pen.p.dim);
}

fn table_row(pen: &mut Pen, g: &Grid, y: i32, row: &Row, label: &[(String, Rgb)], top_calls: f64, top_ms: f64, min_rate: u32, glyph: Option<char>) {
    let calls_color = calls_ink(pen);
    let p = pen.p;
    if let Some(glyph) = glyph {
        pen.c.set(g.glyph, y, glyph, p.text2, None, false);
    }
    let runs: Vec<(&str, Rgb, bool)> = label.iter().map(|(s, c)| (s.as_str(), *c, false)).collect();
    pen.runs(g.name, y, &runs);
    pen.c.right(g.calls, y, &count(row.calls), p.text2);
    let (text, bold) = time_cell(u64::from(row.calls), row.ms, u64::from(row.open), u64::from(row.closed_together));
    if row.calls > 0 {
        let mut frac = f64::from(row.calls) / top_calls;
        if text == "unknown" && g.bar_w > 0 {
            frac = frac.min(f64::from(g.bar_w - 1) / f64::from(g.bar_w));
        }
        hbar(&mut pen.c, g.calls_bar, y, g.bar_w, frac, calls_color);
    }
    pen.c.right_with(g.time, y, &text, if bold { p.fg } else { p.dim }, None, bold);
    if row.ms > 0.0 {
        hbar(&mut pen.c, g.time_bar, y, g.bar_w, row.ms / top_ms, p.accent);
    }
    if let Some(at) = g.typical {
        pen.c.right(at, y, &span_ms(row.median_ms), p.text2);
        let longest = if row.longest_ms > 0.0 { span_ms(Some(row.longest_ms)) } else { "–".into() };
        pen.c.right(g.longest.unwrap_or(at), y, &longest, p.text2);
    }
    failed_cells(pen, g, y, u64::from(row.calls), u64::from(row.failed), min_rate, p.text2);
}

fn fold_row(pen: &mut Pen, g: &Grid, y: i32, rows: &[&Row], min_rate: u32) {
    let calls = rows.iter().map(|r| u64::from(r.calls)).sum::<u64>();
    let ms = rows.iter().map(|r| r.ms).sum::<f64>();
    let failed = rows.iter().map(|r| u64::from(r.failed)).sum::<u64>();
    let open = rows.iter().map(|r| u64::from(r.open)).sum::<u64>();
    let closed = rows.iter().map(|r| u64::from(r.closed_together)).sum::<u64>();
    pen.c.text(g.name, y, &format!("{} more", rows.len()), pen.p.dim);
    pen.c.right(g.calls, y, &count_u(calls), pen.p.dim);
    pen.c.right(g.time, y, &time_cell(calls, ms, open, closed).0, pen.p.dim);
    failed_cells(pen, g, y, calls, failed, min_rate, pen.p.dim);
}

fn failed_cells(pen: &mut Pen, g: &Grid, y: i32, calls: u64, failed: u64, min_rate: u32, ink: Rgb) {
    if calls == 0 {
        pen.c.right(g.failed, y, "–", pen.p.dim);
        return;
    }
    if failed == 0 {
        pen.c.right(g.failed, y, "none", pen.p.dim);
        return;
    }
    pen.c.right(g.failed, y, &count_u(failed), ink);
    if calls < u64::from(min_rate) {
        pen.c.text(g.rate, y, "too few to say", pen.p.dim);
    } else {
        pen.c.right(g.rate + 4, y, &share(failed as f64, calls as f64), ink);
    }
}

/// The rows a table draws, in the value's order. `keep` is the unnamed program row, drawn
/// whatever its size when it has a call. `by_failed` marks the rows with the most failures.
/// A single leftover is drawn, not folded.
fn shown<'a>(rows: &[&'a Row], by_time: usize, by_calls: usize, keep: Option<&'a Row>, by_failed: usize) -> (Vec<&'a Row>, Vec<&'a Row>) {
    let live: Vec<&Row> = rows.iter().copied().filter(|row| row.calls > 0).collect();
    let mut most = live.clone();
    most.sort_by(|a, b| b.calls.cmp(&a.calls).then(b.ms.total_cmp(&a.ms)).then(a.name.is_none().cmp(&b.name.is_none())).then(a.name.cmp(&b.name)));
    most.truncate(by_calls);
    let mut failing: Vec<&Row> = live.iter().copied().filter(|row| row.failed > 0).collect();
    failing.sort_by(|a, b| b.failed.cmp(&a.failed).then(b.calls.cmp(&a.calls)).then(b.ms.total_cmp(&a.ms)).then(a.name.is_none().cmp(&b.name.is_none())).then(a.name.cmp(&b.name)));
    failing.truncate(by_failed);
    let mut pick: Vec<*const Row> = live.iter().take(by_time).map(|row| *row as *const Row).collect();
    for row in most.into_iter().chain(failing) {
        let ptr = row as *const Row;
        if !pick.contains(&ptr) {
            pick.push(ptr);
        }
    }
    if let Some(row) = keep.filter(|row| row.calls > 0) {
        let ptr = row as *const Row;
        if !pick.contains(&ptr) {
            pick.push(ptr);
        }
    }
    let head: Vec<&Row> = live.iter().copied().filter(|row| pick.contains(&(*row as *const Row))).collect();
    let rest: Vec<&Row> = live.iter().copied().filter(|row| !pick.contains(&(*row as *const Row))).collect();
    if rest.len() == 1 { (live, Vec::new()) } else { (head, rest) }
}

fn inside_others(tools: &Tools) -> Vec<(&str, u32)> {
    let mut out: Vec<(&str, u32)> = tools.programs.iter().filter_map(|row| {
        let name = row.name.as_deref().filter(|s| !s.is_empty())?;
        (row.appears_in > row.calls).then_some((name, row.appears_in - row.calls))
    }).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    out
}

fn label_of(row: &Row, head: &[&Row], width: usize, with_server: bool, p: &Palette) -> Vec<(String, Rgb)> {
    let siblings: Vec<Option<&str>> = head.iter().filter(|other| !std::ptr::eq(**other, row)).map(|other| other.name.as_deref()).collect();
    tool_label(row.kind, row.name.as_deref(), server_of(&row.server), &siblings, width, with_server, p)
}

fn long_runs(call: &Long, width: usize, with_server: bool, p: &Palette) -> Vec<(String, Rgb)> {
    if call.name.is_none() {
        vec![(long_label(call.kind, None), p.text2)]
    } else {
        tool_label(call.kind, call.name.as_deref(), server_of(&call.server), &[], width, with_server, p)
    }
}

/// A tool row's words, cut to `width`. The server is shown on the wide report, and at 80
/// columns when another drawn row has the same name. An empty server is no server.
fn tool_label(kind: Kind, name: Option<&str>, server: Option<&str>, siblings: &[Option<&str>], width: usize, with_server: bool, p: &Palette) -> Vec<(String, Rgb)> {
    let Some(name) = name else {
        return vec![(unnamed_tool(kind).to_owned(), if kind == Kind::Tool { p.text2 } else { p.fg })];
    };
    let clash = server.is_some() && siblings.iter().any(|other| other.as_deref() == Some(name));
    if let Some(server) = server.filter(|_| with_server || clash) {
        let name_w = terminal::width(name);
        if name_w + 1 + terminal::width(server) <= width {
            return vec![(name.to_owned(), p.fg), (format!(" {server}"), p.dim)];
        }
        if name_w + 1 + 8 <= width {
            return vec![(name.to_owned(), p.fg), (format!(" {}", mid(server, width - 1 - name_w)), p.dim)];
        }
        if clash {
            return vec![(mid(name, width.saturating_sub(9)), p.fg), (format!(" {}", mid(server, 8)), p.dim)];
        }
    }
    vec![(mid(name, width), p.fg)]
}

fn server_of(server: &Option<String>) -> Option<&str> {
    server.as_deref().filter(|s| !s.is_empty())
}

fn provider_runs(pen: &mut Pen, x: i32, y: i32, name: &str, width: usize) {
    if name.is_empty() || name == "unknown" {
        let dim = pen.p.dim;
        let fitted = terminal::fit("not recorded", width);
        pen.runs(x, y, &[("  ", dim, false), (fitted.as_str(), dim, false)]);
        return;
    }
    let color = pen.p.family_color(terminal::family(name, None));
    let shown = format!(" {}", mid(name, width));
    pen.c.set(x, y, '●', color, None, false);
    pen.c.text(x + 1, y, &shown, pen.p.text2);
}

fn recorded_name(provider: &str) -> &str {
    if provider.is_empty() || provider == "unknown" { "not recorded" } else { provider }
}

fn tops(rows: &[Row]) -> (f64, f64) {
    let calls = rows.iter().map(|r| r.calls).max().unwrap_or(0).max(1);
    let ms = rows.iter().map(|r| r.ms).fold(0.0, f64::max);
    (f64::from(calls), if ms > 0.0 { ms } else { 1.0 })
}

fn tops_ref(rows: &[&Row]) -> (f64, f64) {
    let calls = rows.iter().map(|r| r.calls).max().unwrap_or(0).max(1);
    let ms = rows.iter().map(|r| r.ms).fold(0.0, f64::max);
    (f64::from(calls), if ms > 0.0 { ms } else { 1.0 })
}

/// Eighths, rounded half up, with no track. Any fraction above zero draws at least one eighth.
fn hbar(c: &mut Canvas, x: i32, y: i32, w: i32, frac: f64, fg: Rgb) {
    if w <= 0 {
        return;
    }
    let mut units = (frac.clamp(0.0, 1.0) * w as f64 * 8.0 + 0.5).floor() as i32;
    if frac > 0.0 {
        units = units.max(1);
    }
    let (full, rem) = (units / 8, units % 8);
    for i in 0..full {
        c.set(x + i, y, '█', fg, None, false);
    }
    if rem > 0 {
        c.set(x + full, y, EIGHTHS[rem as usize], fg, None, false);
    }
}

/// `terminal::hbar` draws a track and can round a sliver down to nothing, so a calls bar is this.
fn calls_ink(pen: &Pen) -> Rgb {
    if pen.mono { pen.p.accent } else { pen.shade(pen.p.accent, 0.55, pen.p.bg) }
}

fn kind_glyph(kind: Kind) -> char {
    if kind == Kind::Search { '⊛' } else { model::glyph(kind_name(kind)) }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Shell => "shell",
        Kind::Edit => "edit",
        Kind::Read => "read",
        Kind::Search => "search",
        Kind::Web => "web",
        Kind::Tool => "tool",
    }
}

fn kind_word(kind: Kind) -> &'static str {
    match kind {
        Kind::Shell => "commands",
        Kind::Edit => "file changes",
        Kind::Search => "searches",
        Kind::Web => "web searches",
        Kind::Read => "reads",
        Kind::Tool => "other tools",
    }
}

fn unnamed_tool(kind: Kind) -> &'static str {
    match kind {
        Kind::Edit => "file changes",
        Kind::Search => "file searches",
        Kind::Web => "web searches",
        Kind::Read => "unnamed reads",
        Kind::Tool => "unnamed tools",
        Kind::Shell => "commands",
    }
}

fn unnamed_one(kind: Kind) -> &'static str {
    match kind {
        Kind::Shell => "not on the list",
        Kind::Edit => "file change",
        Kind::Search => "file search",
        Kind::Web => "web search",
        Kind::Read => "unnamed read",
        Kind::Tool => "unnamed tool",
    }
}

fn long_label(kind: Kind, name: Option<&str>) -> String {
    let named = name.filter(|s| !s.is_empty());
    match named {
        Some(name) => name.to_owned(),
        None if kind == Kind::Shell => "not on the list".into(),
        None => unnamed_one(kind).into(),
    }
}

fn tail8(s: &str) -> String {
    let count = s.chars().count();
    if count <= 8 { s.to_owned() } else { s.chars().skip(count - 8).collect() }
}

/// Words onto lines of at most `width`, with each line's start in `text`. A long word is kept
/// whole, as `build.py`'s wrap does; these paragraphs have no word that needs breaking.
fn wrap_indexed(text: &str, width: usize) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    let mut line = String::new();
    let mut start = 0usize;
    let mut pos = 0usize;
    for word in text.split(' ') {
        if !line.is_empty() && terminal::width(&line) + 1 + terminal::width(word) > width {
            out.push((std::mem::take(&mut line), start));
            line = word.to_owned();
            start = pos;
        } else if line.is_empty() {
            start = pos;
            line = word.to_owned();
        } else {
            line.push(' ');
            line.push_str(word);
        }
        pos += word.len() + 1;
    }
    out.push((line, start));
    out
}

fn count(n: u32) -> String {
    recap::count_text(u64::from(n))
}
fn count_u(n: u64) -> String {
    recap::count_text(n)
}

fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A row's time. No calls: `–`. Every call without a known length: `unknown`, not bold.
/// Calls that took no measurable time: `<1s>`, bold. Otherwise the sum, bold.
fn time_cell(calls: u64, ms: f64, open: u64, closed: u64) -> (String, bool) {
    if calls == 0 {
        ("–".into(), false)
    } else if calls == untimed(open, closed) {
        ("unknown".into(), false)
    } else if ms > 0.0 {
        (time_text(ms), true)
    } else {
        ("<1s".into(), true)
    }
}

fn time_text(ms: f64) -> String {
    if ms <= 0.0 {
        return "–".into();
    }
    if ms < 1_000.0 {
        return "<1s".into();
    }
    if ms < 59_500.0 {
        return format!("{}s", (ms / 1000.0).round() as u64);
    }
    if ms < 3_570_000.0 {
        return format!("{}m", (ms / 60_000.0).round() as u64);
    }
    let h = ms / 3_600_000.0;
    if h < 9.95 { format!("{h:.1}h") } else { format!("{}h", commas(h.round() as u64)) }
}

/// `terminal::span`, except under a second is `<1s` and a missing length is `–`.
fn span_ms(ms: Option<f64>) -> String {
    let Some(ms) = ms.filter(|n| n.is_finite()) else { return "–".into() };
    if ms < 1_000.0 { "<1s".into() } else { terminal::span(Some(ms / 1000.0)) }
}

fn share(part: f64, whole: f64) -> String {
    if whole <= 0.0 || part <= 0.0 {
        return "–".into();
    }
    if part >= whole {
        return "100%".into();
    }
    let p = (100.0 * part / whole + 0.5).floor();
    if p < 1.0 { "<1%".into() } else if p > 99.0 { ">99%".into() } else { format!("{p:.0}%") }
}

fn hours_big(ms: f64) -> (String, String) {
    let h = ms / 3_600_000.0;
    if h >= 1000.0 {
        (recap::short_text(h, 10.0), "h".into())
    } else if h >= 9.95 {
        ((h.round() as u64).to_string(), "h".into())
    } else if h >= 1.0 {
        (format!("{h:.1}"), "h".into())
    } else if ms >= 59_500.0 {
        (((ms / 60_000.0).round() as u64).to_string(), "m".into())
    } else if ms > 0.0 {
        (((ms / 1000.0).round().max(1.0) as u64).to_string(), "s".into())
    } else {
        ("0".into(), "s".into())
    }
}

fn mid(s: &str, n: usize) -> String {
    if terminal::width(s) <= n {
        return s.to_owned();
    }
    if n == 0 {
        return String::new();
    }
    if n == 1 {
        return "…".into();
    }
    let head = n / 2;
    let tail = n - 1 - head;
    format!("{}…{}", take_cells(s, head), take_cells_end(s, tail))
}

fn take_cells(s: &str, n: usize) -> String {
    let mut w = 0usize;
    let mut out = String::new();
    for c in s.chars() {
        let cw = terminal::cell_width(c);
        if w + cw > n {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

fn take_cells_end(s: &str, n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let mut w = 0usize;
    let mut chars = Vec::new();
    for c in s.chars().rev() {
        let cw = terminal::cell_width(c);
        if w + cw > n {
            break;
        }
        chars.push(c);
        w += cw;
    }
    chars.into_iter().rev().collect()
}

fn failed_words(calls: u32, failed: u32, min_rate: u32) -> String {
    if calls == 0 {
        String::new()
    } else if failed == 0 {
        "none failed".into()
    } else if calls < min_rate {
        format!("{} failed (too few to say)", count(failed))
    } else {
        format!("{} failed ({})", count(failed), share(f64::from(failed), f64::from(calls)))
    }
}

/// Under 80 columns: one fact a line, no bars and no color. `width` is the terminal's columns.
/// Under 40, lines are left whole.
pub fn list(tools: &Tools, source: Source, width: usize) -> String {
    let m = tools.min_for_rate;
    let ended: u64 = tools.lengths.iter().map(|b| u64::from(b.calls)).sum();
    let (b0_calls, b0_ms) = bucket(tools, 0);
    let period = recap_view::period_text(tools.period, &tools.from, &tools.to);
    let failed_line = if tools.failed == 0 {
        let word = if tools.calls == 1 { "call" } else { "calls" };
        format!("failed       none  of {} {word}", count(tools.calls))
    } else {
        format!("failed       {}  {}", count(tools.failed), failed_note(tools))
    };
    let mut out = vec![
        format!("T3 AGENTS · TOOLS · {period} · {}", coverage(tools, source)),
        String::new(),
        format!("calls        {}  {}", count(tools.calls), calls_note(tools)),
        format!("call time    {}  {}", time_cell(u64::from(tools.calls), tools.ms, u64::from(tools.open), u64::from(tools.closed_together)).0, clock_note(tools)),
        failed_line,
    ];
    if ended == 0 {
        out.push("how long     no call has a known length".into());
        out.push(String::new());
    } else {
        let first = if b0_calls == 0 {
            "no call took under a second".to_owned()
        } else {
            format!("{} of calls under a second: {} of the time", share(f64::from(b0_calls), ended as f64), share(b0_ms, tools.ms))
        };
        out.push(format!("how long     {first}"));
        out.push(format!("             {}", minute_list(tools)));
        out.push(String::new());
    }
    for (i, row) in tools.kinds.iter().enumerate() {
        let lead = if i == 0 { "by kind      " } else { "             " };
        out.push(format!("{lead}{} {} · {} · {}", kind_word(row.kind), count(row.calls), time_cell(u64::from(row.calls), row.ms, u64::from(row.open), u64::from(row.closed_together)).0, failed_words(row.calls, row.failed, m)));
    }
    out.push(String::new());
    let programs: Vec<&Row> = tools.programs.iter().collect();
    let unnamed = programs.iter().copied().find(|row| row.name.is_none());
    let (head, rest) = shown(&programs, 5, 3, unnamed, 2);
    for (i, row) in head.iter().enumerate() {
        let lead = if i == 0 { "programs     " } else { "             " };
        let name = row.name.as_deref().unwrap_or("not on the list");
        out.push(format!("{lead}{name} {} · {} · {}", count(row.calls), time_cell(u64::from(row.calls), row.ms, u64::from(row.open), u64::from(row.closed_together)).0, failed_words(row.calls, row.failed, m)));
    }
    if !rest.is_empty() {
        out.push(format!("             {} more {} · {}", rest.len(), count_u(rest.iter().map(|r| u64::from(r.calls)).sum()), fold_time(&rest)));
    }
    out.push(format!("             {} programs on the list", count(tools.commands.listed)));
    out.push(String::new());
    if tools.commands.calls > 0 {
        out.extend(why_list(tools));
        out.push(String::new());
    }
    let (named, nameless) = tool_parts(tools);
    let (head, rest) = shown(&named, 3, 2, None, 0);
    let mut group = Vec::new();
    for row in &head {
        let name: String = label_of(row, &head, 22, false, &Palette::default()).into_iter().map(|(text, _)| text).collect();
        group.push(format!("{name} {} · {} · {}", count(row.calls), time_cell(u64::from(row.calls), row.ms, u64::from(row.open), u64::from(row.closed_together)).0, failed_words(row.calls, row.failed, m)));
    }
    if !rest.is_empty() {
        group.push(format!("{} more {} · {}", rest.len(), count_u(rest.iter().map(|r| u64::from(r.calls)).sum()), fold_time(&rest)));
    }
    if !nameless.is_empty() {
        let items = nameless.iter().map(|row| format!("{} {}", unnamed_tool(row.kind), count(row.calls))).collect::<Vec<_>>().join(" · ");
        group.push(format!("no name  {items}"));
    }
    for (i, line) in group.iter().enumerate() {
        let lead = if i == 0 { "tools        " } else { "             " };
        out.push(format!("{lead}{line}"));
    }
    if !group.is_empty() {
        out.push(String::new());
    }
    for (i, provider) in tools.providers.iter().enumerate() {
        let lead = if i == 0 { "providers    " } else { "             " };
        let name = recorded_name(&provider.provider);
        if provider.commands == 0 {
            out.push(format!("{lead}{name} no commands"));
        } else {
            let failed = if provider.commands_failed == 0 { "none".to_owned() } else { count(provider.commands_failed) };
            let word = if provider.commands == 1 { "command" } else { "commands" };
            out.push(format!("{lead}{name} {} {word} · {failed} marked failed", count(provider.commands)));
        }
        if provider.closed_together > 0 {
            let short = closed_words(provider.closed_together).split(':').next().unwrap_or("").to_owned();
            out.push(format!("             └ {short}"));
        }
    }
    if !tools.longest.is_empty() {
        out.push(String::new());
        for (i, call) in tools.longest.iter().take(3).enumerate() {
            let lead = if i == 0 { "longest      " } else { "             " };
            let ended_mark = if call.failed { format!(" · {}", ended_word(call)) } else { String::new() };
            out.push(format!("{lead}{} {} · {} · {}{ended_mark}", mid(&long_label(call.kind, call.name.as_deref()), 22), span_ms(Some(call.ms)), recorded_name(&call.provider), tail8(&call.thread)));
        }
    }
    while out.last().is_some_and(|line| line.is_empty()) {
        out.pop();
    }
    fit_list(&out, width).join("\n")
}

const LIST_LABEL: usize = 13;
const LIST_HANG: usize = 2;

/// Wrap a plain list to `width`. Under 40 columns, leave every line whole.
fn fit_list(lines: &[String], width: usize) -> Vec<String> {
    if width < 40 {
        return lines.to_vec();
    }
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if terminal::width(line) <= width {
            out.push(line.clone());
            continue;
        }
        let (lead, text) = if i == 0 { (String::new(), line.clone()) } else { (take_cells(line, LIST_LABEL), line.chars().skip(LIST_LABEL).collect()) };
        let pad = " ".repeat(terminal::width(&lead) + LIST_HANG);
        let parts = break_items(&text, width.saturating_sub(terminal::width(&lead)), width.saturating_sub(terminal::width(&pad)));
        if let Some((first, rest)) = parts.split_first() {
            out.push(format!("{lead}{first}"));
            for part in rest {
                out.push(format!("{pad}{part}"));
            }
        }
    }
    out
}

/// Break `text` between items joined by ` · `. An item longer than its line breaks between words.
fn break_items(text: &str, first: usize, later: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for item in text.split(" · ") {
        let room = if lines.is_empty() { first } else { later };
        if !line.is_empty() && terminal::width(&line) + 3 + terminal::width(item) <= room {
            line.push_str(" · ");
            line.push_str(item);
            continue;
        }
        if !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        let mut item = item.to_owned();
        if terminal::width(&item) > if lines.is_empty() { first } else { later } {
            let mut words: Vec<String> = item.split_whitespace().map(str::to_owned).collect();
            while !words.is_empty() && terminal::width(&words.join(" ")) > if lines.is_empty() { first } else { later } {
                let room = if lines.is_empty() { first } else { later };
                let mut cur = String::new();
                while !words.is_empty() {
                    let word = words[0].clone();
                    if cur.is_empty() && terminal::width(&word) > room {
                        let (head, tail) = split_cells(&word, room);
                        cur = head;
                        words[0] = tail;
                        break;
                    }
                    if !cur.is_empty() && terminal::width(&cur) + 1 + terminal::width(&word) > room {
                        break;
                    }
                    if !cur.is_empty() {
                        cur.push(' ');
                    }
                    cur.push_str(&word);
                    words.remove(0);
                }
                lines.push(cur);
            }
            item = words.join(" ");
        }
        line = item;
    }
    lines.push(line);
    lines
}

fn split_cells(s: &str, n: usize) -> (String, String) {
    let head = take_cells(s, n);
    let tail = s[head.len()..].to_owned();
    (head, tail)
}

fn fold_time(rows: &[&Row]) -> String {
    let calls = rows.iter().map(|row| u64::from(row.calls)).sum();
    let ms = rows.iter().map(|row| row.ms).sum();
    let open = rows.iter().map(|row| u64::from(row.open)).sum();
    let closed = rows.iter().map(|row| u64::from(row.closed_together)).sum();
    time_cell(calls, ms, open, closed).0
}

fn why_list(tools: &Tools) -> Vec<String> {
    let commands = &tools.commands;
    let lead = "why failed   ";
    let pad = "             ";
    if commands.failed == 0 {
        return vec![format!("{lead}no command was marked failed")];
    }
    if commands.exits.failed_known == 0 {
        let head = if commands.failed == 1 { "the one marked failed has no exit code".to_owned() } else { format!("none of the {} marked failed has an exit code", count(commands.failed)) };
        let mut out = vec![format!("{lead}{head}")];
        out.extend(lacking_sentences(tools).into_iter().map(|sentence| format!("{pad}{sentence}")));
        return out;
    }
    let rows = why_rows(tools);
    let mut out = vec![format!("{lead}{} of the {} have an exit code", count(commands.exits.failed_known), count(commands.failed))];
    let reasons: Vec<String> = rows.iter().filter(|row| row.key != "no_code").map(|row| format!("{} {}", count(row.count), row.label)).collect();
    for chunk in reasons.chunks(3) {
        out.push(format!("{pad}{}", chunk.join(" · ")));
    }
    if let Some(missing) = rows.last().filter(|row| row.key == "no_code") {
        let who = who_lacks(tools).iter().map(|(name, n)| format!("{name} {}", count(*n))).collect::<Vec<_>>().join(" · ");
        out.push(format!("{pad}{} no exit code ({who})", count(missing.count)));
    }
    out.extend(lacking_sentences(tools).into_iter().map(|sentence| format!("{pad}{sentence}")));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Bucket, Provider};
    use recap_view::Color;

    const SAMPLE: &str = include_str!("../../../checks/fixtures/tools/sample.json");
    const MONTH: &str = include_str!("../../../checks/fixtures/tools/sample-month.json");
    const THIN: &str = include_str!("../../../checks/fixtures/tools/sample-thin.json");
    const LARGE: &str = include_str!("../../../checks/fixtures/tools/sample-large.json");
    const CLAUDE: &str = include_str!("../../../checks/fixtures/tools/sample-claude.json");
    const QUIET: &str = include_str!("../../../checks/fixtures/tools/sample-quiet.json");
    const OPEN: &str = include_str!("../../../checks/fixtures/tools/sample-open.json");

    fn value(json: &str) -> Tools {
        serde_json::from_str(json).expect("tools sample")
    }
    fn plain(canvas: &Canvas) -> String {
        recap_view::paint(canvas, Color::None)
    }
    fn frame_lines(text: &str) -> Vec<&str> {
        text.lines().collect()
    }
    fn assert_frame(name: &str, got: &str, expected: &str) {
        let got = frame_lines(got);
        let expected = frame_lines(expected);
        for i in 0..got.len().max(expected.len()) {
            let left = got.get(i).copied().unwrap_or("<missing>");
            let right = expected.get(i).copied().unwrap_or("<missing>");
            if left != right {
                panic!("{name} line {}:\nexpected: {right:?}\n     got: {left:?}\n({} expected lines, {} drawn)", i + 1, expected.len(), got.len());
            }
        }
    }
    /// Column of `needle` on a row. `row_text` is a string, so a byte index is not a column.
    fn cell_at(canvas: &Canvas, y: usize, needle: &str) -> usize {
        let mut text = String::new();
        let mut cols = Vec::new();
        for (x, cell) in canvas.cells[y * canvas.w..(y + 1) * canvas.w].iter().enumerate() {
            if cell.ch == terminal::CONT {
                continue;
            }
            text.push(cell.ch);
            cols.push(x);
        }
        let at = text.find(needle).unwrap_or_else(|| panic!("{needle} is not on the row"));
        cols[text[..at].chars().count()]
    }
    fn fits(text: &str, columns: usize) {
        for (i, line) in text.lines().enumerate() {
            let width = terminal::width(line);
            assert!(width <= columns - 1, "line {} is {width} columns in a {columns}-column report: {line}", i + 1);
        }
    }
    fn too_few_follows_a_count(text: &str) {
        for line in text.lines() {
            if let Some(before) = line.split("too few to say").next() {
                if !line.contains("too few to say") {
                    continue;
                }
                assert!(!before.contains("none"), "too few to say follows none: {line}");
                assert!(before.chars().any(|c| c.is_ascii_digit()), "too few to say follows no count: {line}");
            }
        }
    }

    #[test]
    fn frames_match_the_fixtures() {
        let p = Palette::default();
        let full = value(SAMPLE);
        assert_frame("t-80-full", &plain(&report(&full, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-full.txt"));
        assert_frame("t-80-full-mono", &plain(&report(&full, &p, Source::Records, true)), include_str!("../../../checks/fixtures/tools/t-80-full-mono.txt"));
        assert_frame("t-80-demo", &plain(&report(&full, &p, Source::Demo, false)), include_str!("../../../checks/fixtures/tools/t-80-demo.txt"));
        assert_frame("t-120-full", &plain(&wide(&full, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-full.txt"));
        assert_frame("t-120-full-mono", &plain(&wide(&full, &p, Source::Records, true)), include_str!("../../../checks/fixtures/tools/t-120-full-mono.txt"));
        assert_frame("t-list", &list(&full, Source::Records, 60), include_str!("../../../checks/fixtures/tools/t-list.txt"));
        let month = value(MONTH);
        assert_frame("t-80-month", &plain(&report(&month, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-month.txt"));
        assert_frame("t-120-month", &plain(&wide(&month, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-month.txt"));
        let thin = value(THIN);
        assert_frame("t-80-thin", &plain(&report(&thin, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-thin.txt"));
        assert_frame("t-120-thin", &plain(&wide(&thin, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-thin.txt"));
        assert_frame("t-list-thin", &list(&thin, Source::Records, 60), include_str!("../../../checks/fixtures/tools/t-list-thin.txt"));
        let large = value(LARGE);
        assert_frame("t-80-large", &plain(&report(&large, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-large.txt"));
        assert_frame("t-120-large", &plain(&wide(&large, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-large.txt"));
        let claude = value(CLAUDE);
        assert_frame("t-80-claude", &plain(&report(&claude, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-claude.txt"));
        assert_frame("t-120-claude", &plain(&wide(&claude, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-claude.txt"));
        assert_frame("t-list-claude", &list(&claude, Source::Records, 60), include_str!("../../../checks/fixtures/tools/t-list-claude.txt"));
        let quiet = value(QUIET);
        assert_frame("t-80-quiet", &plain(&report(&quiet, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-quiet.txt"));
        assert_frame("t-120-quiet", &plain(&wide(&quiet, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-quiet.txt"));
        assert_frame("t-list-quiet", &list(&quiet, Source::Records, 60), include_str!("../../../checks/fixtures/tools/t-list-quiet.txt"));
        let open = value(OPEN);
        assert_frame("t-80-open", &plain(&report(&open, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-80-open.txt"));
        assert_frame("t-120-open", &plain(&wide(&open, &p, Source::Records, false)), include_str!("../../../checks/fixtures/tools/t-120-open.txt"));
        assert_frame("t-list-open", &list(&open, Source::Records, 60), include_str!("../../../checks/fixtures/tools/t-list-open.txt"));
        let period = recap_view::period_text(full.period, &full.from, &full.to);
        let empty = notice(&p, &period, "no tool calls", false, &format!("No tool calls recorded {period}, or this week so far."), &[
            "Tools reads the tool calls T3 Code has recorded on this machine.".into(),
            "Just looking? t3-agents --tools --demo shows a made-up week.".into(),
        ]);
        assert_frame("t-80-empty", &plain(&empty), include_str!("../../../checks/fixtures/tools/t-80-empty.txt"));
        let unreadable = notice(&p, &period, "can't read T3", true, "Can't read the T3 database at ~/.t3/userdata/statev2.sqlite: database is locked.", &[
            "Is T3 Code installed, and has it been opened at least once?".into(),
            "T3 data somewhere else? Start with --db PATH, or set T3CODE_HOME to its folder.".into(),
            "Just looking? t3-agents --tools --demo shows a made-up week.".into(),
        ]);
        assert_frame("t-80-unreadable", &plain(&unreadable), include_str!("../../../checks/fixtures/tools/t-80-unreadable.txt"));
    }

    #[test]
    fn number_formats_match_the_vectors() {
        for (n, text) in [(0u64, "0"), (999, "999"), (18821, "18,821"), (99999, "99,999"), (114733, "115K"), (1_250_000, "1.2M"), (1_350_000, "1.4M")] {
            assert_eq!(recap::count_text(n), text, "count {n}");
        }
        assert_eq!(recap::short_text(4451.0, 10.0), "4.5K");
        assert_eq!(recap::short_text(26459.0, 10.0), "26K");
        for (ms, text) in [(0.0, "–"), (400.0, "<1s"), (45_000.0, "45s"), (59_499.0, "59s"), (59_500.0, "1m"), (150_000.0, "3m"), (2_400_000.0, "40m"), (3_569_999.0, "59m"), (3_570_000.0, "1.0h"), (24_480_000.0, "6.8h"), (35_819_999.0, "9.9h"), (35_820_000.0, "10h"), (342_000_000.0, "95h"), (4_446_000_000.0, "1,235h")] {
            assert_eq!(time_text(ms), text, "time {ms}");
        }
        for (ms, text) in [(400.0, "<1s"), (36_900.0, "36s"), (125_000.0, "2m05s"), (1_620_000.0, "27m"), (7_860_000.0, "2h11m"), (38_400_000.0, "10h"), (100_800_000.0, "1d4h")] {
            assert_eq!(span_ms(Some(ms)), text, "span {ms}");
        }
        assert_eq!(span_ms(None), "–");
        for (part, whole, text) in [(0.0, 100.0, "–"), (3.0, 1000.0, "<1%"), (5.0, 1000.0, "1%"), (81.0, 100.0, "81%"), (995.0, 1000.0, ">99%"), (100.0, 100.0, "100%")] {
            assert_eq!(share(part, whole), text, "share {part}/{whole}");
        }
        for (n, text) in [(895u64, "895"), (4451, "4.5K"), (26459, "26K"), (151469, "151K")] {
            assert_eq!(recap_view::tile_count(n), text, "tile {n}");
        }
        assert_eq!(hours_big(342_000_000.0), ("95".into(), "h".into()));
        assert_eq!(hours_big(24_480_000.0), ("6.8".into(), "h".into()));
        assert_eq!(hours_big(1234.0 * 3_600_000.0), ("1.2K".into(), "h".into()));
        let name = "list_pull_request_review_comments_with_resolved_thread_state";
        assert_eq!(mid(name, 20), "list_pull_…ead_state");
        assert_eq!(mid("acme-internal-knowledge-graphs", 18), "acme-inte…e-graphs");
        assert_eq!(mid("wait", 20), "wait");
        assert_eq!(terminal::fit("cargo", 20), "cargo");
        assert_eq!(terminal::fit(name, 12), "list_pull_r…");
    }

    #[test]
    fn no_line_passes_the_last_column() {
        let p = Palette::default();
        for (json, columns) in [(SAMPLE, 80), (SAMPLE, 120), (MONTH, 80), (MONTH, 120), (THIN, 80), (THIN, 120), (LARGE, 80), (LARGE, 120), (CLAUDE, 80), (CLAUDE, 120), (QUIET, 80), (QUIET, 120), (OPEN, 80), (OPEN, 120)] {
            let tools = value(json);
            for mono in [false, true] {
                let canvas = if columns == 80 { report(&tools, &p, Source::Records, mono) } else { wide(&tools, &p, Source::Records, mono) };
                fits(&plain(&canvas), columns);
            }
        }
    }

    #[test]
    fn too_few_to_say_follows_a_count_and_never_none() {
        let p = Palette::default();
        for json in [SAMPLE, MONTH, THIN, LARGE, CLAUDE, QUIET, OPEN] {
            let tools = value(json);
            too_few_follows_a_count(&plain(&report(&tools, &p, Source::Records, false)));
            too_few_follows_a_count(&plain(&wide(&tools, &p, Source::Records, false)));
            too_few_follows_a_count(&list(&tools, Source::Records, 60));
        }
    }

    #[test]
    fn ink_and_weight_follow_the_roles() {
        let p = Palette::default();
        let tools = value(SAMPLE);
        let canvas = report(&tools, &p, Source::Records, false);
        let row = |text: &str| (0..canvas.h).find(|&y| canvas.row_text(y).contains(text)).unwrap_or_else(|| panic!("no row {text}"));
        let time = row("68h");
        let at = cell_at(&canvas, time, "68h");
        for i in 0..3 {
            let cell = canvas.get((at + i) as i32, time as i32).unwrap();
            assert!(cell.bold && cell.fg == p.fg, "time cell {:?}", cell.ch);
        }
        let fold = row("37 more");
        for x in 0..canvas.w {
            let cell = canvas.get(x as i32, fold as i32).unwrap();
            if cell.ch != ' ' {
                assert!(cell.fg == p.dim && !cell.bold, "fold {:?}", cell.ch);
            }
        }
        // The full week at 80 columns no longer draws a row under 20 calls. The wide report still does.
        let broad = wide(&tools, &p, Source::Records, false);
        let few = (0..broad.h).find(|&y| broad.row_text(y).contains("too few to say")).expect("no row too few to say");
        let at = cell_at(&broad, few, "too few to say");
        for i in 0.."too few to say".len() {
            let cell = broad.get((at + i) as i32, few as i32).unwrap();
            assert!(cell.fg == p.dim && !cell.bold);
        }
        let commands = row("68h");
        let mut bars = Vec::new();
        let mut prev = false;
        for x in 0..canvas.w {
            let cell = canvas.get(x as i32, commands as i32).unwrap();
            let bar = matches!(cell.ch, '█' | '▏' | '▎' | '▍' | '▌' | '▋' | '▊' | '▉');
            if bar && !prev {
                bars.push(cell.fg);
            }
            prev = bar;
        }
        assert!(bars.len() >= 2, "the commands row has calls and time bars");
        assert_ne!(bars[0], bars[1]);
        assert_eq!(bars[1], p.accent);
        let pen = Pen::new(&p, 1, 1, false);
        assert_eq!(bars[0], pen.shade(p.accent, 0.55, p.bg));
        let mut dots = 0;
        for y in 0..canvas.h {
            for x in 0..canvas.w {
                let cell = canvas.get(x as i32, y as i32).unwrap();
                if cell.ch != '●' {
                    continue;
                }
                dots += 1;
                let row_text = canvas.row_text(y);
                let name = row_text.split('●').nth(1).unwrap().split_whitespace().next().unwrap();
                assert_eq!(cell.fg, p.family_color(terminal::family(name, None)), "{name}");
            }
        }
        assert!(dots >= 4, "providers keep their dots");
        let missing = row("not recorded");
        assert!(!canvas.row_text(missing).contains('●'));
        let hung = row("└");
        let at = cell_at(&canvas, hung, "└");
        let mark = canvas.get(at as i32, hung as i32).unwrap();
        assert_eq!(mark.fg, p.rule);
        assert!(!mark.bold);
        for cell in &canvas.cells {
            assert_ne!(cell.fg, p.hot);
            assert_ne!(cell.bg, p.hot);
            assert_ne!(cell.fg, p.warn);
            assert_ne!(cell.bg, p.warn);
        }
        let period = recap_view::period_text(tools.period, &tools.from, &tools.to);
        let unread = notice(&p, &period, "can't read T3", true, "Can't read the T3 database at ~/.t3/userdata/statev2.sqlite: database is locked.", &["Just looking? t3-agents --tools --demo shows a made-up week.".into()]);
        let mut hot = String::new();
        for cell in &unread.cells {
            if cell.fg == p.hot {
                hot.push(cell.ch);
            }
            assert_ne!(cell.fg, p.warn);
            assert_ne!(cell.bg, p.hot);
            assert_ne!(cell.bg, p.warn);
        }
        assert_eq!(hot, "can't read T3");
    }

    #[test]
    fn round_2_readings_hold() {
        let p = Palette::default();
        let samples = [("sample", SAMPLE), ("month", MONTH), ("thin", THIN), ("large", LARGE), ("claude", CLAUDE)];
        for (name, json) in samples {
            let tools = value(json);
            let shell = tools.kinds.iter().find(|row| row.kind == Kind::Shell).map(|row| u64::from(row.calls)).unwrap_or(0);
            let other: u64 = tools.kinds.iter().filter(|row| row.kind != Kind::Shell).map(|row| u64::from(row.calls)).sum();
            let program_rows: Vec<&Row> = tools.programs.iter().collect();
            let unnamed = program_rows.iter().copied().find(|row| row.name.is_none());
            let (programs, program_rest) = shown(&program_rows, 7, 4, unnamed, 2);
            let program_calls: u64 = programs.iter().chain(program_rest.iter()).map(|row| u64::from(row.calls)).sum();
            assert_eq!(program_calls, shell, "{name}: the program table adds up to the commands kind");
            assert_eq!(program_calls, u64::from(tools.commands.calls), "{name}: the program table adds up to the commands");
            let (named, nameless) = tool_parts(&tools);
            let (tool_rows, tool_rest) = shown(&named, 4, 3, None, 0);
            let tool_calls: u64 = tool_rows.iter().chain(tool_rest.iter()).chain(nameless.iter()).map(|row| u64::from(row.calls)).sum();
            assert_eq!(tool_calls, other, "{name}: the tools table adds up to the other kinds");
            let drawn: u64 = why_rows(&tools).iter().map(|row| u64::from(row.count)).sum();
            assert_eq!(drawn, u64::from(tools.commands.failed), "{name}: why rows add up to the commands marked failed");
            let report80 = plain(&report(&tools, &p, Source::Records, false));
            let report120 = plain(&wide(&tools, &p, Source::Records, false));
            if tools.commands.exits.failed_known > 0 {
                let right = format!("{} of the {} have an exit code", count(tools.commands.exits.failed_known), count(tools.commands.failed));
                assert!(report80.contains(&right), "{name} 80: {right}");
                assert!(report120.contains(&right), "{name} 120: {right}");
            }
            for provider in &tools.providers {
                let lack = provider.commands_failed.saturating_sub(provider.exits.failed_known);
                if provider.provider.is_empty() || provider.provider == "unknown" || lack == 0 || u64::from(lack) * 2 <= u64::from(provider.commands_failed) {
                    continue;
                }
                let named = format!("Of {}'s", provider.provider);
                assert!(report80.contains(&named), "{name} 80 does not name {}", provider.provider);
                assert!(report120.contains(&named), "{name} 120 does not name {}", provider.provider);
            }
        }
        let claude = plain(&report(&value(CLAUDE), &p, Source::Records, false));
        assert!(claude.contains("is not known"), "{claude}");
        let thin = report(&value(THIN), &p, Source::Records, false);
        let thin_text = plain(&thin);
        let mkdir = thin_text.lines().find(|line| line.contains("mkdir")).expect("mkdir");
        assert!(mkdir.contains("unknown"), "{mkdir}");
        let y = (0..thin.h).find(|&row| thin.row_text(row).contains("mkdir")).unwrap();
        let at = cell_at(&thin, y, "unknown");
        for (i, _) in "unknown".chars().enumerate() {
            let cell = thin.get((at + i) as i32, y as i32).unwrap();
            assert!(cell.fg == p.dim && !cell.bold, "unknown {:?}", cell.ch);
        }
        let week = value(SAMPLE);
        assert_eq!(clock_note(&week), "leaves out 308 calls");
        assert_eq!(clock_note(&value(MONTH)), "leaves out 1,172 calls");
        assert!(terminal::width(&clock_note(&value(MONTH))) <= 23);
        assert_eq!(clock_note(&value(CLAUDE)), "the same on the clock");
        let wide_week = plain(&wide(&week, &p, Source::Records, false));
        assert!(wide_week.lines().any(|line| line.contains("python3") && line.contains("timed out") && line.contains("594abb01")));
        assert!(wide_week.lines().any(|line| line.contains("cargo") && line.contains("failed") && line.contains("dca80ad3")));
        assert!(wide_week.lines().any(|line| line.contains("pytest") && line.contains("exit 1") && line.contains("3229ef32")));
        assert!(wide_week.lines().any(|line| line.contains("docker") && line.contains("stopped") && line.contains("e7573e38")));
        assert!(wide_week.lines().any(|line| line.contains("grep") && line.contains("found nothing") && line.contains("3229ef32")));
        for thread in ["5ed63f28", "31b954b4", "1d671adc", "f6a93962", "9f27b31d"] {
            let line = wide_week.lines().find(|line| line.contains(thread)).unwrap_or_else(|| panic!("no {thread}"));
            for word in ["timed out", "exit 1", "stopped", "found nothing", "failed", "marked failed"] {
                assert!(!line.contains(word), "{thread} shows {word}: {line}");
            }
        }
        let large80 = plain(&report(&value(LARGE), &p, Source::Records, false));
        assert!(large80.contains("list_pull_…ead_state"), "{large80}");
        assert!(large80.contains("search exa") && large80.contains("search notion"), "{large80}");
        let large120 = wide(&value(LARGE), &p, Source::Records, false);
        let large_text = plain(&large120);
        assert!(large_text.contains("search exa") && large_text.contains("search notion"), "{large_text}");
        let caption = large120.row_text(6);
        assert!(caption.trim_end().ends_with("· claude"), "{caption}");
    }

    #[test]
    fn a_made_up_week_is_stable() {
        let local: recap::Local = &crate::platform::local_time;
        let period = recap::Period::new(1_794_484_800_000.0, recap::Which::LastWeek, local);
        let once = crate::tools::demo(&period, local).unwrap();
        let twice = crate::tools::demo(&period, local).unwrap();
        let p = Palette::default();
        let a = plain(&report(&once, &p, Source::Demo, false));
        let b = plain(&report(&twice, &p, Source::Demo, false));
        assert_eq!(a, b);
        assert_eq!(plain(&wide(&once, &p, Source::Demo, false)), plain(&wide(&twice, &p, Source::Demo, false)));
        assert_eq!(list(&once, Source::Demo, 60), list(&twice, Source::Demo, 60));
        fits(&a, 80);
        fits(&plain(&wide(&once, &p, Source::Demo, false)), 120);
    }

    #[test]
    fn empty_and_one_call_draw() {
        let p = Palette::default();
        let mut empty = Tools { period: recap::Span::Week, from: "2026-11-09".into(), to: "2026-11-15".into(), days: 7, min_for_rate: crate::tools::MIN_FOR_RATE, ..Tools::default() };
        let _ = plain(&report(&empty, &p, Source::Records, false));
        let _ = plain(&wide(&empty, &p, Source::Records, false));
        empty.calls = 1;
        empty.ms = 1_500.0;
        empty.busy_ms = 1_500.0;
        empty.commands.calls = 1;
        empty.kinds = vec![Row { kind: Kind::Shell, name: Some("cargo".into()), calls: 1, ms: 1_500.0, ..Row::default() }];
        empty.lengths = (0..5).map(|i| Bucket { label: i.to_string(), calls: u32::from(i == 1), ms: if i == 1 { 1_500.0 } else { 0.0 }, ..Bucket::default() }).collect();
        // One command, and a blank start time: a 12-column "not recorded" otherwise runs into
        // "no commands" at column 24 and into "started" at column 44, which the frames never show.
        empty.providers = vec![Provider { provider: String::new(), calls: 1, commands: 1, ms: 1_500.0, ..Provider::default() }];
        empty.longest = vec![Long { kind: Kind::Shell, ms: 1_500.0, provider: String::new(), thread: "abcdefghijklmnop".into(), ..Long::default() }];
        for canvas in [report(&empty, &p, Source::Records, false), wide(&empty, &p, Source::Records, false)] {
            let text = plain(&canvas);
            assert!(text.contains("not recorded"), "{text}");
            assert!(text.contains("ijklmnop"), "{text}");
            assert!(text.lines().filter(|line| line.contains("not recorded")).all(|line| !line.contains('●')));
        }
        assert!(list(&empty, Source::Records, 60).contains("not recorded"));
    }

    fn samples() -> Vec<(&'static str, Tools)> {
        [("sample", SAMPLE), ("month", MONTH), ("thin", THIN), ("large", LARGE), ("claude", CLAUDE), ("quiet", QUIET), ("open", OPEN)].into_iter().map(|(name, json)| (name, value(json))).collect()
    }

    fn assert_no_shell_prompt(name: &str, text: &str) {
        for (i, line) in frame_lines(text).iter().enumerate() {
            assert!(!line.trim_start().starts_with('~'), "{name} line {} starts with ~: {line}", i + 1);
            assert!(!line.contains('$'), "{name} line {} contains $: {line}", i + 1);
        }
    }

    #[test]
    fn no_output_ends_on_a_prompt() {
        let p = Palette::default();
        let local: recap::Local = &crate::platform::local_time;
        let period = recap::Period::new(1_794_484_800_000.0, recap::Which::LastWeek, local);
        let mut drawn = samples();
        drawn.push(("demo", crate::tools::demo(&period, local).unwrap()));
        // The last line names no path: the cache's folder differs between machines.
        assert!(!LAST_NOTE.contains(['~', '/']) && !LAST_NOTE.contains(".cache"), "{LAST_NOTE}");
        for (name, tools) in &drawn {
            let report80 = plain(&report(tools, &p, Source::Records, false));
            let report120 = plain(&wide(tools, &p, Source::Records, false));
            let listed = list(tools, Source::Records, 60);
            assert_no_shell_prompt(&format!("{name} 80"), &report80);
            assert_no_shell_prompt(&format!("{name} 120"), &report120);
            assert_no_shell_prompt(&format!("{name} list"), &listed);
            assert!(frame_lines(&report80).last().is_some_and(|line| line.trim_end() == " your cache folder; nothing is sent."), "{name} 80 ends on {report80}");
            assert!(frame_lines(&report120).last().is_some_and(|line| line.trim_end() == format!(" {LAST_NOTE}")), "{name} 120");
            assert!(frame_lines(&listed).last().is_some_and(|line| !line.is_empty()), "{name} list");
            let when = recap_view::period_text(tools.period, &tools.from, &tools.to);
            let empty = notice(&p, &when, "no tool calls", false, &format!("No tool calls recorded {when}, or this week so far."), &["Tools reads the tool calls T3 Code has recorded on this machine.".into(), "Just looking? t3-agents --tools --demo shows a made-up week.".into()]);
            let unread = notice(&p, &when, "can't read T3", true, "Can't read the T3 database at ~/.t3/userdata/statev2.sqlite: database is locked.", &["Just looking? t3-agents --tools --demo shows a made-up week.".into()]);
            for (which, canvas) in [("empty", empty), ("unreadable", unread)] {
                let text = plain(&canvas);
                assert_no_shell_prompt(&format!("{name} {which}"), &text);
                assert!(frame_lines(&text).last().is_some_and(|line| line.trim().is_empty()), "{name} {which} does not end on a blank row");
            }
        }
    }

    #[test]
    fn round_3_readings_hold() {
        let p = Palette::default();
        let bars = ['█', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
        for (name, tools) in samples() {
            for (columns, canvas) in [(80usize, report(&tools, &p, Source::Records, false)), (120, wide(&tools, &p, Source::Records, false))] {
                let text = plain(&canvas);
                for banned in ["– of ", "come to –", "– on the clock"] {
                    assert!(!text.contains(banned), "{name} {columns} has {banned}");
                }
                for line in text.lines() {
                    let chars: Vec<char> = line.chars().collect();
                    assert!(!chars.windows(2).any(|pair| bars.contains(&pair[0]) && pair[1] == 'u'), "{name} {columns}: unknown sits on a bar: {line}");
                }
                let caption = canvas.row_text(6);
                let end = if columns == 80 { 80 } else { 90 };
                assert!(!caption.chars().take(end).any(|ch| ch == '…'), "{name} {columns} caption is cut: {caption}");
            }
            let listed = list(&tools, Source::Records, 60);
            for line in listed.lines() {
                assert!(terminal::width(line) <= 60, "{name} list line is wider than 60: {line}");
            }
            let report80 = plain(&report(&tools, &p, Source::Records, false));
            let tools_at = report80.find("TOOLS BY NAME").expect("tools");
            let providers_at = report80[tools_at..].find("PROVIDERS").map(|at| tools_at + at).unwrap_or(report80.len());
            let section = &report80[tools_at..providers_at];
            for line in section.lines() {
                if line.contains("file changes") || line.contains("file searches") || line.contains("web searches") {
                    assert!(line.contains("no name"), "{name}: a tools row repeats a kind: {line}");
                }
            }
            let (_, nameless) = tool_parts(&tools);
            if nameless.is_empty() {
                assert!(!section.contains("no name"), "{name}");
            } else {
                let items = nameless.iter().map(|row| format!("{} {}", unnamed_tool(row.kind), count(row.calls))).collect::<Vec<_>>().join(" · ");
                assert!(section.contains(&format!("no name  {items}")), "{name}: no name line is not {items}");
            }
            let program_rows: Vec<&Row> = tools.programs.iter().collect();
            let unnamed = program_rows.iter().copied().find(|row| row.name.is_none());
            for (by_time, by_calls, by_failed, limit) in [(7usize, 4, 2, 14usize), (10, 5, 3, 19)] {
                let (head, rest) = shown(&program_rows, by_time, by_calls, unnamed, by_failed);
                if rest.is_empty() {
                    assert!(head.len() <= limit + 1, "{name}: {} rows and no fold", head.len());
                } else {
                    assert!(head.len() <= limit, "{name}: {} rows plus a fold", head.len());
                }
            }
            if tools.commands.failed == 0 && tools.commands.calls > 0 {
                assert!(report80.contains("No command was marked failed."), "{name}");
            }
        }
        let week = plain(&report(&value(SAMPLE), &p, Source::Records, false));
        let wide_week = plain(&wide(&value(SAMPLE), &p, Source::Records, false));
        assert!(week.lines().any(|line| line.contains("rg") && line.contains("108") && !line.contains("grep")), "{week}");
        assert!(wide_week.lines().any(|line| line.contains("rg") && line.contains("108") && !line.contains("grep")), "{wide_week}");
        let quiet80 = plain(&report(&value(QUIET), &p, Source::Records, false));
        let quiet120 = plain(&wide(&value(QUIET), &p, Source::Records, false));
        assert!(quiet80.contains("openro…-480b"), "{quiet80}");
        assert!(quiet120.contains("openro…-480b"), "{quiet120}");
        assert!(quiet120.contains("openrouter-q…-coder-480b"), "{quiet120}");
        assert!(quiet80.contains("of 40 calls"), "{quiet80}");
        assert!(plain(&report(&value(OPEN), &p, Source::Records, false)).contains("of 5 calls"));
    }
}
