//! The text and number helpers against digests recorded from the TypeScript original (main
//! `2f1d553`): the same ~5.1 million generated inputs `checks/scripts/text-parity.ts` feeds both
//! builds, replayed with only `cargo test`. No Node, no generated files.
//!
//! The recording cannot hold 5.1 million inputs or outputs, so it holds digests (see
//! `checks/fixtures/golden/README.md`):
//!
//! * per group of inputs, a digest of the exact bytes piped to the probe. `golden_text/generators.rs`
//!   is a Rust port of the gate's generators; if it builds anything else, the test says which
//!   group and blames the port, not the helper;
//! * per group, the oracle's first 64 outputs verbatim, so a failure there names the input;
//! * per group, one digest per further 64 outputs, so a failure there names a block of inputs.
//!
//! The helpers run in the same binary and the same way as the live gate (`--probe-helper`), once
//! per timezone the gate was run under, with `TZ` set explicitly and the rest of the environment
//! cleared: the result does not depend on the machine's timezone, locale or clock.
//! `T3_GOLDEN_DIR` points the test at another export.
#[path = "golden_support/mod.rs"]
mod support;
#[path = "golden_text/generators.rs"]
mod generators;
#[path = "golden_text/tables.rs"]
mod tables;

use serde_json::Value;
use std::{
    collections::HashMap,
    io::Write,
    process::{Command, Stdio},
};

const BINARY: &str = env!("CARGO_BIN_EXE_loupe-core");
const BLOCK: usize = 64;
const HEAD: usize = 64;

// ── JSON with correctly rounded numbers ─────────────────────────────────────

/// serde_json's default number parser can be a unit in the last place off on long decimals, and
/// the oracle's digests are over exact doubles, so numbers here go through Rust's `f64::from_str`.
fn parse(text: &str) -> Result<Value, String> {
    let mut parser = Parser { bytes: text.as_bytes(), at: 0 };
    let value = parser.value()?;
    parser.space();
    if parser.at != parser.bytes.len() {
        return Err(format!("trailing text at byte {}", parser.at));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\n' | b'\t' | b'\r')) {
            self.at += 1;
        }
    }
    fn literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(format!("bad literal at byte {}", self.at))
        }
    }
    fn value(&mut self) -> Result<Value, String> {
        self.space();
        match self.bytes.get(self.at).copied() {
            Some(b'n') => self.literal("null", Value::Null),
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                loop {
                    self.space();
                    if self.bytes.get(self.at) == Some(&b']') {
                        self.at += 1;
                        return Ok(Value::Array(items));
                    }
                    if !items.is_empty() {
                        if self.bytes.get(self.at) != Some(&b',') {
                            return Err(format!("expected , at byte {}", self.at));
                        }
                        self.at += 1;
                    }
                    items.push(self.value()?);
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut object = serde_json::Map::new();
                loop {
                    self.space();
                    if self.bytes.get(self.at) == Some(&b'}') {
                        self.at += 1;
                        return Ok(Value::Object(object));
                    }
                    if !object.is_empty() {
                        if self.bytes.get(self.at) != Some(&b',') {
                            return Err(format!("expected , at byte {}", self.at));
                        }
                        self.at += 1;
                        self.space();
                    }
                    let key = self.string()?;
                    self.space();
                    if self.bytes.get(self.at) != Some(&b':') {
                        return Err(format!("expected : at byte {}", self.at));
                    }
                    self.at += 1;
                    object.insert(key, self.value()?);
                }
            }
            Some(_) => {
                let start = self.at;
                while matches!(self.bytes.get(self.at), Some(b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')) {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.bytes[start..self.at]).unwrap();
                let number: f64 = text.parse().map_err(|_| format!("bad number {text:?} at byte {start}"))?;
                serde_json::Number::from_f64(number).map(Value::Number).ok_or_else(|| format!("non-finite number {text:?}"))
            }
            None => Err("unexpected end".into()),
        }
    }
    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self.bytes.get(self.at..self.at + 4).ok_or("short \\u escape")?;
        self.at += 4;
        u32::from_str_radix(std::str::from_utf8(digits).map_err(|e| e.to_string())?, 16).map_err(|e| e.to_string())
    }
    fn string(&mut self) -> Result<String, String> {
        if self.bytes.get(self.at) != Some(&b'"') {
            return Err(format!("expected a string at byte {}", self.at));
        }
        self.at += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = *self.bytes.get(self.at).ok_or("unterminated string")?;
            self.at += 1;
            match byte {
                b'"' => return String::from_utf8(out).map_err(|e| e.to_string()),
                b'\\' => {
                    let escape = *self.bytes.get(self.at).ok_or("unterminated escape")?;
                    self.at += 1;
                    let ch = match escape {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut unit = self.hex4()?;
                            if (0xd800..0xdc00).contains(&unit) && self.bytes[self.at..].starts_with(b"\\u") {
                                self.at += 2;
                                let low = self.hex4()?;
                                unit = 0x10000 + ((unit - 0xd800) << 10) + (low.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(unit).unwrap_or('\u{fffd}')
                        }
                        other => return Err(format!("bad escape \\{}", other as char)),
                    };
                    let mut buffer = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
                }
                other => out.push(other),
            }
        }
    }
}

