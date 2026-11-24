//! Numeric count of asks put to the reader in one message.
//!
//! A question ends in "?" and is addressed to the reader: it says you or your, it asks
//! leave, it offers a choice, or it sits under a heading or label that announces
//! questions or decisions. A statement asks without a question mark. A question the
//! writer answers, or one inside quotation marks, code, a table, a block quote, or a
//! heading, does not count. This is that prototype's behavior; further heuristics are
//! deferred.
//!
//! [`count_asks`] returns the sum. The SQL scalar `t3_ask_count` returns the same sum
//! and nothing else: borrowed text stays inside the call, and NULL, other storage
//! classes, and invalid UTF-8 return NULL. Reader queries return only the number.

use std::str::CharIndices;

/// Questions plus statements put to the reader. The text is borrowed and is not logged.
pub fn count_asks(text: &str) -> u32 {
    let (questions, statements) = tally(text);
    questions.saturating_add(statements)
}

/// Only the count crosses the SQL boundary. The borrowed input lives within this call.
pub fn register(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::{functions::FunctionFlags, types::ValueRef};
    db.create_scalar_function(
        "t3_ask_count",
        1,
        FunctionFlags::SQLITE_UTF8
            | FunctionFlags::SQLITE_DETERMINISTIC
            | FunctionFlags::SQLITE_INNOCUOUS,
        |ctx| {
            let count = match ctx.get_raw(0) {
                ValueRef::Text(bytes) => std::str::from_utf8(bytes)
                    .ok()
                    .map(|text| i64::from(count_asks(text))),
                _ => None,
            };
            Ok(count)
        },
    )
}

fn tally(text: &str) -> (u32, u32) {
    let mut questions = 0u32;
    let mut statements = 0u32;
    for para in paragraphs(text) {
        // No transformation in the sentence walk can introduce an ASCII '?'.
        // Statement asks still use the same normalized paragraph and rules.
        if !para.text.contains('?') {
            if !para.text.ends_with(':') && is_statement(&para.text) {
                statements = statements.saturating_add(1);
            }
            continue;
        }
        let mut label_scope = false;
        let sentences = sentences(&para.text);
        let mut had_question = false;
        for (i, sentence) in sentences.iter().enumerate() {
            let trimmed = sentence.trim();
            let mut body = if let Some(label) = match_label(trimmed) {
                label_scope = ask_scope(&label.text);
                trimmed[label.end..].to_string()
            } else {
                trimmed.to_string()
            };
            body = strip_edge(&body);
            if body.ends_with(':') && ask_scope(&body) {
                label_scope = true;
                continue;
            }
            if !ends_with_question(&body) || body.chars().count() < 6 {
                continue;
            }
            let answered = i + 1 < sentences.len();
            let mut direct = para.in_scope
                || label_scope
                || has_you(&body)
                || is_leave(&body)
                || (has_choice(&body) && !is_factual(&body));
            if direct && is_rhetorical(&body) && answered && !(para.in_scope || label_scope) {
                direct = false;
            }
            if direct {
                had_question = true;
                questions = questions.saturating_add(1);
            }
        }
        if !had_question && !para.text.ends_with(':') && is_statement(&para.text) {
            statements = statements.saturating_add(1);
        }
    }
    (questions, statements)
}

struct Para {
    in_scope: bool,
    text: String,
}

struct Label {
    text: String,
    end: usize,
}

fn paragraphs(text: &str) -> Vec<Para> {
    let mut scan = Scan::default();
    for raw in split_lines(text) {
        if is_fence(raw) {
            scan.flush();
            scan.fenced = !scan.fenced;
            continue;
        }
        if scan.fenced {
            continue;
        }
        let stripped = raw.trim();
        let head = heading_text(raw).or_else(|| bold_line_text(raw));
        let indented_code = raw.starts_with("    ") && scan.current.is_empty();
        if stripped.is_empty()
            || stripped.starts_with('>')
            || stripped.starts_with('|')
            || head.is_some()
            || indented_code
        {
            scan.flush();
            if let Some(title) = head {
                scan.heading_scope = ask_scope(title);
                scan.list_scope = false;
            }
            continue;
        }
        if let Some(marker_end) = list_marker_end(raw) {
            scan.flush();
            scan.current.push(raw[marker_end..].trim().to_string());
            scan.current_is_list = true;
        } else {
            if scan.current.is_empty() {
                scan.current_is_list = false;
            }
            scan.current.push(stripped.to_string());
        }
    }
    scan.flush();
    scan.out
}

#[derive(Default)]
struct Scan {
    fenced: bool,
    heading_scope: bool,
    list_scope: bool,
    current: Vec<String>,
    current_is_list: bool,
    out: Vec<Para>,
}

impl Scan {
    fn flush(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let para = plain(self.current.join(" ")).trim().to_string();
        let in_scope = self.heading_scope || (self.list_scope && self.current_is_list);
        if !self.current_is_list {
            self.list_scope = para.ends_with(':') && ask_scope(&para);
        }
        self.current.clear();
        self.out.push(Para {
            in_scope,
            text: para,
        });
    }
}

