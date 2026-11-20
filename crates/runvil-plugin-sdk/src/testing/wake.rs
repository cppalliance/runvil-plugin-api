//! The waker a fake Host hands every step of one tool call: the
//! [`WakerCell`] its `ctx` points at, and the `wake` that notes a wake
//! there.

use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use runvil_plugin_sys::RvWaker;

/// What the waker of one tool call points at: whether the Plugin has
/// woken the call since its last step began, and a way to block until it
/// does.
///
/// A Plugin may call `wake` from a destructor on the fake Host's own
/// thread, inside a step or inside `RV_TOOL_CANCEL`. So the lock here is
/// never held across a call into the Plugin: each method takes it and
/// lets go of it before it returns.
#[derive(Debug, Default)]
pub(super) struct WakerCell {
    /// Whether a wake has come since the call's last step began.
    woken: Mutex<bool>,
    /// Notified at each wake, for the thread that waits for one.
    wakes: Condvar,
}

impl WakerCell {
    /// The contract's waker over `cell`. Its `ctx` points at the cell, so
    /// the fake Host keeps the cell until the Plugin is unloaded.
    pub(super) fn waker(cell: &Arc<WakerCell>) -> RvWaker {
        RvWaker {
            ctx: Arc::as_ptr(cell).cast_mut().cast(),
            wake,
        }
    }

    /// The flag, locked. It is plain data that no panic leaves half
    /// written, so a poisoned lock is opened all the same.
    fn lock(&self) -> MutexGuard<'_, bool> {
        self.woken.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Forgets every wake so far, as a step begins: that step is the one
    /// they asked for, and a wake from here on asks for the step after
    /// it.
    pub(super) fn clear(&self) {
        *self.lock() = false;
    }

    /// Notes a wake, and lets the thread that waits for one go on.
    fn note(&self) {
        *self.lock() = true;
        self.wakes.notify_all();
    }

    /// Blocks until a wake has come since the last
    /// [`clear`](WakerCell::clear), for at most `limit`. Returns whether
    /// one has come.
    pub(super) fn wait(&self, limit: Duration) -> bool {
        let (woken, _timed_out) = self
            .wakes
            .wait_timeout_while(self.lock(), limit, |woken| !*woken)
            .unwrap_or_else(PoisonError::into_inner);
        *woken
    }
}

/// The `wake` of every waker a fake Host hands out. Any thread may call
/// it, any number of times, until the Plugin is unloaded. It only notes
/// the wake in the cell behind `ctx`, and calls nothing of the Plugin's.
/// It never unwinds into the Plugin, because nothing in it can panic: it
/// takes the cell's lock, which no thread holds across a call into the
/// Plugin, sets the flag, and notifies. A NULL `ctx` names no cell and
/// wakes nothing.
///
/// # Safety
/// `ctx` is NULL, or the `ctx` of a waker a fake Host handed out for a
/// Plugin that is not unloaded yet.
unsafe extern "C" fn wake(ctx: *mut c_void) {
    // SAFETY: the caller passes NULL or the `ctx` of a waker made by
    // `WakerCell::waker`, which points at a cell the `Loaded` Plugin
    // keeps until it is unloaded.
    if let Some(cell) = unsafe { ctx.cast::<WakerCell>().as_ref() } {
        cell.note();
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use runvil_plugin_sys::RvWaker;

    use super::{WakerCell, wake};

    /// Wakes through `waker`, as a Plugin does.
    fn wake_through(waker: RvWaker) {
        // SAFETY: `waker` is over a cell the test keeps alive.
        unsafe { (waker.wake)(waker.ctx) };
    }

    #[test]
    fn a_cell_is_woken_by_its_waker_until_a_step_begins() {
        let cell = Arc::new(WakerCell::default());
        let waker = WakerCell::waker(&cell);
        assert!(!cell.wait(Duration::ZERO), "nothing woke it yet");
        wake_through(waker);
        wake_through(waker);
        assert!(cell.wait(Duration::ZERO));
        assert!(cell.wait(Duration::ZERO), "a wait takes no wake away");
        cell.clear();
        assert!(!cell.wait(Duration::ZERO), "two wakes asked for one step");
    }

    #[test]
    fn a_wait_blocks_until_another_thread_wakes_the_cell() {
        let cell = Arc::new(WakerCell::default());
        let waker = WakerCell::waker(&cell);
        let released = Arc::new(AtomicBool::new(false));
        // The address crosses to the thread, as a raw pointer cannot.
        let ctx = waker.ctx.expose_provenance();
        let waking = {
            let released = Arc::clone(&released);
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(50));
                released.store(true, Ordering::SeqCst);
                // SAFETY: the `ctx` of a cell the test keeps until this
                // thread is joined.
                unsafe { wake(ptr::with_exposed_provenance_mut(ctx)) };
            })
        };
        let limit = Duration::from_secs(60);
        let started = Instant::now();
        assert!(cell.wait(limit));
        assert!(
            released.load(Ordering::SeqCst),
            "the wait ended before the wake"
        );
        assert!(
            started.elapsed() < limit,
            "the wait ended at its limit and not at the wake"
        );
        waking.join().unwrap();
    }

    #[test]
    fn a_wait_gives_up_at_its_limit_when_no_wake_comes() {
        let cell = Arc::new(WakerCell::default());
        assert!(!cell.wait(Duration::from_millis(10)));
    }

    #[test]
    fn a_wake_through_a_null_ctx_reaches_no_cell() {
        // SAFETY: `wake` takes NULL.
        unsafe { wake(ptr::null_mut()) };
    }
}
