//! The file functions a fake Host's `RvToolCallArgs` carries:
//! `file_stat`, `file_list`, and `file_read`, each checking what the
//! Plugin hands it as the Harness does, and [`Lent`], what the last of
//! them handed back.

use std::ffi::c_void;

use runvil_plugin_sys::{
    RV_ERR_FILE, RV_OK, RvBytes, RvDirEntries, RvDirEntry, RvFileError, RvStat, RvStatus, RvStr,
};

use super::session::{Field, Out, TOOL_CALL, answer, field, out};
use crate::entry::rv_str;
use crate::names::MAX_PATH;
use crate::{DirEntry, FileError, Stat};

const PATH: Field = field!("the path", 4096);
const STAT: Out = out!("file_stat", "stat");
const STAT_ERROR: Out = out!("file_stat", "error");
const ENTRIES: Out = out!("file_list", "entries");
const LIST_ERROR: Out = out!("file_list", "error");
const BYTES: Out = out!("file_read", "bytes");
const READ_ERROR: Out = out!("file_read", "error");

// The literal above is the contract's bound on a path.
const _: () = assert!(MAX_PATH == 4096);

/// What a step's last file function handed the Plugin. The contract
/// keeps it valid until the step's next file function, so each one
/// replaces it, and the step's end frees it.
#[derive(Debug, Default)]
pub(super) struct Lent {
    /// The bytes of a read.
    bytes: Vec<u8>,
    /// The names the entries of a listing point at, kept only so that
    /// they stay alive.
    _names: Vec<String>,
    /// The entries of a listing.
    entries: Vec<RvDirEntry>,
    /// The text of a failure.
    text: String,
}

impl Lent {
    /// Keeps `bytes` and returns them as the contract hands them back.
    fn bytes(&mut self, bytes: Vec<u8>) -> RvBytes {
        *self = Lent {
            bytes,
            ..Lent::default()
        };
        RvBytes {
            ptr: self.bytes.as_ptr(),
            len: self.bytes.len(),
        }
    }

    /// Keeps `entries` and returns them as the contract hands them back.
    fn entries(&mut self, entries: Vec<DirEntry>) -> RvDirEntries {
        let stats: Vec<RvStat> = entries.iter().map(|entry| raw_stat(entry.stat)).collect();
        let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
        // Each name's bytes sit in its own allocation, so moving `names`
        // into `self` leaves the pointers below valid.
        let entries = names
            .iter()
            .zip(stats)
            .map(|(name, stat)| RvDirEntry {
                name: rv_str(name),
                stat,
            })
            .collect();
        *self = Lent {
            _names: names,
            entries,
            ..Lent::default()
        };
        RvDirEntries {
            ptr: self.entries.as_ptr(),
            len: self.entries.len(),
        }
    }

    /// Keeps the text of `error` and returns the failure as the contract
    /// hands it back.
    fn error(&mut self, error: &FileError) -> RvFileError {
        *self = Lent {
            text: error.text().to_owned(),
            ..Lent::default()
        };
        RvFileError {
            kind: error.kind().raw(),
            reserved: 0,
            text: rv_str(&self.text),
        }
    }
}

/// `stat` as the contract hands it back.
fn raw_stat(stat: Stat) -> RvStat {
    RvStat {
        file_type: stat.file_type.raw(),
        reserved: 0,
        len: stat.size,
    }
}

/// Says what `path` names; returns `RV_OK` with `stat` filled,
/// `RV_ERR_FILE` with `error` filled, or `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn file_stat(
    ctx: *mut c_void,
    path: RvStr,
    stat: *mut RvStat,
    error: *mut RvFileError,
) -> RvStatus {
    answer(ctx, &TOOL_CALL, |session| {
        let stat = STAT.check(stat)?;
        let error = STAT_ERROR.check(error)?;
        // SAFETY: the contract makes `path` valid for its length during
        // this call.
        let path = unsafe { PATH.read(path) }?;
        Ok(match session.files.stat(&path) {
            Ok(found) => {
                session.lent = Lent::default();
                // SAFETY: `stat` is not NULL and is aligned, and the
                // contract makes it a struct the Plugin owns.
                unsafe { stat.write(raw_stat(found)) };
                RV_OK
            }
            // SAFETY: as above, for `error`.
            Err(failure) => unsafe { fail(&mut session.lent, error, &failure) },
        })
    })
}

