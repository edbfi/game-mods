// SPDX-License-Identifier: AGPL-3.0-only
//! `metro-trainer`: the Metro 2033 Redux trainer. Run `metro-trainer --help` for its command line.
#![forbid(unsafe_code)]

use std::process::ExitCode;

use edbfi_tui::cli::Trainer;
use metro_2033_redux_trainer::profile::METRO;

fn main() -> ExitCode {
    let trainer = Trainer {
        profile: &METRO,
        name: "metro-trainer",
        version: env!("EDBFI_VERSION"),
    };
    ExitCode::from(trainer.main(std::env::args_os().skip(1)))
}
