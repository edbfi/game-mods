// SPDX-License-Identifier: AGPL-3.0-only
//! The commands on Windows: the dashboard, a read-only `check` and `stop`.
#![forbid(unsafe_code)]

use edbfi_trainer::profile::{Profile, ValidProfile};
use edbfi_trainer::session::{self, Candidate, ReadOnly, SelectError};

use super::{Report, Run, Trainer, check_report, connected_report, sentence, stop_report};
use crate::driver::windows::{self as driver, Options};
use crate::outcome::failure_code;
use crate::view::recovery_text;

/// Runs the dashboard and returns its exit code; the driver prints the summary.
pub(super) fn run(trainer: Trainer, run: &Run) -> u8 {
    let options = Options {
        pid: run.pid,
        verify: run.verify,
        plain: run.plain,
        appearance: run.appearance(|name| std::env::var_os(name)),
    };
    let header = format!("{} {}", trainer.name, trainer.version);
    driver::run(trainer.profile, &header, options).exit_code()
}

/// Identifies the one running game, or `pid`, through a handle that can only read it, unless another trainer
/// is connected to it. Never waits for the game to unpack its code.
pub(super) fn check(profile: &'static Profile, pid: Option<u32>) -> Report {
    let valid = match ValidProfile::new(profile) {
        Ok(valid) => valid,
        Err(error) => {
            return Report::failed(vec![format!("The game profile is invalid: {error:?}")], 1);
        }
    };
    let pid = match session::select(profile, pid) {
        Ok(pid) => pid,
        Err(error) => return selection(&error),
    };
    match session::trainer_connected(profile, pid) {
        Ok(false) => {}
        Ok(true) => return connected_report(pid),
        Err(error) => {
            return Report::failed(
                vec![format!(
                    "Cannot tell whether a trainer is connected to process {pid}: {error}"
                )],
                1,
            );
        }
    }
    let candidate = match Candidate::<ReadOnly>::open(pid) {
        Ok(candidate) => candidate,
        Err(error) => {
            let mut lines = vec![format!("Cannot open process {pid} to read it: {error}")];
            // ERROR_ACCESS_DENIED: most often a game that runs as administrator.
            if error.code == 5 {
                lines.push(
                    "If the game runs as administrator, run the check as administrator too."
                        .to_owned(),
                );
            }
            return Report::failed(lines, 1);
        }
    };
    let result = session::identify(candidate, &valid);
    check_report(
        profile,
        pid,
        result
            .as_ref()
            .map(|_| ())
            .map_err(|rejected| &rejected.error),
    )
}

/// Why no game was selected, and its exit code.
fn selection(error: &SelectError) -> Report {
    let mut lines = vec![format!("{}.", sentence(&error.to_string()))];
    lines.push(recovery_text(error.recovery()).to_owned());
    if let SelectError::Ambiguous(_) = error {
        lines.push("Or choose one with --pid N.".to_owned());
    }
    Report::failed(lines, failure_code(error.recovery()))
}

/// Signals the stop event of the trainer connected to each running game, or to `pid`. Opens no game.
pub(super) fn stop(profile: &'static Profile, pid: Option<u32>) -> Report {
    match session::request_stop(profile, pid) {
        Ok(results) => stop_report(profile.game.name, &results),
        Err(error) => Report::failed(vec![format!("Cannot list processes: {error}")], 1),
    }
}
