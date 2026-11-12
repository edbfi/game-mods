// SPDX-License-Identifier: AGPL-3.0-or-later
use std::ffi::OsString;
use std::path::PathBuf;

use edbfi_trainer::addr::Rva;
use edbfi_trainer::identity::Sha256Digest;
use edbfi_trainer::location::{LocationError, Locations};
use edbfi_trainer::patch::OsError;
use edbfi_trainer::profile::SiteId;
use edbfi_trainer::profile::synthetic::SYNTHETIC;
use edbfi_trainer::session::{IdentifyError, SiteCheck, SiteStatus, StopOutcome, VerifyRestore};

use super::*;

fn args(line: &str) -> Vec<OsString> {
    line.split_whitespace().map(OsString::from).collect()
}

fn parsed(line: &str) -> Result<Command, UsageError> {
    parse(args(line))
}

const TRAINER: Trainer = Trainer {
    profile: &SYNTHETIC,
    name: "synthetic-trainer",
    version: "1.2.3",
};

#[test]
fn every_documented_form_parses() {
    assert_eq!(parsed(""), Ok(Command::Run(Run::default())));
    assert_eq!(parsed("--help"), Ok(Command::Help));
    assert_eq!(parsed("--version"), Ok(Command::Version));
    assert_eq!(parsed("check"), Ok(Command::Check { pid: None }));
    assert_eq!(
        parsed("check --pid 42"),
        Ok(Command::Check { pid: Some(42) })
    );
    assert_eq!(parsed("stop"), Ok(Command::Stop { pid: None }));
    assert_eq!(
        parsed("stop --pid 4294967295"),
        Ok(Command::Stop {
            pid: Some(u32::MAX)
        })
    );
    assert_eq!(
        parsed("--pid 7 --plain --verify-restore --colour truecolor --glyphs ascii"),
        Ok(Command::Run(Run {
            pid: Some(7),
            plain: true,
            verify: VerifyRestore::Text,
            colour: Some(Colour::TrueColour),
            glyphs: Some(Glyphs::Ascii),
        }))
    );
    for (value, colour) in [
        ("auto", None),
        ("16", Some(Colour::Ansi16)),
        ("mono", Some(Colour::Mono)),
    ] {
        assert_eq!(
            parsed(&format!("--colour {value}")),
            Ok(Command::Run(Run {
                colour,
                ..Run::default()
            }))
        );
    }
    assert_eq!(parsed("--glyphs auto"), Ok(Command::Run(Run::default())));
}

#[test]
fn anything_else_is_a_usage_error() {
    let invalid = |option, value: &str, expected| UsageError::Invalid {
        option,
        value: value.to_owned(),
        expected,
    };
    for (line, error) in [
        ("--bogus", UsageError::Unknown("--bogus".into())),
        ("run", UsageError::Unknown("run".into())),
        ("-h", UsageError::Unknown("-h".into())),
        ("--pid=5", UsageError::Unknown("--pid=5".into())),
        ("--help --plain", UsageError::NotAlone("--help")),
        ("--plain --help", UsageError::NotAlone("--help")),
        ("check --version", UsageError::NotAlone("--version")),
        ("--plain check", UsageError::Misplaced("check".into())),
        ("check stop", UsageError::Misplaced("stop".into())),
        (
            "check --plain",
            UsageError::NotFor {
                option: "--plain",
                command: "check",
            },
        ),
        (
            "stop --verify-restore",
            UsageError::NotFor {
                option: "--verify-restore",
                command: "stop",
            },
        ),
        ("--plain --plain", UsageError::Repeated("--plain")),
        ("--pid 1 --pid 2", UsageError::Repeated("--pid")),
        (
            "--colour auto --colour 16",
            UsageError::Repeated("--colour"),
        ),
        ("--pid", UsageError::Missing("--pid")),
        ("stop --pid", UsageError::Missing("--pid")),
        ("--glyphs", UsageError::Missing("--glyphs")),
        ("--pid 0", invalid("--pid", "0", "a process id")),
        ("--pid +5", invalid("--pid", "+5", "a process id")),
        ("--pid -1", invalid("--pid", "-1", "a process id")),
        (
            "--pid 4294967296",
            invalid("--pid", "4294967296", "a process id"),
        ),
        ("--pid abc", invalid("--pid", "abc", "a process id")),
        (
            "--colour 256",
            invalid("--colour", "256", "auto, 16, mono or truecolor"),
        ),
        (
            "--colour truecolour",
            invalid("--colour", "truecolour", "auto, 16, mono or truecolor"),
        ),
        (
            "--glyphs unicode",
            invalid("--glyphs", "unicode", "auto or ascii"),
        ),
    ] {
        assert_eq!(parsed(line), Err(error), "{line}");
    }
    assert_eq!(
        parse([OsString::from("")]),
        Err(UsageError::Unknown(String::new()))
    );
}

