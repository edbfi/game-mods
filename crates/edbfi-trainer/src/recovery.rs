// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a user can do about a failure.
#![forbid(unsafe_code)]

/// The single next step the dashboard offers for an error.
///
/// Every library error maps to exactly one action through its `recovery()` method, so the user interface never
/// has to interpret error text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RecoveryAction {
    /// Start the game, then the trainer connects by itself.
    StartGame,
    /// Close every extra copy of the game so exactly one remains.
    CloseDuplicates,
    /// The condition is transient; try the same operation again.
    Retry,
    /// Fix the reported line in the settings file, then reload.
    EditSettings,
    /// Replace the settings file with defaults, keeping a backup of the old one.
    ResetSettings,
    /// Restart the game so its code is in a known state.
    RestartGame,
    /// This game build is not supported by this trainer version.
    UnsupportedBuild,
    /// The game's code differs from the supported build, for example because another tool patched it.
    ModifiedGameCode,
    /// Another trainer already owns this game process; close it first.
    CloseOtherTrainer,
    /// The trainer itself is wrong; report a bug with the log.
    ReportBug,
}
