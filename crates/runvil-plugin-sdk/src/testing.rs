//! A fake Host for a Plugin crate's own tests. It speaks the C contract to
//! a [`Plugin`]'s entry point and checks every call the Plugin makes back
//! as the Harness does, so a test that passes here loads there.
//!
//! [`load`] sends `RV_PLUGIN_LOAD` and returns the [`Loaded`] Plugin.
//! [`Loaded::new_pack`] sends `RV_PACK_NEW` and returns one run's
//! [`PackRun`], and [`PackRun::call`] runs one tool call to its end.
//! Dropping each value sends the verb that frees what it stands for.
//!
//! ```text
//! let loaded = testing::load::<Acme>().unwrap();
//! let pack = loaded.new_pack("acme/url").unwrap();
//! let output = pack.call("fetch", r#"{"url":"https://example.com"}"#).unwrap();
//! ```
//!
//! A Plugin crate enables it from `[dev-dependencies]` only:
//!
//! ```text
//! [dev-dependencies]
//! runvil-plugin-sdk = { git = "...", tag = "...", features = ["test-support"] }
//! ```

mod call;
#[cfg(test)]
mod fixture;
mod json;
mod load;
mod pack;
mod session;

use std::ffi::c_void;
use std::{mem, ptr};

use runvil_plugin_sys::{
    PluginEntry, RV_API_MAJOR, RV_API_MINOR, RV_OK, RV_PACK_DELETE, RV_PACK_NEW, RV_PENDING,
    RV_PLUGIN_LOAD, RV_PLUGIN_UNLOAD, RV_TOOL_CALL, RV_TOOL_CANCEL, RvCall, RvHost, RvLoadArgs,
    RvPackNewArgs, RvStatus, RvToolCallArgs,
};

use self::session::{Described, Session, Verb};
use crate::entry::{rv_str, size_field};
use crate::{Plugin, ToolError, ToolInfo, ToolOutput, entry};

/// How a tool call ended: its output, or its failure.
pub type ToolResult = Result<ToolOutput, ToolError>;

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
    let (status, mut session) = session::open(Verb::Load, |ctx| {
        let args = RvLoadArgs {
            size: size_field::<RvLoadArgs>(),
            reserved: 0,
            ctx,
            refuse: load::refuse,
            describe_plugin: load::describe_plugin,
            insert_pack: load::insert_pack,
        };
        call.args = (&raw const args).cast();
        // SAFETY: the caller guarantees `entry` follows the contract, and
        // `call` and `args` are live structs that nothing else uses.
        unsafe { entry(&raw mut call) }
    });
    session.rejection(status)?;
    let Some(Described { name, version }) = session.described.take() else {
        return Err(Failure::Undescribed);
    };
    Ok(Loaded {
        entry,
        plugin: call.plugin,
        name,
        version,
        packs: mem::take(&mut session.packs),
        _session: session,
        unloaded: false,
    })
}

/// A loaded Plugin. Dropping it sends `RV_PLUGIN_UNLOAD`, as
/// [`Loaded::unload`] does.
#[derive(Debug)]
pub struct Loaded {
    entry: PluginEntry,
    plugin: *mut c_void,
    name: String,
    version: String,
    packs: Vec<InsertedPack>,
    /// What the load's `ctx` points at, kept until the Plugin is unloaded,
    /// as the contract requires of a Host.
    _session: Box<Session>,
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

    /// The Packs the Plugin inserted as it loaded, in order.
    #[must_use]
    pub fn packs(&self) -> &[InsertedPack] {
        &self.packs
    }

