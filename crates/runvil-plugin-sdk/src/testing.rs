//! A fake Host for a Plugin crate's own tests. [`load`] sends
//! `RV_PLUGIN_LOAD` to a [`Plugin`]'s entry point with an `RvHost` table
//! that checks the Plugin's calls as the Harness does, and the [`Loaded`]
//! it returns sends `RV_PLUGIN_UNLOAD` when it drops.
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
    RV_PLUGIN_UNLOAD, RvCall, RvHost, RvPluginInfo, RvStatus, RvStr,
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
    let mut call = new_call(RV_PLUGIN_LOAD, ptr::null_mut());
    let call_ptr = &raw mut call;
    let outer = SESSION.with(|slot| slot.replace(Some(Session::open(call_ptr.cast_const()))));
    // SAFETY: the caller guarantees `entry` follows the contract, and
    // `call_ptr` points at a live `RvCall` that nothing else uses.
    let status = unsafe { entry(call_ptr) };
    let session = SESSION.with(|slot| slot.replace(outer)).unwrap_or_default();
    if let Some(reason) = session.refusal {
        return Err(Failure::Refused(reason));
    }
    if let Some(misuse) = session.misuse {
        return Err(Failure::Misused(misuse));
    }
    if status != RV_OK {
        return Err(Failure::Status(status));
    }
    match session.described {
        Some(Described { name, version }) => Ok(Loaded {
            entry,
            plugin: call.plugin,
            name,
            version,
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
    /// session open, so any `RvHost` function the Plugin calls answers
    /// `RV_ERR_MISUSE`.
    fn send_unload(&mut self) -> RvStatus {
        if self.unloaded {
            return RV_OK;
        }
        self.unloaded = true;
        let mut call = new_call(RV_PLUGIN_UNLOAD, self.plugin);
        let outer = SESSION.try_with(Cell::take).ok().flatten();
        // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
        // contract, and `plugin` is the instance its load set, sent once.
        let status = unsafe { (self.entry)(&raw mut call) };
        let _restored = SESSION.try_with(|slot| slot.set(outer));
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

/// The `RvHost` table every call hands the Plugin.
static HOST: RvHost = RvHost {
    size: size_field::<RvHost>(),
    api_major: RV_API_MAJOR,
    api_minor: RV_API_MINOR,
    reserved: 0,
    refuse,
    describe_plugin,
};

thread_local! {
    /// What the `RV_PLUGIN_LOAD` call open on this thread has reported, or
    /// `None` while no load runs. A `Cell`, so no borrow is ever held.
    static SESSION: Cell<Option<Session>> = const { Cell::new(None) };
}

/// What one `RV_PLUGIN_LOAD` call has reported so far.
#[derive(Default)]
struct Session {
    /// The address of the open call, compared and never dereferenced.
    call: Option<*const RvCall>,
    described: Option<Described>,
    refusal: Option<String>,
    misuse: Option<&'static str>,
}

impl Session {
    /// A session open on `call`, with nothing reported yet.
    fn open(call: *const RvCall) -> Session {
        Session {
            call: Some(call),
            ..Session::default()
        }
    }
}

/// The name and version a Plugin gave `describe_plugin`, copied.
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

/// An `RvCall` for `verb` with this fake Host's table.
fn new_call(verb: u32, plugin: *mut c_void) -> RvCall {
    RvCall {
        size: size_field::<RvCall>(),
        verb,
        host: &raw const HOST,
        plugin,
        pack_run: ptr::null_mut(),
        args: ptr::null(),
    }
}

/// Answers an `RvHost` function. `f` runs only when this thread has an
/// `RV_PLUGIN_LOAD` call open and `call` is its address; otherwise the
/// answer is `RV_ERR_MISUSE`, and an open session records the misuse.
fn answer(call: *const RvCall, f: impl FnOnce(&mut Session) -> RvStatus) -> RvStatus {
    SESSION
        .try_with(|slot| {
            let Some(mut session) = slot.take() else {
                return RV_ERR_MISUSE;
            };
            let status = if session.call.is_some_and(|open| ptr::eq(open, call)) {
                f(&mut session)
            } else {
                session.misuse.get_or_insert(
                    "an RvHost function was handed a call other than the open RV_PLUGIN_LOAD call",
                );
                RV_ERR_MISUSE
            };
            slot.set(Some(session));
            status
        })
        .unwrap_or(RV_ERR_MISUSE)
}

/// Records the Plugin's refusal, validated; returns `RV_ERR_REFUSED`, or
/// `RV_ERR_MISUSE` for a call it doesn't answer or a second refusal.
unsafe extern "C" fn refuse(call: *const RvCall, reason: RvStr) -> RvStatus {
    answer(call, |session| {
        if session.refusal.is_some() {
            session.misuse.get_or_insert("refuse was called twice");
            return RV_ERR_MISUSE;
        }
        // SAFETY: the contract makes `reason` valid for its length during
        // this call.
        match unsafe { REASON.read(reason) } {
            Ok(reason) => {
                session.refusal = Some(reason);
                RV_ERR_REFUSED
            }
            Err(fault) => {
                session.misuse.get_or_insert(fault);
                RV_ERR_MISUSE
            }
        }
    })
}

/// Records the Plugin's description once, validated; returns `RV_OK` or
/// `RV_ERR_MISUSE`.
unsafe extern "C" fn describe_plugin(call: *const RvCall, info: *const RvPluginInfo) -> RvStatus {
    answer(call, |session| {
        // SAFETY: the contract makes `info` point at a readable
        // `RvPluginInfo` during this call.
        match unsafe { describe(session, info) } {
            Ok(()) => RV_OK,
            Err(fault) => {
                session.misuse.get_or_insert(fault);
                RV_ERR_MISUSE
            }
        }
    })
}

/// Validates `info` and copies the name and version into `session`.
///
/// # Safety
/// When `info` is not NULL, it must be readable as an `RvPluginInfo`
/// whose `size` field says how much of it the Plugin wrote, and each
/// string in it must be valid for its length, for the duration of this
/// call.
unsafe fn describe(session: &mut Session, info: *const RvPluginInfo) -> Result<(), &'static str> {
    if session.described.is_some() {
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
    session.described = Some(Described { name, version });
    Ok(())
}
