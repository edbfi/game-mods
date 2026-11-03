// SPDX-License-Identifier: AGPL-3.0-only
//! The command line a trainer executable runs:
//!
//! ```text
//! <name> [--pid N] [--plain] [--verify-restore] [--colour auto|16|mono|truecolor] [--glyphs auto|ascii]
//! <name> check [--pid N]
//! <name> stop [--pid N]
//! <name> --help
//! <name> --version
//! ```
//!
//! The parser is strict: each option at most once, the command first, no other spellings. A usage error
//! exits 2. `check` identifies the game without changing it and `stop` signals the stop event of each
//! connected trainer; both print plain lines. [`outcome`](crate::outcome) lists every exit code.
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write as _};

use edbfi_trainer::location::{LocationError, Locations};
use edbfi_trainer::profile::{HookKind, Profile, Site, SiteId, Toggle};
use edbfi_trainer::session::{IdentifyError, SiteStatus, StopOutcome, VerifyRestore};

use crate::outcome::failure_code;
use crate::theme::{Appearance, Colour, Glyphs};
use crate::view::recovery_text;

#[cfg(windows)]
mod windows;

#[cfg(test)]
mod tests;

/// A trainer executable: its game and how it names itself.
#[derive(Clone, Copy, Debug)]
pub struct Trainer {
    /// The game it trains.
    pub profile: &'static Profile,
    /// The executable's name without `.exe`, as the user types it.
    pub name: &'static str,
    /// Its version.
    pub version: &'static str,
}

impl Trainer {
    /// Runs the command in `args`, which excludes the executable's own name, and returns the exit code.
    #[must_use]
    pub fn main(self, args: impl IntoIterator<Item = OsString>) -> u8 {
        let command = match parse(args) {
            Ok(command) => command,
            Err(error) => {
                let mut stderr = io::stderr().lock();
                let _written = writeln!(stderr, "{}: {error}", self.name);
                let _written = writeln!(stderr, "Run '{} --help' for usage.", self.name);
                return 2;
            }
        };
        match command {
            Command::Help => {
                print(&help(self, &Locations::from_env(self.profile.slug)));
                0
            }
            Command::Version => {
                print(&format!("{} {}\n", self.name, self.version));
                0
            }
            Command::Check { pid } => self.report(platform::check(self.profile, pid)),
            Command::Stop { pid } => self.report(platform::stop(self.profile, pid)),
            Command::Run(run) => platform::run(self, &run),
        }
    }

    fn report(self, report: Report) -> u8 {
        let mut text = String::new();
        for line in &report.lines {
            text.push_str(line);
            text.push('\n');
        }
        print(&text);
        report.code
    }
}

/// Writes `text` to standard output. A closed pipe loses it; the exit code still says what happened.
fn print(text: &str) {
    let mut stdout = io::stdout().lock();
    let _written = stdout.write_all(text.as_bytes());
    let _flushed = stdout.flush();
}

/// What to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Wait for the game, connect and show the dashboard.
    Run(Run),
    /// Identify the game without changing it.
    Check {
        /// Only this process.
        pid: Option<u32>,
    },
    /// Ask the trainer of each running game, or of one, to restore it and exit.
    Stop {
        /// Only this process.
        pid: Option<u32>,
    },
    /// Print the help.
    Help,
    /// Print the version.
    Version,
}

/// How to run the dashboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Run {
    /// Connect only to this process.
    pub pid: Option<u32>,
    /// Print plain lines instead of drawing the dashboard.
    pub plain: bool,
    /// What a restore checks besides the sites.
    pub verify: VerifyRestore,
    /// The colour depth; `None` detects it.
    pub colour: Option<Colour>,
    /// The glyph set; `None` detects it.
    pub glyphs: Option<Glyphs>,
}

