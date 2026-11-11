//! A raw Plugin for the fake Host's own tests: an entry point that
//! follows the contract except for the one [`Breach`] its script names.
//! It ignores what the Host's functions answer and returns `RV_OK`, as a
//! careless Plugin would, so each test sees what the fake Host recorded.

use std::cell::Cell;
use std::ffi::c_void;
use std::ptr;

use runvil_plugin_sys::{
    RV_ERR_UNSUPPORTED, RV_OK, RV_PACK_DELETE, RV_PACK_NEW, RV_PENDING, RV_PLUGIN_LOAD,
    RV_TOOL_CALL, RV_TOOL_CANCEL, RvCall, RvFileError, RvLoadArgs, RvPackInfo, RvPackNewArgs,
    RvPluginInfo, RvStatus, RvStr, RvToolCallArgs, RvToolInfo, RvToolResult,
};

use super::{Failure, Loaded, load_entry};
use crate::entry::{rv_str, size_field};

thread_local! {
    /// The breach the fixture on this thread makes.
    static SCRIPT: Cell<Breach> = const { Cell::new(Breach::None) };
    /// How many `RV_PACK_DELETE` calls the fixture on this thread got.
    pub(super) static DELETES: Cell<usize> = const { Cell::new(0) };
    /// How many `RV_TOOL_CANCEL` calls the fixture on this thread got.
    pub(super) static CANCELS: Cell<usize> = const { Cell::new(0) };
}

/// The one rule the fixture breaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Breach {
    /// Follows the contract.
    None,
    /// Describes itself through a `ctx` that is not its load's.
    LoadCtx,
    /// Calls `describe_plugin` twice.
    DescribeTwice,
    /// Names itself with something other than one segment.
    PluginName,
    /// Gives an empty version.
    NoVersion,
    /// Returns from its load without describing itself.
    Undescribed,
    /// Calls the load's `refuse` twice.
    RefuseLoadTwice,
    /// Inserts a Pack whose name has no vendor.
    PackName,
    /// Inserts one Pack twice.
    PackTwice,
    /// Inserts a Pack whose description has two lines.
    PackDescription,
    /// Inserts a Pack whose info has a nonzero `reserved`.
    PackReserved,
    /// Calls `add_tool` during its load.
    PackFunctionInLoad,
    /// Adds a tool through a `ctx` that is not its call's.
    PackCtx,
    /// Adds a tool whose name is not one segment.
    ToolName,
    /// Adds one tool twice.
    ToolTwice,
    /// Adds a tool whose wire name has a `/`.
    WireName,
    /// Adds a tool with an empty description.
    NoDescription,
    /// Adds a tool whose parameters are a JSON array.
    Parameters,
    /// Calls `describe_plugin` during `RV_PACK_NEW`.
    LoadFunctionInPackNew,
    /// Declines to make its Pack instance.
    RefusePack,
    /// Answers `RV_PACK_NEW` with `RV_ERR_UNSUPPORTED`.
    PackStatus,
    /// Finishes through a `ctx` that is not its step's.
    FinishCtx,
    /// Calls `finish` twice.
    FinishTwice,
    /// Finishes with a kind the contract does not name.
    Kind,
    /// Returns `RV_OK` from a step without finishing.
    NoFinish,
    /// Answers `RV_TOOL_CALL` with `RV_ERR_UNSUPPORTED`.
    CallStatus,
    /// Finishes and then returns `RV_PENDING`.
    FinishThenPending,
    /// Hands `finish` a NULL result and returns `RV_PENDING`.
    MisuseThenPending,
    /// Hands `file_stat` a NULL `stat`, then finishes as if nothing
    /// happened.
    FileStatNull,
}

/// Loads the fixture on this thread with `breach` as its script.
pub(super) fn load(breach: Breach) -> Result<Loaded, Failure> {
    SCRIPT.set(breach);
    // SAFETY: `entry` follows the contract apart from the breach, which
    // the fake Host answers without reading anything invalid, and never
    // unwinds.
    unsafe { load_entry(entry) }
}

