//! The file requests of a tool call at the C boundary. [`receive`] copies
//! the answer a step carries into the call's `Requests`, before the
//! future that waits for it runs, because the answer is the Host's only
//! for that step. [`send`] hands the oldest request waiting there to the
//! Host, through the step's file functions.

use std::mem::size_of;
use std::slice;

use runvil_plugin_sys::{
    RV_ERR_FILE, RV_OK, RvBytes, RvFileAnswer, RvStat, RvStatus, RvStr, RvToolCallArgs,
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
/// Returns whether the Host refused a request. The failure is then the
/// answer its future gets, and no step is due to bring it, so the step
/// wakes its call.
///
/// # Safety
/// `args` belongs to the `RV_TOOL_CALL` step open on this thread.
pub(super) unsafe fn send(requests: &Requests, args: &RvToolCallArgs) -> bool {
    requests.send(|request| {
        // SAFETY: the caller passes the open step's arguments.
        match unsafe { hand_over(args, request) } {
            RV_OK => Ok(()),
            status => Err(FileError::new(
                FileErrorKind::Other,
                format!("the Host answered the file function with status {status}"),
            )),
        }
    })
}

/// Calls the file function of the step `args` that takes `request`, and
/// returns the Host's status.
///
/// # Safety
/// `args` belongs to the `RV_TOOL_CALL` step open on this thread.
unsafe fn hand_over(args: &RvToolCallArgs, request: &Request) -> RvStatus {
    let ctx = args.ctx;
    // SAFETY: the caller passes the open step's arguments, and the paths
    // and the bytes outlive the call that hands them over.
    unsafe {
        match request {
            Request::Stat(path) => (args.file_stat)(ctx, rv_str(path)),
            Request::List(path) => (args.file_list)(ctx, rv_str(path)),
            Request::Read { path, offset, len } => {
                (args.file_read)(ctx, rv_str(path), *offset, *len)
            }
            Request::Write { path, bytes } => (args.file_write)(ctx, rv_str(path), rv_bytes(bytes)),
            Request::Append { path, bytes } => {
                (args.file_append)(ctx, rv_str(path), rv_bytes(bytes))
            }
            Request::Mkdir { path, recursive } => {
                (args.file_mkdir)(ctx, rv_str(path), u32::from(*recursive))
            }
            Request::Remove { path, recursive } => {
                (args.file_remove)(ctx, rv_str(path), u32::from(*recursive))
            }
            Request::Rename { from, to } => (args.file_rename)(ctx, rv_str(from), rv_str(to)),
            Request::Copy { from, to } => (args.file_copy)(ctx, rv_str(from), rv_str(to)),
        }
    }
}

/// `bytes` as the contract's pointer and length.
fn rv_bytes(bytes: &[u8]) -> RvBytes {
    RvBytes {
        ptr: bytes.as_ptr(),
        len: bytes.len(),
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
    use std::future::{Future, pending, poll_fn};
    use std::pin::Pin;
    use std::ptr;
    use std::task::{Context, Poll, Waker};

    use runvil_plugin_sys::{
        RV_ERR_FILE, RV_ERR_MISUSE, RV_FILE_ERROR_DENIED, RV_FILE_TYPE_FILE, RV_OK, RV_PENDING,
        RV_TOOL_CALL, RvBytes, RvCall, RvDirEntries, RvFileAnswer, RvFileError, RvStat, RvStatus,
        RvStr, RvToolCallArgs, RvToolResult, RvWaker,
    };

    use super::{receive, send, text};
    use crate::entry::{entry, rv_str, size_field};
    use crate::testing::{self, Files};
    use crate::tool::CallState;
    use crate::{
        BoxFuture, DirEntry, FileError, FileErrorKind, FileType, Pack, PackInfo, Plugin, Refusal,
        Stat, ToolCall, ToolError, ToolErrorKind, ToolInfo, ToolOutput, next_step,
    };

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

    /// The tools that read, each named for what it does with the path in
    /// its arguments.
    const READS: [&str; 10] = [
        "stat", "list", "read", "head", "twice", "both", "early", "drops", "holds", "panics",
    ];

    /// The tools that change files. `rename` and `copy` take two paths
    /// with a space between them.
    const CHANGES: [&str; 11] = [
        "write",
        "append",
        "mkdir",
        "mkdirs",
        "remove",
        "remove-all",
        "rename",
        "copy",
        "huge",
        "edit",
        "leaves",
    ];

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
            let reads = READS.map(|name| ToolInfo::new(name, "A tool.", SCHEMA));
            let changes =
                CHANGES.map(|name| ToolInfo::new(name, "A tool.", SCHEMA).changes_files());
            reads.into_iter().chain(changes).collect()
        }

        fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            // `early` makes its future here, before the call's own future
            // exists, and awaits it inside that future.
            let early = (call.tool() == "early").then(|| call.read(call.arguments()));
            Box::pin(async move {
                let text = match early {
                    Some(early) => early.await.map(|bytes| format!("{bytes:?}")),
                    None if READS.contains(&call.tool()) => reads(call).await,
                    None => changes(call).await.map(|()| "done".to_owned()),
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

    /// What a tool that reads answers for `call`.
    async fn reads(call: ToolCall<'_>) -> Result<String, FileError> {
        let path = call.arguments();
        match call.tool() {
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
                // Both requests are queued in one step, and the Host takes
                // one of them with each step.
                let mut read = call.read(path);
                let mut stat = call.stat(path);
                queue(&mut read).await;
                queue(&mut stat).await;
                let stat = stat.await;
                let read = read.await;
                read.and_then(|bytes| Ok(format!("{bytes:?} {:?}", stat?)))
            }
            "drops" => {
                // The first request goes to the Host, its future is
                // dropped, and a second file is read.
                let mut dropped = call.read("/notes/b.md");
                queue(&mut dropped).await;
                next_step().await;
                drop(dropped);
                call.read(path).await.map(|bytes| format!("{bytes:?}"))
            }
            "holds" => {
                // The request goes to the Host, and the call then waits
                // on something that never comes, so the step that brings
                // the answer leaves it untaken.
                let mut held = call.stat(path);
                queue(&mut held).await;
                pending::<()>().await;
                held.await.map(|stat| format!("{stat:?}"))
            }
            _ => {
                let mut unsent = call.read(path);
                queue(&mut unsent).await;
                panic!("boom after a request was queued");
            }
        }
    }

    /// Makes the change the tool of `call` is named for.
    async fn changes(call: ToolCall<'_>) -> Result<(), FileError> {
        let path = call.arguments();
        let (from, to) = path.split_once(' ').unwrap_or((path, ""));
        match call.tool() {
            "write" => call.write(path, "written").await,
            "append" => call.append(path, " more").await,
            "mkdir" => call.mkdir(path, false).await,
            "mkdirs" => call.mkdir(path, true).await,
            "remove" => call.remove(path, false).await,
            "remove-all" => call.remove(path, true).await,
            "rename" => call.rename(from, to).await,
            "copy" => call.copy(from, to).await,
            "huge" => call.write(path, vec![0; 16_777_217]).await,
            "edit" => {
                let mut bytes = call.read(path).await?;
                bytes.push(b'!');
                call.write(path, bytes).await
            }
            _ => {
                // The call ends in the step that queued a change.
                let mut unsent = call.write(path, "never written");
                queue(&mut unsent).await;
                Ok(())
            }
        }
    }

    /// The tree every test below starts from.
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

    /// Calls `tool` with `arguments` and returns its output, or its
    /// failure as the kind and the text of the `FileError` behind it,
    /// beside the run's files as the call left them.
    fn ran(tool: &str, arguments: &str) -> (Result<String, String>, Files) {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        let result = match pack
            .call(tool, arguments)
            .expect("the call follows the contract")
        {
            Ok(output) => Ok(output.text().to_owned()),
            Err(error) => Err(error.message().to_owned()),
        };
        (result, pack.files().clone())
    }

    /// What a call of `tool` on `path` answers; see [`ran`].
    fn run(tool: &str, path: &str) -> Result<String, String> {
        ran(tool, path).0
    }

    /// The files a call of `tool` with `arguments` leaves, when the call
    /// succeeds.
    fn left(tool: &str, arguments: &str) -> Files {
        let (result, files) = ran(tool, arguments);
        assert_eq!(result, Ok("done".to_owned()), "{tool} {arguments}");
        files
    }

    /// The whole contents of the file at `path` in `files`.
    fn contents(files: &Files, path: &str) -> Result<Vec<u8>, FileError> {
        files.read(path, 0, u64::MAX)
    }

    /// The failure of an operation that found nothing at `path`.
    fn absent<T>(path: &str) -> Result<T, FileError> {
        Err(FileError::new(
            FileErrorKind::NotFound,
            format!("not found: {path}"),
        ))
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
        // A request in the step that finishes would be a misuse, and the
        // write it asked for would show in the files.
        assert_eq!(left("leaves", "/notes/a.md"), files());
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

    #[test]
    fn write_and_append_change_the_contents_of_a_file() {
        let files = left("write", "/notes/new/c.md");
        assert_eq!(contents(&files, "/notes/new/c.md"), Ok(b"written".to_vec()));
        let files = left("write", "notes/a.md");
        assert_eq!(contents(&files, "/notes/a.md"), Ok(b"written".to_vec()));
        let files = left("append", "/notes/a.md");
        assert_eq!(contents(&files, "/notes/a.md"), Ok(b"hello more".to_vec()));
        let files = left("append", "/log.txt");
        assert_eq!(contents(&files, "/log.txt"), Ok(b" more".to_vec()));
    }

    #[test]
    fn mkdir_and_remove_take_their_recursive_flag_to_the_host() {
        let directory = Ok(Stat::new(FileType::Directory, 0));
        assert_eq!(left("mkdir", "/made").stat("/made"), directory);
        assert_eq!(
            run("mkdir", "/a/b"),
            Err("NotFound: not found: /a/b".to_owned())
        );
        assert_eq!(left("mkdirs", "/a/b").stat("/a/b"), directory);

        let files = left("remove", "/notes/a.md");
        assert_eq!(files.stat("/notes/a.md"), absent("/notes/a.md"));
        assert_eq!(
            run("remove", "/notes"),
            Err("NotEmpty: directory not empty: /notes".to_owned())
        );
        assert_eq!(
            left("remove-all", "/notes").stat("/notes"),
            absent("/notes")
        );
    }

    #[test]
    fn rename_and_copy_take_both_of_their_paths_to_the_host() {
        let files = left("rename", "/notes/a.md /moved/a.md");
        assert_eq!(contents(&files, "/moved/a.md"), Ok(b"hello".to_vec()));
        assert_eq!(files.stat("/notes/a.md"), absent("/notes/a.md"));

        let files = left("copy", "/notes/a.md /copied/a.md");
        assert_eq!(contents(&files, "/copied/a.md"), Ok(b"hello".to_vec()));
        assert_eq!(contents(&files, "/notes/a.md"), Ok(b"hello".to_vec()));
    }

    #[test]
    fn a_change_that_fails_reaches_the_tool_with_its_kind_and_text() {
        for (tool, arguments, failure) in [
            ("write", "/secret", "Denied: the mode allows only /notes"),
            ("write", "/notes", "IsADirectory: is a directory: /notes"),
            ("append", "/secret", "Denied: the mode allows only /notes"),
            ("mkdir", "/notes", "AlreadyExists: already exists: /notes"),
            ("remove", "/absent", "NotFound: not found: /absent"),
            ("rename", "/absent /x", "NotFound: not found: /absent"),
            ("copy", "/notes /x", "IsADirectory: is a directory: /notes"),
        ] {
            let (result, after) = ran(tool, arguments);
            assert_eq!(result, Err(failure.to_owned()), "{tool} {arguments}");
            assert_eq!(after, files(), "{tool} {arguments} left the files alone");
        }
    }

    #[test]
    fn bytes_longer_than_one_write_takes_fail_without_a_misuse() {
        assert_eq!(
            run("huge", "/notes/a.md"),
            Err("Other: the bytes are longer than 16777216 bytes".to_owned())
        );
    }

    #[test]
    fn a_read_and_a_write_of_one_call_take_a_step_each() {
        let files = left("edit", "/notes/a.md");
        assert_eq!(contents(&files, "/notes/a.md"), Ok(b"hello!".to_vec()));
        assert_eq!(steps("edit", "/notes/a.md"), 3);
    }

    #[test]
    fn the_calls_of_one_run_read_what_its_earlier_calls_wrote() {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        assert!(pack.call("write", "/notes/c.md").unwrap().is_ok());
        assert_eq!(
            pack.call("read", "/notes/c.md"),
            Ok(Ok(ToolOutput::untrusted(format!("{:?}", b"written"))))
        );
    }

    #[test]
    fn a_change_the_host_took_has_happened_when_its_call_is_abandoned() {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        let mut call = pack.start("write", "/notes/c.md");
        assert_eq!(call.step(), Ok(None));
        drop(call);
        assert_eq!(
            contents(pack.files(), "/notes/c.md"),
            Ok(b"written".to_vec())
        );
    }

    #[test]
    fn each_tool_that_changes_files_says_so_to_the_host() {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap();
        for tool in pack.tools() {
            assert_eq!(
                tool.changes_files,
                CHANGES.contains(&tool.name.as_str()),
                "{}",
                tool.name
            );
        }
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

    unsafe extern "C" fn unused_bytes(
        _ctx: *mut c_void,
        _path: RvStr,
        _bytes: RvBytes,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_flag(_ctx: *mut c_void, _path: RvStr, _recursive: u32) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_pair(_ctx: *mut c_void, _from: RvStr, _to: RvStr) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_wake(_ctx: *mut c_void) {}

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
            file_write: unused_bytes,
            file_append: unused_bytes,
            file_mkdir: unused_flag,
            file_remove: unused_flag,
            file_rename: unused_pair,
            file_copy: unused_pair,
            resources: rv_str("{}"),
            waker: RvWaker {
                ctx: ptr::null_mut(),
                wake: unused_wake,
            },
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

    /// Polls `future` once, as a step does, with a waker that does
    /// nothing, because a file future registers none.
    fn poll<T>(future: &mut (impl Future<Output = T> + Unpin)) -> Poll<T> {
        Pin::new(future).poll(&mut Context::from_waker(Waker::noop()))
    }

    /// What a `stat` resolves to when the Host takes its request and the
    /// next step carries `answer`.
    fn answered_with(answer: *const RvFileAnswer) -> Result<Stat, FileError> {
        let state = CallState::new("tool", "{}", "{}");
        let mut future = ToolCall::new(&state).stat("/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        // SAFETY: the arguments are whole, and their functions read
        // nothing.
        let refused = unsafe { send(&state.requests, &step_args(taking, ptr::null())) };
        assert!(!refused, "the Host took the request");
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
        let state = CallState::new("tool", "{}", "{}");
        let mut future = ToolCall::new(&state).stat("/a");
        assert_eq!(poll(&mut future), Poll::Pending);
        // SAFETY: the arguments are whole, and their functions read
        // nothing.
        let refused = unsafe { send(&state.requests, &step_args(refusing, ptr::null())) };
        assert!(refused, "the step is told, so that it wakes its call");
        assert_eq!(
            poll(&mut future),
            Poll::Ready(broken("the Host answered the file function with status 4"))
        );
    }

    /// The entry point of [`Reader`], behind a Host that takes no
    /// `file_stat` request and lets the call go on. The Harness fails a
    /// call whose request it does not take, so only such a Host shows
    /// what the SDK does with a refusal.
    unsafe extern "C" fn lax(call: *mut RvCall) -> RvStatus {
        // SAFETY: the fake Host made `call`, with an `RvToolCallArgs`
        // behind `args` for a step. The copy lent in its place lives
        // until the entry point has returned, and the Host's own is put
        // back then.
        unsafe {
            if (*call).verb != RV_TOOL_CALL {
                return entry::<Reader>(call);
            }
            let lent = (*call).args;
            let args = RvToolCallArgs {
                file_stat: refusing,
                ..lent.cast::<RvToolCallArgs>().read()
            };
            (*call).args = (&raw const args).cast();
            let status = entry::<Reader>(call);
            (*call).args = lent;
            status
        }
    }

    #[test]
    fn a_request_the_host_refuses_wakes_the_call_for_the_step_that_tells_the_tool() {
        // SAFETY: `lax` follows the contract apart from the refusal this
        // test checks, and never unwinds.
        let loaded = unsafe { testing::load_entry(lax) }.unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap();
        let mut call = pack.start("stat", "/notes/a.md");
        assert_eq!(call.step(), Ok(None));
        // No request is with the Host, so no answer brings the next step.
        assert!(
            call.is_woken(),
            "only a wake brings the step that delivers the refusal"
        );
        assert_eq!(
            call.step(),
            Ok(Some(Err(ToolError::new(
                ToolErrorKind::Backend,
                "Other: the Host answered the file function with status 4"
            ))))
        );
    }

    #[test]
    fn an_answer_that_the_calls_future_leaves_untaken_wakes_nothing() {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        let mut call = pack.start("holds", "/notes/a.md");
        assert_eq!(call.step(), Ok(None));
        // This step brings the answer, and finds the call's future
        // waiting on something else.
        assert_eq!(call.step(), Ok(None));
        assert!(
            !call.is_woken(),
            "the call stays parked until what it waits on wakes it"
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
        let state = CallState::new("tool", "{}", "{}");
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