    /// Sends `RV_PACK_NEW` for the Pack named `pack`, as the Harness does
    /// for each run that holds the Pack, and returns the instance with the
    /// tools it added.
    ///
    /// # Errors
    /// Returns the [`Failure`] the Harness would report, checked in the
    /// Harness's order: a refusal, then a misuse, then a status other than
    /// `RV_OK`. A rejected call gets no `RV_PACK_DELETE`.
    ///
    /// # Panics
    /// Panics when the Plugin inserted no Pack named `pack`, which the
    /// Harness never asks for.
    pub fn new_pack(&self, pack: &str) -> Result<PackRun<'_>, Failure> {
        assert!(
            self.packs.iter().any(|inserted| inserted.name == pack),
            "the Plugin inserted no Pack named {pack}"
        );
        let mut call = new_call(RV_PACK_NEW, self.plugin);
        let (status, mut session) = session::open(Verb::PackNew, |ctx| {
            let args = RvPackNewArgs {
                size: size_field::<RvPackNewArgs>(),
                reserved: 0,
                ctx,
                pack: rv_str(pack),
                refuse: pack::refuse,
                add_tool: pack::add_tool,
            };
            call.args = (&raw const args).cast();
            // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
            // contract, and `call` and `args` are live structs that
            // nothing else uses.
            unsafe { (self.entry)(&raw mut call) }
        });
        session.rejection(status)?;
        Ok(PackRun {
            loaded: self,
            instance: call.pack_run,
            tools: mem::take(&mut session.tools),
        })
    }

    /// Sends `RV_PLUGIN_UNLOAD` and returns the Plugin's status, which the
    /// Harness ignores.
    #[must_use]
    pub fn unload(mut self) -> RvStatus {
        self.send_unload()
    }

    /// Sends `RV_PLUGIN_UNLOAD` the first time it is called, with no call
    /// open on this thread, so a function the Plugin calls answers
    /// `RV_ERR_MISUSE`.
    fn send_unload(&mut self) -> RvStatus {
        if self.unloaded {
            return RV_OK;
        }
        self.unloaded = true;
        let mut call = new_call(RV_PLUGIN_UNLOAD, self.plugin);
        // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
        // contract, and `plugin` is the instance its load set, sent once.
        session::closed(|| unsafe { (self.entry)(&raw mut call) })
    }
}

impl Drop for Loaded {
    fn drop(&mut self) {
        let _status = self.send_unload();
    }
}

/// A Pack the Plugin inserted as it loaded.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct InsertedPack {
    /// The Pack's name, such as `acme/url`.
    pub name: String,
    /// The line saying what the Pack offers.
    pub description: String,
}

/// One run's instance of a Pack. Dropping it sends `RV_PACK_DELETE`. It
/// borrows the [`Loaded`] Plugin, so the Plugin is unloaded only after
/// each of its instances is gone.
#[derive(Debug)]
pub struct PackRun<'a> {
    loaded: &'a Loaded,
    /// What `RV_PACK_NEW` left in `call->pack_run`.
    instance: *mut c_void,
    tools: Vec<ToolInfo>,
}

impl PackRun<'_> {
    /// The tools the instance added as it was made, in order.
    #[must_use]
    pub fn tools(&self) -> &[ToolInfo] {
        &self.tools
    }

    /// Runs one call of `tool` with `arguments`, the JSON text of its
    /// arguments, to its end: sends `RV_TOOL_CALL` until a step stops
    /// returning `RV_PENDING`. Returns the tool's own result.
    ///
    /// # Errors
    /// Returns the [`Failure`] the Harness would report for a call that
    /// breaks the contract; see [`Call::step`].
    ///
    /// # Panics
    /// Panics when the instance added no tool named `tool`, which the
    /// Harness never calls.
    pub fn call(&self, tool: &str, arguments: &str) -> Result<ToolResult, Failure> {
        let mut call = self.start(tool, arguments);
        loop {
            if let Some(result) = call.step()? {
                return Ok(result);
            }
        }
    }

    /// Starts one call of `tool` with `arguments`, the JSON text of its
    /// arguments, for a test that sends each step itself. Nothing is sent
    /// until the first [`Call::step`].
    ///
    /// # Panics
    /// Panics when the instance added no tool named `tool`, which the
    /// Harness never calls.
    #[must_use]
    pub fn start(&self, tool: &str, arguments: &str) -> Call<'_> {
        assert!(
            self.tools.iter().any(|added| added.name == tool),
            "the Pack instance has no tool named {tool}"
        );
        Call {
            pack: self,
            tool: tool.to_owned(),
            arguments: arguments.to_owned(),
            state: ptr::null_mut(),
            pending: false,
            ended: false,
        }
    }

    /// An `RvCall` for `verb` on this instance.
    fn new_call(&self, verb: u32) -> RvCall {
        RvCall {
            pack_run: self.instance,
            ..new_call(verb, self.loaded.plugin)
        }
    }
}