impl Run {
    /// The appearance asked for, the rest detected through `var`; `None` when nothing was asked for.
    #[must_use]
    pub fn appearance(
        &self,
        var: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Option<Appearance> {
        if self.colour.is_none() && self.glyphs.is_none() {
            return None;
        }
        let detected = Appearance::detect(var);
        Some(Appearance {
            colour: self.colour.unwrap_or(detected.colour),
            glyphs: self.glyphs.unwrap_or(detected.glyphs),
        })
    }
}

/// Why the command line was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UsageError {
    /// An argument is not valid Unicode.
    NotUnicode(String),
    /// An argument no command takes.
    Unknown(String),
    /// An option the command does not take.
    NotFor {
        /// The option.
        option: &'static str,
        /// The command.
        command: &'static str,
    },
    /// A command after the first argument.
    Misplaced(String),
    /// `--help` or `--version` with other arguments.
    NotAlone(&'static str),
    /// An option given twice.
    Repeated(&'static str),
    /// An option without its value.
    Missing(&'static str),
    /// An option's value is not one it takes.
    Invalid {
        /// The option.
        option: &'static str,
        /// The value given.
        value: String,
        /// What it takes.
        expected: &'static str,
    },
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotUnicode(arg) => write!(f, "{arg:?} is not valid Unicode"),
            Self::Unknown(arg) => write!(f, "unknown argument {arg:?}"),
            Self::NotFor { option, command } => write!(f, "{command} does not take {option}"),
            Self::Misplaced(command) => write!(f, "the command {command:?} must come first"),
            Self::NotAlone(option) => write!(f, "{option} takes no other arguments"),
            Self::Repeated(option) => write!(f, "{option} is given more than once"),
            Self::Missing(option) => write!(f, "{option} needs a value"),
            Self::Invalid {
                option,
                value,
                expected,
            } => write!(f, "{option} takes {expected}, not {value:?}"),
        }
    }
}

impl std::error::Error for UsageError {}

/// The options; each is a `&'static str` so errors can name it.
const OPTIONS: [&str; 5] = [
    "--pid",
    "--plain",
    "--verify-restore",
    "--colour",
    "--glyphs",
];

/// Parses `args`, which exclude the executable's own name.
///
/// # Errors
///
/// A [`UsageError`] for anything but the documented forms.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| UsageError::NotUnicode(arg.to_string_lossy().into_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let (command, options) = match args.first().map(String::as_str) {
        Some(only @ ("--help" | "--version")) => {
            let (only, command) = if only == "--help" {
                ("--help", Command::Help)
            } else {
                ("--version", Command::Version)
            };
            return if args.len() == 1 {
                Ok(command)
            } else {
                Err(UsageError::NotAlone(only))
            };
        }
        Some("check") => (Some("check"), &args[1..]),
        Some("stop") => (Some("stop"), &args[1..]),
        _ => (None, &args[..]),
    };
    let mut run = Run::default();
    let mut seen: Vec<&'static str> = Vec::new();
    let mut options = options.iter();
    while let Some(arg) = options.next() {
        let Some(option) = OPTIONS.into_iter().find(|option| option == arg) else {
            return Err(match arg.as_str() {
                "--help" => UsageError::NotAlone("--help"),
                "--version" => UsageError::NotAlone("--version"),
                "check" | "stop" => UsageError::Misplaced(arg.clone()),
                _ => UsageError::Unknown(arg.clone()),
            });
        };
        if let Some(command) = command
            && option != "--pid"
        {
            return Err(UsageError::NotFor { option, command });
        }
        if seen.contains(&option) {
            return Err(UsageError::Repeated(option));
        }
        seen.push(option);
        let mut value = || options.next().ok_or(UsageError::Missing(option));
        match option {
            "--pid" => run.pid = Some(pid(value()?)?),
            "--plain" => run.plain = true,
            "--verify-restore" => run.verify = VerifyRestore::Text,
            "--colour" => {
                run.colour = choice(
                    option,
                    value()?,
                    "auto, 16, mono or truecolor",
                    &[
                        ("auto", None),
                        ("16", Some(Colour::Ansi16)),
                        ("mono", Some(Colour::Mono)),
                        ("truecolor", Some(Colour::TrueColour)),
                    ],
                )?;
            }
            _ => {
                run.glyphs = choice(
                    option,
                    value()?,
                    "auto or ascii",
                    &[("auto", None), ("ascii", Some(Glyphs::Ascii))],
                )?;
            }
        }
    }
    Ok(match command {
        Some("check") => Command::Check { pid: run.pid },
        Some(_) => Command::Stop { pid: run.pid },
        None => Command::Run(run),
    })
}

/// A process id: decimal digits only, not zero.
fn pid(value: &str) -> Result<u32, UsageError> {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| value.parse::<u32>().ok())
        .flatten()
        .filter(|&pid| pid != 0)
        .ok_or_else(|| UsageError::Invalid {
            option: "--pid",
            value: value.to_owned(),
            expected: "a process id",
        })
}

/// The choice named `value` among `choices`.
fn choice<T: Copy>(
    option: &'static str,
    value: &str,
    expected: &'static str,
    choices: &[(&str, T)],
) -> Result<T, UsageError> {
    choices
        .iter()
        .find(|(name, _)| *name == value)
        .map(|&(_, choice)| choice)
        .ok_or_else(|| UsageError::Invalid {
            option,
            value: value.to_owned(),
            expected,
        })
}