/// Lists the directory at `path`; returns `RV_OK` with `entries` filled,
/// `RV_ERR_FILE` with `error` filled, or `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn file_list(
    ctx: *mut c_void,
    path: RvStr,
    entries: *mut RvDirEntries,
    error: *mut RvFileError,
) -> RvStatus {
    answer(ctx, &TOOL_CALL, |session| {
        let entries = ENTRIES.check(entries)?;
        let error = LIST_ERROR.check(error)?;
        // SAFETY: the contract makes `path` valid for its length during
        // this call.
        let path = unsafe { PATH.read(path) }?;
        Ok(match session.files.list(&path) {
            Ok(listed) => {
                let lent = session.lent.entries(listed);
                // SAFETY: `entries` is not NULL and is aligned, and the
                // contract makes it a struct the Plugin owns.
                unsafe { entries.write(lent) };
                RV_OK
            }
            // SAFETY: as above, for `error`.
            Err(failure) => unsafe { fail(&mut session.lent, error, &failure) },
        })
    })
}

/// Reads up to `len` bytes of the file at `path` from the byte `offset`
/// on; returns `RV_OK` with `bytes` filled, `RV_ERR_FILE` with `error`
/// filled, or `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn file_read(
    ctx: *mut c_void,
    path: RvStr,
    offset: u64,
    len: u64,
    bytes: *mut RvBytes,
    error: *mut RvFileError,
) -> RvStatus {
    answer(ctx, &TOOL_CALL, |session| {
        let bytes = BYTES.check(bytes)?;
        let error = READ_ERROR.check(error)?;
        // SAFETY: the contract makes `path` valid for its length during
        // this call.
        let path = unsafe { PATH.read(path) }?;
        Ok(match session.files.read(&path, offset, len) {
            Ok(read) => {
                let lent = session.lent.bytes(read);
                // SAFETY: `bytes` is not NULL and is aligned, and the
                // contract makes it a struct the Plugin owns.
                unsafe { bytes.write(lent) };
                RV_OK
            }
            // SAFETY: as above, for `error`.
            Err(failure) => unsafe { fail(&mut session.lent, error, &failure) },
        })
    })
}

/// Hands `failure` back through `error`, with its text kept in `lent`,
/// and returns `RV_ERR_FILE`.
///
/// # Safety
/// `error` must be valid for a write of an `RvFileError`.
unsafe fn fail(lent: &mut Lent, error: *mut RvFileError, failure: &FileError) -> RvStatus {
    let raw = lent.error(failure);
    // SAFETY: the caller makes `error` valid for the write.
    unsafe { error.write(raw) };
    RV_ERR_FILE
}

#[cfg(test)]
mod tests {
    //! The file functions' rules, by calling each one as a Plugin's step
    //! would and as it must not.

    use std::ffi::c_void;
    use std::sync::Arc;
    use std::{ptr, slice};

    use runvil_plugin_sys::{
        RV_ERR_FILE, RV_ERR_MISUSE, RV_FILE_ERROR_DENIED, RV_FILE_ERROR_NOT_FOUND,
        RV_FILE_TYPE_DIRECTORY, RV_FILE_TYPE_FILE, RV_OK, RvBytes, RvDirEntries, RvFileError,
        RvStat, RvStatus, RvStr,
    };

    use super::{file_list, file_read, file_stat};
    use crate::entry::rv_str;
    use crate::testing::Files;
    use crate::testing::session::{self, Verb};
    use crate::{FileError, FileErrorKind};

    fn files() -> Arc<Files> {
        Arc::new(
            Files::new()
                .file("/notes/a.md", "hello")
                .file("/notes/b.md", "")
                .fails(
                    "/secret",
                    FileError::new(FileErrorKind::Denied, "the mode allows only /notes"),
                ),
        )
    }

