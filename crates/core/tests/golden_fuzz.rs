//! Action sequences against output recorded from the TypeScript original (main `2f1d553`): the 945
//! prefixes of `checks/scripts/semantic-fuzz.ts`. Each of the 45 seeded cases starts from a
//! generated snapshot and runs 21 prefixes of its 20 generated actions; every prefix compares the
//! selected row, the scope, the fold and expand sets and the complete row list.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

#[test]
fn action_sequences_match_frozen_oracle() {
    let fixture = support::load("fuzz.json.gz");
    let cases = fixture["cases"].as_array().expect("cases");
    let dir = support::scratch("fuzz");
    let home = support::empty_home(&dir);
    let mut failures: Vec<String> = Vec::new();
    let mut prefixes = 0;
    for case in cases {
        let index = case["case"].as_u64().unwrap();
        let token = case["input"].as_str().unwrap().trim_start_matches('@');
        let input = dir.join(format!("case-{index}.json"));
        std::fs::write(&input, fixture["assets"][token]["text"].as_str().unwrap()).unwrap();
        let input = input.to_str().unwrap().to_string();
        let actions: Vec<String> = case["actions"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
        let frames = case["frames"].as_array().unwrap();
        assert_eq!(frames.len(), actions.len() + 1, "case {index}: one frame per prefix");
        for (step, expected) in frames.iter().enumerate() {
            prefixes += 1;
            let prefix = actions[..step].join(",");
            let output = support::run(BINARY, &["--snapshot", &input, "--semantic", "--actions", &prefix], &home, &[]);
            assert_eq!(output.status.code(), Some(0), "case {index} prefix {step}: {}", String::from_utf8_lossy(&output.stderr));
            let actual: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("case {index} prefix {step}: {e}"));
            for (field, want) in expected.as_object().unwrap() {
                if &actual[field] != want {
                    failures.push(format!(
                        "case {index} prefix {step} ({}): {field}\n  recorded: {}\n  rust:     {}",
                        if step == 0 { "initial".to_string() } else { actions[step - 1].clone() },
                        brief(want),
                        brief(&actual[field])
                    ));
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} of {prefixes} action prefixes differ (recorded from {}):\n{}",
        failures.len(),
        support::recorded_from(&fixture),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
    assert_eq!(prefixes, 945, "the workload holds 945 prefixes");
    println!("{prefixes} action prefixes match the recording");
}

fn brief(value: &Value) -> String {
    let text = value.to_string();
    if text.len() > 400 { format!("{}…", &text[..400]) } else { text }
}
