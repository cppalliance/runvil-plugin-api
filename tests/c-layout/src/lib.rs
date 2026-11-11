//! `c-layout` - checks `runvil_plugin.h` with real compilers. The build
//! script compiles the header as C99 and as C++; the tests compare every
//! size, offset, and constant the C compiler sees with the Rust mirror in
//! `runvil-plugin-sys`, and check that C++ sees `runvil_plugin` with C
//! linkage.
//!
//! ## Invariants
//!
//! - The C table in `layout.c` and the Rust table in the tests name the
//!   same entries, line for line; a new field or constant joins both.

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
        RV_API_MAJOR, RV_API_MINOR, RV_ERR_FILE, RV_ERR_MISUSE, RV_ERR_REFUSED, RV_ERR_UNSUPPORTED,
        RV_FILE_ERROR_CONFLICT, RV_FILE_ERROR_DENIED, RV_FILE_ERROR_IO,
        RV_FILE_ERROR_IS_A_DIRECTORY, RV_FILE_ERROR_NOT_A_DIRECTORY, RV_FILE_ERROR_NOT_FOUND,
        RV_FILE_ERROR_OTHER, RV_FILE_TYPE_DIRECTORY, RV_FILE_TYPE_FILE, RV_FILE_TYPE_OTHER,
        RV_FILE_TYPE_SYMLINK, RV_MAX_LINE, RV_MAX_NAME, RV_MAX_PATH, RV_MAX_RESULT,
        RV_MAX_TOOL_TEXT, RV_OK, RV_PACK_DELETE, RV_PACK_NEW, RV_PENDING, RV_PLUGIN_LOAD,
        RV_PLUGIN_UNLOAD, RV_RESULT_BACKEND, RV_RESULT_INVALID_ARGUMENTS, RV_RESULT_OTHER,
        RV_RESULT_TRANSPORT, RV_RESULT_TRUSTED, RV_RESULT_UNTRUSTED, RV_TOOL_CALL, RV_TOOL_CANCEL,
        RvBytes, RvCall, RvDirEntries, RvDirEntry, RvFileError, RvFileErrorKind, RvFileType,
        RvHost, RvLoadArgs, RvPackInfo, RvPackNewArgs, RvPluginInfo, RvResultKind, RvStat,
        RvStatus, RvStr, RvToolCallArgs, RvToolInfo, RvToolResult, RvVerb,
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

    /// A type's size, named as `layout.c`'s `SIZE` names it.
    macro_rules! size {
        ($type:ty) => {
            (
                concat!("sizeof(", stringify!($type), ")"),
                size_of::<$type>(),
            )
        };
    }

    /// A field's offset, named as `layout.c`'s `OFFSET` names it.
    macro_rules! offset {
        ($type:ty, $field:ident) => {
            (
                concat!(
                    "offsetof(",
                    stringify!($type),
                    ", ",
                    stringify!($field),
                    ")"
                ),
                offset_of!($type, $field),
            )
        };
    }

    /// A constant's value, named as `layout.c`'s `VALUE` names it.
    macro_rules! value {
        ($constant:ident) => {
            (stringify!($constant), $constant as usize)
        };
    }

    /// What Rust sees, in the order of the table in `layout.c`.
    fn rust_layout() -> BTreeMap<String, usize> {
        [data_structs(), call_structs(), constants()]
            .into_iter()
            .flatten()
            .map(|(what, value)| (what.to_owned(), value))
            .collect()
    }

    /// The first part of the table: the structs the two sides hand each
    /// other.
    fn data_structs() -> Vec<(&'static str, usize)> {
        vec![
            size!(RvStatus),
            size!(RvVerb),
            size!(RvStr),
            offset!(RvStr, ptr),
            offset!(RvStr, len),
            size!(RvBytes),
            offset!(RvBytes, ptr),
            offset!(RvBytes, len),
            size!(RvFileType),
            size!(RvStat),
            offset!(RvStat, file_type),
            offset!(RvStat, reserved),
            offset!(RvStat, len),
            size!(RvDirEntry),
            offset!(RvDirEntry, name),
            offset!(RvDirEntry, stat),
            size!(RvDirEntries),
            offset!(RvDirEntries, ptr),
            offset!(RvDirEntries, len),
            size!(RvFileErrorKind),
            size!(RvFileError),
            offset!(RvFileError, kind),
            offset!(RvFileError, reserved),
            offset!(RvFileError, text),
            size!(RvPluginInfo),
            offset!(RvPluginInfo, size),
            offset!(RvPluginInfo, reserved),
            offset!(RvPluginInfo, name),
            offset!(RvPluginInfo, version),
            size!(RvPackInfo),
            offset!(RvPackInfo, size),
            offset!(RvPackInfo, reserved),
            offset!(RvPackInfo, name),
            offset!(RvPackInfo, description),
            size!(RvToolInfo),
            offset!(RvToolInfo, size),
            offset!(RvToolInfo, reserved),
            offset!(RvToolInfo, name),
            offset!(RvToolInfo, wire_name),
            offset!(RvToolInfo, description),
            offset!(RvToolInfo, parameters),
            size!(RvResultKind),
            size!(RvToolResult),
            offset!(RvToolResult, size),
            offset!(RvToolResult, kind),
            offset!(RvToolResult, text),
        ]
    }

    /// The second part of the table: the Host, each verb's arguments, and
    /// the call.
    fn call_structs() -> Vec<(&'static str, usize)> {
        vec![
            size!(RvHost),
            offset!(RvHost, size),
            offset!(RvHost, api_major),
            offset!(RvHost, api_minor),
            offset!(RvHost, reserved),
            size!(RvLoadArgs),
            offset!(RvLoadArgs, size),
            offset!(RvLoadArgs, reserved),
            offset!(RvLoadArgs, ctx),
            offset!(RvLoadArgs, refuse),
            offset!(RvLoadArgs, describe_plugin),
            offset!(RvLoadArgs, insert_pack),
            size!(RvPackNewArgs),
            offset!(RvPackNewArgs, size),
            offset!(RvPackNewArgs, reserved),
            offset!(RvPackNewArgs, ctx),
            offset!(RvPackNewArgs, pack),
            offset!(RvPackNewArgs, refuse),
            offset!(RvPackNewArgs, add_tool),
            size!(RvToolCallArgs),
            offset!(RvToolCallArgs, size),
            offset!(RvToolCallArgs, reserved),
            offset!(RvToolCallArgs, ctx),
            offset!(RvToolCallArgs, tool),
            offset!(RvToolCallArgs, arguments),
            offset!(RvToolCallArgs, finish),
            offset!(RvToolCallArgs, file_stat),
            offset!(RvToolCallArgs, file_list),
            offset!(RvToolCallArgs, file_read),
            size!(RvCall),
            offset!(RvCall, size),
            offset!(RvCall, verb),
            offset!(RvCall, host),
            offset!(RvCall, plugin),
            offset!(RvCall, pack_run),
            offset!(RvCall, args),
            offset!(RvCall, tool_call),
        ]
    }

    /// The last part of the table: the constants.
    fn constants() -> Vec<(&'static str, usize)> {
        vec![
            value!(RV_OK),
            value!(RV_ERR_MISUSE),
            value!(RV_ERR_REFUSED),
            value!(RV_ERR_UNSUPPORTED),
            value!(RV_PENDING),
            value!(RV_ERR_FILE),
            value!(RV_PLUGIN_LOAD),
            value!(RV_PLUGIN_UNLOAD),
            value!(RV_PACK_NEW),
            value!(RV_PACK_DELETE),
            value!(RV_TOOL_CALL),
            value!(RV_TOOL_CANCEL),
            value!(RV_RESULT_UNTRUSTED),
            value!(RV_RESULT_TRUSTED),
            value!(RV_RESULT_INVALID_ARGUMENTS),
            value!(RV_RESULT_BACKEND),
            value!(RV_RESULT_TRANSPORT),
            value!(RV_RESULT_OTHER),
            value!(RV_FILE_TYPE_OTHER),
            value!(RV_FILE_TYPE_FILE),
            value!(RV_FILE_TYPE_DIRECTORY),
            value!(RV_FILE_TYPE_SYMLINK),
            value!(RV_FILE_ERROR_OTHER),
            value!(RV_FILE_ERROR_NOT_FOUND),
            value!(RV_FILE_ERROR_NOT_A_DIRECTORY),
            value!(RV_FILE_ERROR_IS_A_DIRECTORY),
            value!(RV_FILE_ERROR_DENIED),
            value!(RV_FILE_ERROR_CONFLICT),
            value!(RV_FILE_ERROR_IO),
            value!(RV_API_MAJOR),
            value!(RV_API_MINOR),
            value!(RV_MAX_NAME),
            value!(RV_MAX_LINE),
            value!(RV_MAX_PATH),
            value!(RV_MAX_TOOL_TEXT),
            value!(RV_MAX_RESULT),
        ]
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