    /// Runs `step` as one `RV_TOOL_CALL` step over the test's tree, and
    /// returns what it returned beside the misuse the fake Host recorded.
    fn in_step<T>(step: impl FnOnce(*mut c_void) -> T) -> (T, Option<&'static str>) {
        let mut out = None;
        let (_status, session) = session::open_step(files(), |ctx| {
            out = Some(step(ctx));
            RV_OK
        });
        (out.expect("the step ran"), session.misuse)
    }

    fn empty_stat() -> RvStat {
        RvStat {
            file_type: 9,
            reserved: 9,
            len: 9,
        }
    }

    fn empty_error() -> RvFileError {
        RvFileError {
            kind: 9,
            reserved: 9,
            text: rv_str("untouched"),
        }
    }

    fn empty_bytes() -> RvBytes {
        RvBytes {
            ptr: ptr::null(),
            len: 9,
        }
    }

    fn empty_entries() -> RvDirEntries {
        RvDirEntries {
            ptr: ptr::null(),
            len: 9,
        }
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

    #[test]
    fn file_stat_fills_the_stat_and_leaves_the_error_alone() {
        let ((status, stat, error), misuse) = in_step(|ctx| {
            let (mut stat, mut error) = (empty_stat(), empty_error());
            // SAFETY: the open step's `ctx`, a live path, and two live
            // structs.
            let status =
                unsafe { file_stat(ctx, rv_str("/notes/a.md"), &raw mut stat, &raw mut error) };
            (status, stat, error)
        });
        assert_eq!((status, misuse), (RV_OK, None));
        assert_eq!(
            (stat.file_type, stat.reserved, stat.len),
            (RV_FILE_TYPE_FILE, 0, 5)
        );
        assert_eq!(error.kind, 9, "a success writes no error");
    }

    #[test]
    fn file_stat_reports_a_directory() {
        let ((status, stat), misuse) = in_step(|ctx| {
            let (mut stat, mut error) = (empty_stat(), empty_error());
            // SAFETY: as in the first test.
            let status = unsafe { file_stat(ctx, rv_str("notes"), &raw mut stat, &raw mut error) };
            (status, stat)
        });
        assert_eq!((status, misuse), (RV_OK, None));
        assert_eq!(stat.file_type, RV_FILE_TYPE_DIRECTORY);
    }

    #[test]
    fn a_failed_file_function_fills_the_error_and_leaves_the_result_alone() {
        let ((status, stat, kind, reserved, message), misuse) = in_step(|ctx| {
            let (mut stat, mut error) = (empty_stat(), empty_error());
            // SAFETY: as in the first test.
            let status =
                unsafe { file_stat(ctx, rv_str("/absent"), &raw mut stat, &raw mut error) };
            // SAFETY: the error's text is valid until the next file
            // function of this step.
            let message = unsafe { text(error.text) };
            (status, stat, error.kind, error.reserved, message)
        });
        assert_eq!((status, misuse), (RV_ERR_FILE, None));
        assert_eq!(
            (kind, reserved, message.as_str()),
            (RV_FILE_ERROR_NOT_FOUND, 0, "not found: /absent")
        );
        assert_eq!(stat.file_type, 9, "a failure writes no result");
    }

    #[test]
    fn file_list_hands_back_each_entry_with_its_name() {
        let ((status, listed), misuse) = in_step(|ctx| {
            let (mut entries, mut error) = (empty_entries(), empty_error());
            // SAFETY: as in the first test.
            let status =
                unsafe { file_list(ctx, rv_str("/notes"), &raw mut entries, &raw mut error) };
            // SAFETY: the entries and their names are valid until the
            // next file function of this step.
            let listed: Vec<(String, u32, u64)> = unsafe {
                slice::from_raw_parts(entries.ptr, entries.len)
                    .iter()
                    .map(|entry| (text(entry.name), entry.stat.file_type, entry.stat.len))
                    .collect()
            };
            (status, listed)
        });
        assert_eq!((status, misuse), (RV_OK, None));
        assert_eq!(
            listed,
            [
                ("a.md".to_owned(), RV_FILE_TYPE_FILE, 5),
                ("b.md".to_owned(), RV_FILE_TYPE_FILE, 0),
            ]
        );
    }

    #[test]
    fn file_read_hands_back_the_bytes_inside_the_range() {
        let ((status, read), misuse) = in_step(|ctx| {
            let (mut bytes, mut error) = (empty_bytes(), empty_error());
            // SAFETY: as in the first test.
            let status = unsafe {
                file_read(
                    ctx,
                    rv_str("/notes/a.md"),
                    1,
                    3,
                    &raw mut bytes,
                    &raw mut error,
                )
            };
            // SAFETY: the bytes are valid until the next file function of
            // this step.
            let read = unsafe { slice::from_raw_parts(bytes.ptr, bytes.len) }.to_vec();
            (status, read)
        });
        assert_eq!((status, misuse), (RV_OK, None));
        assert_eq!(read, b"ell");
    }

    #[test]
    fn a_read_of_an_empty_file_hands_back_no_bytes() {
        let ((status, len), misuse) = in_step(|ctx| {
            let (mut bytes, mut error) = (empty_bytes(), empty_error());
            // SAFETY: as in the first test.
            let status = unsafe {
                file_read(
                    ctx,
                    rv_str("/notes/b.md"),
                    0,
                    u64::MAX,
                    &raw mut bytes,
                    &raw mut error,
                )
            };
            (status, bytes.len)
        });
        assert_eq!((status, len, misuse), (RV_OK, 0, None));
    }

    #[test]
    fn a_denied_path_fails_each_function_with_the_policys_reason() {
        let (failures, misuse) = in_step(|ctx| {
            let path = rv_str("/secret");
            let mut failures = Vec::new();
            let mut error = empty_error();
            let (mut stat, mut entries, mut bytes) = (empty_stat(), empty_entries(), empty_bytes());
            // SAFETY: as in the first test, and each error's text is read
            // before the next file function.
            unsafe {
                let status = file_stat(ctx, path, &raw mut stat, &raw mut error);
                failures.push((status, error.kind, text(error.text)));
                let status = file_list(ctx, path, &raw mut entries, &raw mut error);
                failures.push((status, error.kind, text(error.text)));
                let status = file_read(ctx, path, 0, 1, &raw mut bytes, &raw mut error);
                failures.push((status, error.kind, text(error.text)));
            }
            failures
        });
        assert_eq!(misuse, None);
        let denied = (
            RV_ERR_FILE,
            RV_FILE_ERROR_DENIED,
            "the mode allows only /notes".to_owned(),
        );
        assert_eq!(failures, [denied.clone(), denied.clone(), denied]);
    }

    /// Calls `file_stat` with `path`, `stat`, and `error`, and returns
    /// its status beside the misuse recorded.
    fn stat_with(
        path: RvStr,
        stat: *mut RvStat,
        error: *mut RvFileError,
    ) -> (RvStatus, Option<&'static str>) {
        // SAFETY: each test hands over only what the function refuses
        // before it reads or writes through it.
        in_step(|ctx| unsafe { file_stat(ctx, path, stat, error) })
    }

    #[test]
    fn each_breach_of_a_path_is_a_recorded_misuse() {
        let (mut stat, mut error) = (empty_stat(), empty_error());
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
            assert_eq!(
                stat_with(path, &raw mut stat, &raw mut error),
                (RV_ERR_MISUSE, Some(misuse))
            );
        }
        assert_eq!(stat.file_type, 9, "a misuse writes no result");
        assert_eq!(error.kind, 9, "a misuse writes no error");
    }