/// The help text. `locations` are where the settings and log live; nothing is created.
#[must_use]
pub fn help(trainer: Trainer, locations: &Result<Locations, LocationError>) -> String {
    let Trainer {
        profile,
        name,
        version,
    } = trainer;
    let game = profile.game;
    let mut hotkeys = vec![(profile.master_default.default, "Master switch")];
    hotkeys.extend(
        profile
            .toggles
            .iter()
            .map(|toggle| (toggle.binding.default, toggle.label)),
    );
    let hotkeys: Vec<String> = hotkeys
        .into_iter()
        .filter_map(|(chord, label)| Some(format!("  {:<11}{label}", chord?.to_string())))
        .collect();
    let (settings, log) = match locations {
        Ok(locations) => (
            format!(
                "{} (written on first run)",
                locations.settings_file().display()
            ),
            locations.log_file().display().to_string(),
        ),
        Err(error) => (
            format!("none: {error}; changes last only until the trainer exits"),
            format!("none: {error}"),
        ),
    };
    format!(
        "\
{name} {version}: a trainer for {game_name}.

Usage:
  {name} [--pid N] [--plain] [--verify-restore] [--colour MODE] [--glyphs MODE]
  {name} check [--pid N]
  {name} stop [--pid N]
  {name} --help
  {name} --version

With no command, the trainer waits for the game, connects and shows its dashboard. Quitting puts the
game's code back as it was.
  check    Check the running game without changing it: its build, its code and every hook site.
  stop     Ask the trainer connected to each running game to put it back and exit.

Options:
  --pid N            Use only process N. Without it the trainer uses the one {exe} running and
                     refuses to choose between several.
  --plain            Print plain lines instead of drawing the dashboard.
  --verify-restore   After putting the game back, also check its whole code section.
  --colour MODE      auto, 16, mono or truecolor. auto picks truecolor in Windows Terminal and mono
                     when NO_COLOR is set.
  --glyphs MODE      auto or ascii. Use ascii with a font that lacks line-drawing characters.

Supported build: the {exe} whose SHA-256 is
  {sha}
Any other build is refused and never changed.

Default hotkeys (press R in the dashboard to rebind them):
{hotkeys}
Hotkeys act while the game or this window is in front. The game still sees each key, and overlays
such as a GPU's or Steam's may use the same ones: rebind any that clash.

Settings: {settings}
Log:      {log}

Exit codes: 0 done, 1 failed, 2 usage or settings, 3 no game or trainer, 4 unsupported build,
5 game code differs, 6 hooks may remain, 7 another trainer owns the game, 8 several games and no --pid.
",
        game_name = game.name,
        exe = game.exe,
        sha = profile.identity.disk_sha256,
        hotkeys = hotkeys.join("\n"),
    )
}

/// Lines to print and the exit code.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The lines, plain ASCII but for paths and system messages.
    pub lines: Vec<String>,
    /// The exit code.
    pub code: u8,
}

impl Report {
    fn failed(lines: Vec<String>, code: u8) -> Self {
        Self { lines, code }
    }
}

/// The `check` report for process `pid`, which `result` says is the supported build or why not.
#[must_use]
pub fn check_report(profile: &Profile, pid: u32, result: Result<(), &IdentifyError>) -> Report {
    let mut lines = vec![format!(
        "Checked {}, process {pid}, without changing it.",
        profile.game.name
    )];
    let supported = format!(
        "supported ({}, SHA-256 {})",
        profile.game.exe, profile.identity.disk_sha256
    );
    let (build, code) = match result {
        Ok(()) => (supported, "verified".to_owned()),
        Err(
            error @ (IdentifyError::ModulePath
            | IdentifyError::ModuleSize { .. }
            | IdentifyError::Identity(_)
            | IdentifyError::Code { .. }),
        ) => (supported, error.to_string()),
        Err(IdentifyError::Sites { .. }) => (supported, "verified".to_owned()),
        Err(error) => (error.to_string(), "not read".to_owned()),
    };
    lines.push(format!("Build: {build}"));
    lines.push(format!("Code:  {code}"));
    let checks: Option<Vec<(SiteId, SiteStatus)>> = match result {
        Ok(()) => Some(
            profile
                .sites
                .iter()
                .map(|site| (site.id, SiteStatus::Verified))
                .collect(),
        ),
        Err(error) => error.sites().map(|sites| {
            sites
                .iter()
                .map(|check| (check.site, check.status))
                .collect()
        }),
    };
    match &checks {
        Some(checks) => {
            let verified = checks
                .iter()
                .filter(|(_, status)| *status == SiteStatus::Verified)
                .count();
            lines.push(format!(
                "Sites: {verified} of {} verified",
                profile.sites.len()
            ));
            lines.extend(
                checks
                    .iter()
                    .map(|(site, status)| format!("  {:<4}{status}", site.to_string())),
            );
            let failed: Vec<SiteId> = checks
                .iter()
                .filter(|(_, status)| *status != SiteStatus::Verified)
                .map(|&(site, _)| site)
                .collect();
            lines.extend(features(profile, &failed));
        }
        None => lines.push("Sites: not checked".to_owned()),
    }
    match result {
        Ok(()) => {
            lines.push("Every check passed: the trainer can connect to this game.".to_owned());
            Report { lines, code: 0 }
        }
        Err(error) => {
            if let IdentifyError::Sites { .. } = error {
                lines.push(format!("{}.", sentence(&error.to_string())));
            }
            lines.push(recovery_text(error.recovery()).to_owned());
            if error.transient() {
                lines.push(
                    "If the game has only just started, wait for its main menu and check again."
                        .to_owned(),
                );
            }
            Report::failed(lines, failure_code(error.recovery()))
        }
    }
}

