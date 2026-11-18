//! The file functions a fake Host's `RvToolCallArgs` carries: `file_stat`,
//! `file_list`, and `file_read`, each checking what the Plugin hands it
//! as the Harness does and taking one request, and [`Answer`], what the
//! fake Host lends the step after a request.

use std::ffi::c_void;
use std::ptr;

use runvil_plugin_sys::{
    RV_ERR_FILE, RV_MAX_PATH, RV_OK, RvBytes, RvDirEntries, RvDirEntry, RvFileAnswer, RvFileError,
    RvStat, RvStatus, RvStr,
};

use super::Files;
use super::session::{Field, TOOL_CALL, answer, field};
use crate::entry::{rv_str, size_field};
use crate::file::Request;
use crate::{DirEntry, FileError, Stat};

const PATH: Field = field!("the path", 4096, RV_MAX_PATH);

/// What a second file request in one step is called.
const SECOND_REQUEST: &str = "a step made a second file request";
/// What a file request in a step that called `finish` is called.
const REQUEST_AFTER_FINISH: &str = "a step made a file request after it called finish";
/// What `finish` in a step that made a file request is called.
pub(super) const FINISH_AFTER_REQUEST: &str = "a step called finish after it made a file request";

/// Takes the request `read` reads out of a file function's arguments, as
/// the one request of the open step; returns `RV_OK`, or `RV_ERR_MISUSE`
/// for a request the step may not make and for arguments `read` refuses.
/// The step's own state is checked first, so `read` runs only for a
/// request the step may make.
fn take(ctx: *mut c_void, read: impl FnOnce() -> Result<Request, &'static str>) -> RvStatus {
    answer(ctx, &TOOL_CALL, |session| {
        if session.result.is_some() {
            return Err(REQUEST_AFTER_FINISH);
        }
        if session.request.is_some() {
            return Err(SECOND_REQUEST);
        }
        session.request = Some(read()?);
        Ok(RV_OK)
    })
}

/// Takes a request for what `path` names; returns `RV_OK` or
/// `RV_ERR_MISUSE`, and fills nothing.
pub(super) unsafe extern "C" fn file_stat(ctx: *mut c_void, path: RvStr) -> RvStatus {
    // SAFETY: the contract makes `path` valid for its length during this
    // call.
    take(ctx, || Ok(Request::Stat(unsafe { PATH.read(path) }?)))
}

/// Takes a request for the entries of the directory at `path`; returns
/// `RV_OK` or `RV_ERR_MISUSE`, and fills nothing.
pub(super) unsafe extern "C" fn file_list(ctx: *mut c_void, path: RvStr) -> RvStatus {
    // SAFETY: the contract makes `path` valid for its length during this
    // call.
    take(ctx, || Ok(Request::List(unsafe { PATH.read(path) }?)))
}

/// Takes a request for up to `len` bytes of the file at `path` from the
/// byte `offset` on; returns `RV_OK` or `RV_ERR_MISUSE`, and fills
/// nothing.
pub(super) unsafe extern "C" fn file_read(
    ctx: *mut c_void,
    path: RvStr,
    offset: u64,
    len: u64,
) -> RvStatus {
    take(ctx, || {
        // SAFETY: the contract makes `path` valid for its length during
        // this call.
        let path = unsafe { PATH.read(path) }?;
        Ok(Request::Read { path, offset, len })
    })
}

/// The answer to one request, with the memory it points into. The fake
/// Host makes it when the step that asked has returned, and keeps it
/// until the step that receives it has returned.
#[derive(Debug)]
pub(super) struct Answer {
    /// What a path names, or zero when the request asked for no stat.
    stat: RvStat,
    /// The bytes of a read.
    bytes: Vec<u8>,
    /// The names the entries of a listing point at, kept only so that
    /// they stay alive.
    _names: Vec<String>,
    /// The entries of a listing.
    entries: Vec<RvDirEntry>,
    /// Why the operation failed, when it did.
    error: Option<FileError>,
}

