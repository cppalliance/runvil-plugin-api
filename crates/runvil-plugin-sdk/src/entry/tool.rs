//! `RV_TOOL_CALL` and `RV_TOOL_CANCEL`: running the future of a tool call
//! one step at a time, ending the call with its result, and dropping the
//! future of a call the Host abandoned.

use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::atomic::Ordering;
use std::task::{Context, Poll, Waker};

use runvil_plugin_sys::{
    RV_ERR_MISUSE, RV_OK, RV_PENDING, RV_RESULT_BACKEND, RV_RESULT_INVALID_ARGUMENTS,
    RV_RESULT_OTHER, RV_RESULT_TRANSPORT, RV_RESULT_TRUSTED, RV_RESULT_UNTRUSTED, RvCall,
    RvResultKind, RvStatus, RvToolCallArgs, RvToolResult,
};

use super::pack::pack_of;
use super::{args, contain, panic_message, read_str, rv_str, size_field};
use crate::names::{MAX_RESULT, cut};
use crate::tool::CallState;
use crate::{BoxFuture, Pack, ToolCall, ToolError, ToolErrorKind, ToolOutput};

/// One tool call between its steps, behind `call->tool_call`.
struct Call {
    /// The future [`Pack::call`] returned. It borrows `state` and the Pack
    /// instance, and it is declared first, so it is dropped before `state`
    /// is freed.
    future: BoxFuture<'static, Result<ToolOutput, ToolError>>,
    /// The tool name and the arguments the future's [`ToolCall`] reads,
    /// and the step its file methods go through.
    state: OwnedState,
}

/// The owner of one call's [`CallState`]: frees it when dropped. A raw
/// pointer, not a `Box`, so that moving the owner leaves the future's
/// borrow of the state valid.
struct OwnedState(NonNull<CallState>);

impl OwnedState {
    /// Names `step` as the step that is running, or no step when it is
    /// `None`, for the file methods of the call's [`ToolCall`].
    fn set_step(&self, step: Option<&RvToolCallArgs>) {
        let step = step.map_or(ptr::null_mut(), |step| ptr::from_ref(step).cast_mut());
        // SAFETY: the allocation lives until this owner drops.
        unsafe { self.0.as_ref() }
            .step
            .store(step, Ordering::Release);
    }
}

impl Drop for OwnedState {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `Box::leak` in `Call::start`, and
        // this drop is its only free.
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

impl Call {
    /// Starts a call of `tool` on `pack` with `arguments`, in the step
    /// `step`, by making the future of [`Pack::call`].
    ///
    /// # Errors
    /// Returns the call's failure when `Pack::call` panics.
    fn start(
        pack: &'static dyn Pack,
        tool: &str,
        arguments: &str,
        step: &RvToolCallArgs,
    ) -> Result<Box<Call>, ToolError> {
        let state = OwnedState(NonNull::from(Box::leak(Box::new(CallState::new(
            tool, arguments,
        )))));
        // `Pack::call` runs inside the step, so it may read files.
        state.set_step(Some(step));
        // SAFETY: the allocation lives until `state` drops, which is after
        // the future that borrows it: on the panic path below no future
        // exists, and in a `Call` the future is the field dropped first.
        let lent: &'static CallState = unsafe { state.0.as_ref() };
        let future = panic::catch_unwind(AssertUnwindSafe(|| pack.call(ToolCall::new(lent))))
            .map_err(|payload| panicked(&*payload))?;
        state.set_step(None);
        Ok(Box::new(Call { future, state }))
    }