#[cfg(unix)]
#[test]
fn an_argument_that_is_not_unicode_is_refused() {
    use std::os::unix::ffi::OsStringExt as _;
    let error = parse([OsString::from_vec(vec![b'-', 0xFF])]).unwrap_err();
    assert_eq!(error, UsageError::NotUnicode("-\u{FFFD}".into()));
}

#[test]
fn usage_errors_read_as_sentences() {
    assert_eq!(
        UsageError::Unknown("--bogus".into()).to_string(),
        r#"unknown argument "--bogus""#
    );
    assert_eq!(
        UsageError::NotFor {
            option: "--plain",
            command: "check"
        }
        .to_string(),
        "check does not take --plain"
    );
    assert_eq!(
        UsageError::Invalid {
            option: "--pid",
            value: "\u{1b}[2J".into(),
            expected: "a process id"
        }
        .to_string(),
        r#"--pid takes a process id, not "\u{1b}[2J""#,
        "control characters are escaped"
    );
}

#[test]
fn an_explicit_appearance_overrides_only_what_it_names() {
    let wt = |name: &str| (name == "WT_SESSION").then(|| OsString::from("1"));
    assert_eq!(Run::default().appearance(wt), None);
    let glyphs = Run {
        glyphs: Some(Glyphs::Ascii),
        ..Run::default()
    };
    assert_eq!(
        glyphs.appearance(wt),
        Some(Appearance {
            colour: Colour::TrueColour,
            glyphs: Glyphs::Ascii,
        })
    );
    let colour = Run {
        colour: Some(Colour::Mono),
        ..Run::default()
    };
    assert_eq!(
        colour.appearance(wt),
        Some(Appearance {
            colour: Colour::Mono,
            glyphs: Glyphs::Unicode,
        })
    );
}

#[test]
fn help_names_the_build_the_files_the_hotkeys_and_every_exit_code() {
    let locations = Locations {
        settings: PathBuf::from("/config/edbfi/synthetic-game/trainer"),
        logs: PathBuf::from("/state/edbfi/synthetic-game/trainer"),
    };
    let text = help(TRAINER, &Ok(locations.clone()));
    assert!(text.starts_with("synthetic-trainer 1.2.3: a trainer for Synthetic Game.\n"));
    for needle in [
        "synthetic-trainer check [--pid N]",
        "synthetic-trainer stop [--pid N]",
        "the one synthetic.exe running",
        &format!(
            "the synthetic.exe whose SHA-256 is\n  {}\n",
            SYNTHETIC.identity.disk_sha256
        ),
        "  Alt+F8     Master switch",
        &format!(
            "Settings: {} (written on first run)",
            locations.settings_file().display()
        ),
        &format!("Log:      {}", locations.log_file().display()),
        "8 several games and no --pid.",
    ] {
        assert!(text.contains(needle), "{needle:?} missing from:\n{text}");
    }
    for toggle in SYNTHETIC.toggles {
        match toggle.binding.default {
            Some(chord) => assert!(
                text.contains(&format!("  {:<11}{}", chord.to_string(), toggle.label)),
                "{} is listed with its default",
                toggle.key
            ),
            None => assert!(!text.contains(&format!("  {}\n", toggle.label))),
        }
    }
    assert!(
        text.lines().all(|line| line.chars().count() <= 104),
        "help fits a wide console:\n{text}"
    );

    let unset = help(TRAINER, &Err(LocationError::Unset { var: "APPDATA" }));
    assert!(unset.contains(
        "Settings: none: the environment variable APPDATA is not set; changes last only until"
    ));
}

