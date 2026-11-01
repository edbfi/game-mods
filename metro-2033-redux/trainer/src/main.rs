// SPDX-License-Identifier: AGPL-3.0-only
//! Metro 2033 Redux trainer.
//!
//! The Rust trainer is being assembled; until it replaces the C++ build this binary only reports its version.
#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    match (args.next(), args.next()) {
        (Some(flag), None) if flag == "--version" => {
            println!("metro-trainer {}", env!("EDBFI_VERSION"));
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!(
                "metro-trainer: the Rust trainer is not assembled yet; only --version is supported"
            );
            ExitCode::from(2)
        }
    }
}