/// The fixture's entry point.
unsafe extern "C" fn entry(call: *mut RvCall) -> RvStatus {
    let breach = SCRIPT.get();
    // SAFETY: the fake Host made `call`, with the arguments struct of its
    // verb behind `args`.
    unsafe {
        match (*call).verb {
            RV_PLUGIN_LOAD => load_verb(call, &*(*call).args.cast(), breach),
            RV_PACK_NEW => pack_new(call, &*(*call).args.cast(), breach),
            RV_PACK_DELETE => count(&DELETES),
            RV_TOOL_CALL => tool_call(call, &*(*call).args.cast(), breach),
            RV_TOOL_CANCEL => count(&CANCELS),
            _ => RV_OK,
        }
    }
}

/// Adds one to `counter` and answers `RV_OK`.
fn count(counter: &'static std::thread::LocalKey<Cell<usize>>) -> RvStatus {
    counter.set(counter.get() + 1);
    RV_OK
}

/// A pointer that is not NULL, for an instance nothing ever reads.
fn dangling() -> *mut c_void {
    ptr::NonNull::<u8>::dangling().as_ptr().cast()
}

/// `ctx`, or NULL when the fixture breaks `wrong`.
fn ctx_unless(breach: Breach, wrong: Breach, ctx: *mut c_void) -> *mut c_void {
    if breach == wrong {
        ptr::null_mut()
    } else {
        ctx
    }
}

/// `plain`, or `broken` when the fixture breaks `wrong`.
fn text_unless(breach: Breach, wrong: Breach, plain: &'static str, broken: &'static str) -> RvStr {
    rv_str(if breach == wrong { broken } else { plain })
}

/// The fixture's description of itself.
fn plugin_info(breach: Breach) -> RvPluginInfo {
    RvPluginInfo {
        size: size_field::<RvPluginInfo>(),
        reserved: 0,
        name: text_unless(breach, Breach::PluginName, "fixture", "Not A Segment"),
        version: text_unless(breach, Breach::NoVersion, "1", ""),
    }
}

/// Answers `RV_PLUGIN_LOAD`.
///
/// # Safety
/// `call` and `args` are the fake Host's open load.
unsafe fn load_verb(call: *mut RvCall, args: &RvLoadArgs, breach: Breach) -> RvStatus {
    let info = plugin_info(breach);
    let ctx = ctx_unless(breach, Breach::LoadCtx, args.ctx);
    // SAFETY: the open load's own functions, and structs that outlive each
    // call.
    unsafe {
        match breach {
            Breach::Undescribed => {}
            Breach::RefuseLoadTwice => {
                (args.refuse)(args.ctx, rv_str("first"));
                return (args.refuse)(args.ctx, rv_str("second"));
            }
            Breach::DescribeTwice => {
                (args.describe_plugin)(ctx, &raw const info);
                (args.describe_plugin)(ctx, &raw const info);
            }
            _ => {
                (args.describe_plugin)(ctx, &raw const info);
            }
        }
        let pack = RvPackInfo {
            size: size_field::<RvPackInfo>(),
            reserved: u32::from(breach == Breach::PackReserved),
            name: text_unless(breach, Breach::PackName, "acme/fixture", "fixture"),
            description: text_unless(
                breach,
                Breach::PackDescription,
                "A fixture Pack",
                "two\nlines",
            ),
        };
        (args.insert_pack)(args.ctx, &raw const pack);
        if breach == Breach::PackTwice {
            (args.insert_pack)(args.ctx, &raw const pack);
        }
        if breach == Breach::PackFunctionInLoad {
            let tool = tool_info(breach);
            super::pack::add_tool(args.ctx, &raw const tool);
        }
        (*call).plugin = dangling();
    }
    RV_OK
}

/// The fixture's first tool.
fn tool_info(breach: Breach) -> RvToolInfo {
    RvToolInfo {
        size: size_field::<RvToolInfo>(),
        reserved: 0,
        name: text_unless(breach, Breach::ToolName, "tool", "Not A Segment"),
        wire_name: text_unless(breach, Breach::WireName, "Tool", "web/fetch"),
        description: text_unless(breach, Breach::NoDescription, "A fixture tool.", ""),
        parameters: text_unless(breach, Breach::Parameters, "{}", "[]"),
    }
}

