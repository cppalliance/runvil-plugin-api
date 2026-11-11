//! The call a fake Host has open on a thread: the [`Session`] its `ctx`
//! points at, the slot that names it, and the checks every function a
//! Plugin calls back begins with.

use std::cell::Cell;
use std::ffi::c_void;
use std::mem::size_of;
use std::sync::Arc;
use std::{ptr, slice};

use runvil_plugin_sys::{RV_ERR_MISUSE, RV_ERR_REFUSED, RV_MAX_LINE, RV_OK, RvStatus, RvStr};

use super::file::Lent;
use super::{Failure, Files, InsertedPack};
use crate::{ToolError, ToolInfo, ToolOutput};

thread_local! {
    /// The session of the call open on this thread, or NULL while none
    /// is. A function reads through its `ctx` only when it is this one,
    /// so a call from another thread, or after its call returned, never
    /// does.
    static OPEN: Cell<*mut Session> = const { Cell::new(ptr::null_mut()) };
}

/// The verb of an open call, which decides the functions that answer it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Verb {
    /// `RV_PLUGIN_LOAD`.
    Load,
    /// `RV_PACK_NEW`.
    PackNew,
    /// `RV_TOOL_CALL`.
    ToolCall,
}

/// What one call into the Plugin has reported so far; the call's `ctx`
/// points at it.
#[derive(Debug)]
pub(super) struct Session {
    /// The call's verb.
    verb: Verb,
    /// The first way the Plugin broke the contract during the call.
    pub(super) misuse: Option<&'static str>,
    /// The reason the Plugin declined with.
    pub(super) refusal: Option<String>,
    /// The name and version a load described the Plugin with.
    pub(super) described: Option<Described>,
    /// The Packs a load inserted, in order.
    pub(super) packs: Vec<InsertedPack>,
    /// The tools an `RV_PACK_NEW` added, in order.
    pub(super) tools: Vec<ToolInfo>,
    /// The result a step finished its call with.
    pub(super) result: Option<Result<ToolOutput, ToolError>>,
    /// The tree a step's file functions read.
    pub(super) files: Arc<Files>,
    /// What a step's last file function handed the Plugin, which stays
    /// valid until the next one, or until the step returns.
    pub(super) lent: Lent,
}

/// The name and version a Plugin gave `describe_plugin`, copied.
#[derive(Debug)]
pub(super) struct Described {
    pub(super) name: String,
    pub(super) version: String,
}

impl Session {
    /// Why the Harness rejects the call, which returned `status`, when it
    /// does: a refusal, then a misuse, then a status other than `RV_OK`.
    pub(super) fn rejection(&mut self, status: RvStatus) -> Result<(), Failure> {
        if let Some(reason) = self.refusal.take() {
            return Err(Failure::Refused(reason));
        }
        if let Some(misuse) = self.misuse.take() {
            return Err(Failure::Misused(misuse));
        }
        if status != RV_OK {
            return Err(Failure::Status(status));
        }
        Ok(())
    }
}

/// Sends one call of `verb` with a fresh session open on this thread.
/// `send` gets the session's `ctx` and returns the Plugin's status. The
/// session is closed before this returns that status and what the Plugin
/// reported.
pub(super) fn open(
    verb: Verb,
    send: impl FnOnce(*mut c_void) -> RvStatus,
) -> (RvStatus, Box<Session>) {
    open_over(verb, Arc::default(), send)
}

/// Sends one step of a tool call, as [`open`] sends a call, with `files`
/// as the tree its file functions read.
pub(super) fn open_step(
    files: Arc<Files>,
    send: impl FnOnce(*mut c_void) -> RvStatus,
) -> (RvStatus, Box<Session>) {
    open_over(Verb::ToolCall, files, send)
}

/// Sends one call of `verb` over `files`; see [`open`].
fn open_over(
    verb: Verb,
    files: Arc<Files>,
    send: impl FnOnce(*mut c_void) -> RvStatus,
) -> (RvStatus, Box<Session>) {
    let session = Box::into_raw(Box::new(Session {
        verb,
        misuse: None,
        refusal: None,
        described: None,
        packs: Vec::new(),
        tools: Vec::new(),
        result: None,
        files,
        lent: Lent::default(),
    }));
    // A call inside a call, on one thread, keeps the outer call's session.
    let outer = OPEN.with(|open| open.replace(session));
    let status = send(session.cast());
    OPEN.with(|open| open.set(outer));
    // SAFETY: `session` came from `Box::into_raw` above, and with the call
    // closed no function reads through it any more.
    (status, unsafe { Box::from_raw(session) })
}

