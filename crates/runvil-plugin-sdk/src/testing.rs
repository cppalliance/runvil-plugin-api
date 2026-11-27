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
//! A tool that reads or changes files works on a [`Files`] tree, which
//! [`PackRun::with_files`] gives the run. Every call of the run works on
//! the one tree, and [`PackRun::files`] lends it back, so a test reads
//! what a call left there:
//!
//! ```text
//! let files = Files::new().file("/notes/a.md", "one\ntwo\n");
//! let pack = loaded.new_pack("acme/notes").unwrap().with_files(files);
//! pack.call("save", r#"{"path":"/notes/b.md","text":"three\n"}"#).unwrap();
//! assert_eq!(pack.files().read("/notes/b.md", 0, u64::MAX), Ok(b"three\n".to_vec()));
//! ```
//!
//! The fake Host performs a file request on that tree when the step that
//! made it has returned, and lends the call's next step the answer. So
//! the change of a call that is dropped between those two steps has
//! happened.
//!
//! A step that returns `RV_PENDING` parks its call, as it does in the
//! Harness. [`PackRun::call`] sends the next step once the Plugin has
//! woken the call, or at once when the step made a file request, and
//! [`Call::wait`] is that wait for a test that sends each step itself. A
//! call that nothing wakes fails its test after 60 seconds, with a
//! sentence that says so, and does not hang it.
//!
//! A Pack that renders is tested with [`PackRun::render`], which sends
//! `RV_PACK_RENDER` with the tools a section offered and returns the tools
//! the model is shown. It checks each tool as `RV_PACK_NEW` checks one, and
//! then the render as a whole: a catalog tool keeps its offered
//! parameters, compared as text, which is stricter than the Harness; a
//! tool of the instance that the section did not offer is not listed; and
//! no two tools show one wire name. A broken rule is a
//! [`Failure::Rejected`] that names the tool. A model-facing tool that the
//! render listed is called with [`PackRun::call`] under that name.
//!
//! Every call is lent the Host's resources, the JSON text of an object.
//! It is `{}` unless [`PackRun::with_resources`] gives the run another:
//!
//! ```text
//! let resources = r#"{"acme/search":{"url":"http://127.0.0.1:9/search"}}"#;
//! let pack = loaded.new_pack("acme/url").unwrap().with_resources(resources);
//! ```
//!
//! A Plugin crate enables it from `[dev-dependencies]` only:
//!
//! ```text
//! [dev-dependencies]
//! runvil-plugin-sdk = { git = "...", tag = "...", features = ["test-support"] }
//! ```

mod call;
mod file;
#[cfg(test)]
mod fixture;
mod json;
mod load;
mod pack;
mod render;
mod session;
mod tree;
mod wake;

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::Duration;
use std::{mem, ptr};

use runvil_plugin_sys::{
    PluginEntry, RV_API_MAJOR, RV_API_MINOR, RV_OK, RV_PACK_DELETE, RV_PACK_NEW, RV_PENDING,
    RV_PLUGIN_LOAD, RV_PLUGIN_UNLOAD, RV_TOOL_CALL, RV_TOOL_CANCEL, RvCall, RvHost, RvLoadArgs,
    RvPackNewArgs, RvStatus, RvToolCallArgs,
};

use self::session::{Described, Session, Verb};
pub use self::tree::Files;
use self::wake::WakerCell;
use crate::entry::{rv_str, size_field};
use crate::tool::Resources;
use crate::{Plugin, ToolError, ToolInfo, ToolOutput, entry};

/// How a tool call ended: its output, or its failure.
pub type ToolResult = Result<ToolOutput, ToolError>;

/// How long [`Call::wait`] waits for a wake. A wake that is coming comes
/// far sooner in a test, so one that has not come by then is missing.
const WAIT_LIMIT: Duration = Duration::from_secs(60);

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
        wakers: RefCell::new(Vec::new()),
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
    /// What the waker of each tool call so far points at, kept until the
    /// Plugin is unloaded, as the contract requires of a Host. So a wake
    /// that comes after its call ended, from any thread, reaches live
    /// memory.
    wakers: RefCell<Vec<Arc<WakerCell>>>,
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
            pack_id: pack.split_once('/').map_or(pack, |(_, id)| id).to_owned(),
            instance: call.pack_run,
            tools: mem::take(&mut session.tools),
            rendered: RefCell::new(Vec::new()),
            files: Files::new(),
            resources: Resources::new("{}"),
        })
    }

    /// What the waker of one more tool call points at. This value keeps
    /// it until the Plugin is unloaded, however soon the call ends.
    fn new_waker(&self) -> Arc<WakerCell> {
        let cell = Arc::new(WakerCell::default());
        self.wakers.borrow_mut().push(Arc::clone(&cell));
        cell
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
    /// The Pack's id, the second segment of its name, which the tool ids
    /// and the default wire names of its tools start with.
    pack_id: String,
    /// What `RV_PACK_NEW` left in `call->pack_run`.
    instance: *mut c_void,
    tools: Vec<ToolInfo>,
    /// The names of the model-facing tools a render of this instance has
    /// listed, which a test may call as the model does.
    rendered: RefCell<Vec<String>>,
    /// The run's files, which the file requests of each call are
    /// performed on.
    files: Files,
    /// The Host's resources, which every step of each call is lent.
    resources: Resources,
}

