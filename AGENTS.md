# runvil-plugin-api

The contract between a runvil Host and its Plugins, and nothing else: `runvil_plugin.h`, `runvil-plugin-sys` (its raw Rust mirror), and `runvil-plugin-sdk` (a safe Rust layer over the mirror). The Engine, the Harness, and Workshop live in other repositories and depend on this one; this repository depends on none of them.

## Definitions

These terms have exactly one meaning each, everywhere in this repository, as in the Engine's repository. Write them capitalized.

- **Engine**: the crates that parse a prompt and step a run. They never see this contract.
- **Harness**: the crates that step the Engine and perform its effects. The Harness's loader is the only code that calls a Plugin's entry point in production.
- **Host**: an application that runs prompts through the Harness, such as Workshop. The Host chooses which Plugins load.
- **Pack**: code which installs an optional native prelude, an optional Lua prelude, and zero or more tools.
- **Plugin**: an entry point which installs zero or more Packs. A library Plugin is a dynamic library that exports `runvil_plugin`; a linked Plugin is compiled into the Host under a name of its own crate's choosing.

Anything else gets a qualified lowercase name, such as Rust's built-in test harness or a C compiler's plugin. Names defined outside this repository, such as Cargo's `links` key, are used exactly as defined.

## The contract's rules

- `crates/runvil-plugin-sys/src/lib.rs` is the single source of the contract. `crates/runvil-plugin-sys/include/runvil_plugin.h` is generated from it by `cargo xtask header` and never edited by hand; its crate docs, up to `## Invariants`, become the header's opening comment.
- A status code, verb, or field keeps its number and position in every version. A reserved verb number is never reused for anything else.
- The `runvil-plugin-sys` version is the contract's version, `RV_API_MAJOR`.`RV_API_MINOR`; a unit test checks it. Changing the contract means changing both.
- While the major version is 0, each version is its own contract, and the SDK accepts only a Host that reports exactly its version. From 1.0, a minor version only adds, at the end of a struct or of `RvHost`, or as a new verb.
- `runvil-plugin-sys` has no dependencies and no `unsafe`. `runvil-plugin-sdk` depends on `runvil-plugin-sys` alone. Neither ever depends on an Engine or Harness crate.
- A Plugin shares its process with the Host, so nothing here installs process-global state, such as a panic hook (`clippy.toml` bans `std::panic::set_hook`), and no panic ever unwinds across the C boundary.
- `testing` in the SDK checks a Plugin's calls the way the Harness's loader does: the same order of checks, the same bounds, and the same rules on names and versions. When the loader's checks change, this changes with them.

## Engineering

- Prefer types and compiler checks, then behavior tests. A rule a Plugin's constants must follow is checked when the Plugin is compiled, as `entry` checks `Plugin::NAME` and `Plugin::VERSION`.
- Behavior changes ship with tests in the same change.
- Unsafe code stays in its explicitly owned boundary: `entry`, the `export_plugin!` expansion, and `testing` in the SDK, and the test-only calls in `tests/c-layout`. Every `unsafe` block has a `// SAFETY:` comment, and every `unsafe fn` a `# Safety` section.
- Comments explain a non-obvious constraint, ordering requirement, or workaround.
- New unit tests go in a `#[cfg(test)] mod tests` block in the file under test.
- Error and status messages are written for a model to read: concise, factual, and self-contained, naming what is missing or unmet.

## Verification

- Tests, including the header freshness check and the C and C++ layout check: `cargo test --workspace --all-features`.
- Linter: `cargo clippy --workspace --all-targets --all-features`, gated with `CARGO_BUILD_WARNINGS=deny`.
- Formatter: `cargo fmt --all --check`.
- Docs: `cargo doc --workspace --no-deps --all-features` with `RUSTDOCFLAGS="-D warnings"`.
- Toolchain floor of the two published crates: `cargo +1.85 check -p runvil-plugin-sys -p runvil-plugin-sdk --all-features`.
