//! Compiles `layout.c` and `layout.cpp` against the `runvil_plugin.h` that
//! `runvil-plugin-sys` hands over, with warnings as errors, and links
//! both into this crate's tests.

use std::path::PathBuf;

fn main() {
    let Some(include) = std::env::var_os("DEP_RUNVIL_PLUGIN_INCLUDE").map(PathBuf::from) else {
        panic!("runvil-plugin-sys did not set DEP_RUNVIL_PLUGIN_INCLUDE");
    };
    println!("cargo::rerun-if-changed=layout.c");
    println!("cargo::rerun-if-changed=layout.cpp");
    println!(
        "cargo::rerun-if-changed={}",
        include.join("runvil_plugin.h").display()
    );
    cc::Build::new()
        .file("layout.c")
        .include(&include)
        .warnings_into_errors(true)
        .flag_if_supported("-std=c99")
        .flag_if_supported("-pedantic")
        .compile("c_layout");
    cc::Build::new()
        .cpp(true)
        .file("layout.cpp")
        .include(&include)
        .warnings_into_errors(true)
        .flag_if_supported("-pedantic")
        .compile("cpp_layout");
}
