# runvil-plugin-api

The contract between a runvil Host and its Plugins: a C header, the raw Rust mirror of it, and a safe Rust SDK on top. A Plugin depends on this repository alone, never on the Engine or the Harness.

| Piece | What it is | Who uses it |
|---|---|---|
| `crates/runvil-plugin-sys/include/runvil_plugin.h` | The C contract, generated from the Rust source | C, C++, and other non-Rust Plugins |
| `runvil-plugin-sys` | Raw `#[repr(C)]` types and constants, the source the header is generated from | The Harness's loader, and Rust Plugins that want no SDK |
| `runvil-plugin-sdk` | Safe `Plugin` and `Pack` traits, the C entry point, `export_plugin!`, and a fake Host for tests | Most Rust Plugins |

## What a Plugin does

A Plugin loads once, names itself, and inserts its Packs. For each run that holds one of those Packs, the Host asks the Plugin for an instance of it, and the instance lists its tools. The Host then runs each tool call as a series of short steps, and frees the instance when the run ends.

| Verb | When the Host sends it |
|---|---|
| `RV_PLUGIN_LOAD` | Once, as the Host inserts the Plugin |
| `RV_PACK_NEW` | Once for each run that holds one of the Plugin's Packs |
| `RV_TOOL_CALL` | Once for each step of a tool call, until a step ends the call |
| `RV_TOOL_CANCEL` | When the Host abandons a call between two steps |
| `RV_PACK_DELETE` | When the run ends |
| `RV_PLUGIN_UNLOAD` | When the last holder of the Plugin drops it |

## Writing a Plugin in Rust

`Cargo.toml`:

```toml
[package]
name = "acme-plugin"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
runvil-plugin-sdk = { git = "https://github.com/cppalliance/runvil-plugin-api", tag = "api-v0.4.0" }

[dev-dependencies]
runvil-plugin-sdk = { git = "https://github.com/cppalliance/runvil-plugin-api", tag = "api-v0.4.0", features = ["test-support"] }
```

`src/lib.rs`:

```rust
use runvil_plugin_sdk::{
    BoxFuture, Pack, PackInfo, Plugin, Refusal, ToolCall, ToolError, ToolInfo, ToolOutput,
    export_plugin,
};

#[derive(Debug)]
pub struct Acme;

impl Plugin for Acme {
    const NAME: &'static str = "acme";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const PACKS: &'static [PackInfo] =
        &[PackInfo::new("acme/hello").description("Greets whoever runs the prompt")];

    fn load() -> Result<Acme, Refusal> {
        Ok(Acme)
    }

    fn new_pack(&self, _pack: &str) -> Result<Box<dyn Pack>, Refusal> {
        Ok(Box::new(Greeter))
    }
}

struct Greeter;

impl Pack for Greeter {
    fn tools(&self) -> Vec<ToolInfo> {
        vec![ToolInfo::new(
            "greet",
            "Returns a greeting.",
            r#"{"type":"object","properties":{}}"#,
        )]
    }

    fn call<'a>(&'a self, _call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async { Ok(ToolOutput::trusted("Hello.")) })
    }
}

export_plugin!(Acme);
```

`cargo build --release` writes `acme_plugin.dll` (Windows), `libacme_plugin.so` (Linux), or `libacme_plugin.dylib` (macOS) to `target/release`. That one file goes in the Host's Plugin directory. `examples/hello-plugin` is a complete Plugin with tests.