/// Answers `RV_PACK_NEW`.
///
/// # Safety
/// `call` and `args` are the fake Host's open call.
unsafe fn pack_new(call: *mut RvCall, args: &RvPackNewArgs, breach: Breach) -> RvStatus {
    let ctx = ctx_unless(breach, Breach::PackCtx, args.ctx);
    let first = tool_info(breach);
    let other = RvToolInfo {
        name: rv_str("other"),
        wire_name: rv_str(""),
        description: rv_str("Another fixture tool."),
        ..tool_info(Breach::None)
    };
    // SAFETY: the open call's own functions, and structs that outlive each
    // call.
    unsafe {
        match breach {
            Breach::RefusePack => return (args.refuse)(args.ctx, rv_str("no instance today")),
            Breach::PackStatus => return RV_ERR_UNSUPPORTED,
            Breach::LoadFunctionInPackNew => {
                let info = plugin_info(breach);
                super::load::describe_plugin(args.ctx, &raw const info);
            }
            _ => {}
        }
        (args.add_tool)(ctx, &raw const first);
        if breach == Breach::ToolTwice {
            (args.add_tool)(ctx, &raw const first);
        }
        (args.add_tool)(ctx, &raw const other);
        (*call).pack_run = dangling();
    }
    RV_OK
}

/// Answers `RV_TOOL_CALL`. The tool `other` returns `RV_PENDING` twice
/// before it finishes, counting its steps in `call->tool_call`. The tool
/// `tool` finishes at once, with the kind its arguments spell, and its
/// name and arguments as the text.
///
/// # Safety
/// `call` and `args` are the fake Host's open step.
unsafe fn tool_call(call: *mut RvCall, args: &RvToolCallArgs, breach: Breach) -> RvStatus {
    // SAFETY: the Host keeps both strings valid for the step, and the fake
    // Host only ever sends UTF-8.
    let (tool, arguments) = unsafe { (text(args.tool), text(args.arguments)) };
    // SAFETY: `call` is the fake Host's open step.
    let steps = unsafe { (*call).tool_call }.addr();
    if tool == "other" && steps < 2 {
        // SAFETY: as above.
        unsafe { (*call).tool_call = ptr::without_provenance_mut(steps + 1) };
        return RV_PENDING;
    }
    let text = if tool == "other" {
        format!("other after {steps} steps")
    } else {
        format!("{tool} {arguments}")
    };
    let result = RvToolResult {
        size: size_field::<RvToolResult>(),
        kind: if breach == Breach::Kind {
            9
        } else {
            arguments.parse().unwrap_or(0)
        },
        text: rv_str(&text),
    };
    let ctx = ctx_unless(breach, Breach::FinishCtx, args.ctx);
    // SAFETY: the open step's own function, and a result that outlives
    // each call.
    unsafe {
        match breach {
            Breach::NoFinish => return RV_OK,
            Breach::CallStatus => return RV_ERR_UNSUPPORTED,
            Breach::MisuseThenPending => {
                (args.finish)(args.ctx, ptr::null());
                return RV_PENDING;
            }
            Breach::FileStatNull => {
                let mut error = RvFileError {
                    kind: 0,
                    reserved: 0,
                    text: rv_str(""),
                };
                (args.file_stat)(args.ctx, rv_str("/a"), ptr::null_mut(), &raw mut error);
            }
            _ => {}
        }
        (args.finish)(ctx, &raw const result);
        if breach == Breach::FinishTwice {
            (args.finish)(ctx, &raw const result);
        }
    }
    if breach == Breach::FinishThenPending {
        return RV_PENDING;
    }
    RV_OK
}

/// The text `s` holds, or the empty string when it is not UTF-8.
///
/// # Safety
/// `s.ptr` is valid for reads of `s.len` bytes for the lifetime `'a`.
unsafe fn text<'a>(s: RvStr) -> &'a str {
    if s.len == 0 {
        return "";
    }
    // SAFETY: the caller guarantees the pointer and the length.
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr.cast::<u8>(), s.len) };
    std::str::from_utf8(bytes).unwrap_or_default()
}
