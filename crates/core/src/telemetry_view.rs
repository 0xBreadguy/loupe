//! Shared Telemetry Check renderer: 80-col card, 120-col wide card, 39-col narrow list,
//! notices, and the cached interactive board (`TelemetryView`) inside `loupe`.
//!
//! Uses only Loupe's palette roles (`bg`, `panel`, `fg`, `text2`, `dim`, `rule`, `bright`,
//! `accent`, `hot`, `warn`, `sel`) so every built-in and custom theme renders cleanly, and
//! every state carries a distinct `(glyph, word)` pair so `NO_COLOR` / piped plain text
//! preserves full meaning.

use crate::model::State;
use crate::overview::{self, HeaderBadge};
use crate::telemetry::{
    AuthFilesHashed, Confidence, Finding, Liveness, NotChecked, Read, ReadOutcome, Report,
    Severity, Snippet, Source, Stream, StreamId, StreamState, Verdict, format_header_time,
    format_short_date,
};
use crate::terminal::{self, Canvas, Palette, Rgb};
use std::hash::{Hash, Hasher};

pub const CARD_COLUMNS: usize = 80;
pub const WIDE_COLUMNS: usize = 120;
pub const LIST_COLUMNS: usize = 39;
pub const MIN_COLUMNS: usize = 52;
pub const MIN_ROWS: usize = 14;
const COMMAND: &str = "loupe --telemetry-check";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Telemetry,
    Sources,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Rows(isize),
    Page(isize),
    Start,
    End,
}

/// Interactive T3 handoff banner state (shared across Loupe views).
pub use crate::handoff::HandoffState;

/// Glyphs chosen to stay 1 terminal column wide across Unicode terminals.
pub fn restart_glyph() -> char {
    if terminal::width("↻") == 1 {
        '↻'
    } else {
        '↩'
    }
}

pub fn verdict_style(verdict: Verdict, p: &Palette) -> (char, Rgb, Rgb) {
    match verdict {
        Verdict::Off => ('✓', p.accent, p.bright),
        Verdict::Blocked => ('⊘', p.text2, p.fg),
        Verdict::On | Verdict::Partial => ('●', p.warn, p.warn),
        Verdict::Misconfigured => ('✗', p.hot, p.hot),
        Verdict::RestartNeeded => (restart_glyph(), p.warn, p.warn),
        Verdict::Unverified => ('?', p.text2, p.fg),
    }
}

pub fn stream_style(state: StreamState, p: &Palette) -> (char, &'static str, Rgb, Rgb, bool) {
    match state {
        StreamState::Off => ('✓', "off", p.accent, p.bright, true),
        StreamState::Blocked => ('⊘', "blocked", p.text2, p.fg, false),
        StreamState::On => ('●', "on", p.warn, p.warn, false),
        StreamState::OptOutIgnored => ('✗', "ignored", p.hot, p.hot, true),
        StreamState::RestartNeeded => (restart_glyph(), "restart", p.warn, p.warn, true),
        StreamState::Unverified => ('?', "unknown", p.text2, p.fg, false),
        StreamState::None => ('○', "none", p.dim, p.dim, false),
    }
}

fn custom_stream_style(stream: &Stream, p: &Palette) -> (char, String, Rgb, Rgb, bool) {
    if stream.exporters.is_empty() {
        ('○', "none".into(), p.dim, p.dim, false)
    } else if stream.state == StreamState::Off {
        ('○', "off".into(), p.dim, p.text2, false)
    } else {
        ('○', "on".into(), p.accent, p.fg, false)
    }
}

fn stream_visual(stream: &Stream, p: &Palette) -> (char, String, Rgb, Rgb, bool) {
    if stream.id == StreamId::CustomExporters {
        return custom_stream_style(stream, p);
    }
    if stream.state == StreamState::Unverified {
        if stream.decided_detail.contains("shell") {
            return ('?', "unverif".into(), p.text2, p.fg, false);
        }
        if stream.decided_detail.contains("unreadable") {
            return ('?', "locked".into(), p.warn, p.warn, false);
        }
        if stream.decided_detail.contains("macOS") {
            return ('?', "profile".into(), p.text2, p.fg, false);
        }
        if stream.decided_detail.contains("invalid") || stream.decided_detail.contains("both") {
            return ('✗', "invalid".into(), p.warn, p.warn, false);
        }
    }
    let (g, w, gi, wi, b) = stream_style(stream.state, p);
    (g, w.to_owned(), gi, wi, b)
}

fn board_stream_badge(stream: &Stream, p: &Palette) -> (char, &'static str, Rgb, Rgb, bool) {
    if stream.id == StreamId::CustomExporters {
        return if stream.exporters.is_empty() {
            ('○', "none", p.dim, p.dim, false)
        } else if stream.state == StreamState::Off {
            ('○', "off", p.dim, p.text2, false)
        } else {
            ('○', "active", p.accent, p.fg, false)
        };
    }
    if stream.state == StreamState::Unverified {
        if stream.decided_detail.contains("unreadable") {
            return ('?', "locked", p.warn, p.warn, true);
        }
        if stream.decided_detail.contains("macOS") {
            return ('?', "profile", p.text2, p.fg, false);
        }
        if stream.decided_detail.contains("invalid") || stream.decided_detail.contains("both") {
            return ('✗', "invalid", p.warn, p.warn, true);
        }
        if stream.decided_detail.contains("desktop skips it") {
            return ('!', "shell only", p.warn, p.warn, true);
        }
        return ('?', "unverified", p.text2, p.fg, false);
    }
    let (g, w, gi, wi, b) = stream_style(stream.state, p);
    (g, w, gi, wi, b || stream.state == StreamState::On)
}

fn width(s: &str) -> i32 {
    terminal::width(s) as i32
}

fn fit(s: &str, max: i32) -> String {
    terminal::fit(&terminal::clean(s), max.max(0) as usize)
}

