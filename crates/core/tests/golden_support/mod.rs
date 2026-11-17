//! Shared plumbing for the recorded-fixture tests (`golden_*.rs`).
//!
//! A fixture is one gzip-compressed JSON object written by an `checks/scripts/export-*.ts`
//! exporter from the TypeScript original. It holds the oracle's expected output for each case,
//! the inputs the oracle built itself (synthetic SQLite databases as SQL text), and the
//! `oracle/` revision it was recorded from. These helpers keep every test on the same rules:
//! no Node, no network, no real T3 home, a fixed timezone, and scratch files on the target
//! volume rather than `/tmp`.
#![allow(dead_code)]
use flate2::read::GzDecoder;
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// `T3_GOLDEN_DIR` points every test at another export, as `golden.rs` already allows.
pub fn golden_dir() -> PathBuf {
    std::env::var_os("T3_GOLDEN_DIR").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../checks/fixtures/golden")
    })
}

pub fn load(name: &str) -> Value {
    let path = golden_dir().join(name);
    let mut data = String::new();
    GzDecoder::new(std::fs::File::open(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .read_to_string(&mut data)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let value: Value = serde_json::from_str(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(value["version"], 1, "{name}: fixture version");
    assert!(value["dashboard"]["commit"].as_str().is_some_and(|c| c.len() == 40), "{name}: records the dashboard revision");
    value
}

/// The recorded oracle revision, printed by every test so a failure says what it was recorded from.
pub fn recorded_from(fixture: &Value) -> String {
    let dashboard = &fixture["dashboard"];
    format!(
        "dashboard tree {} at {} (src {})",
        dashboard["tree"].as_str().unwrap_or("?"),
        &dashboard["commit"].as_str().unwrap_or("?")[..12.min(dashboard["commit"].as_str().unwrap_or("?").len())],
        &dashboard["src"].as_str().unwrap_or("?")[..12.min(dashboard["src"].as_str().unwrap_or("?").len())],
    )
}

/// An empty, writable directory for one test, on the target volume (`/tmp` is small and some
/// tests bind Unix sockets, whose path length matters).
pub fn scratch(name: &str) -> PathBuf {
    // Keep scratch input next to the build, not in `/tmp` (small quota, long paths for sockets).
    let base = std::env::var_os("CARGO_TARGET_TMPDIR").map(PathBuf::from).unwrap_or_else(|| {
        let exe = std::env::current_exe().expect("test binary path");
        exe.parent().and_then(|p| p.parent()).map(|target| target.join("tmp")).unwrap_or_else(std::env::temp_dir)
    });
    let base = base;
    let _ = std::fs::create_dir_all(&base);
    let dir = base.join(format!("golden-{name}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// An empty T3 home, so nothing the binary does can discover a real installation.
pub fn empty_home(dir: &Path) -> PathBuf {
    let home = dir.join("empty-t3-home");
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// Runs a binary with a fixed environment: `TZ=UTC`, an empty `T3CODE_HOME`, and nothing else
/// from the machine. The binary reads its palette from `BACKGROUND`-style variables, so inheriting
/// the caller's environment would make a recording depend on the desktop it was replayed on.
pub fn run(binary: &str, args: &[&str], home: &Path, env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(binary);
    command
        .args(args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("TZ", "UTC")
        .env("HOME", home)
        .env("T3CODE_HOME", home);
    for (key, value) in env {
        command.env(key, value);
    }
    command.output().unwrap_or_else(|e| panic!("run {binary} {args:?}: {e}"))
}

pub fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Rebuilds a synthetic database from the SQL text an exporter dumped for it.
pub fn rebuild_database(path: &Path, sql: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let db = rusqlite::Connection::open(path).unwrap();
    db.execute_batch(sql).unwrap_or_else(|e| panic!("rebuild {}: {e}", path.display()));
    db.close().unwrap();
}

/// The gate's `normalized(value, root)`: drop `null` object members (Rust spells an absent
/// optional field `null`, JavaScript omits it) and, at the root, the snapshot's `path`. Arrays
/// keep their order and their nulls.
pub fn normalize(value: &Value, root: bool) -> Value {
    match value {
        Value::Object(object) => {
            let mut out = serde_json::Map::new();
            for (key, member) in object {
                if member.is_null() || (root && key == "path") {
                    continue;
                }
                out.insert(key.clone(), normalize(member, false));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(|item| normalize(item, false)).collect()),
        other => other.clone(),
    }
}

fn numbers_close(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

/// The first place two JSON values differ, as a readable path. Numbers compare by value, so `0`
/// equals `0.0`, as under the gate's `assert.deepEqual`.
pub fn first_difference(expected: &Value, actual: &Value, path: &str) -> Option<String> {
    match (expected, actual) {
        (Value::Number(a), Value::Number(b)) => {
            if numbers_close(a.as_f64().unwrap_or(f64::NAN), b.as_f64().unwrap_or(f64::NAN)) { None }
            else { Some(format!("{path}: recorded {a}, rust {b}")) }
        }
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                match b.get(key) {
                    Some(other) => {
                        if let Some(difference) = first_difference(value, other, &format!("{path}.{key}")) {
                            return Some(difference);
                        }
                    }
                    None => return Some(format!("{}.{key}: recorded {}, absent in rust", path, brief(value))),
                }
            }
            for key in b.keys() {
                if !a.contains_key(key) {
                    return Some(format!("{}.{key}: recorded absent, rust {}", path, brief(&b[key])));
                }
            }
            None
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: recorded {} items, rust {}", a.len(), b.len()));
            }
            for (index, (value, other)) in a.iter().zip(b).enumerate() {
                if let Some(difference) = first_difference(value, other, &format!("{path}[{index}]")) {
                    return Some(difference);
                }
            }
            None
        }
        _ => {
            if expected == actual { None } else { Some(format!("{path}: recorded {}, rust {}", brief(expected), brief(actual))) }
        }
    }
}

fn brief(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 200 { format!("{}…", &text[..200]) } else { text }
}

/// FNV-1a (64-bit) over UTF-8, in the same convention the exporters use. A digest only has to
/// notice drift between a recording and a replay, so a fast, dependency-free hash is enough and
/// keeps the crate graph small.
pub fn fnv1a(text: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// The digest of a whole JSON value, independent of object key order; the same byte stream as
/// `canonicalDigest` in `checks/scripts/golden.ts` (see that function for the grammar).
pub fn canonical_digest(value: &Value) -> String {
    let mut bytes: Vec<u8> = Vec::new();
    fn emit(value: &Value, bytes: &mut Vec<u8>) {
        match value {
            Value::Null => bytes.push(b'n'),
            Value::Bool(true) => bytes.push(b'T'),
            Value::Bool(false) => bytes.push(b'F'),
            Value::String(text) => {
                bytes.push(b's');
                bytes.extend_from_slice(text.as_bytes());
            }
            Value::Number(number) => {
                let whole = number.as_f64().filter(|x| x.fract() == 0.0 && x.abs() <= 9007199254740992.0);
                match whole {
                    Some(x) => {
                        bytes.push(b'i');
                        bytes.extend_from_slice(format!("{}", x as i64).as_bytes());
                    }
                    None => {
                        bytes.push(b'f');
                        bytes.extend_from_slice(&number.as_f64().unwrap_or(f64::NAN).to_bits().to_be_bytes());
                    }
                }
            }
            Value::Array(items) => {
                bytes.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        bytes.push(b',');
                    }
                    emit(item, bytes);
                }
                bytes.push(b']');
            }
            Value::Object(object) => {
                bytes.push(b'{');
                let mut keys: Vec<&String> = object.keys().collect();
                keys.sort();
                for key in keys {
                    bytes.push(b'k');
                    bytes.extend_from_slice(key.as_bytes());
                    bytes.push(b'=');
                    emit(&object[key], bytes);
                    bytes.push(b';');
                }
                bytes.push(b'}');
            }
        }
    }
    emit(value, &mut bytes);
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// The recorded digest of a number stream: the numbers in order, comma-joined, as the
/// exporters write them (`cells.join(",")`).
pub fn numbers_digest(numbers: &[impl std::fmt::Display]) -> String {
    let text: Vec<String> = numbers.iter().map(|n| n.to_string()).collect();
    fnv1a(&text.join(","))
}
