//! `RV_TOOL_CALL` and `RV_TOOL_CANCEL`: running the future of a tool call
//! one step at a time, with a waker over the Host's that wakes the call,
//! taking its file requests to the Host and bringing the answers back,
//! ending the call with its result, and dropping the future of a call
//! the Host abandoned.

use std::panic::{self, AssertUnwindSafe};
use std::ptr::{self, NonNull};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

use runvil_plugin_sys::{
    RV_ERR_MISUSE, RV_MAX_RESULT, RV_OK, RV_PENDING, RV_RESULT_BACKEND,
    RV_RESULT_INVALID_ARGUMENTS, RV_RESULT_OTHER, RV_RESULT_TRANSPORT, RV_RESULT_TRUSTED,
    RV_RESULT_UNTRUSTED, RvCall, RvResultKind, RvStatus, RvToolCallArgs, RvToolResult, RvWaker,
};

use super::pack::pack_of;
use super::{args, contain, file, panic_message, read_str, rv_str, size_field};
use crate::file::Requests;
use crate::names::cut;
use crate::tool::CallState;
use crate::{BoxFuture, Pack, ToolCall, ToolError, ToolErrorKind, ToolOutput};

/// One tool call between its steps, behind `call->tool_call`.
struct Call {
    /// The future [`Pack::call`] returned. It borrows `state` and the Pack
    /// instance, and it is declared first, so it is dropped before `state`
    /// is freed.
    future: BoxFuture<'static, Result<ToolOutput, ToolError>>,
    /// The tool name, the arguments, and the Host's resources the
    /// future's [`ToolCall`] reads, and the file requests its file
    /// methods make.
    state: OwnedState,
    /// The waker every poll of the future gets, over the Host's waker
    /// for the call. Every step is handed the same one, so it is made
    /// once, from the first step's.
    waker: Waker,
}

/// The Host's waker for one call, in a form a Rust [`Waker`] can share
/// between threads. It needs no lock and never lets go of the Host's
/// waker: the Host keeps that valid until the Plugin unloads, so a wake
/// that comes after the call ended reaches live memory and does nothing
/// for that call.
struct CallWaker(RvWaker);

// SAFETY: the contract lets a Plugin call `wake` with the waker's `ctx`
// on any thread, until `RV_PLUGIN_UNLOAD` returns, and nothing here reads
// through `ctx`. So the pair may move to any thread of the Plugin, each
// of which the Plugin stops before its unload returns.
unsafe impl Send for CallWaker {}

// SAFETY: as above. The contract also lets `wake` run any number of
// times, on several threads at once, so the pair may be shared between
// threads too.
unsafe impl Sync for CallWaker {}

impl Wake for CallWaker {
    /// Calls the Host's `wake`.
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    /// Calls the Host's `wake`, without a clone of the `Arc`.
    fn wake_by_ref(self: &Arc<Self>) {
        // SAFETY: `wake` and `ctx` are the Host's waker for a call of
        // this Plugin. The contract makes the pair valid on any thread
        // until `RV_PLUGIN_UNLOAD` returns, and a Plugin holds no waker
        // past its unload. `wake` only notes the wake, so it does not
        // unwind.
        unsafe { (self.0.wake)(self.0.ctx) };
    }
}

/// The owner of one call's [`CallState`]: frees it when dropped. A raw
/// pointer, not a `Box`, so that moving the owner leaves the future's
/// borrow of the state valid.
struct OwnedState(NonNull<CallState>);

