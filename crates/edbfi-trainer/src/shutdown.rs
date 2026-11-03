// SPDX-License-Identifier: AGPL-3.0-only
//! Hearing about a closing console window, logoff, shutdown or panic, so the game is restored first.
//!
//! [`install`] sets up, once per process, a console control handler, a panic hook and a hidden window that
//! receives the session-end messages. Each of them zeroes the control word of the current session at once,
//! then asks the owner thread to restore and exit through one event, and the close, logoff and shutdown
//! notices wait a bounded time for the session to finish restoring. The owner waits on [`Shutdown::event`]
//! with its other handles, restores, and lets its session drop.
//!
//! Ctrl+C and Ctrl+Break request shutdown too, until [`Shutdown::route_interrupts`] gives them an event of
//! their own.
#![forbid(unsafe_code)]

use std::time::Duration;

use crate::patch::OsError;
use crate::win32::bridge::{self, Bridge};

/// The installed shutdown bridge.
#[derive(Clone, Copy)]
pub struct Shutdown(&'static Bridge);

impl std::fmt::Debug for Shutdown {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shutdown")
            .field("requested", &self.requested())
            .finish()
    }
}

/// How long a close, logoff or shutdown notice waits for the game to be restored.
pub const CLOSE_BUDGET: Duration = bridge::CLOSE_BUDGET;

/// Installs the bridge, or returns the one already installed.
///
/// # Errors
///
/// The [`OsError`] if the handler, event or window cannot be created.
pub fn install() -> Result<Shutdown, OsError> {
    bridge::install().map(Shutdown)
}

impl Shutdown {
    /// Whether shutdown was requested.
    #[must_use]
    pub fn requested(&self) -> bool {
        self.0.requested()
    }

    /// Waits up to `timeout` for a shutdown request.
    #[must_use]
    pub fn wait(&self, timeout: Duration) -> bool {
        self.0.wait(timeout)
    }

    /// Asks for shutdown, as a closing window would.
    pub fn request(&self) {
        self.0.request();
    }

    /// The event set when shutdown is requested, to wait on with other handles. It stays valid for the whole
    /// process.
    #[must_use]
    pub fn event(&self) -> std::os::windows::io::BorrowedHandle<'static> {
        self.0.borrowed()
    }

    /// From now on Ctrl+C and Ctrl+Break no longer request shutdown: each is counted and signals the returned
    /// event, for an interactive quit. Closing the window, logoff and shutdown still request shutdown and wait
    /// for restoration.
    ///
    /// The event resets itself when a wait returns it, and stays valid for the whole process; read the count
    /// with [`Shutdown::take_interrupts`].
    #[must_use]
    pub fn route_interrupts(&self) -> std::os::windows::io::BorrowedHandle<'static> {
        self.0.route_interrupts()
    }

    /// Interrupts counted since the last call.
    #[must_use]
    pub fn take_interrupts(&self) -> u32 {
        self.0.take_interrupts()
    }
}
