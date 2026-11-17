//! Checks the core's cell grid against output saved from the TypeScript original at main
//! `2f1d553`. No Node.js is involved: the expected cells live in `checks/fixtures/golden`.
//! `T3_GOLDEN_DIR` points the test at another export.
use flate2::read::GzDecoder;
use serde_json::Value;
use std::{io::Read, path::PathBuf, process::Command};

fn cells(v: &Value, key: &str) -> Vec<u64> {
    v[key]
        .as_array()
        .unwrap_or_else(|| panic!("missing {key}"))
        .iter()
        .map(|n| n.as_u64().expect("cell field is an unsigned integer"))
        .collect()
}
fn row_text(ch: &[u64], w: usize, row: usize) -> String {
    ch.iter()
        .skip(row * w)
        .take(w)
        .map(|c| match char::from_u32(*c as u32) {
            Some('\0') | None => ' ',
            Some(c) => c,
        })
        .collect()
}

#[test]
fn grid_matches_frozen_oracle() {
    check_grids("states.jsonl.gz");
}

/// The hidden-character fixture: `clean()` turns each of these into a one-column space before
/// the screen draws it, while `fit()` still measures the raw string. Its states are recorded
/// the same way, in their own file, so `states.jsonl.gz` is never rewritten.
#[test]
fn hidden_characters_match_frozen_oracle() {
    check_grids("hidden.jsonl.gz");
}

/// The label fixture: `itemLabel`'s home-folder shortening and type text, every item type and
/// every agent status. Each state records the `HOME` it was drawn under (`null` for unset), so
/// the replay reads no machine's home folder.
#[test]
fn activity_labels_match_frozen_oracle() {
    check_grids("labels.jsonl.gz");
}

/// The home folder recorded states that do not name one were drawn under. None of their
/// strings contains it, so any fixed value reproduces the recording; an empty one would not,
/// because `itemLabel` prefixes every label when `HOME` is unset.
const FIXED_HOME: &str = "/synthetic/home";

fn check_grids(file: &str) {
    let dir = std::env::var_os("T3_GOLDEN_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../checks/fixtures/golden")
        });
    let mut data = String::new();
    GzDecoder::new(
        std::fs::File::open(dir.join(file))
            .unwrap_or_else(|e| panic!("{}: {e}", dir.join(file).display())),
    )
    .read_to_string(&mut data)
    .expect("gzip JSON lines");
    let mut states = 0;
    let mut failures = Vec::new();
    for line in data.lines() {
        let expected: Value = serde_json::from_str(line).expect("golden record");
        let input = expected["input"].as_str().unwrap();
        let grid = expected["grid"].as_str().unwrap();
        let actions = expected["actions"].as_str().unwrap();
        states += 1;
        let mut command = Command::new(env!("CARGO_BIN_EXE_t3-agents-core"));
        command.env("TZ", "UTC");
        match expected.get("home") {
            Some(Value::String(home)) => {
                command.env("HOME", home);
            }
            Some(_) => {
                command.env_remove("HOME");
            }
            None => {
                command.env("HOME", FIXED_HOME);
            }
        }
        let output = command
            .args(["--snapshot", dir.join(format!("{input}.json")).to_str().unwrap()])
            .args(["--grid", grid, "--actions", actions, "--frame-json"])
            .output()
            .expect("run t3-agents-core");
        let name = format!("fixture={input} grid={grid} actions={actions:?}");
        if !output.status.success() {
            failures.push(format!("{name}: {}", String::from_utf8_lossy(&output.stderr).trim()));
            continue;
        }
        let actual: Value = serde_json::from_slice(&output.stdout).expect("frame JSON");
        let w = expected["w"].as_u64().unwrap() as usize;
        if actual["w"] != expected["w"] || actual["h"] != expected["h"] {
            failures.push(format!("{name}: size {}x{}, expected {}x{}", actual["w"], actual["h"], expected["w"], expected["h"]));
            continue;
        }
        let (ech, efg, ebg, eat) = (cells(&expected, "ch"), cells(&expected, "fg"), cells(&expected, "bg"), cells(&expected, "at"));
        let (ach, afg, abg, aat) = (cells(&actual, "ch"), cells(&actual, "fg"), cells(&actual, "bg"), cells(&actual, "at"));
        let differ = |a: &[u64], b: &[u64]| a.len().abs_diff(b.len()) + a.iter().zip(b).filter(|(x, y)| x != y).count();
        let text = differ(&ech, &ach);
        let colors = (0..efg.len().min(afg.len()))
            .filter(|&i| efg[i] != afg[i] || ebg.get(i) != abg.get(i))
            .count();
        let attributes = differ(&eat, &aat);
        if text + colors + attributes == 0 {
            continue;
        }
        let first = (0..ech.len())
            .find(|&i| ech.get(i) != ach.get(i) || efg.get(i) != afg.get(i) || ebg.get(i) != abg.get(i) || eat.get(i) != aat.get(i))
            .unwrap_or(0);
        let row = first / w.max(1);
        failures.push(format!(
            "{name}: {text} characters, {colors} colors, {attributes} attributes differ; first at row {row}, column {}\n  expected: {}\n  actual:   {}",
            first % w.max(1),
            row_text(&ech, w, row),
            row_text(&ach, w, row)
        ));
    }
    assert!(states > 0, "no golden states in {}", dir.display());
    assert!(
        failures.is_empty(),
        "{} of {states} golden states differ:\n{}",
        failures.len(),
        failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n")
    );
    println!("{states} golden states match");
}