- The Plugin's name and version, and the name and description of each Pack, are checked when the Plugin is compiled: one the Harness would reject fails the build, with a message naming the rule.
- The Pack's name is a vendor and a Pack id joined by `/`. A prompt reaches the tool above as `hello/greet`, the Pack id and the tool's name.
- Arguments and parameter schemas cross as JSON text, so the SDK needs no JSON library. A Plugin parses `call.arguments()` with one of its own.
- A tool call is a future the Host polls once per step, and the Host sends the next step without being woken. A call that does a long job awaits `next_step()` every so often, so that no step works for long and the Host can abandon the call between two steps.
- A tool reads the files of its run through its `ToolCall`: `stat`, `list`, `read`, and `read_range`. Each goes through the Host, so the run's policy applies to every read. A read that fails returns a `FileError` with a kind and the Host's words for what went wrong, and the call goes on, so the tool decides what the model reads. The methods work on the thread that runs the step and on no other. This version has no method that changes a file.
- A panic in `load` or `new_pack` reaches the Host as a refusal, and a panic in a tool call ends that call as a failure. Neither unwinds into the Host.

### Testing

With `test-support` on, `runvil_plugin_sdk::testing` is a fake Host that speaks the contract to the Plugin's entry point and checks every call the Plugin makes back the way the Harness does:

```rust
let loaded = testing::load::<Acme>().unwrap();
let pack = loaded.new_pack("acme/hello").unwrap();
assert_eq!(pack.call("greet", "{}"), Ok(Ok(ToolOutput::trusted("Hello."))));
```

`load` returns the Plugin's name, version, and Packs, or the failure the Harness would report. `new_pack` returns one run's instance with its tools. `call` runs a tool call to its end, and `start` hands back a call whose steps the test sends one at a time. Dropping each value sends the verb that frees what it stands for.

A tool that reads files gets them from a `Files` tree held in memory:

```rust
let files = Files::new()
    .file("/notes/a.md", "one\ntwo\n")
    .fails("/secret", FileError::new(FileErrorKind::Denied, "the mode allows only /notes"));
let pack = loaded.new_pack("acme/notes").unwrap().with_files(files);
```

`file` and `dir` add a node and every directory on the way to it, and `fails` makes every read of a path fail with the error given, as a path the run's policy denies does. A path that fails has nothing under it: `fails` refuses a directory that has entries, because those entries would stay readable.

### Linked and library builds of one Plugin

A Host can also link a Plugin crate in and hand `runvil_plugin_sdk::entry::<Acme>` to the Harness, so no symbol is exported. Two linked Plugins that both export `runvil_plugin` cannot share one binary, so a crate built both ways puts `export_plugin!` behind a feature of its own, as `examples/hello-plugin` does with `dll`.

## Writing a Plugin in C or C++

Include `runvil_plugin.h` and define `RvStatus runvil_plugin(RvCall *call)`. The header's opening comment is the whole contract: the verbs, who calls what on which thread, and what the Plugin must free on every path. The functions a Plugin may call back come with the verb they serve, in its arguments struct behind `call->args` (an `RvLoadArgs` for `RV_PLUGIN_LOAD`, an `RvPackNewArgs` for `RV_PACK_NEW`, an `RvToolCallArgs` for `RV_TOOL_CALL`), and each takes that struct's `ctx` first. A step of a tool call reads its run's files through `file_stat`, `file_list`, and `file_read` in its `RvToolCallArgs`. `tests/c-layout` compiles the header as C99 and as C++ on every CI run.

## Versions

The contract's version is `RV_API_MAJOR`.`RV_API_MINOR`, and `runvil-plugin-sys` carries the same version number. While the major version is 0, each version is its own contract, so a Plugin built for 0.4 loads only in a Host that speaks exactly 0.4. From 1.0, minor versions only add, and a Plugin built for 1.n loads in any Host that speaks 1.n or later.

Cargo allows one package per `links` value in a build, and `runvil-plugin-sys` sets `links = "runvil_plugin"`, so a Host can never link two versions of the contract into one binary.

## Platform notes

- Windows: the Harness resolves a library Plugin's own DLL dependencies only from the Plugin's directory and System32. This repository links with the static CRT (`.cargo/config.toml`) so a Plugin does not need the MSVC runtime DLLs; a Plugin built elsewhere should do the same.
- Keep Cargo's default `panic = "unwind"`. With `panic = "abort"`, a panic in a Plugin aborts the Host's process instead of becoming a refusal or a failed call.

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