impl PackRun<'_> {
    /// The tools the instance added as it was made, in order.
    #[must_use]
    pub fn tools(&self) -> &[ToolInfo] {
        &self.tools
    }

    /// The instance with `files` as the files of its run, in place of a
    /// tree with nothing but its root. Every call made from here on reads
    /// and changes them.
    #[must_use]
    pub fn with_files(mut self, files: Files) -> Self {
        self.files = files;
        self
    }

    /// The files of the run as they are, with every change its calls
    /// have made. A test reads them after a call, and may change them
    /// between two calls or two steps, as another task of a run does.
    #[must_use]
    pub fn files(&self) -> &Files {
        &self.files
    }

    /// The instance with `json` as the Host's resources for every call
    /// made from here on, in place of `{}`. A Host sends the JSON text of
    /// an object keyed by resource name, and each step of a call is lent
    /// the text as it is here.
    #[must_use]
    pub fn with_resources(mut self, json: impl Into<String>) -> Self {
        self.resources = Resources::new(json);
        self
    }

    /// Runs one call of `tool` with `arguments`, the JSON text of its
    /// arguments, to its end, as the Harness does: sends a step with
    /// [`Call::step`], and after each one that returns `RV_PENDING`
    /// waits with [`Call::wait`] until the call is due its next step.
    /// Returns the tool's own result.
    ///
    /// # Errors
    /// Returns the [`Failure`] the Harness would report for a call that
    /// breaks the contract; see [`Call::step`].
    ///
    /// # Panics
    /// Panics when the instance added no tool named `tool` and no render
    /// of it has listed one, which the Harness never calls, and when a
    /// step parks the call and the Plugin does not wake it within 60
    /// seconds; see [`Call::wait`].
    pub fn call(&self, tool: &str, arguments: &str) -> Result<ToolResult, Failure> {
        let mut call = self.start(tool, arguments);
        loop {
            if let Some(result) = call.step()? {
                return Ok(result);
            }
            call.wait();
        }
    }

    /// Starts one call of `tool` with `arguments`, the JSON text of its
    /// arguments, for a test that sends each step itself. Nothing is sent
    /// until the first [`Call::step`].
    ///
    /// # Panics
    /// Panics when the instance added no tool named `tool` and no render
    /// of it has listed one, which the Harness never calls. A model-facing
    /// tool that [`PackRun::render`] listed is called by the name it was
    /// listed under, as a model calls it.
    #[must_use]
    pub fn start(&self, tool: &str, arguments: &str) -> Call<'_> {
        assert!(
            self.tools.iter().any(|added| added.name == tool)
                || self.rendered.borrow().iter().any(|listed| listed == tool),
            "the Pack instance has no tool named {tool}"
        );
        Call {
            pack: self,
            tool: tool.to_owned(),
            arguments: arguments.to_owned(),
            state: ptr::null_mut(),
            pending: false,
            ended: false,
            answer: None,
            waker: self.loaded.new_waker(),
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
    /// The answer to the file request the last step made, which the next
    /// step is lent.
    answer: Option<file::Answer>,
    /// What the call's waker points at: whether the Plugin has woken the
    /// call since its last step began. The [`Loaded`] Plugin keeps it
    /// too, until the Plugin is unloaded.
    waker: Arc<WakerCell>,
}

