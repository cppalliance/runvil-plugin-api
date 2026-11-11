//! `hello-plugin` - a small library Plugin: it loads, names itself
//! `hello`, and inserts one Pack, `acme/hello`, whose one tool, `greet`,
//! returns a greeting. `cargo build -p hello-plugin --release` writes
//! `hello_plugin.dll` on Windows, `libhello_plugin.so` on Linux, and
//! `libhello_plugin.dylib` on macOS to `target/release`; that one file is
//! what goes in a Host's Plugin directory.
//!
//! ## Invariants
//!
//! - Uses `runvil-plugin-sdk` alone and writes no `unsafe`.

use runvil_plugin_sdk::{
    BoxFuture, Pack, PackInfo, Plugin, Refusal, ToolCall, ToolError, ToolInfo, ToolOutput,
};

/// The Plugin instance the Host holds while the Plugin is loaded. A real
/// Plugin keeps here what every run shares, such as an HTTP client;
/// dropping it is the unload.
#[derive(Debug)]
pub struct Hello;

impl Plugin for Hello {
    const NAME: &'static str = "hello";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const PACKS: &'static [PackInfo] =
        &[PackInfo::new("acme/hello").description("Greets whoever runs the prompt")];

    fn load() -> Result<Hello, Refusal> {
        Ok(Hello)
    }

    fn new_pack(&self, _pack: &str) -> Result<Box<dyn Pack>, Refusal> {
        Ok(Box::new(Greeter))
    }
}

/// One run's instance of the Pack `acme/hello`. A real Pack keeps here
/// what one run needs; dropping it ends its part in the run.
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
        // A Pack with several tools matches on `call.tool()`, and parses
        // `call.arguments()` with a JSON library of its own.
        Box::pin(async { Ok(ToolOutput::trusted("Hello from the hello Plugin.")) })
    }
}

#[cfg(feature = "dll")]
runvil_plugin_sdk::export_plugin!(Hello);

#[cfg(test)]
mod tests {
    use runvil_plugin_sdk::sys::RV_OK;
    use runvil_plugin_sdk::{ToolInfo, ToolOutput, testing};

    use super::Hello;

    #[test]
    fn the_plugin_loads_with_its_name_version_and_pack_and_unloads() {
        let loaded = testing::load::<Hello>().unwrap();
        assert_eq!(loaded.name(), "hello");
        assert_eq!(loaded.version(), env!("CARGO_PKG_VERSION"));
        let packs: Vec<&str> = loaded
            .packs()
            .iter()
            .map(|pack| pack.name.as_str())
            .collect();
        assert_eq!(packs, ["acme/hello"]);
        assert_eq!(loaded.unload(), RV_OK);
    }

    #[test]
    fn the_pack_offers_greet_and_a_call_returns_the_greeting() {
        let loaded = testing::load::<Hello>().unwrap();
        let pack = loaded.new_pack("acme/hello").unwrap();
        assert_eq!(
            pack.tools(),
            [ToolInfo::new(
                "greet",
                "Returns a greeting.",
                r#"{"type":"object","properties":{}}"#,
            )]
        );
        assert_eq!(
            pack.call("greet", "{}"),
            Ok(Ok(ToolOutput::trusted("Hello from the hello Plugin.")))
        );
    }
}
