//! `RV_PACK_NEW` and `RV_PACK_DELETE`: making one run's instance of a
//! Pack, adding its tools, and dropping the instance.

use std::panic::{self, AssertUnwindSafe};
use std::ptr;

use runvil_plugin_sys::{RV_ERR_MISUSE, RV_OK, RvCall, RvPackNewArgs, RvStatus, RvStr, RvToolInfo};

use super::{args, contain, panic_message, read_str, refuse, rv_str, size_field};
use crate::pack::PackRun;
use crate::{Pack, Plugin, Refusal, ToolInfo};

/// Answers `RV_PACK_NEW`: makes the instance with [`Plugin::new_pack`],
/// adds each of its tools, and only then hands the instance to the Host.
/// A refusal, or a panic while the instance is made or lists its tools,
/// declines through the Host's `refuse`, and every path that fails frees
/// what it made.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `plugin` is NULL or the
/// pointer `load::<P>` set, and whose `args` is NULL or an `RvPackNewArgs`
/// that follows the contract.
pub(super) unsafe fn new<P: Plugin>(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let plugin = unsafe { (*call).plugin }.cast::<P>();
    if plugin.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: for this verb `args` is NULL or an `RvPackNewArgs`.
    let args = match unsafe { args::<RvPackNewArgs>(call) } {
        Ok(args) => args,
        Err(status) => return status,
    };
    // SAFETY: the Host keeps the Pack's name valid for the call.
    let Some(name) = (unsafe { read_str(args.pack) }) else {
        return RV_ERR_MISUSE;
    };
    // SAFETY: a non-NULL `plugin` is the instance a successful load set,
    // which the Host unloads only after this call returns.
    let plugin = unsafe { &*plugin };
    let made = panic::catch_unwind(AssertUnwindSafe(|| {
        let pack = plugin.new_pack(name)?;
        let tools = pack.tools();
        Ok((pack, tools))
    }));
    let (pack, tools) = match made {
        Ok(Ok(made)) => made,
        // SAFETY: `args` belongs to the open call.
        Ok(Err(refusal)) => return unsafe { refuse(args.refuse, args.ctx, &refusal) },
        Err(payload) => {
            let refusal = Refusal::new(format!(
                "the Pack panicked while it was made: {}",
                panic_message(&*payload)
            ));
            // SAFETY: as above.
            return unsafe { refuse(args.refuse, args.ctx, &refusal) };
        }
    };
    // SAFETY: `args` belongs to the open call, and this is its thread.
    let status = unsafe { add_tools(args, &tools) };
    if status != RV_OK {
        contain(move || drop(pack));
        return status;
    }
    let pack_run = Box::new(PackRun { pack });
    // SAFETY: the caller makes `call` point to a whole `RvCall`, which
    // nothing else reads or writes while this call runs.
    unsafe { (&raw mut (*call).pack_run).write(Box::into_raw(pack_run).cast()) };
    RV_OK
}

/// Adds each of `tools` to the instance being made, stopping at the first
/// status that is not `RV_OK`.
///
/// # Safety
/// `args` belongs to the `RV_PACK_NEW` call open on this thread.
unsafe fn add_tools(args: &RvPackNewArgs, tools: &[ToolInfo]) -> RvStatus {
    for tool in tools {
        let info = RvToolInfo {
            size: size_field::<RvToolInfo>(),
            reserved: 0,
            name: rv_str(&tool.name),
            wire_name: tool.wire_name.as_deref().map_or(EMPTY, rv_str),
            description: rv_str(&tool.description),
            parameters: rv_str(&tool.parameters),
        };
        // SAFETY: the caller passes the open call's arguments, and `info`
        // and its strings outlive the call.
        let status = unsafe { (args.add_tool)(args.ctx, &raw const info) };
        if status != RV_OK {
            return status;
        }
    }
    RV_OK
}

/// The empty string, which lets the Host derive a tool's wire name.
const EMPTY: RvStr = RvStr {
    ptr: ptr::null(),
    len: 0,
};

/// Answers `RV_PACK_DELETE`: drops the instance `RV_PACK_NEW` set,
/// containing a panic in its `Drop`.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `pack_run` is NULL or the
/// pointer [`new`] set, not yet deleted, with no tool call of it still
/// open.
pub(super) unsafe fn delete(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let pack_run = unsafe { (*call).pack_run }.cast::<PackRun>();
    if pack_run.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: a non-NULL `pack_run` is the `Box<PackRun>` a successful
    // `RV_PACK_NEW` leaked, and the Host deletes each instance once, so
    // this is its only free.
    let pack_run = unsafe { Box::from_raw(pack_run) };
    contain(move || drop(pack_run));
    RV_OK
}

