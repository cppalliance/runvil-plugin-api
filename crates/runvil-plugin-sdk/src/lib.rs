//! `runvil-plugin-sdk` - write a Plugin in safe Rust. Implement [`Plugin`]
//! for the type that holds the Plugin's state and [`Pack`] for each Pack
//! it offers, then give the Harness the Plugin's entry point: a library
//! Plugin exports it with [`export_plugin!`], and a Host that links the
//! Plugin in hands [`entry::<P>`](entry) to the Harness directly.
//!
//! ```text
//! use runvil_plugin_sdk::{
//!     BoxFuture, Pack, PackInfo, Plugin, Refusal, ToolCall, ToolError, ToolInfo, ToolOutput,
//!     export_plugin,
//! };
//!
//! #[derive(Debug)]
//! pub struct Hello;
//!
//! impl Plugin for Hello {
//!     const NAME: &'static str = "hello";
//!     const VERSION: &'static str = env!("CARGO_PKG_VERSION");
//!     const PACKS: &'static [PackInfo] = &[PackInfo::new("acme/hello")];
//!
//!     fn load() -> Result<Hello, Refusal> {
//!         Ok(Hello)
//!     }
//!
//!     fn new_pack(&self, _pack: &str) -> Result<Box<dyn Pack>, Refusal> {
//!         Ok(Box::new(Greeter))
//!     }
//! }
//!
//! struct Greeter;
//!
//! impl Pack for Greeter {
//!     fn tools(&self) -> Vec<ToolInfo> {
//!         vec![ToolInfo::new(
//!             "greet",
//!             "Returns a greeting.",
//!             r#"{"type":"object","properties":{}}"#,
//!         )]
//!     }
//!
//!     fn call<'a>(&'a self, _call: ToolCall<'a>) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
//!         Box::pin(async { Ok(ToolOutput::trusted("Hello.")) })
//!     }
//! }
//!
//! export_plugin!(Hello);
//! ```
//!
//! The entry point speaks the C contract in [`sys`] and keeps its rules
//! for the Plugin: it checks the Host's version, describes the Plugin
//! exactly once, inserts its Packs, frees every instance and every call's
//! state on the path the contract names, and never lets a panic unwind
//! into the Host.
//!
//! A tool reads the files of its run through its [`ToolCall`]:
//! [`stat`](ToolCall::stat), [`list`](ToolCall::list),
//! [`read`](ToolCall::read), and [`read_range`](ToolCall::read_range).
//! Each goes through the Host, so the run's policy applies, and each
//! fails with a [`FileError`] that leaves the call running.
//!
//! The `test-support` feature adds `testing`, a fake Host for a Plugin
//! crate's own tests.
//!
//! ## Invariants
//!
//! - Depends on `runvil-plugin-sys` alone, which it re-exports as [`sys`].
//!   JSON crosses as text, which a Plugin parses with a library of its
//!   own.
//! - The crate's `unsafe` sits in [`entry`] and its child modules, the
//!   code [`export_plugin!`] expands to, and `testing`. A Plugin written
//!   against it needs none.
//! - What the Host lends through a file function is copied before the
//!   [`ToolCall`] method returns, so nothing a Plugin holds points into
//!   the Host's memory.
//! - A panic in [`Plugin::load`] or [`Plugin::new_pack`] becomes a
//!   refusal, a panic in a tool call ends that call as a failure, and a
//!   panic while something the Plugin owns drops stays inside the Plugin;
//!   none unwinds across the C boundary.
//! - The Plugin instance is made on the loading thread, may be dropped on
//!   any thread, and makes Pack instances on any thread, so [`Plugin`]
//!   requires `Send + Sync`. Calls of one Pack instance may run at the
//!   same time, so [`Pack`] requires them too.

#![expect(
    unsafe_code,
    reason = "the C entry point reads the contract's raw pointers"
)]

mod entry;
mod file;
mod names;
mod pack;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;
mod tool;

pub use entry::entry;
pub use file::{DirEntry, FileError, FileErrorKind, FileType, Stat};
pub use pack::{BoxFuture, Pack, PackInfo};
pub use runvil_plugin_sys as sys;
pub use tool::{NextStep, ToolCall, ToolError, ToolErrorKind, ToolInfo, ToolOutput, next_step};

