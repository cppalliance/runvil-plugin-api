//! A fake Host for a Plugin crate's own tests. [`load`] sends
//! `RV_PLUGIN_LOAD` to a [`Plugin`]'s entry point with an `RvLoadArgs`
//! whose functions check the Plugin's calls as the Harness does, and the
//! [`Loaded`] it returns sends `RV_PLUGIN_UNLOAD` when it drops.
//!
//! A Plugin crate enables it from `[dev-dependencies]` only:
//!
//! ```text
//! [dev-dependencies]
//! runvil-plugin-sdk = { git = "...", tag = "...", features = ["test-support"] }
//! ```

use std::cell::Cell;
use std::ffi::c_void;
use std::{ptr, slice};

use runvil_plugin_sys::{
    PluginEntry, RV_API_MAJOR, RV_API_MINOR, RV_ERR_MISUSE, RV_ERR_REFUSED, RV_OK, RV_PLUGIN_LOAD,
    RV_PLUGIN_UNLOAD, RvCall, RvHost, RvLoadArgs, RvPluginInfo, RvStatus, RvStr,
};

use crate::entry::size_field;
use crate::names::{MAX_LEN, is_valid_name, is_valid_version};
use crate::{Plugin, Refusal, entry};

/// Loads the Plugin `P` through [`entry::<P>`](entry), as the Harness
/// loads a Plugin a Host hands it.
///
/// # Errors
/// Returns the [`Failure`] the Harness would report.
pub fn load<P: Plugin>() -> Result<Loaded, Failure> {
    // SAFETY: `entry::<P>` follows the C contract and never unwinds.
    unsafe { load_entry(entry::<P>) }
}

/// Loads the Plugin behind `entry`, such as one written against
/// [`sys`](crate::sys) directly.
///
/// # Safety
/// `entry` must follow the C contract and must not unwind.
///
/// # Errors
/// Returns the [`Failure`] the Harness would report, checked in the
/// Harness's order: a refusal, then a misuse, then a status other than
/// `RV_OK`, then a missing description.
pub unsafe fn load_entry(entry: PluginEntry) -> Result<Loaded, Failure> {
    let state = Box::into_raw(Box::new(LoadState::default()));
    let ctx = state.cast::<c_void>();
    let args = RvLoadArgs {
        size: size_field::<RvLoadArgs>(),
        reserved: 0,
        ctx,
        refuse,
        describe_plugin,
    };
    let mut call = new_call(RV_PLUGIN_LOAD, ptr::null_mut(), (&raw const args).cast());
    // A load inside a load, on one thread, keeps the outer load's `ctx`.
    let outer = OPEN.with(|open| open.replace(ctx));
    // SAFETY: the caller guarantees `entry` follows the contract, and
    // `call` and `args` are live structs that nothing else uses.
    let status = unsafe { entry(&raw mut call) };
    OPEN.with(|open| open.set(outer));
    // SAFETY: `state` came from `Box::into_raw` above, and with the load
    // closed no function reads through `ctx` any more.
    let mut state = unsafe { Box::from_raw(state) };
    if let Some(reason) = state.refusal.take() {
        return Err(Failure::Refused(reason));
    }
    if let Some(misuse) = state.misuse.take() {
        return Err(Failure::Misused(misuse));
    }
    if status != RV_OK {
        return Err(Failure::Status(status));
    }
    match state.described.take() {
        Some(Described { name, version }) => Ok(Loaded {
            entry,
            plugin: call.plugin,
            name,
            version,
            _ctx: state,
            unloaded: false,
        }),
        None => Err(Failure::Undescribed),
    }
}

/// A loaded Plugin. Dropping it sends `RV_PLUGIN_UNLOAD`, as
/// [`Loaded::unload`] does.
#[derive(Debug)]
pub struct Loaded {
    entry: PluginEntry,
    plugin: *mut c_void,
    name: String,
    version: String,
    /// What the load's `ctx` points at, kept until the Plugin is unloaded,
    /// as the contract requires of a Host.
    _ctx: Box<LoadState>,
    unloaded: bool,
}

impl Loaded {
    /// The name the Plugin described itself with.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version the Plugin described itself with.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Sends `RV_PLUGIN_UNLOAD` and returns the Plugin's status, which the
    /// Harness ignores.
    #[must_use]
    pub fn unload(mut self) -> RvStatus {
        self.send_unload()
    }

