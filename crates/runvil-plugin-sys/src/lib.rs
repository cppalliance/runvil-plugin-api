//! The C contract every Plugin entry point follows, library or linked.
//! `runvil_plugin.h` declares it for C and C++, and the `runvil-plugin-sys`
//! crate mirrors it for Rust; `cargo xtask header` writes the header from
//! the crate's source, comments included, so the two never disagree.
//!
//! A Plugin is one entry point, with the shape of `runvil_plugin` (in
//! Rust, [`PluginEntry`]). A library Plugin exports it under that name; a
//! linked Plugin has the same shape under a name of its own crate's
//! choosing. Declaring the types is safe Rust; only calling an entry point
//! or reading through its pointers needs `unsafe`.
//!
//! # The contract
//!
//! - Every call passes an [`RvCall`] that names a verb. Loading returns the
//!   Plugin's instance pointer, and every later call passes it back.
//! - [`RvHost`] holds only what is true on every call: the contract version
//!   the Host speaks. Each function a Plugin may call back belongs to one
//!   verb and sits in that verb's arguments struct, behind `call->args`,
//!   beside the `ctx` pointer it takes first. For [`RV_PLUGIN_LOAD`] that
//!   struct is [`RvLoadArgs`].
//! - Strings are a pointer plus a length, UTF-8, not NUL-terminated.
//! - Verb names follow `RV_<OBJECT>_<ACTION>`. Libraries open and close;
//!   Plugins load and unload.
//!
//! # Versions
//!
//! - [`RV_API_MAJOR`] and [`RV_API_MINOR`] name the contract's version, and
//!   the `runvil-plugin-sys` crate's version is the same number.
//! - A 0.x version makes no compatibility promise. Each one is a contract
//!   of its own, and a Plugin built for one loads only in a Host whose
//!   [`RvHost`] reports exactly that version.
//! - From 1.0, a minor version only adds: a field at the end of a struct,
//!   such as a function at the end of a verb's arguments struct, whose
//!   `size` field shows that it is there, or a verb with an arguments
//!   struct of its own. A Plugin built for 1.n loads in a Host that reports
//!   major version 1 and a minor version of at least n.
//!
//! # Threads and lifetime
//!
//! - [`RV_PLUGIN_LOAD`] runs on the thread of the insert call. The
//!   functions in its [`RvLoadArgs`] answer only while that call runs, only
//!   on its thread, and only for its `ctx`. Any other use, including during
//!   [`RV_PLUGIN_UNLOAD`], returns [`RV_ERR_MISUSE`].
//! - The Host keeps each `ctx` valid while the Plugin is loaded, until
//!   [`RV_PLUGIN_UNLOAD`] returns, so a late call through it is answered
//!   [`RV_ERR_MISUSE`] instead of reaching freed memory. The `ctx` of a load
//!   the loader rejects is gone once that load returns, and the Plugin must
//!   not use it.
//! - [`RV_PLUGIN_UNLOAD`] may come on any thread, because the last holder
//!   of the Plugin drops it. The instance must be freeable from a thread
//!   other than the one that loaded it.
//! - [`RV_PLUGIN_UNLOAD`] must stop every thread the Plugin started and
//!   release everything it holds before it returns. A library Plugin's
//!   library closes right after, and a thread still running crashes the
//!   process.
//! - A load call the loader rejects gets no [`RV_PLUGIN_UNLOAD`]: a
//!   refusal, a recorded misuse, a non-OK status, or no description, even
//!   when the Plugin returned [`RV_OK`]. The Plugin frees whatever it made
//!   before returning on every such path. A load call that succeeds is
//!   unloaded later, even if the loader then drops the Plugin, for example
//!   because its name repeats one already loaded.
//!
//! # Reserved verbs
//!
//! Verbs 3 to 8 are reserved for later versions, so their numbers never
//! change. Each one gets its own arguments struct behind `call->args`, so
//! no verb reads fields meant for another, and that struct holds the
//! functions the verb may call back, under the verb's own lifetime and
//! thread rules. No constants are declared for them yet.
//!
//! - `RV_PACK_NEW` (3): make one Pack's per-run instance and set
//!   `call->pack_run`.
//! - `RV_PACK_DELETE` (4): free that per-run instance.
//! - `RV_TOOL_CALL` (5): start one tool call, which finishes later through
//!   a function in its arguments struct, callable after the verb returns.
//! - `RV_TOOL_CANCEL` (6): abandon a started call.
//! - `RV_PRELUDE_INSTALL` (7): install the Pack's native prelude, once per
//!   Lua VM.
//! - `RV_PACK_RENDER` (8): render the tools a section offered from one
//!   Pack for one model, which finishes later through a function in its
//!   arguments struct.
//!
//! # Status codes
//!
//! - [`RV_OK`] (0) is success.
//! - Codes 1 to 3 are reserved for the Lua API's own errors: Lua raised an
//!   error, a fatal error such as the memory limit or a cancel, and a wrong
//!   type.
//! - [`RV_ERR_MISUSE`] (4) means the caller broke the contract.
//! - [`RV_ERR_REFUSED`] (5) means the Plugin declined to load.
//! - [`RV_ERR_UNSUPPORTED`] (6) means the Plugin doesn't know the verb.
//!
//! ## Invariants
//!
//! These are about the Rust crate, so the header's comment stops before
//! them.
//!
//! - No dependencies and no `unsafe`: this crate only declares.
//! - `include/runvil_plugin.h` is generated from this file and never
//!   edited by hand; `cargo test -p build-xtask` fails when it is stale.
//! - Status codes and verbs keep their numbers in every version; the unit
//!   tests pin them.

