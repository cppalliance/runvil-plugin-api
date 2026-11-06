//! `build-xtask` - automation for this repository. `cargo xtask header`
//! writes `runvil_plugin.h` from `runvil-plugin-sys`, and `--check` fails
//! when the committed header differs from what it would write.
//!
//! ## Invariants
//!
//! - The header's only source is `crates/runvil-plugin-sys/src/lib.rs`;
//!   `cargo test -p build-xtask` fails when the committed header is stale.

mod header;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(root) = Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2) else {
        eprintln!("error: cannot locate the workspace root from CARGO_MANIFEST_DIR");
        return ExitCode::FAILURE;
    };
    match args.get(1).map(String::as_str) {
        Some("header") => header::run(root, &args[2..]),
        _ => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: cargo xtask header [--check]");
    ExitCode::from(2)
}