// ── the recording ────────────────────────────────────────────────────────────

struct Group<'a> {
    seq: usize,
    name: &'a str,
    occurrence: usize,
    n: usize,
    inputs: &'a str,
    head: Vec<&'a str>,
    blocks: Vec<&'a str>,
    zones: HashMap<&'a str, (Vec<&'a str>, Vec<&'a str>)>,
}

fn strings(value: &Value) -> Vec<&str> {
    value.as_array().expect("array").iter().map(|item| item.as_str().expect("string")).collect()
}

fn groups_of(fixture: &Value) -> Vec<Group<'_>> {
    let cases = fixture["cases"].as_array().expect("cases");
    let mut groups: Vec<Group> = Vec::new();
    for case in cases.iter().filter(|case| case["kind"] == "group") {
        let mut zones = HashMap::new();
        if let Some(entries) = case["zones"].as_object() {
            for (zone, entry) in entries {
                zones.insert(zone.as_str(), (strings(&entry["head"]), strings(&entry["blocks"])));
            }
        }
        groups.push(Group {
            seq: case["seq"].as_u64().unwrap() as usize,
            name: case["name"].as_str().unwrap(),
            occurrence: case["occurrence"].as_u64().unwrap() as usize,
            n: case["n"].as_u64().unwrap() as usize,
            inputs: case["inputs"].as_str().unwrap(),
            head: strings(&case["head"]),
            blocks: strings(&case["blocks"]),
            zones,
        });
    }
    groups
}

/// A machine whose timezone database lacks a zone would silently read it as UTC and report the
/// clock groups as differing; say so instead.
fn require_zone(zone: &str, offset_at_epoch: &str) {
    let Ok(output) = Command::new("date").args(["-d", "@0", "+%z"]).env_clear().env("TZ", zone).output() else { return };
    if !output.status.success() {
        return;
    }
    let seen = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(seen, offset_at_epoch, "this machine's timezone database does not know {zone} (install tzdata): `TZ={zone} date -d @0 +%z` printed {seen}");
}