fn plain(text: String) -> String {
    // Keep the order: one substitution may expose a later pattern.
    let text = substitute(text, "![", find_image);
    let text = substitute(text, "`", find_inline_code);
    let text = substitute(text, "[", find_link);
    let text = substitute(text, "://", find_url);
    substitute(text, "?", find_quoted)
}

fn substitute(
    text: String,
    required: &str,
    find: fn(&str) -> Option<(usize, usize, &str)>,
) -> String {
    if !text.contains(required) {
        return text;
    }
    let mut out = None;
    let mut pos = 0;
    while let Some((start, end, replacement)) = find(&text[pos..]) {
        let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
        out.push_str(&text[pos..pos + start]);
        out.push_str(replacement);
        pos += end;
    }
    match out {
        Some(mut out) => {
            out.push_str(&text[pos..]);
            out
        }
        None => text,
    }
}

fn find_image(text: &str) -> Option<(usize, usize, &str)> {
    let (start, _, end) = find_bracket_span(text, "![")?;
    Some((start, end, ""))
}

fn find_inline_code(text: &str) -> Option<(usize, usize, &str)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            if let Some(close) = text[i + 1..].find('`') {
                return Some((i, i + 1 + close + 1, ""));
            }
        }
        i += len_at(text, i);
    }
    None
}

fn find_link(text: &str) -> Option<(usize, usize, &str)> {
    let (start, label_end, end) = find_bracket_span(text, "[")?;
    Some((start, end, &text[start + 1..label_end]))
}

fn find_bracket_span(text: &str, opener: &str) -> Option<(usize, usize, usize)> {
    let mut pos = 0;
    loop {
        let start = pos + text[pos..].find(opener)?;
        let label_start = start + opener.len();
        let label_end = label_start + text[label_start..].find(']')?;
        pos = label_end + 1;
        // Every opener before this ']' uses the same closer. If it fails,
        // resume after it. If there is no ')' left, later openers also fail.
        if text[pos..].starts_with('(') {
            let end = pos + 1 + text[pos + 1..].find(')')? + 1;
            return Some((start, label_end, end));
        }
    }
}

fn find_url(text: &str) -> Option<(usize, usize, &str)> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let scheme = if bytes[i..].starts_with(b"https://") {
            Some(8)
        } else if bytes[i..].starts_with(b"http://") {
            Some(7)
        } else {
            None
        };
        if let Some(scheme) = scheme {
            let mut j = i + scheme;
            let content = j;
            while j < bytes.len() && !text[j..].chars().next().is_some_and(char::is_whitespace) {
                j += len_at(text, j);
            }
            if j > content {
                return Some((i, j, ""));
            }
        }
        i += len_at(text, i);
    }
    None
}

fn find_quoted(text: &str) -> Option<(usize, usize, &str)> {
    let mut chars = text.char_indices();
    let mut prev_is_word = false;
    while let Some((at, ch)) = chars.next() {
        let end = match ch {
            '"' | '\u{201C}' => quoted_span(
                chars.clone(),
                240,
                |c| matches!(c, '"' | '\u{201C}' | '\u{201D}'),
                |c| matches!(c, '"' | '\u{201D}'),
            ),
            '\'' if !prev_is_word => quoted_span(chars.clone(), 120, |c| c == '\'', |c| c == '\''),
            _ => None,
        };
        if let Some(end) = end {
            return Some((at, end, ""));
        }
        prev_is_word = is_word_char(ch);
    }
    None
}

fn quoted_span(
    mut chars: CharIndices<'_>,
    limit: usize,
    forbidden: impl Fn(char) -> bool,
    closes: impl Fn(char) -> bool,
) -> Option<usize> {
    for content in 0..=limit {
        let (_, ch) = chars.next()?;
        if ch == '?' {
            if let Some((at, closer)) = chars.clone().next().filter(|&(_, c)| closes(c)) {
                return Some(at + closer.len_utf8());
            }
        }
        if forbidden(ch) || content == limit {
            return None;
        }
    }
    None
}

fn len_at(text: &str, i: usize) -> usize {
    text[i..].chars().next().map_or(1, char::len_utf8)
}

fn sentences(para: &str) -> Vec<String> {
    split_sentences(para)
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Split after `.` `!` `?` or `:`. Closers and the following whitespace are the separator,
/// so the terminator stays with the sentence before it.
fn split_sentences(para: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = para.char_indices().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        if matches!(chars[i].1, '.' | '!' | '?' | ':') {
            let mut j = i + 1;
            while j < chars.len() && is_sentence_closer(chars[j].1) {
                j += 1;
            }
            let mut k = j;
            let mut saw_space = false;
            while k < chars.len() && chars[k].1.is_whitespace() {
                saw_space = true;
                k += 1;
            }
            if saw_space && k < chars.len() && is_sentence_start(chars[k].1) {
                let punct_end = chars[i].0 + chars[i].1.len_utf8();
                parts.push(&para[start..punct_end]);
                start = chars[k].0;
                i = k;
                continue;
            }
        }
        i += 1;
    }
    parts.push(&para[start..]);
    parts
}

fn is_sentence_closer(c: char) -> bool {
    matches!(c, '"' | '\'' | '\u{201D}' | ')' | '*' | '_')
}

