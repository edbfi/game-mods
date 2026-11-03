// SPDX-License-Identifier: AGPL-3.0-only
//! Where the [`Arbiter`](super::Arbiter)'s events come from on Windows.
//!
//! [`Keyboard`] hears keyboard and foreground notices on the hidden window's thread and wakes the owner
//! through one event; it never polls. [`Console`] reads the trainer's console records. [`System`] answers
//! the arbiter's questions about what is held and what is in front. The owner waits on
//! [`Keyboard::event`] and [`Console::handle`] with its other handles, and hands what it finds to the
//! arbiter.
#![forbid(unsafe_code)]

use std::os::windows::io::BorrowedHandle;

use crate::chord::Modifiers;
use crate::patch::OsError;
use crate::shutdown::Shutdown;
use crate::win32::{console, input};

use super::{Batch, ConsoleFocus, Epoch, Probe};

/// The keyboard and foreground notices.
#[derive(Clone, Copy)]
pub struct Keyboard(&'static input::Notices);

impl std::fmt::Debug for Keyboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keyboard").finish_non_exhaustive()
    }
}

/// What happened since the last [`Keyboard::take`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wakes {
    /// A key went down, repeated or went up somewhere: call [`Arbiter::physical`](super::Arbiter::physical).
    pub keyboard: bool,
    /// A keyboard arrived or was removed: call [`Arbiter::devices`](super::Arbiter::devices).
    pub devices: bool,
    /// The foreground window changed: call [`Arbiter::foreground`](super::Arbiter::foreground), then read
    /// the console once.
    pub foreground: bool,
    /// The newest barrier acknowledged: call [`Arbiter::barrier`](super::Arbiter::barrier).
    pub barrier: Option<Epoch>,
}

impl Keyboard {
    /// The notices of the hidden window that `shutdown` created.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if Windows refused the Raw Input registration, the foreground hook or the event. The
    /// game's hotkeys cannot work then; the trainer must say so rather than fall back to polling.
    pub fn start(shutdown: &Shutdown) -> Result<Self, OsError> {
        let _window_exists = shutdown;
        input::notices()
            .unwrap_or(Err(OsError::new("CreateWindowExW", 0)))
            .map(Self)
    }

    /// The event signalled after each notice, to wait on with other handles. Notices merge; a wake may find
    /// nothing new.
    #[must_use]
    pub fn event(&self) -> BorrowedHandle<'static> {
        self.0.event()
    }

    /// Clears and returns what happened. Handle the keyboard and device notices before the barrier.
    #[must_use]
    pub fn take(&self) -> Wakes {
        self.0.take()
    }

    /// Asks for `epoch` to be acknowledged after every keyboard notice already queued.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the request cannot be posted.
    pub fn barrier(&self, epoch: Epoch) -> Result<(), OsError> {
        self.0.barrier(epoch)
    }
}

/// The trainer's console input. Dropping it restores the console modes it found.
pub struct Console(console::Reader);

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console").finish_non_exhaustive()
    }
}

impl Console {
    /// The most console input records one [`Console::read`] takes. A repeat count can expand one into several
    /// [`Record`](super::Record)s.
    pub const BATCH: usize = console::BATCH;

    /// Opens the console's input, even when standard input is redirected, and sets its modes: no QuickEdit,
    /// mouse, line, echo or virtual-terminal input; processed and window input on.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if there is no console or its modes cannot be set.
    pub fn open() -> Result<Self, OsError> {
        console::Reader::open().map(Self)
    }

    /// The input handle, signalled while records wait.
    #[must_use]
    pub fn handle(&self) -> BorrowedHandle<'_> {
        self.0.handle()
    }

    /// Reads the waiting records without blocking, at most [`Console::BATCH`]. Reading with nothing waiting
    /// returns an empty, complete batch.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the console can no longer be read. Tell the arbiter with
    /// [`Arbiter::console_failed`](super::Arbiter::console_failed).
    pub fn read(&mut self) -> Result<Batch, OsError> {
        self.0.read()
    }

    /// Discards every waiting record, as a barrier needs.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the console can no longer be used.
    pub fn flush(&mut self) -> Result<(), OsError> {
        self.0.flush()
    }
}

/// The keyboard and windows as Windows reports them now.
#[derive(Clone, Copy, Debug, Default)]
pub struct System;

const VK_SHIFT: u8 = 0x10;
const VK_CONTROL: u8 = 0x11;
const VK_MENU: u8 = 0x12;
const VK_LWIN: u8 = 0x5B;
const VK_RWIN: u8 = 0x5C;

impl Probe for System {
    fn down(&self, vk: u8) -> bool {
        input::key_down(vk)
    }

    fn modifiers(&self) -> Option<Modifiers> {
        if input::key_down(VK_LWIN) || input::key_down(VK_RWIN) {
            return None;
        }
        let mut modifiers = Modifiers::NONE;
        for (vk, modifier) in [
            (VK_CONTROL, Modifiers::CTRL),
            (VK_MENU, Modifiers::ALT),
            (VK_SHIFT, Modifiers::SHIFT),
        ] {
            if input::key_down(vk) {
                modifiers = modifiers.union(modifier);
            }
        }
        Some(modifiers)
    }

    fn foreground(&self) -> Option<u32> {
        input::foreground()
    }

    fn console(&self) -> ConsoleFocus {
        input::console_focus()
    }
}
