// SPDX-License-Identifier: AGPL-3.0-only
//! Finding and opening the game on Windows, and asking a running trainer to stop.
#![forbid(unsafe_code)]

use crate::patch::OsError;
use crate::profile::Profile;
use crate::recovery::RecoveryAction;
use crate::win32::owner;
use crate::win32::process::{self, WinGame};

use super::{Candidate, ReadOnly, StopOutcome, Write};

/// The ids of every running process with the profile's executable name.
///
/// # Errors
///
/// The [`OsError`] if the process list cannot be read.
pub fn find(profile: &Profile) -> Result<Vec<u32>, OsError> {
    process::find(profile.game.exe)
}

/// The one game process to use: `pid` if given and it is the game, otherwise the only one running.
///
/// # Errors
///
/// A [`SelectError`] if no game runs, `pid` is not one, or several run and none was named.
pub fn select(profile: &Profile, pid: Option<u32>) -> Result<u32, SelectError> {
    let found = find(profile).map_err(SelectError::Os)?;
    match (pid, found.as_slice()) {
        (Some(pid), found) if found.contains(&pid) => Ok(pid),
        (Some(pid), _) => Err(SelectError::NotGame(pid)),
        (None, []) => Err(SelectError::NotRunning),
        (None, &[only]) => Ok(only),
        (None, many) => Err(SelectError::Ambiguous(many.len())),
    }
}

/// Why no single game process was selected.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SelectError {
    /// The process list could not be read.
    #[error("cannot list processes: {0}")]
    Os(OsError),
    /// The game is not running.
    #[error("the game is not running")]
    NotRunning,
    /// The named process is not the game.
    #[error("process {0} is not the game")]
    NotGame(u32),
    /// Several copies of the game run and none was named.
    #[error("{0} copies of the game are running")]
    Ambiguous(usize),
}

impl SelectError {
    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Os(_) => RecoveryAction::Retry,
            Self::NotRunning | Self::NotGame(_) => RecoveryAction::StartGame,
            Self::Ambiguous(_) => RecoveryAction::CloseDuplicates,
        }
    }
}

impl Candidate<ReadOnly> {
    /// Opens process `pid` to identify it. It can never be patched.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if it cannot be opened.
    pub fn open(pid: u32) -> Result<Self, OsError> {
        Ok(Self::new(Box::new(WinGame::open(pid, false)?)))
    }
}

impl Candidate<Write> {
    /// Opens process `pid` to identify and patch it.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if it cannot be opened with those rights.
    pub fn open(pid: u32) -> Result<Self, OsError> {
        Ok(Self::new(Box::new(WinGame::open(pid, true)?)))
    }
}

/// Asks the trainer of each running game, or only of `pid`, to restore and exit. The games themselves are
/// never opened.
///
/// # Errors
///
/// The [`OsError`] if the process list cannot be read.
pub fn request_stop(
    profile: &Profile,
    pid: Option<u32>,
) -> Result<Vec<(u32, StopOutcome)>, OsError> {
    let games = match pid {
        Some(pid) => vec![pid],
        None => find(profile)?,
    };
    Ok(games
        .into_iter()
        .map(|pid| (pid, owner::request_stop(profile.slug, pid)))
        .collect())
}
