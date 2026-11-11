//! What a Plugin offers: the [`PackInfo`] that names each of its Packs,
//! and the [`Pack`] trait one run's instance of a Pack implements.

use std::future::Future;
use std::pin::Pin;

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
    /// polls it once, and a `Pending` ends the step. The Host then sends
    /// the next step without being woken, so the future never waits on a
    /// waker; a long job awaits [`next_step`](crate::next_step) every so
    /// often instead. A step that works for long, or blocks, stalls its
    /// run, and the Host can abandon a call only between steps, which
    /// drops the future.
    ///
    /// A panic in the future ends the call as a failure and never unwinds
    /// into the Host.
    ///
    /// # Errors
    /// Returns a [`ToolError`], whose message a model reads, when the call
    /// rejects its arguments or fails.
    fn call<'a>(&'a self, call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>>;
}

/// One run's instance of a Pack as `call->pack_run` holds it: a thin
/// pointer to this is what crosses the C boundary.
pub(crate) struct PackRun {
    pub(crate) pack: Box<dyn Pack>,
}
