//! The C entry point every [`Plugin`] gets: [`entry`], which answers each
//! verb of the contract for the Plugin type it is instantiated with. The
//! code of each verb sits in the child module of the object it acts on:
//! `plugin`, `pack`, or `tool`.

mod pack;
mod plugin;
mod tool;

use std::any::Any;
use std::mem::size_of;
use std::panic::{self, AssertUnwindSafe};
use std::slice;

use runvil_plugin_sys::{
    RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_PACK_DELETE, RV_PACK_NEW, RV_PLUGIN_LOAD,
    RV_PLUGIN_UNLOAD, RV_TOOL_CALL, RV_TOOL_CANCEL, RvCall, RvStatus, RvStr,
};

use crate::names::{
    is_valid_description, is_valid_name, is_valid_pack_name, is_valid_version, same,
};
use crate::{Plugin, Refusal};

/// The C entry point of the Plugin `P`, with the shape of
/// [`PluginEntry`](runvil_plugin_sys::PluginEntry).
///
/// A library Plugin exports it with
/// [`export_plugin!`](crate::export_plugin). A Host that links `P` in
/// hands `entry::<P>` to the Harness, which needs no exported symbol, so
/// any number of linked Plugins share one binary.
///
/// It answers each verb of the contract:
///
/// - `RV_PLUGIN_LOAD` by checking the Host's version, making the instance
///   with [`Plugin::load`], describing the Plugin with [`Plugin::NAME`]
///   and [`Plugin::VERSION`], and inserting each of [`Plugin::PACKS`];
/// - `RV_PLUGIN_UNLOAD` by dropping the instance;
/// - `RV_PACK_NEW` by making one run's instance with
///   [`Plugin::new_pack`] and adding each of its
///   [`tools`](crate::Pack::tools);
/// - `RV_PACK_DELETE` by dropping that instance;
/// - `RV_TOOL_CALL` by polling the future of
///   [`Pack::call`](crate::Pack::call) once, and ending the call with its
///   result when the future is ready;
/// - `RV_TOOL_CANCEL` by dropping that future;
/// - any other verb with `RV_ERR_UNSUPPORTED`.
///
/// A Host whose version this crate does not speak, or whose structs are
/// shorter than this crate's, gets `RV_ERR_UNSUPPORTED` before anything is
/// made. No panic in the Plugin's code unwinds into the Host.
///
/// Building `entry::<P>` fails when `P::NAME`, `P::VERSION`, or one of
/// `P::PACKS` breaks the rules the Harness checks.
///
/// # Safety
/// `call` must be NULL or point to an `RvCall` made as the contract in
/// [`runvil_plugin_sys`] describes, whose `host`, `args`, and Plugin
/// pointers follow it too, for the duration of the call.
pub unsafe extern "C" fn entry<P: Plugin>(call: *mut RvCall) -> RvStatus {
    const { check_constants::<P>() };
    if call.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: `call` is not NULL and the caller makes it point to an
    // `RvCall`; `size` is its first field, so it is readable before the
    // struct's length is known.
    let size = unsafe { (&raw const (*call).size).read() };
    if (size as usize) < size_of::<RvCall>() {
        return RV_ERR_UNSUPPORTED;
    }
    // SAFETY: `size` shows the whole `RvCall` is there.
    let verb = unsafe { (*call).verb };
    // SAFETY: in each arm the whole `RvCall` is there, and the caller
    // vouches for the pointers the verb reads.
    unsafe {
        match verb {
            RV_PLUGIN_LOAD => plugin::load::<P>(call),
            RV_PLUGIN_UNLOAD => plugin::unload::<P>(call),
            RV_PACK_NEW => pack::new::<P>(call),
            RV_PACK_DELETE => pack::delete(call),
            RV_TOOL_CALL => tool::step(call),
            RV_TOOL_CANCEL => tool::cancel(call),
            _ => RV_ERR_UNSUPPORTED,
        }
    }
}