impl Answer {
    /// Performs `request` on `files` and keeps what the operation
    /// answers: the one thing the request asked for, or its failure.
    pub(super) fn to(request: &Request, files: &Files) -> Answer {
        let found = match request {
            Request::Stat(path) => files.stat(path).map(Answer::of_stat),
            Request::List(path) => files.list(path).map(Answer::of_entries),
            Request::Read { path, offset, len } => {
                files.read(path, *offset, *len).map(Answer::of_bytes)
            }
        };
        found.unwrap_or_else(Answer::failed)
    }

    /// An answer that holds nothing.
    fn empty() -> Answer {
        Answer {
            stat: RvStat {
                file_type: 0,
                reserved: 0,
                len: 0,
            },
            bytes: Vec::new(),
            _names: Vec::new(),
            entries: Vec::new(),
            error: None,
        }
    }

    /// The answer that holds `stat`.
    fn of_stat(stat: Stat) -> Answer {
        Answer {
            stat: raw_stat(stat),
            ..Answer::empty()
        }
    }

    /// The answer that holds `entries`.
    fn of_entries(entries: Vec<DirEntry>) -> Answer {
        let stats: Vec<RvStat> = entries.iter().map(|entry| raw_stat(entry.stat)).collect();
        let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
        // Each name's bytes sit in its own allocation, so moving `names`
        // into the answer leaves the pointers below valid.
        let entries = names
            .iter()
            .zip(stats)
            .map(|(name, stat)| RvDirEntry {
                name: rv_str(name),
                stat,
            })
            .collect();
        Answer {
            _names: names,
            entries,
            ..Answer::empty()
        }
    }

    /// The answer that holds `bytes`.
    fn of_bytes(bytes: Vec<u8>) -> Answer {
        Answer {
            bytes,
            ..Answer::empty()
        }
    }

    /// The answer of an operation that failed with `error`.
    fn failed(error: FileError) -> Answer {
        Answer {
            error: Some(error),
            ..Answer::empty()
        }
    }

    /// The answer as the contract lends it to a step: the one field the
    /// request asked for, or `error`, and every other field zero. It
    /// points into this value, which the caller keeps until the step has
    /// returned.
    pub(super) fn raw(&self) -> RvFileAnswer {
        let (status, error) = match &self.error {
            Some(error) => (
                RV_ERR_FILE,
                RvFileError {
                    kind: error.kind().raw(),
                    reserved: 0,
                    text: rv_str(error.text()),
                },
            ),
            None => (
                RV_OK,
                RvFileError {
                    kind: 0,
                    reserved: 0,
                    text: RvStr {
                        ptr: ptr::null(),
                        len: 0,
                    },
                },
            ),
        };
        RvFileAnswer {
            size: size_field::<RvFileAnswer>(),
            status,
            stat: self.stat,
            entries: RvDirEntries {
                ptr: first(&self.entries),
                len: self.entries.len(),
            },
            bytes: RvBytes {
                ptr: first(&self.bytes),
                len: self.bytes.len(),
            },
            error,
        }
    }
}

/// Where `values` begin, or NULL when there are none, so that a field
/// with nothing in it is zero.
fn first<T>(values: &[T]) -> *const T {
    if values.is_empty() {
        ptr::null()
    } else {
        values.as_ptr()
    }
}

/// `stat` as the contract hands it back.
fn raw_stat(stat: Stat) -> RvStat {
    RvStat {
        file_type: stat.file_type.raw(),
        reserved: 0,
        len: stat.len,
    }
}

#[cfg(test)]
mod tests {
    //! The file functions' rules, by calling each one as a Plugin's step
    //! would and as it must not, and what an answer lends a step.

    use std::ffi::c_void;
    use std::{ptr, slice};

    use runvil_plugin_sys::{
        RV_ERR_FILE, RV_ERR_MISUSE, RV_FILE_ERROR_DENIED, RV_FILE_ERROR_NOT_FOUND,
        RV_FILE_TYPE_DIRECTORY, RV_FILE_TYPE_FILE, RV_OK, RV_RESULT_TRUSTED, RvFileAnswer,
        RvStatus, RvStr, RvToolResult,
    };

