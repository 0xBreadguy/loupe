//! Demo data against output recorded from the TypeScript original (main `2f1d553`): the 18 clock
//! phases of `checks/scripts/demo-parity.mjs`. For each phase the whole snapshot is compared
//! after the gate's normalization: null object members are absent, the root path is ignored, every
//! value, array order, timestamp, event, plan, history, request and item field stays exact.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

fn brief(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 200 { format!("{}…", &text[..200]) } else { text }
}

#[test]
fn demo_data_matches_frozen_oracle() {
    let fixture = support::load("demo.json.gz");
    let cases = fixture["cases"].as_array().expect("cases");
    let dir = support::scratch("demo");
    let home = support::empty_home(&dir);
    let mut failures = Vec::new();
    for case in cases {
        let offset = case["offset"].as_i64().unwrap();
        let now = case["now"].as_f64().unwrap();
        let epoch = case["epoch"].as_f64().unwrap();
        let output = support::run(BINARY, &["--demo-snapshot", &format!("{now}"), "--demo-epoch", &format!("{epoch}")], &home, &[]);
        assert_eq!(output.status.code(), Some(0), "{offset}ms: {}", String::from_utf8_lossy(&output.stderr));
        let actual: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{offset}ms: {e}"));
        let actual = support::normalize(&actual, true);
        let expected = &case["expected"];
        if let Some(difference) = support::first_difference(expected, &actual, "") {
            let agents = actual["agents"].as_array().map(Vec::len).unwrap_or(0);
            failures.push(format!("{offset}ms ({agents} agents): {difference}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} of {} demo phases differ (recorded from {}):\n{}",
        failures.len(),
        cases.len(),
        support::recorded_from(&fixture),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
    assert_eq!(cases.len(), 18, "the workload holds 18 phases");
    println!("{} demo clock phases match the recording", cases.len());
}
