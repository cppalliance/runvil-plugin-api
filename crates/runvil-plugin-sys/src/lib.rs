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
//! - Every call passes an [`RvCall`] that names a verb. The call carries
//!   three pointers that belong to the Plugin, which the Host stores and
//!   passes back and never reads through: `plugin`, the Plugin instance;
//!   `pack_run`, one run's instance of a Pack; and `tool_call`, the state
//!   one tool call keeps between its steps.
//! - [`RvHost`] holds only what is true on every call: the contract version
//!   the Host speaks. Each function a Plugin may call back belongs to one
//!   verb and sits in that verb's arguments struct, behind `call->args`,
//!   beside the `ctx` pointer it takes first.
//! - Strings are a pointer plus a length, UTF-8, not NUL-terminated.
//! - A struct or a string that one side passes to the other is valid only
//!   until the function it was passed to returns. The receiver copies what
//!   it keeps.
//! - Verb names follow `RV_<OBJECT>_<ACTION>`. Libraries open and close;
//!   Plugins load and unload.
//!
//! # Verbs
//!
//! - [`RV_PLUGIN_LOAD`] (1), with an [`RvLoadArgs`]: makes the Plugin
//!   instance and sets `call->plugin`, names the Plugin once with
//!   `describe_plugin`, and inserts each of the Plugin's Packs with
//!   `insert_pack`.
//! - [`RV_PLUGIN_UNLOAD`] (2), with no arguments: frees `call->plugin`.
//! - [`RV_PACK_NEW`] (3), with an [`RvPackNewArgs`]: makes one run's
//!   instance of a Pack and sets `call->pack_run`, and lists that
//!   instance's tools with `add_tool`.
//! - [`RV_PACK_DELETE`] (4), with no arguments: frees `call->pack_run`.
//! - [`RV_TOOL_CALL`] (5), with an [`RvToolCallArgs`]: runs one step of a
//!   tool call on `call->pack_run`.
//! - [`RV_TOOL_CANCEL`] (6), with no arguments: frees `call->tool_call`,
//!   the state of a tool call the Host has abandoned.
//!
//! A Plugin answers a verb it does not know with [`RV_ERR_UNSUPPORTED`].
//!
//! # Packs
//!
//! - A Plugin inserts each of its Packs once, while it loads. The Host
//!   makes one instance of a Pack for each run that holds the Pack, with
//!   [`RV_PACK_NEW`], and frees the instance with [`RV_PACK_DELETE`] when
//!   the run ends.
//! - An instance's tools are the ones it adds during [`RV_PACK_NEW`], and
//!   they stay fixed for the run. The Host forms each tool's id from the
//!   Pack's id, which is the second segment of the Pack's name, and the
//!   tool's name: the tool `fetch` of the Pack `acme/url` is `url/fetch`.
//! - [`RV_PACK_NEW`] runs while the Host prepares a run, so it returns
//!   promptly: it does not block and does no I/O. What is costly to make
//!   belongs in the Plugin instance, which every run shares.
//!
//! # Tool calls
//!
//! - A tool call is a series of steps. The Host starts the call by sending
//!   [`RV_TOOL_CALL`] with `call->tool_call` NULL, and that step may set
//!   `call->tool_call` to state of its own. A step that returns
//!   [`RV_PENDING`] asks for another step: the Host sends [`RV_TOOL_CALL`]
//!   again with the same `tool`, the same `arguments`, and the
//!   `call->tool_call` the last step left.
//! - A step ends the call by calling `finish` once and returning
//!   [`RV_OK`]. Any return other than [`RV_PENDING`] ends the call, and a
//!   call that ends without a result fails.
//! - The Host may abandon a call between two steps, for example when its
//!   run is canceled. It then sends [`RV_TOOL_CANCEL`] once, with the
//!   `call->tool_call` the last step left, and no further step.
//! - A step does a bounded slice of work and returns. A step that works
//!   for long, or blocks, stalls its run, and a run can be canceled only
//!   between steps. [`RV_PENDING`] is not a way to wait: the Host sends the
//!   next step as soon as the run's other work has had a turn.
//!
//! # Threads and lifetime
//!
//! - [`RV_PLUGIN_LOAD`] runs on the thread of the insert call. Every other
//!   verb may come on any thread, and the steps of one tool call may come
//!   on different threads.
//! - Calls may run at the same time on different threads, with three
//!   exceptions. No two calls that name the same `tool_call` run at the
//!   same time. [`RV_PACK_DELETE`] comes after every tool call of its
//!   `pack_run` has ended, and runs beside no call that names it.
//!   [`RV_PLUGIN_UNLOAD`] comes after every Pack instance of the Plugin is
//!   deleted, and runs beside no other call.
//! - The functions in a verb's arguments struct answer only while that
//!   call runs, only on its thread, and only for its `ctx`. Any other use
//!   returns [`RV_ERR_MISUSE`], and so does a function called against its
//!   own rules, such as `describe_plugin` called twice. A misuse during a
//!   call fails that call, whatever status the Plugin returns.
//! - The Host keeps the `ctx` of a load valid while the Plugin is loaded,
//!   until [`RV_PLUGIN_UNLOAD`] returns, so a late call through it is
//!   answered [`RV_ERR_MISUSE`] instead of reaching freed memory. The `ctx`
//!   of a load the Host rejects, and the `ctx` of every other call, is
//!   gone once the call returns, and the Plugin must not use it.
//! - [`RV_PLUGIN_UNLOAD`] must stop every thread the Plugin started and
//!   release everything it holds before it returns. A library Plugin's
//!   library closes right after, and a thread still running crashes the
//!   process.
//!
//! # What the Plugin frees
//!
//! - A load the Host rejects gets no [`RV_PLUGIN_UNLOAD`]: a refusal, a
//!   misuse, a status other than [`RV_OK`], or no description. The Plugin
//!   frees whatever it made before it returns on every such path. A load
//!   that succeeds is unloaded later, even if the Host then drops the
//!   Plugin, for example because its name repeats one already loaded.
//! - An [`RV_PACK_NEW`] the Host rejects gets no [`RV_PACK_DELETE`]: a
//!   refusal, a misuse, or a status other than [`RV_OK`]. The Plugin frees
//!   whatever it made before it returns on every such path. One that
//!   succeeds gets exactly one [`RV_PACK_DELETE`].
//! - The step that ends a tool call frees the call's state before it
//!   returns. A call that is abandoned gets [`RV_TOOL_CANCEL`] instead,
//!   which frees the state; its `call->tool_call` is NULL when no step set
//!   it. Each call gets one or the other, never both.
//!
//! # Reserved verbs
//!
//! Verbs 7 and 8 are reserved for later versions, so their numbers never
//! change. Each one gets its own arguments struct behind `call->args`. No
//! constants are declared for them yet.
//!
//! - `RV_PRELUDE_INSTALL` (7): install the Pack's native prelude, once per
//!   Lua VM.
//! - `RV_PACK_RENDER` (8): render the tools a section offered from one
//!   Pack for one model.
//!
//! # Status codes
//!
//! - [`RV_OK`] (0) is success.
//! - Codes 1 to 3 are reserved for the Lua API's own errors: Lua raised an
//!   error, a fatal error such as the memory limit or a cancel, and a wrong
//!   type.
//! - [`RV_ERR_MISUSE`] (4) means the caller broke the contract.
//! - [`RV_ERR_REFUSED`] (5) means the Plugin declined: to load, or to make
//!   a Pack instance.
//! - [`RV_ERR_UNSUPPORTED`] (6) means the Plugin doesn't know the verb.
//! - [`RV_PENDING`] (7) means a tool call needs another step. Only
//!   [`RV_TOOL_CALL`] returns it.
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
//! ## Invariants
//!
//! These are about the Rust crate, so the header's comment stops before
//! them.
//!
//! - No dependencies and no `unsafe`: this crate only declares.
//! - `include/runvil_plugin.h` is generated from this file and never
//!   edited by hand; `cargo test -p build-xtask` fails when it is stale.
//! - Status codes, verbs, and result kinds keep their numbers in every
//!   version; the unit tests pin them.

