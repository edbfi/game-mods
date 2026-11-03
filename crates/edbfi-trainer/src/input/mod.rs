// SPDX-License-Identifier: AGPL-3.0-only
//! What the user did, as the dashboard receives it.
//!
//! The input layer turns key events into [`Command`]s: a bound chord that fired, a key pressed in the trainer's
//! own console, or an interrupt. [`arbiter`] decides which events act; on Windows, `platform` reads them.
//! Deciding what a console key means is the dashboard's job, because it depends on what is on screen.
#![forbid(unsafe_code)]

pub mod arbiter;
#[cfg(windows)]
pub mod platform;

pub use arbiter::{Arbiter, Batch, ConsoleFocus, Epoch, Flush, KeyRecord, Probe, Record};

use crate::chord::{Chord, Key, Modifiers};
use crate::settings::Target;

/// One user action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Command {
    /// The chord bound to this target was pressed, in the game or in the console.
    Hotkey(Target),
    /// A key was pressed in the trainer's console.
    Press(Press),
    /// Ctrl+C was pressed in the trainer's console.
    Interrupt,
}

/// A key pressed in the trainer's console, with the modifiers held.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Press {
    /// The key.
    pub key: ConsoleKey,
    /// Ctrl, Alt and Shift as held, exactly.
    pub modifiers: Modifiers,
}

impl Press {
    /// `key` with no modifier.
    #[must_use]
    pub const fn plain(key: ConsoleKey) -> Self {
        Self {
            key,
            modifiers: Modifiers::NONE,
        }
    }

    /// The chord this press would bind, if its key can be bound.
    #[must_use]
    pub const fn chord(self) -> Option<Chord> {
        match self.key {
            ConsoleKey::Key(key) => Some(Chord::new(self.modifiers, key)),
            _ => None,
        }
    }
}

/// A console key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConsoleKey {
    /// A key a chord can use: letters, digits, the keypad digits, F1–F24, Space and the named navigation keys.
    Key(Key),
    /// The Up arrow.
    Up,
    /// The Down arrow.
    Down,
    /// The Left arrow.
    Left,
    /// The Right arrow.
    Right,
    /// Tab; Shift+Tab arrives as Tab with Shift held.
    Tab,
    /// Enter.
    Enter,
    /// Escape.
    Escape,
    /// Backspace.
    Backspace,
    /// Any other key that types this character, such as `?`.
    Char(char),
}

/// What the keyboard and foreground notices reported since they were last taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Wakes {
    /// A key went down, repeated or went up somewhere: call [`Arbiter::physical`].
    pub keyboard: bool,
    /// A keyboard arrived or was removed: call [`Arbiter::devices`].
    pub devices: bool,
    /// The foreground window changed: call [`Arbiter::foreground`], then read the console once.
    pub foreground: bool,
    /// The newest barrier acknowledged: call [`Arbiter::barrier`].
    pub barrier: Option<Epoch>,
}
