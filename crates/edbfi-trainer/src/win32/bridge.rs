// SPDX-License-Identifier: AGPL-3.0-only
//! The process-wide bridge between asynchronous shutdown notices and the owner thread.
//!
//! Windows tells a console process about a closing window, logoff or shutdown on a thread of its own, and
//! kills the process soon after. A panic can happen on any thread. None of these can reach the session the
//! owner thread holds, so the bridge keeps what they need: an event that asks the owner to restore and exit,
//! and a slot holding the current session's lease on its control word.
//!
//! A lease pins the trainer's view of the control word and an event the session sets once its restoration
//! has finished. A callback counts itself as a reader before loading the slot, zeroes the control word
//! through the lease it found, and stops reading when done. A session retiring its lease empties the slot,
//! then waits for the reader count to drop to zero before unmapping, so no callback ever touches an unmapped
//! view. All of it is lock-free and allocation-free on the callback side.

use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::ptr::{self, NonNull};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use windows_sys::Win32::Foundation::{FALSE, HANDLE, TRUE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_sys::core::BOOL;

use crate::patch::OsError;

use super::process::none;
use super::{last_error, millis, owned};

/// How long a close, logoff or shutdown notice waits for restoration. Windows allows about five seconds
/// before it ends the process.
pub(crate) const CLOSE_BUDGET: Duration = Duration::from_millis(4500);

/// A session's control word and completion event, pinned while any callback may use them.
pub(crate) struct Lease {
    word: NonNull<AtomicU32>,
    completion: OwnedHandle,
}

// SAFETY: `word` points into a mapped view that stays mapped until the lease is retired, which waits for
// every reader; the view is only accessed through atomics, so sharing the pointer between threads is sound.
unsafe impl Send for Lease {}
// SAFETY: as for `Send`; every access through `word` is atomic and the event handle is thread-safe.
unsafe impl Sync for Lease {}

impl Lease {
    pub(crate) const fn new(word: NonNull<AtomicU32>, completion: OwnedHandle) -> Self {
        Self { word, completion }
    }

    /// The event set when the session's restoration has finished.
    pub(crate) fn completion(&self) -> HANDLE {
        self.completion.as_raw_handle()
    }

    fn zero(&self) {
        // SAFETY: a reader only reaches a lease through the slot, and the view stays mapped until every
        // reader has left (see `retire`).
        unsafe { self.word.as_ref() }.store(0, Ordering::SeqCst);
    }

    fn wait(&self, timeout: Duration) {
        // SAFETY: the completion event lives as long as the lease.
        let _signalled = unsafe { WaitForSingleObject(self.completion(), millis(timeout)) };
    }
}

/// The bridge. One per process, created by [`install`].
pub(crate) struct Bridge {
    shutdown: OwnedHandle,
    slot: AtomicPtr<Lease>,
    readers: AtomicUsize,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();
static INSTALL: Mutex<()> = Mutex::new(());

impl Bridge {
    /// Asks the owner to restore and exit.
    pub(crate) fn request(&self) {
        // SAFETY: the shutdown event lives for the whole process.
        let _set = unsafe { SetEvent(self.shutdown.as_raw_handle()) };
    }

    /// Whether shutdown was requested.
    pub(crate) fn requested(&self) -> bool {
        self.wait(Duration::ZERO)
    }

    /// Waits up to `timeout` for a shutdown request.
    pub(crate) fn wait(&self, timeout: Duration) -> bool {
        // SAFETY: as in `request`.
        unsafe {
            WaitForSingleObject(self.shutdown.as_raw_handle(), millis(timeout)) == WAIT_OBJECT_0
        }
    }

    /// The shutdown event, for a wait on several handles.
    pub(crate) fn borrowed(&'static self) -> std::os::windows::io::BorrowedHandle<'static> {
        use std::os::windows::io::AsHandle as _;
        self.shutdown.as_handle()
    }

    /// Runs `with` on the current lease, if any, as a counted reader.
    fn read(&self, with: impl FnOnce(&Lease)) {
        self.readers.fetch_add(1, Ordering::SeqCst);
        let lease = self.slot.load(Ordering::SeqCst);
        // SAFETY: a non-null slot points at a lease that `retire` will not free while this reader is counted.
        if let Some(lease) = unsafe { lease.as_ref() } {
            with(lease);
        }
        self.readers.fetch_sub(1, Ordering::SeqCst);
    }

    /// Zeroes the control word now. Safe from a panic hook: no allocation, lock or I/O.
    pub(crate) fn emergency_zero(&self) {
        self.read(Lease::zero);
    }

    /// A closing window, logoff or shutdown: zero the controls, ask the owner to restore, and wait for it
    /// within the operating system's budget.
    pub(crate) fn ending(&self) {
        self.read(|lease| {
            lease.zero();
            self.request();
            lease.wait(CLOSE_BUDGET);
        });
        self.request();
    }
}

/// The bridge, if installed.
pub(crate) fn get() -> Option<&'static Bridge> {
    BRIDGE.get()
}

/// Puts `lease` in the bridge's slot, if a bridge is installed and no other session holds it.
pub(crate) fn publish(lease: &Lease) -> bool {
    get().is_some_and(|bridge| {
        bridge
            .slot
            .compare_exchange(
                ptr::null_mut(),
                ptr::from_ref(lease).cast_mut(),
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    })
}

/// Takes `lease` out of the slot and waits until no callback is using it.
pub(crate) fn retire(lease: &Lease) {
    if let Some(bridge) = get() {
        let _taken = bridge.slot.compare_exchange(
            ptr::from_ref(lease).cast_mut(),
            ptr::null_mut(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        while bridge.readers.load(Ordering::SeqCst) != 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// The console control handler. Ctrl+C and Ctrl+Break ask for a normal exit; a closing window, logoff or
/// shutdown also zero the controls and wait for restoration, since Windows ends the process when this
/// returns.
unsafe extern "system" fn console(kind: u32) -> BOOL {
    let handled = std::panic::catch_unwind(|| {
        let Some(bridge) = get() else {
            return FALSE;
        };
        match kind {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                bridge.request();
                TRUE
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                bridge.ending();
                TRUE
            }
            _ => FALSE,
        }
    });
    handled.unwrap_or_else(|_| {
        if let Some(bridge) = get() {
            bridge.request();
        }
        TRUE
    })
}

/// Clears a shutdown request left by an earlier test.
#[cfg(test)]
pub(crate) fn reset_for_tests() {
    if let Some(bridge) = get() {
        // SAFETY: the shutdown event lives for the whole process.
        let _reset = unsafe {
            windows_sys::Win32::System::Threading::ResetEvent(bridge.shutdown.as_raw_handle())
        };
    }
}

/// Runs the console handler as Windows would, for tests.
#[cfg(test)]
pub(crate) fn console_notice(kind: u32) -> bool {
    // SAFETY: the handler is an ordinary function; calling it directly is what Windows does on its thread.
    (unsafe { console(kind) }) != FALSE
}

/// Which parts of the bridge are in place; an install that failed part-way retries only the rest.
static HANDLER: AtomicBool = AtomicBool::new(false);
static HOOK: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicBool = AtomicBool::new(false);

/// Installs the bridge once per process: the shutdown event, the console handler, a panic hook that zeroes
/// the controls, and the hidden window that hears about session end. A failed part is tried again by the
/// next call; only a complete install returns the bridge.
pub(crate) fn install() -> Result<&'static Bridge, OsError> {
    let _once = INSTALL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let bridge = match get() {
        Some(bridge) => bridge,
        None => {
            // SAFETY: a manual-reset, unsignalled, unnamed event takes no pointers but the null ones.
            let raw = unsafe { CreateEventW(none(), TRUE, FALSE, none()) };
            let shutdown = owned(raw, "CreateEventW")?;
            BRIDGE.get_or_init(|| Bridge {
                shutdown,
                slot: AtomicPtr::new(ptr::null_mut()),
                readers: AtomicUsize::new(0),
            })
        }
    };
    if !HANDLER.load(Ordering::SeqCst) {
        // SAFETY: `console` is a valid handler for the life of the process.
        if unsafe { SetConsoleCtrlHandler(Some(console), TRUE) } == FALSE {
            return Err(last_error("SetConsoleCtrlHandler"));
        }
        HANDLER.store(true, Ordering::SeqCst);
    }
    if !HOOK.swap(true, Ordering::SeqCst) {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if let Some(bridge) = get() {
                bridge.emergency_zero();
            }
            // While a game is frozen the hook does nothing else: the previous hook may allocate, format and
            // lock, and the patch code catching this panic must roll back and resume the game first.
            if !crate::patch::freeze::holding() {
                previous(info);
            }
        }));
    }
    if !WINDOW.load(Ordering::SeqCst) {
        super::window::start(bridge)?;
        WINDOW.store(true, Ordering::SeqCst);
    }
    Ok(bridge)
}