fn sites(statuses: &[SiteStatus]) -> Vec<SiteCheck> {
    SYNTHETIC
        .sites
        .iter()
        .zip(statuses)
        .map(|(site, &status)| SiteCheck {
            site: site.id,
            status,
        })
        .collect()
}

#[test]
fn a_supported_game_checks_every_site_and_every_feature() {
    let report = check_report(&SYNTHETIC, 42, Ok(()));
    assert_eq!(report.code, 0);
    assert_eq!(
        report.lines[..3],
        [
            "Checked Synthetic Game, process 42, without changing it.".to_owned(),
            format!(
                "Build: supported (synthetic.exe, SHA-256 {})",
                SYNTHETIC.identity.disk_sha256
            ),
            "Code:  verified".to_owned(),
        ]
    );
    assert_eq!(report.lines[3], "Sites: 4 of 4 verified");
    for (line, site) in report.lines[4..8].iter().zip(SYNTHETIC.sites) {
        assert_eq!(line, &format!("  {:<4}verified", site.id.to_string()));
    }
    assert_eq!(report.lines[8], "Features: all 5 ready");
    assert!(report.lines[9].starts_with("Every check passed"));
}

#[test]
fn a_failed_site_names_the_features_it_blocks() {
    let mut statuses = [SiteStatus::Verified; 4];
    statuses[3] = SiteStatus::OriginalDiffers;
    let error = IdentifyError::Sites {
        sites: sites(&statuses),
    };
    let report = check_report(&SYNTHETIC, 42, Err(&error));
    assert_eq!(
        report.code, 1,
        "a profile that misses verified code is a bug"
    );
    let text = report.lines.join("\n");
    assert!(text.contains("Code:  verified"), "{text}");
    assert!(text.contains("Sites: 3 of 4 verified"), "{text}");
    assert!(
        text.contains("  S4  the site's bytes differ from the original"),
        "{text}"
    );
    // The pressure site serves only the toggles that keep the pressure class's pressure.
    assert!(text.contains("Features blocked:\n"), "{text}");
    assert!(
        text.contains("A hook site does not match the profile."),
        "{text}"
    );

    let moved = [
        SiteStatus::ContextMoved {
            found: Rva::new(0x1234),
        },
        SiteStatus::ContextMissing,
        SiteStatus::ContextAmbiguous,
        SiteStatus::OutsideCode,
    ];
    let error = IdentifyError::Code {
        sites: sites(&moved),
    };
    let report = check_report(&SYNTHETIC, 42, Err(&error));
    assert_eq!(report.code, 5);
    let text = report.lines.join("\n");
    assert!(text.contains("Sites: 0 of 4 verified"), "{text}");
    assert!(
        text.contains("Code:  the game's code differs from the supported build"),
        "{text}"
    );
    for group in SYNTHETIC.groups {
        let count = SYNTHETIC
            .toggles
            .iter()
            .filter(|toggle| toggle.group == group.key)
            .count();
        assert!(
            text.contains(&format!("  {}: all {count} (needs ", group.title)),
            "{text}"
        );
    }
    assert!(text.contains("wait for its main menu"), "{text}");
}

#[test]
fn only_the_toggles_a_site_serves_are_blocked() {
    for site in SYNTHETIC.sites {
        let lines = features(&SYNTHETIC, &[site.id]);
        let users: Vec<&str> = SYNTHETIC
            .toggles
            .iter()
            .filter(|toggle| needs(&SYNTHETIC, toggle, site))
            .map(|toggle| toggle.label)
            .collect();
        assert!(!users.is_empty(), "{} serves something", site.id);
        assert_eq!(lines[0], "Features blocked:");
        let text = lines.join("\n");
        assert!(text.contains(&format!("(needs {})", site.id)), "{text}");
        for toggle in SYNTHETIC.toggles {
            let mentioned = text.contains(toggle.label)
                || text.contains(&format!("{}: all", group_title(toggle.group)));
            assert_eq!(
                mentioned,
                users.contains(&toggle.label),
                "{} for {}:\n{text}",
                toggle.key,
                site.id
            );
        }
    }
    assert_eq!(features(&SYNTHETIC, &[]), ["Features: all 5 ready"]);
    assert_eq!(
        features(&SYNTHETIC, &[SiteId(99)]),
        ["Features: all 5 ready"]
    );
}