/// Sends a call that has no arguments, with no session open on this
/// thread, so a function the Plugin calls meanwhile answers
/// `RV_ERR_MISUSE`.
pub(super) fn closed(send: impl FnOnce() -> RvStatus) -> RvStatus {
    // The last holder of a Plugin may drop it inside a thread-local
    // destructor; `try_with` keeps such a call going out even on a thread
    // whose slot is gone.
    let outer = OPEN.try_with(|open| open.replace(ptr::null_mut()));
    let status = send();
    if let Ok(outer) = outer {
        let _restored = OPEN.try_with(|open| open.set(outer));
    }
    status
}

/// The functions of one verb's arguments struct, and what a breach of
/// the `ctx` rules by one of them is called.
pub(super) struct Family {
    verb: Verb,
    wrong_ctx: &'static str,
    wrong_call: &'static str,
}

/// The functions in an `RvLoadArgs`.
pub(super) const LOAD: Family = Family {
    verb: Verb::Load,
    wrong_ctx: "a load function was handed a ctx other than the open RV_PLUGIN_LOAD call's",
    wrong_call: "a load function was called outside RV_PLUGIN_LOAD",
};

/// The functions in an `RvPackNewArgs`.
pub(super) const PACK_NEW: Family = Family {
    verb: Verb::PackNew,
    wrong_ctx: "a Pack function was handed a ctx other than the open RV_PACK_NEW call's",
    wrong_call: "a Pack function was called outside RV_PACK_NEW",
};

/// The functions in an `RvToolCallArgs`.
pub(super) const TOOL_CALL: Family = Family {
    verb: Verb::ToolCall,
    wrong_ctx: "a tool call function was handed a ctx other than the open RV_TOOL_CALL step's",
    wrong_call: "a tool call function was called outside RV_TOOL_CALL",
};

/// Answers a function of `family`. `f` runs only when this thread has a
/// call of the family's verb open and `ctx` is its `ctx`; otherwise the
/// answer is `RV_ERR_MISUSE`. A fault `f` returns is answered the same
/// way. An open call records the first misuse.
pub(super) fn answer(
    ctx: *mut c_void,
    family: &Family,
    f: impl FnOnce(&mut Session) -> Result<RvStatus, &'static str>,
) -> RvStatus {
    let open = OPEN.try_with(Cell::get).unwrap_or(ptr::null_mut());
    if open.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: a non-NULL `open` is the session of the call running on this
    // thread, which `open_over` keeps alive for the call, and no other
    // reference to it exists while a function runs.
    let session = unsafe { &mut *open };
    let outcome = if !ptr::eq(open.cast::<c_void>(), ctx) {
        Err(family.wrong_ctx)
    } else if session.verb != family.verb {
        Err(family.wrong_call)
    } else {
        f(session)
    };
    outcome.unwrap_or_else(|fault| {
        session.misuse.get_or_insert(fault);
        RV_ERR_MISUSE
    })
}

/// Records the Plugin's refusal of the open call of `family`, validated;
/// returns `RV_ERR_REFUSED`, or `RV_ERR_MISUSE` for a call it doesn't
/// answer or a second refusal.
pub(super) fn refuse(ctx: *mut c_void, family: &Family, reason: RvStr) -> RvStatus {
    answer(ctx, family, |session| {
        if session.refusal.is_some() {
            return Err("refuse was called twice");
        }
        // SAFETY: the contract makes `reason` valid for its length during
        // this call.
        session.refusal = Some(unsafe { REASON.read(reason) }?);
        Ok(RV_ERR_REFUSED)
    })
}

/// One string's bound, and what each way of breaking it is called.
pub(super) struct Field {
    cap: usize,
    too_long: &'static str,
    null_with_length: &'static str,
    not_utf8: &'static str,
}

