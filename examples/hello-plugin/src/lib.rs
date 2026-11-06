//! `hello-plugin` - the smallest library Plugin: it loads, names itself
//! `hello`, and unloads. `cargo build -p hello-plugin --release` writes
//! `hello_plugin.dll` on Windows, `libhello_plugin.so` on Linux, and
//! `libhello_plugin.dylib` on macOS to `target/release`; that one file is
//! what goes in a Host's Plugin directory.
//!
//! ## Invariants
//!
//! - Uses `runvil-plugin-sdk` alone and writes no `unsafe`.

use runvil_plugin_sdk::{Plugin, Refusal};

/// The Plugin instance the Host holds while the Plugin is loaded. A real
/// Plugin keeps its shared state here, such as an HTTP client; dropping it
/// is the unload.
#[derive(Debug)]
pub struct Hello;

impl Plugin for Hello {
    const NAME: &'static str = "hello";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    fn load() -> Result<Hello, Refusal> {
        Ok(Hello)
    }
}

#[cfg(feature = "dll")]
runvil_plugin_sdk::export_plugin!(Hello);

#[cfg(test)]
mod tests {
    use runvil_plugin_sdk::sys::RV_OK;
    use runvil_plugin_sdk::testing;

    use super::Hello;

    #[test]
    fn the_plugin_loads_with_its_name_and_version_and_unloads() {
        let loaded = testing::load::<Hello>().unwrap();
        assert_eq!(loaded.name(), "hello");
        assert_eq!(loaded.version(), env!("CARGO_PKG_VERSION"));
        assert_eq!(loaded.unload(), RV_OK);
    }
}
