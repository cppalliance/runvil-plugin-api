//! `RV_PACK_NEW`, `RV_PACK_RENDER`, and `RV_PACK_DELETE`: making one run's
//! instance of a Pack and adding its tools, listing the tools a model is
//! shown, and dropping the instance.

use std::ffi::c_void;
use std::mem::size_of;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::slice;

use runvil_plugin_sys::{
    RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_OK, RV_TOOL_CHANGES_FILES, RvCall, RvPackNewArgs,
    RvPackRenderArgs, RvStatus, RvStr, RvToolInfo,
};

use super::{args, contain, panic_message, read_str, refuse, rv_str, size_field};
use crate::pack::PackRun;
use crate::{Pack, Plugin, Refusal, RenderContext, ToolInfo};

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
    let status = unsafe { add_tools(args.add_tool, args.ctx, &tools) };
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

/// The function a call's arguments carry for adding one tool.
type AddTool = unsafe extern "C" fn(*mut c_void, *const RvToolInfo) -> RvStatus;

/// Adds each of `tools` through `add_tool`, stopping at the first status
/// that is not `RV_OK`.
///
/// # Safety
/// `add_tool` and `ctx` come from the arguments struct of the
/// `RV_PACK_NEW` or `RV_PACK_RENDER` call open on this thread.
unsafe fn add_tools(add_tool: AddTool, ctx: *mut c_void, tools: &[ToolInfo]) -> RvStatus {
    for tool in tools {
        let info = RvToolInfo {
            size: size_field::<RvToolInfo>(),
            flags: if tool.changes_files {
                RV_TOOL_CHANGES_FILES
            } else {
                0
            },
            name: rv_str(&tool.name),
            wire_name: tool.wire_name.as_deref().map_or(EMPTY, rv_str),
            description: rv_str(&tool.description),
            parameters: rv_str(&tool.parameters),
        };
        // SAFETY: the caller passes the open call's function and `ctx`, and
        // `info` and its strings outlive the call.
        let status = unsafe { add_tool(ctx, &raw const info) };
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

/// Answers `RV_PACK_RENDER`: copies what the Host lends, calls
/// [`Pack::render`] with it, and adds each tool the Pack lists. A refusal,
/// or a panic in the render, declines through the Host's `refuse`. The
/// instance is as it was after every outcome, so nothing is freed.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `pack_run` is NULL or the
/// pointer [`new`] set, not yet deleted, and whose `args` is NULL or an
/// `RvPackRenderArgs` that follows the contract.
pub(super) unsafe fn render(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`, and the
    // Host deletes the instance only after this call returns.
    let Some(pack) = (unsafe { pack_of((*call).pack_run) }) else {
        return RV_ERR_MISUSE;
    };
    // SAFETY: for this verb `args` is NULL or an `RvPackRenderArgs`.
    let args = match unsafe { args::<RvPackRenderArgs>(call) } {
        Ok(args) => args,
        Err(status) => return status,
    };
    // SAFETY: the Host keeps both strings valid for the call.
    let (entry, model) = unsafe { (read_str(args.entry), read_str(args.model)) };
    let (Some(entry), Some(model)) = (entry, model) else {
        return RV_ERR_MISUSE;
    };
    // SAFETY: the Host keeps the offered tools valid for the call.
    let offered = match unsafe { copy_offered(args) } {
        Ok(offered) => offered,
        Err(status) => return status,
    };
    let cx = RenderContext::new(entry, model);
    let rendered = panic::catch_unwind(AssertUnwindSafe(|| pack.render(&cx, offered)));
    let tools = match rendered {
        Ok(Ok(tools)) => tools,
        // SAFETY: `args` belongs to the open call.
        Ok(Err(refusal)) => return unsafe { refuse(args.refuse, args.ctx, &refusal) },
        Err(payload) => {
            let refusal = Refusal::new(format!(
                "the Pack panicked while it rendered: {}",
                panic_message(&*payload)
            ));
            // The payload belongs to the Pack, and its drop may panic.
            contain(move || drop(payload));
            // SAFETY: as above.
            return unsafe { refuse(args.refuse, args.ctx, &refusal) };
        }
    };
    // SAFETY: `args` belongs to the open call, and this is its thread.
    unsafe { add_tools(args.add_tool, args.ctx, &tools) }
}

/// A copy of the tools a render is offered.
///
/// # Safety
/// `args.offered` is NULL or points to `args.offered_len` readable
/// `RvToolInfo` values whose strings are valid, for the call.
unsafe fn copy_offered(args: &RvPackRenderArgs) -> Result<Vec<ToolInfo>, RvStatus> {
    if args.offered_len == 0 {
        return Ok(Vec::new());
    }
    let longest = isize::MAX as usize / size_of::<RvToolInfo>();
    if args.offered.is_null() || !args.offered.is_aligned() || args.offered_len > longest {
        return Err(RV_ERR_MISUSE);
    }
    // SAFETY: the pointer is not NULL, is aligned, and the length cannot
    // overflow a slice; the caller vouches that the values are readable.
    let offered = unsafe { slice::from_raw_parts(args.offered, args.offered_len) };
    // SAFETY: the caller vouches for each value's strings.
    offered
        .iter()
        .map(|info| unsafe { copy_tool(info) })
        .collect()
}

/// A copy of one offered tool.
///
/// # Safety
/// Each string in `info` is valid for its length, for the call.
unsafe fn copy_tool(info: &RvToolInfo) -> Result<ToolInfo, RvStatus> {
    if (info.size as usize) < size_of::<RvToolInfo>() {
        return Err(RV_ERR_UNSUPPORTED);
    }
    // SAFETY: the caller vouches for the strings.
    let strings = unsafe {
        (
            read_str(info.name),
            read_str(info.wire_name),
            read_str(info.description),
            read_str(info.parameters),
        )
    };
    let (Some(name), Some(wire_name), Some(description), Some(parameters)) = strings else {
        return Err(RV_ERR_MISUSE);
    };
    Ok(ToolInfo {
        name: name.to_owned(),
        wire_name: (!wire_name.is_empty()).then(|| wire_name.to_owned()),
        description: description.to_owned(),
        parameters: parameters.to_owned(),
        changes_files: info.flags & RV_TOOL_CHANGES_FILES != 0,
    })
}

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
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use runvil_plugin_sys::{
        RV_ERR_MISUSE, RV_ERR_REFUSED, RV_ERR_UNSUPPORTED, RV_OK, RV_PACK_DELETE, RV_PACK_NEW,
        RV_PACK_RENDER, RV_TOOL_CHANGES_FILES, RvCall, RvPackRenderArgs, RvStatus, RvStr,
        RvToolInfo,
    };

    use super::{EMPTY, PackRun};
    use crate::entry::{entry, read_str, rv_str, size_field};
    use crate::testing::{self, Failure};
    use crate::{
        BoxFuture, Pack, PackInfo, Plugin, Refusal, RenderContext, ToolCall, ToolError, ToolInfo,
        ToolOutput,
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
                ToolInfo::new("second", "The second tool.", SCHEMA).changes_files(),
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
                ToolInfo::new("second", "The second tool.", SCHEMA).changes_files(),
            ]
        );
        assert!(!pack.tools()[0].changes_files, "no flag unless it is set");
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

    /// A change to a render's arguments that breaks one rule.
    type Edit = fn(&mut RvPackRenderArgs, &mut [RvToolInfo]);

    /// What a render's hand-built Host recorded.
    #[derive(Default)]
    struct Recorder {
        /// Each tool added, as the Host reads it.
        added: Vec<ToolInfo>,
        /// The reason of each refusal.
        refusals: Vec<String>,
        /// The status `add_tool` returns for the tool at this index.
        fail_at: Option<usize>,
    }

    /// `add_tool` of a hand-built Host: records the tool and fails the
    /// one at `fail_at`.
    unsafe extern "C" fn record_tool(ctx: *mut c_void, info: *const RvToolInfo) -> RvStatus {
        // SAFETY: the test passes a `Recorder` as `ctx`, and the entry point
        // passes a readable `RvToolInfo` with valid strings.
        let (recorder, info) = unsafe { (&mut *ctx.cast::<Recorder>(), &*info) };
        // SAFETY: the entry point's strings are valid for the call.
        let text = |s: RvStr| unsafe { read_str(s) }.unwrap().to_owned();
        if recorder.fail_at == Some(recorder.added.len()) {
            return RV_ERR_MISUSE;
        }
        recorder.added.push(ToolInfo {
            name: text(info.name),
            wire_name: Some(text(info.wire_name)).filter(|name| !name.is_empty()),
            description: text(info.description),
            parameters: text(info.parameters),
            changes_files: info.flags & RV_TOOL_CHANGES_FILES != 0,
        });
        RV_OK
    }

    /// `refuse` of a hand-built Host: records the reason.
    unsafe extern "C" fn record_refusal(ctx: *mut c_void, reason: RvStr) -> RvStatus {
        // SAFETY: the test passes a `Recorder` as `ctx`, and the entry point's
        // reason is valid for the call.
        let (recorder, reason) =
            unsafe { (&mut *ctx.cast::<Recorder>(), read_str(reason).unwrap()) };
        recorder.refusals.push(reason.to_owned());
        RV_ERR_REFUSED
    }

    /// What a render's Pack does.
    #[derive(Clone, Copy)]
    enum Renders {
        /// Keeps the default.
        Default,
        /// Hides `second`, shows `first` as `Look`, and adds `lite`.
        Reshapes,
        /// Refuses.
        Refuses,
        /// Panics.
        Panics,
        /// Panics with a payload whose drop panics too.
        PanicsWithBomb,
    }

    /// A panic payload whose drop panics.
    struct Bomb;

    impl Drop for Bomb {
        fn drop(&mut self) {
            panic!("the payload's drop panics");
        }
    }

    impl Pack for Renders {
        fn tools(&self) -> Vec<ToolInfo> {
            Vec::new()
        }

        fn call<'a>(&'a self, _call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            uncalled()
        }

        fn render(
            &self,
            cx: &RenderContext<'_>,
            offered: Vec<ToolInfo>,
        ) -> Result<Vec<ToolInfo>, Refusal> {
            match self {
                Renders::Default => Ok(offered),
                Renders::Reshapes => {
                    let mut shown: Vec<ToolInfo> = offered
                        .into_iter()
                        .filter(|tool| tool.name != "second")
                        .map(|tool| tool.wire_name("Look"))
                        .collect();
                    shown.push(ToolInfo::new(
                        "lite",
                        format!("seen by {} in {}", cx.model, cx.entry),
                        SCHEMA,
                    ));
                    Ok(shown)
                }
                Renders::Refuses => Err(Refusal::new("no tools for this model")),
                Renders::Panics => panic!("boom"),
                Renders::PanicsWithBomb => std::panic::panic_any(Bomb),
            }
        }
    }

    const ENTRY: &str = "entry-1";
    const MODEL: &str = r#"{"name":"m"}"#;

    /// The two tools a section offered, in the Host's own form.
    fn offered() -> Vec<ToolInfo> {
        vec![
            ToolInfo::new("first", "The first tool.", SCHEMA).wire_name("First"),
            ToolInfo::new("second", "The second tool.", SCHEMA).changes_files(),
        ]
    }

    /// Sends `RV_PACK_RENDER` for an instance of `pack` with `offered`, to
    /// a Host that records what the entry point does, and returns the
    /// status and the record. `edit` may break the arguments first.
    fn render_with(
        pack: Renders,
        fail_at: Option<usize>,
        edit: impl FnOnce(&mut RvPackRenderArgs, &mut [RvToolInfo]),
    ) -> (RvStatus, Recorder) {
        let tools = offered();
        let mut values: Vec<RvToolInfo> = tools
            .iter()
            .map(|tool| RvToolInfo {
                size: size_field::<RvToolInfo>(),
                flags: if tool.changes_files {
                    RV_TOOL_CHANGES_FILES
                } else {
                    0
                },
                name: rv_str(&tool.name),
                wire_name: tool.wire_name.as_deref().map_or(EMPTY, rv_str),
                description: rv_str(&tool.description),
                parameters: rv_str(&tool.parameters),
            })
            .collect();
        let mut recorder = Recorder {
            fail_at,
            ..Recorder::default()
        };
        let mut args = RvPackRenderArgs {
            size: size_field::<RvPackRenderArgs>(),
            reserved: 0,
            ctx: (&raw mut recorder).cast(),
            entry: rv_str(ENTRY),
            model: rv_str(MODEL),
            offered: values.as_ptr(),
            offered_len: values.len(),
            refuse: record_refusal,
            add_tool: record_tool,
        };
        edit(&mut args, &mut values);
        let pack_run = Box::into_raw(Box::new(PackRun {
            pack: Box::new(pack),
        }));
        let mut call = RvCall {
            size: size_field::<RvCall>(),
            verb: RV_PACK_RENDER,
            host: ptr::null(),
            plugin: ptr::null_mut(),
            pack_run: pack_run.cast(),
            args: (&raw const args).cast(),
            tool_call: ptr::null_mut(),
        };
        // SAFETY: the call, the arguments, the tools, and the recorder
        // follow the contract and outlive the call; `Maker` is never read.
        let status = unsafe { entry::<Maker>(&raw mut call) };
        // SAFETY: `pack_run` is the box leaked above, freed once, with no
        // call of it still open.
        drop(unsafe { Box::from_raw(pack_run) });
        (status, recorder)
    }

    #[test]
    fn a_render_that_keeps_the_default_lists_the_offered_tools_as_they_came() {
        let (status, recorder) = render_with(Renders::Default, None, |_, _| {});
        assert_eq!(status, RV_OK);
        assert_eq!(recorder.added, offered());
        assert_eq!(recorder.refusals, Vec::<String>::new());
    }

    #[test]
    fn a_render_may_rename_hide_and_add_and_reads_the_entry_and_the_model() {
        let (status, recorder) = render_with(Renders::Reshapes, None, |_, _| {});
        assert_eq!(status, RV_OK);
        assert_eq!(
            recorder.added,
            [
                ToolInfo::new("first", "The first tool.", SCHEMA).wire_name("Look"),
                ToolInfo::new("lite", format!("seen by {MODEL} in {ENTRY}"), SCHEMA),
            ]
        );
    }

    #[test]
    fn a_render_that_offers_nothing_adds_nothing_even_with_a_null_pointer() {
        let (status, recorder) = render_with(Renders::Default, None, |args, _| {
            args.offered = ptr::null();
            args.offered_len = 0;
        });
        assert_eq!(status, RV_OK);
        assert_eq!(recorder.added, Vec::<ToolInfo>::new());
    }

    #[test]
    fn a_refusal_of_a_render_reaches_the_host_with_its_reason() {
        let (status, recorder) = render_with(Renders::Refuses, None, |_, _| {});
        assert_eq!(status, RV_ERR_REFUSED);
        assert_eq!(recorder.refusals, ["no tools for this model"]);
        assert_eq!(recorder.added, Vec::<ToolInfo>::new());
    }

    #[test]
    fn a_panic_in_a_render_becomes_a_refusal() {
        let (status, recorder) = render_with(Renders::Panics, None, |_, _| {});
        assert_eq!(status, RV_ERR_REFUSED);
        assert_eq!(
            recorder.refusals,
            ["the Pack panicked while it rendered: boom"]
        );
    }

    #[test]
    fn a_panic_payload_whose_drop_panics_is_contained_and_the_render_is_refused() {
        let (status, recorder) = render_with(Renders::PanicsWithBomb, None, |_, _| {});
        assert_eq!(status, RV_ERR_REFUSED);
        assert_eq!(
            recorder.refusals,
            ["the Pack panicked while it rendered: a panic payload that is not text"]
        );
    }

    #[test]
    fn a_status_from_add_tool_stops_the_render_and_is_returned() {
        let (status, recorder) = render_with(Renders::Default, Some(1), |_, _| {});
        assert_eq!(status, RV_ERR_MISUSE);
        assert_eq!(recorder.added, offered()[..1]);
    }

    #[test]
    fn a_render_the_host_breaks_is_a_misuse_or_unsupported() {
        let cases: [(&str, Edit, RvStatus); 7] = [
            (
                "offered tools with a NULL pointer",
                |args, _| args.offered = ptr::null(),
                RV_ERR_MISUSE,
            ),
            (
                "a misaligned pointer",
                |args, _| args.offered = args.offered.cast::<u8>().wrapping_add(1).cast(),
                RV_ERR_MISUSE,
            ),
            (
                "an entry that is not UTF-8",
                |args, _| {
                    args.entry = RvStr {
                        ptr: [0xff_u8].as_ptr().cast(),
                        len: 1,
                    };
                },
                RV_ERR_MISUSE,
            ),
            (
                "a model with a NULL pointer",
                |args, _| {
                    args.model = RvStr {
                        ptr: ptr::null(),
                        len: 2,
                    };
                },
                RV_ERR_MISUSE,
            ),
            (
                "a struct shorter than this crate's",
                |args, _| args.size = 8,
                RV_ERR_UNSUPPORTED,
            ),
            (
                "an offered tool struct shorter than this crate's",
                |_, tools| tools[1].size = 8,
                RV_ERR_UNSUPPORTED,
            ),
            (
                "an offered tool whose name has a NULL pointer",
                |_, tools| {
                    tools[0].name = RvStr {
                        ptr: ptr::null(),
                        len: 5,
                    };
                },
                RV_ERR_MISUSE,
            ),
        ];
        for (what, edit, expected) in cases {
            let (status, recorder) = render_with(Renders::Default, None, edit);
            assert_eq!(status, expected, "{what}");
            assert_eq!(recorder.added, Vec::<ToolInfo>::new(), "{what}");
        }
    }

    #[test]
    fn a_render_without_its_instance_or_its_arguments_is_a_misuse() {
        let mut call = RvCall {
            size: size_field::<RvCall>(),
            verb: RV_PACK_RENDER,
            host: ptr::null(),
            plugin: ptr::null_mut(),
            pack_run: ptr::null_mut(),
            args: ptr::null(),
            tool_call: ptr::null_mut(),
        };
        // SAFETY: the entry point checks the instance pointer first.
        assert_eq!(unsafe { entry::<Maker>(&raw mut call) }, RV_ERR_MISUSE);
        let pack_run = Box::into_raw(Box::new(PackRun {
            pack: Box::new(Renders::Default),
        }));
        call.pack_run = pack_run.cast();
        // SAFETY: the entry point checks `args` before it reads through it.
        assert_eq!(unsafe { entry::<Maker>(&raw mut call) }, RV_ERR_MISUSE);
        // SAFETY: `pack_run` is the box leaked above, freed once.
        drop(unsafe { Box::from_raw(pack_run) });
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