    use super::{Answer, file_list, file_read, file_stat};
    use crate::entry::{rv_str, size_field};
    use crate::file::Request;
    use crate::testing::Files;
    use crate::testing::call::finish;
    use crate::testing::session::{self, Verb};
    use crate::{FileError, FileErrorKind};

    fn files() -> Files {
        Files::new()
            .file("/notes/a.md", "hello")
            .file("/notes/b.md", "")
            .fails(
                "/secret",
                FileError::new(FileErrorKind::Denied, "the mode allows only /notes"),
            )
    }

    /// Runs `step` as one `RV_TOOL_CALL` step, and returns what it
    /// returned beside the request the fake Host took and the misuse it
    /// recorded.
    fn in_step<T>(
        step: impl FnOnce(*mut c_void) -> T,
    ) -> (T, Option<Request>, Option<&'static str>) {
        let mut out = None;
        let (_status, session) = session::open(Verb::ToolCall, |ctx| {
            out = Some(step(ctx));
            RV_OK
        });
        (out.expect("the step ran"), session.request, session.misuse)
    }

    /// The text `s` holds.
    ///
    /// # Safety
    /// `s` holds UTF-8 that is valid for reads of its length.
    unsafe fn text(s: RvStr) -> String {
        // SAFETY: the caller guarantees the pointer and the length.
        let bytes = unsafe { slice::from_raw_parts(s.ptr.cast::<u8>(), s.len) };
        String::from_utf8(bytes.to_vec()).expect("the Host hands back UTF-8")
    }

    /// Whether `answer` holds no stat, no entries, and no bytes.
    fn holds_nothing(answer: &RvFileAnswer) -> bool {
        (answer.stat.file_type, answer.stat.reserved, answer.stat.len) == (0, 0, 0)
            && answer.entries.ptr.is_null()
            && answer.entries.len == 0
            && answer.bytes.ptr.is_null()
            && answer.bytes.len == 0
    }

    /// Whether `answer` holds no error.
    fn holds_no_error(answer: &RvFileAnswer) -> bool {
        (answer.error.kind, answer.error.reserved) == (0, 0)
            && answer.error.text.ptr.is_null()
            && answer.error.text.len == 0
    }

    #[test]
    fn each_file_function_takes_its_request() {
        // SAFETY: the open step's `ctx` and a live path.
        let stat = in_step(|ctx| unsafe { file_stat(ctx, rv_str("/notes/a.md")) });
        assert_eq!(
            stat,
            (RV_OK, Some(Request::Stat("/notes/a.md".to_owned())), None)
        );
        // SAFETY: as above.
        let list = in_step(|ctx| unsafe { file_list(ctx, rv_str("notes")) });
        assert_eq!(list, (RV_OK, Some(Request::List("notes".to_owned())), None));
        // SAFETY: as above.
        let read = in_step(|ctx| unsafe { file_read(ctx, rv_str("/notes/a.md"), 1, 3) });
        let request = Request::Read {
            path: "/notes/a.md".to_owned(),
            offset: 1,
            len: 3,
        };
        assert_eq!(read, (RV_OK, Some(request), None));
    }

    #[test]
    fn a_second_request_in_one_step_is_a_recorded_misuse_and_the_first_stays() {
        // SAFETY: the open step's `ctx` and live paths.
        let (statuses, request, misuse) = in_step(|ctx| unsafe {
            (
                file_stat(ctx, rv_str("/a")),
                file_read(ctx, rv_str("/b"), 0, 1),
            )
        });
        assert_eq!(statuses, (RV_OK, RV_ERR_MISUSE));
        assert_eq!(request, Some(Request::Stat("/a".to_owned())));
        assert_eq!(misuse, Some("a step made a second file request"));
    }

