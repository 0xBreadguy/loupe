//! Cross-machine data against output recorded from the TypeScript original (main `2f1d553`): the
//! single case of `checks/scripts/cross-machine-parity.ts`. A synthetic database with remote
//! parents, a retry resolved through an earlier provider turn and a delegation cycle is rebuilt
//! from the fixture, read with `--db`, and compared three ways: the whole snapshot, the 78
//! full-grid states over it, and the focused state of every agent. The reader must also leave the
//! database byte-identical and must never return the payloads marked `SECRET`.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

fn digest_json(value: &Value) -> String {
    let numbers: Vec<u64> = value.as_array().unwrap().iter().map(|n| n.as_u64().unwrap_or(0)).collect();
    support::numbers_digest(&numbers)
}

#[test]
fn cross_machine_data_matches_frozen_oracle() {
    let fixture = support::load("cross.json.gz");
    let case = &fixture["cases"][0];
    let dir = support::scratch("cross");
    let home = support::empty_home(&dir);
    // The database and the machine id beside it come back exactly where the gate had them.
    let tree = case["input"].as_str().unwrap().trim_start_matches('@');
    let input = dir.join("input");
    std::fs::create_dir_all(&input).unwrap();
    for (name, file) in fixture["assets"][tree]["files"].as_object().unwrap() {
        let token = file.as_str().unwrap().trim_start_matches('@');
        let target = input.join(name);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        match fixture["assets"][token]["kind"].as_str().unwrap() {
            "sql" => support::rebuild_database(&target, fixture["assets"][token]["sql"].as_str().unwrap()),
            _ => {
                std::fs::write(&target, fixture["assets"][token]["text"].as_str().unwrap()).unwrap();
            }
        }
    }
    let database = input.join("synthetic.sqlite");
    let before = std::fs::read(&database).unwrap();
    let now = case["now"].as_f64().unwrap();

    let output = support::run(BINARY, &["--db", database.to_str().unwrap(), "--now-ms", &format!("{now}"), "--dump-snapshot"], &home, &[]);
    assert_eq!(output.status.code(), Some(0), "reader: {}", String::from_utf8_lossy(&output.stderr));
    let text = support::stdout_text(&output);
    assert!(!text.contains("SECRET"), "the reader must not return payloads outside its allowlist");
    let actual: Value = serde_json::from_slice(&output.stdout).unwrap();
    let actual = support::normalize(&actual, true);
    assert!(
        support::first_difference(&case["snapshot"], &actual, "").is_none(),
        "snapshot differs (recorded from {}): {}",
        support::recorded_from(&fixture),
        support::first_difference(&case["snapshot"], &actual, "").unwrap_or_default()
    );
    assert_eq!(std::fs::read(&database).unwrap(), before, "the reader must leave the database byte-identical");

    // Rendering: the same snapshot file the gate handed to `--snapshot`, at every grid and action
    // set, compared by plane digest.
    let render = dir.join("snapshot.json");
    std::fs::write(&render, case["render"].to_string()).unwrap();
    let render = render.to_str().unwrap().to_string();
    let mut states = 0;
    let mut failures = Vec::new();
    let grid_states = case["grid"].as_array().unwrap();
    for state in grid_states {
        states += 1;
        let grid = state["grid"].as_str().unwrap();
        let actions = state["actions"].as_str().unwrap();
        let output = support::run(BINARY, &["--snapshot", &render, "--grid", grid, "--actions", actions, "--frame-json"], &home, &[]);
        assert_eq!(output.status.code(), Some(0), "{grid} {actions:?}: {}", String::from_utf8_lossy(&output.stderr));
        let frame: Value = serde_json::from_slice(&output.stdout).unwrap();
        for plane in ["ch", "fg", "bg", "at"] {
            let actual = digest_json(&frame[plane]);
            if actual != state[plane].as_str().unwrap() {
                failures.push(format!("grid {grid} actions {actions:?}: {plane} plane {actual} instead of {}", state[plane]));
            }
        }
    }
    let focused = case["focused"].as_array().unwrap();
    for state in focused {
        states += 1;
        let id = state["id"].as_str().unwrap();
        let output = support::run(BINARY, &["--snapshot", &render, "--grid", "160x44", "--actions", &format!("pick:{id}"), "--frame-json"], &home, &[]);
        assert_eq!(output.status.code(), Some(0), "{id}: {}", String::from_utf8_lossy(&output.stderr));
        let frame: Value = serde_json::from_slice(&output.stdout).unwrap();
        for plane in ["ch", "fg", "bg", "at"] {
            let actual = digest_json(&frame[plane]);
            if actual != state[plane].as_str().unwrap() {
                failures.push(format!("agent {id}: {plane} plane {actual} instead of {}", state[plane]));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{} of {states} grid states differ:\n{}", failures.len(), failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n"));
    assert_eq!(grid_states.len(), 78, "six grids by thirteen action sets");
    assert_eq!(focused.len(), case["snapshot"]["agents"].as_array().unwrap().len(), "one focused state per agent");
    println!("cross-machine snapshot, {} grid states and {} focused states match the recording", grid_states.len(), focused.len());
}