/// Runs the probe the way the live gate does and returns its output lines.
fn probe(args: &[&str], zone: &str, home: &std::path::Path, payload: &str) -> Vec<String> {
    let mut child = Command::new(BINARY)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("TZ", zone)
        .env("HOME", home)
        .env("T3CODE_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("run {BINARY} {args:?}: {e}"));
    let mut stdin = child.stdin.take().unwrap();
    let input = payload.to_string();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    let output = child.wait_with_output().unwrap();
    writer.join().unwrap();
    assert!(output.status.success(), "{args:?} exited {:?}: {}", output.status.code(), String::from_utf8_lossy(&output.stderr));
    let text = String::from_utf8(output.stdout).expect("probe output is UTF-8");
    if text.is_empty() { Vec::new() } else { text.strip_suffix('\n').unwrap_or(&text).split('\n').map(str::to_string).collect() }
}

fn digest_of(line: &str) -> Result<String, String> {
    Ok(support::canonical_digest(&parse(line)?))
}

fn label(group: &Group) -> String {
    format!("group {} `{}`#{}", group.seq, group.name, group.occurrence)
}

fn brief(text: &str) -> String {
    let mut out: String = text.chars().take(120).collect();
    if out.len() < text.len() {
        out.push('…');
    }
    out
}

/// Compares one group's output lines with the recording and says what differs.
fn check_outputs(group: &Group, lines: &[String], payload_lines: &[String], head: &[&str], blocks: &[&str], failures: &mut Vec<String>) {
    if lines.len() != group.n {
        failures.push(format!("{}: {} outputs for {} inputs", label(group), lines.len(), group.n));
        return;
    }
    let mut digests: Vec<String> = Vec::with_capacity(lines.len());
    for (index, text) in lines.iter().enumerate() {
        match digest_of(text) {
            Ok(digest) => digests.push(digest),
            Err(error) => {
                failures.push(format!("{}: output {index} is not JSON ({error}) for input {}: {}", label(group), brief(&payload_lines[index]), brief(text)));
                return;
            }
        }
    }
    // The first 64 are recorded verbatim, so the exact input can be named.
    for (index, recorded) in head.iter().enumerate().take(HEAD.min(group.n)) {
        let want = parse(recorded).expect("recorded head value");
        let got = parse(&lines[index]).unwrap();
        if let Some(difference) = support::first_difference(&want, &got, "") {
            failures.push(format!(
                "{}: input {index} {} gives {} in Rust but {} was recorded ({})",
                label(group),
                brief(&payload_lines[index]),
                brief(&lines[index]),
                brief(recorded),
                difference.trim_start_matches(':').trim()
            ));
            return;
        }
    }
    // After that only a digest per block is recorded, which names the inputs but not the one.
    let mut differing = Vec::new();
    for (block, recorded) in blocks.iter().enumerate() {
        let start = HEAD + block * BLOCK;
        let end = (start + BLOCK).min(group.n);
        if support::fnv1a(&digests[start..end].join(",")) != *recorded {
            differing.push((start, end));
        }
    }
    if let Some(&(start, end)) = differing.first() {
        let shown: Vec<String> = (start..end.min(start + 3)).map(|i| format!("{} -> {}", brief(&payload_lines[i]), brief(&lines[i]))).collect();
        failures.push(format!(
            "{}: {} of {} blocks differ from the recording; the first is inputs {start}..{end}. Rust gives, for its first inputs: {}. The recording keeps only a digest there; find the exact input with `checks/scripts/text-parity.ts` (it prints the first five differences per group) or regenerate with `checks/scripts/export-text.ts`",
            label(group),
            differing.len(),
            blocks.len(),
            shown.join("; ")
        ));
    }
}

/// Replays the gate. `zone` is the timezone to run the probes under; `None` only checks that the
/// ported generators rebuild every recorded input.
fn replay(zone: Option<&str>) {
    let fixture = support::load("text.json.gz");
    let recorded_zones = strings(&fixture["zones"]);
    assert_eq!(recorded_zones, ["UTC", "Asia/Kolkata", "America/Los_Angeles"], "zones the fixture was recorded under");
    if let Some(zone) = zone {
        require_zone(zone, match zone { "UTC" => "+0000", "Asia/Kolkata" => "+0530", _ => "-0800" });
    }
    let groups = groups_of(&fixture);
    let cases = fixture["cases"].as_array().unwrap();
    let kept: HashMap<&str, Option<Vec<bool>>> = cases
        .iter()
        .filter(|case| case["kind"] == "kept")
        .map(|case| (case["label"].as_str().unwrap(), case["flags"].as_str().map(|flags| flags.chars().map(|c| c == '1').collect())))
        .collect();
    let lookup = |label: &str| kept.get(label).unwrap_or_else(|| panic!("the recording has no `{label}` filter")).clone();

    let dir = support::scratch("text");
    let home = support::empty_home(&dir);
    let mut failures: Vec<String> = Vec::new();
    let mut checked_inputs = 0usize;
    let mut probed_groups = 0usize;
    let utc = zone == Some("UTC");

    if zone.is_none() || utc {
        // `clean`: every code point through `--probe-hidden`; the oracle's answer is a list of ranges.
        let hidden = cases.iter().find(|case| case["kind"] == "hidden").expect("hidden case");
        let count = hidden["count"].as_u64().unwrap() as u32;
        let payload: String = (0..count).map(|cp| format!("{cp}\n")).collect();
        if support::fnv1a(&payload) != hidden["inputs"].as_str().unwrap() {
            failures.push("group `clean`: the generator for the code point list differs from the recording".into());
        } else if zone.is_some() {
            let lines = probe(&["--probe-hidden"], "UTC", &home, &payload);
            let ranges: Vec<(u32, u32)> = hidden["ranges"].as_array().unwrap().iter().map(|r| (r[0].as_u64().unwrap() as u32, r[1].as_u64().unwrap() as u32)).collect();
            let want = |cp: u32| ranges.iter().any(|&(a, b)| (a..=b).contains(&cp));
            if lines.len() != count as usize {
                failures.push(format!("group `clean`: {} outputs for {count} inputs", lines.len()));
            } else {
                let wrong: Vec<u32> = (0..count).filter(|&cp| (lines[cp as usize] == "1") != want(cp)).collect();
                if let Some(first) = wrong.first() {
                    failures.push(format!(
                        "group `clean`: {} of {count} code points differ; the first is U+{first:04X}: recorded {}, Rust {}",
                        wrong.len(),
                        if want(*first) { "hidden" } else { "shown" },
                        if lines[*first as usize] == "1" { "hidden" } else { "shown" }
                    ));
                }
            }
            checked_inputs += count as usize;
            probed_groups += 1;
        }
    }

    let mut next = 0usize;
    let mut work = |name: &str, lines: Vec<String>| {
        let Some(group) = groups.get(next) else {
            failures.push(format!("the generators built a group #{next} `{name}` the recording does not have"));
            next += 1;
            return;
        };
        next += 1;
        if group.name != name {
            failures.push(format!("{}: the generators built a `{name}` group in its place (the order or the set of groups differs)", label(group)));
            return;
        }
        let payload = lines.join("\n") + "\n";
        if lines.len() != group.n || support::fnv1a(&payload) != group.inputs {
            failures.push(format!(
                "{}: the Rust port of the generators built different inputs ({} lines, digest {}; recorded {} lines, digest {}). The helper was not run. Fix `golden_text/generators.rs`; first input built: {}",
                label(group), lines.len(), support::fnv1a(&payload), group.n, group.inputs, brief(lines.first().map_or("", String::as_str))
            ));
            return;
        }
        let Some(zone) = zone else { return };
        // The groups that do not depend on the timezone are recorded once, under UTC, and the
        // gate's own zones agreed on them when the fixture was made.
        let (head, blocks) = match group.zones.get(zone) {
            Some((head, blocks)) => (head.clone(), blocks.clone()),
            None if utc => (group.head.clone(), group.blocks.clone()),
            None => return,
        };
        let output = probe(&["--probe-helper", name], zone, &home, &payload);
        checked_inputs += lines.len();
        probed_groups += 1;
        check_outputs(group, &output, &lines, &head, &blocks, &mut failures);
    };
    generators::groups(&lookup, &mut work);
    let built = next;
    if built != groups.len() {
        failures.push(format!("the generators built {built} groups; the recording has {}", groups.len()));
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} problem(s) in {} groups (recorded from {}):\n{}",
        failures.len(),
        built,
        support::recorded_from(&fixture),
        failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n")
    );
    println!("{built} groups rebuilt from the ported generators; {probed_groups} probed under {} ({checked_inputs} inputs) match the recording", zone.unwrap_or("no zone"));
}