/// Fails the build of [`entry::<P>`](entry) when one of `P`'s constants
/// breaks a rule the Harness checks as the Plugin loads.
const fn check_constants<P: Plugin>() {
    assert!(
        is_valid_name(P::NAME),
        "Plugin::NAME must be 1 to 64 bytes of lowercase ASCII letters, digits, '-', '_', and '.'"
    );
    assert!(
        is_valid_version(P::VERSION),
        "Plugin::VERSION must be 1 to 64 bytes with no control characters"
    );
    let mut at = 0;
    while at < P::PACKS.len() {
        let pack = &P::PACKS[at];
        assert!(
            is_valid_pack_name(pack.name),
            "each name in Plugin::PACKS must be a vendor and a Pack id joined by '/', each 1 to 64 bytes of lowercase ASCII letters, digits, '-', '_', and '.'"
        );
        assert!(
            is_valid_description(pack.description),
            "each description in Plugin::PACKS must be at most 1024 bytes with no control characters"
        );
        let mut earlier = 0;
        while earlier < at {
            assert!(
                !same(P::PACKS[earlier].name, pack.name),
                "no two Packs in Plugin::PACKS may share a name"
            );
            earlier += 1;
        }
        at += 1;
    }
}

/// The arguments struct of the verb `call` names: `RV_ERR_MISUSE` when the
/// call has none, and `RV_ERR_UNSUPPORTED` when it is shorter than this
/// crate's `T`.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `args` is NULL or points to a
/// `T` made as the contract describes, for the lifetime `'a`. `T` is one
/// of the contract's arguments structs, each of which begins with its
/// `size`.
unsafe fn args<'a, T>(call: *mut RvCall) -> Result<&'a T, RvStatus> {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let args = unsafe { (*call).args }.cast::<T>();
    if args.is_null() {
        return Err(RV_ERR_MISUSE);
    }
    // SAFETY: `args` is not NULL, and `size` is the struct's first field,
    // so it is readable before the struct's length is known.
    let size = unsafe { args.cast::<u32>().read() };
    if (size as usize) < size_of::<T>() {
        return Err(RV_ERR_UNSUPPORTED);
    }
    // SAFETY: the struct is long enough for every field of `T`, and the
    // Host keeps it alive and unchanged for the call.
    Ok(unsafe { &*args })
}

/// The text `s` holds, or `None` when it is not UTF-8 or has a NULL
/// pointer with a nonzero length.
///
/// # Safety
/// When `s.len` is nonzero and `s.ptr` is not NULL, `s.ptr` must be valid
/// for reads of `s.len` bytes for the lifetime `'a`.
unsafe fn read_str<'a>(s: RvStr) -> Option<&'a str> {
    if s.len == 0 {
        return Some("");
    }
    if s.ptr.is_null() {
        return None;
    }
    // SAFETY: `s.ptr` is not NULL, and the caller guarantees it is valid
    // for `s.len` bytes.
    let bytes = unsafe { slice::from_raw_parts(s.ptr.cast::<u8>(), s.len) };
    std::str::from_utf8(bytes).ok()
}

/// Declines the open call through the Host's `refuse`, which returns
/// `RV_ERR_REFUSED`.
///
/// # Safety
/// `refuse` and `ctx` come from the arguments struct of the call open on
/// this thread.
unsafe fn refuse(
    refuse: unsafe extern "C" fn(*mut std::ffi::c_void, RvStr) -> RvStatus,
    ctx: *mut std::ffi::c_void,
    refusal: &Refusal,
) -> RvStatus {
    // SAFETY: the caller passes the open call's function and `ctx`, and
    // the reason outlives the call.
    unsafe { refuse(ctx, rv_str(refusal.reason())) }
}

/// Runs `f`, which drops something the Plugin owns, and keeps a panic in
/// it from unwinding across the C boundary.
fn contain(f: impl FnOnce()) {
    let _panicked = panic::catch_unwind(AssertUnwindSafe(f));
}

