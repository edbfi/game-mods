// SPDX-License-Identifier: AGPL-3.0-only
//! `metro-trainer`'s `main` with the `dummy_game` example's profile instead of Metro's: the same command
//! line, driver and shutdown handling, attached to a process the Windows tests own.
//!
//! `EDBFI_DUMMY_GAME` names the dummy's executable and `EDBFI_DUMMY_LAYOUT` holds the first line it printed.

use std::process::ExitCode;

use edbfi_trainer::profile::dummy;
use edbfi_tui::cli::Trainer;

fn main() -> ExitCode {
    let var = |name| std::env::var_os(name).ok_or_else(|| format!("{name} is not set"));
    let profile = var("EDBFI_DUMMY_GAME")
        .and_then(|path| std::fs::read(&path).map_err(|error| format!("{path:?}: {error}")))
        .and_then(|file| {
            let layout = var("EDBFI_DUMMY_LAYOUT")?;
            dummy::profile(&file, &layout.to_string_lossy())
        });
    match profile {
        Ok(profile) => {
            let trainer = Trainer {
                profile,
                name: "dummy-trainer",
                version: "0.0.0",
            };
            ExitCode::from(trainer.main(std::env::args_os().skip(1)))
        }
        Err(error) => {
            eprintln!("dummy-trainer: {error}");
            ExitCode::from(2)
        }
    }
}