    #[test]
    fn a_request_and_finish_in_one_step_are_a_recorded_misuse_in_either_order() {
        let result = RvToolResult {
            size: size_field::<RvToolResult>(),
            kind: RV_RESULT_TRUSTED,
            text: rv_str("done"),
        };
        // SAFETY: the open step's `ctx`, a live path, and a live result.
        let (statuses, request, misuse) = in_step(|ctx| unsafe {
            (finish(ctx, &raw const result), file_stat(ctx, rv_str("/a")))
        });
        assert_eq!(statuses, (RV_OK, RV_ERR_MISUSE));
        assert_eq!(request, None);
        assert_eq!(
            misuse,
            Some("a step made a file request after it called finish")
        );

        // SAFETY: as above.
        let (statuses, request, misuse) = in_step(|ctx| unsafe {
            (file_stat(ctx, rv_str("/a")), finish(ctx, &raw const result))
        });
        assert_eq!(statuses, (RV_OK, RV_ERR_MISUSE));
        assert_eq!(request, Some(Request::Stat("/a".to_owned())));
        assert_eq!(
            misuse,
            Some("a step called finish after it made a file request")
        );
    }

    /// Calls `file_stat` with `path`, and returns its status beside the
    /// request taken and the misuse recorded.
    fn stat_with(path: RvStr) -> (RvStatus, Option<Request>, Option<&'static str>) {
        // SAFETY: each test hands over only what the function refuses
        // before it reads through it, or a live path.
        in_step(|ctx| unsafe { file_stat(ctx, path) })
    }

    #[test]
    fn each_breach_of_a_path_is_a_recorded_misuse_and_takes_no_request() {
        let long = "a".repeat(4097);
        let not_utf8 = [0xff_u8, 0xfe];
        for (path, misuse) in [
            (rv_str(&long), "the path is longer than 4096 bytes"),
            (
                RvStr {
                    ptr: ptr::null(),
                    len: 2,
                },
                "the path has a NULL pointer and a nonzero length",
            ),
            (
                RvStr {
                    ptr: not_utf8.as_ptr().cast(),
                    len: not_utf8.len(),
                },
                "the path is not UTF-8",
            ),
        ] {
            assert_eq!(stat_with(path), (RV_ERR_MISUSE, None, Some(misuse)));
        }
    }

    #[test]
    fn a_path_of_exactly_4096_bytes_is_within_the_bound() {
        let longest = format!("/{}", "a".repeat(4095));
        assert_eq!(
            stat_with(rv_str(&longest)),
            (RV_OK, Some(Request::Stat(longest.clone())), None)
        );
    }

    #[test]
    fn a_file_function_answers_only_the_open_step_through_its_own_ctx() {
        let path = rv_str("/notes/a.md");

        // SAFETY: a `ctx` that is not the open step's is refused before
        // anything is read through it.
        let wrong_ctx = in_step(|_ctx| unsafe { file_stat(ptr::null_mut(), path) });
        assert_eq!(
            wrong_ctx,
            (
                RV_ERR_MISUSE,
                None,
                Some(
                    "a tool call function was handed a ctx other than the open RV_TOOL_CALL step's"
                )
            )
        );

        let (status, session) = session::open(Verb::PackNew, |ctx| {
            // SAFETY: a call that is not a step is refused before
            // anything is read.
            unsafe { file_stat(ctx, path) }
        });
        assert_eq!(
            (status, session.misuse),
            (
                RV_ERR_MISUSE,
                Some("a tool call function was called outside RV_TOOL_CALL")
            )
        );
        assert_eq!(session.request, None);

        // SAFETY: with no call open, the function reads nothing.
        let closed = session::closed(|| unsafe { file_stat(ptr::null_mut(), path) });
        assert_eq!(closed, RV_ERR_MISUSE);
    }

    #[test]
    fn the_answer_to_a_stat_holds_the_stat_and_nothing_else() {
        let answer = Answer::to(&Request::Stat("/notes/a.md".to_owned()), &files());
        let raw = answer.raw();
        assert_eq!(
            (raw.size, raw.status),
            (size_field::<RvFileAnswer>(), RV_OK)
        );
        assert_eq!(
            (raw.stat.file_type, raw.stat.reserved, raw.stat.len),
            (RV_FILE_TYPE_FILE, 0, 5)
        );
        assert!(raw.entries.ptr.is_null() && raw.entries.len == 0);
        assert!(raw.bytes.ptr.is_null() && raw.bytes.len == 0);
        assert!(holds_no_error(&raw));

        let directory = Answer::to(&Request::Stat("notes".to_owned()), &files());
        assert_eq!(directory.raw().stat.file_type, RV_FILE_TYPE_DIRECTORY);
    }