#![cfg_attr(not(test), no_std)]

use core::ffi::{c_char, c_void};

/// What every function in the contract returns: zero for success.
pub type RvStatus = u32;
/// Success.
pub const RV_OK: RvStatus = 0;
/// The caller broke the contract.
pub const RV_ERR_MISUSE: RvStatus = 4;
/// The Plugin declined to load or to make a Pack instance; `refuse`
/// returns it.
pub const RV_ERR_REFUSED: RvStatus = 5;
/// The Plugin does not know the verb it was sent.
pub const RV_ERR_UNSUPPORTED: RvStatus = 6;
/// The tool call needs another step; only [`RV_TOOL_CALL`] returns it.
pub const RV_PENDING: RvStatus = 7;

/// The operation an [`RvCall`] asks for. A fixed-width integer, not a C
/// enum, so its size never depends on the compiler.
pub type RvVerb = u32;
/// Make the Plugin instance, name the Plugin, and insert its Packs.
pub const RV_PLUGIN_LOAD: RvVerb = 1;
/// Free the Plugin instance.
pub const RV_PLUGIN_UNLOAD: RvVerb = 2;
/// Make one run's instance of a Pack and list its tools.
pub const RV_PACK_NEW: RvVerb = 3;
/// Free one run's instance of a Pack.
pub const RV_PACK_DELETE: RvVerb = 4;
/// Run one step of a tool call.
pub const RV_TOOL_CALL: RvVerb = 5;
/// Free the state of a tool call the Host has abandoned.
pub const RV_TOOL_CANCEL: RvVerb = 6;