#[test]
fn ported_generators_rebuild_every_recorded_input() {
    replay(None);
}

#[test]
fn helpers_match_the_recording_in_utc() {
    replay(Some("UTC"));
}

#[test]
fn helpers_match_the_recording_in_asia_kolkata() {
    replay(Some("Asia/Kolkata"));
}

#[test]
fn helpers_match_the_recording_in_america_los_angeles() {
    replay(Some("America/Los_Angeles"));
}

#[test]
fn json_parser_rounds_like_javascript() {
    // serde_json's default parser gets some of these wrong; Rust's f64 parser does not.
    for text in ["0.1", "5e-324", "1.7976931348623157e308", "123456789012345680000", "4.35", "2.2250738585072014e-308", "9007199254740993"] {
        let ours = parse(text).unwrap().as_f64().unwrap();
        assert_eq!(ours, text.parse::<f64>().unwrap(), "{text}");
    }
    assert_eq!(parse(r#"{"a":[1,"é🍞",null,true,-0]}"#).unwrap()["a"][1], "é🍞");
    assert_eq!(generators::js_number(1e21), "1e+21");
    assert_eq!(generators::js_number(123456789012345680000.0), "123456789012345680000");
    assert_eq!(generators::js_number(1e-7), "1e-7");
    assert_eq!(generators::js_number(0.000001), "0.000001");
    assert_eq!(generators::js_number(-60000.5), "-60000.5");
    assert_eq!(generators::js_number(1.2345e25), "1.2345e+25");
    // An exact tie between two shortest digit strings: V8 prints the even one (Rust's `{:e}` the odd).
    assert_eq!(generators::js_number(0.0096607208251953125), "0.009660720825195312");
}