fn wrap_lines(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(12);
    let cleaned = terminal::clean(text);
    let mut lines = Vec::new();
    for para in cleaned.split('\n') {
        let mut cur = String::new();
        let mut cur_w = 0usize;
        for word in para.split_whitespace() {
            let ww = terminal::width(word);
            if cur.is_empty() {
                if ww <= max_width {
                    cur.push_str(word);
                    cur_w = ww;
                } else {
                    // Long token (e.g. file path or URL): split by fitting chunks.
                    let mut rest = word;
                    while !rest.is_empty() {
                        let chunk = take_width(rest, max_width);
                        lines.push(chunk.to_owned());
                        rest = &rest[chunk.len()..];
                    }
                }
            } else if cur_w + 1 + ww <= max_width {
                cur.push(' ');
                cur.push_str(word);
                cur_w += 1 + ww;
            } else {
                lines.push(std::mem::take(&mut cur));
                if ww <= max_width {
                    cur.push_str(word);
                    cur_w = ww;
                } else {
                    let mut rest = word;
                    while !rest.is_empty() {
                        let chunk = take_width(rest, max_width);
                        if rest.len() == chunk.len() {
                            cur.push_str(chunk);
                            cur_w = terminal::width(chunk);
                        } else {
                            lines.push(chunk.to_owned());
                        }
                        rest = &rest[chunk.len()..];
                    }
                }
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn take_width(s: &str, max_cells: usize) -> &str {
    let mut used = 0usize;
    let mut end = 0usize;
    for (idx, ch) in s.char_indices() {
        let mut buf = [0u8; 4];
        let cw = terminal::width(ch.encode_utf8(&mut buf));
        if used + cw > max_cells && end > 0 {
            break;
        }
        used += cw;
        end = idx + ch.len_utf8();
    }
    if end == 0 {
        s.char_indices()
            .nth(1)
            .map_or(s, |(i, _)| &s[..i])
    } else {
        &s[..end]
    }
}

/// Formats a shell command for display without ever breaking inside quotes or JSON.
///
/// - If it fits on one line, returns it verbatim.
/// - Otherwise splits only at top-level shell word boundaries outside quotes, appending
///   ` \` line continuations so copy-pasting executes identically in POSIX sh/bash/zsh.
/// - If any single quoted/unbreakable token exceeds `max_width` (e.g. narrow terminals),
///   returns a single clipped line with `clipped = true` so callers tell the user to run
///   `loupe --telemetry-check --json` instead of offering broken JSON.
fn format_command_lines(cmd: &str, max_width: usize) -> (Vec<String>, bool) {
    let cleaned = terminal::clean(cmd);
    let max_width = max_width.max(12);
    if terminal::width(&cleaned) <= max_width {
        return (vec![cleaned], false);
    }

    // Plain-English instruction (not a shell command) can wrap normally.
    if !cleaned.contains('\'') && !cleaned.contains('"') && !cleaned.contains('|') && !cleaned.contains('>') && !cleaned.contains("&&") {
        return (wrap_lines(&cleaned, max_width), false);
    }

    // Tokenize at top-level whitespace outside single/double quotes.
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in cleaned.chars() {
        if escaped {
            cur.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && in_double {
            cur.push(ch);
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            cur.push(ch);
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            cur.push(ch);
            continue;
        }
        if ch.is_whitespace() && !in_single && !in_double {
            if !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur));
            }
        } else {
            cur.push(ch);
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }

    // If any single token (plus ` \` continuation if not last) won't fit, refuse to break it.
    let last_idx = tokens.len().saturating_sub(1);
    for (i, tok) in tokens.iter().enumerate() {
        let extra = if i < last_idx { 2 } else { 0 };
        if terminal::width(tok) + extra > max_width {
            return (vec![fit(&cleaned, max_width as i32)], true);
        }
    }

    // Prefer splitting before `|`, `&&`, `>>`, `>` by packing tokens greedily into lines,
    // keeping a 2-column margin for ` \` on every line except the last.
    let mut raw_lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for (i, tok) in tokens.iter().enumerate() {
        let reserve = if i < last_idx { 2 } else { 0 };
        let candidate = if line.is_empty() {
            tok.clone()
        } else {
            format!("{line} {tok}")
        };
        if !line.is_empty() && terminal::width(&candidate) + reserve > max_width {
            raw_lines.push(std::mem::take(&mut line));
            line = tok.clone();
        } else {
            line = candidate;
        }
    }
    if !line.is_empty() {
        raw_lines.push(line);
    }

    let n = raw_lines.len();
    for (i, l) in raw_lines.iter_mut().enumerate() {
        if i + 1 < n {
            l.push_str(" \\");
        }
    }
    (raw_lines, false)
}

fn source_counts(report: &Report) -> (usize, usize, usize) {
    let mut files_read = 0usize;
    let mut stat_only = 0usize;
    let mut absent_count = 0usize;
    for r in &report.inputs.reads {
        match r.outcome {
            ReadOutcome::Read if r.name != "environment" => files_read += 1,
            ReadOutcome::StatOnly => stat_only += 1,
            ReadOutcome::Absent => absent_count += 1,
            _ => {}
        }
    }
    (files_read, stat_only, absent_count)
}

fn sources_badge(report: &Report) -> String {
    let (read, stat, absent) = source_counts(report);
    if stat > 0 {
        format!("{read} read · {stat} stat · {absent} absent")
    } else {
        format!("{read} read · {absent} absent")
    }
}

fn provenance_line(report: &Report, narrow: bool) -> String {
    let (files, stat, _) = source_counts(report);
    if narrow {
        format!("read {files} files · sent nothing")
    } else if stat > 0 {
        format!("read {files} local files · {stat} stat only · nothing sent · nothing written")
    } else {
        format!("read {files} local files · nothing sent · nothing written")
    }
}

fn highlight_stream_index(report: &Report) -> usize {
    if let Some(f) = report.findings.iter().find(|f| f.severity.is_to_fix()) {
        if let Some(st) = f.stream {
            if let Some(i) = report.streams.iter().position(|s| s.id == st) {
                return i;
            }
        }
    }
    report
        .streams
        .iter()
        .position(|s| {
            s.counts_toward_verdict
                && matches!(
                    s.state,
                    StreamState::OptOutIgnored | StreamState::RestartNeeded | StreamState::On
                )
        })
        .unwrap_or(0)
}

// ── CLI Cards (80 / 120 / 39 columns) ────────────────────────────────────────

/// 80-column themed card (`loupe --telemetry-check` default for 80..119 columns or `--compact`).
pub fn card(report: &Report, p: &Palette, _mono: bool) -> Canvas {
    let w = CARD_COLUMNS;
    let mut c = Canvas::cleared(w, 80, p.bg, p.fg);
    let ww = w as i32;
    let mut y = 0i32;

    // Header row
    c.fill(0, y, ww, 1, p.panel);
    let mut x = 1 + c.text_bold(1, y, "Loupe", p.bright) + 2;
    x += c.text_bg(x, y, " TELEMETRY ", p.bg, p.text2, true) + 2;
    let when = format_header_time(report.inputs.checked_at);
    x += c.text(x, y, &when, p.dim) + 2;
    let src_note = if report.inputs.source == Source::Demo {
        "demo data · made up"
    } else {
        "read-only"
    };
    let max_src = (ww - 1 - width(COMMAND) - 2 - x).max(0);
    if max_src >= 6 {
        c.text(x, y, &fit(src_note, max_src), p.dim);
    }
    c.right(ww - 1, y, COMMAND, p.dim);
    y += 2;

    // Verdict strip (2 rows on panel)
    let (v_glyph, v_g_ink, v_t_ink) = verdict_style(report.verdict, p);
    c.fill(0, y, ww, 2, p.panel);
    c.set(1, y, '▌', v_t_ink, Some(p.panel), false);
    c.set(1, y + 1, '▌', v_t_ink, Some(p.panel), false);
    c.set(3, y, v_glyph, v_g_ink, Some(p.panel), true);
    c.text_bg(5, y, &fit(report.verdict_heading(), ww - 6), v_t_ink, p.panel, true);
    c.text_bg(5, y + 1, &fit(&report.summary, ww - 6), p.fg, p.panel, false);
    y += 3;

    // Streams section
    terminal::section_with(&mut c, 1, y, ww - 2, "WHAT T3 CODE SENDS", p, "decided by", p.dim);
    y += 1;
    for s in &report.streams {
        draw_card_stream_row(&mut c, 1, y, ww - 2, s, p, false);
        y += 1;
    }
    y += 1;

    // Other settings (compact single row if it fits, else wrapped)
    if !report.other.is_empty() {
        terminal::section_with(&mut c, 1, y, ww - 2, "OTHER SETTINGS", p, "settings.json", p.dim);
        y += 1;
        let parts: Vec<String> = report
            .other
            .iter()
            .map(|o| {
                if o.id == "install_id" {
                    format!("· {} {}", o.label, o.value)
                } else {
                    format!("· {}: {}", o.label, o.value)
                }
            })
            .collect();
        let joined = parts.join("   ");
        if width(&joined) <= ww - 2 {
            let mut ox = 1;
            for (idx, o) in report.other.iter().enumerate() {
                if idx > 0 {
                    ox += 3;
                }
                c.set(ox, y, '·', p.dim, None, false);
                ox += 2;
                if o.id == "install_id" {
                    ox += c.text(ox, y, o.label, p.text2) + 1;
                    ox += c.text(ox, y, &o.value, p.fg);
                } else {
                    ox += c.text(ox, y, &format!("{}: ", o.label), p.text2);
                    ox += c.text(ox, y, &o.value, p.fg);
                }
            }
            y += 1;
        } else {
            for line in wrap_lines(&joined, (ww - 2) as usize) {
                c.text(1, y, &line, p.text2);
                y += 1;
            }
        }
        y += 1;
    }

    // Findings
    if !report.findings.is_empty() {
        let to_fix = report.findings.iter().filter(|f| f.severity.is_to_fix()).count();
        let right = if to_fix > 0 {
            format!("{to_fix} to fix")
        } else {
            "notes".into()
        };
        let right_ink = if report.findings.iter().any(|f| f.severity == Severity::OptOutIgnored) {
            p.hot
        } else if to_fix > 0 {
            p.warn
        } else {
            p.dim
        };
        terminal::section_with(&mut c, 1, y, ww - 2, "FINDINGS", p, &right, right_ink);
        y += 1;
        for f in &report.findings {
            y = draw_finding(&mut c, 1, y, ww - 2, f, p);
        }
        y += 1;
    }

    // Not checked here
    if !report.not_checked.is_empty() {
        terminal::section_with(&mut c, 1, y, ww - 2, "NOT CHECKED HERE", p, "", p.dim);
        y += 1;
        for nc in &report.not_checked {
            draw_not_checked_row(&mut c, 1, y, ww - 2, nc, p);
            y += 1;
        }
        y += 1;
    }

    // Primary fix snippet
    if let Some(snip) = report.fix.first() {
        terminal::section_with(
            &mut c,
            1,
            y,
            ww - 2,
            &snip.heading,
            p,
            "Loupe changes nothing",
            p.dim,
        );
        y += 1;
        let (cmd_lines, clipped) = format_command_lines(&snip.command, (ww - 4) as usize);
        c.fill(1, y, ww - 2, cmd_lines.len() as i32, p.panel);
        for line in &cmd_lines {
            c.text_bg(2, y, line, p.fg, p.panel, false);
            y += 1;
        }
        if clipped {
            c.text(1, y, "full command: loupe --telemetry-check --json", p.dim);
            y += 1;
        }
        for line in wrap_lines(&snip.note, (ww - 2) as usize) {
            c.text(1, y, &line, p.dim);
            y += 1;
        }
        y += 1;
    }

    // Provenance footer
    c.text(1, y, &fit(&provenance_line(report, false), ww - 2), p.dim);
    y += 1;

    crop_canvas(&c, w, y as usize, p)
}

/// 120-column wide card (`loupe --telemetry-check --wide` or terminal >= 120 cols).
pub fn wide(report: &Report, p: &Palette, _mono: bool) -> Canvas {
    let w = WIDE_COLUMNS;
    let mut c = Canvas::cleared(w, 80, p.bg, p.fg);
    let ww = w as i32;
    let mut y = 0i32;

    // Header
    c.fill(0, y, ww, 1, p.panel);
    let mut x = 1 + c.text_bold(1, y, "Loupe", p.bright) + 2;
    x += c.text_bg(x, y, " TELEMETRY ", p.bg, p.text2, true) + 2;
    let when = format_header_time(report.inputs.checked_at);
    x += c.text(x, y, &when, p.dim) + 2;
    let src_note = if report.inputs.source == Source::Demo {
        "demo data · made up"
    } else {
        "read-only"
    };
    c.text(x, y, src_note, p.dim);
    c.right(ww - 1, y, COMMAND, p.dim);
    y += 2;

    // Verdict strip (1 row on 120-col)
    let (v_glyph, v_g_ink, v_t_ink) = verdict_style(report.verdict, p);
    c.fill(0, y, ww, 1, p.panel);
    c.set(1, y, '▌', v_t_ink, Some(p.panel), false);
    c.set(3, y, v_glyph, v_g_ink, Some(p.panel), true);
    let vx = 5 + c.text_bg(5, y, report.verdict_heading(), v_t_ink, p.panel, true) + 3;
    if ww - 1 > vx {
        c.text_bg(vx, y, &fit(&report.summary, ww - 1 - vx), p.fg, p.panel, false);
    }
    y += 2;

    let left_x = 1i32;
    let left_w = 72i32;
    let right_x = left_x + left_w + 2;
    let right_w = ww - right_x - 1;

    // ── Left column ──────────────────────────────────────────────
    let mut ly = y;
    terminal::section_with(&mut c, left_x, ly, left_w, "WHAT T3 CODE SENDS", p, "how sure", p.dim);
    ly += 1;
    for s in &report.streams {
        draw_card_stream_row(&mut c, left_x, ly, left_w, s, p, true);
        ly += 1;
    }
    ly += 1;

    if !report.other.is_empty() {
        terminal::section_with(&mut c, left_x, ly, left_w, "OTHER SETTINGS", p, "settings.json", p.dim);
        ly += 1;
        let parts: Vec<String> = report
            .other
            .iter()
            .map(|o| {
                if o.id == "install_id" {
                    format!("· {} {}", o.label, o.value)
                } else {
                    format!("· {}: {}", o.label, o.value)
                }
            })
            .collect();
        let joined = parts.join("  ");
        for line in wrap_lines(&joined, left_w as usize) {
            c.text(left_x, ly, &line, p.text2);
            ly += 1;
        }
        ly += 1;
    }

    if !report.findings.is_empty() {
        let to_fix = report.findings.iter().filter(|f| f.severity.is_to_fix()).count();
        let right = if to_fix > 0 {
            format!("{to_fix} to fix")
        } else {
            "notes".into()
        };
        let right_ink = if report.findings.iter().any(|f| f.severity == Severity::OptOutIgnored) {
            p.hot
        } else if to_fix > 0 {
            p.warn
        } else {
            p.dim
        };
        terminal::section_with(&mut c, left_x, ly, left_w, "FINDINGS", p, &right, right_ink);
        ly += 1;
        for f in &report.findings {
            ly = draw_finding(&mut c, left_x, ly, left_w, f, p);
        }
        ly += 1;
    }

    if !report.not_checked.is_empty() {
        terminal::section_with(&mut c, left_x, ly, left_w, "NOT CHECKED HERE", p, "", p.dim);
        ly += 1;
        for nc in &report.not_checked {
            draw_not_checked_row(&mut c, left_x, ly, left_w, nc, p);
            ly += 1;
        }
        ly += 1;
    }

    // ── Right column: Decision ladder + Sources + Fix ────────────
    let mut ry = y;
    let hi = highlight_stream_index(report);
    if let Some(stream) = report.streams.get(hi) {
        let head = format!("HOW T3 DECIDES: {}", stream.id.label().to_ascii_uppercase());
        terminal::section_with(&mut c, right_x, ry, right_w, &fit(&head, right_w - 4), p, "", p.dim);
        ry += 1;
        ry = draw_ladder(&mut c, right_x, ry, right_w, stream, p);
        ry += 1;
    }

    let src_right = sources_badge(report);
    terminal::section_with(&mut c, right_x, ry, right_w, "SOURCES", p, &src_right, p.dim);
    ry += 1;
    for rec in &report.inputs.reads {
        draw_source_compact_row(&mut c, right_x, ry, right_w, rec, p);
        ry += 1;
    }
    ry += 1;

    if let Some(snip) = report.fix.first() {
        let right = if right_w >= 54 {
            "Loupe changes nothing"
        } else {
            "read-only"
        };
        terminal::section_with(&mut c, right_x, ry, right_w, &snip.heading, p, right, p.dim);
        ry += 1;
        let (cmd_lines, clipped) = format_command_lines(&snip.command, (right_w - 2) as usize);
        c.fill(right_x, ry, right_w, cmd_lines.len() as i32, p.panel);
        for line in &cmd_lines {
            c.text_bg(right_x + 1, ry, line, p.fg, p.panel, false);
            ry += 1;
        }
        if clipped {
            c.text(right_x, ry, &fit("full command: --telemetry-check --json", right_w), p.dim);
            ry += 1;
        }
        for line in wrap_lines(&snip.note, right_w as usize) {
            c.text(right_x, ry, &line, p.dim);
            ry += 1;
        }
        ry += 1;
    }

    let foot_y = ly.max(ry);
    c.text(1, foot_y, &fit(&provenance_line(report, false), ww - 2), p.dim);
    crop_canvas(&c, w, (foot_y + 1) as usize, p)
}

/// 39-column narrow list (`--narrow` or terminal < 80 cols).
pub fn list(report: &Report, p: &Palette, _mono: bool) -> Canvas {
    let w = LIST_COLUMNS;
    let mut c = Canvas::cleared(w, 96, p.bg, p.fg);
    let ww = w as i32;
    let mut y = 0i32;

    c.text_bold(1, y, "Loupe", p.bright);
    c.text_bg(8, y, " TELEMETRY ", p.bg, p.text2, true);
    let when = format_header_time(report.inputs.checked_at);
    c.right(ww - 1, y, &fit(&when, 12), p.dim);
    y += 1;
    let src = if report.inputs.source == Source::Demo {
        "demo data · made up"
    } else {
        "read-only"
    };
    c.text(1, y, src, p.dim);
    y += 2;

    // Verdict
    let (v_glyph, v_g_ink, v_t_ink) = verdict_style(report.verdict, p);
    c.set(0, y, '▌', v_t_ink, None, false);
    c.set(1, y, v_glyph, v_g_ink, None, true);
    c.text_bold(3, y, &fit(report.verdict_heading(), ww - 4), v_t_ink);
    y += 1;
    for line in wrap_lines(&report.summary, (ww - 2) as usize) {
        c.text(1, y, &line, p.fg);
        y += 1;
    }
    y += 1;

    // Streams (2 lines each)
    for s in &report.streams {
        let (glyph, word, g_ink, w_ink, bold) = stream_visual(s, p);
        c.set(1, y, glyph, g_ink, None, bold);
        c.text(3, y, &fit(s.id.short_label(), 16), p.fg);
        c.right(ww - 1, y, &fit(&word, 12), w_ink);
        y += 1;
        c.text(3, y, &fit(&s.decided_detail, ww - 4), p.dim);
        y += 1;
    }
    y += 1;

    // Findings
    for f in &report.findings {
        let mark_ink = match f.severity {
            Severity::OptOutIgnored => Some(p.hot),
            Severity::Restart | Severity::Warn => Some(p.warn),
            Severity::Caveat | Severity::Info => None,
        };
        let lines = wrap_lines(&f.message, (ww - 2) as usize);
        for (idx, line) in lines.iter().enumerate() {
            if let Some(ink) = mark_ink {
                if idx == 0 {
                    c.set(0, y, '▌', ink, None, false);
                }
            }
            c.text(1, y, line, if mark_ink.is_some() { p.fg } else { p.text2 });
            y += 1;
        }
    }
    if let Some(id_item) = report.other.iter().find(|o| o.id == "install_id") {
        c.text(1, y, &fit(&format!("· install ID {}", id_item.value), ww - 2), p.dim);
        y += 1;
    }
    y += 1;

    // Not checked
    for nc in &report.not_checked {
        for (idx, line) in wrap_lines(&format!("{}: {}", nc.label, nc.detail), (ww - 4) as usize)
            .iter()
            .enumerate()
        {
            if idx == 0 {
                c.set(1, y, '?', p.text2, None, false);
            }
            c.text(3, y, line, p.dim);
            y += 1;
        }
    }
    y += 1;

    // Fix
    if let Some(snip) = report.fix.first() {
        c.text(1, y, "Turn it off (Loupe changes nothing):", p.text2);
        y += 1;
        let (cmd_lines, clipped) = format_command_lines(&snip.command, (ww - 2) as usize);
        for line in &cmd_lines {
            c.text(1, y, line, p.fg);
            y += 1;
        }
        if clipped {
            c.text(1, y, &fit("full command: --telemetry-check --json", ww - 2), p.dim);
            y += 1;
        }
        for line in wrap_lines(&snip.note, (ww - 2) as usize) {
            c.text(1, y, &line, p.dim);
            y += 1;
        }
        y += 1;
    }

    c.text(1, y, &fit(&provenance_line(report, true), ww - 2), p.dim);
    crop_canvas(&c, w, (y + 1) as usize, p)
}

/// Standalone notice card (used when unavailable or on error).
pub fn notice(p: &Palette, cols: usize, rows: usize, title: &str, message: &str, tips: &[String]) -> Canvas {
    let w = cols.max(24);
    let h = rows.max(8);
    let mut c = Canvas::cleared(w, h, p.bg, p.fg);
    let ww = w as i32;
    c.fill(0, 0, ww, 1, p.panel);
    c.text_bold(1, 0, "Loupe", p.bright);
    c.text_bg(8, 0, " TELEMETRY ", p.bg, p.text2, true);
    let mut y = 2i32;
    c.text_bold(2, y, &fit(title, ww - 4), p.fg);
    y += 2;
    for line in wrap_lines(message, (ww - 4).max(16) as usize) {
        if y >= h as i32 - 2 {
            break;
        }
        c.text(2, y, &line, p.text2);
        y += 1;
    }
    y += 1;
    for tip in tips {
        for line in wrap_lines(tip, (ww - 4).max(16) as usize) {
            if y >= h as i32 - 2 {
                break;
            }
            c.text(2, y, &line, p.dim);
            y += 1;
        }
    }
    c.fill(0, h as i32 - 1, ww, 1, p.panel);
    let mut fx = 1;
    fx += c.text_bold(fx, h as i32 - 1, "e", p.accent) + 1;
    fx += c.text(fx, h as i32 - 1, "back", p.dim) + 2;
    fx += c.text_bold(fx, h as i32 - 1, "q", p.accent) + 1;
    c.text(fx, h as i32 - 1, "quit", p.dim);
    c
}

// ── Shared sub-renderers ─────────────────────────────────────────────────────

fn draw_card_stream_row(
    c: &mut Canvas,
    x: i32,
    y: i32,
    w: i32,
    s: &Stream,
    p: &Palette,
    show_confidence: bool,
) {
    let (glyph, word, g_ink, w_ink, bold) = stream_visual(s, p);
    c.set(x, y, glyph, g_ink, None, bold);
    c.text(x + 2, y, &fit(s.id.label(), 20), p.fg);
    let state_x = x + 23;
    if bold {
        c.text_bold(state_x, y, &fit(&word, 8), w_ink);
    } else {
        c.text(state_x, y, &fit(&word, 8), w_ink);
    }
    let detail_x = state_x + 9;
    let conf_w = if show_confidence {
        width(s.confidence.label()) + 1
    } else {
        0
    };
    let detail_w = (x + w - conf_w - detail_x).max(0);
    if detail_w > 0 {
        let d_ink = if s.state == StreamState::OptOutIgnored {
            p.hot
        } else {
            p.text2
        };
        c.text(detail_x, y, &fit(&s.decided_detail, detail_w), d_ink);
    }
    if show_confidence {
        c.right(x + w, y, s.confidence.label(), p.dim);
    }
}

fn draw_finding(c: &mut Canvas, x: i32, mut y: i32, w: i32, f: &Finding, p: &Palette) -> i32 {
    let margin_ink = match f.severity {
        Severity::OptOutIgnored => Some(p.hot),
        Severity::Restart | Severity::Warn => Some(p.warn),
        Severity::Caveat | Severity::Info => None,
    };
    let text_ink = if margin_ink.is_some() { p.fg } else { p.text2 };
    let lines = wrap_lines(&f.message, (w - 2).max(16) as usize);
    for (idx, line) in lines.iter().enumerate() {
        if let Some(ink) = margin_ink {
            if idx == 0 {
                c.set(x - 1, y, '▌', ink, None, false);
            }
        }
        c.text(x + 1, y, line, text_ink);
        y += 1;
    }
    y
}

fn draw_not_checked_row(c: &mut Canvas, x: i32, y: i32, w: i32, nc: &NotChecked, p: &Palette) {
    c.set(x, y, '?', p.text2, None, false);
    c.text(x + 2, y, &fit(nc.label, 18), p.text2);
    let dx = x + 21;
    if x + w > dx {
        c.text(dx, y, &fit(&nc.detail, x + w - dx), p.dim);
    }
}

fn draw_ladder(c: &mut Canvas, x: i32, mut y: i32, w: i32, s: &Stream, p: &Palette) -> i32 {
    for (idx, step) in s.ladder.iter().enumerate() {
        let num = format!("{}", idx + 1);
        c.text(x, y, &num, p.dim);
        let mark_w = if w >= 60 { 11 } else { 4 };
        let val_w = if w >= 60 { 18 } else { 11 };
        let step_w = (w - 3 - val_w - mark_w - 1).max(12);
        c.text(x + 2, y, &fit(&step.step, step_w), p.text2);
        let val_x = x + 2 + step_w + 1;
        let val_ink = if step.decisive { p.fg } else { p.dim };
        c.text(val_x, y, &fit(&step.value, val_w), val_ink);
        if step.decisive {
            let (g, word, g_ink, w_ink, bold) =
                stream_style(step.state_hint.unwrap_or(s.state), p);
            let raw_tag = if w >= 60 {
                format!("▸ {g} {word}")
            } else {
                format!("▸ {g}")
            };
            let tag = fit(&raw_tag, mark_w);
            if bold {
                c.text_bold(x + w - width(&tag), y, &tag, w_ink);
            } else {
                c.right(x + w, y, &tag, g_ink);
            }
        } else {
            c.right(x + w, y, "·", p.dim);
        }
        y += 1;
    }
    if s.ladder.is_empty() {
        c.text(x + 2, y, "no configured exporters", p.dim);
        y += 1;
    } else if !s.ladder.iter().any(|l| l.decisive) && s.counts_toward_verdict {
        let (_, word, _, w_ink, bold) = stream_style(s.state, p);
        let line = format!("= {word}: {}", s.decided_detail);
        if bold {
            c.text_bold(x, y, &fit(&line, w), w_ink);
        } else {
            c.text(x, y, &fit(&line, w), w_ink);
        }
        y += 1;
    }
    y
}

fn draw_source_compact_row(
    c: &mut Canvas,
    x: i32,
    y: i32,
    w: i32,
    rec: &crate::telemetry::ReadRecord,
    p: &Palette,
) {
    let (glyph, g_ink) = match rec.outcome {
        ReadOutcome::Read | ReadOutcome::StatOnly => ('✓', p.text2),
        ReadOutcome::Absent => ('·', p.dim),
        ReadOutcome::Unreadable | ReadOutcome::Invalid => ('△', p.warn),
        ReadOutcome::Never => ('✗', p.dim),
    };
    c.set(x, y, glyph, g_ink, None, false);
    let compact_path = if width(&rec.path) + width(&rec.detail) + 4 > w {
        rec.path
            .strip_prefix("~/.t3/userdata/")
            .or_else(|| rec.path.strip_prefix("~/.t3/"))
            .map(str::to_owned)
            .unwrap_or_else(|| match rec.path.as_str() {
                "this shell · T3's process tree" => "shell · process tree".into(),
                "secrets/, agent logins, environ" => "secrets/, logins, env".into(),
                other => other.to_owned(),
            })
    } else {
        rec.path.clone()
    };
    let detail_w = width(&rec.detail).min((w - 5) / 2).max(0);
    let path_w = (w - 2 - detail_w - 2).max(8);
    c.text(x + 2, y, &fit(&compact_path, path_w), p.fg);
    if detail_w > 0 {
        c.right(x + w, y, &fit(&rec.detail, detail_w), p.dim);
    }
}

fn crop_canvas(src: &Canvas, w: usize, h: usize, p: &Palette) -> Canvas {
    let h = h.clamp(1, src.h);
    let mut out = Canvas::cleared(w, h, p.bg, p.fg);
    for row in 0..h {
        let dst_off = row * w;
        let src_off = row * src.w;
        out.cells[dst_off..dst_off + w].copy_from_slice(&src.cells[src_off..src_off + w]);
    }
    out
}

// ── Interactive Board (`TelemetryView`, cached body like InsightsView) ───────

#[derive(Clone, Debug, PartialEq, Eq)]
struct BodyKey {
    revision: u64,
    cols: usize,
    compact_height: bool,
    tab: Tab,
    selected: usize,
    show_commands: bool,
    help: bool,
    palette_tag: u64,
}

fn palette_tag(p: &Palette) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.bg.hash(&mut h);
    p.panel.hash(&mut h);
    p.fg.hash(&mut h);
    p.text2.hash(&mut h);
    p.dim.hash(&mut h);
    p.faint.hash(&mut h);
    p.track.hash(&mut h);
    p.rule.hash(&mut h);
    p.bright.hash(&mut h);
    p.accent.hash(&mut h);
    p.hot.hash(&mut h);
    p.warn.hash(&mut h);
    p.sel.hash(&mut h);
    p.dark.hash(&mut h);
    h.finish()
}

fn row_ink(p: &Palette, role: Rgb, selected: bool) -> Rgb {
    if !selected {
        return role;
    }
    if role == p.hot || role == p.warn || role == p.accent {
        let target = if p.dark { 0xffffff } else { 0 };
        return crate::integration::reach(role, target, 4.5, &[p.sel]);
    }
    if role == p.dim {
        return p.text2;
    }
    if role == p.text2 {
        return p.fg;
    }
    role
}

/// Interactive board state and body canvas cache.
///
/// Reuses the cached body across Live wakes (header clock / snapshot ticks), so only
/// row 0 and row `h-1` repaint when nothing in the report, selection, or size changed.
#[derive(Default)]
pub struct TelemetryView {
    pub tab: Tab,
    pub selected: usize,
    pub scroll: usize,
    pub show_commands: bool,
    pub help: bool,
    body_key: Option<BodyKey>,
    body: Option<Canvas>,
    /// `(body_y, selectable_row_index)` hits for mouse clicks on the TELEMETRY tab.
    hits: Vec<(i32, usize)>,
    /// The header's tab names, `(first x, end x, tab)`, as the last frame drew them.
    tab_hits: Vec<(i32, i32, Tab)>,
    body_builds: u64,
    last_viewport_h: usize,
    follow_selection: bool,
}

impl Default for Tab {
    fn default() -> Self {
        Tab::Telemetry
    }
}

impl TelemetryView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn body_builds(&self) -> u64 {
        self.body_builds
    }

    pub fn toggle_tab(&mut self) {
        self.tab = match self.tab {
            Tab::Telemetry => Tab::Sources,
            Tab::Sources => Tab::Telemetry,
        };
        self.scroll = 0;
    }

    pub fn toggle_commands(&mut self) {
        self.show_commands = !self.show_commands;
    }

    pub fn move_selection(&mut self, delta: isize, total_rows: usize) {
        if self.tab == Tab::Sources || self.help {
            self.scroll_motion(Motion::Rows(delta));
            return;
        }
        if total_rows == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, total_rows.saturating_sub(1) as isize)
            as usize;
        self.selected = next;
        self.follow_selection = true;
    }

    pub fn scroll_motion(&mut self, motion: Motion) {
        let page = self.last_viewport_h.saturating_sub(2).max(1) as isize;
        match motion {
            Motion::Rows(d) => {
                self.scroll = (self.scroll as isize + d).max(0) as usize;
            }
            Motion::Page(d) => {
                self.scroll = (self.scroll as isize + d * page).max(0) as usize;
            }
            Motion::Start => self.scroll = 0,
            Motion::End => self.scroll = usize::MAX / 2,
        }
    }

    /// A click: on a tab's name in the header it shows that view; on the TELEMETRY tab's rows
    /// it selects the row. Returns whether anything changed.
    pub fn press(&mut self, x: i32, y: i32) -> bool {
        if self.help {
            return false;
        }
        if y == 0 {
            let Some(&(_, _, tab)) = self.tab_hits.iter().find(|(from, to, _)| (*from..*to).contains(&x)) else {
                return false;
            };
            if tab != self.tab {
                self.toggle_tab();
            }
            return true;
        }
        if self.tab != Tab::Telemetry || y < 1 {
            return false;
        }
        let body_y = (y - 1) + self.scroll as i32;
        if let Some((_, idx)) = self.hits.iter().find(|(hy, _)| *hy == body_y) {
            self.selected = *idx;
            return true;
        }
        false
    }

    /// Number of selectable rows on the TELEMETRY tab: 3 streams + other settings + not checked.
    pub fn selectable_count(report: &Report) -> usize {
        report.streams.len() + report.other.len() + report.not_checked.len()
    }

    /// Paint the interactive Telemetry board into a full-screen `Canvas`.
    pub fn render(
        &mut self,
        state: &State,
        report: Option<&Report>,
        revision: u64,
        checking: bool,
        unavailable_db: bool,
        cols: usize,
        rows: usize,
    ) -> Canvas {
        self.render_with_handoff(
            state,
            report,
            revision,
            checking,
            unavailable_db,
            &HandoffState::Idle,
            cols,
            rows,
        )
    }

    /// Paint the interactive Telemetry board with an optional T3 thread handoff status bar.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_handoff(
        &mut self,
        state: &State,
        report: Option<&Report>,
        revision: u64,
        checking: bool,
        unavailable_db: bool,
        handoff: &HandoffState,
        cols: usize,
        rows: usize,
    ) -> Canvas {
        let p = &state.palette;
        if cols < MIN_COLUMNS || rows < MIN_ROWS {
            return floor_notice(p, cols, rows);
        }
        if unavailable_db {
            return notice(
                p,
                cols,
                rows,
                "Telemetry reads T3's data folder",
                "Opened with --db, so Telemetry reads nothing on this machine.",
                &[
                    "loupe --telemetry-check checks this machine.".into(),
                    "e or Esc returns to Live.".into(),
                ],
            );
        }
        let Some(report) = report else {
            return notice(
                p,
                cols,
                rows,
                "Telemetry",
                "Checking local T3 Code configuration…",
                &["e or Esc returns to Live.".into()],
            );
        };

        let total_rows = Self::selectable_count(report);
        if self.selected >= total_rows && total_rows > 0 {
            self.selected = total_rows - 1;
        }

        let compact_height = rows < 20;
        let key = BodyKey {
            revision,
            cols,
            compact_height,
            tab: self.tab,
            selected: self.selected,
            show_commands: self.show_commands,
            help: self.help,
            palette_tag: palette_tag(p),
        };
        if self.body_key.as_ref() != Some(&key) {
            let (body, hits) = if self.help {
                (build_help_body(cols, report.inputs.source == Source::Demo, p), Vec::new())
            } else {
                match self.tab {
                    Tab::Telemetry => build_telemetry_body(
                        report,
                        cols,
                        compact_height,
                        self.selected,
                        self.show_commands,
                        p,
                    ),
                    Tab::Sources => (build_sources_body(report, cols, p), Vec::new()),
                }
            };
            self.body = Some(body);
            self.hits = hits;
            self.body_key = Some(key);
            self.body_builds += 1;
        }

        let mut c = Canvas::cleared(cols, rows, p.bg, p.fg);
        let ww = cols as i32;

        // Row 0: Shared Live header with TELEMETRY / SOURCES tabs
        let tabs = [
            ("TELEMETRY", self.tab == Tab::Telemetry),
            ("SOURCES", self.tab == Tab::Sources),
        ];
        overview::draw_header_badge(&mut c, state, p, None, Some(HeaderBadge::Tabs(&tabs)));
        self.tab_hits = [(" TELEMETRY ", Tab::Telemetry), (" SOURCES ", Tab::Sources)]
            .into_iter()
            .filter_map(|(label, tab)| terminal::find_in_row(&c, 0, label).map(|(from, to)| (from, to, tab)))
            .collect();

        // Body viewport (rows 1 .. rows - 1, or rows - 2 when handoff bar is visible)
        let show_handoff = !matches!(handoff, HandoffState::Idle);
        let chrome_rows = if show_handoff { 3 } else { 2 };
        let view_h = rows.saturating_sub(chrome_rows);
        self.last_viewport_h = view_h;
        if self.follow_selection && !self.help && self.tab == Tab::Telemetry && view_h > 0 {
            if let Some((hy, _)) = self.hits.iter().find(|(_, idx)| *idx == self.selected) {
                let row_y = (*hy).max(0) as usize;
                if row_y < self.scroll {
                    self.scroll = row_y;
                } else if row_y >= self.scroll + view_h {
                    self.scroll = row_y.saturating_sub(view_h.saturating_sub(1));
                }
            }
            self.follow_selection = false;
        }
        if let Some(body) = &self.body {
            let max_scroll = body.h.saturating_sub(view_h);
            self.scroll = self.scroll.min(max_scroll);
            let copy_h = view_h.min(body.h.saturating_sub(self.scroll));
            for dy in 0..copy_h {
                let src_row = self.scroll + dy;
                let dst_row = 1 + dy;
                let src_off = src_row * body.w;
                let dst_off = dst_row * c.w;
                let n = body.w.min(c.w);
                c.cells[dst_off..dst_off + n].copy_from_slice(&body.cells[src_off..src_off + n]);
            }
            // Scroll indicators (`▲ N above` / `▼ N more`) with clean panel backdrop
            if self.scroll > 0 && view_h > 0 {
                let cue = format!(" ▲ {} above ", self.scroll);
                let cx = (ww - width(&cue)).max(0);
                c.text_bg(cx, 1, &cue, p.text2, p.panel, false);
            }
            let remaining = body.h.saturating_sub(self.scroll + view_h);
            if remaining > 0 && view_h > 0 {
                let cue = format!(" ▼ {} more ", remaining);
                let cx = (ww - width(&cue)).max(0);
                c.text_bg(cx, (1 + view_h - 1) as i32, &cue, p.text2, p.panel, false);
            }
        }

        if show_handoff && rows >= 4 {
            crate::handoff::draw_handoff_bar(&mut c, (rows - 2) as i32, handoff, p);
        }

        // Footer (row h - 1)
        draw_board_footer(
            &mut c,
            state,
            report,
            self.tab,
            self.show_commands,
            self.help,
            checking,
            p,
        );
        c
    }
}