/// A Plugin: the instance the Host holds while the Plugin is loaded, and
/// the maker of each run's Pack instances.
///
/// The Host loads a Plugin once, through [`Plugin::load`], and unloads it
/// by dropping the instance, after every Pack instance it made is gone.
/// The drop may come on any thread, and it must stop every thread the
/// Plugin started and release everything the Plugin holds before it
/// returns: a library Plugin's library closes right after, and a thread
/// still running crashes the process.
pub trait Plugin: Send + Sync + Sized + 'static {
    /// The name the Plugin reports: 1 to 64 bytes of lowercase ASCII
    /// letters, digits, `-`, `_`, and `.`, such as `acme-url`. A name the
    /// Host would reject fails the build of [`entry::<Self>`](entry).
    const NAME: &'static str;

    /// The version the Plugin reports: 1 to 64 bytes with no control
    /// characters, such as `1.4.0`. A version the Host would reject fails
    /// the build of [`entry::<Self>`](entry).
    const VERSION: &'static str;

    /// The Packs the Plugin inserts as it loads, none unless it names
    /// some. Two Packs with one name, or a name or description the Host
    /// would reject, fail the build of [`entry::<Self>`](entry).
    const PACKS: &'static [PackInfo] = &[];

    /// Makes the instance, once, on the thread the Host loads the Plugin
    /// on. What every run shares, such as a connection pool, belongs in
    /// it.
    ///
    /// # Errors
    /// Returns a [`Refusal`] to decline the load; the Host reports its
    /// reason and never unloads the Plugin.
    fn load() -> Result<Self, Refusal>;

    /// Makes one run's instance of the Pack named `pack`, one of
    /// [`Plugin::PACKS`]. The Host calls it while it prepares a run, on
    /// any thread, so it returns promptly: it does not block and does no
    /// I/O. A Plugin that names Packs implements it; the default refuses.
    ///
    /// # Errors
    /// Returns a [`Refusal`], whose reason a model can read, when the run
    /// goes without the Pack.
    fn new_pack(&self, pack: &str) -> Result<Box<dyn Pack>, Refusal> {
        Err(Refusal::new(format!(
            "the Plugin {} makes no instance of the Pack {pack}",
            Self::NAME
        )))
    }
}

/// A declined load, or a declined Pack instance, and its reason, which
/// the Host reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    reason: String,
}

impl Refusal {
    /// The contract's bound on a reason, in bytes.
    pub const MAX_REASON: usize = runvil_plugin_sys::RV_MAX_LINE;

    /// A refusal with `reason`, cut to [`Refusal::MAX_REASON`] bytes at a
    /// character boundary, because the Host rejects a longer reason as a
    /// misuse.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Refusal {
        let mut reason = reason.into();
        let kept = names::cut(&reason, Refusal::MAX_REASON).len();
        reason.truncate(kept);
        Refusal { reason }
    }

    /// The reason the Host reports.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Exports the entry point of the Plugin type `$plugin` as
/// `runvil_plugin`, the one symbol the Harness resolves in a library
/// Plugin.
///
/// Use it once, at the root of a crate built as a `cdylib`. A Plugin that
/// is also linked into a Host puts it behind a Cargo feature of its own,
/// because two linked Plugins exporting `runvil_plugin` cannot share one
/// binary; the Host hands [`entry::<P>`](entry) to the Harness instead.
#[macro_export]
macro_rules! export_plugin {
    ($plugin:ty) => {
        /// The entry point the Harness resolves in this library Plugin.
        ///
        /// # Safety
        /// `call` must follow the C contract in `runvil_plugin.h`.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn runvil_plugin(
            call: *mut $crate::sys::RvCall,
        ) -> $crate::sys::RvStatus {
            // SAFETY: the caller follows the C contract, which is all
            // `entry` requires.
            unsafe { $crate::entry::<$plugin>(call) }
        }
    };
}

#[cfg(test)]
mod tests {
    use super::Refusal;

    #[test]
    fn a_short_reason_is_kept_whole() {
        assert_eq!(
            Refusal::new("no key configured").reason(),
            "no key configured"
        );
    }

    #[test]
    fn a_long_reason_is_cut_at_a_character_boundary() {
        // 'é' is two bytes, so byte 1024 falls inside one.
        let reason = format!("a{}", "é".repeat(600));
        let refusal = Refusal::new(reason);
        assert_eq!(refusal.reason().len(), 1023);
        assert!(refusal.reason().ends_with('é'));
    }
}
