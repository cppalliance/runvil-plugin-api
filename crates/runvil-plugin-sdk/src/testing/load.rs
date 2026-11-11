//! The functions a fake Host's `RvLoadArgs` carries: `refuse`,
//! `describe_plugin`, and `insert_pack`, each checking what the Plugin
//! hands it as the Harness does.

use std::ffi::c_void;

use runvil_plugin_sys::{RV_OK, RvPackInfo, RvPluginInfo, RvStatus, RvStr};

use super::InsertedPack;
use super::session::{self, Described, Field, LOAD, Shape, answer, field, shape};
use crate::names::{has_control, is_valid_name, is_valid_pack_name, is_valid_version};

const PLUGIN_INFO: Shape = shape!("describe_plugin", "info", "RvPluginInfo");
const NAME: Field = field!("the name", 64);
const VERSION: Field = field!("the version", 64);

const PACK_INFO: Shape = shape!("insert_pack", "info", "RvPackInfo");
/// Two segments of 64 bytes and the `/` between them.
const PACK_NAME: Field = field!("the Pack's name", 129);
const PACK_DESCRIPTION: Field = field!("the Pack's description", 1024);

/// What a zero `reserved` field holding anything else is called.
pub(super) const RESERVED: &str = "the info's reserved field is not zero";

/// Records the Plugin's refusal to load.
pub(super) unsafe extern "C" fn refuse(ctx: *mut c_void, reason: RvStr) -> RvStatus {
    session::refuse(ctx, &LOAD, reason)
}

/// Records the Plugin's description once, validated; returns `RV_OK` or
/// `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn describe_plugin(
    ctx: *mut c_void,
    info: *const RvPluginInfo,
) -> RvStatus {
    answer(ctx, &LOAD, |session| {
        if session.described.is_some() {
            return Err("describe_plugin was called twice");
        }
        // SAFETY: the contract makes `info` point at a readable
        // `RvPluginInfo` during this call.
        let info = unsafe { PLUGIN_INFO.read(info) }?;
        if info.reserved != 0 {
            return Err(RESERVED);
        }
        // SAFETY: the contract makes each string valid for its length.
        let name = unsafe { NAME.read(info.name) }?;
        if !is_valid_name(&name) {
            return Err("the name is not one name segment, such as `acme-url`");
        }
        // SAFETY: as above.
        let version = unsafe { VERSION.read(info.version) }?;
        if version.is_empty() {
            return Err("the version is empty");
        }
        if !is_valid_version(&version) {
            return Err("the version has a control character");
        }
        session.described = Some(Described { name, version });
        Ok(RV_OK)
    })
}

/// Records one Pack the Plugin inserts, validated; returns `RV_OK` or
/// `RV_ERR_MISUSE`.
pub(super) unsafe extern "C" fn insert_pack(ctx: *mut c_void, info: *const RvPackInfo) -> RvStatus {
    answer(ctx, &LOAD, |session| {
        // SAFETY: the contract makes `info` point at a readable
        // `RvPackInfo` during this call.
        let info = unsafe { PACK_INFO.read(info) }?;
        if info.reserved != 0 {
            return Err(RESERVED);
        }
        // SAFETY: the contract makes each string valid for its length.
        let name = unsafe { PACK_NAME.read(info.name) }?;
        if !is_valid_pack_name(&name) {
            return Err(
                "the Pack's name is not a vendor and a Pack id joined by `/`, such as `acme/url`",
            );
        }
        if session.packs.iter().any(|pack| pack.name == name) {
            return Err("two Packs of one Plugin share a name");
        }
        // SAFETY: as above.
        let description = unsafe { PACK_DESCRIPTION.read(info.description) }?;
        if has_control(&description) {
            return Err("the Pack's description has a control character");
        }
        session.packs.push(InsertedPack { name, description });
        Ok(RV_OK)
    })
}

#[cfg(test)]
mod tests {
    //! The load functions' rules, through a raw entry point that breaks
    //! one of them at a time; the SDK's own entry point never does.

    use std::ffi::c_void;
    use std::ptr;
    use std::sync::Mutex;

    use runvil_plugin_sys::{
        RV_ERR_MISUSE, RV_OK, RV_PLUGIN_LOAD, RvCall, RvLoadArgs, RvPluginInfo, RvStatus,
    };

    use crate::entry::rv_str;
    use crate::testing::fixture::{self, Breach};
    use crate::testing::{Failure, load_entry};

    type Describe = unsafe extern "C" fn(*mut c_void, *const RvPluginInfo) -> RvStatus;

    /// What `stasher` kept from its load: the `ctx` as an address, so it
    /// can cross threads, the `describe_plugin` function, and what that
    /// function answered on another thread during the load.
    #[derive(Clone, Copy)]
    struct Stash {
        ctx: usize,
        describe: Describe,
        from_another_thread: RvStatus,
    }

    static STASH: Mutex<Option<Stash>> = Mutex::new(None);

