//! The C entry point every [`Plugin`] gets: [`entry`], which answers each
//! verb of the contract for the Plugin type it is instantiated with.

use std::any::Any;
use std::mem::size_of;
use std::panic::{self, AssertUnwindSafe};

use runvil_plugin_sys::{
    RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_OK, RV_PLUGIN_LOAD,
    RV_PLUGIN_UNLOAD, RvCall, RvHost, RvLoadArgs, RvPluginInfo, RvStatus, RvStr,
};

use crate::names::{is_valid_name, is_valid_version};
use crate::{Plugin, Refusal};

/// The C entry point of the Plugin `P`, with the shape of
/// [`PluginEntry`](runvil_plugin_sys::PluginEntry).
///
/// A library Plugin exports it with
/// [`export_plugin!`](crate::export_plugin). A Host that links `P` in
/// hands `entry::<P>` to the Harness, which needs no exported symbol, so
/// any number of linked Plugins share one binary.
///
/// It answers `RV_PLUGIN_LOAD` by checking the Host's version and the
/// load's [`RvLoadArgs`], making the instance with [`Plugin::load`], and
/// describing the Plugin with [`Plugin::NAME`] and [`Plugin::VERSION`]. It
/// answers `RV_PLUGIN_UNLOAD` by dropping the instance, and any other verb
/// with `RV_ERR_UNSUPPORTED`. A Host whose version this crate does not
/// speak, or whose structs are shorter than this crate's, gets
/// `RV_ERR_UNSUPPORTED` before anything is made.
///
/// Building `entry::<P>` fails when `P::NAME` or `P::VERSION` breaks the
/// rules the Harness checks.
///
/// # Safety
/// `call` must be NULL or point to an `RvCall` made as the contract in
/// [`runvil_plugin_sys`] describes, whose `host`, `args`, and instance
/// pointer follow it too, for the duration of the call.
pub unsafe extern "C" fn entry<P: Plugin>(call: *mut RvCall) -> RvStatus {
    const {
        assert!(
            is_valid_name(P::NAME),
            "Plugin::NAME must be 1 to 64 bytes of lowercase ASCII letters, digits, '-', '_', and '.'"
        );
        assert!(
            is_valid_version(P::VERSION),
            "Plugin::VERSION must be 1 to 64 bytes with no control characters"
        );
    }
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
    match verb {
        // SAFETY: the whole `RvCall` is there, and the caller vouches for
        // its `host` and `args`.
        RV_PLUGIN_LOAD => unsafe { load::<P>(call) },
        // SAFETY: the whole `RvCall` is there, and the caller vouches for
        // its instance pointer.
        RV_PLUGIN_UNLOAD => unsafe { unload::<P>(call) },
        _ => RV_ERR_UNSUPPORTED,
    }
}

/// Answers `RV_PLUGIN_LOAD`: checks the Host's version, then the load's
/// arguments, makes the instance, describes the Plugin once, and only then
/// hands the instance to the Host. Every path that fails frees what it
/// made. The version comes first, so a Host of another version gets
/// `RV_ERR_UNSUPPORTED` whatever its `args` hold.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `host` is NULL or an `RvHost`,
/// and whose `args` is NULL or an `RvLoadArgs`, that follow the contract,
/// as [`entry`]'s caller guarantees.
unsafe fn load<P: Plugin>(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let host = unsafe { (*call).host };
    if host.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: `host` is not NULL, and `size` is its first field, so it is
    // readable before the struct's length is known.
    let host_size = unsafe { (&raw const (*host).size).read() };
    if (host_size as usize) < size_of::<RvHost>() {
        return RV_ERR_UNSUPPORTED;
    }
    // SAFETY: the struct is long enough for every field this reads, and the
    // Host keeps it alive and unchanged for the call.
    let host = unsafe { &*host };
    if !speaks(host.api_major, host.api_minor) {
        return RV_ERR_UNSUPPORTED;
    }
    // SAFETY: the caller makes `call` point to a whole `RvCall`, and for
    // this verb its `args` is NULL or an `RvLoadArgs`.
    let args = unsafe { (*call).args }.cast::<RvLoadArgs>();
    if args.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: `args` is not NULL, and `size` is its first field, so it is
    // readable before the struct's length is known.
    let args_size = unsafe { (&raw const (*args).size).read() };
    if (args_size as usize) < size_of::<RvLoadArgs>() {
        return RV_ERR_UNSUPPORTED;
    }
    // SAFETY: the struct is long enough for every field this reads, and the
    // Host keeps it alive and unchanged for the call.
    let args = unsafe { &*args };
    let instance = match panic::catch_unwind(P::load) {
        Ok(Ok(instance)) => Box::new(instance),
        // SAFETY: `args` belongs to the open load call.
        Ok(Err(refusal)) => return unsafe { refuse(args, &refusal) },
        Err(payload) => {
            let refusal = Refusal::new(format!(
                "the Plugin panicked while loading: {}",
                panic_message(&*payload)
            ));
            // SAFETY: as above.
            return unsafe { refuse(args, &refusal) };
        }
    };
    let info = RvPluginInfo {
        size: size_field::<RvPluginInfo>(),
        reserved: 0,
        name: rv_str(P::NAME),
        version: rv_str(P::VERSION),
    };
    // SAFETY: `args` belongs to the open load call, this is its thread, and
    // `info` and its strings outlive the call.
    let status = unsafe { (args.describe_plugin)(args.ctx, &raw const info) };
    if status != RV_OK {
        contain(move || drop(instance));
        return status;
    }
    // SAFETY: the caller makes `call` point to a whole `RvCall`, which
    // nothing else reads or writes while this call runs.
    unsafe { (&raw mut (*call).plugin).write(Box::into_raw(instance).cast()) };
    RV_OK
}

