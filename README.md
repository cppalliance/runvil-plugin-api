# runvil-plugin-api

The contract between a runvil Host and its Plugins: a C header, the raw Rust mirror of it, and a safe Rust SDK on top. A Plugin depends on this repository alone, never on the Engine or the Harness.

| Piece | What it is | Who uses it |
|---|---|---|
| `crates/runvil-plugin-sys/include/runvil_plugin.h` | The C contract, generated from the Rust source | C, C++, and other non-Rust Plugins |
| `runvil-plugin-sys` | Raw `#[repr(C)]` types and constants, the source the header is generated from | The Harness's loader, and Rust Plugins that want no SDK |
| `runvil-plugin-sdk` | Safe `Plugin` and `Pack` traits, the C entry point, `export_plugin!`, and a fake Host for tests | Most Rust Plugins |

## What a Plugin does

A Plugin loads once, names itself, and inserts its Packs. For each run that holds one of those Packs, the Host asks the Plugin for an instance of it, and the instance lists its tools. When a section of a prompt is about to show a model its tools, the Host may ask the instance which tools that model should see. The Host then runs each tool call as a series of short steps, and frees the instance when the run ends.

| Verb | When the Host sends it |
|---|---|
| `RV_PLUGIN_LOAD` | Once, as the Host inserts the Plugin |
| `RV_PACK_NEW` | Once for each run that holds one of the Plugin's Packs |
| `RV_PACK_RENDER` | When a section entry's first model round is about to show its tools, once for each of the section's two sets of tools that holds a tool of the Pack |
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
runvil-plugin-sdk = { git = "https://github.com/cppalliance/runvil-plugin-api", tag = "api-v0.7.0" }

[dev-dependencies]
runvil-plugin-sdk = { git = "https://github.com/cppalliance/runvil-plugin-api", tag = "api-v0.7.0", features = ["test-support"] }
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
- The Pack's name is a vendor and a Pack id joined by `/`. A prompt reaches the tool above as `hello/greet`, the Pack id and the tool's name. A model calls it by its wire name, which is `hello_greet` unless `ToolInfo::wire_name` sets another.
- Arguments and parameter schemas cross as JSON text, so the SDK needs no JSON library. A Plugin parses `call.arguments()` with one of its own.
- `call.resources()` is what the Host offers every Plugin, such as the address and the key of a service: the JSON text of an object keyed by resource name, as it was when the call started, and `{}` when the Host offers nothing. A Plugin parses it the same way. It can hold a key, so no `Debug` in the SDK prints it, and a Plugin puts none of it in an output or a failure.
- A tool call is a future the Host polls once per step. A `Pending` parks the call, and the Host sends the next step when the future's waker is woken. So a call may wait on a waker like any Rust future: on a timer, a channel, or a task on a runtime the Plugin owns. A future that returns `Pending` without arranging a wake, and without a file request, parks for good. A call that does a long job awaits `next_step()` every so often, which ends the step and wakes the call for the next one, so that no step works for long and the Host can abandon the call between two steps.
- A tool reaches the files of its run through its `ToolCall`. `stat`, `list`, `read`, and `read_range` read them, and `write`, `append`, `mkdir`, `remove`, `rename`, and `copy` change them. Each returns a future, as in `call.read(path).await`. Awaiting it ends the step with a request to the Host, and a later step goes on with the answer, so the run's policy applies to every operation. An operation that fails resolves to a `FileError` with a kind and the Host's words for what went wrong, and the call goes on, so the tool decides what the model reads. An answer comes only with a step, so a file method is awaited inside the future that `Pack::call` returns. A file future gets its answer only when a step polls it, so it does not work inside a set that polls a child only after that child's waker fired, such as `FuturesUnordered`.
- A tool that changes files says so with `ToolInfo::changes_files()`. Of the calls one reply of a model asks for, the Host runs a call of such a tool by itself, so two changes of one reply happen in the order the reply lists them.
- A panic in `load`, `new_pack`, or `render` reaches the Host as a refusal, and a panic in a tool call ends that call as a failure. None unwinds into the Host.

### Catalog tools and model-facing tools

A tool a Pack lists when its instance is made, with `tools`, is a **catalog tool**. A prompt's Lua can call it by its id, such as `fs/edit`, a section offers it to a model with `tools.offer("fs/edit")` or offers the whole Pack with `tools.offer("fs")`, and its parameters are the same for the whole run, so a prompt author can read them in the Host's docs.