fn is_sentence_start(c: char) -> bool {
    c.is_ascii_alphabetic()
        || c.is_digit(10)
        || matches!(c, '"' | '\'' | '\u{201C}' | '(' | '*' | '_' | '[' | '\\')
}

fn is_fence(line: &str) -> bool {
    let rest = trim_start_ws(line);
    rest.starts_with("```") || rest.starts_with("~~~")
}

fn heading_text(line: &str) -> Option<&str> {
    let indent = line
        .chars()
        .take(3)
        .take_while(|c| c.is_whitespace())
        .map(char::len_utf8)
        .sum::<usize>();
    let rest = &line[indent..];
    let hashes = rest.bytes().take_while(|&b| b == b'#').count();
    let title = &rest[hashes..];
    if !(1..=6).contains(&hashes) || !title.starts_with(char::is_whitespace) {
        return None;
    }
    Some(trim_start_ws(title))
}

fn bold_line_text(line: &str) -> Option<&str> {
    let trimmed = trim_start_ws(line);
    let body = strip_bold_marker(trimmed).unwrap_or(trimmed);
    if !body.starts_with("**") {
        return None;
    }
    let after = &body[2..];
    let star = after.find('*')?;
    if star == 0 || !after[star..].starts_with("**") {
        return None;
    }
    let mut i = star + 2;
    let bytes = after.as_bytes();
    if bytes.get(i) == Some(&b':') {
        i += 1;
    }
    if after[i..].chars().all(char::is_whitespace) {
        Some(&after[..star])
    } else {
        None
    }
}

fn strip_bold_marker(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if !matches!(bytes.first(), Some(b'-' | b'*')) {
        return None;
    }
    let ws = eat_ws(&line[1..])?;
    Some(&line[1 + ws..])
}

fn list_marker_end(line: &str) -> Option<usize> {
    let rest = trim_start_ws(line);
    let indent = line.len() - rest.len();
    let bytes = rest.as_bytes();
    let marker = if matches!(bytes.first(), Some(b'-' | b'*' | b'+')) {
        1
    } else {
        let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
        if digits == 0 || !matches!(bytes.get(digits), Some(b'.' | b')')) {
            return None;
        }
        digits + 1
    };
    let ws = eat_ws(&rest[marker..])?;
    Some(indent + marker + ws)
}

fn match_label(sentence: &str) -> Option<Label> {
    match_bold_label(sentence).or_else(|| match_plain_label(sentence))
}

fn match_bold_label(sentence: &str) -> Option<Label> {
    if !sentence.starts_with("**") {
        return None;
    }
    let after = &sentence[2..];
    let star = after.find('*')?;
    if star == 0 || !after[star..].starts_with("**") {
        return None;
    }
    let before = &after[..star];
    let (content, _) =
        if before.len() >= 2 && matches!(before.as_bytes()[before.len() - 1], b':' | b'.') {
            (&before[..before.len() - 1], 1)
        } else {
            (before, 0)
        };
    let content_len = content.chars().count();
    if !(1..=70).contains(&content_len) {
        return None;
    }
    let mut i = star + 2;
    let bytes = after.as_bytes();
    if matches!(bytes.get(i), Some(b':' | b'.')) {
        i += 1;
    }
    let ws = eat_ws(&after[i..])?;
    Some(Label {
        text: content.to_string(),
        end: 2 + i + ws,
    })
}

fn match_plain_label(sentence: &str) -> Option<Label> {
    let mut chars = sentence.char_indices();
    let (_, first) = chars.next()?;
    if !first.is_ascii_uppercase() {
        return None;
    }
    let mut text = String::from(first);
    let mut end = first.len_utf8();
    let mut extra = 0;
    for (_, c) in chars {
        if extra == 32 || !(is_word_char(c) || matches!(c, '\'' | ' ' | '-')) {
            break;
        }
        text.push(c);
        extra += 1;
        end += c.len_utf8();
    }
    if !sentence[end..].starts_with(':') {
        return None;
    }
    let ws = eat_ws(&sentence[end + 1..])?;
    Some(Label {
        text,
        end: end + 1 + ws,
    })
}

fn ask_scope(text: &str) -> bool {
    text.char_indices()
        .any(|(i, _)| word_start(text, i) && ask_scope_at(text, i))
}

fn ask_scope_at(text: &str, i: usize) -> bool {
    let s = &text[i..];
    if bounded(text, i, optional_s(s, "question")) || bounded(text, i, optional_s(s, "decision")) {
        return true;
    }
    if let Some(n) = eat(s, "need") {
        let n = eat(&s[n..], "ed")
            .map(|m| n + m)
            .or_else(|| eat(&s[n..], "s").map(|m| n + m))
            .unwrap_or(n);
        if let Some(m) = eat(&s[n..], " from you") {
            if boundary(text, i + n + m) {
                return true;
            }
        }
    }
    if bounded(text, i, eat(s, "your call")) || bounded(text, i, eat(s, "for you")) {
        return true;
    }
    if let Some(n) = eat(s, "open ") {
        for stem in ["item", "point"] {
            if bounded(text, i, optional_s(&s[n..], stem).map(|m| n + m)) {
                return true;
            }
        }
    }
    [
        "still open",
        "to confirm",
        "please confirm",
        "to settle",
        "blocked on",
        "over to you",
        "what i need",
        "request",
    ]
    .iter()
    .any(|phrase| bounded(text, i, eat(s, phrase)))
}