    #[test]
    fn a_path_of_exactly_4096_bytes_is_within_the_bound() {
        let (mut stat, mut error) = (empty_stat(), empty_error());
        let longest = format!("/{}", "a".repeat(4095));
        assert_eq!(
            stat_with(rv_str(&longest), &raw mut stat, &raw mut error),
            (RV_ERR_FILE, None)
        );
    }

    #[test]
    fn each_struct_to_fill_must_be_there_and_aligned() {
        let (mut stat, mut error) = (empty_stat(), empty_error());
        let (mut entries, mut bytes) = (empty_entries(), empty_bytes());
        let path = rv_str("/notes/a.md");
        let (stat_at, error_at) = (&raw mut stat, &raw mut error);
        let (entries_at, bytes_at) = (&raw mut entries, &raw mut bytes);
        let odd_error = error_at.wrapping_byte_add(1);

        assert_eq!(
            stat_with(path, ptr::null_mut(), error_at),
            (
                RV_ERR_MISUSE,
                Some("file_stat was handed a NULL stat pointer")
            )
        );
        assert_eq!(
            stat_with(path, stat_at.wrapping_byte_add(1), error_at),
            (
                RV_ERR_MISUSE,
                Some("file_stat was handed a misaligned stat pointer")
            )
        );
        assert_eq!(
            stat_with(path, stat_at, ptr::null_mut()),
            (
                RV_ERR_MISUSE,
                Some("file_stat was handed a NULL error pointer")
            )
        );
        assert_eq!(
            stat_with(path, stat_at, odd_error),
            (
                RV_ERR_MISUSE,
                Some("file_stat was handed a misaligned error pointer")
            )
        );

        // SAFETY: each call hands over only what the function refuses
        // before it reads or writes through it.
        let list = |entries, error| in_step(|ctx| unsafe { file_list(ctx, path, entries, error) });
        assert_eq!(
            list(ptr::null_mut(), error_at),
            (
                RV_ERR_MISUSE,
                Some("file_list was handed a NULL entries pointer")
            )
        );
        assert_eq!(
            list(entries_at.wrapping_byte_add(1), error_at),
            (
                RV_ERR_MISUSE,
                Some("file_list was handed a misaligned entries pointer")
            )
        );
        assert_eq!(
            list(entries_at, ptr::null_mut()),
            (
                RV_ERR_MISUSE,
                Some("file_list was handed a NULL error pointer")
            )
        );
        assert_eq!(
            list(entries_at, odd_error),
            (
                RV_ERR_MISUSE,
                Some("file_list was handed a misaligned error pointer")
            )
        );

        // SAFETY: as above.
        let read =
            |bytes, error| in_step(|ctx| unsafe { file_read(ctx, path, 0, 1, bytes, error) });
        assert_eq!(
            read(ptr::null_mut(), error_at),
            (
                RV_ERR_MISUSE,
                Some("file_read was handed a NULL bytes pointer")
            )
        );
        assert_eq!(
            read(bytes_at.wrapping_byte_add(1), error_at),
            (
                RV_ERR_MISUSE,
                Some("file_read was handed a misaligned bytes pointer")
            )
        );
        assert_eq!(
            read(bytes_at, ptr::null_mut()),
            (
                RV_ERR_MISUSE,
                Some("file_read was handed a NULL error pointer")
            )
        );
        assert_eq!(
            read(bytes_at, odd_error),
            (
                RV_ERR_MISUSE,
                Some("file_read was handed a misaligned error pointer")
            )
        );
    }

