//! Exercise the Linux storage inventory over one explicitly synthetic fixture.
//! The Python driver creates the fixture and checks this JSON against an independent
//! metadata walk. This program opens no database and reads no file contents.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use serde_json::{Value, json};
use t3_agents_core::storage::{
    Folder, Inventory, InventoryInput, Owner, Project, Thread, Usage, inventory,
};

const UID: u32 = 4242;

fn usage(usage: Usage) -> Value {
    json!({
        "worktree_bytes": usage.worktree_bytes,
        "dependency_bytes": usage.dependency_bytes,
        "scratch_bytes": usage.scratch_bytes,
        "other_bytes": usage.other_bytes,
    })
}

fn owner(owner: &Owner) -> Value {
    match owner {
        Owner::Thread { id, settled } => json!({"kind": "Thread", "id": id, "settled": settled}),
        Owner::Shared => json!({"kind": "Shared"}),
        Owner::Missing => json!({"kind": "Missing"}),
    }
}

fn folder(root: &Path, folder: &Folder) -> Value {
    json!({
        "path": folder.path.strip_prefix(root).expect("fixture folder").to_string_lossy(),
        "kind": format!("{:?}", folder.kind),
        "owner": owner(&folder.owner),
        "project_id": folder.project_id,
        "coverage": format!("{:?}", folder.coverage),
        "usage": usage(folder.usage),
    })
}

fn output(root: &Path, result: &Inventory, elapsed_ns: u128) -> Value {
    json!({
        "elapsed_ns": elapsed_ns,
        "worktrees": format!("{:?}", result.worktrees),
        "claude": format!("{:?}", result.claude),
        "folders": result.folders.iter().map(|row| folder(root, row)).collect::<Vec<_>>(),
        "projects": result.projects.iter().map(|(id, row)| (id.clone(), usage(*row))).collect::<serde_json::Map<_, _>>(),
        "threads": result.threads.iter().map(|(id, row)| (id.clone(), usage(*row))).collect::<serde_json::Map<_, _>>(),
        "other": usage(result.other),
        "total": usage(result.total),
    })
}

fn run(fixture: &Path, warmup: bool) -> Result<Value, String> {
    if !fixture.is_absolute() || !fixture.join(".t3-storage-synthetic").is_file() {
        return Err("fixture must be absolute and carry the synthetic marker".into());
    }
    let managed = fixture.join("worktrees");
    let project = fixture.join("checkout/repo");
    let active = managed.join("repo/active");
    let settled = managed.join("repo/settled");
    let shared = managed.join("repo/shared");
    let gone = managed.join("repo/gone");
    let temp = fixture.join("temp");
    let projects = [Project {
        id: "repo",
        root: &project,
    }];
    let threads = [
        Thread {
            id: "active",
            project_id: "repo",
            cwd: &active,
            settled: false,
        },
        Thread {
            id: "settled",
            project_id: "repo",
            cwd: &settled,
            settled: true,
        },
        Thread {
            id: "shared-a",
            project_id: "repo",
            cwd: &shared,
            settled: false,
        },
        Thread {
            id: "shared-b",
            project_id: "repo",
            cwd: &shared,
            settled: true,
        },
        Thread {
            id: "gone",
            project_id: "repo",
            cwd: &gone,
            settled: true,
        },
    ];
    let input = InventoryInput {
        worktree_root: &managed,
        projects: &projects,
        threads: &threads,
        temp_dir: &temp,
        uid: UID,
    };
    if warmup {
        inventory(&input, || false).map_err(|_| "warmup cancelled".to_string())?;
    }
    let start = Instant::now();
    let result = inventory(&input, || false).map_err(|_| "scan cancelled".to_string())?;
    Ok(output(fixture, &result, start.elapsed().as_nanos()))
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut fixture: Option<PathBuf> = None;
    let mut warmup = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--fixture" => fixture = args.next().map(PathBuf::from),
            "--warmup" => warmup = true,
            _ => {
                eprintln!(
                    "usage: storage-walker-check --fixture ABSOLUTE_SYNTHETIC_ROOT [--warmup]"
                );
                return ExitCode::from(2);
            }
        }
    }
    let Some(fixture) = fixture else {
        eprintln!("usage: storage-walker-check --fixture ABSOLUTE_SYNTHETIC_ROOT [--warmup]");
        return ExitCode::from(2);
    };
    match run(&fixture, warmup) {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("storage-walker-check: {error}");
            ExitCode::FAILURE
        }
    }
}