/// The [`Field`] of the string `$what` names, bounded at `$cap` bytes.
/// The number is a literal, because it is part of the text of a misuse. A
/// third argument is the contract's limit, which the number must equal.
macro_rules! field {
    ($what:literal, $cap:literal $(, $limit:expr)?) => {{
        $(const _: () = assert!($cap == $limit);)?
        $crate::testing::session::Field::new(
            $cap,
            concat!($what, " is longer than ", $cap, " bytes"),
            concat!($what, " has a NULL pointer and a nonzero length"),
            concat!($what, " is not UTF-8"),
        )
    }};
}
pub(super) use field;

/// A refusal's reason.
const REASON: Field = field!("the refusal reason", 1024, RV_MAX_LINE);

impl Field {
    /// A field bounded at `cap` bytes, with the name of each breach.
    pub(super) const fn new(
        cap: usize,
        too_long: &'static str,
        null_with_length: &'static str,
        not_utf8: &'static str,
    ) -> Field {
        Field {
            cap,
            too_long,
            null_with_length,
            not_utf8,
        }
    }

    /// Copies the string `s` holds. The length is checked against the cap
    /// before any byte is read, and `s.ptr` is read only when `s.len` is
    /// nonzero, because `slice::from_raw_parts` with a NULL pointer is
    /// undefined behavior even at length 0.
    ///
    /// # Safety
    /// When `s.len` is nonzero, `s.ptr` must be valid for reads of `s.len`
    /// bytes for the duration of this call.
    pub(super) unsafe fn read(&self, s: RvStr) -> Result<String, &'static str> {
        if s.len > self.cap {
            return Err(self.too_long);
        }
        if s.len == 0 {
            return Ok(String::new());
        }
        if s.ptr.is_null() {
            return Err(self.null_with_length);
        }
        // SAFETY: `s.ptr` is not NULL and `s.len` is at most the cap, and
        // the caller guarantees `s.ptr` is valid for `s.len` bytes.
        let bytes = unsafe { slice::from_raw_parts(s.ptr.cast::<u8>(), s.len) };
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| self.not_utf8)
    }
}

/// One struct a Plugin hands a function, and what each way of handing it
/// over wrongly is called.
pub(super) struct Shape {
    null: &'static str,
    misaligned: &'static str,
    short: &'static str,
}

/// The [`Shape`] of the struct `$struct`, which the function `$function`
/// takes and calls its `$what`.
macro_rules! shape {
    ($function:literal, $what:literal, $struct:literal) => {
        $crate::testing::session::Shape::new(
            concat!($function, " was handed a NULL ", $what),
            concat!($function, " was handed a misaligned ", $what),
            concat!("the ", $what, "'s size is shorter than ", $struct),
        )
    };
}
pub(super) use shape;

impl Shape {
    /// A shape with the name of each breach.
    pub(super) const fn new(
        null: &'static str,
        misaligned: &'static str,
        short: &'static str,
    ) -> Shape {
        Shape {
            null,
            misaligned,
            short,
        }
    }

    /// The struct `at` points at, once it is known to be whole.
    ///
    /// # Safety
    /// When `at` is not NULL and is aligned, it must be readable as a
    /// struct that begins with a `u32` saying how much of it the Plugin
    /// wrote, and readable as a whole `T` when that says so, for the
    /// lifetime `'a`.
    pub(super) unsafe fn read<'a, T>(&self, at: *const T) -> Result<&'a T, &'static str> {
        if at.is_null() {
            return Err(self.null);
        }
        if !at.is_aligned() {
            return Err(self.misaligned);
        }
        // SAFETY: `at` is not NULL and is aligned, and `size` is the first
        // field of a struct the Plugin wrote, so it is readable on its own.
        let size = unsafe { at.cast::<u32>().read() };
        if (size as usize) < size_of::<T>() {
            return Err(self.short);
        }
        // SAFETY: `size` says the Plugin wrote a whole `T`.
        Ok(unsafe { &*at })
    }
}

/// One struct a Plugin hands a function to fill, and what each way of
/// handing it over wrongly is called.
pub(super) struct Out {
    null: &'static str,
    misaligned: &'static str,
}

/// The [`Out`] of the struct the function `$function` fills and calls
/// its `$what`.
macro_rules! out {
    ($function:literal, $what:literal) => {
        $crate::testing::session::Out::new(
            concat!($function, " was handed a NULL ", $what, " pointer"),
            concat!($function, " was handed a misaligned ", $what, " pointer"),
        )
    };
}
pub(super) use out;