fn has_you(text: &str) -> bool {
    const WORDS: &[&str] = &[
        "you", "your", "yours", "you'd", "you'll", "you're", "you've",
    ];
    text.char_indices().any(|(i, _)| {
        word_start(text, i)
            && WORDS
                .iter()
                .any(|word| bounded(text, i, eat(&text[i..], word)))
    })
}

fn is_leave(body: &str) -> bool {
    let p = leave_prefix(body);
    leave_at(body, p)
}

fn leave_at(body: &str, p: usize) -> bool {
    let s = &body[p..];
    if ["may", "should", "shall"]
        .iter()
        .any(|word| bounded(body, p, eat(s, word)))
    {
        return true;
    }
    for modal in ["can", "could", "do", "would", "will", "must"] {
        let Some(n) = eat(s, modal) else { continue };
        let Some(ws) = eat_ws(&s[n..]) else { continue };
        let rest = &s[n + ws..];
        if ["i", "we"]
            .iter()
            .any(|pronoun| bounded(body, p, eat(rest, pronoun).map(|m| n + ws + m)))
        {
            return true;
        }
    }
    if bounded(body, p, eat(s, "want me to")) {
        return true;
    }
    if let Some(n) = eat(s, "ok") {
        let n = eat(&s[n..], "ay").map(|m| n + m).unwrap_or(n);
        if let Some(ws) = eat_ws(&s[n..]) {
            let rest = &s[n + ws..];
            if ["to", "if", "with"]
                .iter()
                .any(|word| bounded(body, p, eat(rest, word).map(|m| n + ws + m)))
            {
                return true;
            }
        }
    }
    if let Some(n) = eat(s, "is ") {
        let object = ["it ", "that ", "this "]
            .iter()
            .find_map(|word| eat(&s[n..], word));
        if let Some(object) = object {
            let rest = &s[n + object..];
            if ["ok", "okay", "fine", "alright", "acceptable"]
                .iter()
                .any(|word| bounded(body, p, eat(rest, word).map(|m| n + object + m)))
            {
                return true;
            }
        }
    }
    if let Some(n) = eat(s, "any ") {
        if ["preference", "objection", "concern"]
            .iter()
            .any(|stem| bounded(body, p, optional_s(&s[n..], stem).map(|m| n + m)))
        {
            return true;
        }
    }
    if let Some(n) = eat(s, "sound") {
        let n = eat(&s[n..], "s").map(|m| n + m).unwrap_or(n);
        if s[n..].starts_with(' ') {
            let rest = &s[n + 1..];
            if ["good", "right", "ok"]
                .iter()
                .any(|word| bounded(body, p, eat(rest, word).map(|m| n + 1 + m)))
            {
                return true;
            }
        }
    }
    bounded(body, p, eat(s, "go ahead")) || bounded(body, p, eat(s, "proceed"))
}

fn is_factual(body: &str) -> bool {
    let p = factual_prefix(body);
    let s = &body[p..];
    [
        "does", "did", "do", "is", "are", "was", "were", "has", "have", "had", "where", "what",
        "why", "how", "when", "who", "which", "can", "could", "will", "would",
    ]
    .iter()
    .any(|word| bounded(body, p, eat(s, word)))
}

fn is_rhetorical(body: &str) -> bool {
    let p = eat_ws_prefix(body, "so").unwrap_or(0);
    let s = &body[p..];
    if ["how come", "what if", "what changed", "what now", "why"]
        .iter()
        .any(|phrase| bounded(body, p, eat(s, phrase)))
    {
        return true;
    }
    let Some(n) = eat(s, "what ") else {
        return false;
    };
    let Some(verb) = eat(&s[n..], "does ").or_else(|| eat(&s[n..], "did ")) else {
        return false;
    };
    let rest = &s[n + verb..];
    let Some(object) = eat(rest, "this ")
        .or_else(|| eat(rest, "that "))
        .or_else(|| eat(rest, "it "))
    else {
        return false;
    };
    bounded(
        body,
        p,
        eat(&rest[object..], "mean").map(|m| n + verb + object + m),
    )
}