impl Call<'_> {
    /// Sends one `RV_TOOL_CALL`. Returns `None` when the step returned
    /// `RV_PENDING`, which parks the call, and the tool's own result once
    /// the call ended. [`Call::wait`] blocks until a parked call is due
    /// its next step. This sends a step whether or not one is due,
    /// because the contract lets a Host send a step that no wake asked
    /// for.
    ///
    /// A file request the step made is performed on the run's [`Files`]
    /// before this returns, and the call's next step is lent the answer.
    /// A call that is dropped before that step leaves the operation done.
    ///
    /// The step is lent the run's resources and the call's waker, the
    /// same one at every step. A wake that came before the step is
    /// answered by it, and a wake from its beginning on asks for the step
    /// after it.
    ///
    /// # Errors
    /// Returns the [`Failure`] the Harness would report, checked in the
    /// Harness's order: a misuse, then a step that finished and still
    /// returned `RV_PENDING`, then a status other than `RV_OK`, then a
    /// step that returned `RV_OK` without finishing. A failure ends the
    /// call, and the file request of a step that failed is not performed.
    ///
    /// # Panics
    /// Panics when the call has ended, which the Harness never steps.
    pub fn step(&mut self) -> Result<Option<ToolResult>, Failure> {
        assert!(!self.ended, "the call has ended and takes no more steps");
        let mut call = RvCall {
            tool_call: self.state,
            ..self.pack.new_call(RV_TOOL_CALL)
        };
        // The answer and what it points to stay valid until this
        // function returns, which is after the step has.
        let answer = self.answer.take();
        let lent = answer.as_ref().map(file::Answer::raw);
        // This step answers every wake so far. The cell's lock is let go
        // before the Plugin runs, because a Plugin may wake the call on
        // this thread during the step.
        self.waker.clear();
        let (status, mut session) = session::open(Verb::ToolCall, |ctx| {
            let args = RvToolCallArgs {
                size: size_field::<RvToolCallArgs>(),
                reserved: 0,
                ctx,
                tool: rv_str(&self.tool),
                arguments: rv_str(&self.arguments),
                finish: call::finish,
                file_stat: file::file_stat,
                file_list: file::file_list,
                file_read: file::file_read,
                answer: lent.as_ref().map_or(ptr::null(), ptr::from_ref),
                file_write: file::file_write,
                file_append: file::file_append,
                file_mkdir: file::file_mkdir,
                file_remove: file::file_remove,
                file_rename: file::file_rename,
                file_copy: file::file_copy,
                resources: rv_str(self.pack.resources.text()),
                waker: WakerCell::waker(&self.waker),
            };
            call.args = (&raw const args).cast();
            // SAFETY: `load_entry`'s caller guaranteed `entry` follows the
            // contract, and `call`, `args`, and the answer `args` names
            // are live structs that nothing else uses.
            unsafe { (self.pack.loaded.entry)(&raw mut call) }
        });
        self.state = call.tool_call;
        self.pending = status == RV_PENDING;
        let request = session.request.take();
        let outcome = call::judge(status, &mut session);
        if matches!(outcome, Ok(None)) {
            self.answer = request.map(|request| file::Answer::to(&request, &self.pack.files));
        } else {
            self.end();
        }
        outcome
    }

    /// Blocks until the call is due its next step, as the Harness waits
    /// before it sends one. It returns at once in three cases:
    ///
    /// - No step was sent yet. A call's first step is due as soon as the
    ///   call starts, as [`Call::step`] sends it without a wake.
    /// - The last step made a file request. The fake Host has performed
    ///   it, so the answer is ready.
    /// - The Plugin has woken the call since its last step began.
    ///
    /// Otherwise the call is parked, and this blocks until the Plugin
    /// wakes it, from any thread.
    ///
    /// # Panics
    /// Panics when the call has ended, which is due no step, as
    /// [`Call::step`] panics for such a call. Panics when the call is
    /// parked and no wake comes within 60 seconds, with a sentence that
    /// says so, so a call that is never woken fails its test and does not
    /// hang it.
    pub fn wait(&self) {
        self.wait_for(WAIT_LIMIT);
    }

    /// [`Call::wait`], with `limit` as the time a wake may take.
    fn wait_for(&self, limit: Duration) {
        assert!(!self.ended, "the call has ended and waits for no step");
        // A call that has not ended and is not pending was sent no step
        // yet, and an answer is here only after a step made a request.
        if !self.pending || self.answer.is_some() {
            return;
        }
        assert!(
            self.waker.wait(limit),
            "the call of the tool {} was not woken within {limit:?}: its last step returned \
             RV_PENDING with no file request, so its next step is due only after the Plugin \
             wakes the call",
            self.tool
        );
    }

    /// Whether the Plugin has woken the call since its last step began,
    /// for this crate's own tests, which check a wake without waiting
    /// for one.
    #[cfg(test)]
    pub(crate) fn is_woken(&self) -> bool {
        self.waker.wait(Duration::ZERO)
    }

    /// Ends the call: sends `RV_TOOL_CANCEL` when its last step returned
    /// `RV_PENDING`, so the Plugin frees the call's state. No lock is held
    /// meanwhile, because a Plugin may wake the call as it frees that
    /// state.
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
    /// `RV_PACK_RENDER` returned `RV_OK`, and the Harness rejects what the
    /// render listed, for the reason given, which names the tool.
    Rejected(String),
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
