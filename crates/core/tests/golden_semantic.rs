//! Semantic state against output recorded from the TypeScript original (main `2f1d553`): the 124
//! frames of `checks/scripts/parity.ts`. Each frame is one snapshot after a prefix of the 19
//! standard actions; the test runs `--semantic` for every prefix and compares the selected row,
//! the scope, the sorted fold and expand sets, and the whole row list with every row's fields.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

#[test]
fn semantic_state_matches_frozen_oracle() {
    let fixture = support::load("semantic.json.gz");
    let cases = fixture["cases"].as_array().expect("cases");
    let dir = support::scratch("semantic");
    let home = support::empty_home(&dir);
    let mut failures: Vec<String> = Vec::new();
    let mut frames_checked = 0;
    for case in cases {
        let name = case["case"].as_str().unwrap();
        let token = case["input"].as_str().unwrap().trim_start_matches('@');
        let asset = &fixture["assets"][token];
        assert_eq!(asset["kind"], "text", "{name}: input is a snapshot file");
        let input = dir.join(format!("{token}.json"));
        std::fs::write(&input, asset["text"].as_str().unwrap()).unwrap();
        let input = input.to_str().unwrap().to_string();
        let actions: Vec<String> = case["actions"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
        let frames = case["frames"].as_array().unwrap();
        assert_eq!(frames.len(), actions.len() + 1, "{name}: one frame per action prefix");
        for (index, expected) in frames.iter().enumerate() {
            frames_checked += 1;
            let prefix = actions[..index].join(",");
            let output = support::run(BINARY, &["--snapshot", &input, "--semantic", "--actions", &prefix], &home, &[]);
            assert_eq!(output.status.code(), Some(0), "{name} step {index}: {}", String::from_utf8_lossy(&output.stderr));
            let actual: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("{name} step {index}: {e}"));
            let label = if index == 0 { "initial".to_string() } else { actions[index - 1].clone() };
            for (field, want) in expected.as_object().unwrap() {
                let got = &actual[field];
                if got != want {
                    failures.push(format!(
                        "{name} step {index} ({label}): {field}\n  recorded: {}\n  rust:     {}",
                        brief(want),
                        brief(got)
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} of {frames_checked} semantic frames differ (recorded from {}):\n{}",
        failures.len(),
        support::recorded_from(&fixture),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
    assert_eq!(frames_checked, 124, "the workload holds 124 frames");
    println!("{frames_checked} semantic frames match the recording");
}

fn brief(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 500 { format!("{}…", &text[..500]) } else { text }
}