/// How a tool call ended, in an [`RvToolResult`]. A fixed-width integer,
/// as [`RvVerb`] is.
pub type RvResultKind = u32;
/// The call succeeded, and its text may hold data an attacker can
/// influence, such as the contents of a file or a web page. The Host marks
/// such text before a model reads it. Zero, so a result that names no kind
/// is treated with care.
pub const RV_RESULT_UNTRUSTED: RvResultKind = 0;
/// The call succeeded, and the Plugin vouches for every byte of its text.
pub const RV_RESULT_TRUSTED: RvResultKind = 1;
/// The call failed: its arguments do not match the tool's parameters.
pub const RV_RESULT_INVALID_ARGUMENTS: RvResultKind = 2;
/// The call failed: what the tool reaches refused or failed the request.
pub const RV_RESULT_BACKEND: RvResultKind = 3;
/// The call failed: the request never got through.
pub const RV_RESULT_TRANSPORT: RvResultKind = 4;
/// The call failed for any other reason.
pub const RV_RESULT_OTHER: RvResultKind = 5;

/// The contract's major version, which [`RvHost`] reports.
pub const RV_API_MAJOR: u32 = 0;
/// The contract's minor version, which [`RvHost`] reports.
pub const RV_API_MINOR: u32 = 3;

/// A UTF-8 string that is not NUL-terminated.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RvStr {
    /// The first byte. It may be NULL when `len` is zero.
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
    /// one name segment, such as `acme-url`.
    pub name: RvStr,
    /// Non-empty, at most 64 bytes, with no control characters.
    pub version: RvStr,
}

/// What a Plugin tells `insert_pack` about one of its Packs.
#[repr(C)]
#[derive(Debug)]
pub struct RvPackInfo {
    /// `size_of::<RvPackInfo>()`; in C, `sizeof(RvPackInfo)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Pack's name: a vendor and a Pack id joined by `/`, such as
    /// `acme/url`. Each is one name segment, as a Plugin's name is.
    pub name: RvStr,
    /// One line saying what the Pack offers: at most 1024 bytes with no
    /// control characters. It may be empty.
    pub description: RvStr,
}

/// What a Pack instance tells `add_tool` about one of its tools.
#[repr(C)]
#[derive(Debug)]
pub struct RvToolInfo {
    /// `size_of::<RvToolInfo>()`; in C, `sizeof(RvToolInfo)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The tool's name within its Pack: one name segment, such as `fetch`.
    pub name: RvStr,
    /// The name a model calls the tool by: 1 to 64 bytes of ASCII letters,
    /// digits, `_`, and `-`. Empty lets the Host derive it from the tool's
    /// id, as `url_fetch` from `url/fetch`.
    pub wire_name: RvStr,
    /// What the tool does, for a model to read: non-empty, at most 65536
    /// bytes.
    pub description: RvStr,
    /// The tool's parameters: the JSON text of a JSON Schema whose value is
    /// an object, at most 65536 bytes.
    pub parameters: RvStr,
}

/// How a tool call ended, which a step tells `finish`.
#[repr(C)]
#[derive(Debug)]
pub struct RvToolResult {
    /// `size_of::<RvToolResult>()`; in C, `sizeof(RvToolResult)`.
    pub size: u32,
    /// Whether the call succeeded, and if so how far its text is trusted,
    /// or else why it failed: one of the `RV_RESULT_` constants.
    pub kind: RvResultKind,
    /// The call's output when it succeeded, and otherwise a message a model
    /// can read that says what went wrong: at most 16777216 bytes.
    pub text: RvStr,
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
    /// Inserts one Pack, once for each Pack the Plugin offers. No two
    /// Packs of one Plugin share a name.
    pub insert_pack: unsafe extern "C" fn(ctx: *mut c_void, info: *const RvPackInfo) -> RvStatus,
}

