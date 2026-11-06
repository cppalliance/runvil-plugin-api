//! Hands each dependent's build script the directory holding
//! `runvil_plugin.h`, as `DEP_RUNVIL_PLUGIN_INCLUDE`, so C or C++ code
//! compiles against the header of the exact version Cargo resolved.

fn main() {
    println!(
        "cargo::metadata=include={}/include",
        env!("CARGO_MANIFEST_DIR")
    );
    println!("cargo::rerun-if-changed=build.rs");
}