#![cfg_attr(not(test), no_std)]

use core::ffi::{c_char, c_void};

/// What every function in the contract returns: zero for success.
pub type RvStatus = u32;
/// Success.
pub const RV_OK: RvStatus = 0;
/// The caller broke the contract.
pub const RV_ERR_MISUSE: RvStatus = 4;
/// The Plugin declined to load; `refuse` returns it.
pub const RV_ERR_REFUSED: RvStatus = 5;
/// The Plugin does not know the verb it was sent.
pub const RV_ERR_UNSUPPORTED: RvStatus = 6;

/// The operation an [`RvCall`] asks for. A fixed-width integer, not a C
/// enum, so its size never depends on the compiler.
pub type RvVerb = u32;
/// Make the Plugin instance and name the Plugin.
pub const RV_PLUGIN_LOAD: RvVerb = 1;
/// Free the Plugin instance.
pub const RV_PLUGIN_UNLOAD: RvVerb = 2;

/// The contract's major version, which [`RvHost`] reports.
pub const RV_API_MAJOR: u32 = 0;
/// The contract's minor version, which [`RvHost`] reports.
pub const RV_API_MINOR: u32 = 2;

/// A UTF-8 string that is not NUL-terminated.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvStr {
    /// The first byte.
    pub ptr: *const c_char,
    /// The length in bytes.
    pub len: usize,
}

/// What a Plugin tells `describe_plugin` about itself.
#[repr(C)]
#[derive(Debug)]
pub struct RvPluginInfo {
    /// `size_of::<RvPluginInfo>()`; in C, `sizeof(RvPluginInfo)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// 1 to 64 bytes of lowercase ASCII letters, digits, `-`, `_`, and `.`:
    /// one Pack id segment, such as `acme-url`.
    pub name: RvStr,
    /// Non-empty, at most 64 bytes, with no control characters.
    pub version: RvStr,
}

/// What the Host reports on every call: the contract version it speaks.
/// The functions a Plugin may call back sit in each verb's arguments
/// struct instead.
#[repr(C)]
#[derive(Debug)]
pub struct RvHost {
    /// `size_of::<RvHost>()`; in C, `sizeof(RvHost)`.
    pub size: u32,
    /// [`RV_API_MAJOR`].
    pub api_major: u32,
    /// [`RV_API_MINOR`].
    pub api_minor: u32,
    /// Zero.
    pub reserved: u32,
}

/// The arguments of [`RV_PLUGIN_LOAD`]: the functions the Plugin may call
/// while it loads, each taking `ctx` first.
#[repr(C)]
#[derive(Debug)]
pub struct RvLoadArgs {
    /// `size_of::<RvLoadArgs>()`; in C, `sizeof(RvLoadArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this load, passed back as the first argument
    /// of each function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// Declines the load, at most once, with a UTF-8 reason of at most 1024
    /// bytes; returns [`RV_ERR_REFUSED`].
    pub refuse: unsafe extern "C" fn(ctx: *mut c_void, reason: RvStr) -> RvStatus,
    /// Names the Plugin, exactly once.
    pub describe_plugin:
        unsafe extern "C" fn(ctx: *mut c_void, info: *const RvPluginInfo) -> RvStatus,
}

