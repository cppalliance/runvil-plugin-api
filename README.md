# runvil-plugin-api

The contract between a runvil Host and its Plugins: a C header, the raw Rust mirror of it, and a safe Rust SDK on top. A Plugin depends on this repository alone, never on the Engine or the Harness.

| Piece | What it is | Who uses it |
|---|---|---|
| `crates/runvil-plugin-sys/include/runvil_plugin.h` | The C contract, generated from the Rust source | C, C++, and other non-Rust Plugins |
| `runvil-plugin-sys` | Raw `#[repr(C)]` types and constants, a line-for-line mirror of the header | The Harness's loader, and Rust Plugins that want no SDK |
| `runvil-plugin-sdk` | A safe `Plugin` trait, the C entry point, `export_plugin!`, and a fake Host for tests | Most Rust Plugins |

## Writing a Plugin in Rust

`Cargo.toml`:

```toml
[package]
name = "acme-plugin"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
runvil-plugin-sdk = { git = "https://github.com/cppalliance/runvil-plugin-api", tag = "api-v0.1.0" }

[dev-dependencies]
runvil-plugin-sdk = { git = "https://github.com/cppalliance/runvil-plugin-api", tag = "api-v0.1.0", features = ["test-support"] }
```

`src/lib.rs`:

```rust
use runvil_plugin_sdk::{Plugin, Refusal, export_plugin};

#[derive(Debug)]
pub struct Acme;

impl Plugin for Acme {
    const NAME: &'static str = "acme";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn load() -> Result<Acme, Refusal> {
        Ok(Acme)
    }
}

export_plugin!(Acme);
```

`cargo build --release` writes `acme_plugin.dll` (Windows), `libacme_plugin.so` (Linux), or `libacme_plugin.dylib` (macOS) to `target/release`. That one file goes in the Host's Plugin directory. `examples/hello-plugin` is a complete Plugin with a test.

A name or version the Harness would reject fails the build, with a message naming the rule. A panic in `load` reaches the Host as a refusal instead of unwinding into it.

### Testing

With `test-support` on, `runvil_plugin_sdk::testing::load::<Acme>()` loads the Plugin through a fake Host that checks every call the way the Harness does, and returns its name and version, or the failure the Harness would report.

### Linked and library builds of one Plugin

A Host can also link a Plugin crate in and hand `runvil_plugin_sdk::entry::<Acme>` to the Harness, so no symbol is exported. Two linked Plugins that both export `runvil_plugin` cannot share one binary, so a crate built both ways puts `export_plugin!` behind a feature of its own, as `examples/hello-plugin` does with `dll`.

## Writing a Plugin in C or C++

Include `runvil_plugin.h` and define `RvStatus runvil_plugin(RvCall *call)`. The header's opening comment is the whole contract: the verbs, who calls what on which thread, and what the Plugin must free on every path. `tests/c-layout` compiles the header as C99 and as C++ on every CI run.

## Versions

The contract's version is `RV_API_MAJOR`.`RV_API_MINOR`, and `runvil-plugin-sys` carries the same version number. While the major version is 0, each version is its own contract, so a Plugin built for 0.1 loads only in a Host that speaks exactly 0.1. From 1.0, minor versions only add, and a Plugin built for 1.n loads in any Host that speaks 1.n or later.

Cargo allows one package per `links` value in a build, and `runvil-plugin-sys` sets `links = "runvil_plugin"`, so a Host can never link two versions of the contract into one binary.

## Platform notes

- Windows: the Harness resolves a library Plugin's own DLL dependencies only from the Plugin's directory and System32. This repository links with the static CRT (`.cargo/config.toml`) so a Plugin does not need the MSVC runtime DLLs; a Plugin built elsewhere should do the same.
- Keep Cargo's default `panic = "unwind"`. With `panic = "abort"`, a panic in a Plugin aborts the Host's process instead of becoming a refusal.

## Working on this repository

| Task | Command |
|---|---|
| Regenerate the header after editing `crates/runvil-plugin-sys/src/lib.rs` | `cargo xtask header` |
| Check the header is current | `cargo xtask header --check` (also part of `cargo test`) |
| Test, including the C and C++ layout check | `cargo test --workspace --all-features` |
| Lint | `cargo clippy --workspace --all-targets --all-features` |

The C and C++ layout check needs a C and C++ compiler: MSVC on Windows, and the system compiler elsewhere.

## License

Boost Software License 1.0; see `LICENSE`.
