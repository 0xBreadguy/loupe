//! The product command line against output recorded from the TypeScript original (main
//! `2f1d553`): all 75 cases of `checks/scripts/cli-parity.ts`, comparing exit code, standard
//! output bytes and standard error bytes. The synthetic databases the cases read travel in the
//! fixture as SQL text and are rebuilt here, so no Node and no `generated/` directory is needed.
#[path = "golden_support/mod.rs"]
mod support;
use serde_json::Value;
use std::{collections::HashMap, path::{Path, PathBuf}};

const BINARY: &str = env!("CARGO_BIN_EXE_t3-agents-core");

fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "hex has an even length");
    (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
}

/// Puts every input a case names on disk under `root` and returns token → path.
fn resolve(assets: &serde_json::Map<String, Value>, token: &str, root: &Path, done: &mut HashMap<String, PathBuf>) -> PathBuf {
    if let Some(path) = done.get(token) {
        return path.clone();
    }
    let asset = assets.get(token).unwrap_or_else(|| panic!("asset {token} is missing"));
    let path = root.join(token);
    match asset["kind"].as_str().unwrap() {
        "sql" => {
            std::fs::create_dir_all(root).unwrap();
            support::rebuild_database(&path, asset["sql"].as_str().unwrap());
        }
        "text" => std::fs::write(&path, asset["text"].as_str().unwrap()).unwrap(),
        "tree" => {
            std::fs::create_dir_all(&path).unwrap();
            for (name, file) in asset["files"].as_object().unwrap() {
                let target = path.join(name);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                // A file inside a tree is always its own asset.
                let source = resolve(assets, file.as_str().unwrap().trim_start_matches('@'), root, done);
                std::fs::copy(&source, &target).unwrap_or_else(|e| panic!("{}: {e}", target.display()));
            }
        }
        other => panic!("asset {token}: unknown kind {other}"),
    }
    done.insert(token.to_string(), path.clone());
    path
}

fn materialize(fixture: &Value, root: &Path) -> HashMap<String, PathBuf> {
    let assets = fixture["assets"].as_object().expect("assets");
    let mut done: HashMap<String, PathBuf> = HashMap::new();
    let mut tokens: Vec<String> = Vec::new();
    let collect = |value: &Value, tokens: &mut Vec<String>| {
        if let Some(text) = value.as_str() {
            for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '@' || c == '/')) {
                if let Some(rest) = word.strip_prefix('@') {
                    let token = rest.split('/').next().unwrap();
                    if !token.is_empty() && !tokens.iter().any(|t| t == token) {
                        tokens.push(token.to_string());
                    }
                }
            }
        }
    };
    for case in fixture["cases"].as_array().unwrap() {
        for arg in case["args"].as_array().unwrap() {
            collect(arg, &mut tokens);
        }
        for value in case["env"].as_object().unwrap().values() {
            collect(value, &mut tokens);
        }
    }
    for token in tokens {
        resolve(assets, &token, root, &mut done);
    }
    done
}

/// Token → real path, longest token first so a tree is replaced before a file inside it.
fn pairs(paths: &HashMap<String, PathBuf>, root: &Path) -> Vec<(String, String)> {
    let mut keys: Vec<&String> = paths.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    let mut pairs: Vec<(String, String)> = keys.into_iter().map(|key| (format!("@{key}"), paths[key].to_str().unwrap().to_string())).collect();
    pairs.push(("{tmp}".into(), root.to_str().unwrap().into()));
    pairs
}

fn substitute(text: &str, pairs: &[(String, String)]) -> String {
    pairs.iter().fold(text.to_string(), |out, (from, to)| out.replace(from, to))
}

fn replace_bytes(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut i = 0;
    while i < haystack.len() {
        if haystack[i..].starts_with(needle) {
            out.extend_from_slice(with);
            i += needle.len();
        } else {
            out.push(haystack[i]);
            i += 1;
        }
    }
    out
}

/// A recorded stream with the placeholders put back, byte for byte.
fn substitute_bytes(bytes: &[u8], pairs: &[(String, String)]) -> Vec<u8> {
    pairs.iter().fold(bytes.to_vec(), |out, (from, to)| replace_bytes(&out, from.as_bytes(), to.as_bytes()))
}