fn floor_notice(p: &Palette, cols: usize, rows: usize) -> Canvas {
    let mut c = Canvas::cleared(cols.max(1), rows.max(1), p.bg, p.fg);
    let ww = cols as i32;
    c.text_bold(1, 0, &fit("Loupe · Telemetry", ww - 2), p.bright);
    if rows >= 3 {
        c.text(1, 2, &fit(&format!("Minimum: {MIN_COLUMNS} × {MIN_ROWS}"), ww - 2), p.dim);
    }
    if rows >= 4 {
        c.text(1, 3, &fit("e back   q quit", ww - 2), p.accent);
    }
    c
}

#[allow(clippy::too_many_arguments)]
fn draw_board_footer(
    c: &mut Canvas,
    state: &State,
    report: &Report,
    tab: Tab,
    show_commands: bool,
    help: bool,
    checking: bool,
    p: &Palette,
) {
    let w = c.w as i32;
    let y = c.h as i32 - 1;
    c.fill(0, y, w, 1, p.panel);

    let can_fix = report.verdict != Verdict::Off;
    let cmd_label = if show_commands { "hide cmds" } else { "commands" };
    let keys: Vec<(&str, &str)> = if help {
        vec![("?", "close help"), ("e", "back"), ("q", "quit")]
    } else if tab == Tab::Sources {
        if can_fix {
            vec![
                ("?", "help"),
                ("e", "back"),
                ("tab", "telemetry"),
                ("f", "fix in T3"),
                ("↑↓", "scroll"),
                ("r", "recheck"),
                ("q", "quit"),
            ]
        } else {
            vec![
                ("?", "help"),
                ("e", "back"),
                ("tab", "telemetry"),
                ("↑↓", "scroll"),
                ("r", "recheck"),
                ("q", "quit"),
            ]
        }
    } else if can_fix {
        vec![
            ("?", "help"),
            ("e", "back"),
            ("tab", "sources"),
            ("⏎", cmd_label),
            ("f", "fix in T3"),
            ("↑↓", "select"),
            ("r", "recheck"),
            ("q", "quit"),
        ]
    } else {
        vec![
            ("?", "help"),
            ("e", "back"),
            ("tab", "sources"),
            ("↑↓", "select"),
            ("r", "recheck"),
            ("q", "quit"),
        ]
    };

    let status = if checking {
        "checking…".into()
    } else if report.inputs.source == Source::Demo {
        "demo data · made up".into()
    } else {
        let age_s = ((state.now - report.inputs.checked_at) / 1000.0).max(0.0);
        format!("read-only · checked {} ago", terminal::duration(age_s))
    };
    let status_w = width(&status);
    let mut x = 1;
    for (k, label) in keys {
        if x + width(k) + width(label) + 3 > w - status_w - 3 {
            break;
        }
        x += c.text_bold(x, y, k, p.accent) + 1;
        x += c.text(x, y, label, p.dim) + 2;
    }
    c.right(w - 1, y, &status, p.dim);
}