    #[test]
    fn the_answer_to_a_listing_holds_each_entry_with_its_name() {
        let answer = Answer::to(&Request::List("/notes".to_owned()), &files());
        let raw = answer.raw();
        assert_eq!(raw.status, RV_OK);
        // SAFETY: the entries and their names are valid while `answer`
        // lives.
        let listed: Vec<(String, u32, u64)> = unsafe {
            slice::from_raw_parts(raw.entries.ptr, raw.entries.len)
                .iter()
                .map(|entry| (text(entry.name), entry.stat.file_type, entry.stat.len))
                .collect()
        };
        assert_eq!(
            listed,
            [
                ("a.md".to_owned(), RV_FILE_TYPE_FILE, 5),
                ("b.md".to_owned(), RV_FILE_TYPE_FILE, 0),
            ]
        );
        assert_eq!(
            (raw.stat.file_type, raw.stat.reserved, raw.stat.len),
            (0, 0, 0)
        );
        assert!(raw.bytes.ptr.is_null() && raw.bytes.len == 0);
        assert!(holds_no_error(&raw));
    }

    #[test]
    fn the_answer_to_a_read_holds_the_bytes_inside_the_range() {
        let request = Request::Read {
            path: "/notes/a.md".to_owned(),
            offset: 1,
            len: 3,
        };
        let answer = Answer::to(&request, &files());
        let raw = answer.raw();
        assert_eq!(raw.status, RV_OK);
        // SAFETY: the bytes are valid while `answer` lives.
        let read = unsafe { slice::from_raw_parts(raw.bytes.ptr, raw.bytes.len) };
        assert_eq!(read, b"ell");
        assert!(raw.entries.ptr.is_null() && raw.entries.len == 0);
        assert!(holds_no_error(&raw));
    }

    #[test]
    fn the_answer_to_a_read_of_an_empty_file_holds_no_bytes() {
        let request = Request::Read {
            path: "/notes/b.md".to_owned(),
            offset: 0,
            len: u64::MAX,
        };
        let raw = Answer::to(&request, &files()).raw();
        assert_eq!(raw.status, RV_OK);
        assert!(holds_nothing(&raw) && holds_no_error(&raw));
    }

    #[test]
    fn the_answer_to_a_failed_operation_holds_the_error_and_nothing_else() {
        let answer = Answer::to(&Request::Stat("/absent".to_owned()), &files());
        let raw = answer.raw();
        assert_eq!(raw.status, RV_ERR_FILE);
        // SAFETY: the error's text is valid while `answer` lives.
        let message = unsafe { text(raw.error.text) };
        assert_eq!(
            (raw.error.kind, raw.error.reserved, message.as_str()),
            (RV_FILE_ERROR_NOT_FOUND, 0, "not found: /absent")
        );
        assert!(holds_nothing(&raw));
    }

    #[test]
    fn a_denied_path_fails_each_operation_with_the_policys_reason() {
        let path = "/secret".to_owned();
        for request in [
            Request::Stat(path.clone()),
            Request::List(path.clone()),
            Request::Read {
                path: path.clone(),
                offset: 0,
                len: 1,
            },
        ] {
            let answer = Answer::to(&request, &files());
            let raw = answer.raw();
            // SAFETY: the error's text is valid while `answer` lives.
            let message = unsafe { text(raw.error.text) };
            assert_eq!(
                (raw.status, raw.error.kind, message.as_str()),
                (
                    RV_ERR_FILE,
                    RV_FILE_ERROR_DENIED,
                    "the mode allows only /notes"
                ),
                "{request:?}"
            );
        }
    }
}
