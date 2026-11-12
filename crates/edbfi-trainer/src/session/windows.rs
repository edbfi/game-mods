// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finding and opening the game on Windows, asking a running trainer to stop, and the handles the owner
//! waits on: the game's process, signalled when it exits, and the stop event, signalled when another trainer
//! process asks this one to restore and exit.
#![forbid(unsafe_code)]

use std::os::windows::io::BorrowedHandle;

use crate::patch::OsError;
use crate::profile::Profile;
use crate::recovery::RecoveryAction;
use crate::win32::owner;
use crate::win32::process::{self, WinGame};

use super::{Access, Candidate, Installed, ReadOnly, Recovery, Session, State, StopOutcome, Write};

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

impl<A: Access> Candidate<A> {
    /// The process handle, signalled once the game exits.
    #[must_use]
    pub fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
        self.game.handle()
    }
}

impl<S: State, A: Access> Session<S, A> {
    /// The process handle, signalled once the game exits.
    #[must_use]
    pub fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
        self.core.game.handle()
    }
}

impl Session<Installed> {
    /// The stop event, signalled once another trainer process asks this one to restore and exit.
    #[must_use]
    pub fn stop_handle(&self) -> Option<BorrowedHandle<'_>> {
        self.state.owner.stop_event()
    }
}

impl Session<Recovery> {
    /// The stop event, signalled once another trainer process asks this one to restore and exit.
    #[must_use]
    pub fn stop_handle(&self) -> Option<BorrowedHandle<'_>> {
        self.state.owner.stop_event()
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

/// Whether another trainer owns process `pid`, so its code may hold that trainer's hooks. Creates nothing
/// and never opens the game.
///
/// # Errors
///
/// The [`OsError`] if the trainer's owner object cannot be looked up.
pub fn trainer_connected(profile: &Profile, pid: u32) -> Result<bool, OsError> {
    owner::owned_by_trainer(profile.slug, pid)
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
