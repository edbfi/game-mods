// SPDX-License-Identifier: AGPL-3.0-only
//! How a run ended: the process exit code and the lines printed after the dashboard closes.
//!
//! | Code | Meaning |
//! |---|---|
//! | 0 | Finished; the game holds nothing of the trainer's |
//! | 1 | An unexpected, access, platform or I/O failure |
//! | 2 | The settings could not be used |
//! | 3 | No game to connect to |
//! | 4 | The game is not the supported build |
//! | 5 | The game's code differs from the supported build |
//! | 6 | Hooks may remain in the game |
//! | 7 | Another trainer owns the game |
//! | 8 | Several games run and none was chosen |
#![forbid(unsafe_code)]

use std::path::Path;

use edbfi_trainer::lifecycle::{Finish, SettingsProblem};
use edbfi_trainer::recovery::RecoveryAction;
use edbfi_trainer::runtime::Done;

use crate::text;
use crate::view::recovery_text;

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The runtime ran and ended so.
    Ran(Done),
    /// The trainer could not start; nothing touched the game.
    NotStarted(String),
    /// The dashboard failed, by a panic or a failed wait, and the runtime then finished on its own.
    Faulted {
        /// What failed.
        fault: String,
        /// How the runtime finished afterwards.
        done: Done,
    },
}

