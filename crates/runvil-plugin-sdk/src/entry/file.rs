//! The file requests of a tool call at the C boundary. [`receive`] copies
//! the answer a step carries into the call's `Requests`, before the
//! future that waits for it runs, because the answer is the Host's only
//! for that step. [`send`] hands the oldest request waiting there to the
//! Host, through the step's file functions.

use std::mem::size_of;
use std::slice;

use runvil_plugin_sys::{
    RV_ERR_FILE, RV_OK, RvFileAnswer, RvStat, RvStatus, RvStr, RvToolCallArgs,
};

use super::rv_str;
use crate::file::{Answer, Request, Requests};
use crate::{DirEntry, FileError, FileErrorKind, FileType, Stat};

/// Puts the answer the step `args` carries into `requests`, for the
/// future whose request is with the Host. A step that carries no answer
/// while a request is with the Host fails that future, and an answer to
/// no request is left unread.
///
/// # Safety
/// `args` belongs to the `RV_TOOL_CALL` step open on this thread: its
/// `answer` is NULL, or points to an `RvFileAnswer` that follows the
/// contract and stays valid, with what it points to, for this call.
pub(super) unsafe fn receive(requests: &Requests, args: &RvToolCallArgs) {
    if !requests.is_sent() {
        return;
    }
    // SAFETY: the caller vouches for `answer`.
    requests.answered(unsafe { copy(args.answer) });
}

/// A copy of what the answer at `answer` holds, or the failure it
/// reports. An answer that is missing or cut short, and one whose status
/// the contract gives no answer, is a failure that says so.
///
/// # Safety
/// `answer` is NULL, or points to an `RvFileAnswer` that follows the
/// contract and is valid, with what it points to, for this call.
unsafe fn copy(answer: *const RvFileAnswer) -> Result<Answer, FileError> {
    if answer.is_null() {
        return Err(FileError::new(
            FileErrorKind::Other,
            "the Host sent a step without the answer to the tool call's file request",
        ));
    }
    // SAFETY: `answer` is not NULL, and `size` is its first field, so it
    // is readable before the struct's length is known.
    let size = unsafe { answer.cast::<u32>().read() };
    if (size as usize) < size_of::<RvFileAnswer>() {
        return Err(FileError::new(
            FileErrorKind::Other,
            "the Host's answer to the file request is shorter than an RvFileAnswer",
        ));
    }
    // SAFETY: the struct is long enough for every field, and the Host
    // keeps it valid for the call and writes nothing to it meanwhile.
    let answer = unsafe { &*answer };
    match answer.status {
        RV_OK => {
            // SAFETY: the status is `RV_OK`, so `entries` holds that many
            // entries and `bytes` that many bytes, valid for the call.
            let (entries, bytes) = unsafe {
                (
                    lent(answer.entries.ptr, answer.entries.len),
                    lent(answer.bytes.ptr, answer.bytes.len),
                )
            };
            let entries = entries
                .iter()
                .map(|entry| {
                    // SAFETY: each entry's name is valid for the call.
                    let name = unsafe { text(entry.name) };
                    DirEntry::new(name, stat_of(entry.stat))
                })
                .collect();
            Ok(Answer {
                stat: stat_of(answer.stat),
                entries,
                bytes: bytes.to_vec(),
            })
        }
        RV_ERR_FILE => {
            // SAFETY: the status is `RV_ERR_FILE`, so `error` is filled,
            // and its text is valid for the call.
            let text = unsafe { text(answer.error.text) };
            let kind = FileErrorKind::from_raw(answer.error.kind);
            Err(FileError::new(kind, text))
        }
        other => Err(FileError::new(
            FileErrorKind::Other,
            format!("the Host answered the file request with status {other}"),
        )),
    }
}

/// `stat` as a tool reads it.
fn stat_of(stat: RvStat) -> Stat {
    Stat::new(FileType::from_raw(stat.file_type), stat.len)
}

/// Hands the oldest request waiting in `requests` to the Host, through
/// the file functions of the step `args`, unless a request is with the
/// Host already. A request the Host does not take fails its future with
/// the status the Host returned.
///
/// # Safety
/// `args` belongs to the `RV_TOOL_CALL` step open on this thread.
pub(super) unsafe fn send(requests: &Requests, args: &RvToolCallArgs) {
    requests.send(|request| {
        // SAFETY: the caller passes the open step's arguments.
        match unsafe { hand_over(args, request) } {
            RV_OK => Ok(()),
            status => Err(FileError::new(
                FileErrorKind::Other,
                format!("the Host answered the file function with status {status}"),
            )),
        }
    });
}

