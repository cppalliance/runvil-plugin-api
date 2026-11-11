//! The Host's file functions, called for a [`ToolCall`](crate::ToolCall)
//! through the step that is running. Each copies what the Host hands
//! back before it returns, because the Host keeps it only until the next
//! file function.

use std::ffi::c_void;
use std::ptr;
use std::slice;
use std::sync::atomic::Ordering;

use runvil_plugin_sys::{
    RV_ERR_FILE, RV_OK, RvBytes, RvDirEntries, RvFileError, RvStat, RvStatus, RvStr, RvToolCallArgs,
};

use super::rv_str;
use crate::names::MAX_PATH;
use crate::tool::CallState;
use crate::{DirEntry, FileError, FileErrorKind, FileType, Stat};

/// What `path` names, for the call `state` belongs to.
pub(crate) fn stat(state: &CallState, path: &str) -> Result<Stat, FileError> {
    let step = running_step(state, path)?;
    let mut stat = RvStat {
        file_type: 0,
        reserved: 0,
        len: 0,
    };
    let mut error = no_error();
    // SAFETY: `step` is the running step's arguments, the path outlives
    // the call, and `stat` and `error` are live structs of this frame.
    let status = unsafe { (step.file_stat)(step.ctx, rv_str(path), &raw mut stat, &raw mut error) };
    // SAFETY: the Host filled `error` when it returned `RV_ERR_FILE`, and
    // no file function has been called since.
    unsafe { outcome(status, &error) }?;
    Ok(Stat::new(FileType::from_raw(stat.file_type), stat.len))
}

/// The entries of the directory at `path`, for the call `state` belongs
/// to.
pub(crate) fn list(state: &CallState, path: &str) -> Result<Vec<DirEntry>, FileError> {
    let step = running_step(state, path)?;
    let mut entries = RvDirEntries {
        ptr: ptr::null(),
        len: 0,
    };
    let mut error = no_error();
    // SAFETY: as in `stat`.
    let status =
        unsafe { (step.file_list)(step.ctx, rv_str(path), &raw mut entries, &raw mut error) };
    // SAFETY: as in `stat`.
    unsafe { outcome(status, &error) }?;
    // SAFETY: the Host returned `RV_OK`, so `entries` holds that many
    // entries, each with a name, all valid until the next file function.
    let entries = unsafe { lent(entries.ptr, entries.len) };
    Ok(entries
        .iter()
        .map(|entry| {
            // SAFETY: as above.
            let name = unsafe { text(entry.name) };
            let file_type = FileType::from_raw(entry.stat.file_type);
            DirEntry::new(name, Stat::new(file_type, entry.stat.len))
        })
        .collect())
}

/// Up to `len` bytes of the file at `path` from the byte `offset` on, for
/// the call `state` belongs to.
pub(crate) fn read(
    state: &CallState,
    path: &str,
    offset: u64,
    len: u64,
) -> Result<Vec<u8>, FileError> {
    let step = running_step(state, path)?;
    let mut bytes = RvBytes {
        ptr: ptr::null(),
        len: 0,
    };
    let mut error = no_error();
    // SAFETY: as in `stat`.
    let status = unsafe {
        (step.file_read)(
            step.ctx,
            rv_str(path),
            offset,
            len,
            &raw mut bytes,
            &raw mut error,
        )
    };
    // SAFETY: as in `stat`.
    unsafe { outcome(status, &error) }?;
    // SAFETY: the Host returned `RV_OK`, so `bytes` holds that many
    // bytes, valid until the next file function.
    Ok(unsafe { lent(bytes.ptr, bytes.len) }.to_vec())
}

/// What a file method needs of the step that is running: its `ctx` and
/// its three file functions. They are copied out of the step's arguments,
/// so nothing here borrows those arguments past the step.
#[derive(Clone, Copy)]
struct Step {
    ctx: *mut c_void,
    file_stat: unsafe extern "C" fn(*mut c_void, RvStr, *mut RvStat, *mut RvFileError) -> RvStatus,
    file_list:
        unsafe extern "C" fn(*mut c_void, RvStr, *mut RvDirEntries, *mut RvFileError) -> RvStatus,
    file_read: unsafe extern "C" fn(
        *mut c_void,
        RvStr,
        u64,
        u64,
        *mut RvBytes,
        *mut RvFileError,
    ) -> RvStatus,
}