/// Answers `RV_PLUGIN_UNLOAD`: drops the instance `RV_PLUGIN_LOAD` set,
/// containing a panic in its `Drop`, because the Host has no one to tell.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `plugin` is NULL or the
/// pointer `load::<P>` set, not yet unloaded.
unsafe fn unload<P: Plugin>(call: *mut RvCall) -> RvStatus {
    // SAFETY: the caller makes `call` point to a whole `RvCall`.
    let plugin = unsafe { (*call).plugin };
    if plugin.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: a non-NULL `plugin` is the `Box<P>` a successful load
    // leaked, and the Harness unloads each load once, so this is its only
    // free.
    let instance = unsafe { Box::from_raw(plugin.cast::<P>()) };
    contain(move || drop(instance));
    RV_OK
}

/// Declines the load through the Host's `refuse`, which returns
/// `RV_ERR_REFUSED`.
///
/// # Safety
/// `args` belongs to the `RV_PLUGIN_LOAD` call open on this thread.
unsafe fn refuse(args: &RvLoadArgs, refusal: &Refusal) -> RvStatus {
    // SAFETY: the caller passes the open load's arguments, and the reason
    // outlives the call.
    unsafe { (args.refuse)(args.ctx, rv_str(refusal.reason())) }
}

/// Whether a Host table reporting version `major`.`minor` speaks the
/// contract this crate was built for: exactly that version while the
/// major version is 0, and from 1.0 the same major version with a minor
/// version at least as high.
const fn speaks(major: u32, minor: u32) -> bool {
    if RV_API_MAJOR == 0 {
        major == RV_API_MAJOR && minor == RV_API_MINOR
    } else {
        major == RV_API_MAJOR && minor >= RV_API_MINOR
    }
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
const fn rv_str(s: &str) -> RvStr {
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
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use runvil_plugin_sys::{
        RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_OK, RV_PLUGIN_LOAD,
        RV_PLUGIN_UNLOAD, RvCall, RvHost, RvLoadArgs, RvPluginInfo, RvStatus, RvStr,
    };

    use super::{entry, size_field, speaks};
    use crate::testing::{self, Failure};
    use crate::{Plugin, Refusal};

    /// How many `Counted` instances are alive. One test makes them, so
    /// tests running in parallel never share the count.
    static COUNTED_LIVE: AtomicIsize = AtomicIsize::new(0);
    /// How many `Undescribed` instances are alive, for the one test that
    /// makes them.
    static UNDESCRIBED_LIVE: AtomicIsize = AtomicIsize::new(0);

    struct Counted;

    impl Plugin for Counted {
        const NAME: &'static str = "counted";
        const VERSION: &'static str = "1.2.3";

        fn load() -> Result<Counted, Refusal> {
            COUNTED_LIVE.fetch_add(1, Ordering::SeqCst);
            Ok(Counted)
        }
    }

    impl Drop for Counted {
        fn drop(&mut self) {
            COUNTED_LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct Undescribed;

    impl Plugin for Undescribed {
        const NAME: &'static str = "undescribed";
        const VERSION: &'static str = "1";

        fn load() -> Result<Undescribed, Refusal> {
            UNDESCRIBED_LIVE.fetch_add(1, Ordering::SeqCst);
            Ok(Undescribed)
        }
    }

    impl Drop for Undescribed {
        fn drop(&mut self) {
            UNDESCRIBED_LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct Declines;

    impl Plugin for Declines {
        const NAME: &'static str = "declines";
        const VERSION: &'static str = "1";

        fn load() -> Result<Declines, Refusal> {
            Err(Refusal::new("no API key is configured"))
        }
    }

    struct PanicsOnLoad;

    impl Plugin for PanicsOnLoad {
        const NAME: &'static str = "panics-on-load";
        const VERSION: &'static str = "1";

        fn load() -> Result<PanicsOnLoad, Refusal> {
            panic!("boom")
        }
    }

    struct PanicsOnDrop;

    impl Plugin for PanicsOnDrop {
        const NAME: &'static str = "panics-on-drop";
        const VERSION: &'static str = "1";

        fn load() -> Result<PanicsOnDrop, Refusal> {
            Ok(PanicsOnDrop)
        }
    }

    impl Drop for PanicsOnDrop {
        fn drop(&mut self) {
            panic!("boom while dropping");
        }
    }

    /// A Plugin for the hand-built calls, which never reach `load` on a
    /// correct path.
    struct Inert;

    impl Plugin for Inert {
        const NAME: &'static str = "inert";
        const VERSION: &'static str = "1";

        fn load() -> Result<Inert, Refusal> {
            Ok(Inert)
        }
    }

    unsafe extern "C" fn refuse_as_misuse(_ctx: *mut c_void, _reason: RvStr) -> RvStatus {
        RV_ERR_MISUSE
    }

    unsafe extern "C" fn describe_as_misuse(
        _ctx: *mut c_void,
        _info: *const RvPluginInfo,
    ) -> RvStatus {
        RV_ERR_MISUSE
    }

    fn host(size: u32, api_major: u32, api_minor: u32) -> RvHost {
        RvHost {
            size,
            api_major,
            api_minor,
            reserved: 0,
        }
    }

    /// Load arguments that answer every function `RV_ERR_MISUSE`, so a
    /// load that reached them would not return `RV_ERR_UNSUPPORTED`.
    fn misuse_args(size: u32) -> RvLoadArgs {
        RvLoadArgs {
            size,
            reserved: 0,
            ctx: ptr::null_mut(),
            refuse: refuse_as_misuse,
            describe_plugin: describe_as_misuse,
        }
    }

    fn call(verb: u32, host: *const RvHost, args: *const RvLoadArgs) -> RvCall {
        RvCall {
            size: size_field::<RvCall>(),
            verb,
            host,
            plugin: ptr::null_mut(),
            pack_run: ptr::null_mut(),
            args: args.cast(),
        }
    }

    #[test]
    fn a_plugin_loads_describes_itself_and_unloads_once() {
        let loaded = testing::load::<Counted>().unwrap();
        assert_eq!((loaded.name(), loaded.version()), ("counted", "1.2.3"));
        assert_eq!(COUNTED_LIVE.load(Ordering::SeqCst), 1);
        assert_eq!(loaded.unload(), RV_OK);
        assert_eq!(COUNTED_LIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_refusal_reaches_the_host_with_its_reason() {
        let failure = testing::load::<Declines>().unwrap_err();
        assert_eq!(
            failure,
            Failure::Refused("no API key is configured".to_owned())
        );
    }

    #[test]
    fn a_panic_while_loading_becomes_a_refusal() {
        let failure = testing::load::<PanicsOnLoad>().unwrap_err();
        assert_eq!(
            failure,
            Failure::Refused("the Plugin panicked while loading: boom".to_owned())
        );
    }

    #[test]
    fn a_panic_while_unloading_stays_inside_the_plugin() {
        let loaded = testing::load::<PanicsOnDrop>().unwrap();
        assert_eq!(loaded.unload(), RV_OK);
    }

    #[test]
    fn a_failed_description_frees_the_instance_and_returns_the_status() {
        let host = host(size_field::<RvHost>(), RV_API_MAJOR, RV_API_MINOR);
        let args = misuse_args(size_field::<RvLoadArgs>());
        let mut call = call(RV_PLUGIN_LOAD, &raw const host, &raw const args);
        // SAFETY: `call`, `host`, and `args` follow the contract's layout
        // and live for the call.
        let status = unsafe { entry::<Undescribed>(&raw mut call) };
        assert_eq!(status, RV_ERR_MISUSE);
        assert!(call.plugin.is_null());
        assert_eq!(UNDESCRIBED_LIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_null_call_is_a_misuse() {
        // SAFETY: the entry point accepts NULL.
        assert_eq!(unsafe { entry::<Inert>(ptr::null_mut()) }, RV_ERR_MISUSE);
    }

    #[test]
    fn an_unknown_verb_is_unsupported() {
        let mut call = call(3, ptr::null(), ptr::null());
        // SAFETY: a verb the entry point doesn't know reads nothing but
        // the call.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_UNSUPPORTED);
    }

    #[test]
    fn a_short_call_is_unsupported() {
        let mut call = call(RV_PLUGIN_LOAD, ptr::null(), ptr::null());
        call.size = 8;
        // SAFETY: the call is whole; only its `size` field claims less.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_UNSUPPORTED);
    }

    #[test]
    fn a_load_without_a_host_is_a_misuse() {
        let mut call = call(RV_PLUGIN_LOAD, ptr::null(), ptr::null());
        // SAFETY: the entry point checks `host` before reading through it.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_MISUSE);
    }

    #[test]
    fn a_load_without_arguments_is_a_misuse() {
        let host = host(size_field::<RvHost>(), RV_API_MAJOR, RV_API_MINOR);
        let mut call = call(RV_PLUGIN_LOAD, &raw const host, ptr::null());
        // SAFETY: the entry point checks `args` before reading through it.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_MISUSE);
    }

    #[test]
    fn short_load_arguments_are_unsupported_before_anything_is_made() {
        let host = host(size_field::<RvHost>(), RV_API_MAJOR, RV_API_MINOR);
        let args = misuse_args(8);
        let mut call = call(RV_PLUGIN_LOAD, &raw const host, &raw const args);
        // SAFETY: the structs are whole; only the `size` of `args` claims
        // less.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_UNSUPPORTED);
        assert!(call.plugin.is_null());
    }

    #[test]
    fn a_host_of_another_version_or_a_short_host_is_unsupported_before_anything_is_made() {
        let full = size_field::<RvHost>();
        let args = misuse_args(size_field::<RvLoadArgs>());
        for (size, major, minor) in [
            (full, RV_API_MAJOR + 1, RV_API_MINOR),
            (full, RV_API_MAJOR, RV_API_MINOR + 1),
            (full, RV_API_MAJOR, RV_API_MINOR - 1),
            (8, RV_API_MAJOR, RV_API_MINOR),
        ] {
            let host = host(size, major, minor);
            let mut call = call(RV_PLUGIN_LOAD, &raw const host, &raw const args);
            // SAFETY: `call`, `host`, and `args` follow the contract's
            // layout and live for the call.
            let status = unsafe { entry::<Inert>(&raw mut call) };
            assert_eq!(status, RV_ERR_UNSUPPORTED, "{size} {major}.{minor}");
            assert!(call.plugin.is_null());
        }
    }

    #[test]
    fn a_0_1_host_without_load_arguments_is_unsupported_not_a_misuse() {
        let host = host(size_field::<RvHost>(), 0, 1);
        let mut call = call(RV_PLUGIN_LOAD, &raw const host, ptr::null());
        // SAFETY: the version check comes before `args` is read.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_UNSUPPORTED);
    }

    #[test]
    fn an_unload_without_an_instance_is_a_misuse() {
        let mut call = call(RV_PLUGIN_UNLOAD, ptr::null(), ptr::null());
        // SAFETY: the entry point checks `plugin` before freeing it.
        assert_eq!(unsafe { entry::<Inert>(&raw mut call) }, RV_ERR_MISUSE);
    }

    #[test]
    fn a_0x_host_must_report_exactly_this_version() {
        assert!(speaks(RV_API_MAJOR, RV_API_MINOR));
        assert!(!speaks(RV_API_MAJOR, RV_API_MINOR + 1));
        assert!(!speaks(RV_API_MAJOR, RV_API_MINOR - 1));
        assert!(!speaks(RV_API_MAJOR + 1, RV_API_MINOR));
    }
}