/// Calls the file function of the step `args` that takes `request`, and
/// returns the Host's status.
///
/// # Safety
/// `args` belongs to the `RV_TOOL_CALL` step open on this thread.
unsafe fn hand_over(args: &RvToolCallArgs, request: &Request) -> RvStatus {
    // SAFETY: the caller passes the open step's arguments, and the path
    // outlives the call that hands it over.
    unsafe {
        match request {
            Request::Stat(path) => (args.file_stat)(args.ctx, rv_str(path)),
            Request::List(path) => (args.file_list)(args.ctx, rv_str(path)),
            Request::Read { path, offset, len } => {
                (args.file_read)(args.ctx, rv_str(path), *offset, *len)
            }
        }
    }
}

/// The `len` values the Host lent at `ptr`, or none when `ptr` is NULL.
///
/// # Safety
/// When `ptr` is not NULL and `len` is nonzero, `ptr` must be valid for
/// reads of `len` values for the lifetime `'a`.
unsafe fn lent<'a, T>(ptr: *const T, len: usize) -> &'a [T] {
    if ptr.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: `ptr` is not NULL, and the caller guarantees it is valid
    // for `len` values.
    unsafe { slice::from_raw_parts(ptr, len) }
}

/// A copy of the text the Host lent in `s`. The contract makes it UTF-8;
/// a byte that is not is replaced, so a Host's mistake cannot make a
/// `String` that is not UTF-8.
///
/// # Safety
/// When `s.ptr` is not NULL and `s.len` is nonzero, `s.ptr` must be valid
/// for reads of `s.len` bytes.
unsafe fn text(s: RvStr) -> String {
    // SAFETY: the caller's guarantee is `lent`'s requirement.
    let bytes = unsafe { lent(s.ptr.cast::<u8>(), s.len) };
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::future::{Future, poll_fn};
    use std::pin::Pin;
    use std::ptr;
    use std::task::{Context, Poll, Waker};

    use runvil_plugin_sys::{
        RV_ERR_FILE, RV_ERR_MISUSE, RV_FILE_ERROR_DENIED, RV_FILE_TYPE_FILE, RV_OK, RV_PENDING,
        RvBytes, RvDirEntries, RvFileAnswer, RvFileError, RvStat, RvStatus, RvStr, RvToolCallArgs,
        RvToolResult,
    };

    use super::{receive, send, text};
    use crate::entry::{rv_str, size_field};
    use crate::testing::{self, Files};
    use crate::tool::CallState;
    use crate::{
        BoxFuture, DirEntry, FileError, FileErrorKind, FileType, Pack, PackInfo, Plugin, Refusal,
        Stat, ToolCall, ToolError, ToolErrorKind, ToolInfo, ToolOutput, next_step,
    };

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

    /// A Plugin whose tools report what the file methods resolve to.
    struct Reader;

    impl Plugin for Reader {
        const NAME: &'static str = "reader";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[PackInfo::new("acme/reader")];

        fn load() -> Result<Reader, Refusal> {
            Ok(Reader)
        }

        fn new_pack(&self, _pack: &str) -> Result<Box<dyn Pack>, Refusal> {
            Ok(Box::new(Tools))
        }
    }

    struct Tools;

    /// Polls `future` once, so that its request is queued, and goes on
    /// in the same step.
    async fn queue<T>(future: &mut (impl Future<Output = T> + Unpin)) {
        poll_fn(|cx| {
            assert!(Pin::new(&mut *future).poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
    }

    impl Pack for Tools {
        fn tools(&self) -> Vec<ToolInfo> {
            [
                "stat", "list", "read", "head", "twice", "both", "early", "leaves", "panics",
                "drops",
            ]
            .into_iter()
            .map(|name| ToolInfo::new(name, "A tool.", SCHEMA))
            .collect()
        }

        fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            // `early` makes its future here, before the call's own future
            // exists, and awaits it inside that future.
            let early = (call.tool() == "early").then(|| call.read(call.arguments()));
            Box::pin(async move {
                let path = call.arguments();
                let text = match call.tool() {
                    "early" => early
                        .expect("the future was made before the call's own")
                        .await
                        .map(|bytes| format!("{bytes:?}")),
                    "stat" => call.stat(path).await.map(|stat| format!("{stat:?}")),
                    "list" => call.list(path).await.map(|entries| format!("{entries:?}")),
                    "read" => call.read(path).await.map(|bytes| format!("{bytes:?}")),
                    "head" => call
                        .read_range(path, 1, 3)
                        .await
                        .map(|bytes| format!("{bytes:?}")),
                    "twice" => {
                        let first = call.read(path).await;
                        let second = call.read(path).await;
                        Ok(format!("{}", first == second && first.is_ok()))
                    }
                    "both" => {
                        // Both requests are queued in one step, and the
                        // Host takes one of them with each step.
                        let mut read = call.read(path);
                        let mut stat = call.stat(path);
                        queue(&mut read).await;
                        queue(&mut stat).await;
                        let stat = stat.await;
                        let read = read.await;
                        read.and_then(|bytes| Ok(format!("{bytes:?} {:?}", stat?)))
                    }
                    "leaves" => {
                        // The call ends in the step that queued a request.
                        let mut unsent = call.read(path);
                        queue(&mut unsent).await;
                        Ok("left".to_owned())
                    }
                    "drops" => {
                        // The first request goes to the Host, its future
                        // is dropped, and a second file is read.
                        let mut dropped = call.read("/notes/b.md");
                        queue(&mut dropped).await;
                        next_step().await;
                        drop(dropped);
                        call.read(path).await.map(|bytes| format!("{bytes:?}"))
                    }
                    _ => {
                        let mut unsent = call.read(path);
                        queue(&mut unsent).await;
                        panic!("boom after a request was queued");
                    }
                };
                text.map(ToolOutput::untrusted).map_err(|error| {
                    ToolError::new(
                        ToolErrorKind::Backend,
                        format!("{:?}: {}", error.kind(), error.text()),
                    )
                })
            })
        }
    }

    /// The tree every test below reads.
    fn files() -> Files {
        Files::new()
            .file("/notes/a.md", "hello")
            .file("/notes/b.md", "")
            .dir("/empty")
            .fails(
                "/secret",
                FileError::new(FileErrorKind::Denied, "the mode allows only /notes"),
            )
    }

    /// Calls `tool` on `path` and returns its output, or its failure as
    /// the kind and the text of the `FileError` behind it.
    fn run(tool: &str, path: &str) -> Result<String, String> {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        match pack
            .call(tool, path)
            .expect("the call follows the contract")
        {
            Ok(output) => Ok(output.text().to_owned()),
            Err(error) => Err(error.message().to_owned()),
        }
    }

    /// How many steps a call of `tool` on `path` takes to its end.
    fn steps(tool: &str, path: &str) -> usize {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        let mut call = pack.start(tool, path);
        let mut steps = 1;
        while call
            .step()
            .expect("the step follows the contract")
            .is_none()
        {
            steps += 1;
        }
        steps
    }

    #[test]
    fn stat_reports_what_a_path_names() {
        let file = Stat::new(FileType::File, 5);
        let directory = Stat::new(FileType::Directory, 0);
        assert_eq!(run("stat", "/notes/a.md"), Ok(format!("{file:?}")));
        assert_eq!(run("stat", "/notes"), Ok(format!("{directory:?}")));
        assert_eq!(run("stat", "/"), Ok(format!("{directory:?}")));
    }

    #[test]
    fn list_copies_each_entry_with_its_name_and_metadata() {
        let entries = vec![
            DirEntry::new("a.md", Stat::new(FileType::File, 5)),
            DirEntry::new("b.md", Stat::new(FileType::File, 0)),
        ];
        assert_eq!(run("list", "/notes"), Ok(format!("{entries:?}")));
        assert_eq!(run("list", "/empty"), Ok("[]".to_owned()));
    }

    #[test]
    fn read_copies_the_whole_file_and_read_range_a_part_of_it() {
        assert_eq!(run("read", "/notes/a.md"), Ok(format!("{:?}", b"hello")));
        assert_eq!(run("read", "/notes/b.md"), Ok("[]".to_owned()));
        assert_eq!(run("head", "/notes/a.md"), Ok(format!("{:?}", b"ell")));
    }

    #[test]
    fn a_path_without_a_leading_slash_is_taken_from_the_root() {
        assert_eq!(run("read", "notes/a.md"), Ok(format!("{:?}", b"hello")));
    }

    #[test]
    fn a_failed_operation_reaches_the_tool_with_its_kind_and_text() {
        assert_eq!(
            run("stat", "/absent"),
            Err("NotFound: not found: /absent".to_owned())
        );
        assert_eq!(
            run("list", "/notes/a.md"),
            Err("NotADirectory: not a directory: /notes/a.md".to_owned())
        );
        assert_eq!(
            run("read", "/notes"),
            Err("IsADirectory: is a directory: /notes".to_owned())
        );
        assert_eq!(
            run("read", "/secret"),
            Err("Denied: the mode allows only /notes".to_owned())
        );
    }

    #[test]
    fn a_path_longer_than_the_contract_allows_fails_without_a_misuse() {
        let long = "a".repeat(4097);
        assert_eq!(
            run("stat", &long),
            Err("Other: the path is longer than 4096 bytes".to_owned())
        );
        let longest = format!("/{}", "a".repeat(4095));
        assert_eq!(
            run("stat", &longest),
            Err(format!("NotFound: not found: {longest}"))
        );
    }

    #[test]
    fn each_request_ends_its_step_and_the_next_step_brings_the_answer() {
        assert_eq!(steps("read", "/notes/a.md"), 2);
        assert_eq!(steps("twice", "/notes/a.md"), 3);
        assert_eq!(run("twice", "/notes/a.md"), Ok("true".to_owned()));
    }

    #[test]
    fn requests_queued_in_one_step_go_to_the_host_one_with_each_step() {
        let file = Stat::new(FileType::File, 5);
        assert_eq!(
            run("both", "/notes/a.md"),
            Ok(format!("{:?} {file:?}", b"hello"))
        );
        assert_eq!(steps("both", "/notes/a.md"), 3);
    }

    #[test]
    fn a_future_made_before_the_calls_own_future_is_awaited_inside_it() {
        assert_eq!(run("early", "/notes/a.md"), Ok(format!("{:?}", b"hello")));
    }

    #[test]
    fn a_call_that_ends_with_a_request_queued_leaves_none_with_the_host() {
        // A request in the step that finishes would be a misuse.
        assert_eq!(run("leaves", "/notes/a.md"), Ok("left".to_owned()));
        assert_eq!(steps("leaves", "/notes/a.md"), 1);

        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        assert_eq!(
            pack.call("panics", "/notes/a.md"),
            Ok(Err(ToolError::new(
                ToolErrorKind::Other,
                "the tool panicked: boom after a request was queued"
            )))
        );
    }

    #[test]
    fn the_answer_to_a_dropped_future_reaches_no_other_future() {
        assert_eq!(run("drops", "/notes/a.md"), Ok(format!("{:?}", b"hello")));
    }

    unsafe extern "C" fn taking(_ctx: *mut c_void, _path: RvStr) -> RvStatus {
        RV_OK
    }

    unsafe extern "C" fn refusing(_ctx: *mut c_void, _path: RvStr) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_read(
        _ctx: *mut c_void,
        _path: RvStr,
        _offset: u64,
        _len: u64,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_finish(
        _ctx: *mut c_void,
        _result: *const RvToolResult,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    /// The arguments of a step whose `file_stat` is `file_stat`, and
    /// which carries `answer`.
    fn step_args(
        file_stat: unsafe extern "C" fn(*mut c_void, RvStr) -> RvStatus,
        answer: *const RvFileAnswer,
    ) -> RvToolCallArgs {
        RvToolCallArgs {
            size: size_field::<RvToolCallArgs>(),
            reserved: 0,
            ctx: ptr::null_mut(),
            tool: rv_str("tool"),
            arguments: rv_str("{}"),
            finish: unused_finish,
            file_stat,
            file_list: refusing,
            file_read: unused_read,
            answer,
        }
    }

    /// An answer that holds nothing but `status`.
    fn bare_answer(status: RvStatus) -> RvFileAnswer {
        RvFileAnswer {
            size: size_field::<RvFileAnswer>(),
            status,
            stat: RvStat {
                file_type: 0,
                reserved: 0,
                len: 0,
            },
            entries: RvDirEntries {
                ptr: ptr::null(),
                len: 0,
            },
            bytes: RvBytes {
                ptr: ptr::null(),
                len: 0,
            },
            error: RvFileError {
                kind: 0,
                reserved: 0,
                text: RvStr {
                    ptr: ptr::null(),
                    len: 0,
                },
            },
        }
    }

    /// Polls `future` once, as a step does.
    fn poll<T>(future: &mut (impl Future<Output = T> + Unpin)) -> Poll<T> {
        Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
    }

    /// What a `stat` resolves to when the Host takes its request and the
    /// next step carries `answer`.
    fn answered_with(answer: *const RvFileAnswer) -> Result<Stat, FileError> {
        let state = CallState::new("tool", "{}");
        let mut future = ToolCall::new(&state).stat("/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        // SAFETY: the arguments are whole, and their functions read
        // nothing.
        unsafe { send(&state.requests, &step_args(taking, ptr::null())) };
        // SAFETY: each test passes NULL or an answer that is whole and
        // outlives this call.
        unsafe { receive(&state.requests, &step_args(taking, answer)) };
        match poll(&mut future) {
            Poll::Ready(answer) => answer,
            Poll::Pending => panic!("the step brought no answer"),
        }
    }

    /// The failure of a `stat` whose answer broke the contract in the way
    /// `text` names.
    fn broken(text: &str) -> Result<Stat, FileError> {
        Err(FileError::new(FileErrorKind::Other, text))
    }

    #[test]
    fn a_request_the_host_does_not_take_fails_its_future_with_the_status() {
        let state = CallState::new("tool", "{}");
        let mut future = ToolCall::new(&state).stat("/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        // SAFETY: the arguments are whole, and their functions read
        // nothing.
        unsafe { send(&state.requests, &step_args(refusing, ptr::null())) };
        assert_eq!(
            poll(&mut future),
            Poll::Ready(broken("the Host answered the file function with status 4"))
        );
    }

    #[test]
    fn an_answer_is_copied_with_the_field_its_request_names() {
        let mut answer = bare_answer(RV_OK);
        answer.stat.file_type = RV_FILE_TYPE_FILE;
        answer.stat.len = 5;
        assert_eq!(
            answered_with(&raw const answer),
            Ok(Stat::new(FileType::File, 5))
        );
    }

    #[test]
    fn a_failed_answer_is_copied_with_its_kind_and_text() {
        let mut answer = bare_answer(RV_ERR_FILE);
        answer.error.kind = RV_FILE_ERROR_DENIED;
        answer.error.text = rv_str("no");
        assert_eq!(
            answered_with(&raw const answer),
            Err(FileError::new(FileErrorKind::Denied, "no"))
        );
    }

    #[test]
    fn a_step_without_the_answer_a_request_waits_for_fails_its_future() {
        assert_eq!(
            answered_with(ptr::null()),
            broken("the Host sent a step without the answer to the tool call's file request")
        );
    }

    #[test]
    fn an_answer_shorter_than_the_contracts_fails_its_future() {
        let mut answer = bare_answer(RV_OK);
        answer.size = 8;
        assert_eq!(
            answered_with(&raw const answer),
            broken("the Host's answer to the file request is shorter than an RvFileAnswer")
        );
    }

    #[test]
    fn an_answer_with_a_status_the_contract_gives_none_fails_its_future() {
        let answer = bare_answer(RV_PENDING);
        assert_eq!(
            answered_with(&raw const answer),
            broken("the Host answered the file request with status 7")
        );
    }

    #[test]
    fn an_answer_to_no_request_is_left_unread() {
        let state = CallState::new("tool", "{}");
        let mut answer = bare_answer(RV_OK);
        answer.stat.len = 5;
        // SAFETY: the arguments and the answer are whole.
        unsafe { receive(&state.requests, &step_args(taking, &raw const answer)) };
        let mut future = ToolCall::new(&state).stat("/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        assert_eq!(poll(&mut future), Poll::Pending);
    }

    #[test]
    fn text_that_is_not_utf8_is_copied_with_the_bad_bytes_replaced() {
        let bytes = [b'a', 0xff];
        let lent = RvStr {
            ptr: bytes.as_ptr().cast(),
            len: bytes.len(),
        };
        // SAFETY: `lent` points at `bytes`, which outlives the read.
        assert_eq!(unsafe { text(lent) }, "a\u{fffd}");
        let null = RvStr {
            ptr: ptr::null(),
            len: 3,
        };
        // SAFETY: a NULL pointer is read as no text.
        assert_eq!(unsafe { text(null) }, "");
    }
}
