//! Hold the product's asks count to the recorded corpus.
//!
//! One message in, one number out: the questions put to the person plus the
//! statements that ask the same thing without a question mark. The reference is the
//! `asks/questions.py` prototype from the Insights thread (2026-11-18), which returns
//! `(questions, statements)`; its count is their sum. `checks/fixtures/asks-counts.json`
//! records both parts and the total for every made-up case, so this check needs neither
//! the prototype nor Python. `checks/scripts/asks-corpus.py --check --reference PATH`
//! re-runs the reference when someone names it on purpose.
//!
//! Run it with `checks/scripts/asks-count.sh`, which builds this crate against
//! `crates/core`. It is separate from the workspace on purpose: it is the harness,
//! not the product, and it must not take the workspace's tests down while the
//! `asks` module it exercises is still being written.
//!
//! Exit codes: 0 every case matches, 1 a count differs, 2 the corpus or an argument
//! could not be used.

#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;

use serde_json::Value;

/// The product's answer for one message. This is the whole coupling: if
/// `count_asks` returns something other than one count, adapt this function and say
/// why in the check's notes. The corpus keeps `questions`, `statements` and `total`,
/// so an adapter for a pair is a one-line change here, not a corpus change.
fn product_count(text: &str) -> u32 {
    loupe_core::asks::count_asks(text)
}

fn usage() {
    println!(
        "usage: asks-count [--json] [FIXTURE]\n\
         \n\
         Compare loupe_core::asks::count_asks with checks/fixtures/asks-counts.json.\n\
         \n\
         --json     print {{\"case-id\": count, ...}} to stdout for the differential run in\n\
         \x20          checks/scripts/asks-corpus.py --actual; differences still go to\n\
         \x20          stderr, and the exit stays 0 so that run can read the counts\n\
         FIXTURE    another corpus, with the same shape (default: the tracked one)"
    );
}

fn main() -> ExitCode {
    let mut as_json = false;
    let mut fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures/asks-counts.json");
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--json" => as_json = true,
            "-h" | "--help" => {
                usage();
                return ExitCode::SUCCESS;
            }
            other if other.starts_with('-') => {
                eprintln!("asks-count: unknown option {other}");
                usage();
                return ExitCode::from(2);
            }
            other => fixture = PathBuf::from(other),
        }
    }

    let raw = match std::fs::read_to_string(&fixture) {
        Ok(raw) => raw,
        Err(error) => {
            eprintln!("asks-count: cannot read {}: {error}", fixture.display());
            return ExitCode::from(2);
        }
    };
    let doc: Value = match serde_json::from_str(&raw) {
        Ok(doc) => doc,
        Err(error) => {
            eprintln!("asks-count: {} is not JSON: {error}", fixture.display());
            return ExitCode::from(2);
        }
    };
    if doc.get("format").and_then(Value::as_u64) != Some(1) {
        eprintln!("asks-count: {} has an unknown format", fixture.display());
        return ExitCode::from(2);
    }
    let Some(cases) = doc.get("cases").and_then(Value::as_array).filter(|cases| !cases.is_empty())
    else {
        eprintln!("asks-count: {} has no cases", fixture.display());
        return ExitCode::from(2);
    };

    let mut counts = serde_json::Map::new();
    let mut mismatches = 0usize;
    for case in cases {
        let id = case.get("id").and_then(Value::as_str).unwrap_or("(no id)");
        let Some(text) = case.get("text").and_then(Value::as_str) else {
            eprintln!("asks-count: {id}: no text");
            return ExitCode::from(2);
        };
        let Some(expected) = case
            .get("expected")
            .and_then(|expected| expected.get("total"))
            .and_then(Value::as_u64)
        else {
            eprintln!("asks-count: {id}: no expected total");
            return ExitCode::from(2);
        };
        let got = product_count(text);
        counts.insert(id.to_owned(), Value::from(got));
        if u64::from(got) != expected {
            mismatches += 1;
            let kind = case.get("kind").and_then(Value::as_str).unwrap_or("");
            eprintln!("MISMATCH {id} ({kind}): expected {expected}, got {got}");
            eprintln!("  text: {text:?}");
        }
    }

    if as_json {
        println!("{}", Value::Object(counts));
        return ExitCode::SUCCESS;
    }
    if mismatches == 0 {
        println!("asks count: {} of {} cases match the corpus", cases.len(), cases.len());
        return ExitCode::SUCCESS;
    }
    eprintln!("asks count: {mismatches} of {} cases differ from the corpus", cases.len());
    ExitCode::FAILURE
}