    /// Sends `RV_PLUGIN_UNLOAD` the first time it is called, with no load
    /// open on this thread, so a load function the Plugin calls answers
    /// `RV_ERR_MISUSE`.
    fn send_unload(&mut self) -> RvStatus {
        if self.unloaded {
            return RV_OK;
        }
        self.unloaded = true;
        let mut call = new_call(RV_PLUGIN_UNLOAD, self.plugin, ptr::null());
        // An unload inside a load, on one thread, must not see that load.
        let outer = OPEN.try_with(|open| open.replace(ptr::null_mut()));
        // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
        // contract, and `plugin` is the instance its load set, sent once.
        let status = unsafe { (self.entry)(&raw mut call) };
        if let Ok(outer) = outer {
            let _restored = OPEN.try_with(|open| open.set(outer));
        }
        status
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let _status = self.send_unload();
    }
}

/// Why a load failed, as the Harness would report it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The Plugin declined, with this reason.
    Refused(String),
    /// The Plugin broke the contract in the way named.
    Misused(&'static str),
    /// `RV_PLUGIN_LOAD` returned this status without a refusal or a
    /// misuse.
    Status(RvStatus),
    /// `RV_PLUGIN_LOAD` returned `RV_OK` without describing the Plugin.
    Undescribed,
}

/// The `RvHost` every call hands the Plugin.
static HOST: RvHost = RvHost {
    size: size_field::<RvHost>(),
    api_major: RV_API_MAJOR,
    api_minor: RV_API_MINOR,
    reserved: 0,
};

thread_local! {
    /// The `ctx` of the `RV_PLUGIN_LOAD` call open on this thread, or NULL
    /// while none is. A load function reads through its `ctx` only when it
    /// is this one, so a call from another thread or after the load never
    /// does.
    static OPEN: Cell<*mut c_void> = const { Cell::new(ptr::null_mut()) };
}

/// What one load has reported so far; the load's `ctx` points at it.
#[derive(Debug, Default)]
struct LoadState {
    described: Option<Described>,
    refusal: Option<String>,
    misuse: Option<&'static str>,
}

/// The name and version a Plugin gave `describe_plugin`, copied.
#[derive(Debug)]
struct Described {
    name: String,
    version: String,
}

/// One string field's bound, and what each way of breaking it is called.
struct Field {
    cap: usize,
    too_long: &'static str,
    null_with_length: &'static str,
    not_utf8: &'static str,
}

const NAME: Field = Field {
    cap: MAX_LEN,
    too_long: "the name is longer than 64 bytes",
    null_with_length: "the name has a NULL pointer and a nonzero length",
    not_utf8: "the name is not UTF-8",
};

const VERSION: Field = Field {
    cap: MAX_LEN,
    too_long: "the version is longer than 64 bytes",
    null_with_length: "the version has a NULL pointer and a nonzero length",
    not_utf8: "the version is not UTF-8",
};

const REASON: Field = Field {
    cap: Refusal::MAX_REASON,
    too_long: "the refusal reason is longer than 1024 bytes",
    null_with_length: "the refusal reason has a NULL pointer and a nonzero length",
    not_utf8: "the refusal reason is not UTF-8",
};