fn confidence_glyph(conf: Confidence) -> char {
    match conf {
        Confidence::Verified => '●',
        Confidence::Likely => '◐',
        Confidence::Unverified => '◌',
        Confidence::Read => '·',
    }
}

fn stream_meter(stream: &Stream, p: &Palette) -> (f64, Rgb) {
    match stream.state {
        StreamState::Off => {
            let frac = if stream.confidence == Confidence::Verified { 1.0 } else { 0.85 };
            (frac, p.accent)
        }
        StreamState::Blocked => (0.65, p.text2),
        StreamState::Unverified => {
            let warn = stream.decided_detail.contains("unreadable")
                || stream.decided_detail.contains("invalid")
                || stream.decided_detail.contains("both")
                || stream.decided_detail.contains("desktop skips it");
            (0.35, if warn { p.warn } else { p.dim })
        }
        StreamState::RestartNeeded => (0.6, p.warn),
        StreamState::On => (1.0, p.warn),
        StreamState::OptOutIgnored => (1.0, p.hot),
        StreamState::None => (0.0, p.dim),
    }
}

fn verdict_meter(report: &Report, p: &Palette) -> (f64, Rgb, String) {
    let tracked: Vec<&Stream> = report
        .streams
        .iter()
        .filter(|s| s.counts_toward_verdict)
        .collect();
    let total = tracked.len().max(1);
    let off_verified = tracked
        .iter()
        .filter(|s| s.state == StreamState::Off && s.confidence == Confidence::Verified)
        .count();
    let off_any = tracked.iter().filter(|s| s.state == StreamState::Off).count();
    let blocked = tracked.iter().filter(|s| s.state == StreamState::Blocked).count();
    let unverified = tracked
        .iter()
        .filter(|s| s.state == StreamState::Unverified)
        .count();

    match report.verdict {
        Verdict::Off => (
            1.0,
            p.accent,
            if off_verified == total {
                format!("{off_verified}/{total} verified off")
            } else {
                format!("{off_any}/{total} off")
            },
        ),
        Verdict::Blocked => (
            0.7,
            p.text2,
            format!("{}/{} blocked/off", off_any + blocked, total),
        ),
        Verdict::Partial => (
            (off_any + blocked) as f64 / total as f64,
            p.warn,
            format!("{}/{} off · {} on", off_any + blocked, total, total - off_any - blocked),
        ),
        Verdict::On => (1.0, p.warn, format!("0/{total} off")),
        Verdict::Misconfigured => (1.0, p.hot, "opt-out broken".into()),
        Verdict::RestartNeeded => (0.6, p.warn, "restart T3".into()),
        Verdict::Unverified => {
            if report
                .findings
                .iter()
                .any(|f| (f.id == "shell_env_unverified" || f.id == "shell_otel_unverified") && f.severity == Severity::Warn)
            {
                (0.35, p.warn, "0/2 in desktop env".into())
            } else if report
                .findings
                .iter()
                .any(|f| f.id == "shell_env_unverified" || f.id == "shell_otel_unverified")
            {
                (0.35, p.dim, format!("0/{total} verified"))
            } else if unverified > 0 {
                (0.25, p.warn, format!("{unverified}/{total} unverified"))
            } else {
                (0.25, p.dim, "unverified".into())
            }
        }
    }
}

fn verdict_accent_ink(report: &Report, p: &Palette) -> Rgb {
    match report.verdict {
        Verdict::Off => p.accent,
        Verdict::Misconfigured => p.hot,
        Verdict::On | Verdict::Partial | Verdict::RestartNeeded => p.warn,
        Verdict::Unverified
            if matches!(
                &report.inputs.policy,
                Read::Invalid { .. } | Read::Unreadable(_)
            ) || report.findings.iter().any(|f| {
                f.id == "invalid_telemetry_enabled"
                    || f.id == "macos_dual_policy_conflict"
                    || ((f.id == "shell_env_unverified" || f.id == "shell_otel_unverified")
                        && f.severity == Severity::Warn)
            }) =>
        {
            p.warn
        }
        Verdict::Blocked | Verdict::Unverified => p.text2,
    }
}

fn draw_panel_box(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, bar_ink: Rgb, p: &Palette) {
    if w <= 0 || h <= 0 {
        return;
    }
    c.fill(x, y, w, h, p.panel);
    for row in 0..h {
        c.set(x, y + row, '▌', bar_ink, Some(p.panel), false);
    }
}

