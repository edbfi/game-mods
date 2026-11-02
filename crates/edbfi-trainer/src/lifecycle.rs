// SPDX-License-Identifier: AGPL-3.0-only
//! What the runtime tells the dashboard, and what the dashboard asks of it.
//!
//! The runtime owns the session and its typestates; the dashboard only sees a [`Phase`], the [`Ack`] of each
//! publication and journal entries, and answers with [`RuntimeCommand`]s. Nothing here touches a process.
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::time::Duration;

use crate::control::{Ack, DesiredMask};
use crate::journal::Entry;
use crate::profile::SiteId;
use crate::recovery::RecoveryAction;
use crate::settings::{SettingsError, StoreError};

/// Where the runtime is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The settings cannot be used; nothing connects until the problem is resolved.
    Settings(SettingsProblem),
    /// No supported game is running; the trainer connects when one starts.
    Waiting,
    /// A game was found and is being checked.
    Verifying {
        /// The check under way.
        stage: VerifyStage,
    },
    /// Hooks are being written with every cheat off. Input waits until this ends.
    Installing,
    /// Hooks are in place and publications reach the game.
    Attached {
        /// How long connecting took, from finding the game to the hooks being in place.
        attach_duration: Duration,
    },
    /// Every cheat is off and the game's code is being restored. Input waits until this ends.
    Restoring,
    /// The trainer is exiting.
    Finished(Finish),
    /// Something failed; the failure says what to do.
    Failed(Failure),
}

impl Phase {
    /// Whether publications reach the game.
    #[must_use]
    pub const fn is_attached(&self) -> bool {
        matches!(self, Self::Attached { .. })
    }
}

/// The checks run on a game before any hook is written, in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum VerifyStage {
    /// Comparing the executable file with the supported build.
    Executable,
    /// Waiting for the game to unpack its code.
    Unpacking,
    /// Comparing the unpacked code with the supported build.
    Code,
    /// Checking every hook site's bytes and signature.
    Sites,
}

impl VerifyStage {
    /// Every stage, in order.
    pub const ALL: [Self; 4] = [Self::Executable, Self::Unpacking, Self::Code, Self::Sites];
}

/// How the trainer ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Finish {
    /// The trainer never changed the game.
    Untouched,
    /// The trainer had changed the game and restored every site.
    Restored,
}

/// A failure the dashboard shows until it is resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    /// The one next step to offer.
    pub recovery: RecoveryAction,
    /// What happened, for a person. May hold any text; the dashboard sanitises it.
    pub message: String,
    /// Hook sites whose original code could not be restored. While any remain, the trainer publishes only zero
    /// and never reconnects to that game.
    pub unrestored: Vec<SiteId>,
}

impl Failure {
    /// Whether game code may still hold the trainer's hooks.
    #[must_use]
    pub const fn left_hooks(&self) -> bool {
        !self.unrestored.is_empty()
    }

    /// Whether [`RuntimeCommand::Retry`] can help: never while hooks remain, nor when only a different game
    /// build or Windows version would.
    #[must_use]
    pub const fn can_retry(&self) -> bool {
        !self.left_hooks()
            && !matches!(
                self.recovery,
                RecoveryAction::UnsupportedBuild | RecoveryAction::UpdateWindows
            )
    }
}

/// Why the settings cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingsProblem {
    /// The file exists but is not valid.
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong with it.
        error: SettingsError,
    },
    /// The file or its folder cannot be used.
    Store {
        /// The folder or file involved.
        path: PathBuf,
        /// What failed, for a person.
        message: String,
        /// The one next step to offer.
        recovery: RecoveryAction,
        /// Whether no saved settings can exist, so continuing with unsaved defaults loses nothing.
        session_only: bool,
    },
}

impl SettingsProblem {
    /// The problem `error` reports.
    #[must_use]
    pub fn store(error: &StoreError) -> Self {
        let path = match error {
            StoreError::Folder { path, .. }
            | StoreError::Leased { path }
            | StoreError::Unreadable { path, .. }
            | StoreError::Create { path, .. }
            | StoreError::Write { path, .. } => path.clone(),
        };
        Self::Store {
            path,
            message: error.to_string(),
            recovery: error.recovery(),
            session_only: error.allows_session_only(),
        }
    }

    /// The one next step to offer.
    #[must_use]
    pub const fn recovery(&self) -> RecoveryAction {
        match self {
            Self::Invalid { error, .. } => error.recovery(),
            Self::Store { recovery, .. } => *recovery,
        }
    }
}

/// Something the runtime reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeEvent {
    /// The runtime moved to a new phase.
    Phase(Phase),
    /// A publication completed. The runtime sends exactly one per [`RuntimeCommand::Publish`] it carries out,
    /// in the order it received them; the dashboard matches each to its publication by that order.
    Ack(Ack),
    /// Something was recorded in the journal.
    Journal(Entry),
}

/// Something the dashboard asks the runtime to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuntimeCommand {
    /// Publish this mask. Ignored unless attached.
    Publish(DesiredMask),
    /// Try the failed step again: reload the settings, or look for the game again.
    Retry,
    /// Replace the invalid settings file with defaults, keeping a backup.
    ResetSettings,
    /// Continue with defaults that are never saved, because no settings file can exist.
    SessionOnly,
    /// Publish zero, restore the game's code if attached, then exit.
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(recovery: RecoveryAction, unrestored: Vec<SiteId>) -> Failure {
        Failure {
            recovery,
            message: String::new(),
            unrestored,
        }
    }

    #[test]
    fn left_hooks_never_allow_a_retry() {
        assert!(failure(RecoveryAction::Retry, vec![]).can_retry());
        assert!(!failure(RecoveryAction::Retry, vec![SiteId(3)]).can_retry());
        assert!(failure(RecoveryAction::RestartGame, vec![SiteId(3)]).left_hooks());
        assert!(!failure(RecoveryAction::UnsupportedBuild, vec![]).can_retry());
        assert!(failure(RecoveryAction::ModifiedGameCode, vec![]).can_retry());
    }

    #[test]
    fn store_errors_keep_their_path_and_policy() {
        let error = StoreError::Create {
            path: PathBuf::from("settings.toml"),
            source: std::io::Error::other("read-only"),
        };
        let SettingsProblem::Store {
            path,
            recovery,
            session_only,
            message,
        } = SettingsProblem::store(&error)
        else {
            panic!("a store problem");
        };
        assert_eq!(path, PathBuf::from("settings.toml"));
        assert_eq!(recovery, RecoveryAction::Retry);
        assert!(session_only);
        assert!(message.contains("read-only"));
        let leased = SettingsProblem::store(&StoreError::Leased {
            path: PathBuf::from("dir"),
        });
        assert_eq!(leased.recovery(), RecoveryAction::CloseOtherTrainer);
        assert!(
            Phase::Attached {
                attach_duration: Duration::ZERO
            }
            .is_attached()
        );
    }
}