/// The Pack behind `pack_run`, for as long as the Host keeps the instance,
/// or `None` when `pack_run` is NULL.
///
/// # Safety
/// A non-NULL `pack_run` is the pointer [`new`] set, and the Host deletes
/// the instance only after every use of the returned reference has ended.
pub(super) unsafe fn pack_of<'a>(pack_run: *mut std::ffi::c_void) -> Option<&'a dyn Pack> {
    // SAFETY: the caller guarantees a non-NULL `pack_run` is a live
    // `PackRun`.
    let pack_run = unsafe { pack_run.cast::<PackRun>().as_ref() }?;
    Some(&*pack_run.pack)
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use runvil_plugin_sys::{RV_ERR_MISUSE, RV_PACK_DELETE, RV_PACK_NEW, RvCall};

    use crate::entry::{entry, size_field};
    use crate::testing::{self, Failure};
    use crate::{
        BoxFuture, Pack, PackInfo, Plugin, Refusal, ToolCall, ToolError, ToolInfo, ToolOutput,
    };

    /// How many `Counted` Pack instances are alive, for the one test that
    /// makes them.
    static COUNTED_LIVE: AtomicIsize = AtomicIsize::new(0);
    /// How many `BadTool` Pack instances are alive, for the one test that
    /// makes them.
    static BAD_TOOL_LIVE: AtomicIsize = AtomicIsize::new(0);

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

    /// The call of a Pack whose tools are never called.
    fn uncalled<'a>() -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async { Ok(ToolOutput::trusted("")) })
    }

    /// A Plugin whose Pack `acme/one` counts its instances, whose Pack
    /// `acme/declines` refuses, whose Pack `acme/panics` panics, and whose
    /// Pack `acme/bad-tool` offers a tool the Host rejects.
    struct Maker;

    impl Plugin for Maker {
        const NAME: &'static str = "maker";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[
            PackInfo::new("acme/one"),
            PackInfo::new("acme/declines"),
            PackInfo::new("acme/panics"),
            PackInfo::new("acme/bad-tool"),
        ];

        fn load() -> Result<Maker, Refusal> {
            Ok(Maker)
        }

        fn new_pack(&self, pack: &str) -> Result<Box<dyn Pack>, Refusal> {
            match pack {
                "acme/one" => {
                    COUNTED_LIVE.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(Counted))
                }
                "acme/declines" => Err(Refusal::new("this run has no network")),
                "acme/bad-tool" => {
                    BAD_TOOL_LIVE.fetch_add(1, Ordering::SeqCst);
                    Ok(Box::new(BadTool))
                }
                _ => panic!("boom"),
            }
        }
    }

    struct Counted;

    impl Pack for Counted {
        fn tools(&self) -> Vec<ToolInfo> {
            vec![
                ToolInfo::new("first", "The first tool.", SCHEMA).wire_name("First"),
                ToolInfo::new("second", "The second tool.", SCHEMA),
            ]
        }

        fn call<'a>(&'a self, _call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            uncalled()
        }
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            COUNTED_LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct BadTool;

    impl Pack for BadTool {
        fn tools(&self) -> Vec<ToolInfo> {
            vec![ToolInfo::new("Not A Segment", "A tool.", SCHEMA)]
        }

        fn call<'a>(&'a self, _call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            uncalled()
        }
    }

    impl Drop for BadTool {
        fn drop(&mut self) {
            BAD_TOOL_LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// A Plugin that lists a Pack and keeps the default `new_pack`.
    struct NoMaker;

    impl Plugin for NoMaker {
        const NAME: &'static str = "no-maker";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[PackInfo::new("acme/one")];

        fn load() -> Result<NoMaker, Refusal> {
            Ok(NoMaker)
        }
    }

    #[test]
    fn a_pack_instance_is_made_lists_its_tools_and_is_deleted_once() {
        let loaded = testing::load::<Maker>().unwrap();
        let pack = loaded.new_pack("acme/one").unwrap();
        assert_eq!(COUNTED_LIVE.load(Ordering::SeqCst), 1);
        assert_eq!(
            pack.tools(),
            [
                ToolInfo::new("first", "The first tool.", SCHEMA).wire_name("First"),
                ToolInfo::new("second", "The second tool.", SCHEMA),
            ]
        );
        drop(pack);
        assert_eq!(COUNTED_LIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_refusal_reaches_the_host_with_its_reason() {
        let loaded = testing::load::<Maker>().unwrap();
        let failure = loaded.new_pack("acme/declines").unwrap_err();
        assert_eq!(
            failure,
            Failure::Refused("this run has no network".to_owned())
        );
    }

    #[test]
    fn a_panic_while_the_instance_is_made_becomes_a_refusal() {
        let loaded = testing::load::<Maker>().unwrap();
        let failure = loaded.new_pack("acme/panics").unwrap_err();
        assert_eq!(
            failure,
            Failure::Refused("the Pack panicked while it was made: boom".to_owned())
        );
    }

    #[test]
    fn a_tool_the_host_rejects_frees_the_instance() {
        let loaded = testing::load::<Maker>().unwrap();
        let failure = loaded.new_pack("acme/bad-tool").unwrap_err();
        assert_eq!(
            failure,
            Failure::Misused("the tool's name is not one name segment, such as `fetch`")
        );
        assert_eq!(BAD_TOOL_LIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_plugin_that_keeps_the_default_new_pack_refuses() {
        let loaded = testing::load::<NoMaker>().unwrap();
        let failure = loaded.new_pack("acme/one").unwrap_err();
        assert_eq!(
            failure,
            Failure::Refused(
                "the Plugin no-maker makes no instance of the Pack acme/one".to_owned()
            )
        );
    }

    #[test]
    fn a_pack_verb_without_its_instance_is_a_misuse() {
        for verb in [RV_PACK_NEW, RV_PACK_DELETE] {
            let mut call = RvCall {
                size: size_field::<RvCall>(),
                verb,
                host: ptr::null(),
                plugin: ptr::null_mut(),
                pack_run: ptr::null_mut(),
                args: ptr::null(),
                tool_call: ptr::null_mut(),
            };
            // SAFETY: the entry point checks the instance pointer each
            // verb needs before it reads anything else.
            assert_eq!(unsafe { entry::<Maker>(&raw mut call) }, RV_ERR_MISUSE);
        }
    }
}