/// The features the `failed` sites block, by group, or that every feature is ready.
fn features(profile: &Profile, failed: &[SiteId]) -> Vec<String> {
    let sites: Vec<&Site> = profile
        .sites
        .iter()
        .filter(|site| failed.contains(&site.id))
        .collect();
    let mut lines = Vec::new();
    for group in profile.groups {
        let toggles: Vec<&Toggle> = profile
            .toggles
            .iter()
            .filter(|toggle| toggle.group == group.key)
            .collect();
        let blocked: Vec<&Toggle> = toggles
            .iter()
            .copied()
            .filter(|toggle| sites.iter().any(|site| needs(profile, toggle, site)))
            .collect();
        if blocked.is_empty() {
            continue;
        }
        let by: Vec<String> = sites
            .iter()
            .filter(|site| blocked.iter().any(|toggle| needs(profile, toggle, site)))
            .map(|site| site.id.to_string())
            .collect();
        let which = if blocked.len() == toggles.len() {
            format!("all {}", toggles.len())
        } else {
            blocked
                .iter()
                .map(|toggle| toggle.label)
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!(
            "  {}: {which} (needs {})",
            group.title,
            by.join(", ")
        ));
    }
    if lines.is_empty() {
        vec![format!("Features: all {} ready", profile.toggles.len())]
    } else {
        lines.insert(0, "Features blocked:".to_owned());
        lines
    }
}

/// Whether `toggle` needs `site`'s hook.
fn needs(profile: &Profile, toggle: &Toggle, site: &Site) -> bool {
    match site.kind {
        HookKind::Gate { bit, .. } => toggle.controls.contains(bit),
        HookKind::NoReload(_) => profile
            .classes
            .iter()
            .any(|class| toggle.controls.contains(class.bit)),
        HookKind::Pressure { bit, .. } => profile
            .derived
            .iter()
            .any(|derived| derived.bit == bit && derived.when_any.contains(&toggle.key)),
    }
}

/// The `stop` report for `game` from what each request did.
#[must_use]
pub fn stop_report(game: &str, results: &[(u32, StopOutcome)]) -> Report {
    if results.is_empty() {
        return Report::failed(vec![format!("{game} is not running; nothing to stop.")], 3);
    }
    let mut lines = Vec::new();
    let (mut requested, mut failed) = (0, 0);
    for (pid, outcome) in results {
        lines.push(match outcome {
            StopOutcome::Requested => {
                requested += 1;
                format!("Process {pid}: asked its trainer to put the game back and exit.")
            }
            StopOutcome::NoTrainer => format!("Process {pid}: no trainer is connected."),
            StopOutcome::Failed(error) => {
                failed += 1;
                format!("Process {pid}: cannot ask its trainer to stop: {error}")
            }
        });
    }
    let code = if failed > 0 {
        lines.push(format!(
            "{failed} of {} requests failed; {requested} sent.",
            results.len()
        ));
        1
    } else if requested == 0 {
        lines.push("No trainer is connected to the game.".to_owned());
        3
    } else {
        lines.push(
            "Each trainer restores its game and exits by itself; its log says how that went."
                .to_owned(),
        );
        0
    };
    Report { lines, code }
}

/// `text` with its first letter capitalised.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

#[cfg(windows)]
use windows as platform;

/// The commands that need Windows, refused elsewhere.
#[cfg(not(windows))]
mod platform {
    use super::{Report, Run, Trainer};
    use edbfi_trainer::profile::Profile;

    fn refused() -> Report {
        Report::failed(vec!["This trainer runs only on Windows.".to_owned()], 1)
    }

    pub(super) fn check(_profile: &'static Profile, _pid: Option<u32>) -> Report {
        refused()
    }

    pub(super) fn stop(_profile: &'static Profile, _pid: Option<u32>) -> Report {
        refused()
    }

    pub(super) fn run(trainer: Trainer, _run: &Run) -> u8 {
        trainer.report(refused())
    }
}
