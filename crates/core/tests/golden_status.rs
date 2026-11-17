//! The bar's status line against output recorded from the TypeScript original (main `2f1d553`).
//! Replays all 347 cases of `checks/scripts/status-parity.ts` with only `cargo test`:
//! fixture snapshots through `--snapshot`, four database error lines, and the demo watcher's
//! 1,206-second timer replay. `T3_GOLDEN_DIR` points the test at another export.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

fn line(output: &std::process::Output) -> String {
    let text = support::stdout_text(output);
    text.strip_suffix('\n').unwrap_or(&text).to_string()
}

#[test]
fn status_line_matches_frozen_oracle() {
    let fixture = support::load("status.json.gz");
    let cases = fixture["cases"].as_array().expect("cases");
    let dir = support::scratch("status");
    let home = support::empty_home(&dir);
    let mut failures: Vec<String> = Vec::new();
    let mut checked = 0;
    for case in cases {
        let label = case["label"].as_str().unwrap();
        let expected = case["expected"].as_str().unwrap_or_else(|| panic!("{label}: expected"));
        checked += 1;
        match case["kind"].as_str().unwrap() {
            "snapshot" => {
                let snapshot = case["snapshot"].to_string();
                let file = dir.join(format!("{checked}.json"));
                std::fs::write(&file, &snapshot).unwrap();
                let file = file.to_str().unwrap().to_string();
                let now = case["now"].as_f64().unwrap();
                let mut args: Vec<&str> = vec!["--status", "--demo", "--snapshot", &file];
                let now = format!("{now}");
                if case["explicit"].as_bool().unwrap() {
                    args.extend(["--now-ms", &now]);
                }
                let output = support::run(BINARY, &args, &home, &[]);
                assert_eq!(output.status.code(), Some(0), "{label}: exit; {}", support::stdout_text(&output));
                let actual = line(&output);
                if actual != expected {
                    failures.push(format!("{label}\n  recorded: {expected}\n  rust:     {actual}"));
                }
            }
            "database" => {
                // Rebuild the broken input this case names, then use its real path in the line.
                let base = dir.join(format!("db-{checked}"));
                std::fs::create_dir_all(&base).unwrap();
                let invalid = case["invalid"].as_str().unwrap();
                let path = match invalid {
                    "missing" => base.join("does-not-exist.sqlite"),
                    "not-sqlite" => {
                        let path = base.join("not-a-database.sqlite");
                        std::fs::write(&path, "this is not a SQLite database\n").unwrap();
                        path
                    }
                    "folder" => base.join("a-folder"),
                    "schema" => {
                        let path = base.join("wrong-schema.sqlite");
                        let db = rusqlite::Connection::open(&path).unwrap();
                        db.execute_batch("CREATE TABLE untouched(value TEXT)").unwrap();
                        db.close().unwrap();
                        path
                    }
                    other => panic!("{label}: unknown invalid input {other}"),
                };
                if invalid == "folder" {
                    std::fs::create_dir_all(&path).unwrap();
                }
                let path_text = path.to_str().unwrap();
                let expected = expected.replace("{path}", path_text);
                let output = support::run(BINARY, &["--status", "--db", path_text], &home, &[]);
                assert_eq!(output.status.code(), Some(0), "{label}: exit");
                let actual = line(&output);
                if actual != expected {
                    failures.push(format!("{label}\n  recorded: {expected}\n  rust:     {actual}"));
                }
            }
            "demo-watch" => {
                let args: Vec<String> = case["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect();
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                let output = support::run(BINARY, &args, &home, &[]);
                assert_eq!(output.status.code(), Some(0), "{label}: exit; {}", support::stdout_text(&output));
                let actual = support::stdout_text(&output);
                if actual != expected {
                    let recorded: Vec<&str> = expected.lines().collect();
                    let rust: Vec<&str> = actual.lines().collect();
                    let first = (0..recorded.len().max(rust.len())).find(|&i| recorded.get(i) != rust.get(i)).unwrap_or(0);
                    failures.push(format!(
                        "{label}: {} recorded lines, {} rust lines; first difference at line {}\n  recorded: {}\n  rust:     {}",
                        recorded.len(),
                        rust.len(),
                        first + 1,
                        recorded.get(first).unwrap_or(&"<missing>"),
                        rust.get(first).unwrap_or(&"<missing>"),
                    ));
                }
            }
            other => panic!("{label}: unknown case kind {other}"),
        }
    }
    assert!(checked > 300, "fixture holds only {checked} cases");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        failures.is_empty(),
        "{} of {checked} status cases differ (recorded from {}):\n{}",
        failures.len(),
        support::recorded_from(&fixture),
        failures.iter().take(6).cloned().collect::<Vec<_>>().join("\n")
    );
    let _: Value = fixture;
    println!("{checked} status cases match the recording");
}