fn has_choice(text: &str) -> bool {
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for i in 0..chars.len() {
        let or_at = if chars[i].1 == ',' {
            (i + 2 < chars.len() && chars[i + 1].1.is_whitespace()).then_some(i + 2)
        } else if chars[i].1.is_whitespace() {
            (i + 1 < chars.len()).then_some(i + 1)
        } else {
            None
        };
        let Some(at) = or_at else { continue };
        let start = chars[at].0;
        if eat(&text[start..], "or").is_some()
            && boundary(text, start)
            && boundary(text, start + 2)
            && text[start + 2..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
        {
            return true;
        }
    }
    false
}

fn is_statement(text: &str) -> bool {
    text.char_indices()
        .any(|(i, _)| word_start(text, i) && statement_at(text, i))
}

fn statement_at(text: &str, i: usize) -> bool {
    let s = &text[i..];
    if let Some(n) = eat(s, "still ") {
        let rest = &s[n..];
        if eat(rest, "open with you").is_some()
            || eat(rest, "open from before").is_some()
            || eat(rest, "open for you").is_some()
            || eat(rest, "open:").is_some()
            || eat(rest, "unanswered").is_some()
        {
            return true;
        }
        if let Some(m) = eat(rest, "waiting ") {
            if let Some(prep) = eat(&rest[m..], "on ").or_else(|| eat(&rest[m..], "for ")) {
                let who = &rest[m + prep..];
                if eat(who, "you").is_some() {
                    return true;
                }
            }
        }
    }
    if waiting_on_your(s) || need_your(s) || tell_or_know(s) || decision_still(s) || for_your(s) {
        return true;
    }
    if let Some(n) = eat(s, "awaiting your") {
        if boundary(text, i + n) {
            return true;
        }
    }
    eat(s, "say the word").is_some()
        || eat(s, "say if you want").is_some()
        || eat(s, "without your word").is_some()
        || waiting_for_you(s)
}

fn waiting_on_your(s: &str) -> bool {
    let Some(n) = eat(s, "waiting ").or_else(|| eat(s, "wait ")) else {
        return false;
    };
    let Some(prep) = eat(&s[n..], "on ").or_else(|| eat(&s[n..], "for ")) else {
        return false;
    };
    let Some(your) = eat(&s[n + prep..], "your ") else {
        return false;
    };
    your_noun(&s[n + prep + your..])
}

fn need_your(s: &str) -> bool {
    let Some(n) = eat(s, "needed ")
        .or_else(|| eat(s, "needs "))
        .or_else(|| eat(s, "need "))
    else {
        return false;
    };
    let Some(your) = eat(&s[n..], "your ") else {
        return false;
    };
    const NOUNS: &[&str] = &[
        "word", "call", "ok", "okay", "decision", "answer", "approval", "input", "go-ahead",
    ];
    NOUNS.iter().any(|noun| eat(&s[n + your..], noun).is_some())
}

fn your_noun(s: &str) -> bool {
    const NOUNS: &[&str] = &[
        "word", "call", "answer", "go-ahead", "ok", "okay", "decision", "review", "approval",
        "try-out", "reply", "input",
    ];
    NOUNS.iter().any(|noun| eat(s, noun).is_some())
}

fn tell_or_know(s: &str) -> bool {
    let Some(n) = eat(s, "tell me ").or_else(|| eat(s, "let me know ")) else {
        return false;
    };
    ["which", "if", "whether"]
        .iter()
        .any(|word| eat(&s[n..], word).is_some())
}

fn waiting_for_you(s: &str) -> bool {
    let Some(n) = eat(s, "are ").or_else(|| eat(s, "is ")) else {
        return false;
    };
    eat(&s[n..], "waiting for you").is_some()
}

fn decision_still(s: &str) -> bool {
    let Some(n) = optional_s(s, "decision").or_else(|| optional_s(s, "question")) else {
        return false;
    };
    if !s[n..].starts_with(' ') {
        return false;
    }
    let rest = &s[n + 1..];
    if eat(rest, "for you").is_some() {
        return true;
    }
    let rest = eat(rest, "are ")
        .or_else(|| eat(rest, "is "))
        .map(|m| &rest[m..])
        .unwrap_or(rest);
    eat(rest, "still open").is_some()
}

fn for_your(s: &str) -> bool {
    let Some(n) = eat(s, "for your ") else {
        return false;
    };
    ["call", "word", "decision"]
        .iter()
        .any(|noun| eat(&s[n..], noun).is_some())
}

fn leave_prefix(s: &str) -> usize {
    comma_ws_prefix(s, "so")
        .or_else(|| ws_prefix(s, "and"))
        .or_else(|| ws_prefix(s, "or"))
        .or_else(|| comma_ws_prefix(s, "also"))
        .or_else(|| comma_ws_prefix(s, "first"))
        .unwrap_or(0)
}

fn factual_prefix(s: &str) -> usize {
    comma_ws_prefix(s, "so")
        .or_else(|| ws_prefix(s, "and"))
        .or_else(|| ws_prefix(s, "but"))
        .or_else(|| comma_ws_prefix(s, "first"))
        .or_else(|| comma_ws_prefix(s, "next"))
        .or_else(|| comma_ws_prefix(s, "then"))
        .unwrap_or(0)
}

fn comma_ws_prefix(s: &str, word: &str) -> Option<usize> {
    let n = eat(s, word)?;
    let n = if s[n..].starts_with(',') { n + 1 } else { n };
    Some(n + eat_ws(&s[n..])?)
}

fn ws_prefix(s: &str, word: &str) -> Option<usize> {
    let n = eat(s, word)?;
    Some(n + eat_ws(&s[n..])?)
}

fn eat_ws_prefix(s: &str, word: &str) -> Option<usize> {
    ws_prefix(s, word)
}

fn optional_s(s: &str, stem: &str) -> Option<usize> {
    let n = eat(s, stem)?;
    Some(eat(&s[n..], "s").map(|m| n + m).unwrap_or(n))
}

fn bounded(text: &str, base: usize, consumed: Option<usize>) -> bool {
    consumed.is_some_and(|n| boundary(text, base + n))
}

fn eat(s: &str, lit: &str) -> Option<usize> {
    let mut chars = s.chars();
    for expected in lit.chars() {
        if !chars
            .next()
            .is_some_and(|got| got.eq_ignore_ascii_case(&expected))
        {
            return None;
        }
    }
    Some(lit.len())
}

fn eat_ws(s: &str) -> Option<usize> {
    let mut n = 0;
    for c in s.chars() {
        if !c.is_whitespace() {
            break;
        }
        n += c.len_utf8();
    }
    (n > 0).then_some(n)
}

fn trim_start_ws(s: &str) -> &str {
    s.trim_start_matches(char::is_whitespace)
}

fn strip_edge(s: &str) -> String {
    s.trim_matches(is_edge).to_string()
}

fn ends_with_question(s: &str) -> bool {
    s.trim_end_matches(is_edge).ends_with('?')
}

fn is_edge(c: char) -> bool {
    matches!(
        c,
        ' ' | '\t'
            | '*'
            | '_'
            | ')'
            | '"'
            | '\''
            | '\u{201D}'
            | '\u{2019}'
            | ']'
            | '\u{201C}'
            | '('
            | '['
    )
}

fn is_word_char(c: char) -> bool {
    c == '_' || c.is_alphanumeric()
}

fn boundary(text: &str, i: usize) -> bool {
    let prev = text[..i].chars().next_back().map(is_word_char);
    let next = text[i..].chars().next().map(is_word_char);
    match (prev, next) {
        (Some(left), Some(right)) => left != right,
        (None, Some(right)) => right,
        (Some(left), None) => left,
        (None, None) => false,
    }
}

fn word_start(text: &str, i: usize) -> bool {
    text[i..].chars().next().is_some_and(is_word_char) && boundary(text, i)
}

fn split_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    let mut ended_on_break = false;
    while let Some((i, c)) = chars.next() {
        let width = line_break(c, chars.peek().map(|(_, next)| *next));
        if width == 0 {
            ended_on_break = false;
            continue;
        }
        out.push(&text[start..i]);
        if c == '\r' && width > 1 {
            chars.next();
        }
        start = i + width;
        ended_on_break = true;
    }
    if !ended_on_break {
        out.push(&text[start..]);
    }
    out
}

