//! Headless commands against a pipe that has lost its reader.
#![cfg(unix)]

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("closed-stdout-{}-{name}", std::process::id()));
        fs::create_dir_all(path.parent().unwrap()).expect("create cargo test scratch");
        fs::create_dir(&path).expect("create fresh test directory");
        fs::create_dir(path.join("t3")).expect("create empty T3CODE_HOME");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn closed_stdout(scratch: &Scratch, args: &[&str], merge_stderr: bool) -> Output {
    let (reader, writer) = io::pipe().expect("create stdout pipe");
    // Make the first write fail deterministically, even for a single short line.
    drop(reader);
    let stderr = if merge_stderr {
        Stdio::from(writer.try_clone().expect("duplicate stdout writer"))
    } else {
        Stdio::piped()
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_loupe-term"))
        .args(args)
        .arg("--demo")
        .env_clear()
        .env("HOME", &scratch.0)
        .env("T3CODE_HOME", scratch.0.join("t3"))
        .stdin(Stdio::null())
        .stdout(writer)
        .stderr(stderr)
        .spawn()
        .expect("start loupe-term");

    // A watcher that ignores the write failure would otherwise hang the test.
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("poll loupe-term").is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{args:?} did not exit after stdout closed");
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().expect("collect loupe-term stderr")
}

#[test]
fn reports_and_themes_exit_quietly() {
    let scratch = Scratch::new("product");
    let cases: &[&[&str]] = &[
        &["--trace", "--json"],
        &["--trace"],
        &["--recap"],
        &["--recap", "--json"],
        &["--tools"],
        &["--tools", "--json"],
        &["--telemetry-check"],
        &["--telemetry-check", "--json"],
        &["--themes"],
        &["--theme-check", "paper"],
    ];
    for args in cases {
        let output = closed_stdout(&scratch, args, false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(stderr.is_empty(), "{args:?}: {stderr}");
    }
}

#[test]
fn shared_commands_follow_oracle_exit_codes() {
    let scratch = Scratch::new("oracle");
    // oracle/src/main.ts uses console.log for --once and a direct stdout write
    // for --help and --status. oracle/src/status.ts handles watch write errors.
    let cases: &[(&[&str], i32)] = &[
        (&["--once"], 0),
        (&["--status", "--watch"], 0),
        (&["--help"], 1),
        (&["--status"], 1),
    ];
    for &(args, code) in cases {
        let output = closed_stdout(&scratch, args, false);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(code), "{args:?}: {stderr}");
        if code == 0 {
            assert!(stderr.is_empty(), "{args:?}: {stderr}");
        } else {
            assert!(stderr.contains("Broken pipe"), "{args:?}: {stderr}");
            assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn merged_stderr_cannot_panic_when_reporting_a_stdout_error() {
    let scratch = Scratch::new("merged");
    for (args, code) in [
        (&["--trace", "--json"][..], 0),
        (&["--help"][..], 1),
        (&["--status"][..], 1),
    ] {
        let output = closed_stdout(&scratch, args, true);
        assert_eq!(output.status.code(), Some(code), "{args:?} with 2>&1");
    }
}