impl Out {
    /// An out struct with the name of each breach.
    pub(super) const fn new(null: &'static str, misaligned: &'static str) -> Out {
        Out { null, misaligned }
    }

    /// `at`, once it is known to be a place a `T` can be written to. It
    /// reads nothing.
    pub(super) fn check<T>(&self, at: *mut T) -> Result<*mut T, &'static str> {
        if at.is_null() {
            return Err(self.null);
        }
        if !at.is_aligned() {
            return Err(self.misaligned);
        }
        Ok(at)
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use runvil_plugin_sys::{RvPluginInfo, RvStat, RvStr};

    use super::{Field, Out, Shape};

    const WORD: Field = field!("the word", 4);
    const INFO: Shape = shape!("describe_plugin", "info", "RvPluginInfo");
    const STAT: Out = out!("file_stat", "stat");

    fn rv_str(bytes: &[u8]) -> RvStr {
        RvStr {
            ptr: bytes.as_ptr().cast(),
            len: bytes.len(),
        }
    }

    #[test]
    fn a_field_copies_a_string_within_its_bound() {
        // SAFETY: each string points at a live literal of its length.
        unsafe {
            assert_eq!(WORD.read(rv_str(b"word")), Ok("word".to_owned()));
            assert_eq!(WORD.read(rv_str(b"")), Ok(String::new()));
        }
        let null_and_empty = RvStr {
            ptr: ptr::null(),
            len: 0,
        };
        // SAFETY: a zero length reads no byte.
        assert_eq!(unsafe { WORD.read(null_and_empty) }, Ok(String::new()));
    }

    #[test]
    fn a_field_names_each_way_a_string_breaks_its_rules() {
        // SAFETY: the length is refused before any byte is read.
        let too_long = unsafe { WORD.read(rv_str(b"words")) };
        assert_eq!(too_long, Err("the word is longer than 4 bytes"));
        let null = RvStr {
            ptr: ptr::null(),
            len: 2,
        };
        // SAFETY: a NULL pointer is refused before any byte is read.
        let null = unsafe { WORD.read(null) };
        assert_eq!(
            null,
            Err("the word has a NULL pointer and a nonzero length")
        );
        // SAFETY: the string points at a live literal of its length.
        let not_utf8 = unsafe { WORD.read(rv_str(&[0xff, 0xfe])) };
        assert_eq!(not_utf8, Err("the word is not UTF-8"));
    }

    #[test]
    fn a_shape_names_each_way_a_struct_is_handed_over_wrongly() {
        let whole = RvPluginInfo {
            size: 40,
            reserved: 0,
            name: rv_str(b"a"),
            version: rv_str(b"1"),
        };
        // SAFETY: `whole` is a live `RvPluginInfo` of its stated size.
        assert!(unsafe { INFO.read(&raw const whole) }.is_ok());
        // SAFETY: NULL is refused before anything is read.
        let null = unsafe { INFO.read(ptr::null::<RvPluginInfo>()) };
        assert_eq!(null.unwrap_err(), "describe_plugin was handed a NULL info");
        let odd = (&raw const whole).wrapping_byte_add(1);
        // SAFETY: a misaligned pointer is refused before anything is read.
        let misaligned = unsafe { INFO.read(odd) };
        assert_eq!(
            misaligned.unwrap_err(),
            "describe_plugin was handed a misaligned info"
        );
        let short = RvPluginInfo { size: 8, ..whole };
        // SAFETY: `short` is whole; only its `size` field claims less.
        let short = unsafe { INFO.read(&raw const short) };
        assert_eq!(
            short.unwrap_err(),
            "the info's size is shorter than RvPluginInfo"
        );
    }

    #[test]
    fn an_out_names_each_way_a_struct_to_fill_is_handed_over_wrongly() {
        let mut stat = RvStat {
            file_type: 0,
            reserved: 0,
            len: 0,
        };
        let at = &raw mut stat;
        assert_eq!(STAT.check(at), Ok(at));
        assert_eq!(
            STAT.check(ptr::null_mut::<RvStat>()),
            Err("file_stat was handed a NULL stat pointer")
        );
        assert_eq!(
            STAT.check(at.wrapping_byte_add(1)),
            Err("file_stat was handed a misaligned stat pointer")
        );
    }
}