    fn info(name: &'static str) -> RvPluginInfo {
        RvPluginInfo {
            size: 40,
            reserved: 0,
            name: rv_str(name),
            version: rv_str("1"),
        }
    }

    /// The load arguments of a load call, or `None` for any other verb.
    ///
    /// # Safety
    /// `call` is a call the fake Host made.
    unsafe fn load_args<'a>(call: *mut RvCall) -> Option<&'a RvLoadArgs> {
        // SAFETY: the caller passes a call the fake Host made, whose `args`
        // is an `RvLoadArgs` for `RV_PLUGIN_LOAD`.
        unsafe { ((*call).verb == RV_PLUGIN_LOAD).then(|| &*(*call).args.cast::<RvLoadArgs>()) }
    }

    /// Calls `describe_plugin` from another thread during its load, keeps
    /// the `ctx` for later, then describes itself on the loading thread.
    unsafe extern "C" fn stasher(call: *mut RvCall) -> RvStatus {
        // SAFETY: the fake Host made `call`.
        let Some(args) = (unsafe { load_args(call) }) else {
            return RV_OK;
        };
        let ctx = args.ctx.expose_provenance();
        let describe = args.describe_plugin;
        let from_another_thread = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    let info = info("stasher");
                    // SAFETY: the load's own `ctx` and function, called from
                    // a thread the contract says gets `RV_ERR_MISUSE`.
                    unsafe { describe(ptr::with_exposed_provenance_mut(ctx), &raw const info) }
                })
                .join()
                .unwrap_or(RV_OK)
        });
        if let Ok(mut stash) = STASH.lock() {
            *stash = Some(Stash {
                ctx,
                describe,
                from_another_thread,
            });
        }
        let info = info("stasher");
        // SAFETY: the open load's own `ctx`, on its thread.
        let status = unsafe { describe(args.ctx, &raw const info) };
        // SAFETY: the fake Host made `call`, and this load sets the
        // instance it unloads; no instance is ever read through it.
        unsafe { (*call).plugin = ptr::NonNull::<u8>::dangling().as_ptr().cast() };
        status
    }

    #[test]
    fn a_load_function_answers_only_during_the_load_and_on_its_thread() {
        // SAFETY: `stasher` follows the contract apart from the calls this
        // test checks, and never unwinds.
        let loaded = unsafe { load_entry(stasher) }.unwrap();
        assert_eq!(loaded.name(), "stasher");
        let stash = STASH.lock().unwrap().unwrap();
        assert_eq!(stash.from_another_thread, RV_ERR_MISUSE);
        let info = info("stasher");
        // SAFETY: a late call through a `ctx` the fake Host keeps valid
        // while the Plugin is loaded.
        let late = unsafe {
            (stash.describe)(ptr::with_exposed_provenance_mut(stash.ctx), &raw const info)
        };
        assert_eq!(late, RV_ERR_MISUSE);
        assert_eq!(loaded.unload(), RV_OK);
    }

    #[test]
    fn a_load_that_follows_the_contract_loads_with_its_pack() {
        let loaded = fixture::load(Breach::None).unwrap();
        assert_eq!((loaded.name(), loaded.version()), ("fixture", "1"));
        assert_eq!(loaded.packs().len(), 1);
        assert_eq!(loaded.packs()[0].name, "acme/fixture");
        assert_eq!(loaded.packs()[0].description, "A fixture Pack");
    }

    #[test]
    fn each_breach_of_a_load_function_is_a_recorded_misuse() {
        for (breach, misuse) in [
            (
                Breach::LoadCtx,
                "a load function was handed a ctx other than the open RV_PLUGIN_LOAD call's",
            ),
            (Breach::DescribeTwice, "describe_plugin was called twice"),
            (
                Breach::PluginName,
                "the name is not one name segment, such as `acme-url`",
            ),
            (Breach::NoVersion, "the version is empty"),
            (
                Breach::PackName,
                "the Pack's name is not a vendor and a Pack id joined by `/`, such as `acme/url`",
            ),
            (Breach::PackTwice, "two Packs of one Plugin share a name"),
            (
                Breach::PackDescription,
                "the Pack's description has a control character",
            ),
            (
                Breach::PackReserved,
                "the info's reserved field is not zero",
            ),
            (
                Breach::PackFunctionInLoad,
                "a Pack function was called outside RV_PACK_NEW",
            ),
        ] {
            assert_eq!(
                fixture::load(breach).unwrap_err(),
                Failure::Misused(misuse),
                "{breach:?}"
            );
        }
    }

    #[test]
    fn a_load_that_returns_without_describing_the_plugin_is_undescribed() {
        assert_eq!(
            fixture::load(Breach::Undescribed).unwrap_err(),
            Failure::Undescribed
        );
    }

    #[test]
    fn a_second_refusal_is_a_misuse_and_the_first_still_names_the_reason() {
        assert_eq!(
            fixture::load(Breach::RefuseLoadTwice).unwrap_err(),
            Failure::Refused("first".to_owned())
        );
    }
}