impl Drop for PackRun<'_> {
    fn drop(&mut self) {
        let mut call = self.new_call(RV_PACK_DELETE);
        // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
        // contract, and `instance` is what its `RV_PACK_NEW` set, deleted
        // once, after each of its calls, which borrow it.
        let _status = session::closed(|| unsafe { (self.loaded.entry)(&raw mut call) });
    }
}

/// One tool call, step by step. Dropping it while its last step returned
/// `RV_PENDING` sends `RV_TOOL_CANCEL`, as the Harness does when it
/// abandons a call. It borrows the [`PackRun`], so the instance is deleted
/// only after each of its calls has ended.
#[derive(Debug)]
pub struct Call<'a> {
    pack: &'a PackRun<'a>,
    tool: String,
    arguments: String,
    /// What the last step left in `call->tool_call`.
    state: *mut c_void,
    /// Whether the last step returned `RV_PENDING`.
    pending: bool,
    ended: bool,
}

impl Call<'_> {
    /// Sends one `RV_TOOL_CALL`. Returns `None` when the step returned
    /// `RV_PENDING`, so the call needs another step, and the tool's own
    /// result once the call ended.
    ///
    /// # Errors
    /// Returns the [`Failure`] the Harness would report, checked in the
    /// Harness's order: a misuse, then a step that finished and still
    /// returned `RV_PENDING`, then a status other than `RV_OK`, then a
    /// step that returned `RV_OK` without finishing. A failure ends the
    /// call.
    ///
    /// # Panics
    /// Panics when the call has ended, which the Harness never steps.
    pub fn step(&mut self) -> Result<Option<ToolResult>, Failure> {
        assert!(!self.ended, "the call has ended and takes no more steps");
        let mut call = RvCall {
            tool_call: self.state,
            ..self.pack.new_call(RV_TOOL_CALL)
        };
        let (status, mut session) = session::open(Verb::ToolCall, |ctx| {
            let args = RvToolCallArgs {
                size: size_field::<RvToolCallArgs>(),
                reserved: 0,
                ctx,
                tool: rv_str(&self.tool),
                arguments: rv_str(&self.arguments),
                finish: call::finish,
            };
            call.args = (&raw const args).cast();
            // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
            // contract, and `call` and `args` are live structs that
            // nothing else uses.
            unsafe { (self.pack.loaded.entry)(&raw mut call) }
        });
        self.state = call.tool_call;
        self.pending = status == RV_PENDING;
        let outcome = call::judge(status, &mut session);
        if !matches!(outcome, Ok(None)) {
            self.end();
        }
        outcome
    }

    /// Ends the call: sends `RV_TOOL_CANCEL` when its last step returned
    /// `RV_PENDING`, so the Plugin frees the call's state.
    fn end(&mut self) {
        self.ended = true;
        if !mem::take(&mut self.pending) {
            return;
        }
        let mut call = RvCall {
            tool_call: self.state,
            ..self.pack.new_call(RV_TOOL_CANCEL)
        };
        // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
        // contract, and the call's last step returned `RV_PENDING`, so its
        // state is canceled here once.
        let _status = session::closed(|| unsafe { (self.pack.loaded.entry)(&raw mut call) });
    }
}

impl Drop for Call<'_> {
    fn drop(&mut self) {
        self.end();
    }
}

/// Why the Harness would reject a call into the Plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The Plugin declined, with this reason.
    Refused(String),
    /// The Plugin broke the contract in the way named.
    Misused(&'static str),
    /// The call returned this status without a refusal or a misuse.
    Status(RvStatus),
    /// `RV_PLUGIN_LOAD` returned `RV_OK` without describing the Plugin.
    Undescribed,
    /// `RV_TOOL_CALL` returned `RV_OK` without finishing the call.
    Unfinished,
}

/// The `RvHost` every call hands the Plugin.
static HOST: RvHost = RvHost {
    size: size_field::<RvHost>(),
    api_major: RV_API_MAJOR,
    api_minor: RV_API_MINOR,
    reserved: 0,
};

/// An `RvCall` for `verb` with this fake Host's `RvHost`, the Plugin
/// instance `plugin`, and nothing else set.
fn new_call(verb: u32, plugin: *mut c_void) -> RvCall {
    RvCall {
        size: size_field::<RvCall>(),
        verb,
        host: &raw const HOST,
        plugin,
        pack_run: ptr::null_mut(),
        args: ptr::null(),
        tool_call: ptr::null_mut(),
    }
}