/// The text of a panic's payload, when it has one.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(message) = payload.downcast_ref::<&str>() {
        message
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message
    } else {
        "a panic payload that is not text"
    }
}

/// `s` as the contract's pointer and length.
pub(crate) const fn rv_str(s: &str) -> RvStr {
    RvStr {
        ptr: s.as_ptr().cast(),
        len: s.len(),
    }
}

/// `size_of::<T>()` as the `u32` a contract struct's `size` field holds.
#[expect(
    clippy::cast_possible_truncation,
    reason = "the assertion bounds the size to a u32"
)]
pub(crate) const fn size_field<T>() -> u32 {
    assert!(
        size_of::<T>() <= u32::MAX as usize,
        "a contract struct's size fits its u32 size field"
    );
    size_of::<T>() as u32
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use runvil_plugin_sys::{RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_PLUGIN_LOAD, RvCall, RvStr};

    use super::{entry, panic_message, read_str, size_field};
    use crate::{Plugin, Refusal};

    /// A Plugin for calls that never reach `load`.
    struct Inert;

    impl Plugin for Inert {
        const NAME: &'static str = "inert";
        const VERSION: &'static str = "1";

        fn load() -> Result<Inert, Refusal> {
            Ok(Inert)
        }
    }

    fn call(verb: u32) -> RvCall {
        RvCall {
            size: size_field::<RvCall>(),
            verb,
            host: ptr::null(),
            plugin: ptr::null_mut(),
            pack_run: ptr::null_mut(),
            args: ptr::null(),
            tool_call: ptr::null_mut(),
        }
    }

    #[test]
    fn a_null_call_is_a_misuse() {
        // SAFETY: the entry point accepts NULL.
        assert_eq!(unsafe { entry::<Inert>(ptr::null_mut()) }, RV_ERR_MISUSE);
    }

    #[test]
    fn an_unknown_verb_is_unsupported() {
        let mut call = call(9);
        // SAFETY: a verb the entry point doesn't know reads nothing but
        // the call.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_UNSUPPORTED);
    }

    #[test]
    fn a_short_call_is_unsupported() {
        let mut call = call(RV_PLUGIN_LOAD);
        call.size = 8;
        // SAFETY: the call is whole; only its `size` field claims less.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_UNSUPPORTED);
    }

    #[test]
    fn a_string_reads_as_text_only_when_it_is_utf8_with_a_pointer() {
        let text = "héllo";
        let whole = RvStr {
            ptr: text.as_ptr().cast(),
            len: text.len(),
        };
        // SAFETY: `whole` points at `text`, which outlives the read.
        assert_eq!(unsafe { read_str(whole) }, Some("héllo"));
        let empty = RvStr {
            ptr: ptr::null(),
            len: 0,
        };
        // SAFETY: a zero length reads no byte.
        assert_eq!(unsafe { read_str(empty) }, Some(""));
        let null = RvStr {
            ptr: ptr::null(),
            len: 3,
        };
        // SAFETY: a NULL pointer is refused before any byte is read.
        assert_eq!(unsafe { read_str(null) }, None);
        // The first byte of 'é' alone is not UTF-8.
        let cut = RvStr {
            ptr: text.as_ptr().cast(),
            len: 2,
        };
        // SAFETY: `cut` points at the first two bytes of `text`.
        assert_eq!(unsafe { read_str(cut) }, None);
    }

    #[test]
    fn a_panic_payload_reads_as_its_text() {
        let text: Box<dyn std::any::Any + Send> = Box::new("boom");
        assert_eq!(panic_message(&*text), "boom");
        let owned: Box<dyn std::any::Any + Send> = Box::new(String::from("bang"));
        assert_eq!(panic_message(&*owned), "bang");
        let other: Box<dyn std::any::Any + Send> = Box::new(7_u32);
        assert_eq!(panic_message(&*other), "a panic payload that is not text");
    }
}