/// One call into a Plugin's entry point.
#[repr(C)]
#[derive(Debug)]
pub struct RvCall {
    /// `size_of::<RvCall>()`; in C, `sizeof(RvCall)`.
    pub size: u32,
    /// The operation.
    pub verb: RvVerb,
    /// What the Host reports on every call.
    pub host: *const RvHost,
    /// The Plugin instance: NULL going into [`RV_PLUGIN_LOAD`], which may
    /// set it.
    pub plugin: *mut c_void,
    /// The per-run Pack instance; NULL until the Pack verbs exist.
    pub pack_run: *mut c_void,
    /// The verb's own arguments: an [`RvLoadArgs`] for [`RV_PLUGIN_LOAD`],
    /// and NULL for [`RV_PLUGIN_UNLOAD`].
    pub args: *const c_void,
}

/// The shape of every Plugin entry point. The header declares the one a
/// library Plugin exports, `runvil_plugin`, instead of this type.
///
/// The entry point must not unwind: a Rust panic across `extern "C"`
/// aborts the process.
pub type PluginEntry = unsafe extern "C" fn(*mut RvCall) -> RvStatus;

#[cfg(test)]
mod tests {
    //! The contract's layout on 64-bit targets, the numbers no version may
    //! change, and the crate version that names the contract's version.

    #[cfg(target_pointer_width = "64")]
    use core::mem::{offset_of, size_of};

    use super::{
        RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_REFUSED, RV_ERR_UNSUPPORTED, RV_OK,
        RV_PLUGIN_LOAD, RV_PLUGIN_UNLOAD,
    };
    #[cfg(target_pointer_width = "64")]
    use super::{RvCall, RvHost, RvLoadArgs, RvPluginInfo, RvStr};

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn a_string_is_a_pointer_then_a_length() {
        assert_eq!(size_of::<RvStr>(), 16);
        assert_eq!(offset_of!(RvStr, ptr), 0);
        assert_eq!(offset_of!(RvStr, len), 8);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_plugin_info_keeps_its_layout() {
        assert_eq!(size_of::<RvPluginInfo>(), 40);
        assert_eq!(offset_of!(RvPluginInfo, size), 0);
        assert_eq!(offset_of!(RvPluginInfo, reserved), 4);
        assert_eq!(offset_of!(RvPluginInfo, name), 8);
        assert_eq!(offset_of!(RvPluginInfo, version), 24);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_host_keeps_its_layout() {
        assert_eq!(size_of::<RvHost>(), 16);
        assert_eq!(offset_of!(RvHost, size), 0);
        assert_eq!(offset_of!(RvHost, api_major), 4);
        assert_eq!(offset_of!(RvHost, api_minor), 8);
        assert_eq!(offset_of!(RvHost, reserved), 12);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_load_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvLoadArgs>(), 32);
        assert_eq!(offset_of!(RvLoadArgs, size), 0);
        assert_eq!(offset_of!(RvLoadArgs, reserved), 4);
        assert_eq!(offset_of!(RvLoadArgs, ctx), 8);
        assert_eq!(offset_of!(RvLoadArgs, refuse), 16);
        assert_eq!(offset_of!(RvLoadArgs, describe_plugin), 24);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_call_keeps_its_layout() {
        assert_eq!(size_of::<RvCall>(), 40);
        assert_eq!(offset_of!(RvCall, size), 0);
        assert_eq!(offset_of!(RvCall, verb), 4);
        assert_eq!(offset_of!(RvCall, host), 8);
        assert_eq!(offset_of!(RvCall, plugin), 16);
        assert_eq!(offset_of!(RvCall, pack_run), 24);
        assert_eq!(offset_of!(RvCall, args), 32);
    }

    #[test]
    fn the_status_codes_keep_their_numbers() {
        assert_eq!(RV_OK, 0);
        assert_eq!(RV_ERR_MISUSE, 4);
        assert_eq!(RV_ERR_REFUSED, 5);
        assert_eq!(RV_ERR_UNSUPPORTED, 6);
    }

    #[test]
    fn the_verbs_keep_their_numbers() {
        assert_eq!(RV_PLUGIN_LOAD, 1);
        assert_eq!(RV_PLUGIN_UNLOAD, 2);
    }

    #[test]
    fn the_crate_version_is_the_contract_version() {
        assert_eq!(env!("CARGO_PKG_VERSION_MAJOR"), RV_API_MAJOR.to_string());
        assert_eq!(env!("CARGO_PKG_VERSION_MINOR"), RV_API_MINOR.to_string());
    }
}
