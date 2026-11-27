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
- A status code, verb, result kind, file type, file error kind, tool flag, or field keeps its number and position in every version. A reserved verb number is never reused for anything else.
- The `runvil-plugin-sys` version is the contract's version, `RV_API_MAJOR`.`RV_API_MINOR`; a unit test checks it. Changing the contract means changing both.
- While the major version is 0, each version is its own contract, and the SDK accepts only a Host that reports exactly its version. From 1.0, a minor version only adds, at the end of a struct, or as a new verb with an arguments struct of its own.
- `RvHost` holds only what is true on every call. Each function a Plugin may call back belongs to one verb and sits in that verb's arguments struct, beside the `ctx` it takes first, under that verb's own lifetime and thread rules. `wake` is the one exception: it sits in the `RvWaker` of a tool call, beside the waker's own `ctx`, which it takes, and the Plugin may call it on any thread until `RV_PLUGIN_UNLOAD` returns.
- A Host keeps the `ctx` of a load valid while the Plugin is loaded, until `RV_PLUGIN_UNLOAD` returns, and answers a call through any `ctx` outside its verb's rules with `RV_ERR_MISUSE`; it never reads through a `ctx` it has not matched to the call open on the calling thread. A waker's `ctx` is the one exception: `wake` reads through it on any thread, so a Host keeps what it points at valid until `RV_PLUGIN_UNLOAD` returns, and a wake after its call ended does nothing for that call.
- A step that returns `RV_PENDING` parks its call. The Host sends the next step after the Plugin wakes the call, or when the answer to the step's file request is ready, and it sends no step while that answer is outstanding. `wake` only notes the wake and returns nothing, so a Plugin may call it while it holds a lock.
- The SDK polls a call's future with a waker over the Host's `RvWaker`. That waker takes no lock and never lets go of the Host's, because the Host keeps it valid until the unload. A future that returns `Pending` without arranging a wake, and without a file request, parks for good. The SDK itself wakes a call only in `next_step`, and when the Host refuses a file request, whose failure no answer would bring.
- The contract stays minimal. A verb, a field, or a function joins only when a Plugin that exists needs it, and each one says who calls it, on which thread, how long what it passes stays valid, and who frees what.
- A render shapes what a model is shown and keeps nothing. `RV_PACK_RENDER` lends a Pack instance the tools a section offered from it and the model that will read them, and takes back the tools the model is shown through `add_tool`. A tool added under an offered tool's name keeps that tool's parameters, so one catalog name always means one parameter schema, and a tool with other parameters is a model-facing tool under a name of its own, which no script can name. A Plugin that does not render answers `RV_ERR_UNSUPPORTED`, and the model is shown the offered tools as they are without any answer being read. The SDK's default `Pack::render` is not that answer: it lists the offered tools through `add_tool`, so the Host checks them like any other list.
- JSON crosses the contract as text. Neither crate here parses it, so neither needs a JSON library.
- The Host's resources can hold a key. The SDK copies their text on a call's first step into one private type whose `Debug` prints none of it, and no message here quotes it.
- A tool reaches its run's files only through the file functions of its step, so the run's policy applies to every operation. A file function only takes a request, and the call's next step receives the answer. The answer belongs to the Host and is valid for that step alone, so the SDK copies it before it polls the tool's future.
- Every file function has that one form, a request in one step and its answer in the next, whether it reads a file or changes one, so that the Harness can perform each file operation of a tool as an effect and record it. The file methods of the SDK return futures.
- `runvil-plugin-sys` has no dependencies and no `unsafe`. `runvil-plugin-sdk` depends on `runvil-plugin-sys` alone. Neither ever depends on an Engine or Harness crate.
- A Plugin shares its process with the Host, so nothing here installs process-global state, such as a panic hook (`clippy.toml` bans `std::panic::set_hook`), and no panic ever unwinds across the C boundary.
- `testing` in the SDK checks a Plugin's calls the way the Harness's loader does: the same order of checks, the same bounds, the same rules on names, versions, and tool infos, and the same words for each misuse. It parks a call as the loader does too: a step answers the wakes that came before it began, the next step is due after a wake or as soon as a file answer is ready, and what a waker points at is kept until the Plugin is unloaded. It checks a render as the Engine's merge does, with one difference: it compares the parameters of a catalog tool as text, because this crate has no JSON library, so it is stricter than the Engine. When the loader's checks change, this changes with them.

## Engineering

- Prefer types and compiler checks, then behavior tests. A rule a Plugin's constants must follow is checked when the Plugin is compiled, as `entry` checks `Plugin::NAME` and `Plugin::VERSION`.
- Behavior changes ship with tests in the same change.
- Unsafe code stays in its explicitly owned boundary: in the SDK, `entry` and its child modules, the `export_plugin!` expansion, and `testing`; in `tests/c-layout`, the test-only calls. Every `unsafe` block has a `// SAFETY:` comment, and every `unsafe fn` a `# Safety` section.
- Nothing holds a lock across a call into a Plugin, or across a Host's `wake`. A Plugin may wake a call from a destructor on the Host's own thread, inside a step or inside `RV_TOOL_CANCEL`, and a Host's `wake` may take a lock of its own.
- Comments explain a non-obvious constraint, ordering requirement, or workaround.
- New unit tests go in a `#[cfg(test)] mod tests` block in the file under test.
- Error and status messages are written for a model to read: concise, factual, and self-contained, naming what is missing or unmet.

## Verification

- Tests, including the header freshness check and the C and C++ layout check: `cargo test --workspace --all-features`.
- Linter: `cargo clippy --workspace --all-targets --all-features`, gated with `CARGO_BUILD_WARNINGS=deny`.
- Formatter: `cargo fmt --all --check`.
- Docs: `cargo doc --workspace --no-deps --all-features` with `RUSTDOCFLAGS="-D warnings"`.
- Toolchain floor of the two crates a Plugin depends on: `cargo +1.85 check -p runvil-plugin-sys -p runvil-plugin-sdk --all-features`.
