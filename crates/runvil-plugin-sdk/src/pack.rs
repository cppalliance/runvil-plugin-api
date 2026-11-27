//! What a Plugin offers: the [`PackInfo`] that names each of its Packs,
//! and the [`Pack`] trait one run's instance of a Pack implements.

use std::future::Future;
use std::pin::Pin;

use crate::Refusal;
use crate::tool::{ToolCall, ToolError, ToolInfo, ToolOutput};

/// A boxed future that is `Send`, which [`Pack::call`] returns. An
/// implementation wraps its body in `Box::pin(async move { ... })`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A Pack described as data, which a Plugin lists in
/// [`Plugin::PACKS`](crate::Plugin::PACKS).
///
/// ```text
/// const PACKS: &'static [PackInfo] =
///     &[PackInfo::new("acme/url").description("Fetch and search the web")];
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PackInfo {
    /// The Pack's name: a vendor and a Pack id joined by `/`, such as
    /// `acme/url`, each 1 to 64 bytes of lowercase ASCII letters, digits,
    /// `-`, `_`, and `.`. The Pack id prefixes the id of every tool the
    /// Pack offers, as in `url/fetch`.
    pub name: &'static str,
    /// One line saying what the Pack offers: at most 1024 bytes with no
    /// control characters. Empty unless set.
    pub description: &'static str,
}

impl PackInfo {
    /// Names the Pack `name`, with an empty description.
    #[must_use]
    pub const fn new(name: &'static str) -> PackInfo {
        PackInfo {
            name,
            description: "",
        }
    }

    /// Sets the one line saying what the Pack offers.
    #[must_use]
    pub const fn description(self, description: &'static str) -> PackInfo {
        PackInfo {
            description,
            ..self
        }
    }
}