    #[test]
    fn a_file_function_answers_only_the_open_step_through_its_own_ctx() {
        let (mut stat, mut error) = (empty_stat(), empty_error());
        let path = rv_str("/notes/a.md");
        let (stat_at, error_at) = (&raw mut stat, &raw mut error);

        // SAFETY: a `ctx` that is not the open step's is refused before
        // anything is read through it.
        let wrong_ctx =
            in_step(|_ctx| unsafe { file_stat(ptr::null_mut(), path, stat_at, error_at) });
        assert_eq!(
            wrong_ctx,
            (
                RV_ERR_MISUSE,
                Some(
                    "a tool call function was handed a ctx other than the open RV_TOOL_CALL step's"
                )
            )
        );

        let (status, session) = session::open(Verb::PackNew, |ctx| {
            // SAFETY: a call that is not a step is refused before
            // anything is read or written.
            unsafe { file_stat(ctx, path, stat_at, error_at) }
        });
        assert_eq!(
            (status, session.misuse),
            (
                RV_ERR_MISUSE,
                Some("a tool call function was called outside RV_TOOL_CALL")
            )
        );

        // SAFETY: with no call open, the function reads nothing.
        let closed =
            session::closed(|| unsafe { file_stat(ptr::null_mut(), path, stat_at, error_at) });
        assert_eq!(closed, RV_ERR_MISUSE);
        assert_eq!(stat.file_type, 9, "no misuse wrote a result");
    }
}
