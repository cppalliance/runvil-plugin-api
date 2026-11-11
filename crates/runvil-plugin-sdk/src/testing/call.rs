//! The function a fake Host's `RvToolCallArgs` carries to end a call,
//! `finish`, and how the Harness judges what a step did.

use std::ffi::c_void;

use runvil_plugin_sys::{
    RV_OK, RV_PENDING, RV_RESULT_BACKEND, RV_RESULT_INVALID_ARGUMENTS, RV_RESULT_OTHER,
    RV_RESULT_TRANSPORT, RV_RESULT_TRUSTED, RV_RESULT_UNTRUSTED, RvStatus, RvToolResult,
};

use super::session::{Field, Session, Shape, TOOL_CALL, answer, field, shape};
use super::{Failure, ToolResult};
use crate::names::MAX_RESULT;
use crate::{ToolError, ToolErrorKind, ToolOutput};

const RESULT: Shape = shape!("finish", "result", "RvToolResult");
const TEXT: Field = field!("the result's text", 16_777_216);

// The literal above is the contract's bound on a result's text.
const _: () = assert!(MAX_RESULT == 16_777_216);

/// Records the result a step ends its call with, validated; returns
/// `RV_OK` or `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn finish(ctx: *mut c_void, result: *const RvToolResult) -> RvStatus {
    answer(ctx, &TOOL_CALL, |session| {
        if session.result.is_some() {
            return Err("finish was called twice");
        }
        // SAFETY: the contract makes `result` point at a readable
        // `RvToolResult` during this call.
        let result = unsafe { RESULT.read(result) }?;
        let failure = match result.kind {
            RV_RESULT_UNTRUSTED | RV_RESULT_TRUSTED => None,
            RV_RESULT_INVALID_ARGUMENTS => Some(ToolErrorKind::InvalidArguments),
            RV_RESULT_BACKEND => Some(ToolErrorKind::Backend),
            RV_RESULT_TRANSPORT => Some(ToolErrorKind::Transport),
            RV_RESULT_OTHER => Some(ToolErrorKind::Other),
            _ => return Err("the result's kind is not one of the RV_RESULT constants"),
        };
        // SAFETY: the contract makes the text valid for its length.
        let text = unsafe { TEXT.read(result.text) }?;
        session.result = Some(match failure {
            Some(kind) => Err(ToolError::new(kind, text)),
            None if result.kind == RV_RESULT_TRUSTED => Ok(ToolOutput::trusted(text)),
            None => Ok(ToolOutput::untrusted(text)),
        });
        Ok(RV_OK)
    })
}

/// What the Harness makes of a step that returned `status`: `None` for a
/// call that needs another step, its result once it ended, or why the
/// call failed, checked in the Harness's order: a misuse, then a result
/// beside `RV_PENDING`, then a status other than `RV_OK`, then no result.
pub(super) fn judge(
    status: RvStatus,
    session: &mut Session,
) -> Result<Option<ToolResult>, Failure> {
    if let Some(misuse) = session.misuse.take() {
        return Err(Failure::Misused(misuse));
    }
    match (status, session.result.take()) {
        (RV_PENDING, None) => Ok(None),
        (RV_PENDING, Some(_)) => Err(Failure::Misused(
            "a step called finish and then returned RV_PENDING",
        )),
        (RV_OK, Some(result)) => Ok(Some(result)),
        (RV_OK, None) => Err(Failure::Unfinished),
        (status, _) => Err(Failure::Status(status)),
    }
}

#[cfg(test)]
mod tests {
    //! The rules of a tool call's steps, through a raw entry point that
    //! breaks one of them at a time.

    use runvil_plugin_sys::RV_ERR_UNSUPPORTED;

    use crate::testing::Failure;
    use crate::testing::fixture::{self, Breach, CANCELS};
    use crate::{ToolError, ToolErrorKind, ToolOutput};