    /// Polls the future once, in the step `step`. A panic in it ends the
    /// call as a failure.
    fn poll(&mut self, step: &RvToolCallArgs) -> Poll<Result<ToolOutput, ToolError>> {
        // The Host sends the next step on its own, so nothing wakes.
        let mut cx = Context::from_waker(Waker::noop());
        self.state.set_step(Some(step));
        let polled = panic::catch_unwind(AssertUnwindSafe(|| self.future.as_mut().poll(&mut cx)))
            .unwrap_or_else(|payload| Poll::Ready(Err(panicked(&*payload))));
        // The step's arguments are gone once it returns, so nothing may
        // reach them after this.
        self.state.set_step(None);
        polled
    }
}

/// The failure of a call whose code panicked with `payload`.
fn panicked(payload: &(dyn std::any::Any + Send)) -> ToolError {
    ToolError::new(
        ToolErrorKind::Other,
        format!("the tool panicked: {}", panic_message(payload)),
    )
}

/// Answers `RV_TOOL_CALL`: starts the call when `call->tool_call` is
/// NULL, polls its future once, and either returns `RV_PENDING` with the
/// call's state in `call->tool_call`, or frees that state and ends the
/// call through the Host's `finish`. Every return other than `RV_PENDING`
/// leaves no state behind.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `pack_run` is NULL or a live
/// instance `RV_PACK_NEW` set, whose `tool_call` is NULL or the state the
/// call's last step left, and whose `args` is NULL or an `RvToolCallArgs`
/// that follows the contract.
pub(super) unsafe fn step(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let state = unsafe { (*call).tool_call }.cast::<Call>();
    // The state is taken out first, because every return below other than
    // `RV_PENDING` ends the call and so must free it.
    // SAFETY: a non-NULL `tool_call` is the `Box<Call>` the last step
    // leaked, and the Host runs one step of a call at a time.
    let open = (!state.is_null()).then(|| unsafe { Box::from_raw(state) });
    // SAFETY: as above; nothing else reads or writes the call meanwhile.
    unsafe { (&raw mut (*call).tool_call).write(ptr::null_mut()) };
    // SAFETY: for this verb `args` is NULL or an `RvToolCallArgs`.
    let args = match unsafe { args::<RvToolCallArgs>(call) } {
        Ok(args) => args,
        Err(status) => {
            contain(move || drop(open));
            return status;
        }
    };
    let mut open = match open {
        Some(open) => open,
        // SAFETY: the caller vouches for `pack_run` and for `args`.
        None => match unsafe { start(call, args) } {
            Ok(open) => open,
            Err(status) => return status,
        },
    };
    match open.poll(args) {
        Poll::Pending => {
            // SAFETY: as above.
            unsafe { (&raw mut (*call).tool_call).write(Box::into_raw(open).cast()) };
            RV_PENDING
        }
        Poll::Ready(result) => {
            contain(move || drop(open));
            // SAFETY: `args` belongs to the open step.
            unsafe { finish(args, &result) }
        }
    }
}

/// Starts the call `args` describes on the instance `call` names. When
/// the call cannot start, returns the status that ends it: `RV_ERR_MISUSE`
/// for a call with no instance or with text that is not UTF-8, and what
/// the Host's `finish` answers for a `Pack::call` that panicked.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `pack_run` is NULL or a live
/// instance, and `args` belongs to the step open on this thread.
unsafe fn start(call: *mut RvCall, args: &RvToolCallArgs) -> Result<Box<Call>, RvStatus> {
    // SAFETY: the caller makes `call` point to a whole `RvCall`, and the
    // Host deletes the instance only after every tool call of it has
    // ended, so the Pack outlives this call's future.
    let pack: &'static dyn Pack = unsafe { pack_of((*call).pack_run) }.ok_or(RV_ERR_MISUSE)?;
    // SAFETY: the Host keeps both strings valid for the step.
    let (tool, arguments) = unsafe { (read_str(args.tool), read_str(args.arguments)) };
    let (Some(tool), Some(arguments)) = (tool, arguments) else {
        return Err(RV_ERR_MISUSE);
    };
    // SAFETY: `args` belongs to the open step.
    Call::start(pack, tool, arguments, args).map_err(|error| unsafe { finish(args, &Err(error)) })
}

/// Ends the call through the Host's `finish` with `result`, and returns
/// what it answers. An output longer than a result may hold ends the call
/// as a failure that says so, and a failure's message is cut to fit.
///
/// # Safety
/// `args` belongs to the `RV_TOOL_CALL` step open on this thread.
unsafe fn finish(args: &RvToolCallArgs, result: &Result<ToolOutput, ToolError>) -> RvStatus {
    let too_long;
    let (kind, text) = match result {
        Ok(output) if output.text().len() > MAX_RESULT => {
            too_long = format!(
                "the tool's output is {} bytes, and a result holds at most {MAX_RESULT} bytes",
                output.text().len()
            );
            (RV_RESULT_OTHER, too_long.as_str())
        }
        Ok(output) if output.is_trusted() => (RV_RESULT_TRUSTED, output.text()),
        Ok(output) => (RV_RESULT_UNTRUSTED, output.text()),
        Err(error) => (kind_of(error.kind()), cut(error.message(), MAX_RESULT)),
    };
    let result = RvToolResult {
        size: size_field::<RvToolResult>(),
        kind,
        text: rv_str(text),
    };
    // SAFETY: the caller passes the open step's arguments, and `result`
    // and its text outlive the call.
    unsafe { (args.finish)(args.ctx, &raw const result) }
}

/// The contract's number for a failure of `kind`.
fn kind_of(kind: ToolErrorKind) -> RvResultKind {
    match kind {
        ToolErrorKind::InvalidArguments => RV_RESULT_INVALID_ARGUMENTS,
        ToolErrorKind::Backend => RV_RESULT_BACKEND,
        ToolErrorKind::Transport => RV_RESULT_TRANSPORT,
        ToolErrorKind::Other => RV_RESULT_OTHER,
    }
}

/// Answers `RV_TOOL_CANCEL`: drops the future of an abandoned call,
/// containing a panic in its `Drop`. A call whose steps left no state has
/// nothing to free.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `tool_call` is NULL or the
/// state the call's last step left, with no step of that call running.
pub(super) unsafe fn cancel(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let state = unsafe { (*call).tool_call }.cast::<Call>();
    if !state.is_null() {
        // SAFETY: a non-NULL `tool_call` is the `Box<Call>` the last step
        // leaked, and the Host cancels a call once, so this is its only
        // free.
        let open = unsafe { Box::from_raw(state) };
        contain(move || drop(open));
    }
    RV_OK
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

    use runvil_plugin_sys::{RV_ERR_MISUSE, RV_OK, RV_TOOL_CALL, RV_TOOL_CANCEL, RvCall};

    use crate::entry::{entry, size_field};
    use crate::names::MAX_RESULT;
    use crate::testing;
    use crate::{
        BoxFuture, Pack, PackInfo, Plugin, Refusal, ToolCall, ToolError, ToolErrorKind, ToolInfo,
        ToolOutput, next_step,
    };

    /// How many `Guard` values are alive: one sits in each `slow` call's
    /// future, so the count shows whether the future was dropped.
    static GUARDS_LIVE: AtomicIsize = AtomicIsize::new(0);
    /// How many steps `counted` calls have run.
    static COUNTED_STEPS: AtomicUsize = AtomicUsize::new(0);

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

    struct Tools;

    impl Plugin for Tools {
        const NAME: &'static str = "tools";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[PackInfo::new("acme/tools")];

        fn load() -> Result<Tools, Refusal> {
            Ok(Tools)
        }

        fn new_pack(&self, _pack: &str) -> Result<Box<dyn Pack>, Refusal> {
            Ok(Box::new(Instance {
                greeting: "hello".to_owned(),
            }))
        }
    }

    /// An instance with state of its own, which its calls borrow.
    struct Instance {
        greeting: String,
    }

    struct Guard;

    impl Guard {
        fn new() -> Guard {
            GUARDS_LIVE.fetch_add(1, Ordering::SeqCst);
            Guard
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            GUARDS_LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    impl Pack for Instance {
        fn tools(&self) -> Vec<ToolInfo> {
            [
                "echo",
                "page",
                "fails",
                "slow",
                "counted",
                "panics",
                "panics-early",
                "huge",
            ]
            .into_iter()
            .map(|name| ToolInfo::new(name, "A tool.", SCHEMA))
            .collect()
        }

        fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            assert!(call.tool() != "panics-early", "boom before the future");
            Box::pin(async move {
                match call.tool() {
                    "echo" => Ok(ToolOutput::trusted(format!(
                        "{} {} {}",
                        self.greeting,
                        call.tool(),
                        call.arguments()
                    ))),
                    "page" => Ok(ToolOutput::untrusted("text from outside")),
                    "fails" => Err(failure(call.arguments())),
                    "slow" => {
                        let _guard = Guard::new();
                        next_step().await;
                        next_step().await;
                        Ok(ToolOutput::trusted("done"))
                    }
                    "counted" => {
                        for _ in 0..3 {
                            COUNTED_STEPS.fetch_add(1, Ordering::SeqCst);
                            next_step().await;
                        }
                        COUNTED_STEPS.fetch_add(1, Ordering::SeqCst);
                        Ok(ToolOutput::trusted("counted"))
                    }
                    "huge" => Ok(ToolOutput::trusted("a".repeat(MAX_RESULT + 1))),
                    _ => panic!("boom"),
                }
            })
        }
    }

    /// The failure the `fails` tool returns for `arguments`, which names
    /// its kind.
    fn failure(arguments: &str) -> ToolError {
        let kind = match arguments {
            "\"invalid\"" => ToolErrorKind::InvalidArguments,
            "\"backend\"" => ToolErrorKind::Backend,
            "\"transport\"" => ToolErrorKind::Transport,
            _ => ToolErrorKind::Other,
        };
        ToolError::new(kind, format!("failed as {arguments}"))
    }

    #[test]
    fn a_call_gets_its_tool_and_arguments_and_borrows_its_instance() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(
            pack.call("echo", r#"{"a":1}"#),
            Ok(Ok(ToolOutput::trusted(r#"hello echo {"a":1}"#)))
        );
    }

    #[test]
    fn an_untrusted_output_stays_untrusted() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(
            pack.call("page", "{}"),
            Ok(Ok(ToolOutput::untrusted("text from outside")))
        );
    }

    #[test]
    fn a_failure_reaches_the_host_with_its_kind_and_message() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        for (arguments, kind) in [
            ("\"invalid\"", ToolErrorKind::InvalidArguments),
            ("\"backend\"", ToolErrorKind::Backend),
            ("\"transport\"", ToolErrorKind::Transport),
            ("\"other\"", ToolErrorKind::Other),
        ] {
            assert_eq!(
                pack.call("fails", arguments),
                Ok(Err(ToolError::new(kind, format!("failed as {arguments}")))),
                "{arguments}"
            );
        }
    }

    #[test]
    fn a_call_that_awaits_next_step_takes_one_step_more_for_each() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        let mut call = pack.start("counted", "{}");
        for steps in 1..=3 {
            assert_eq!(call.step(), Ok(None), "step {steps}");
            assert_eq!(COUNTED_STEPS.load(Ordering::SeqCst), steps);
        }
        assert_eq!(call.step(), Ok(Some(Ok(ToolOutput::trusted("counted")))));
        assert_eq!(COUNTED_STEPS.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn an_abandoned_call_drops_its_future_and_a_finished_one_already_has() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        let mut abandoned = pack.start("slow", "{}");
        assert_eq!(abandoned.step(), Ok(None));
        assert_eq!(GUARDS_LIVE.load(Ordering::SeqCst), 1);
        drop(abandoned);
        assert_eq!(GUARDS_LIVE.load(Ordering::SeqCst), 0);

        assert_eq!(pack.call("slow", "{}"), Ok(Ok(ToolOutput::trusted("done"))));
        assert_eq!(GUARDS_LIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_panic_in_a_call_ends_it_as_a_failure() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(
            pack.call("panics", "{}"),
            Ok(Err(ToolError::new(
                ToolErrorKind::Other,
                "the tool panicked: boom"
            )))
        );
        assert_eq!(
            pack.call("panics-early", "{}"),
            Ok(Err(ToolError::new(
                ToolErrorKind::Other,
                "the tool panicked: boom before the future"
            )))
        );
    }

    #[test]
    fn an_output_longer_than_a_result_holds_ends_the_call_as_a_failure() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(
            pack.call("huge", "{}"),
            Ok(Err(ToolError::new(
                ToolErrorKind::Other,
                "the tool's output is 16777217 bytes, and a result holds at most 16777216 bytes"
            )))
        );
    }

    #[test]
    fn a_step_without_arguments_is_a_misuse_and_a_cancel_without_state_is_fine() {
        let mut call = RvCall {
            size: size_field::<RvCall>(),
            verb: RV_TOOL_CALL,
            host: ptr::null(),
            plugin: ptr::null_mut(),
            pack_run: ptr::null_mut(),
            args: ptr::null(),
            tool_call: ptr::null_mut(),
        };
        // SAFETY: the entry point checks `args` before reading through it.
        assert_eq!(unsafe { entry::<Tools>(&raw mut call) }, RV_ERR_MISUSE);
        call.verb = RV_TOOL_CANCEL;
        // SAFETY: a cancel with no state reads nothing but the call.
        assert_eq!(unsafe { entry::<Tools>(&raw mut call) }, RV_OK);
    }
}