impl Outcome {
    /// The process exit code.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Ran(Done::Finished(_)) => 0,
            Self::Ran(Done::HooksLeft { .. }) => 6,
            Self::Ran(Done::Settings(_)) => 2,
            Self::Ran(Done::Failed(failure)) => match failure.recovery {
                RecoveryAction::StartGame => 3,
                RecoveryAction::UnsupportedBuild => 4,
                RecoveryAction::ModifiedGameCode => 5,
                RecoveryAction::CloseOtherTrainer => 7,
                RecoveryAction::CloseDuplicates => 8,
                RecoveryAction::RestartGame if failure.left_hooks() => 6,
                RecoveryAction::EditSettings | RecoveryAction::ResetSettings => 2,
                _ => 1,
            },
            Self::NotStarted(_) => 1,
            Self::Faulted {
                done: Done::HooksLeft { .. },
                ..
            } => 6,
            Self::Faulted { .. } => 1,
        }
    }

    /// The plain ASCII lines to print once the dashboard has closed. `log` is the log file, if any.
    #[must_use]
    pub fn summary(&self, log: Option<&Path>) -> Vec<String> {
        let mut lines = match self {
            Self::Faulted { fault, done } => {
                let mut lines = vec![format!("The trainer failed: {}", text::ascii(fault))];
                lines.extend(Self::Ran(done.clone()).summary(None));
                lines
            }
            Self::Ran(Done::Finished(Finish::Restored)) => {
                vec!["The game's code is restored and no cheats remain.".to_owned()]
            }
            Self::Ran(Done::Finished(Finish::Untouched)) => {
                vec!["The game was not changed.".to_owned()]
            }
            Self::Ran(Done::HooksLeft { sites, message }) => {
                let sites: Vec<String> = sites.iter().map(ToString::to_string).collect();
                let which = if sites.is_empty() {
                    String::new()
                } else {
                    format!(" (sites {})", sites.join(", "))
                };
                vec![
                    format!(
                        "Some of the game's code may still hold trainer hooks{which}: {}",
                        text::ascii(message)
                    ),
                    "Every cheat is off. Restart the game so its code is in a known state."
                        .to_owned(),
                ]
            }
            Self::Ran(Done::Failed(failure)) => vec![
                text::ascii(&failure.message),
                recovery_text(failure.recovery).to_owned(),
            ],
            Self::Ran(Done::Settings(problem)) => {
                let (path, message) = match problem {
                    SettingsProblem::Invalid { path, error } => (path, error.to_string()),
                    SettingsProblem::Store { path, message, .. } => (path, message.clone()),
                };
                vec![
                    format!(
                        "The settings in {} could not be used: {}",
                        text::ascii(&path.display().to_string()),
                        text::ascii(&message)
                    ),
                    recovery_text(problem.recovery()).to_owned(),
                ]
            }
            Self::NotStarted(message) => vec![format!(
                "The trainer could not start: {}",
                text::ascii(message)
            )],
        };
        if self.exit_code() != 0
            && let Some(log) = log
        {
            lines.push(format!(
                "Details are in {}",
                text::ascii(&log.display().to_string())
            ));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use edbfi_trainer::lifecycle::Failure;
    use edbfi_trainer::profile::SiteId;
    use edbfi_trainer::settings::StoreError;

    use super::*;

    fn failed(recovery: RecoveryAction, unrestored: Vec<SiteId>) -> Outcome {
        Outcome::Ran(Done::Failed(Failure {
            recovery,
            message: "Ünsupported".into(),
            unrestored,
        }))
    }

    #[test]
    fn every_ending_has_its_exit_code() {
        assert_eq!(
            Outcome::Ran(Done::Finished(Finish::Restored)).exit_code(),
            0
        );
        assert_eq!(
            Outcome::Ran(Done::Finished(Finish::Untouched)).exit_code(),
            0
        );
        let hooks = Outcome::Ran(Done::HooksLeft {
            sites: vec![SiteId(3)],
            message: "denied".into(),
        });
        assert_eq!(hooks.exit_code(), 6);
        let settings = Outcome::Ran(Done::Settings(SettingsProblem::store(
            &StoreError::Leased {
                path: PathBuf::from("dir"),
            },
        )));
        assert_eq!(settings.exit_code(), 2);
        for (recovery, code) in [
            (RecoveryAction::StartGame, 3),
            (RecoveryAction::UnsupportedBuild, 4),
            (RecoveryAction::ModifiedGameCode, 5),
            (RecoveryAction::CloseOtherTrainer, 7),
            (RecoveryAction::CloseDuplicates, 8),
            (RecoveryAction::Retry, 1),
            (RecoveryAction::ReportBug, 1),
            (RecoveryAction::UpdateWindows, 1),
        ] {
            assert_eq!(
                failed(recovery, Vec::new()).exit_code(),
                code,
                "{recovery:?}"
            );
        }
        assert_eq!(
            failed(RecoveryAction::RestartGame, vec![SiteId(1)]).exit_code(),
            6
        );
        assert_eq!(Outcome::NotStarted("no".into()).exit_code(), 1);
        let fault = |done| Outcome::Faulted {
            fault: "panicked".into(),
            done,
        };
        assert_eq!(fault(Done::Finished(Finish::Restored)).exit_code(), 1);
        let left = Done::HooksLeft {
            sites: Vec::new(),
            message: "stopped".into(),
        };
        assert_eq!(fault(left.clone()).exit_code(), 6);
        let lines = fault(left).summary(None);
        assert!(
            lines[0].contains("panicked") && lines[1].contains("hooks: stopped"),
            "{lines:?}"
        );
    }

    #[test]
    fn summaries_are_ascii_and_point_at_the_log_only_on_failure() {
        let log = PathBuf::from("trainer.log");
        let restored = Outcome::Ran(Done::Finished(Finish::Restored)).summary(Some(&log));
        assert_eq!(restored.len(), 1);
        let failure = failed(RecoveryAction::UnsupportedBuild, Vec::new()).summary(Some(&log));
        assert!(failure.iter().all(|line| line.is_ascii()), "{failure:?}");
        assert!(failure.last().unwrap().contains("trainer.log"));
        let hooks = Outcome::Ran(Done::HooksLeft {
            sites: vec![SiteId(3), SiteId(7)],
            message: "denied".into(),
        })
        .summary(None);
        assert!(hooks[0].contains("S3, S7"), "{hooks:?}");
    }
}
