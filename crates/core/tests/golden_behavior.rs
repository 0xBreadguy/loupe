//! Behaviour over time against output recorded from the TypeScript original (main `2f1d553`): the
//! 104 timed frames of `checks/scripts/behavior-parity.ts`. Every frame compares the state
//! (calm, paused, help, effect, follow, scrolls, notice, picked, arrival and shimmer clocks,
//! tweens, acknowledgements, waits, fps), the position of the CRT effect, and a digest of each of
//! the four cell planes. `firstSeen` is recorded only where it changes.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 || (a.is_nan() && b.is_nan())
}

/// The gate's `assert.deepEqual`: numbers compare by value (`0` equals `0.0`), arrays and objects
/// structurally.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => close(x.as_f64().unwrap_or(f64::NAN), y.as_f64().unwrap_or(f64::NAN)) || x.as_f64() == y.as_f64(),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(a, b)| same(a, b)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w))),
        _ => a == b,
    }
}

fn compare_tween(expected: &Value, actual: &Value) -> Option<String> {
    let (Some(want), Some(got)) = (expected.as_array(), actual.as_array()) else {
        return Some(format!("tween shape: recorded {expected}, rust {actual}"));
    };
    if want.len() != got.len() {
        return Some(format!("tween length {} recorded, {} rust", want.len(), got.len()));
    }
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        if w[0] != g[0] || !close(w[1].as_f64().unwrap_or(f64::NAN), g[1].as_f64().unwrap_or(f64::NAN)) {
            return Some(format!("tween[{i}]: recorded {w}, rust {g}"));
        }
    }
    None
}

#[test]
fn behaviour_over_time_matches_frozen_oracle() {
    let fixture = support::load("behavior.json.gz");
    let cases = fixture["cases"].as_array().expect("cases");
    let dir = support::scratch("behavior");
    let home = support::empty_home(&dir);
    let snapshot_token = cases[0]["snapshot"].as_str().unwrap().trim_start_matches('@');
    let snapshot_asset = &fixture["assets"][snapshot_token];
    assert_eq!(snapshot_asset["kind"], "text", "the demo snapshot travels as a file");
    let snapshot = dir.join("demo.json");
    std::fs::write(&snapshot, snapshot_asset["text"].as_str().unwrap()).unwrap();
    let snapshot = snapshot.to_str().unwrap().to_string();
    let mut failures: Vec<String> = Vec::new();
    let mut frames_checked = 0;
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let spec_path = dir.join(format!("{name}.json"));
        let token = case["spec"].as_str().unwrap().trim_start_matches('@');
        std::fs::write(&spec_path, fixture["assets"][token]["text"].as_str().unwrap()).unwrap();
        if let Some(path) = std::env::var_os("T3_DUMP_BEHAVIOR") {
            std::fs::copy(&spec_path, std::path::Path::new(&path).join(format!("{name}.spec.json"))).unwrap();
            std::fs::copy(&snapshot, std::path::Path::new(&path).join("demo.json")).unwrap();
        }
        let output = support::run(BINARY, &["--snapshot", &snapshot, "--behavior-script", spec_path.to_str().unwrap()], &home, &[]);
        assert_eq!(output.status.code(), Some(0), "{name}: {}", String::from_utf8_lossy(&output.stderr));
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{name}: {e}"));
        let actual = actual["frames"].as_array().unwrap();
        let expected = case["frames"].as_array().unwrap();
        assert_eq!(actual.len(), expected.len(), "{name}: frame count");
        let mut carried: Option<Value> = None;
        for (index, want) in expected.iter().enumerate() {
            frames_checked += 1;
            let got = &actual[index];
            for (field, value) in want.as_object().unwrap() {
                let problem = match field.as_str() {
                    "ch" | "fg" | "bg" | "attr" => {
                        let plane: Vec<u64> = got[field].as_array().unwrap().iter().map(|n| n.as_u64().unwrap_or(0)).collect();
                        let digest = support::numbers_digest(&plane);
                        if &Value::String(digest) == value { None } else {
                            Some(format!("{field} plane: recorded digest {}, rust {}", value.as_str().unwrap_or("?"), support::numbers_digest(&plane)))
                        }
                    }
                    "fxPosition" => {
                        let a = value.as_f64().unwrap_or(f64::NAN);
                        let b = got[field].as_f64().unwrap_or(f64::NAN);
                        if close(a, b) { None } else { Some(format!("{field}: recorded {a}, rust {b}")) }
                    }
                    "tween" => compare_tween(value, &got[field]),
                    _ => {
                        if same(value, &got[field]) { None } else {
                            Some(format!("{field}\n    recorded: {}\n    rust:     {}", brief(value), brief(&got[field])))
                        }
                    }
                };
                if let Some(problem) = problem {
                    failures.push(format!("{name} frame {index}: {problem}"));
                }
            }
            if let Some(value) = want.get("firstSeen") {
                carried = Some(value.clone());
            }
            if let Some(carried) = &carried {
                if !same(carried, &got["firstSeen"]) {
                    failures.push(format!("{name} frame {index}: firstSeen\n    recorded: {}\n    rust:     {}", brief(carried), brief(&got["firstSeen"])));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} problems in {frames_checked} behaviour frames (recorded from {}):\n{}",
        failures.len(),
        support::recorded_from(&fixture),
        failures.iter().take(8).cloned().collect::<Vec<_>>().join("\n")
    );
    assert_eq!(frames_checked, 104, "the workload holds 104 frames");
    println!("{frames_checked} behaviour frames match the recording");
}

fn brief(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 300 { format!("{}…", &text[..300]) } else { text }
}
