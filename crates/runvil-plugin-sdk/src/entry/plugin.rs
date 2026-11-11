//! `RV_PLUGIN_LOAD` and `RV_PLUGIN_UNLOAD`: making the Plugin instance,
//! describing the Plugin, inserting its Packs, and dropping the instance.

use std::mem::size_of;
use std::panic;

use runvil_plugin_sys::{
    RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_OK, RvCall, RvHost,
    RvLoadArgs, RvPackInfo, RvPluginInfo, RvStatus,
};

use super::{args, contain, panic_message, refuse, rv_str, size_field};
use crate::{Plugin, Refusal};

/// Answers `RV_PLUGIN_LOAD`: checks the Host's version, then the load's
/// arguments, makes the instance, describes the Plugin once, inserts each
/// of its Packs, and only then hands the instance to the Host. Every path
/// that fails frees what it made. The version comes first, so a Host of
/// another version gets `RV_ERR_UNSUPPORTED` whatever its `args` hold.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `host` is NULL or an `RvHost`,
/// and whose `args` is NULL or an `RvLoadArgs`, that follow the contract.
pub(super) unsafe fn load<P: Plugin>(call: *mut RvCall) -> RvStatus {
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
    // SAFETY: for this verb `args` is NULL or an `RvLoadArgs`.
    let args = match unsafe { args::<RvLoadArgs>(call) } {
        Ok(args) => args,
        Err(status) => return status,
    };
    let instance = match panic::catch_unwind(P::load) {
        Ok(Ok(instance)) => Box::new(instance),
        // SAFETY: `args` belongs to the open load call.
        Ok(Err(refusal)) => return unsafe { refuse(args.refuse, args.ctx, &refusal) },
        Err(payload) => {
            let refusal = Refusal::new(format!(
                "the Plugin panicked while loading: {}",
                panic_message(&*payload)
            ));
            // SAFETY: as above.
            return unsafe { refuse(args.refuse, args.ctx, &refusal) };
        }
    };
    // SAFETY: `args` belongs to the open load call, and this is its thread.
    let status = unsafe { describe::<P>(args) };
    if status != RV_OK {
        contain(move || drop(instance));
        return status;
    }
    // SAFETY: the caller makes `call` point to a whole `RvCall`, which
    // nothing else reads or writes while this call runs.
    unsafe { (&raw mut (*call).plugin).write(Box::into_raw(instance).cast()) };
    RV_OK
}

/// Describes the Plugin `P` and inserts each of its Packs, stopping at the
/// first status that is not `RV_OK`.
///
/// # Safety
/// `args` belongs to the `RV_PLUGIN_LOAD` call open on this thread.
unsafe fn describe<P: Plugin>(args: &RvLoadArgs) -> RvStatus {
    let info = RvPluginInfo {
        size: size_field::<RvPluginInfo>(),
        reserved: 0,
        name: rv_str(P::NAME),
        version: rv_str(P::VERSION),
    };
    // SAFETY: the caller passes the open load's arguments, and `info` and
    // its strings outlive the call.
    let status = unsafe { (args.describe_plugin)(args.ctx, &raw const info) };
    if status != RV_OK {
        return status;
    }
    for pack in P::PACKS {
        let info = RvPackInfo {
            size: size_field::<RvPackInfo>(),
            reserved: 0,
            name: rv_str(pack.name),
            description: rv_str(pack.description),
        };
        // SAFETY: as above.
        let status = unsafe { (args.insert_pack)(args.ctx, &raw const info) };
        if status != RV_OK {
            return status;
        }
    }
    RV_OK
}