/// Top visual summary tile strip (4 raised `p.panel` cards on wide screens, 3 on medium,
/// 1 Verdict card on narrow, or a compact 2-row strip when viewport height < 20).
fn draw_summary_tiles(
    c: &mut Canvas,
    x0: i32,
    y0: i32,
    cw: i32,
    compact_height: bool,
    report: &Report,
    p: &Palette,
) -> i32 {
    let (v_glyph, v_g_ink, v_t_ink) = verdict_style(report.verdict, p);
    let v_bar = verdict_accent_ink(report, p);

    // Very short terminal (< 20 rows): sleek 2-row summary panel so the list fits immediately.
    if compact_height {
        draw_panel_box(c, x0, y0, cw, 2, v_bar, p);
        let mut x = x0 + 2;
        c.set(x, y0, v_glyph, v_g_ink, Some(p.panel), true);
        x += 2;
        x += c.text_bg(x, y0, report.verdict_heading(), v_t_ink, p.panel, true) + 2;
        if let (Some(prod), Some(relay)) = (report.streams.first(), report.streams.get(1)) {
            let (pg, pw, pgi, _, _) = board_stream_badge(prod, p);
            let (rg, rw, rgi, _, _) = board_stream_badge(relay, p);
            let pill_str = format!("events {pg} {pw}  ·  traces {rg} {rw}");
            if x + width(&pill_str) + 2 <= x0 + cw {
                let mut rx = x0 + cw - 1 - width(&pill_str);
                rx += c.text_bg(rx, y0, "events ", p.dim, p.panel, false);
                c.set(rx, y0, pg, pgi, Some(p.panel), true);
                rx += 2;
                rx += c.text_bg(rx, y0, pw, p.fg, p.panel, false);
                rx += c.text_bg(rx, y0, "  ·  traces ", p.dim, p.panel, false);
                c.set(rx, y0, rg, rgi, Some(p.panel), true);
                rx += 2;
                c.text_bg(rx, y0, rw, p.fg, p.panel, false);
            }
        }
        c.text_bg(
            x0 + 2,
            y0 + 1,
            &fit(&report.summary, cw - 3),
            p.text2,
            p.panel,
            false,
        );
        return y0 + 3;
    }

    // Narrow (52..79 cols): single full-width Verdict card with stream indicators.
    if cw < 78 {
        let h = 4;
        draw_panel_box(c, x0, y0, cw, h, v_bar, p);
        c.text_bg(x0 + 2, y0, "VERDICT", p.dim, p.panel, true);
        let tag = if report.inputs.source == Source::Demo {
            "demo"
        } else {
            "read-only"
        };
        c.right_with(x0 + cw - 1, y0, tag, p.dim, Some(p.panel), false);
        c.set(x0 + 2, y0 + 1, v_glyph, v_g_ink, Some(p.panel), true);
        c.text_bg(
            x0 + 4,
            y0 + 1,
            &fit(report.verdict_heading(), cw - 5),
            v_t_ink,
            p.panel,
            true,
        );
        let (frac, bar_ink, label) = verdict_meter(report, p);
        let bar_w = (cw - 6 - width(&label)).clamp(6, 18);
        terminal::hbar(c, x0 + 2, y0 + 2, bar_w, frac, bar_ink, p.track);
        c.text_bg(
            x0 + 2 + bar_w + 2,
            y0 + 2,
            &fit(&label, cw - 4 - bar_w - 2),
            p.text2,
            p.panel,
            false,
        );
        c.text_bg(
            x0 + 2,
            y0 + 3,
            &fit(&report.summary, cw - 3),
            p.text2,
            p.panel,
            false,
        );
        return y0 + h + 1;
    }

    // Wide (>= 98 inner cols): 4 raised tiles (Verdict, Product Events, Relay Traces, T3 Server).
    // Medium (78..97 inner cols): 3 raised tiles (Verdict, Product Events, Relay Traces).
    let four_tiles = cw >= 98;
    let tile_h = 5i32;
    let gap = 1i32;
    let (w0, w1, w2, w3) = if four_tiles {
        let avail = cw - 3 * gap;
        let t0 = ((avail * 31) / 100).max(28);
        let rest = avail - t0;
        let t1 = rest / 3;
        let t2 = rest / 3;
        let t3 = rest - t1 - t2;
        (t0, t1, t2, t3)
    } else {
        let avail = cw - 2 * gap;
        let t0 = ((avail * 38) / 100).max(26);
        let rest = avail - t0;
        let t1 = rest / 2;
        let t2 = rest - t1;
        (t0, t1, t2, 0)
    };

    // Tile 1: VERDICT
    let t0_x = x0;
    draw_panel_box(c, t0_x, y0, w0, tile_h, v_bar, p);
    c.text_bg(t0_x + 2, y0, "VERDICT", p.dim, p.panel, true);
    let scope_tag = if report.inputs.source == Source::Demo {
        "demo"
    } else {
        "local only"
    };
    if w0 >= 24 {
        c.right_with(t0_x + w0 - 1, y0, scope_tag, p.dim, Some(p.panel), false);
    }
    c.set(t0_x + 2, y0 + 1, v_glyph, v_g_ink, Some(p.panel), true);
    c.text_bg(
        t0_x + 4,
        y0 + 1,
        &fit(report.verdict_heading(), w0 - 5),
        v_t_ink,
        p.panel,
        true,
    );
    let (v_frac, v_m_ink, v_m_label) = verdict_meter(report, p);
    let label_w = width(&v_m_label);
    let v_bar_w = (w0 - 5 - label_w).clamp(4, 16);
    if v_bar_w >= 4 && w0 >= label_w + 10 {
        terminal::hbar(c, t0_x + 2, y0 + 2, v_bar_w, v_frac, v_m_ink, p.track);
        c.text_bg(
            t0_x + 2 + v_bar_w + 2,
            y0 + 2,
            &fit(&v_m_label, w0 - 4 - v_bar_w - 1),
            p.text2,
            p.panel,
            false,
        );
    } else {
        c.text_bg(t0_x + 2, y0 + 2, &fit(&v_m_label, w0 - 3), p.text2, p.panel, false);
    }
    let sum_lines = wrap_lines(&report.summary, (w0 - 3).max(12) as usize);
    if let Some(first) = sum_lines.first() {
        c.text_bg(t0_x + 2, y0 + 3, &fit(first, w0 - 3), p.text2, p.panel, false);
    }
    if sum_lines.len() >= 2 {
        let second = if sum_lines.len() == 2 {
            sum_lines[1].clone()
        } else {
            sum_lines[1..].join(" ")
        };
        c.text_bg(t0_x + 2, y0 + 4, &fit(&second, w0 - 3), p.dim, p.panel, false);
    }

    // Tile 2 & Tile 3: PRODUCT EVENTS & RELAY TRACES
    let min_stream_w = w1.min(w2);
    let stream_tiles = [
        (t0_x + w0 + gap, w1, "PRODUCT EVENTS", report.streams.first()),
        (t0_x + w0 + gap + w1 + gap, w2, "RELAY TRACES", report.streams.get(1)),
    ];
    for (tx, tw, title, stream_opt) in stream_tiles {
        let Some(s) = stream_opt else { continue };
        let (g, word, g_ink, w_ink, bold) = board_stream_badge(s, p);
        let (m_frac, m_ink) = stream_meter(s, p);
        let stripe_ink = match s.state {
            StreamState::OptOutIgnored => p.hot,
            StreamState::On | StreamState::RestartNeeded => p.warn,
            StreamState::Off => p.accent,
            _ => g_ink,
        };
        draw_panel_box(c, tx, y0, tw, tile_h, stripe_ink, p);
        c.text_bg(tx + 2, y0, &fit(title, tw - 3), p.dim, p.panel, true);
        if min_stream_w >= 31 {
            let conf_str = format!("{} {}", confidence_glyph(s.confidence), s.confidence.label());
            c.right_with(tx + tw - 1, y0, &conf_str, p.dim, Some(p.panel), false);
        } else if min_stream_w >= 22 {
            let conf_str = format!("{}", confidence_glyph(s.confidence));
            c.right_with(tx + tw - 1, y0, &conf_str, p.dim, Some(p.panel), false);
        }
        c.set(tx + 2, y0 + 1, g, g_ink, Some(p.panel), true);
        let upper_word = word.to_ascii_uppercase();
        c.text_bg(tx + 4, y0 + 1, &fit(&upper_word, tw - 5), w_ink, p.panel, bold || true);
        let bar_w = (tw - 4).clamp(6, 24);
        terminal::hbar(c, tx + 2, y0 + 2, bar_w, m_frac, m_ink, p.track);
        c.text_bg(tx + 2, y0 + 3, &fit(&s.destination, tw - 3), p.text2, p.panel, false);
        c.text_bg(tx + 2, y0 + 4, &fit(&s.decided_detail, tw - 3), p.dim, p.panel, false);
    }

    // Tile 4: T3 SERVER (wide only)
    if four_tiles {
        let tx = t0_x + w0 + gap + w1 + gap + w2 + gap;
        let tw = w3;
        let auth_warn = report.server.auth_files_hashed == AuthFilesHashed::Yes;
        let (srv_glyph, srv_word, srv_ink) = match report.server.state {
            Liveness::Running => ('◉', "RUNNING", p.accent),
            Liveness::Stopped => ('○', "STOPPED", p.dim),
            Liveness::PidReused => ('△', "PID REUSED", p.warn),
            Liveness::Unknown => ('?', "UNKNOWN", p.dim),
        };
        let stripe_ink = if auth_warn || report.verdict == Verdict::RestartNeeded {
            p.warn
        } else if report.server.state == Liveness::Running {
            p.accent
        } else {
            p.rule
        };
        draw_panel_box(c, tx, y0, tw, tile_h, stripe_ink, p);
        c.text_bg(tx + 2, y0, "T3 SERVER", p.dim, p.panel, true);
        let launch_tag = report.server.launch.kind_str();
        if tw >= 18 {
            c.right_with(tx + tw - 1, y0, launch_tag, p.dim, Some(p.panel), false);
        }
        c.set(tx + 2, y0 + 1, srv_glyph, srv_ink, Some(p.panel), true);
        c.text_bg(tx + 4, y0 + 1, &fit(srv_word, tw - 5), p.fg, p.panel, true);

        let short_ver = |v: &str| -> String {
            if let Some(idx) = v.find("-nightly.") {
                format!("{}-nightly", &v[..idx])
            } else {
                v.to_owned()
            }
        };
        let ver_text = match (report.server.version.as_deref(), report.server.auth_files_hashed) {
            (Some(v), AuthFilesHashed::Yes) => (format!("v{} · hashes auth", short_ver(v)), p.warn),
            (Some(v), AuthFilesHashed::No) => (format!("v{} · auth safe", short_ver(v)), p.text2),
            (Some(v), AuthFilesHashed::Unknown) => (format!("v{}", short_ver(v)), p.text2),
            (None, _) => ("version unknown".into(), p.dim),
        };
        c.text_bg(
            tx + 2,
            y0 + 2,
            &fit(&ver_text.0, tw - 3),
            ver_text.1,
            p.panel,
            auth_warn,
        );

        let (pol_text, pol_ink): (&str, Rgb) = match (&report.inputs.policy, &report.server.policy_loaded) {
            (Read::Invalid { .. }, _) => ("policy.json · invalid", p.warn),
            (Read::Unreadable(_), _) => ("policy.json · locked", p.warn),
            (Read::Ok { value, .. }, Some(_)) if value.telemetry == Some(false) => {
                ("policy.json · loaded", p.accent)
            }
            (Read::Ok { value, .. }, None) if value.telemetry == Some(false) => {
                ("policy.json · restart T3", p.warn)
            }
            (Read::Ok { .. }, _) => ("policy.json · telemetry unset", p.dim),
            (Read::Absent, _) => ("policy.json · not set", p.dim),
        };
        c.text_bg(tx + 2, y0 + 3, &fit(pol_text, tw - 3), pol_ink, p.panel, false);
        c.text_bg(
            tx + 2,
            y0 + 4,
            &fit(&sources_badge(report), tw - 3),
            p.dim,
            p.panel,
            false,
        );
    }

    y0 + tile_h + 1
}

fn build_telemetry_body(
    report: &Report,
    cols: usize,
    compact_height: bool,
    selected: usize,
    show_commands: bool,
    p: &Palette,
) -> (Canvas, Vec<(i32, usize)>) {
    let mut c = Canvas::cleared(cols, 180, p.bg, p.fg);
    let ww = cols as i32;
    let mut hits = Vec::new();

    // Cap content width at 184 cols on ultrawide monitors so tables & pipelines stay scannable.
    let x0 = 1i32;
    let cw = (ww - 2).min(184).max(MIN_COLUMNS as i32 - 2);
    let top_y = draw_summary_tiles(&mut c, x0, 0, cw, compact_height, report, p);

    if cw >= 114 {
        let left_w = ((cw * 46) / 100).clamp(54, 86);
        let div_x = x0 + left_w + 1;
        let right_x = div_x + 2;
        let right_w = (x0 + cw - right_x).max(40);

        // ── Left Column: Grouped Audit Lists + Findings ───────────────
        let mut ly = top_y;
        ly = draw_grouped_rows(&mut c, x0, ly, left_w, report, selected, p, true, &mut hits);
        ly = draw_findings_block(&mut c, x0, ly, left_w, report, p);

        // ── Right Column: Visual Decision Pipeline + Fix Cards + Sources
        let mut ry = top_y;
        ry = draw_selected_detail(&mut c, right_x, ry, right_w, report, selected, p);
        ry += 1;
        ry = draw_action_panel(&mut c, right_x, ry, right_w, report, show_commands, p);
        ry = draw_compact_sources_panel(&mut c, right_x, ry, right_w, report, p);

        let bottom_y = ly.max(ry);
        for dy in top_y..bottom_y.saturating_sub(1) {
            c.set(div_x, dy, '│', p.faint, None, false);
        }

        let h = bottom_y.max(1) as usize;
        return (crop_canvas(&c, cols, h, p), hits);
    }

    // Stacked layout (52..115 columns)
    let wide_cols = cw >= 76;
    let mut y = top_y;
    y = draw_grouped_rows(&mut c, x0, y, cw, report, selected, p, wide_cols, &mut hits);
    y = draw_selected_detail(&mut c, x0, y, cw, report, selected, p);
    y += 1;
    y = draw_action_panel(&mut c, x0, y, cw, report, show_commands, p);
    y = draw_findings_block(&mut c, x0, y, cw, report, p);

    (crop_canvas(&c, cols, y.max(1) as usize, p), hits)
}