Models are trained on particular tool shapes. A Claude model expects a string-replace tool, and an OpenAI model expects a tool that takes a diff. A section is one heading of a prompt, a section entry is one run of it, and its tools come in two sets: the ones every round of the section sends, and the ones the model finds through two discovery tools. A Pack shows each model the shape it was trained on with `Pack::render`, which the Host calls when a section entry's first model round is about to advertise its tools. `render` gets the section entry, the model as JSON text, and the catalog tools the section offered, and returns the tools the model is shown. The Host keeps that answer for the whole section entry.

- A tool the Pack leaves out of the answer is not shown to the model.
- A tool the Pack lists under the `name` of an offered tool is that catalog tool. It may take another `wire_name` and `description`, and its `parameters` stay as offered. So one catalog name always means one parameter schema, and Lua can still call the tool by its id.
- A tool the Pack lists under a name that no tool of its catalog has is a **model-facing tool**. Only a model calls it, and no script can name it. It is how a model sees other parameters than the catalog's: the Pack lists it under a name of its own, and `Pack::call` answers its calls by that name. A catalog tool the section did not offer in this set may not be listed under its own name.

For example, a Pack `acme/fs` lists the catalog tool `edit`, which takes `path`, `old`, and `new`. For a Claude model it renders `edit` out and lists a model-facing tool `str_replace` with the wire name `str_replace_based_edit_tool` and the parameters that model was trained on. For an OpenAI model it lists `apply_patch`, with one parameter that holds a diff. Every other model sees `edit` as it is, which is the default. A model-facing tool that changes files says so with `ToolInfo::changes_files()`, because the Host does not copy that flag from the catalog.

```rust
fn render(&self, cx: &RenderContext<'_>, offered: Vec<ToolInfo>) -> Result<Vec<ToolInfo>, Refusal> {
    // `serde_json` is the Plugin's own JSON library; the SDK has none.
    let model = serde_json::from_str::<serde_json::Value>(cx.model).unwrap_or_default();
    if !model["name"].as_str().unwrap_or_default().contains("claude") {
        return Ok(offered);
    }
    let mut shown: Vec<ToolInfo> = offered.into_iter().filter(|tool| tool.name != "edit").collect();
    shown.push(
        ToolInfo::new("str_replace", "Replace one exact string in a file.", STR_REPLACE_SCHEMA)
            .wire_name("str_replace_based_edit_tool")
            .changes_files(),
    );
    Ok(shown)
}
```

`STR_REPLACE_SCHEMA` stands for the JSON text of that tool's parameters. A prompt names catalog ids only, and its prose describes a tool by what it does and not by the name a model sees. A prompt that offers a Pack trusts that Pack's render to choose the model-facing tools, so a Plugin that needs a prompt to offer reading without editing inserts two Packs.

### Testing

With `test-support` on, `runvil_plugin_sdk::testing` is a fake Host that speaks the contract to the Plugin's entry point and checks every call the Plugin makes back the way the Harness does:

```rust
let loaded = testing::load::<Acme>().unwrap();
let pack = loaded.new_pack("acme/hello").unwrap();
assert_eq!(pack.call("greet", "{}"), Ok(Ok(ToolOutput::trusted("Hello."))));
```

`load` returns the Plugin's name, version, and Packs, or the failure the Harness would report. `new_pack` returns one run's instance with its tools. `call` runs a tool call to its end as the Harness does: it sends a step, and after each step that parks the call it waits until the Plugin has woken the call, or goes on at once when the step made a file request. A call that nothing wakes fails its test after 60 seconds, with a sentence that says so, and does not hang it. `start` hands back a call whose steps the test sends one at a time with `step`, and `wait` blocks until the call is due its next one. Dropping each value sends the verb that frees what it stands for.

Every call is lent `{}` as the Host's resources, unless `with_resources` gives the run another text:

```rust
let resources = r#"{"acme/search":{"url":"http://127.0.0.1:9/search"}}"#;
let pack = loaded.new_pack("acme/hello").unwrap().with_resources(resources);
```

A tool that reads or changes files works on a `Files` tree held in memory:

```rust
let files = Files::new()
    .file("/notes/a.md", "one\ntwo\n")
    .fails("/secret", FileError::new(FileErrorKind::Denied, "the mode allows only /notes"));
let pack = loaded.new_pack("acme/notes").unwrap().with_files(files);
pack.call("save", r#"{"path":"/notes/b.md","text":"three\n"}"#).unwrap();
assert_eq!(pack.files().read("/notes/b.md", 0, u64::MAX), Ok(b"three\n".to_vec()));
```