/// One run's instance of a Pack: the tools it offers and the calls it
/// performs.
///
/// The Host makes an instance for each run that holds the Pack, through
/// [`Plugin::new_pack`](crate::Plugin::new_pack), and drops it when the
/// run ends, after every call on it has ended. Cleanup goes in the
/// implementation's `Drop`. Calls of one instance may run at the same
/// time on different threads, so an implementation is `Send + Sync`.
pub trait Pack: Send + Sync + 'static {
    /// The tools this instance offers its run. The Host reads the list
    /// once, right after the instance is made, and it stays fixed for the
    /// run.
    fn tools(&self) -> Vec<ToolInfo>;

    /// Performs one call of the tool `call` names.
    ///
    /// The Host runs the returned future one step at a time: each step
    /// polls it once, and a `Pending` ends the step and parks the call.
    /// The Host sends the next step when the future's waker is woken, so
    /// the future may wait on a waker like any Rust future: on a timer, a
    /// channel, or a task on a runtime the Plugin owns, each of which
    /// wakes it from a thread of the Plugin. A future that returns
    /// `Pending` without arranging a wake, and without a file request,
    /// parks for good.
    ///
    /// A step that works for long, or blocks, stalls its run, and the
    /// Host can abandon a call only between steps, which drops the
    /// future. So a long job awaits [`next_step`](crate::next_step) every
    /// so often, and a call that waits on something outside its run
    /// awaits it and blocks no step for it.
    ///
    /// The future may hand its waker to a thread of the Plugin, which may
    /// hold it past the call's end, because the Host keeps the waker
    /// valid until the Plugin is unloaded: a wake that comes late does
    /// nothing for the call. Dropping the Plugin instance stops every
    /// thread that holds one, as it stops every thread of the Plugin.
    ///
    /// The future reaches the run's files through the file methods of
    /// `call`. Awaiting one ends the step with a request to the Host, and
    /// the Host sends the next step when the answer is ready. An answer
    /// comes only with a step, so a file method is awaited inside the
    /// returned future. This function returns before that future is first
    /// polled, so it can make the future of a file method and cannot have
    /// its answer. A file future gets its answer only when a step polls
    /// it, so it does not work inside a set that polls a child only after
    /// that child's waker fired, such as `FuturesUnordered`.
    ///
    /// A panic in the future ends the call as a failure and never unwinds
    /// into the Host.
    ///
    /// # Errors
    /// Returns a [`ToolError`], whose message a model reads, when the call
    /// rejects its arguments or fails.
    fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>>;

    /// Lists the tools a model is shown, given the tools a section
    /// offered from this Pack. The Host calls it when a section entry's
    /// first model round is about to advertise its tools, once for each of
    /// the entry's two sets of tools that holds a tool of this Pack, with
    /// the same [`RenderContext`] each time. What it lists stays fixed for
    /// that section entry. The default lists `offered` as it came.
    ///
    /// `offered` holds the catalog tools of this Pack the section offered
    /// in this set, as [`Pack::tools`] listed them. The answer decides
    /// what the model sees:
    ///
    /// - A tool left out of the answer is not shown.
    /// - A tool listed under the `name` of an offered tool is that catalog
    ///   tool. It may take another `wire_name` and `description`, and its
    ///   `parameters` stay the offered text, so one catalog name always
    ///   means one parameter schema. Lua can call it by its id, whatever
    ///   the model is shown.
    /// - A tool listed under a name that no tool of this instance's catalog
    ///   has is a model-facing tool: only a model calls it, and no script
    ///   can name it. It is how a model is shown other parameters than the
    ///   catalog's, under a name of its own, and [`Pack::call`] answers its
    ///   calls by that name. It takes the flags it is given, so a
    ///   model-facing tool that changes files says so with
    ///   [`ToolInfo::changes_files`].
    /// - A catalog tool the section did not offer may not be listed, under
    ///   its own name or any other that the catalog has: the Host fails
    ///   the round that asked for the render.
    ///
    /// It returns promptly: it does not block and does no I/O. It may
    /// run on any thread and at the same time as calls of this instance.
    /// A panic in it ends the render as a refusal and never unwinds into
    /// the Host.
    ///
    /// # Errors
    /// Returns a [`Refusal`], whose reason a model can read, when the Pack
    /// cannot render for this model. The round that asked for the render
    /// then fails.
    fn render(
        &self,
        cx: &RenderContext<'_>,
        offered: Vec<ToolInfo>,
    ) -> Result<Vec<ToolInfo>, Refusal> {
        let _ = cx;
        Ok(offered)
    }
}

/// What a [`Pack`] learns about the section entry it renders for, in
/// [`Pack::render`].
///
/// A section entry is one execution of a section. The entry point builds
/// the context from what the Host lends for the call, and a Pack keeps
/// none of it past the call. A later version adds facts as new fields, so
/// a Pack reads the fields it names and matches no other way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderContext<'a> {
    /// The section entry, the value a prompt's Lua reads as `sys.id`.
    pub entry: &'a str,
    /// The JSON text of an object that describes the model of the entry's
    /// first round: `server`, `name`, `description`, `context` (the window
    /// in tokens), `thinking` (`"never"`, `"always"`, or `"switchable"`),
    /// and `provider` when the Host's catalog names one. The SDK needs no
    /// JSON library, so a Pack parses it with one of its own. A later
    /// version adds a fact as a new key, so a Pack ignores a key it does
    /// not know.
    pub model: &'a str,
}

impl<'a> RenderContext<'a> {
    /// A context for the section entry `entry` and the model `model`, for
    /// a Pack author who calls [`Pack::render`] in a test without a Host.
    #[must_use]
    pub fn new(entry: &'a str, model: &'a str) -> RenderContext<'a> {
        RenderContext { entry, model }
    }
}

/// One run's instance of a Pack as `call->pack_run` holds it: a thin
/// pointer to this is what crosses the C boundary.
pub(crate) struct PackRun {
    pub(crate) pack: Box<dyn Pack>,
}