#[allow(clippy::too_many_arguments)]
fn draw_grouped_rows(
    c: &mut Canvas,
    x: i32,
    mut y: i32,
    w: i32,
    report: &Report,
    selected: usize,
    p: &Palette,
    wide_cols: bool,
    hits: &mut Vec<(i32, usize)>,
) -> i32 {
    let mut row_idx = 0usize;

    // 1. Streams
    terminal::section_with(
        c,
        x,
        y,
        w,
        "TELEMETRY STREAMS",
        p,
        if wide_cols { "↑↓ inspect · how sure" } else { "↑↓ inspect" },
        p.dim,
    );
    y += 1;
    for s in &report.streams {
        let is_sel = row_idx == selected;
        hits.push((y, row_idx));
        let bg = is_sel.then_some(p.sel);
        if is_sel {
            c.fill(x, y, w, 1, p.sel);
            c.set(x, y, '▌', p.bright, bg, false);
        }
        let (glyph, word, g_ink, w_ink, bold) = board_stream_badge(s, p);
        c.set(x + 2, y, glyph, row_ink(p, g_ink, is_sel), bg, bold);
        let badge_w = 10i32;
        c.text_with(
            x + 4,
            y,
            &fit(word, badge_w),
            row_ink(p, w_ink, is_sel),
            bg,
            bold,
            usize::MAX,
        );
        let name_x = x + 4 + badge_w + 2;
        let name_w = 20.min((w - (name_x - x) - 2).max(12));
        c.text_with(
            name_x,
            y,
            &fit(s.id.label(), name_w),
            row_ink(p, p.fg, is_sel),
            bg,
            is_sel,
            usize::MAX,
        );
        if wide_cols {
            let conf = format!("{} {}", confidence_glyph(s.confidence), s.confidence.label());
            let conf_col_w = width(&conf).max(12) + 2;
            let detail_x = name_x + name_w + 2;
            let detail_w = (x + w - conf_col_w - detail_x).max(0);
            if detail_w >= 14 {
                let d_ink = if s.state == StreamState::OptOutIgnored {
                    row_ink(p, p.hot, is_sel)
                } else {
                    row_ink(p, p.text2, is_sel)
                };
                c.text_with(
                    detail_x,
                    y,
                    &fit(&s.decided_detail, detail_w),
                    d_ink,
                    bg,
                    false,
                    usize::MAX,
                );
            }
            c.text_with(
                x + w - width(&conf),
                y,
                &conf,
                row_ink(p, p.dim, is_sel),
                bg,
                false,
                usize::MAX,
            );
        }
        y += 1;
        if !wide_cols && is_sel {
            c.text(x + 4, y, &fit(&s.decided_detail, w - 5), p.dim);
            y += 1;
        }
        row_idx += 1;
    }
    y += 1;

    // 2. Other local settings (separated cleanly from what T3 sends)
    if !report.other.is_empty() {
        terminal::section_with(
            c,
            x,
            y,
            w,
            "OTHER SETTINGS",
            p,
            if wide_cols { "settings.json · informational" } else { "informational" },
            p.dim,
        );
        y += 1;
        for o in &report.other {
            let is_sel = row_idx == selected;
            hits.push((y, row_idx));
            let bg = is_sel.then_some(p.sel);
            if is_sel {
                c.fill(x, y, w, 1, p.sel);
                c.set(x, y, '▌', p.bright, bg, false);
            }
            c.set(x + 2, y, '·', row_ink(p, p.text2, is_sel), bg, false);
            let (state_text, note_text): (String, String) = if o.id == "install_id" {
                ("present".into(), format!("{} · stat only", o.value))
            } else {
                (
                    o.value.clone(),
                    o.note.as_deref().unwrap_or("settings.json").to_owned(),
                )
            };
            let badge_w = 10i32;
            c.text_with(
                x + 4,
                y,
                &fit(&state_text, badge_w),
                row_ink(p, p.text2, is_sel),
                bg,
                false,
                usize::MAX,
            );
            let name_x = x + 4 + badge_w + 2;
            let name_w = 20.min((w - (name_x - x) - 2).max(12));
            c.text_with(
                name_x,
                y,
                &fit(o.label, name_w),
                row_ink(p, p.fg, is_sel),
                bg,
                is_sel,
                usize::MAX,
            );
            if wide_cols {
                let conf = "· read";
                let detail_x = name_x + name_w + 2;
                let detail_w = (x + w - (width(conf) + 2) - detail_x).max(0);
                if detail_w >= 14 {
                    c.text_with(
                        detail_x,
                        y,
                        &fit(&note_text, detail_w),
                        row_ink(p, p.dim, is_sel),
                        bg,
                        false,
                        usize::MAX,
                    );
                }
                c.text_with(
                    x + w - width(conf),
                    y,
                    conf,
                    row_ink(p, p.dim, is_sel),
                    bg,
                    false,
                    usize::MAX,
                );
            }
            y += 1;
            row_idx += 1;
        }
        y += 1;
    }

    // 3. Not read by Loupe (explicit privacy boundary)
    if !report.not_checked.is_empty() {
        terminal::section_with(
            c,
            x,
            y,
            w,
            "NOT READ BY LOUPE",
            p,
            "privacy boundary",
            p.dim,
        );
        y += 1;
        for nc in &report.not_checked {
            let is_sel = row_idx == selected;
            hits.push((y, row_idx));
            let bg = is_sel.then_some(p.sel);
            if is_sel {
                c.fill(x, y, w, 1, p.sel);
                c.set(x, y, '▌', p.bright, bg, false);
            }
            c.set(x + 2, y, '⊘', row_ink(p, p.dim, is_sel), bg, false);
            let badge_w = 10i32;
            c.text_with(
                x + 4,
                y,
                "private",
                row_ink(p, p.dim, is_sel),
                bg,
                false,
                usize::MAX,
            );
            let name_x = x + 4 + badge_w + 2;
            let name_w = 20.min((w - (name_x - x) - 2).max(12));
            c.text_with(
                name_x,
                y,
                &fit(nc.label, name_w),
                row_ink(p, p.fg, is_sel),
                bg,
                is_sel,
                usize::MAX,
            );
            if wide_cols {
                let detail_x = name_x + name_w + 2;
                let detail_w = (x + w - detail_x).max(0);
                if detail_w >= 8 {
                    c.text_with(
                        detail_x,
                        y,
                        &fit(&nc.detail, detail_w),
                        row_ink(p, p.dim, is_sel),
                        bg,
                        false,
                        usize::MAX,
                    );
                }
            }
            y += 1;
            row_idx += 1;
        }
        y += 1;
    }

    y
}

fn draw_findings_block(
    c: &mut Canvas,
    x: i32,
    mut y: i32,
    w: i32,
    report: &Report,
    p: &Palette,
) -> i32 {
    if report.findings.is_empty() {
        if report.verdict == Verdict::Off {
            terminal::section_with(c, x, y, w, "FINDINGS", p, "all clear", p.accent);
            y += 1;
            draw_panel_box(c, x, y, w, 1, p.accent, p);
            c.text_bg(
                x + 2,
                y,
                &fit("✓ No issues found — both T3 telemetry streams are off.", w - 3),
                p.fg,
                p.panel,
                false,
            );
            y += 2;
        }
        return y;
    }

    let to_fix = report.findings.iter().filter(|f| f.severity.is_to_fix()).count();
    let right = if to_fix > 0 {
        format!("{to_fix} to fix")
    } else if report.findings.len() == 1 {
        "1 note".into()
    } else {
        format!("{} notes", report.findings.len())
    };
    let right_ink = if report
        .findings
        .iter()
        .any(|f| f.severity == Severity::OptOutIgnored)
    {
        p.hot
    } else if to_fix > 0 {
        p.warn
    } else {
        p.dim
    };
    terminal::section_with(c, x, y, w, "FINDINGS", p, &right, right_ink);
    y += 1;

    for f in &report.findings {
        let (bar_ink, tag, elevated) = match f.severity {
            Severity::OptOutIgnored => (p.hot, Some("OPT-OUT IGNORED"), true),
            Severity::Restart => (p.warn, Some("RESTART NEEDED"), true),
            Severity::Warn => (p.warn, Some("WARNING"), true),
            Severity::Caveat => (p.rule, None, false),
            Severity::Info => (p.faint, None, false),
        };
        let body_lines = wrap_lines(&f.message, (w - 3).max(16) as usize);
        let fix_lines = f
            .fix
            .as_deref()
            .map(|fx| wrap_lines(&format!("→ {fx}"), (w - 3).max(16) as usize))
            .unwrap_or_default();
        let card_h =
            (i32::from(tag.is_some()) + body_lines.len() as i32 + fix_lines.len() as i32).max(1);

        if elevated {
            draw_panel_box(c, x, y, w, card_h, bar_ink, p);
            let mut cy = y;
            if let Some(t) = tag {
                c.text_bg(x + 2, cy, t, bar_ink, p.panel, true);
                cy += 1;
            }
            for line in &body_lines {
                c.text_bg(x + 2, cy, &fit(line, w - 3), p.fg, p.panel, false);
                cy += 1;
            }
            for line in &fix_lines {
                c.text_bg(x + 2, cy, &fit(line, w - 3), p.accent, p.panel, false);
                cy += 1;
            }
            y = cy + 1;
        } else {
            for (idx, line) in body_lines.iter().enumerate() {
                c.set(x, y + idx as i32, '▌', bar_ink, None, false);
                c.text(x + 2, y + idx as i32, &fit(line, w - 3), p.text2);
            }
            y += body_lines.len() as i32 + 1;
        }
    }
    y
}

fn draw_selected_detail(
    c: &mut Canvas,
    x: i32,
    mut y: i32,
    w: i32,
    report: &Report,
    selected: usize,
    p: &Palette,
) -> i32 {
    if let Some(s) = report.streams.get(selected) {
        let heading = format!("HOW T3 DECIDES · {}", s.id.short_label().to_ascii_uppercase());
        terminal::section_with(c, x, y, w, &fit(&heading, w - 16), p, &fit(&s.destination, w / 2), p.dim);
        y += 1;
        y = draw_visual_pipeline(c, x, y, w, s, p);
        if let Some(caveat) = &s.caveat {
            let lines = wrap_lines(caveat, (w - 4).max(16) as usize);
            for (idx, line) in lines.iter().enumerate() {
                if idx == 0 {
                    c.text(x + 1, y, "↳", p.dim);
                }
                c.text(x + 3, y, &fit(line, w - 4), p.dim);
                y += 1;
            }
        }
        return y;
    }

    let other_idx = selected.saturating_sub(report.streams.len());
    if let Some(o) = report.other.get(other_idx) {
        let title = o.label.to_ascii_uppercase();
        terminal::section_with(c, x, y, w, &title, p, "informational", p.dim);
        y += 1;
        let desc = match o.id {
            "agent_feedback" => "Controls whether agents can send T3 Code bug reports (Settings › Labs). Never sends prompts unless you click Send on a card.",
            "provider_update_checks" => "Controls whether T3 Code checks for provider CLI updates on startup (userdata/settings.json).",
            "install_id" => "T3's local random installation ID file (userdata/anonymous-id). Loupe checks only its modification date and never opens it.",
            "share_usage_data" => "Reserved slot for T3 Code's upcoming Settings toggle; not read by T3 yet.",
            _ => "Informational setting read from userdata/settings.json; does not affect the telemetry verdict.",
        };
        let lines = wrap_lines(desc, (w - 3).max(16) as usize);
        let h = 1 + lines.len() as i32;
        draw_panel_box(c, x, y, w, h, p.accent, p);
        c.text_bg(
            x + 2,
            y,
            &fit(&format!("Current value: {}", o.value), w - 3),
            p.fg,
            p.panel,
            true,
        );
        for (i, line) in lines.iter().enumerate() {
            c.text_bg(x + 2, y + 1 + i as i32, &fit(line, w - 3), p.text2, p.panel, false);
        }
        return y + h;
    }

    let nc_idx = selected.saturating_sub(report.streams.len() + report.other.len());
    if let Some(nc) = report.not_checked.get(nc_idx) {
        let title = nc.label.to_ascii_uppercase();
        terminal::section_with(c, x, y, w, &title, p, "not read by Loupe", p.dim);
        y += 1;
        let policy_path = match report.inputs.platform {
            crate::telemetry::Platform::MacOs => "/Library/Application Support/T3Code/policy.json",
            _ => "/etc/t3code/policy.json",
        };
        let app_env_desc = format!(
            "Loupe never reads /proc/<pid>/environ because provider API keys live there. Write managed policy ({policy_path}) below to lock telemetry off across every launch path and let Loupe verify it."
        );
        let extra = match nc.id {
            "activity_sharing" => "Publish agent activity is stored inside T3's secret store (userdata/secrets/), which Loupe deliberately never opens. Check Settings › Sharing & access or run `t3 connect status`.",
            "app_environment" => app_env_desc.as_str(),
            _ => nc.detail.as_str(),
        };
        let lines = wrap_lines(extra, (w - 3).max(16) as usize);
        let h = 1 + lines.len() as i32;
        draw_panel_box(c, x, y, w, h, p.rule, p);
        c.text_bg(
            x + 2,
            y,
            &fit(&format!("Privacy boundary: {}", nc.detail), w - 3),
            p.fg,
            p.panel,
            true,
        );
        for (i, line) in lines.iter().enumerate() {
            c.text_bg(x + 2, y + 1 + i as i32, &fit(line, w - 3), p.text2, p.panel, false);
        }
        return y + h;
    }
    y
}