`file` and `dir` add a node and every directory on the way to it, and `fails` makes every operation on a path fail with the error given, a change included, as a path the run's policy denies does. A path that fails has nothing under it: `fails` refuses a directory that has entries, because those entries would stay readable, and a change that would make something under it fails with the same error. The calls of one `PackRun` share the tree, and `PackRun::files` lends it to the test, which reads what a call left there. The fake Host performs a file request on the tree once the step that made it has returned, and lends the call's next step the answer.

A Pack that renders is tested with `PackRun::render(entry, model, offered)`, which sends `RV_PACK_RENDER` with the tools the section offered and returns the tools the model is shown:

```rust
// `Fs` is a Plugin that inserts the Pack `acme/fs` and renders as above.
let loaded = testing::load::<Fs>().unwrap();
let pack = loaded.new_pack("acme/fs").unwrap();
let shown = pack.render("entry-1", r#"{"name":"claude-x"}"#, pack.tools()).unwrap();
assert_eq!(shown[0].wire_name.as_deref(), Some("str_replace_based_edit_tool"));
```

It checks each listed tool as the Harness does, and then the render as a whole: a catalog tool must keep its offered `parameters`, a tool of the instance the section did not offer in this set may not be listed, and no two tools may show one wire name. A broken rule is a `Failure::Rejected` whose text names the tool. The fake Host compares `parameters` as text, which is stricter than the Harness, which compares the JSON they hold. A render that lists the offered `ToolInfo` with another wire name or description passes both. A model-facing tool that a render listed is then called with `PackRun::call` under the name it was listed under, as a model calls it.

### Linked and library builds of one Plugin

A Host can also link a Plugin crate in and pass `runvil_plugin_sdk::entry::<Acme>` to the Harness's `insert_plugin`, so no symbol is exported. Two linked Plugins that both export `runvil_plugin` cannot share one binary, so a crate built both ways puts `export_plugin!` behind a feature of its own, as `examples/hello-plugin` does with `dll`.

## Writing a Plugin in C or C++

Include `runvil_plugin.h` and define `RvStatus runvil_plugin(RvCall *call)`. The header's opening comment is the whole contract: the verbs, who calls what on which thread, and what the Plugin must free on every path. The functions a Plugin may call back come with the verb they serve, in its arguments struct behind `call->args` (an `RvLoadArgs` for `RV_PLUGIN_LOAD`, an `RvPackNewArgs` for `RV_PACK_NEW`, an `RvPackRenderArgs` for `RV_PACK_RENDER`, an `RvToolCallArgs` for `RV_TOOL_CALL`), and each takes that struct's `ctx` first. A step of a tool call makes one file request through one of the nine file functions in its `RvToolCallArgs`, such as `file_read` or `file_write`, and returns `RV_PENDING`, and the call's next step finds the answer behind `answer` in its own `RvToolCallArgs`. A step that returns `RV_PENDING` without a file request parks its call, and the Host owes it no step until a wake. So such a step first calls `args->waker.wake(args->waker.ctx)` when it wants the next step right away, or a thread of the Plugin makes that call later, when what the tool call waits for is there. `wake` takes the `ctx` of its own `RvWaker` and not the step's, and it is the one function a Plugin may call on any thread, until `RV_PLUGIN_UNLOAD` returns. `resources` in the same `RvToolCallArgs` is the JSON text of what the Host offers every Plugin. A Plugin that shapes its tools for a model answers `RV_PACK_RENDER`: its `RvPackRenderArgs` holds the section entry in `entry`, the model as JSON text in `model`, the tools the section offered in `offered` and `offered_len`, and the functions `add_tool` and `refuse`. The Plugin calls `add_tool` once for each tool the model is shown and returns `RV_OK`. A Plugin that does not render answers `RV_ERR_UNSUPPORTED`, and the model is shown the offered tools as they are. `tests/c-layout` compiles the header as C99 and as C++ on every CI run.

## Versions

The contract's version is `RV_API_MAJOR`.`RV_API_MINOR`, and `runvil-plugin-sys` has the same version number. While the major version is 0, each version is its own contract, so a Plugin built for 0.7 loads only in a Host that speaks exactly 0.7. From 1.0, minor versions only add, and a Plugin built for 1.n loads in any Host that speaks 1.n or later.

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