    #[test]
    fn a_call_that_follows_the_contract_ends_with_its_result_and_no_cancel() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        assert_eq!(
            pack.call("tool", "{}"),
            Ok(Ok(ToolOutput::untrusted("tool {}")))
        );
        assert_eq!(CANCELS.get(), 0);
    }

    #[test]
    fn each_result_kind_reads_back_as_its_output_or_failure() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        assert_eq!(
            pack.call("tool", "1"),
            Ok(Ok(ToolOutput::trusted("tool 1")))
        );
        for (arguments, kind) in [
            ("2", ToolErrorKind::InvalidArguments),
            ("3", ToolErrorKind::Backend),
            ("4", ToolErrorKind::Transport),
            ("5", ToolErrorKind::Other),
        ] {
            assert_eq!(
                pack.call("tool", arguments),
                Ok(Err(ToolError::new(kind, format!("tool {arguments}")))),
                "{arguments}"
            );
        }
    }

    #[test]
    fn a_pending_call_gets_more_steps_with_the_state_its_last_step_left() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        let mut call = pack.start("other", "{}");
        assert_eq!(call.step(), Ok(None));
        assert_eq!(call.step(), Ok(None));
        assert_eq!(
            call.step(),
            Ok(Some(Ok(ToolOutput::untrusted("other after 2 steps"))))
        );
        drop(call);
        assert_eq!(CANCELS.get(), 0, "a call that ended gets no cancel");
    }

    #[test]
    fn an_abandoned_pending_call_is_canceled_once_and_an_unsent_one_never() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        drop(pack.start("other", "{}"));
        assert_eq!(CANCELS.get(), 0, "no step was sent, so nothing is canceled");

        let mut call = pack.start("other", "{}");
        assert_eq!(call.step(), Ok(None));
        drop(call);
        assert_eq!(CANCELS.get(), 1);
    }

    #[test]
    fn each_breach_of_a_step_fails_the_call() {
        for (breach, failure, cancels) in [
            (
                Breach::FinishCtx,
                Failure::Misused(
                    "a tool call function was handed a ctx other than the open RV_TOOL_CALL step's",
                ),
                0,
            ),
            (
                Breach::FinishTwice,
                Failure::Misused("finish was called twice"),
                0,
            ),
            (
                Breach::Kind,
                Failure::Misused("the result's kind is not one of the RV_RESULT constants"),
                0,
            ),
            (Breach::NoFinish, Failure::Unfinished, 0),
            (Breach::CallStatus, Failure::Status(RV_ERR_UNSUPPORTED), 0),
            (
                Breach::FinishThenPending,
                Failure::Misused("a step called finish and then returned RV_PENDING"),
                1,
            ),
            (
                Breach::MisuseThenPending,
                Failure::Misused("finish was handed a NULL result"),
                1,
            ),
            (
                Breach::FileStatNull,
                Failure::Misused("file_stat was handed a NULL stat pointer"),
                0,
            ),
        ] {
            let loaded = fixture::load(breach).unwrap();
            let pack = loaded.new_pack("acme/fixture").unwrap();
            assert_eq!(pack.call("tool", "{}"), Err(failure), "{breach:?}");
            assert_eq!(
                CANCELS.get(),
                cancels,
                "{breach:?}: a failed call whose last step returned RV_PENDING is canceled"
            );
            CANCELS.set(0);
        }
    }

    #[test]
    #[should_panic(expected = "the Pack instance has no tool named absent")]
    fn calling_a_tool_the_instance_never_added_is_the_tests_own_fault() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        let _call = pack.start("absent", "{}");
    }

    #[test]
    #[should_panic(expected = "the call has ended and takes no more steps")]
    fn stepping_a_call_that_ended_is_the_tests_own_fault() {
        let loaded = fixture::load(Breach::None).unwrap();
        let pack = loaded.new_pack("acme/fixture").unwrap();
        let mut call = pack.start("tool", "{}");
        let _result = call.step();
        let _again = call.step();
    }
}