/// Visual connected vertical decision pipeline with fixed readable column spacing
/// (never drifts 80 spaces apart on wide screens).
fn draw_visual_pipeline(
    c: &mut Canvas,
    x: i32,
    mut y: i32,
    w: i32,
    s: &Stream,
    p: &Palette,
) -> i32 {
    if s.ladder.is_empty() {
        draw_panel_box(c, x, y, w, 1, p.rule, p);
        c.text_bg(x + 2, y, &fit("○ No custom OTLP exporters configured.", w - 3), p.dim, p.panel, false);
        return y + 1;
    }

    let step_w = 30.min((w - 26).max(14));
    let val_x = x + 7 + step_w + 2;

    for (idx, step) in s.ladder.iter().enumerate() {
        let num = format!("{}.", idx + 1);
        if step.decisive {
            let effective_state = step.state_hint.unwrap_or(s.state);
            let (g, word, g_ink, w_ink, bold) = if effective_state == s.state {
                board_stream_badge(s, p)
            } else {
                stream_style(effective_state, p)
            };
            let stripe = match effective_state {
                StreamState::OptOutIgnored => p.hot,
                StreamState::On | StreamState::RestartNeeded => p.warn,
                StreamState::Off => p.accent,
                _ => g_ink,
            };
            let desktop_skip_expl = s.state == StreamState::Unverified
                && s.decided_detail.contains("desktop skips it");
            let shell_expl = desktop_skip_expl
                || (s.state == StreamState::Unverified
                    && (s.decided_detail.contains("shell")
                        || s.decided_detail.contains("not confirmed in T3")
                        || s.decided_detail.contains("app env private")));
            let card_h = if shell_expl { 2 } else { 1 };
            draw_panel_box(c, x, y, w, card_h, stripe, p);
            c.set(x + 2, y, '◆', stripe, Some(p.panel), true);
            c.text_bg(x + 4, y, &num, p.dim, p.panel, false);
            c.text_bg(x + 7, y, &fit(&step.step, step_w), p.fg, p.panel, true);

            let badge = format!("{g} {word}");
            let badge_w = width(&badge);
            let val_room = (x + w - 1 - badge_w - 2 - val_x).max(0);
            if val_room >= 6 {
                c.text_bg(val_x, y, &fit(&step.value, val_room), p.fg, p.panel, false);
            }
            c.right_with(x + w - 1, y, &badge, w_ink, Some(p.panel), bold);

            if shell_expl {
                let expl_text = if desktop_skip_expl {
                    "└─ Desktop T3 skips interactive shell rc files; add policy.json (or desktop env) below."
                } else {
                    "└─ Only seen in this shell; T3's live env is private — add policy.json below to lock & verify."
                };
                c.text_bg(
                    x + 7,
                    y + 1,
                    &fit(expl_text, w - 8),
                    p.text2,
                    p.panel,
                    false,
                );
            }
            y += card_h;
        } else {
            c.set(x + 2, y, '○', p.dim, None, false);
            c.text(x + 4, y, &num, p.dim);
            c.text(x + 7, y, &fit(&step.step, step_w), p.text2);
            let val_room = (x + w - 4 - val_x).max(0);
            if val_room >= 6 {
                c.text(val_x, y, &fit(&step.value, val_room), p.dim);
            }
            c.right(x + w - 1, y, "·", p.faint);
            y += 1;
        }
    }

    if !s.ladder.iter().any(|l| l.decisive) && s.counts_toward_verdict {
        let (g, word, g_ink, w_ink, bold) = board_stream_badge(s, p);
        draw_panel_box(c, x, y, w, 1, g_ink, p);
        c.set(x + 2, y, g, g_ink, Some(p.panel), true);
        c.text_bg(x + 7, y, &fit("Default (no opt-out)", step_w), p.fg, p.panel, true);
        let badge = format!("{g} {word}");
        let val_room = (x + w - 1 - width(&badge) - 2 - val_x).max(0);
        if val_room >= 6 {
            c.text_bg(val_x, y, &fit(&s.decided_detail, val_room), p.text2, p.panel, false);
        }
        c.right_with(x + w - 1, y, &badge, w_ink, Some(p.panel), bold);
        y += 1;
    }
    y
}

fn snippet_label_and_badge(snip: &Snippet) -> (&'static str, &'static str) {
    match snip.for_kind {
        "managed_policy" => ("Managed policy (policy.json)", "RECOMMENDED · VERIFIED · SUDO"),
        "desktop_omarchy" => ("Omarchy Hyprland env", "NO SUDO · LIKELY OFF"),
        "desktop_uwsm" => ("uwsm session env", "NO SUDO · LIKELY OFF"),
        "desktop_linux" => ("Systemd environment.d", "NO SUDO · LIKELY OFF"),
        "service" => ("Service environment file", "NO SUDO · VERIFIED"),
        "shell_macos" => ("Login shell profile (~/.zshrc)", "NO SUDO · LIKELY OFF"),
        "shell" => ("Shell profile", "NO SUDO"),
        "policy_fix" => ("Validate managed policy", "REQUIRED TO START"),
        "restart" => ("Restart T3 Code", "APPLY SAVED CONFIG"),
        _ => ("Recommended step", "LOCAL"),
    }
}

fn snippet_summary(snip: &Snippet, platform: crate::telemetry::Platform) -> &'static str {
    match snip.for_kind {
        "managed_policy" => match platform {
            crate::telemetry::Platform::MacOs => {
                "Recommended: create /Library/Application Support/T3Code/policy.json and restart T3. Locks telemetry off across desktop, CLI, and services, and T3 confirms it on startup."
            }
            _ => {
                "Recommended: create /etc/t3code/policy.json and restart T3. Locks telemetry off across desktop, CLI, and services, and T3 confirms it on startup."
            }
        },
        "desktop_omarchy" => {
            "Without sudo: add T3CODE_TELEMETRY_ENABLED=false to ~/.config/hypr/hyprland.lua, reload Hyprland, and restart T3 (tracked from disk, likely off)."
        }
        "desktop_uwsm" => {
            "Without sudo: add T3CODE_TELEMETRY_ENABLED=false to ~/.config/uwsm/env, then log out and back in (tracked from disk, likely off)."
        }
        "desktop_linux" => {
            "Without sudo: set T3CODE_TELEMETRY_ENABLED=false in ~/.config/environment.d/t3code.conf, then log out and back in (tracked from disk, likely off)."
        }
        "service" => {
            "Add T3CODE_TELEMETRY_ENABLED=false to service.env and restart the T3 service."
        }
        "shell_macos" => {
            "Without sudo: export T3CODE_TELEMETRY_ENABLED=false in ~/.zshrc and restart T3. Tracked from disk (likely off) rather than T3's live env."
        }
        "shell" => {
            "Without sudo: export T3CODE_TELEMETRY_ENABLED=false in your shell profile, then restart T3 Code."
        }
        "policy_fix" => "Run `t3 policy check` to fix the invalid policy.json before restarting T3 Code.",
        "restart" => "Quit and reopen T3 Code so the running server picks up your saved opt-out.",
        _ => "Apply this local configuration change, then restart T3 Code.",
    }
}

fn draw_action_panel(
    c: &mut Canvas,
    x: i32,
    mut y: i32,
    w: i32,
    report: &Report,
    show_commands: bool,
    p: &Palette,
) -> i32 {
    if report.verdict == Verdict::Off {
        terminal::section_with(c, x, y, w, "STATUS", p, "read-only · off", p.dim);
        y += 1;
        let msg = if report.server.policy_loaded.is_some() {
            "Telemetry is locked off via managed policy and loaded by T3 Code. Nothing to fix."
        } else if let Some(de) = &report.inputs.desktop_env {
            return {
                let text = format!(
                    "Telemetry is turned off in {} and T3 Code started after the last edit. Nothing to fix.",
                    de.source_label
                );
                let lines = wrap_lines(&text, (w - 3).max(16) as usize);
                draw_panel_box(c, x, y, w, lines.len() as i32, p.accent, p);
                for (i, line) in lines.iter().enumerate() {
                    c.text_bg(x + 2, y + i as i32, &fit(line, w - 3), p.text2, p.panel, false);
                }
                y + lines.len() as i32 + 1
            };
        } else {
            "Product usage events and relay traces are both off. Nothing to fix."
        };
        let lines = wrap_lines(msg, (w - 3).max(16) as usize);
        draw_panel_box(c, x, y, w, lines.len() as i32, p.accent, p);
        for (i, line) in lines.iter().enumerate() {
            c.text_bg(x + 2, y + i as i32, &fit(line, w - 3), p.text2, p.panel, false);
        }
        return y + lines.len() as i32 + 1;
    }

    if report.fix.is_empty() {
        return y;
    }

    let heading = match report.verdict {
        Verdict::Unverified if matches!(&report.inputs.policy, Read::Invalid { .. }) => "HOW TO FIX POLICY",
        Verdict::Unverified
            if report
                .inputs
                .shell_rc_source
                .as_ref()
                .is_some_and(|rc| rc.interactive_only) =>
        {
            "HOW TO APPLY TO DESKTOP T3"
        }
        Verdict::Unverified => "HOW TO LOCK & VERIFY OFF",
        Verdict::RestartNeeded => "HOW TO APPLY IT",
        Verdict::Misconfigured => "HOW TO FIX IT",
        _ => "HOW TO TURN IT OFF",
    };
    let right_tag = if show_commands {
        if w >= 52 {
            "⏎ hide commands · f fix in T3"
        } else {
            "⏎ hide · f fix"
        }
    } else if w >= 52 {
        "⏎ show commands · f fix in T3"
    } else {
        "⏎ commands · f fix"
    };
    terminal::section_with(c, x, y, w, heading, p, right_tag, p.dim);
    y += 1;

    for (idx, snip) in report.fix.iter().take(2).enumerate() {
        let (title_text, badge_text) = snippet_label_and_badge(snip);
        let numbered_title = format!("{}. {title_text}", idx + 1);
        let desc_text = snippet_summary(snip, report.inputs.platform);
        let desc_lines = wrap_lines(desc_text, (w - 4).max(16) as usize);
        let (cmd_lines, clipped) = if show_commands {
            format_command_lines(&snip.command, (w - 4).max(16) as usize)
        } else {
            (Vec::new(), false)
        };
        let card_h = 1
            + desc_lines.len() as i32
            + if show_commands {
                1 + cmd_lines.len() as i32 + i32::from(clipped)
            } else {
                0
            };

        let stripe = if idx == 0 {
            match report.verdict {
                Verdict::Misconfigured => p.hot,
                Verdict::RestartNeeded => p.warn,
                _ => p.accent,
            }
        } else {
            p.rule
        };
        draw_panel_box(c, x, y, w, card_h, stripe, p);

        // Card header: numbered option title + concise right-aligned tag
        let badge_ink = if idx == 0 { stripe } else { p.dim };
        let title_room = (w - 4 - width(badge_text) - 2).max(12);
        c.text_bg(x + 2, y, &fit(&numbered_title, title_room), p.bright, p.panel, true);
        if w >= width(&numbered_title).min(title_room) + width(badge_text) + 5 {
            c.right_with(x + w - 1, y, badge_text, badge_ink, Some(p.panel), idx == 0);
        }
        let mut cy = y + 1;

        for line in &desc_lines {
            c.text_bg(
                x + 2,
                cy,
                &fit(line, w - 3),
                if idx == 0 { p.text2 } else { p.dim },
                p.panel,
                false,
            );
            cy += 1;
        }
        if show_commands {
            cy += 1; // breathing row before command block
            for line in &cmd_lines {
                c.text_bg(x + 2, cy, &fit(line, w - 3), p.bright, p.panel, false);
                cy += 1;
            }
            if clipped {
                c.text_bg(
                    x + 2,
                    cy,
                    &fit("full command: loupe --telemetry-check --json", w - 3),
                    p.dim,
                    p.panel,
                    false,
                );
                cy += 1;
            }
        }
        y = cy + 1;
    }
    y
}

fn draw_compact_sources_panel(
    c: &mut Canvas,
    x: i32,
    mut y: i32,
    w: i32,
    report: &Report,
    p: &Palette,
) -> i32 {
    let right = if w >= 52 {
        format!("{} · tab for details", sources_badge(report))
    } else {
        sources_badge(report)
    };
    terminal::section_with(c, x, y, w, "LOCAL SOURCES", p, &right, p.dim);
    y += 1;
    for rec in &report.inputs.reads {
        draw_source_compact_row(c, x, y, w, rec, p);
        y += 1;
    }
    y + 1
}

fn build_sources_body(report: &Report, cols: usize, p: &Palette) -> Canvas {
    let mut c = Canvas::cleared(cols, 96, p.bg, p.fg);
    let ww = cols as i32;
    let cw = (ww - 2).min(184).max(MIN_COLUMNS as i32 - 2);
    let mut y = 0i32;

    // Server Metadata & Privacy summary panel
    let ver = report.server.version.as_deref().unwrap_or("unknown");
    let st = match report.server.state {
        Liveness::Running => "running",
        Liveness::Stopped => "stopped",
        Liveness::PidReused => "pid reused",
        Liveness::Unknown => "unknown",
    };
    let launch = report.server.launch.kind_str();
    let auth_note = match report.server.auth_files_hashed {
        AuthFilesHashed::Yes => "hashes ~/.codex/auth.json & ~/.claude.json (< 2026-11-24.5209)",
        AuthFilesHashed::No => "does not hash provider auth files (#42299)",
        AuthFilesHashed::Unknown => "auth-file hashing status unknown for this version",
    };
    let has_pol = report.server.policy_loaded.is_some();
    let card_h = 3 + i32::from(has_pol);
    let stripe = if report.server.auth_files_hashed == AuthFilesHashed::Yes {
        p.warn
    } else {
        p.accent
    };
    draw_panel_box(&mut c, 1, y, cw, card_h, stripe, p);
    c.text_bg(3, y, "T3 SERVER & AUDIT SUMMARY", p.dim, p.panel, true);
    c.right_with(1 + cw - 1, y, "read-only · nothing sent", p.dim, Some(p.panel), false);
    c.text_bg(
        3,
        y + 1,
        &fit(&format!("state: {st}  ·  launch: {launch}  ·  version: {ver}"), cw - 3),
        p.fg,
        p.panel,
        true,
    );
    c.text_bg(
        3,
        y + 2,
        &fit(auth_note, cw - 3),
        if report.server.auth_files_hashed == AuthFilesHashed::Yes {
            p.warn
        } else {
            p.text2
        },
        p.panel,
        false,
    );
    if let Some(pol) = &report.server.policy_loaded {
        c.text_bg(
            3,
            y + 3,
            &fit(
                &format!(
                    "loaded policy: {} ({}) · fingerprint {}",
                    pol.organization, pol.source, pol.fingerprint
                ),
                cw - 3,
            ),
            p.accent,
            p.panel,
            false,
        );
    }
    y += card_h + 1;

    let right = format!("{} · nothing written", sources_badge(report));
    terminal::section_with(&mut c, 1, y, cw, "LOCAL SOURCES INSPECTED", p, &right, p.dim);
    y += 1;

    let mtime_w = 8;
    let path_w = ((cw - 18 - mtime_w) / 2).max(16);
    let detail_x = 16 + path_w + 2;
    let detail_w = (1 + cw - mtime_w - detail_x).max(0);
    for rec in &report.inputs.reads {
        let (glyph, state_word, g_ink) = match rec.outcome {
            ReadOutcome::Read => ('✓', "read", p.accent),
            ReadOutcome::StatOnly => ('✓', "stat only", p.text2),
            ReadOutcome::Absent => ('·', "absent", p.dim),
            ReadOutcome::Unreadable => ('△', "unreadable", p.warn),
            ReadOutcome::Invalid => ('△', "invalid", p.warn),
            ReadOutcome::Never => ('✗', "never opened", p.dim),
        };
        c.set(1, y, glyph, g_ink, None, false);
        c.text(3, y, &fit(state_word, 12), g_ink);
        let mtime_str = rec
            .modified_ms
            .map(format_short_date)
            .unwrap_or_default();
        c.text(16, y, &fit(&rec.path, path_w), p.fg);
        if detail_w > 0 {
            c.text(detail_x, y, &fit(&rec.detail, detail_w), p.dim);
        }
        if !mtime_str.is_empty() {
            c.right(1 + cw, y, &mtime_str, p.dim);
        }
        y += 1;
    }

    crop_canvas(&c, cols, (y + 1).max(1) as usize, p)
}