fn line_break(c: char, next: Option<char>) -> usize {
    match c {
        '\r' if next == Some('\n') => 2,
        '\n' | '\r' | '\u{000B}' | '\u{000C}' | '\u{001C}' | '\u{001D}' | '\u{001E}'
        | '\u{0085}' | '\u{2028}' | '\u{2029}' => c.len_utf8(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::{count_asks, plain, register, tally};

    fn expect(cases: &[(&str, u32, u32)]) {
        for (i, (text, questions, statements)) in cases.iter().enumerate() {
            assert_eq!(tally(text), (*questions, *statements), "case {i}");
            assert_eq!(
                count_asks(text),
                questions.saturating_add(*statements),
                "sum {i}"
            );
        }
    }

    #[test]
    fn questionless_paragraphs_still_count_statements_after_markup_cleanup() {
        expect(&[
            (
                "Completed the requested change and verified the test suite.",
                0,
                0,
            ),
            ("Tell me which version.", 0, 1),
            ("I need [your decision](https://example.test).", 0, 1),
            ("I need `an inline example`your decision.", 0, 1),
            ("I need your decision:", 0, 0),
            ("… I need\u{2028}your decision.", 0, 1),
            ("![I need your decision](https://example.test).", 0, 0),
        ]);
    }

    #[test]
    fn prototype_cases_match_the_python_classifier() {
        expect(&[
            ("Should I run a third count-only check?", 1, 0),
            (
                "3. May Loupe measure folder sizes on disk? Today it reads only the database.",
                1,
                0,
            ),
            (
                "- **Storage:** do you want the warning in the bar too?",
                1,
                0,
            ),
            ("Why does this fail? The parser drops the quote.", 0, 0),
            ("Why does the test fail?", 0, 0),
            ("## Why not the strictest form?", 0, 0),
            (
                "## Decisions for you\n\n1. One table or six lenses?\n2. Ship the month first?",
                2,
                0,
            ),
            ("```\nshould I do this?\n```", 0, 0),
            ("> Should I keep it?", 0, 0),
            ("Run `git log -- file?` to see", 0, 0),
            (
                "Does the function work on a read-only connection? Let me test.",
                0,
                0,
            ),
            ("First, did the plugin load?", 0, 0),
            (
                "Before going further: do any tests in this checkout already read these fixtures? If one does, they could break.",
                0,
                0,
            ),
            ("Want me to start a Storage thread?", 1, 0),
            ("Is the build green?", 0, 0),
            ("1. Trace first, or the task list first?", 1, 0),
            (
                "The video opens on \"Why not just put it in CLAUDE.md?\", plus a still if you'd rather post an image.",
                0,
                0,
            ),
            (
                "**One question for you:** may the reader also take a command's exit code? It is a number.",
                1,
                0,
            ),
            (
                "Two decisions are still open before I write v1.1:\n\n1. **Classifier:** turn it on as an opt-in? It reads labels.\n2. **Deleted threads:** keep their counts? I'd keep them.",
                2,
                0,
            ),
            (
                "Still waiting on your word for restoring the original install.",
                0,
                1,
            ),
            ("Your SSH command is still open in that pager.", 0, 0),
            (
                "- **The time bar adds up too:** tools 62h, model 70h, waiting on you 7.0h.",
                0,
                0,
            ),
            (
                "May I run it once on your database,\nread-only and numbers only as before,\nto confirm the counting?",
                1,
                0,
            ),
            ("Is it the parser or the lexer? Checking both.", 0, 0),
        ]);
    }

    #[test]
    fn markdown_quotes_scope_and_self_questions_stay_exact() {
        expect(&[
            ("", 0, 0),
            ("?", 0, 0),
            ("Hi?", 0, 0),
            ("May I?", 1, 0),
            ("Why did you do that?", 1, 0),
            ("Why did you do that? Because the quote was dropped.", 0, 0),
            ("## Decisions for you\n\nWhy not ship it?", 1, 0),
            ("Note: should I continue?", 1, 0),
            ("Storage: do you want this?", 1, 0),
            ("**Questions**\n\nShip it?", 1, 0),
            ("- **Questions:**\n\nShip the month?", 1, 0),
            ("say the word", 0, 1),
            ("I need your decision before continuing.", 0, 1),
            ("Questions for you are still open.", 0, 1),
            ("A question inside `should I?` stays out", 0, 0),
            (
                "See [the note](https://example.com/a?b=1) and move on",
                0,
                0,
            ),
            ("See ![x](https://example.com/a.png) later?", 0, 0),
            ("He said 'keep it?' and left.", 0, 0),
            ("He said \u{201C}Should I keep it?\u{201D} yesterday.", 0, 0),
            ("    should I do this?", 0, 0),
            ("```rust\nshould I do this?\n```\n\nShould I do this?", 1, 0),
            ("~~~\nshould I?\n~~~\n", 0, 0),
            ("> should I?\n\nShould I?", 1, 0),
            ("| Should I? |\n", 0, 0),
            ("## Not a scope\n\nShould I run it?", 1, 0),
            ("First, should I run it?", 1, 0),
            ("So, may I run it?", 1, 0),
            ("But is the build green?", 0, 0),
            ("Trace first, or stop?", 1, 0),
            ("Can we ship this?", 1, 0),
            ("Can the parser ship this?", 0, 0),
            ("Do I continue?", 1, 0),
            ("Would we keep it?", 1, 0),
            ("Is it okay to proceed?", 1, 0),
            ("Okay to merge?", 1, 0),
            ("Any objections?", 1, 0),
            ("Sound good?", 1, 0),
            ("Go ahead?", 1, 0),
            ("What if you want both? I can do that.", 0, 0),
            ("What if we stop? The next line answers.", 0, 0),
            ("Still open: the color.", 0, 1),
            ("still open for you", 0, 1),
            ("awaiting your review now", 0, 1),
            ("let me know whether to keep the row", 0, 1),
            ("without your word I will wait", 0, 1),
            ("for your call on the name", 0, 1),
            ("The question is still open.", 0, 1),
            ("Two decisions are still open.", 0, 1),
            ("**One question for you:**\n\nMay I?", 1, 0),
            ("Hello? You there?", 1, 0),
            (
                "\u{201C}Why?\u{201D} is the title, but should I rename it?",
                0,
                0,
            ),
            ("Before going further: should I read the fixtures?", 1, 0),
            ("1) May I measure it?", 1, 0),
            ("* Should I stop?", 1, 0),
            ("+ Should I stop?", 1, 0),
            ("10. Should I stop?", 1, 0),
            ("Not a list. Should I stop?", 1, 0),
            ("Decisions:\n\n1. Ship it?\n2. Wait?", 1, 0),
            ("Something else:\n\n1. Ship it?", 0, 0),
            ("Your call\n\n1. Ship it now?", 0, 0),
            ("### Decisions for you\nShip now?", 1, 0),
            ("#### Questions\n- one?\n- two?", 0, 0),
            ("The link [should I?](http://x) remains?", 0, 0),
            ("Run https://example.com/path?q=1 then stop", 0, 0),
            ("It's your call.", 0, 0),
            ("needed your approval yesterday", 0, 1),
            ("needs your ok", 0, 1),
            ("tell me which file", 0, 1),
            ("are waiting for you at the door", 0, 1),
            ("questions are still open today", 0, 1),
            ("decision is still open", 0, 1),
            ("So should I?", 1, 0),
            ("Also, shall we continue?", 1, 0),
            ("And will I regret it?", 1, 0),
            ("Or must we stop?", 1, 0),
            ("Please confirm the count?", 0, 0),
            ("## Please confirm\n\nThe count is 4.", 0, 0),
            ("## Please confirm\n\nIs four right?", 1, 0),
            ("Blocked on you?", 1, 0),
            ("I am blocked on you for the name.", 0, 0),
            ("over to you", 0, 0),
            ("what I need is the name", 0, 0),
            ("to settle the name?", 0, 0),
            ("A, or B?", 1, 0),
            ("Red or blue?", 1, 0),
            ("red, or blue?", 1, 0),
            ("Is red, or blue, the one? I checked.", 0, 0),
            ("\nShould I?\n", 1, 0),
            ("\"Should I keep it?\"", 0, 0),
            ("'file?' is the name", 0, 0),
            ("the function's name?", 0, 0),
            ("See `code` and should I merge?", 0, 0),
            (
                "## Decisions for you\n\nKeep it?\n\n## Notes\n\nKeep it?",
                1,
                0,
            ),
            ("**Decisions:**\n\nKeep it?", 1, 0),
            ("- **Decisions:**\n\nKeep it?", 1, 0),
            ("**Not scope:**\n\nKeep it?", 0, 0),
            ("Still waiting on your word? I can wait.", 1, 0),
            ("No ask here. The build finished.", 0, 0),
            ("MAY I RUN IT?", 1, 0),
            ("Do you want it?", 1, 0),
            ("What do you want?", 1, 0),
            ("How do you want it done? I'll start.", 1, 0),
            ("Which file do you want?", 1, 0),
            ("Can you read it?", 1, 0),
            ("Must I stop?", 1, 0),
            ("Shall I stop?", 1, 0),
            ("Is that okay?", 1, 0),
            ("Any concern?", 1, 0),
            ("Any preferences?", 1, 0),
            ("Sounds right?", 1, 0),
            ("Proceed?", 1, 0),
            ("Want me to?", 1, 0),
            ("ok to ship?", 1, 0),
            ("okay if I ship?", 1, 0),
            ("okay with waiting?", 1, 0),
            ("First, should I go?", 1, 0),
            ("Next, does it load?", 0, 0),
            ("Then, is it loaded?", 0, 0),
            ("And do you agree?", 1, 0),
            ("But should I stop?", 0, 0),
            ("So, is it your call?", 1, 0),
            ("May I run it once on your database\r\nto confirm?", 1, 0),
        ]);
    }

    #[test]
    fn bracket_normalization_keeps_first_closer_and_recovers_after_failed_labels() {
        for (input, expected) in [
            ("[broken ![also broken", "[broken ![also broken"),
            (
                "[first](unclosed ![second](unclosed",
                "[first](unclosed ![second](unclosed",
            ),
            (
                "[outer [inner] text] [café](docs)",
                "[outer [inner] text] café",
            ),
            (
                "![outer ![inner] text] ![café](image)",
                "![outer ![inner] text] ",
            ),
            ("[outer [inner](docs) tail)", "outer [inner tail)"),
            ("![outer ![inner](image) tail)", " tail)"),
            ("[`label`](docs) ![`alt`](image)", " "),
        ] {
            assert_eq!(plain(input.into()), expected, "{input}");
        }
    }

    #[test]
    fn many_short_quoted_questions_leave_the_reader_question() {
        let input = format!(
            "Notes cite \"Should I proceed?\" and “Can you wait?” and 'May I stop?'. {}\n\nShould I send the summary?",
            "Examples include “Can you wait?” and 'May I stop?'. ".repeat(4_096)
        );
        assert_eq!(tally(&input), (1, 0));
        assert_eq!(
            plain(r#"é'do you agree?' "can you wait?"'may I stop?'"#.into()),
            "é'do you agree?' "
        );
        for (open, close, limit) in [('“', '”', 240), ('\'', '\'', 120)] {
            let at_limit = format!("{open}{}?{close}", "é".repeat(limit));
            assert_eq!(plain(at_limit), "");
            let too_long = format!("{open}{}?{close}", "é".repeat(limit + 1));
            assert_eq!(plain(too_long.clone()), too_long);
        }
    }

    #[test]
    fn sql_scalar_returns_only_the_count() {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        register(&db).unwrap();
        db.execute_batch("CREATE TABLE messages(id INTEGER PRIMARY KEY, body TEXT)")
            .unwrap();
        let rows = [
            "Should I run a third count-only check?",
            "Why does the test fail?",
            "Still waiting on your word for restoring the original install.",
            "",
        ];
        for (id, body) in rows.iter().enumerate() {
            db.execute(
                "INSERT INTO messages(id, body) VALUES (?1, ?2)",
                rusqlite::params![id as i64, body],
            )
            .unwrap();
        }
        db.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON")
            .unwrap();
        assert!(
            db.execute("INSERT INTO messages(id, body) VALUES (9, 'x')", [])
                .is_err()
        );
        let flags: i64 = db
            .query_row(
                "SELECT flags FROM pragma_function_list WHERE name='t3_ask_count' AND narg=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(flags & (0x800 | 0x200000), 0x800 | 0x200000);
        let mut stmt = db
            .prepare("SELECT id, t3_ask_count(body) FROM messages ORDER BY id")
            .unwrap();
        let got: Vec<i64> = stmt
            .query_map([], |row| row.get(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let expected: Vec<i64> = rows
            .iter()
            .map(|body| i64::from(count_asks(body)))
            .collect();
        assert_eq!(got, expected);
        for value in [
            "NULL",
            "17",
            "1.5",
            "x'636172676f'",
            "CAST(x'80ff' AS TEXT)",
        ] {
            let result: Option<i64> = db
                .query_row(&format!("SELECT t3_ask_count({value})"), [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(result, None, "{value}");
        }
    }
}