/// Answers `RV_PLUGIN_UNLOAD`: drops the instance `RV_PLUGIN_LOAD` set,
/// containing a panic in its `Drop`, because the Host has no one to tell.
///
/// # Safety
/// `call` points to a whole `RvCall` whose `plugin` is NULL or the
/// pointer `load::<P>` set, not yet unloaded.
pub(super) unsafe fn unload<P: Plugin>(call: *mut RvCall) -> RvStatus {
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

/// Whether a Host that reports version `major`.`minor` speaks the
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

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::atomic::{AtomicIsize, Ordering};

    use runvil_plugin_sys::{
        RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_UNSUPPORTED, RV_OK, RV_PLUGIN_LOAD,
        RV_PLUGIN_UNLOAD, RvCall, RvHost, RvLoadArgs, RvPackInfo, RvPluginInfo, RvStatus, RvStr,
    };

    use super::speaks;
    use crate::entry::{entry, size_field};
    use crate::testing::{self, Failure};
    use crate::{PackInfo, Plugin, Refusal};

    /// How many `Counted` instances are alive. One test makes them, so
    /// tests running in parallel never share the count.
    static COUNTED_LIVE: AtomicIsize = AtomicIsize::new(0);
    /// How many `Undescribed` instances are alive, for the one test that
    /// makes them.
    static UNDESCRIBED_LIVE: AtomicIsize = AtomicIsize::new(0);
    /// How many `Uninserted` instances are alive, for the one test that
    /// makes them.
    static UNINSERTED_LIVE: AtomicIsize = AtomicIsize::new(0);

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

    /// A Plugin with one Pack, for a Host that accepts the description
    /// and refuses the Pack.
    struct Uninserted;

    impl Plugin for Uninserted {
        const NAME: &'static str = "uninserted";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[PackInfo::new("acme/url")];

        fn load() -> Result<Uninserted, Refusal> {
            UNINSERTED_LIVE.fetch_add(1, Ordering::SeqCst);
            Ok(Uninserted)
        }
    }

    impl Drop for Uninserted {
        fn drop(&mut self) {
            UNINSERTED_LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    struct TwoPacks;

    impl Plugin for TwoPacks {
        const NAME: &'static str = "two-packs";
        const VERSION: &'static str = "1";
        const PACKS: &'static [PackInfo] = &[
            PackInfo::new("acme/url").description("Fetch and search the web"),
            PackInfo::new("acme/mail"),
        ];

        fn load() -> Result<TwoPacks, Refusal> {
            Ok(TwoPacks)
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

    unsafe extern "C" fn describe_as_ok(_ctx: *mut c_void, _info: *const RvPluginInfo) -> RvStatus {
        RV_OK
    }

    unsafe extern "C" fn insert_as_misuse(_ctx: *mut c_void, _info: *const RvPackInfo) -> RvStatus {
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
            insert_pack: insert_as_misuse,
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
            tool_call: ptr::null_mut(),
        }
    }

    #[test]
    fn a_plugin_loads_describes_itself_and_unloads_once() {
        let loaded = testing::load::<Counted>().unwrap();
        assert_eq!((loaded.name(), loaded.version()), ("counted", "1.2.3"));
        assert_eq!(loaded.packs().len(), 0);
        assert_eq!(COUNTED_LIVE.load(Ordering::SeqCst), 1);
        assert_eq!(loaded.unload(), RV_OK);
        assert_eq!(COUNTED_LIVE.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_plugin_inserts_each_of_its_packs_in_order() {
        let loaded = testing::load::<TwoPacks>().unwrap();
        let packs: Vec<(&str, &str)> = loaded
            .packs()
            .iter()
            .map(|pack| (pack.name.as_str(), pack.description.as_str()))
            .collect();
        assert_eq!(
            packs,
            [("acme/url", "Fetch and search the web"), ("acme/mail", "")]
        );
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
    fn a_failed_pack_insert_frees_the_instance_and_returns_the_status() {
        let host = host(size_field::<RvHost>(), RV_API_MAJOR, RV_API_MINOR);
        let args = RvLoadArgs {
            describe_plugin: describe_as_ok,
            ..misuse_args(size_field::<RvLoadArgs>())
        };
        let mut call = call(RV_PLUGIN_LOAD, &raw const host, &raw const args);
        // SAFETY: `call`, `host`, and `args` follow the contract's layout
        // and live for the call.
        let status = unsafe { entry::<Uninserted>(&raw mut call) };
        assert_eq!(status, RV_ERR_MISUSE);
        assert!(call.plugin.is_null());
        assert_eq!(UNINSERTED_LIVE.load(Ordering::SeqCst), 0);
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
    fn an_older_host_without_load_arguments_is_unsupported_not_a_misuse() {
        let host = host(size_field::<RvHost>(), RV_API_MAJOR, RV_API_MINOR - 1);
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