fn build_help_body(cols: usize, is_demo: bool, p: &Palette) -> Canvas {
    let mut c = Canvas::cleared(cols, 24, p.bg, p.fg);
    let ww = cols as i32;
    let mut y = 0i32;
    terminal::section_with(&mut c, 1, y, ww - 2, "TELEMETRY HELP", p, "", p.dim);
    y += 1;
    let lines = crate::keymap::lines(crate::keymap::Screen::Telemetry, false);
    let end = c.h as i32;
    y = crate::keymap::draw(&mut c, y, end, &lines, p, p.fg);
    y += 1;
    let note = "Reads local files only: T3's settings and runtime note, the managed policy and the hosts file. Never opens T3's secrets, agent logins, /proc/*/environ, or the install ID itself. Sends nothing and changes nothing.";
    for line in wrap_lines(note, (ww - 4).max(20) as usize) {
        c.text(2, y, &line, p.text2);
        y += 1;
    }
    if is_demo {
        y += 1;
        c.text(2, y, "Demo: a made-up machine, not yours.", p.dim);
        y += 1;
    }
    crop_canvas(&c, cols, (y + 1) as usize, p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Snapshot, State};
    use crate::telemetry::demo;
    use crate::theme;

    #[test]
    fn card_wide_and_list_have_exact_widths_and_nonempty_text() {
        let rep = demo::report(1_795_532_700_000.0);
        let p = Palette::fallback();

        let c80 = card(&rep, &p, false);
        assert_eq!(c80.w, CARD_COLUMNS);
        assert!(c80.h >= 18);
        let t80 = c80.to_text();
        assert!(t80.contains("OPT-OUT NOT WORKING"));
        assert!(t80.contains("product usage events"));
        assert!(t80.contains("relay traces"));
        assert!(t80.contains("nothing sent"));

        let c120 = wide(&rep, &p, false);
        assert_eq!(c120.w, WIDE_COLUMNS);
        let t120 = c120.to_text();
        assert!(t120.contains("HOW T3 DECIDES:"));
        assert!(t120.contains("SOURCES"));

        let c39 = list(&rep, &p, false);
        assert_eq!(c39.w, LIST_COLUMNS);
        let t39 = c39.to_text();
        assert!(t39.contains("OPT-OUT NOT WORKING"));
        assert!(t39.contains("sent nothing"));
    }

    #[test]
    fn glyph_and_word_pairs_are_unique_across_states() {
        let p = Palette::fallback();
        let states = [
            StreamState::Off,
            StreamState::Blocked,
            StreamState::On,
            StreamState::OptOutIgnored,
            StreamState::RestartNeeded,
            StreamState::Unverified,
            StreamState::None,
        ];
        let mut seen = std::collections::HashSet::new();
        for st in states {
            let (g, w, _, _, _) = stream_style(st, &p);
            assert!(
                terminal::width(w) <= 8,
                "stream state word for {st:?} exceeds 8 cols: {w:?}"
            );
            assert!(
                seen.insert((g, w)),
                "duplicate (glyph, word) pair for {st:?}: ({g:?}, {w:?})"
            );
        }
    }

    #[test]
    fn restart_glyph_is_single_width() {
        let g = restart_glyph();
        let mut buf = [0u8; 4];
        assert_eq!(terminal::width(g.encode_utf8(&mut buf)), 1);
    }

    /// A click on a tab's name in the header shows that view, from Sources too; help ignores clicks.
    #[test]
    fn clicking_a_tab_name_switches_views() {
        let rep = demo::report(1_795_532_700_000.0);
        let mut state = State::new(Snapshot::default());
        state.demo = true;
        state.now = 1_795_532_705_000.0;
        let mut view = TelemetryView::new();
        let canvas = view.render(&state, Some(&rep), 1, false, false, 120, 40);
        let (sources, _) = terminal::find_in_row(&canvas, 0, " SOURCES ").unwrap();
        let (telemetry, _) = terminal::find_in_row(&canvas, 0, " TELEMETRY ").unwrap();
        assert!(view.press(sources + 2, 0));
        assert_eq!(view.tab, Tab::Sources);
        let canvas = view.render(&state, Some(&rep), 1, false, false, 120, 40);
        assert!(canvas.to_text().contains("LOCAL SOURCES INSPECTED"));
        assert!(!view.press(1, 0), "Loupe's name is not a tab");
        assert_eq!(view.tab, Tab::Sources);
        assert!(view.press(telemetry + 1, 0));
        assert_eq!(view.tab, Tab::Telemetry);
        view.help = true;
        assert!(!view.press(sources + 2, 0));
        assert_eq!(view.tab, Tab::Telemetry);
    }

    /// Help lists the shared keys in the key table's order and words.
    #[test]
    fn help_lists_the_shared_keys() {
        let rep = demo::report(1_795_532_700_000.0);
        let mut state = State::new(Snapshot::default());
        state.demo = true;
        let mut view = TelemetryView::new();
        view.help = true;
        let text = view.render(&state, Some(&rep), 1, false, false, 120, 40).to_text();
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle} missing:\n{text}"));
        assert!(at("J K / PgDn PgUp") < at("Tab / ⇧Tab"));
        assert!(at("s i e w") < at("c C / m"));
        assert!(at("? / h") < at("Esc / q"));
        assert!(text.contains("s Storage · i Insights · w Trace · e dashboard"));
    }

    #[test]
    fn board_body_cache_stays_flat_across_live_wakes_and_resizes_never_panic() {
        let rep = demo::report(1_795_532_700_000.0);
        let mut state = State::new(Snapshot::default());
        state.demo = true;
        state.now = 1_795_532_705_000.0;
        let mut view = TelemetryView::new();

        let _ = view.render(&state, Some(&rep), 1, false, false, 100, 30);
        assert_eq!(view.body_builds(), 1);
        // Simulate a Live wake (clock ticks forward, revision unchanged): body cache MUST hit.
        state.now += 2_000.0;
        let _ = view.render(&state, Some(&rep), 1, false, false, 100, 30);
        assert_eq!(view.body_builds(), 1);

        // Switching tab rebuilds body once.
        view.toggle_tab();
        let _ = view.render(&state, Some(&rep), 1, false, false, 100, 30);
        assert_eq!(view.body_builds(), 2);

        // Sweep sizes including below floor up to 300x80: must never panic.
        for &(cols, rows) in &[(40, 10), (48, 14), (52, 14), (79, 24), (80, 24), (120, 40), (300, 80)] {
            let c = view.render(&state, Some(&rep), 1, false, false, cols, rows);
            assert_eq!((c.w, c.h), (cols, rows));
        }
    }

    #[test]
    fn all_builtin_themes_keep_telemetry_inks_legible() {
        let paths = theme::Paths::from_env(None, None, None);
        for b in theme::BUILTINS {
            let (setting, _) = theme::setting(Some(b.name), None, &paths).unwrap();
            let themes = theme::Themes::new(setting, paths.clone());
            let p = themes.palette();
            for bg in [p.bg, p.panel] {
                assert!(
                    crate::integration::contrast(p.fg, bg) >= 4.5,
                    "theme {} fg contrast too low",
                    b.name
                );
                assert!(
                    crate::integration::contrast(p.hot, bg) >= 3.0,
                    "theme {} hot contrast too low",
                    b.name
                );
                assert!(
                    crate::integration::contrast(p.warn, bg) >= 3.0,
                    "theme {} warn contrast too low",
                    b.name
                );
            }
        }
    }

    #[test]
    fn command_formatter_never_splits_quotes_or_json() {
        let uwsm = "echo 'export T3CODE_TELEMETRY_ENABLED=false' >> ~/.config/uwsm/env";
        let (lines, clipped) = format_command_lines(uwsm, 41);
        assert!(!clipped, "uwsm command should fit via safe backslash continuations: {lines:?}");
        assert!(lines.iter().all(|l| terminal::width(l) <= 41), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("'export T3CODE_TELEMETRY_ENABLED=false'")),
            "single-quoted payload must stay intact: {lines:?}"
        );
        // Joining continuation lines reproduces the exact command.
        let joined = lines
            .iter()
            .map(|l| l.strip_suffix(" \\").unwrap_or(l))
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined, uwsm);

        let policy = "printf '{\"schemaVersion\":1,\"organization\":\"Local\",\"limits\":{\"telemetry\":false}}\\n' | sudo tee \"/etc/t3code/policy.json\"";
        let (lines_wide, clipped_wide) = format_command_lines(policy, 116);
        assert!(!clipped_wide);
        assert!(lines_wide.len() == 2 && lines_wide[0].ends_with(" \\"));
        // At narrow width where the JSON token itself won't fit, clip instead of breaking JSON.
        let (lines_narrow, clipped_narrow) = format_command_lines(policy, 50);
        assert!(clipped_narrow);
        assert_eq!(lines_narrow.len(), 1);
    }

    #[test]
    fn omarchy_shell_unverified_board_is_clear_actionable_and_has_no_stray_prompt() {
        let mut inp = demo::inputs(1_795_532_700_000.0);
        inp.uwsm_detected = false;
        inp.omarchy_hypr_detected = true;
        inp.hosts = Read::Absent;
        inp.shell = crate::telemetry::EnvSet::default();
        inp.shell
            .set_raw(crate::telemetry::EnvKey::TelemetryEnabled, "false");
        inp.server.liveness = Liveness::Running;
        inp.server.launch = crate::telemetry::Launch::Desktop("Hyprland".into());
        inp.shell_rc_source = Some(crate::telemetry::ShellRcSource {
            path: "~/.bashrc".into(),
            interactive_only: true,
        });
        let rep = crate::telemetry::evaluate(inp);

        let mut state = State::new(Snapshot::default());
        state.now = 1_795_532_710_000.0;
        let mut view = TelemetryView::new();

        for &(cols, rows) in &[(185, 55), (120, 34), (96, 28), (64, 22), (120, 16)] {
            view.scroll = 0;
            let canvas = view.render(&state, Some(&rep), 1, false, false, cols, rows);
            let text = canvas.to_text();
            assert!(
                !text.contains("have a T3 thread turn this off"),
                "stray floating prompt must not appear at {cols}x{rows}:\n{text}"
            );
            assert!(
                !text.contains("in shell  "),
                "confusing 'in shell' status badge must not appear at {cols}x{rows}:\n{text}"
            );
            assert!(
                text.contains("shell only") || text.contains("SHELL ONLY") || text.contains("~/.bashrc"),
                "clear ~/.bashrc shell-only diagnosis expected at {cols}x{rows}:\n{text}"
            );
            if cols >= 116 && rows >= 30 {
                assert!(
                    text.contains("1. Managed policy"),
                    "recommended managed policy option #1 expected without scrolling at {cols}x{rows}:\n{text}"
                );
                assert!(
                    text.contains("2. Omarchy Hyprland env"),
                    "no-sudo Omarchy Hyprland env option #2 expected without scrolling at {cols}x{rows}:\n{text}"
                );
                assert!(
                    !text.contains("sudo install -d"),
                    "raw shell command should stay collapsed by default at {cols}x{rows}:\n{text}"
                );
                assert!(
                    text.contains("VERDICT") && text.contains("PRODUCT EVENTS"),
                    "visual summary tiles expected at {cols}x{rows}:\n{text}"
                );
            } else if rows >= 22 {
                // Stacked viewport: full scrollable body contains both actionable fix cards.
                let body_text = view.body.as_ref().expect("body built").to_text();
                assert!(
                    body_text.contains("Managed policy") && body_text.contains("Omarchy Hyprland env"),
                    "stacked body should contain both options at {cols}x{rows}:\n{body_text}"
                );
            }
        }

        // Expanding commands reveals both copy-paste shell commands cleanly.
        view.toggle_commands();
        let expanded = view.render(&state, Some(&rep), 1, false, false, 160, 44).to_text();
        assert!(expanded.contains("hl.env(\"T3CODE_TELEMETRY_ENABLED\", \"false\")"));
        assert!(expanded.contains("sudo install -d -m 755 /etc/t3code"));
    }

    #[test]
    fn board_click_hit_testing_selects_grouped_rows() {
        let rep = demo::report(1_795_532_700_000.0);
        let mut state = State::new(Snapshot::default());
        state.demo = true;
        let mut view = TelemetryView::new();
        let _ = view.render(&state, Some(&rep), 1, false, false, 140, 40);
        assert_eq!(view.selected, 0);
        assert!(view.hits.len() == TelemetryView::selectable_count(&rep));
        // Clicking the screen row (`body_y + 1`) of the second stream selects index 1.
        let (second_body_y, second_idx) = view.hits[1];
        assert_eq!(second_idx, 1);
        assert!(view.press(2, second_body_y + 1));
        assert_eq!(view.selected, 1);
        // Clicking the header (y=0) away from the tab names returns false.
        assert!(!view.press(0, 0));
    }
}
