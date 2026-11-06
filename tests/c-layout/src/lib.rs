//! `c-layout` - checks `runvil_plugin.h` with real compilers. The build
//! script compiles the header as C99 and as C++; the tests compare every
//! size, offset, and constant the C compiler sees with the Rust mirror in
//! `runvil-plugin-sys`, and check that C++ sees `runvil_plugin` with C
//! linkage.
//!
//! ## Invariants
//!
//! - The C table in `layout.c` and the Rust table in the tests name the
//!   same entries; a new field or constant joins both.

#[cfg(test)]
#[expect(
    unsafe_code,
    reason = "the tests call the C and C++ functions the build script compiled"
)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::{CStr, c_char};
    use std::mem::offset_of;
    use std::{ptr, slice};

    use runvil_plugin_sys::{
        RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_REFUSED, RV_ERR_UNSUPPORTED, RV_OK,
        RV_PLUGIN_LOAD, RV_PLUGIN_UNLOAD, RvCall, RvHost, RvPluginInfo, RvStatus, RvStr, RvVerb,
    };

    /// One entry of the table in `layout.c`.
    #[repr(C)]
    struct Entry {
        what: *const c_char,
        value: usize,
    }

    unsafe extern "C" {
        /// `layout.c`'s table, and its length through `count`.
        fn rv_layout(count: *mut usize) -> *const Entry;
        /// The C entry point `layout.c` defines against the header.
        fn runvil_plugin(call: *mut RvCall) -> RvStatus;
        /// `sizeof(RvCall)` as C++ sees it.
        safe fn rv_cpp_size_of_call() -> usize;
        /// `runvil_plugin` as C++ names it.
        safe fn rv_cpp_entry() -> unsafe extern "C" fn(*mut RvCall) -> RvStatus;
    }

    /// What the C compiler saw, by the name `layout.c` gives each entry.
    fn c_layout() -> BTreeMap<String, usize> {
        let mut count = 0;
        // SAFETY: `rv_layout` writes the table's length and returns a
        // static table that long.
        let entries = unsafe { slice::from_raw_parts(rv_layout(&raw mut count), count) };
        entries
            .iter()
            .map(|entry| {
                // SAFETY: each `what` is a NUL-terminated string literal.
                let what = unsafe { CStr::from_ptr(entry.what) };
                (what.to_str().unwrap().to_owned(), entry.value)
            })
            .collect()
    }

    /// What Rust sees, named as `layout.c` names each entry.
    fn rust_layout() -> BTreeMap<String, usize> {
        [
            ("sizeof(RvStatus)", size_of::<RvStatus>()),
            ("sizeof(RvVerb)", size_of::<RvVerb>()),
            ("sizeof(RvStr)", size_of::<RvStr>()),
            ("offsetof(RvStr, ptr)", offset_of!(RvStr, ptr)),
            ("offsetof(RvStr, len)", offset_of!(RvStr, len)),
            ("sizeof(RvPluginInfo)", size_of::<RvPluginInfo>()),
            (
                "offsetof(RvPluginInfo, size)",
                offset_of!(RvPluginInfo, size),
            ),
            (
                "offsetof(RvPluginInfo, reserved)",
                offset_of!(RvPluginInfo, reserved),
            ),
            (
                "offsetof(RvPluginInfo, name)",
                offset_of!(RvPluginInfo, name),
            ),
            (
                "offsetof(RvPluginInfo, version)",
                offset_of!(RvPluginInfo, version),
            ),
            ("sizeof(RvHost)", size_of::<RvHost>()),
            ("offsetof(RvHost, size)", offset_of!(RvHost, size)),
            ("offsetof(RvHost, api_major)", offset_of!(RvHost, api_major)),
            ("offsetof(RvHost, api_minor)", offset_of!(RvHost, api_minor)),
            ("offsetof(RvHost, reserved)", offset_of!(RvHost, reserved)),
            ("offsetof(RvHost, refuse)", offset_of!(RvHost, refuse)),
            (
                "offsetof(RvHost, describe_plugin)",
                offset_of!(RvHost, describe_plugin),
            ),
            ("sizeof(RvCall)", size_of::<RvCall>()),
            ("offsetof(RvCall, size)", offset_of!(RvCall, size)),
            ("offsetof(RvCall, verb)", offset_of!(RvCall, verb)),
            ("offsetof(RvCall, host)", offset_of!(RvCall, host)),
            ("offsetof(RvCall, plugin)", offset_of!(RvCall, plugin)),
            ("offsetof(RvCall, pack_run)", offset_of!(RvCall, pack_run)),
            ("offsetof(RvCall, args)", offset_of!(RvCall, args)),
            ("RV_OK", RV_OK as usize),
            ("RV_ERR_MISUSE", RV_ERR_MISUSE as usize),
            ("RV_ERR_REFUSED", RV_ERR_REFUSED as usize),
            ("RV_ERR_UNSUPPORTED", RV_ERR_UNSUPPORTED as usize),
            ("RV_PLUGIN_LOAD", RV_PLUGIN_LOAD as usize),
            ("RV_PLUGIN_UNLOAD", RV_PLUGIN_UNLOAD as usize),
            ("RV_API_MAJOR", RV_API_MAJOR as usize),
            ("RV_API_MINOR", RV_API_MINOR as usize),
        ]
        .into_iter()
        .map(|(what, value)| (what.to_owned(), value))
        .collect()
    }

    #[test]
    fn c_and_rust_agree_on_every_size_offset_and_constant() {
        assert_eq!(c_layout(), rust_layout());
    }

    #[test]
    fn a_c_plugin_defines_the_entry_point_the_header_declares() {
        // SAFETY: the C entry point never reads its argument.
        let status = unsafe { runvil_plugin(ptr::null_mut()) };
        assert_eq!(status, RV_ERR_UNSUPPORTED);
    }

    #[test]
    fn cpp_sees_the_same_call_and_reaches_the_c_entry_point() {
        assert_eq!(rv_cpp_size_of_call(), size_of::<RvCall>());
        // SAFETY: the address is the C entry point, which never reads its
        // argument.
        let status = unsafe { rv_cpp_entry()(ptr::null_mut()) };
        assert_eq!(status, RV_ERR_UNSUPPORTED);
    }
}
