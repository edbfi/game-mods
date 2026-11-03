// SPDX-License-Identifier: AGPL-3.0-only
//! The trainer executable's command line, run as a user runs it. `EDBFI_TRAINER_EXE` points these tests at
//! another build of it, such as the release candidate that CI cross-builds.
#![expect(
    clippy::unwrap_used,
    reason = "helpers shared by tests; a failed call is a test failure"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn exe() -> PathBuf {
    std::env::var_os("EDBFI_TRAINER_EXE").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_metro-trainer")),
        PathBuf::from,
    )
}

/// An empty folder that stands in for the user's profile, removed afterwards.
struct Home(PathBuf);

impl Home {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("edbfi-cli-{}-{name}", std::process::id()));
        let _removed = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut command = Command::new(exe());
        for var in ["APPDATA", "XDG_CONFIG_HOME"] {
            command.env(var, self.0.join("config"));
        }
        for var in ["LOCALAPPDATA", "XDG_STATE_HOME"] {
            command.env(var, self.0.join("state"));
        }
        command.env("HOME", &self.0).args(args).output().unwrap()
    }

    /// Whether anything was created in it.
    fn empty(&self) -> bool {
        std::fs::read_dir(&self.0).unwrap().next().is_none()
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn version_and_help_print_and_create_nothing() {
    let home = Home::new("help");
    let version = home.run(&["--version"]);
    assert!(version.status.success());
    assert!(
        text(&version.stdout).starts_with("metro-trainer "),
        "{version:?}"
    );

    let help = home.run(&["--help"]);
    assert!(help.status.success(), "{help:?}");
    let out = text(&help.stdout);
    for needle in [
        "metro-trainer check [--pid N]",
        "metro-trainer stop [--pid N]",
        "the metro.exe whose SHA-256 is",
        "Alt+F8     Master switch",
        "8 several games and no --pid.",
    ] {
        assert!(out.contains(needle), "{needle:?} missing from:\n{out}");
    }
    let settings = home
        .path()
        .join("config")
        .join("edbfi")
        .join("metro-2033-redux")
        .join("trainer")
        .join("settings.toml");
    assert!(
        out.contains(&format!("Settings: {}", settings.display())),
        "{out}"
    );
    assert!(help.stderr.is_empty());
    assert!(home.empty(), "help creates no settings or log");
}

#[test]
fn usage_errors_exit_2_and_say_why() {
    let home = Home::new("usage");
    for (args, why) in [
        (&["--bogus"][..], r#"unknown argument "--bogus""#),
        (&["check", "--plain"], "check does not take --plain"),
        (&["--pid", "0"], r#"--pid takes a process id, not "0""#),
        (
            &["stop", "--pid", "1", "--pid", "2"],
            "--pid is given more than once",
        ),
        (&["--help", "--plain"], "--help takes no other arguments"),
        (
            &["--plain", "check"],
            r#"the command "check" must come first"#,
        ),
    ] {
        let output = home.run(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(
            text(&output.stderr),
            format!("metro-trainer: {why}\nRun 'metro-trainer --help' for usage.\n"),
            "{args:?}"
        );
    }
    assert!(home.empty());
}

#[cfg(windows)]
#[test]
fn check_and_stop_refuse_a_process_that_is_not_the_game() {
    let home = Home::new("refuse");
    let me = std::process::id().to_string();

    let check = home.run(&["check", "--pid", &me]);
    assert_eq!(check.status.code(), Some(3), "{check:?}");
    let out = text(&check.stdout);
    assert!(
        out.starts_with(&format!("Process {me} is not the game.\n")),
        "{out}"
    );

    let stop = home.run(&["stop", "--pid", &me]);
    assert_eq!(stop.status.code(), Some(3), "{stop:?}");
    assert_eq!(
        text(&stop.stdout),
        format!("Process {me}: no trainer is connected.\nNo trainer is connected to the game.\n")
    );
    assert!(home.empty(), "check and stop create no settings or log");
}

#[cfg(not(windows))]
#[test]
fn elsewhere_every_command_that_needs_the_game_says_so() {
    let home = Home::new("elsewhere");
    for args in [&[][..], &["check"], &["stop", "--pid", "5"]] {
        let output = home.run(args);
        assert_eq!(output.status.code(), Some(1), "{args:?}");
        assert_eq!(text(&output.stdout), "This trainer runs only on Windows.\n");
    }
    assert!(home.empty());
}
