//! `runvil-plugin-sdk` - write a Plugin in safe Rust. Implement [`Plugin`]
//! for the type that holds the Plugin's state, then give the Harness its
//! entry point: a library Plugin exports it with [`export_plugin!`], and a
//! Host that links the Plugin in hands [`entry::<P>`](entry) to the
//! Harness directly.
//!
//! ```text
//! use runvil_plugin_sdk::{Plugin, Refusal, export_plugin};
//!
//! #[derive(Debug)]
//! pub struct Hello;
//!
//! impl Plugin for Hello {
//!     const NAME: &'static str = "hello";
//!     const VERSION: &'static str = env!("CARGO_PKG_VERSION");
//!
//!     fn load() -> Result<Hello, Refusal> {
//!         Ok(Hello)
//!     }
//! }
//!
//! export_plugin!(Hello);
//! ```
//!
//! The entry point speaks the C contract in [`sys`] and keeps its rules
//! for the Plugin: it checks the Host's version, describes the Plugin
//! exactly once, frees the instance on unload, and never lets a panic
//! unwind into the Host.
//!
//! The `test-support` feature adds `testing`, a fake Host for a Plugin
//! crate's own tests.
//!
//! ## Invariants
//!
//! - Depends on `runvil-plugin-sys` alone, which it re-exports as [`sys`].
//! - The crate's `unsafe` sits in [`entry`], the code [`export_plugin!`]
//!   expands to, and `testing`. A Plugin written against it needs none.
//! - A panic in [`Plugin::load`] becomes a refusal, and a panic while the
//!   instance drops stays inside the Plugin; neither unwinds across the C
//!   boundary.
//! - The instance is made on the loading thread and may be dropped on any
//!   thread, so [`Plugin`] requires `Send`.

#![expect(
    unsafe_code,
    reason = "the C entry point reads the contract's raw pointers"
)]

mod entry;
mod names;
#[cfg(any(test, feature = "test-support"))]
pub mod testing;

pub use entry::entry;
pub use runvil_plugin_sys as sys;

/// A Plugin: the instance the Host holds while the Plugin is loaded.
///
/// The Host loads a Plugin once, through [`Plugin::load`], and unloads it
/// by dropping the instance. The drop may come on any thread, and it must
/// stop every thread the Plugin started and release everything the
/// Plugin holds before it returns: a library Plugin's library closes
/// right after, and a thread still running crashes the process.
pub trait Plugin: Send + Sized + 'static {
    /// The name the Plugin reports: 1 to 64 bytes of lowercase ASCII
    /// letters, digits, `-`, `_`, and `.`, such as `acme-url`. A name the
    /// Host would reject fails the build of [`entry::<Self>`](entry).
    const NAME: &'static str;

    /// The version the Plugin reports: 1 to 64 bytes with no control
    /// characters, such as `1.4.0`. A version the Host would reject fails
    /// the build of [`entry::<Self>`](entry).
    const VERSION: &'static str;

    /// Makes the instance, once, on the thread the Host loads the Plugin
    /// on.
    ///
    /// # Errors
    /// Returns a [`Refusal`] to decline the load; the Host reports its
    /// reason and never unloads the Plugin.
    fn load() -> Result<Self, Refusal>;
}

/// A declined load and its reason, which the Host reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    reason: String,
}

impl Refusal {
    /// The contract's bound on a reason, in bytes.
    pub const MAX_REASON: usize = 1024;

    /// A refusal with `reason`, cut to [`Refusal::MAX_REASON`] bytes at a
    /// character boundary, because the Host rejects a longer reason as a
    /// misuse.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Refusal {
        let mut reason = reason.into();
        let mut end = reason.len().min(Refusal::MAX_REASON);
        while !reason.is_char_boundary(end) {
            end -= 1;
        }
        reason.truncate(end);
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