impl Field {
    /// Copies the string `s` holds. The length is checked against the cap
    /// before any byte is read, and `s.ptr` is read only when `s.len` is
    /// nonzero, because `slice::from_raw_parts` with a NULL pointer is
    /// undefined behavior even at length 0.
    ///
    /// # Safety
    /// When `s.len` is nonzero, `s.ptr` must be valid for reads of `s.len`
    /// bytes for the duration of this call.
    unsafe fn read(&self, s: RvStr) -> Result<String, &'static str> {
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

/// An `RvCall` for `verb` with this fake Host's `RvHost`.
fn new_call(verb: u32, plugin: *mut c_void, args: *const c_void) -> RvCall {
    RvCall {
        size: size_field::<RvCall>(),
        verb,
        host: &raw const HOST,
        plugin,
        pack_run: ptr::null_mut(),
        args,
    }
}

/// Answers a load function. `f` runs only when this thread has a load open
/// and `ctx` is its `ctx`; otherwise the answer is `RV_ERR_MISUSE`, and an
/// open load records the misuse.
fn answer(ctx: *mut c_void, f: impl FnOnce(&mut LoadState) -> RvStatus) -> RvStatus {
    let open = OPEN.try_with(Cell::get).unwrap_or(ptr::null_mut());
    if open.is_null() {
        return RV_ERR_MISUSE;
    }
    // SAFETY: a non-NULL `open` is the `LoadState` of the load running on
    // this thread, which `load_entry` keeps alive for the call, and no
    // other reference to it exists while a load function runs.
    let state = unsafe { &mut *open.cast::<LoadState>() };
    if !ptr::eq(open, ctx) {
        state.misuse.get_or_insert(
            "a load function was handed a ctx other than the open RV_PLUGIN_LOAD call's",
        );
        return RV_ERR_MISUSE;
    }
    f(state)
}

/// Records the Plugin's refusal, validated; returns `RV_ERR_REFUSED`, or
/// `RV_ERR_MISUSE` for a call it doesn't answer or a second refusal.
unsafe extern "C" fn refuse(ctx: *mut c_void, reason: RvStr) -> RvStatus {
    answer(ctx, |state| {
        if state.refusal.is_some() {
            state.misuse.get_or_insert("refuse was called twice");
            return RV_ERR_MISUSE;
        }
        // SAFETY: the contract makes `reason` valid for its length during
        // this call.
        match unsafe { REASON.read(reason) } {
            Ok(reason) => {
                state.refusal = Some(reason);
                RV_ERR_REFUSED
            }
            Err(fault) => {
                state.misuse.get_or_insert(fault);
                RV_ERR_MISUSE
            }
        }
    })
}

/// Records the Plugin's description once, validated; returns `RV_OK` or
/// `RV_ERR_MISUSE`.
unsafe extern "C" fn describe_plugin(ctx: *mut c_void, info: *const RvPluginInfo) -> RvStatus {
    answer(ctx, |state| {
        // SAFETY: the contract makes `info` point at a readable
        // `RvPluginInfo` during this call.
        match unsafe { describe(state, info) } {
            Ok(()) => RV_OK,
            Err(fault) => {
                state.misuse.get_or_insert(fault);
                RV_ERR_MISUSE
            }
        }
    })
}

/// Validates `info` and copies the name and version into `state`.
///
/// # Safety
/// When `info` is not NULL, it must be readable as an `RvPluginInfo`
/// whose `size` field says how much of it the Plugin wrote, and each
/// string in it must be valid for its length, for the duration of this
/// call.
unsafe fn describe(state: &mut LoadState, info: *const RvPluginInfo) -> Result<(), &'static str> {
    if state.described.is_some() {
        return Err("describe_plugin was called twice");
    }
    if info.is_null() {
        return Err("describe_plugin was handed a NULL info");
    }
    if !info.is_aligned() {
        return Err("describe_plugin was handed a misaligned info");
    }
    // SAFETY: `info` is not NULL and is aligned, and `size` is the first
    // field of a struct the Plugin wrote, so it is readable on its own.
    let size = unsafe { (&raw const (*info).size).read() };
    if (size as usize) < size_of::<RvPluginInfo>() {
        return Err("the info's size is shorter than RvPluginInfo");
    }
    // SAFETY: `size` says the Plugin wrote a whole `RvPluginInfo`.
    let info = unsafe { &*info };
    if info.reserved != 0 {
        return Err("the info's reserved field is not zero");
    }
    // SAFETY: the caller makes each string valid for its length.
    let name = unsafe { NAME.read(info.name) }?;
    if !is_valid_name(&name) {
        return Err("the name is not one Pack id segment, such as `acme-url`");
    }
    // SAFETY: as above.
    let version = unsafe { VERSION.read(info.version) }?;
    if version.is_empty() {
        return Err("the version is empty");
    }
    if !is_valid_version(&version) {
        return Err("the version has a control character");
    }
    state.described = Some(Described { name, version });
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The `ctx` rules, through raw entry points that break them on
    //! purpose; the SDK's own entry point never does.

    use std::ffi::c_void;
    use std::ptr;
    use std::sync::Mutex;

    use runvil_plugin_sys::{
        RV_ERR_MISUSE, RV_OK, RV_PLUGIN_LOAD, RvCall, RvLoadArgs, RvPluginInfo, RvStatus, RvStr,
    };

    use super::{Failure, load_entry};
    use crate::entry::size_field;

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
            size: size_field::<RvPluginInfo>(),
            reserved: 0,
            name: RvStr {
                ptr: name.as_ptr().cast(),
                len: name.len(),
            },
            version: RvStr {
                ptr: "1".as_ptr().cast(),
                len: 1,
            },
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

    /// Describes itself through a `ctx` that isn't its load's.
    unsafe extern "C" fn wrong_ctx(call: *mut RvCall) -> RvStatus {
        // SAFETY: the fake Host made `call`.
        let Some(args) = (unsafe { load_args(call) }) else {
            return RV_OK;
        };
        let info = info("wrong-ctx");
        // SAFETY: the load's own function, handed a NULL `ctx` on purpose.
        unsafe { (args.describe_plugin)(ptr::null_mut(), &raw const info) }
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
    fn a_load_function_handed_another_ctx_is_a_recorded_misuse() {
        // SAFETY: `wrong_ctx` follows the contract apart from the call this
        // test checks, and never unwinds.
        let failure = unsafe { load_entry(wrong_ctx) }.unwrap_err();
        assert_eq!(
            failure,
            Failure::Misused(
                "a load function was handed a ctx other than the open RV_PLUGIN_LOAD call's"
            )
        );
    }
}