/// The step that is running for the call `state` belongs to.
///
/// Fails without asking the Host when `path` is longer than the contract
/// allows, which the Host would treat as a misuse and fail the whole call
/// for, and when no step is running, as in a `Drop` that runs while the
/// call is canceled.
fn running_step(state: &CallState, path: &str) -> Result<Step, FileError> {
    if path.len() > MAX_PATH {
        return Err(FileError::new(
            FileErrorKind::Other,
            format!("the path is longer than {MAX_PATH} bytes"),
        ));
    }
    let step = state.step.load(Ordering::Acquire);
    if step.is_null() {
        return Err(FileError::new(
            FileErrorKind::Other,
            "no step of the tool call is running, so its files are out of reach",
        ));
    }
    // SAFETY: the pointer is set only while the step it points at runs.
    // A `ToolCall` borrows its call, so code that holds one runs inside a
    // step of that call, or on a thread a step waits for before it
    // returns, and the Host keeps the arguments unchanged for the step.
    let args: &RvToolCallArgs = unsafe { &*step };
    Ok(Step {
        ctx: args.ctx,
        file_stat: args.file_stat,
        file_list: args.file_list,
        file_read: args.file_read,
    })
}

/// An `RvFileError` for a file function to fill.
fn no_error() -> RvFileError {
    RvFileError {
        kind: 0,
        reserved: 0,
        text: RvStr {
            ptr: ptr::null(),
            len: 0,
        },
    }
}