impl OwnedState {
    /// The file requests of the call, which each step carries to the
    /// Host and back.
    fn requests(&self) -> &Requests {
        // SAFETY: the allocation lives until this owner drops.
        &unsafe { self.0.as_ref() }.requests
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
    /// Starts a call on `pack` that keeps `state` and is woken through
    /// `waker`, the Host's waker for the call, by making the future of
    /// [`Pack::call`].
    ///
    /// # Errors
    /// Returns the call's failure when `Pack::call` panics.
    fn start(
        pack: &'static dyn Pack,
        state: CallState,
        waker: RvWaker,
    ) -> Result<Box<Call>, ToolError> {
        let state = OwnedState(NonNull::from(Box::leak(Box::new(state))));
        // SAFETY: the allocation lives until `state` drops, which is after
        // the future that borrows it: on the panic path below no future
        // exists, and in a `Call` the future is the field dropped first.
        let lent: &'static CallState = unsafe { state.0.as_ref() };
        let future = panic::catch_unwind(AssertUnwindSafe(|| pack.call(ToolCall::new(lent))))
            .map_err(|payload| panicked(&*payload))?;
        Ok(Box::new(Call {
            future,
            state,
            waker: Waker::from(Arc::new(CallWaker(waker))),
        }))
    }

    /// Polls the future once, with the call's waker, so that what the
    /// future waits on wakes the call when it is ready. A panic in the
    /// future ends the call as a failure.
    fn poll(&mut self) -> Poll<Result<ToolOutput, ToolError>> {
        let mut cx = Context::from_waker(&self.waker);
        panic::catch_unwind(AssertUnwindSafe(|| self.future.as_mut().poll(&mut cx)))
            .unwrap_or_else(|payload| Poll::Ready(Err(panicked(&*payload))))
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
/// NULL, takes in the answer the step carries, polls the call's future
/// once, and either hands the Host the call's next file request and
/// returns `RV_PENDING` with the call's state in `call->tool_call`, or
/// frees that state and ends the call through the Host's `finish`. Every
/// return other than `RV_PENDING` leaves no state behind.
///
/// `RV_PENDING` parks the call. The Host sends its next step when the
/// answer to the request is ready, or else when something wakes the
/// call. The future's own wakes go through the call's waker, and this
/// function wakes the call itself when the Host refuses a request.
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
    // The answer is valid only for this step, so it is copied before the
    // future that waits for it runs.
    // SAFETY: `args` belongs to the open step.
    unsafe { file::receive(open.state.requests(), args) };
    match open.poll() {
        Poll::Pending => {
            // A request goes out only after a poll that left the call
            // open, so a future that ended or panicked in that poll leaves
            // no request with the Host.
            // SAFETY: `args` belongs to the open step.
            let refused = unsafe { file::send(open.state.requests(), args) };
            if refused {
                // The refusal is the answer the request's future gets,
                // and it is here already. No request is with the Host,
                // so no answer brings the step that delivers it, and the
                // call would park for good. The lock on the requests is
                // let go by now, so none is held across the wake.
                open.waker.wake_by_ref();
            }
            // SAFETY: nothing else reads or writes the call meanwhile.
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

/// Starts the call `args` describes on the instance `call` names, with
/// copies of the tool name, the arguments, and the Host's resources,
/// which the Host lends for this step only, and with the waker every
/// step of the call is handed. When the call cannot start, returns the
/// status that ends it: `RV_ERR_MISUSE` for a call with no instance or
/// with text that is not UTF-8, and what the Host's `finish` answers for
/// a `Pack::call` that panicked.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `pack_run` is NULL or a live
/// instance, and `args` belongs to the step open on this thread.
unsafe fn start(call: *mut RvCall, args: &RvToolCallArgs) -> Result<Box<Call>, RvStatus> {
    // SAFETY: the caller makes `call` point to a whole `RvCall`, and the
    // Host deletes the instance only after every tool call of it has
    // ended, so the Pack outlives this call's future.
    let pack: &'static dyn Pack = unsafe { pack_of((*call).pack_run) }.ok_or(RV_ERR_MISUSE)?;
    // SAFETY: the Host keeps the three strings valid for the step.
    let texts = unsafe {
        (
            read_str(args.tool),
            read_str(args.arguments),
            read_str(args.resources),
        )
    };
    let (Some(tool), Some(arguments), Some(resources)) = texts else {
        return Err(RV_ERR_MISUSE);
    };
    let state = CallState::new(tool, arguments, resources);
    // SAFETY: `args` belongs to the open step.
    Call::start(pack, state, args.waker).map_err(|error| unsafe { finish(args, &Err(error)) })
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
        Ok(output) if output.text().len() > RV_MAX_RESULT => {
            too_long = format!(
                "the tool's output is {} bytes, and a result holds at most {RV_MAX_RESULT} bytes",
                output.text().len()
            );
            (RV_RESULT_OTHER, too_long.as_str())
        }
        Ok(output) if output.is_trusted() => (RV_RESULT_TRUSTED, output.text()),
        Ok(output) => (RV_RESULT_UNTRUSTED, output.text()),
        Err(error) => (kind_of(error.kind()), cut(error.message(), RV_MAX_RESULT)),
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
    use std::future::{Future, pending, poll_fn};
    use std::pin::Pin;
    use std::ptr;
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll, Waker};
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use runvil_plugin_sys::{
        RV_ERR_MISUSE, RV_MAX_RESULT, RV_OK, RV_TOOL_CALL, RV_TOOL_CANCEL, RvCall, RvStatus, RvStr,
        RvToolCallArgs,
    };

    use crate::entry::{entry, size_field};
    use crate::testing::{self, Failure};
    use crate::{
        BoxFuture, Pack, PackInfo, Plugin, Refusal, ToolCall, ToolError, ToolErrorKind, ToolInfo,
        ToolOutput, next_step,
    };

    /// How many `Guard` values are alive: one sits in each `slow` call's
    /// future, so the count shows whether the future was dropped.
    static GUARDS_LIVE: AtomicIsize = AtomicIsize::new(0);
    /// How many steps `counted` calls have run.
    static COUNTED_STEPS: AtomicUsize = AtomicUsize::new(0);
    /// How many times the guard of a `guarded` call woke its call as it
    /// was dropped.
    static DROP_WAKES: AtomicUsize = AtomicUsize::new(0);
    /// The waker the `keeps` call kept, which a test wakes after the call
    /// has ended.
    static KEPT: Mutex<Option<Waker>> = Mutex::new(None);

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

    /// Resources that hold a key, which no `Debug` may print.
    const RESOURCES: &str = r#"{"acme/search":{"api_key":"hunter2"}}"#;

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

    /// Wakes the task it was made in when it is dropped, as the sender of
    /// a channel wakes its receiver.
    struct WakesOnDrop(Waker);

    impl Drop for WakesOnDrop {
        fn drop(&mut self) {
            self.0.wake_by_ref();
            DROP_WAKES.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// The future of the `handoff` tool. Its first poll hands the task's
    /// waker to a thread, which wakes the call a while later. Its next
    /// poll says whether that wake had come by then.
    #[derive(Default)]
    struct Handoff {
        /// The thread that wakes the call, once the first poll started it.
        waking: Option<JoinHandle<()>>,
        /// Whether that thread has come to its wake.
        released: Arc<AtomicBool>,
    }

    impl Future for Handoff {
        type Output = &'static str;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<&'static str> {
            let Some(waking) = self.waking.take() else {
                let waker = cx.waker().clone();
                let released = Arc::clone(&self.released);
                self.waking = Some(thread::spawn(move || {
                    thread::sleep(Duration::from_millis(50));
                    released.store(true, Ordering::SeqCst);
                    waker.wake();
                }));
                return Poll::Pending;
            };
            let woken = self.released.load(Ordering::SeqCst);
            waking.join().unwrap();
            Poll::Ready(if woken {
                "woken"
            } else {
                "stepped before the wake"
            })
        }
    }

    impl Drop for Handoff {
        /// Joins the thread, as a Plugin stops each of its threads before
        /// it is unloaded.
        fn drop(&mut self) {
            if let Some(waking) = self.waking.take() {
                waking.join().unwrap();
            }
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
                "resources",
                "debug",
                "paced",
                "handoff",
                "keeps",
                "guarded",
                "idle",
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
                    "huge" => Ok(ToolOutput::trusted("a".repeat(RV_MAX_RESULT + 1))),
                    "panics" => panic!("boom"),
                    _ => Ok(ToolOutput::trusted(waits(call).await)),
                }
            })
        }
    }

    /// What a tool that reads the Host's resources, or waits for a wake,
    /// answers for `call`.
    async fn waits(call: ToolCall<'_>) -> String {
        match call.tool() {
            "resources" => {
                // The text is read in a later step than the one the Host
                // lent it for.
                next_step().await;
                call.resources().to_owned()
            }
            "debug" => format!("{call:?}"),
            "paced" => {
                for _ in 0..3 {
                    next_step().await;
                }
                "paced".to_owned()
            }
            "handoff" => Handoff::default().await.to_owned(),
            "keeps" => {
                poll_fn(|cx| {
                    *KEPT.lock().unwrap() = Some(cx.waker().clone());
                    Poll::Ready(())
                })
                .await;
                "kept".to_owned()
            }
            "guarded" => {
                let _guard = poll_fn(|cx| Poll::Ready(WakesOnDrop(cx.waker().clone()))).await;
                pending::<()>().await;
                "never".to_owned()
            }
            _ => {
                pending::<()>().await;
                "never".to_owned()
            }
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
    fn a_call_that_awaits_next_step_wakes_itself_for_each_next_step() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        let mut call = pack.start("paced", "{}");
        for step in 1..=3 {
            assert_eq!(call.step(), Ok(None), "step {step}");
            assert!(call.is_woken(), "step {step} asks for the one after it");
            call.wait();
        }
        assert_eq!(call.step(), Ok(Some(Ok(ToolOutput::trusted("paced")))));
        // `call` waits for a wake after each step that parks the call, so
        // it comes to the result only when each wake came.
        assert_eq!(
            pack.call("paced", "{}"),
            Ok(Ok(ToolOutput::trusted("paced")))
        );
    }

    #[test]
    fn a_call_stays_parked_until_another_thread_wakes_it() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        // `call` sends the second step when its wait ends, and the tool
        // says whether the wake had come by then.
        assert_eq!(
            pack.call("handoff", "{}"),
            Ok(Ok(ToolOutput::trusted("woken")))
        );
    }

    #[test]
    fn a_destructor_may_wake_its_call_inside_a_cancel() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        let mut call = pack.start("guarded", "{}");
        assert_eq!(call.step(), Ok(None));
        assert!(!call.is_woken(), "the call waits on nothing that wakes");
        assert_eq!(DROP_WAKES.load(Ordering::SeqCst), 0);
        // The cancel drops the future on the Host's own thread, and the
        // guard in it wakes the call as it goes. A Host that held a lock
        // of its `wake` across the cancel would block here for good.
        drop(call);
        assert_eq!(DROP_WAKES.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_waker_kept_past_its_call_wakes_no_other_call_and_harms_nothing() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(
            pack.call("keeps", "{}"),
            Ok(Ok(ToolOutput::trusted("kept")))
        );
        let late = KEPT.lock().unwrap().take().expect("the call kept a waker");
        // The Host keeps the waker valid until the unload, so each wake
        // below reaches live memory.
        late.wake_by_ref();
        let mut parked = pack.start("idle", "{}");
        assert_eq!(parked.step(), Ok(None));
        late.wake_by_ref();
        assert!(!parked.is_woken(), "the late wake is not this call's");
        drop(parked);
        assert_eq!(
            pack.call("echo", "1"),
            Ok(Ok(ToolOutput::trusted("hello echo 1")))
        );
        drop(pack);
        // The instance is deleted, and the Plugin is still loaded.
        late.wake();
    }

    #[test]
    fn a_call_is_lent_the_hosts_resources_and_an_empty_object_by_default() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(
            pack.call("resources", "{}"),
            Ok(Ok(ToolOutput::trusted("{}")))
        );
        let pack = pack.with_resources(RESOURCES);
        assert_eq!(
            pack.call("resources", "{}"),
            Ok(Ok(ToolOutput::trusted(RESOURCES)))
        );
    }

    #[test]
    fn no_debug_text_of_a_call_or_its_run_holds_the_resources() {
        let loaded = testing::load::<Tools>().unwrap();
        let pack = loaded
            .new_pack("acme/tools")
            .unwrap()
            .with_resources(RESOURCES);
        let lent = pack.call("debug", "{}").unwrap().unwrap();
        let call = pack.start("debug", "{}");
        for shown in [
            lent.text().to_owned(),
            format!("{pack:?}"),
            format!("{call:?}"),
        ] {
            assert!(shown.contains("Resources { .. }"), "{shown}");
            assert!(!shown.contains("hunter2"), "{shown}");
            assert!(!shown.contains("acme/search"), "{shown}");
        }
    }

    /// Bytes that are not UTF-8.
    static NOT_UTF8: [u8; 2] = [0xff, 0xfe];

    /// The entry point of [`Tools`], behind a Host that lends every step
    /// resources that are not UTF-8.
    unsafe extern "C" fn garbled(call: *mut RvCall) -> RvStatus {
        // SAFETY: the fake Host made `call`, with an `RvToolCallArgs`
        // behind `args` for a step. The copy lent in its place lives
        // until the entry point has returned, and the Host's own is put
        // back then.
        unsafe {
            if (*call).verb != RV_TOOL_CALL {
                return entry::<Tools>(call);
            }
            let lent = (*call).args;
            let args = RvToolCallArgs {
                resources: RvStr {
                    ptr: NOT_UTF8.as_ptr().cast(),
                    len: NOT_UTF8.len(),
                },
                ..lent.cast::<RvToolCallArgs>().read()
            };
            (*call).args = (&raw const args).cast();
            let status = entry::<Tools>(call);
            (*call).args = lent;
            status
        }
    }

    #[test]
    fn resources_that_are_not_utf8_are_a_misuse() {
        // SAFETY: `garbled` follows the contract apart from the text this
        // test checks, and never unwinds.
        let loaded = unsafe { testing::load_entry(garbled) }.unwrap();
        let pack = loaded.new_pack("acme/tools").unwrap();
        assert_eq!(pack.call("echo", "{}"), Err(Failure::Status(RV_ERR_MISUSE)));
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