/// The arguments of [`RV_PACK_NEW`]: the Pack to make an instance of, and
/// the functions the Plugin may call while it makes one, each taking `ctx`
/// first.
#[repr(C)]
#[derive(Debug)]
pub struct RvPackNewArgs {
    /// `size_of::<RvPackNewArgs>()`; in C, `sizeof(RvPackNewArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this call, passed back as the first argument
    /// of each function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// The name of the Pack, as the Plugin gave it to `insert_pack`.
    pub pack: RvStr,
    /// Declines to make the instance, at most once, with a UTF-8 reason of
    /// at most 1024 bytes that a model can read; returns
    /// [`RV_ERR_REFUSED`]. The run then goes without the Pack.
    pub refuse: unsafe extern "C" fn(ctx: *mut c_void, reason: RvStr) -> RvStatus,
    /// Adds one tool to the instance, once for each tool it offers. No two
    /// tools of one instance share a name.
    pub add_tool: unsafe extern "C" fn(ctx: *mut c_void, info: *const RvToolInfo) -> RvStatus,
}

/// The arguments of [`RV_TOOL_CALL`]: the tool, its arguments, and the
/// function that ends the call. Every step of one call gets the same
/// `tool` and `arguments`.
#[repr(C)]
#[derive(Debug)]
pub struct RvToolCallArgs {
    /// `size_of::<RvToolCallArgs>()`; in C, `sizeof(RvToolCallArgs)`.
    pub size: u32,
    /// Zero.
    pub reserved: u32,
    /// The Host's context for this step, passed back as the first argument
    /// of the function below. The Plugin never reads through it.
    pub ctx: *mut c_void,
    /// The tool's name, as the instance gave it to `add_tool`.
    pub tool: RvStr,
    /// The call's arguments: the JSON text of the value the caller passed.
    pub arguments: RvStr,
    /// Ends the call with its result, exactly once, in the step that then
    /// returns [`RV_OK`].
    pub finish: unsafe extern "C" fn(ctx: *mut c_void, result: *const RvToolResult) -> RvStatus,
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
    /// The Plugin instance: NULL going into [`RV_PLUGIN_LOAD`], which sets
    /// it, and that pointer on every later call.
    pub plugin: *mut c_void,
    /// One run's instance of a Pack: NULL going into [`RV_PACK_NEW`],
    /// which sets it, that pointer on [`RV_PACK_DELETE`] and on the
    /// instance's tool calls, and NULL otherwise.
    pub pack_run: *mut c_void,
    /// The verb's own arguments: an [`RvLoadArgs`] for [`RV_PLUGIN_LOAD`],
    /// an [`RvPackNewArgs`] for [`RV_PACK_NEW`], an [`RvToolCallArgs`] for
    /// [`RV_TOOL_CALL`], and NULL for every other verb.
    pub args: *const c_void,
    /// The state of one tool call: NULL going into the call's first
    /// [`RV_TOOL_CALL`], what the last step left on each later step and on
    /// [`RV_TOOL_CANCEL`], and NULL otherwise.
    pub tool_call: *mut c_void,
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
        RV_PACK_DELETE, RV_PACK_NEW, RV_PENDING, RV_PLUGIN_LOAD, RV_PLUGIN_UNLOAD,
        RV_RESULT_BACKEND, RV_RESULT_INVALID_ARGUMENTS, RV_RESULT_OTHER, RV_RESULT_TRANSPORT,
        RV_RESULT_TRUSTED, RV_RESULT_UNTRUSTED, RV_TOOL_CALL, RV_TOOL_CANCEL,
    };
    #[cfg(target_pointer_width = "64")]
    use super::{
        RvCall, RvHost, RvLoadArgs, RvPackInfo, RvPackNewArgs, RvPluginInfo, RvStr, RvToolCallArgs,
        RvToolInfo, RvToolResult,
    };

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
    fn the_pack_info_keeps_its_layout() {
        assert_eq!(size_of::<RvPackInfo>(), 40);
        assert_eq!(offset_of!(RvPackInfo, size), 0);
        assert_eq!(offset_of!(RvPackInfo, reserved), 4);
        assert_eq!(offset_of!(RvPackInfo, name), 8);
        assert_eq!(offset_of!(RvPackInfo, description), 24);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_tool_info_keeps_its_layout() {
        assert_eq!(size_of::<RvToolInfo>(), 72);
        assert_eq!(offset_of!(RvToolInfo, size), 0);
        assert_eq!(offset_of!(RvToolInfo, reserved), 4);
        assert_eq!(offset_of!(RvToolInfo, name), 8);
        assert_eq!(offset_of!(RvToolInfo, wire_name), 24);
        assert_eq!(offset_of!(RvToolInfo, description), 40);
        assert_eq!(offset_of!(RvToolInfo, parameters), 56);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_tool_result_keeps_its_layout() {
        assert_eq!(size_of::<RvToolResult>(), 24);
        assert_eq!(offset_of!(RvToolResult, size), 0);
        assert_eq!(offset_of!(RvToolResult, kind), 4);
        assert_eq!(offset_of!(RvToolResult, text), 8);
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
        assert_eq!(size_of::<RvLoadArgs>(), 40);
        assert_eq!(offset_of!(RvLoadArgs, size), 0);
        assert_eq!(offset_of!(RvLoadArgs, reserved), 4);
        assert_eq!(offset_of!(RvLoadArgs, ctx), 8);
        assert_eq!(offset_of!(RvLoadArgs, refuse), 16);
        assert_eq!(offset_of!(RvLoadArgs, describe_plugin), 24);
        assert_eq!(offset_of!(RvLoadArgs, insert_pack), 32);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_pack_new_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvPackNewArgs>(), 48);
        assert_eq!(offset_of!(RvPackNewArgs, size), 0);
        assert_eq!(offset_of!(RvPackNewArgs, reserved), 4);
        assert_eq!(offset_of!(RvPackNewArgs, ctx), 8);
        assert_eq!(offset_of!(RvPackNewArgs, pack), 16);
        assert_eq!(offset_of!(RvPackNewArgs, refuse), 32);
        assert_eq!(offset_of!(RvPackNewArgs, add_tool), 40);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_tool_call_arguments_keep_their_layout() {
        assert_eq!(size_of::<RvToolCallArgs>(), 56);
        assert_eq!(offset_of!(RvToolCallArgs, size), 0);
        assert_eq!(offset_of!(RvToolCallArgs, reserved), 4);
        assert_eq!(offset_of!(RvToolCallArgs, ctx), 8);
        assert_eq!(offset_of!(RvToolCallArgs, tool), 16);
        assert_eq!(offset_of!(RvToolCallArgs, arguments), 32);
        assert_eq!(offset_of!(RvToolCallArgs, finish), 48);
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn the_call_keeps_its_layout() {
        assert_eq!(size_of::<RvCall>(), 48);
        assert_eq!(offset_of!(RvCall, size), 0);
        assert_eq!(offset_of!(RvCall, verb), 4);
        assert_eq!(offset_of!(RvCall, host), 8);
        assert_eq!(offset_of!(RvCall, plugin), 16);
        assert_eq!(offset_of!(RvCall, pack_run), 24);
        assert_eq!(offset_of!(RvCall, args), 32);
        assert_eq!(offset_of!(RvCall, tool_call), 40);
    }

    #[test]
    fn the_status_codes_keep_their_numbers() {
        assert_eq!(RV_OK, 0);
        assert_eq!(RV_ERR_MISUSE, 4);
        assert_eq!(RV_ERR_REFUSED, 5);
        assert_eq!(RV_ERR_UNSUPPORTED, 6);
        assert_eq!(RV_PENDING, 7);
    }

    #[test]
    fn the_verbs_keep_their_numbers() {
        assert_eq!(RV_PLUGIN_LOAD, 1);
        assert_eq!(RV_PLUGIN_UNLOAD, 2);
        assert_eq!(RV_PACK_NEW, 3);
        assert_eq!(RV_PACK_DELETE, 4);
        assert_eq!(RV_TOOL_CALL, 5);
        assert_eq!(RV_TOOL_CANCEL, 6);
    }

    #[test]
    fn the_result_kinds_keep_their_numbers() {
        assert_eq!(RV_RESULT_UNTRUSTED, 0);
        assert_eq!(RV_RESULT_TRUSTED, 1);
        assert_eq!(RV_RESULT_INVALID_ARGUMENTS, 2);
        assert_eq!(RV_RESULT_BACKEND, 3);
        assert_eq!(RV_RESULT_TRANSPORT, 4);
        assert_eq!(RV_RESULT_OTHER, 5);
    }

    #[test]
    fn the_crate_version_is_the_contract_version() {
        assert_eq!(env!("CARGO_PKG_VERSION_MAJOR"), RV_API_MAJOR.to_string());
        assert_eq!(env!("CARGO_PKG_VERSION_MINOR"), RV_API_MINOR.to_string());
    }
}