/// What a file function that returned `status` did: nothing to report for
/// `RV_OK`, the failure in `error` for `RV_ERR_FILE`, and a failure that
/// names the status for any other.
///
/// # Safety
/// When `status` is `RV_ERR_FILE`, `error` holds what the Host's file
/// function wrote, and no file function has been called since.
unsafe fn outcome(status: RvStatus, error: &RvFileError) -> Result<(), FileError> {
    match status {
        RV_OK => Ok(()),
        RV_ERR_FILE => {
            // SAFETY: the caller vouches for the error's text.
            let text = unsafe { text(error.text) };
            Err(FileError::new(FileErrorKind::from_raw(error.kind), text))
        }
        other => Err(FileError::new(
            FileErrorKind::Other,
            format!("the Host answered the file function with status {other}"),
        )),
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
    use std::ptr;
    use std::sync::Mutex;
    use std::sync::atomic::Ordering;

    use runvil_plugin_sys::{
        RV_ERR_FILE, RV_ERR_MISUSE, RV_FILE_ERROR_DENIED, RV_OK, RvBytes, RvDirEntries,
        RvFileError, RvStat, RvStatus, RvStr, RvToolCallArgs, RvToolResult,
    };

    use super::{no_error, outcome, text};
    use crate::entry::{rv_str, size_field};
    use crate::testing::{self, Files};
    use crate::tool::CallState;
    use crate::{
        BoxFuture, DirEntry, FileError, FileErrorKind, FileType, Pack, PackInfo, Plugin, Refusal,
        Stat, ToolCall, ToolError, ToolErrorKind, ToolInfo, ToolOutput, next_step,
    };

    const SCHEMA: &str = r#"{"type":"object","properties":{}}"#;

    /// A Plugin whose tools report what the file methods return.
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

    /// What the last `guarded` call's guard got from `stat` when it
    /// dropped.
    static DROPPED: Mutex<Option<Result<Stat, FileError>>> = Mutex::new(None);

    /// Stats its call's path when it drops, and keeps the answer.
    struct StatOnDrop<'a>(ToolCall<'a>);

    impl Drop for StatOnDrop<'_> {
        fn drop(&mut self) {
            let answer = self.0.stat(self.0.arguments());
            if let Ok(mut dropped) = DROPPED.lock() {
                *dropped = Some(answer);
            }
        }
    }

    impl Pack for Tools {
        fn tools(&self) -> Vec<ToolInfo> {
            [
                "stat", "list", "read", "head", "twice", "threaded", "early", "guarded",
            ]
            .into_iter()
            .map(|name| ToolInfo::new(name, "A tool.", SCHEMA))
            .collect()
        }

        fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
            // `early` reads here, before its future exists.
            let early = (call.tool() == "early").then(|| call.read(call.arguments()));
            Box::pin(async move {
                let path = call.arguments();
                let text = match call.tool() {
                    "early" => early
                        .expect("the read ran before the future")
                        .map(|bytes| format!("{bytes:?}")),
                    "guarded" => {
                        let _guard = StatOnDrop(call);
                        next_step().await;
                        Ok("done".to_owned())
                    }
                    "stat" => call.stat(path).map(|stat| format!("{stat:?}")),
                    "list" => call.list(path).map(|entries| format!("{entries:?}")),
                    "read" => call.read(path).map(|bytes| format!("{bytes:?}")),
                    "head" => call
                        .read_range(path, 1, 3)
                        .map(|bytes| format!("{bytes:?}")),
                    "twice" => {
                        // The second step's file functions belong to a
                        // new `ctx`, which the call must pick up.
                        let first = call.read(path);
                        next_step().await;
                        let second = call.read(path);
                        Ok(format!("{}", first == second && first.is_ok()))
                    }
                    _ => std::thread::scope(|scope| {
                        scope
                            .spawn(|| call.stat(path).map(|stat| format!("{stat:?}")))
                            .join()
                            .expect("the scoped thread returns")
                    }),
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
    fn a_failed_file_function_reaches_the_tool_with_its_kind_and_text() {
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
    fn each_step_reads_through_its_own_arguments() {
        assert_eq!(run("twice", "/notes/a.md"), Ok("true".to_owned()));
    }

    #[test]
    fn a_file_method_works_before_the_calls_future_exists() {
        assert_eq!(run("early", "/notes/a.md"), Ok(format!("{:?}", b"hello")));
    }

    #[test]
    fn a_file_method_in_a_drop_that_runs_as_the_call_is_canceled_never_asks_the_host() {
        let loaded = testing::load::<Reader>().unwrap();
        let pack = loaded.new_pack("acme/reader").unwrap().with_files(files());
        let mut call = pack.start("guarded", "/notes/a.md");
        assert_eq!(call.step(), Ok(None));
        // The drop sends `RV_TOOL_CANCEL`, which drops the future and its
        // guard with no step running.
        drop(call);
        assert_eq!(
            DROPPED.lock().unwrap().take(),
            Some(Err(FileError::new(
                FileErrorKind::Other,
                "no step of the tool call is running, so its files are out of reach"
            )))
        );
    }

    #[test]
    fn a_file_method_on_another_thread_is_refused_by_the_host() {
        assert_eq!(
            run("threaded", "/notes/a.md"),
            Err("Other: the Host answered the file function with status 4".to_owned())
        );
    }

    /// A file function that fills nothing and answers `RV_ERR_MISUSE`.
    unsafe extern "C" fn refusing_stat(
        _ctx: *mut c_void,
        _path: RvStr,
        _stat: *mut RvStat,
        _error: *mut RvFileError,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_list(
        _ctx: *mut c_void,
        _path: RvStr,
        _entries: *mut RvDirEntries,
        _error: *mut RvFileError,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_read(
        _ctx: *mut c_void,
        _path: RvStr,
        _offset: u64,
        _len: u64,
        _bytes: *mut RvBytes,
        _error: *mut RvFileError,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn unused_finish(
        _ctx: *mut c_void,
        _result: *const RvToolResult,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    #[test]
    fn the_running_step_is_the_one_the_call_state_names() {
        let step = RvToolCallArgs {
            size: size_field::<RvToolCallArgs>(),
            reserved: 0,
            ctx: ptr::null_mut(),
            tool: rv_str("tool"),
            arguments: rv_str("{}"),
            finish: unused_finish,
            file_stat: refusing_stat,
            file_list: unused_list,
            file_read: unused_read,
        };
        let state = CallState::new("tool", "{}");
        state
            .step
            .store((&raw const step).cast_mut(), Ordering::Release);
        assert_eq!(
            ToolCall::new(&state).stat("/a"),
            Err(FileError::new(
                FileErrorKind::Other,
                "the Host answered the file function with status 4"
            ))
        );
    }

    #[test]
    fn an_outcome_reads_the_error_only_for_a_failed_file_function() {
        let mut error = no_error();
        error.kind = RV_FILE_ERROR_DENIED;
        error.text = rv_str("no");
        // SAFETY: the error's text is a live literal.
        unsafe {
            assert_eq!(outcome(RV_OK, &error), Ok(()));
            assert_eq!(
                outcome(RV_ERR_FILE, &error),
                Err(FileError::new(FileErrorKind::Denied, "no"))
            );
        }
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