fn group_title(key: &str) -> &'static str {
    SYNTHETIC
        .groups
        .iter()
        .find(|group| group.key == key)
        .unwrap()
        .title
}

#[test]
fn identity_failures_say_which_stage_failed_and_exit_by_their_recovery() {
    let found =
        Sha256Digest::from_hex("00000000000000000000000000000000000000000000000000000000000000ff");
    for (error, build, code, exit) in [
        (
            IdentifyError::Unsupported { found },
            format!("unsupported game build (executable SHA-256 {found})"),
            "not read",
            4,
        ),
        (
            IdentifyError::Os(OsError::new("OpenProcess", 5)),
            IdentifyError::Os(OsError::new("OpenProcess", 5)).to_string(),
            "not read",
            1,
        ),
        (
            IdentifyError::ModulePath,
            "supported".to_owned(),
            "the loaded game is not the executable it was started from",
            5,
        ),
    ] {
        let report = check_report(&SYNTHETIC, 7, Err(&error));
        assert_eq!(report.code, exit, "{error:?}");
        assert!(
            report.lines[1].starts_with(&format!("Build: {build}")),
            "{:?}",
            report.lines
        );
        assert_eq!(report.lines[2], format!("Code:  {code}"));
        assert_eq!(report.lines[3], "Sites: not checked");
        assert_eq!(report.lines[4], recovery_text(error.recovery()));
    }
    // A game whose loader has not run yet may be checked again in a moment.
    let starting = check_report(
        &SYNTHETIC,
        7,
        Err(&IdentifyError::Os(OsError::new("EnumProcessModules", 299))),
    );
    assert!(starting.lines.last().unwrap().contains("main menu"));
}

#[test]
fn stop_counts_requests_and_failures() {
    let none = stop_report("Synthetic Game", &[]);
    assert_eq!(none.code, 3);
    assert_eq!(
        none.lines,
        ["Synthetic Game is not running; nothing to stop."]
    );

    let idle = stop_report("Synthetic Game", &[(5, StopOutcome::NoTrainer)]);
    assert_eq!(idle.code, 3);
    assert_eq!(
        idle.lines,
        [
            "Process 5: no trainer is connected.",
            "No trainer is connected to the game."
        ]
    );

    let sent = stop_report(
        "Synthetic Game",
        &[(5, StopOutcome::Requested), (6, StopOutcome::NoTrainer)],
    );
    assert_eq!(sent.code, 0);
    assert!(sent.lines[0].contains("asked its trainer"));
    assert!(
        !sent.lines.iter().any(|line| line.contains("restored")),
        "a request is not a restoration: {:?}",
        sent.lines
    );

    let partial = stop_report(
        "Synthetic Game",
        &[
            (5, StopOutcome::Requested),
            (6, StopOutcome::Failed(OsError::new("SetEvent", 5))),
        ],
    );
    assert_eq!(partial.code, 1);
    assert_eq!(
        partial.lines.last().unwrap(),
        "1 of 2 requests failed; 1 sent."
    );

    // A game without a trainer is not a request.
    let mixed = stop_report(
        "Synthetic Game",
        &[
            (5, StopOutcome::Requested),
            (6, StopOutcome::NoTrainer),
            (7, StopOutcome::Failed(OsError::new("SetEvent", 5))),
        ],
    );
    assert_eq!(mixed.code, 1);
    assert_eq!(
        mixed.lines.last().unwrap(),
        "1 of 2 requests failed; 1 sent."
    );
}

#[test]
fn a_game_another_trainer_is_connected_to_is_not_checked() {
    let report = connected_report(42);
    assert_eq!(report.code, 7);
    assert!(report.lines[0].starts_with("Process 42: a trainer is connected"));
}
