//! Headless core binary. It speaks the product command line (`--once`, `--status`, `--help`)
//! and, when an experiment flag is present, the parity and benchmark modes. No presenter is
//! linked, so the interactive dashboard is not available here.
use std::io::{self, Write};

use loupe_core::{cli, product};

fn main() {
    let result = if cli::experiment_requested() {
        cli::headless()
    } else {
        product::headless()
    };
    match result {
        Ok(true) => {}
        Ok(false) => {
            if !cli::experiment_requested() {
                // Same message as the original when there is no terminal to draw on.
                product::ensure_terminal();
            }
            eprintln!(
                "loupe-core has no presenter. Use --once, --status, --help, or a front door binary."
            );
            std::process::exit(2);
        }
        Err(e) => {
            // Stderr may share stdout's closed pipe (2>&1).
            let _ = writeln!(io::stderr().lock(), "loupe-core: {e}");
            std::process::exit(1);
        }
    }
}