fn after_first_line(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|b| *b == b'\n') {
        Some(i) => &bytes[i + 1..],
        None => &[],
    }
}

fn show(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    if text.len() > 600 { format!("{}…", &text[..600.min(text.len())]) } else { text.into_owned() }
}

#[test]
fn command_line_matches_frozen_oracle() {
    let fixture = support::load("cli.json.gz");
    let cases = fixture["cases"].as_array().expect("cases");
    let top = support::scratch("cli");
    let home_default = support::empty_home(&top);
    let paths = materialize(&fixture, &top.join("inputs"));
    let mut failures = Vec::new();
    for (index, case) in cases.iter().enumerate() {
        let label = case["label"].as_str().unwrap();
        let root = top.join(format!("case-{index}"));
        std::fs::create_dir_all(&root).unwrap();
        let local = pairs(&paths, &root);
        let mut args: Vec<String> = case["args"].as_array().unwrap().iter().map(|a| substitute(a.as_str().unwrap(), &local)).collect();
        for extra in case["nativeArgs"].as_array().unwrap() {
            args.push(substitute(extra.as_str().unwrap(), &local));
        }
        let env = case["env"].as_object().unwrap();
        let home = substitute(env["HOME"].as_str().unwrap_or(""), &local);
        let home = if home.is_empty() { home_default.clone() } else { PathBuf::from(home) };
        std::fs::create_dir_all(&home).unwrap();
        let mut extra: Vec<(String, String)> = Vec::new();
        if let Some(timezone) = env["TZ"].as_str() {
            extra.push(("TZ".into(), timezone.into()));
        }
        if let Some(path) = env.get("T3CODE_HOME").and_then(Value::as_str) {
            extra.push(("T3CODE_HOME".into(), substitute(path, &local)));
        }
        let extra: Vec<(&str, &str)> = extra.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        // The cases about the default origin need T3CODE_HOME absent, exactly as the gate ran
        // them; HOME is always a scratch path, so nothing can reach a real installation.
        let output = std::process::Command::new(BINARY)
            .args(&args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", &home)
            .envs(extra.iter().map(|(k, v)| (*k, *v)))
            .output()
            .unwrap_or_else(|e| panic!("{label}: run: {e}"));
        let expected = &case["expected"];
        let recorded_status = expected["status"].as_i64().unwrap();
        let recorded_out = substitute_bytes(&unhex(expected["stdout"].as_str().unwrap()), &local);
        let recorded_err = substitute_bytes(&unhex(expected["stderr"].as_str().unwrap()), &local);
        let ignore_first = case["ignoreFirstStderrLine"].as_bool().unwrap();
        let actual_err = if ignore_first { after_first_line(&output.stderr) } else { output.stderr.as_slice() };
        let mut problems: Vec<String> = Vec::new();
        if output.status.code().map(i64::from) != Some(recorded_status) {
            problems.push(format!("exit {:?}, recorded {recorded_status}", output.status.code()));
        }
        if output.stdout != recorded_out {
            problems.push(format!("stdout\n  recorded: {}\n  rust:     {}", show(&recorded_out), show(&output.stdout)));
        }
        if actual_err != recorded_err {
            problems.push(format!("stderr\n  recorded: {}\n  rust:     {}", show(&recorded_err), show(actual_err)));
        }
        if !problems.is_empty() {
            failures.push(format!("{label} {args:?}\n  {}", problems.join("\n  ")));
        }
    }
    assert!(cases.len() > 70, "fixture holds only {} cases", cases.len());
    let result = if failures.is_empty() {
        Ok(())
    } else {
        Err(())
    };
    let _ = std::fs::remove_dir_all(&top);
    if result.is_err() {
        panic!(
            "{} of {} command-line cases differ (recorded from {}):\n{}",
            failures.len(),
            cases.len(),
            support::recorded_from(&fixture),
            failures.iter().take(4).cloned().collect::<Vec<_>>().join("\n\n")
        );
    }
    println!("{} command-line cases match the recording", cases.len());
}
