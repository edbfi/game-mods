// SPDX-License-Identifier: AGPL-3.0-only
//! The trainer's console output, as the dashboard draws on it.
//!
//! [`Output`] turns on virtual-terminal processing and UTF-8 for the console and puts back what it found when
//! dropped. While the dashboard is on the alternate screen, [`Output::arm`] has a panic leave it first, so
//! the panic message lands on the normal screen.
#![forbid(unsafe_code)]

use crate::patch::OsError;
use crate::win32::screen::{self, Screen};

/// The trainer's console output. Dropping it restores the output mode, code page and title it found.
pub struct Output(Screen);

impl std::fmt::Debug for Output {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Output").finish_non_exhaustive()
    }
}

impl Output {
    /// Opens the console's output (`CONOUT$`, so it works with standard output redirected too), saves its
    /// mode, output code page and title, turns on processed output and virtual-terminal processing (line
    /// wrapping stays as found), and sets the output code page to UTF-8. Installs the panic hook if it is not
    /// yet. Only one is open at a time.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if there is no console, the console has no virtual-terminal processing, another
    /// `Output` is open, or the code page cannot be set.
    pub fn open() -> Result<Self, OsError> {
        Screen::open().map(Self)
    }

    /// The visible window size in cells: width, then height.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the console can no longer be read.
    pub fn size(&self) -> Result<(u16, u16), OsError> {
        self.0.size()
    }

    /// Sets the console window's title.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if Windows refuses it.
    pub fn set_title(&self, title: &str) -> Result<(), OsError> {
        self.0.set_title(title)
    }

    /// The dashboard is on the alternate screen, so a panic must leave it. While armed, the first panic
    /// resets colours, shows the cursor, leaves the alternate screen and restores the original output mode,
    /// before the panic is printed, with no allocation, lock or formatting; that uses the arming up.
    pub fn arm(&self) {
        self.0.arm();
    }

    /// Whether the dashboard is still on the alternate screen: `false` once a panic on any thread left it, so
    /// nothing more may be drawn.
    #[must_use]
    pub fn armed(&self) -> bool {
        self.0.armed()
    }

    /// The dashboard is leaving the alternate screen itself. Returns whether it is still on it: `false` once
    /// a panic already left it and turned terminal processing back off, so no escape may be written.
    pub fn disarm(&self) -> bool {
        self.0.disarm()
    }
}

/// Whether standard output is this process's console, rather than a file or pipe.
#[must_use]
pub fn stdout_is_console() -> bool {
    screen::stdout_is_console()
}
