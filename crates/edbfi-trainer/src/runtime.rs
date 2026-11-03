// SPDX-License-Identifier: AGPL-3.0-only
//! What the owner thread does between the dashboard and the game.
//!
//! [`Runtime`] owns the settings file, the journal and the game's session typestates. The driver tells it
//! what happened — a [`RuntimeCommand`] from the dashboard, a due step, the game exiting, a stop request —
//! and drains the [`RuntimeEvent`]s it reports. Every call returns quickly except the steps that read or
//! change the game, which the driver runs only after drawing the phase that announces them.
//!
//! ```text
//! Settings ─resolved→ Waiting ─found→ Verifying ─identified→ Installing ─installed→ Attached
//!                        ↑                │ unpacking: again in 250 ms, for 60 s            │
//!                        └─game exited────┴──────────────────────────────────────────────────┤
//!                                                                    quit, stop or shutdown → Restoring → Finished
//! ```
//!
//! A failure shows [`Phase::Failed`]. If hooks may remain in the game, the session is pinned: the trainer
//! keeps its ownership, publishes nothing and never connects to that process again, until the game exits or
//! a restore succeeds.
#![forbid(unsafe_code)]

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::journal::{EntryKind, Event, Journal};
use crate::lifecycle::{
    Failure, Finish, Phase, RuntimeCommand, RuntimeEvent, SettingsProblem, VerifyStage,
};
use crate::location::{LocationError, Locations};
use crate::patch::OsError;
use crate::profile::{SiteId, ValidProfile};
use crate::recovery::RecoveryAction;
use crate::session::{
    Candidate, Identified, Installed, Recovery, Session, TrainerError, VerifyRestore, Write,
    identify,
};
use crate::settings::{External, Load, Saved, Settings, Store, StoreError};

/// How often the trainer looks for the game while none is found.
pub const DISCOVERY: Duration = Duration::from_secs(1);
/// How soon identification is tried again while the game is still unpacking its code.
pub const UNPACK_RETRY: Duration = Duration::from_millis(250);
/// How long a game may take to unpack its code before the trainer gives up.
pub const UNPACK_DEADLINE: Duration = Duration::from_secs(60);

/// How the runtime finds and opens game processes.
pub trait Discover {
    /// The ids of every running process with the game's executable name.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if the process list cannot be read.
    fn find(&self) -> Result<Vec<u32>, OsError>;

    /// Opens process `pid` to identify and patch it.
    ///
    /// # Errors
    ///
    /// The [`OsError`] if it cannot be opened with those rights.
    fn open(&self, pid: u32) -> Result<Candidate<Write>, OsError>;
}

/// The running processes, as Windows lists them.
#[cfg(windows)]
#[derive(Clone, Copy, Debug)]
pub struct Processes(pub &'static crate::profile::Profile);

#[cfg(windows)]
impl Discover for Processes {
    fn find(&self) -> Result<Vec<u32>, OsError> {
        crate::session::find(self.0)
    }

    fn open(&self, pid: u32) -> Result<Candidate<Write>, OsError> {
        Candidate::<Write>::open(pid)
    }
}

/// How the runtime runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// Connect only to this process.
    pub pid: Option<u32>,
    /// What every restore checks besides the sites.
    pub verify: VerifyRestore,
}

/// What the runtime starts with, for the dashboard.
#[derive(Clone, Debug)]
pub struct Started {
    /// The settings in use: as loaded, or defaults while [`Phase::Settings`] blocks.
    pub settings: Settings,
    /// The settings file, or `None` if settings are not saved.
    pub settings_path: Option<PathBuf>,
    /// The log file, or `None` if the journal has none.
    pub log_path: Option<PathBuf>,
    /// The first phase.
    pub phase: Phase,
}

/// How the runtime ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Done {
    /// It finished; the game holds nothing of the trainer's.
    Finished(Finish),
    /// It exited while hooks may remain in the game.
    HooksLeft {
        /// The sites that may still hold the trainer's jumps.
        sites: Vec<SiteId>,
        /// What the last restore reported.
        message: String,
    },
    /// The user quit while this failure was shown.
    Failed(Failure),
    /// The user quit while the settings could not be used.
    Settings(SettingsProblem),
}

/// The game, at whatever point the runtime has reached with it.
enum Slot {
    /// No game.
    Empty,
    /// A game found and being identified.
    Found {
        candidate: Candidate<Write>,
        found: Instant,
        unpacking: Option<Instant>,
    },
    /// The supported build, about to be hooked.
    Identified {
        session: Session<Identified>,
        found: Instant,
    },
    /// Hooked; publications reach it.
    Attached(Session<Installed>),
    /// A failed install or restore: hooks may remain. Kept until the game exits or a restore succeeds.
    Pinned(Session<Recovery>),
}

/// The owner thread's runtime. See the module documentation.
pub struct Runtime {
    profile: ValidProfile<'static>,
    discover: Box<dyn Discover>,
    options: Options,
    locations: Result<Locations, LocationError>,
    store: Option<Store>,
    journal: Journal,
    phase: Phase,
    slot: Slot,
    /// When the next step is due; `None` waits for something to happen.
    due: Option<Instant>,
    events: Vec<RuntimeEvent>,
    /// Quitting: the next restore ends the run.
    quitting: bool,
    /// Quitting without the user: a stop request, a closing window or shutdown. Nothing waits for a reply.
    forced: bool,
    done: Option<Done>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("phase", &self.phase)
            .field("due", &self.due)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

impl Runtime {
    /// Starts the runtime: opens and reads the settings, writing defaults on first run.
    #[must_use]
    pub fn start(
        profile: ValidProfile<'static>,
        discover: Box<dyn Discover>,
        locations: Result<Locations, LocationError>,
        journal: Journal,
        options: Options,
    ) -> (Self, Started) {
        let mut runtime = Self {
            profile,
            discover,
            options,
            locations,
            store: None,
            journal,
            phase: Phase::Waiting,
            slot: Slot::Empty,
            due: None,
            events: Vec::new(),
            quitting: false,
            forced: false,
            done: None,
        };
        if let Some(failure) = runtime.journal.failure() {
            let message = failure.to_string();
            runtime.record(Event::warning(EntryKind::Session, message));
        }
        let settings = match runtime.load() {
            Ok(settings) => {
                runtime.waiting();
                settings
            }
            Err(problem) => {
                runtime.phase = Phase::Settings(*problem);
                Settings::defaults(&runtime.profile)
            }
        };
        // The dashboard starts from these; reporting them again would only repeat them.
        runtime.events.clear();
        let started = Started {
            settings,
            settings_path: runtime.store.as_ref().map(|store| store.path().to_owned()),
            log_path: runtime.journal.path().map(ToOwned::to_owned),
            phase: runtime.phase.clone(),
        };
        (runtime, started)
    }

    /// The phase last reported.
    #[must_use]
    pub const fn phase(&self) -> &Phase {
        &self.phase
    }

    /// When [`Runtime::step`] is next due; `None` while only an outside event can change anything.
    #[must_use]
    pub const fn due(&self) -> Option<Instant> {
        self.due
    }

    /// The process the trainer is attached to, whose window must be in front for a game hotkey.
    #[must_use]
    pub fn game(&self) -> Option<u32> {
        match &self.slot {
            Slot::Attached(session) => Some(session.pid()),
            _ => None,
        }
    }

    /// How the run ended, once it has.
    #[must_use]
    pub const fn done(&self) -> Option<&Done> {
        self.done.as_ref()
    }

    /// The journal.
    #[must_use]
    pub const fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Takes what happened since the last call, in order.
    pub fn events(&mut self) -> Vec<RuntimeEvent> {
        std::mem::take(&mut self.events)
    }

    /// The game process, signalled when it exits. Wait on it while there is one.
    #[cfg(windows)]
    #[must_use]
    pub fn process_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
        match &self.slot {
            Slot::Empty => None,
            Slot::Found { candidate, .. } => candidate.process_handle(),
            Slot::Identified { session, .. } => session.process_handle(),
            Slot::Attached(session) => session.process_handle(),
            Slot::Pinned(session) => session.process_handle(),
        }
    }

    /// The stop event other trainer processes set to ask this one to restore and exit. Wait on it while there
    /// is one.
    #[cfg(windows)]
    #[must_use]
    pub fn stop_handle(&self) -> Option<std::os::windows::io::BorrowedHandle<'_>> {
        match &self.slot {
            Slot::Attached(session) => session.stop_handle(),
            Slot::Pinned(session) => session.stop_handle(),
            _ => None,
        }
    }

    /// Carries out a dashboard command.
    pub fn command(&mut self, command: RuntimeCommand) {
        match command {
            RuntimeCommand::Publish(mask) => {
                if let Slot::Attached(session) = &mut self.slot {
                    let ack = session.publish(mask);
                    self.events.push(RuntimeEvent::Ack(ack));
                }
            }
            RuntimeCommand::Retry => self.retry(),
            RuntimeCommand::ResetSettings => self.reset(),
            RuntimeCommand::SessionOnly => {
                // Only where no saved settings can exist: unsaved defaults must never hide a file's choices.
                if matches!(
                    self.phase,
                    Phase::Settings(SettingsProblem::Store {
                        session_only: true,
                        ..
                    })
                ) {
                    self.store = None;
                    self.record(Event::warning(
                        EntryKind::Settings,
                        "continuing with defaults that are not saved",
                    ));
                    let settings = Settings::defaults(&self.profile);
                    self.events.push(RuntimeEvent::Loaded {
                        settings,
                        path: None,
                    });
                    self.waiting();
                }
            }
            RuntimeCommand::Quit => self.quit(false),
        }
    }

    /// Another trainer process asked this one to stop: restore and exit, with no confirmation.
    pub fn stop(&mut self) {
        if !self.forced {
            self.record(Event::info(EntryKind::Session, "asked to stop"));
        }
        self.quit(true);
    }

    /// The console is closing, the user is logging off, Windows is shutting down or the trainer panicked:
    /// restore and exit, with no confirmation.
    pub fn shutdown(&mut self) {
        if !self.forced {
            self.record(Event::info(EntryKind::Session, "shutting down"));
        }
        self.quit(true);
    }

    /// The game process may have exited: ends its session without touching the game.
    pub fn exited(&mut self) {
        let gone = match &self.slot {
            Slot::Empty => false,
            Slot::Found { candidate, .. } => candidate.exited(),
            Slot::Identified { session, .. } => session.exit_proof().is_some(),
            Slot::Attached(session) => session.exit_proof().is_some(),
            Slot::Pinned(session) => session.exit_proof().is_some(),
        };
        if !gone {
            return;
        }
        // The sessions are dropped: a game that has exited holds nothing of the trainer's.
        self.slot = Slot::Empty;
        self.record(Event::info(EntryKind::Game, "the game exited"));
        if self.quitting {
            self.finish(Finish::Untouched);
        } else {
            self.waiting();
        }
    }

    /// Runs the step that is due: looks for the game, identifies it, hooks it or restores it.
    pub fn step(&mut self) {
        self.due = None;
        if self.done.is_some() {
            return;
        }
        match self.phase {
            Phase::Waiting => self.discover(),
            Phase::Failed(Failure {
                recovery: RecoveryAction::CloseDuplicates,
                ..
            }) if matches!(self.slot, Slot::Empty) => self.discover(),
            Phase::Verifying { .. } => self.identify(),
            Phase::Installing => self.install(),
            Phase::Restoring => self.restore(),
            _ => {}
        }
    }

    /// Saves `settings`, unless they are not saved this session.
    ///
    /// # Errors
    ///
    /// The [`StoreError`] if the file cannot be written; it holds its previous contents.
    pub fn save(&mut self, settings: &Settings) -> Result<Saved, StoreError> {
        let Some(store) = &mut self.store else {
            return Ok(Saved::Unchanged);
        };
        if matches!(self.phase, Phase::Settings(_)) {
            return Ok(Saved::Unchanged);
        }
        let saved = store.save(&self.profile, settings);
        match &saved {
            Ok(Saved::Written {
                external: Some(External::Kept(path)),
            }) => {
                let message = format!(
                    "the settings file had been changed outside the trainer; that version is kept as {}",
                    path.display()
                );
                self.record(Event::warning(EntryKind::Settings, message));
            }
            Ok(Saved::Written {
                external: Some(External::Deleted),
            }) => self.record(Event::warning(
                EntryKind::Settings,
                "the settings file had been deleted; it was written again",
            )),
            Ok(_) => {}
            Err(error) => {
                let message = error.to_string();
                self.record(Event::warning(EntryKind::Settings, message));
            }
        }
        saved
    }

    // ----- settings -----

    /// Opens the store if needed and loads the settings.
    fn load(&mut self) -> Result<Settings, Box<SettingsProblem>> {
        if self.store.is_none() {
            let locations = self.locations.clone().map_err(|error| {
                let path = match &error {
                    LocationError::Unset { var } | LocationError::Relative { var, .. } => {
                        PathBuf::from(format!("%{var}%"))
                    }
                };
                Box::new(SettingsProblem::Store {
                    path,
                    message: error.to_string(),
                    recovery: error.recovery(),
                    session_only: true,
                })
            })?;
            let store = Store::open(&locations.settings)
                .map_err(|error| Box::new(self.store_problem(&error)))?;
            self.store = Some(store);
        }
        let Some(store) = &mut self.store else {
            unreachable!("the store was just opened")
        };
        let path = store.path().to_owned();
        match store.load(&self.profile) {
            Ok(Load::Created(settings)) => {
                let message = format!("created {}", path.display());
                self.record(Event::info(EntryKind::Settings, message));
                Ok(settings)
            }
            Ok(Load::Read(parsed)) => {
                if let Some(upgrade) = parsed.upgrade {
                    let message =
                        format!("upgraded {} from schema {}", path.display(), upgrade.from);
                    self.record(Event::info(EntryKind::Settings, message));
                    // A failed upgrade save is reported, and the next save tries again.
                    let _saved = self.save_now(&parsed.settings);
                }
                Ok(parsed.settings)
            }
            Ok(Load::Invalid(error)) => {
                let message = format!("{} is not valid: {error}", path.display());
                self.record(Event::error(EntryKind::Settings, message));
                Err(Box::new(SettingsProblem::Invalid { path, error }))
            }
            Err(error) => Err(Box::new(self.store_problem(&error))),
        }
    }

    /// Saves while the phase may still be [`Phase::Settings`].
    fn save_now(&mut self, settings: &Settings) -> Result<Saved, StoreError> {
        let phase = std::mem::replace(&mut self.phase, Phase::Waiting);
        let saved = self.save(settings);
        self.phase = phase;
        saved
    }

    fn store_problem(&mut self, error: &StoreError) -> SettingsProblem {
        let message = error.to_string();
        self.record(Event::error(EntryKind::Settings, message));
        SettingsProblem::store(error)
    }

    fn reset(&mut self) {
        if !matches!(self.phase, Phase::Settings(SettingsProblem::Invalid { .. })) {
            return;
        }
        let Some(store) = &mut self.store else {
            return;
        };
        match store.reset(&self.profile) {
            Ok(reset) => {
                let path = store.path().to_owned();
                let message = match &reset.backup {
                    Some(backup) => format!(
                        "reset {} to defaults; the invalid file is kept as {}",
                        path.display(),
                        backup.display()
                    ),
                    None => format!("reset {} to defaults", path.display()),
                };
                self.record(Event::warning(EntryKind::Settings, message));
                self.events.push(RuntimeEvent::Loaded {
                    settings: reset.settings,
                    path: Some(path),
                });
                self.waiting();
            }
            Err(error) => {
                let problem = self.store_problem(&error);
                self.enter(Phase::Settings(problem));
            }
        }
    }

    // ----- the game -----

    fn retry(&mut self) {
        match &self.phase {
            Phase::Settings(_) => match self.load() {
                Ok(settings) => {
                    let path = self.store.as_ref().map(|store| store.path().to_owned());
                    self.events.push(RuntimeEvent::Loaded { settings, path });
                    self.waiting();
                }
                Err(problem) => self.enter(Phase::Settings(*problem)),
            },
            Phase::Failed(failure) if failure.can_retry() => {
                if matches!(self.slot, Slot::Pinned(_)) {
                    self.enter(Phase::Restoring);
                    self.due = Some(Instant::now());
                } else {
                    self.slot = Slot::Empty;
                    self.waiting();
                    self.due = Some(Instant::now());
                }
            }
            _ => {}
        }
    }

    fn quit(&mut self, forced: bool) {
        if self.done.is_some() {
            return;
        }
        self.forced |= forced;
        self.quitting = true;
        match &self.slot {
            Slot::Attached(_) | Slot::Pinned(_) => {
                if self.phase != Phase::Restoring {
                    self.enter(Phase::Restoring);
                }
                self.due = Some(Instant::now());
            }
            Slot::Found { .. } | Slot::Identified { .. } | Slot::Empty => {
                // Nothing was written: dropping the candidate or session releases it.
                self.slot = Slot::Empty;
                match &self.phase {
                    Phase::Failed(failure) => self.done = Some(Done::Failed(failure.clone())),
                    Phase::Settings(problem) => self.done = Some(Done::Settings(problem.clone())),
                    _ => {}
                }
                if self.done.is_none() {
                    self.finish(Finish::Untouched);
                } else {
                    self.due = None;
                }
            }
        }
    }

    fn finish(&mut self, finish: Finish) {
        self.due = None;
        self.done = Some(Done::Finished(finish));
        self.enter(Phase::Finished(finish));
    }

    fn waiting(&mut self) {
        self.enter(Phase::Waiting);
        self.due = Some(Instant::now());
    }

    fn enter(&mut self, phase: Phase) {
        // A failure or settings problem met again is reported again, so a retry visibly ran.
        if phase != self.phase || matches!(phase, Phase::Failed(_) | Phase::Settings(_)) {
            self.phase = phase.clone();
            self.events.push(RuntimeEvent::Phase(phase));
        }
    }

    /// Records `event` in the journal and reports it to the dashboard.
    pub fn record(&mut self, event: Event) {
        self.journal.record(event);
        if let Some(entry) = self.journal.activity().last() {
            self.events.push(RuntimeEvent::Journal(entry.clone()));
        }
    }

    fn fail(&mut self, recovery: RecoveryAction, message: String, unrestored: Vec<SiteId>) {
        self.record(Event::error(EntryKind::Game, message.clone()));
        self.enter(Phase::Failed(Failure {
            recovery,
            message,
            unrestored,
        }));
    }

    fn discover(&mut self) {
        let mut pids = match self.discover.find() {
            Ok(pids) => pids,
            Err(error) => {
                self.fail(
                    RecoveryAction::Retry,
                    format!("cannot list processes: {error}"),
                    Vec::new(),
                );
                return;
            }
        };
        if let Some(pid) = self.options.pid {
            pids.retain(|&found| found == pid);
        }
        match pids.as_slice() {
            [] => {
                if !matches!(self.phase, Phase::Waiting) {
                    self.enter(Phase::Waiting);
                }
                self.due = Some(Instant::now() + DISCOVERY);
            }
            &[pid] => match self.discover.open(pid) {
                Ok(candidate) => {
                    let message = format!("found the game, process {pid}");
                    self.record(Event::info(EntryKind::Game, message));
                    self.slot = Slot::Found {
                        candidate,
                        found: Instant::now(),
                        unpacking: None,
                    };
                    self.enter(Phase::Verifying {
                        stage: VerifyStage::Executable,
                    });
                    self.due = Some(Instant::now());
                }
                Err(error) => self.fail(
                    RecoveryAction::Retry,
                    format!("cannot open the game, process {pid}: {error}"),
                    Vec::new(),
                ),
            },
            many => {
                let message = format!(
                    "{} copies of the game are running; close all but one, or choose one with --pid",
                    many.len()
                );
                let unchanged = matches!(
                    &self.phase,
                    Phase::Failed(failure) if failure.message == message
                );
                if !unchanged {
                    self.fail(RecoveryAction::CloseDuplicates, message, Vec::new());
                }
                // Keep looking: once only one runs, the trainer connects to it.
                self.due = Some(Instant::now() + DISCOVERY);
            }
        }
    }

    fn identify(&mut self) {
        let Slot::Found {
            candidate,
            found,
            unpacking,
        } = std::mem::replace(&mut self.slot, Slot::Empty)
        else {
            return;
        };
        match identify(candidate, &self.profile) {
            Ok(session) => {
                self.record(Event::info(
                    EntryKind::Game,
                    "the game is the supported build",
                ));
                self.slot = Slot::Identified { session, found };
                self.enter(Phase::Installing);
                self.due = Some(Instant::now());
            }
            Err(rejected) => {
                let now = Instant::now();
                let since = unpacking.unwrap_or(now);
                if rejected.candidate.exited() {
                    self.record(Event::info(
                        EntryKind::Game,
                        "the game exited while it was being checked",
                    ));
                    self.waiting();
                } else if rejected.error.transient() && now.duration_since(since) < UNPACK_DEADLINE
                {
                    self.slot = Slot::Found {
                        candidate: rejected.candidate,
                        found,
                        unpacking: Some(since),
                    };
                    self.enter(Phase::Verifying {
                        stage: VerifyStage::Unpacking,
                    });
                    self.due = Some(now + UNPACK_RETRY);
                } else {
                    let message = if rejected.error.transient() {
                        format!(
                            "the game did not finish starting within {} s: {}",
                            UNPACK_DEADLINE.as_secs(),
                            rejected.error
                        )
                    } else {
                        rejected.error.to_string()
                    };
                    self.fail(rejected.error.recovery(), message, Vec::new());
                }
            }
        }
    }

    fn install(&mut self) {
        let Slot::Identified { session, found } = std::mem::replace(&mut self.slot, Slot::Empty)
        else {
            return;
        };
        let prepared = match session.claim() {
            Ok(claimed) => claimed.prepare(),
            Err(failure) => {
                self.fail(
                    failure.error.recovery(),
                    failure.error.to_string(),
                    Vec::new(),
                );
                return;
            }
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                self.fail(
                    failure.error.recovery(),
                    failure.error.to_string(),
                    Vec::new(),
                );
                return;
            }
        };
        match prepared.install() {
            Ok(session) => {
                let attach_duration = found.elapsed();
                let message = format!("hooked the game in {} ms", attach_duration.as_millis());
                self.record(Event::info(EntryKind::Hooks, message));
                self.slot = Slot::Attached(session);
                if self.quitting {
                    self.enter(Phase::Restoring);
                    self.due = Some(Instant::now());
                } else {
                    self.enter(Phase::Attached { attach_duration });
                }
            }
            Err(failure) => {
                let error = failure.error;
                self.record(Event::error(
                    EntryKind::Hooks,
                    format!("hooking the game failed: {error}"),
                ));
                // Whatever the failed install left is restored now; only what stays pins the game.
                self.recover(failure.session, &error);
            }
        }
    }

    /// Restores a failed install's leftovers, then shows `error`.
    fn recover(&mut self, session: Session<Recovery>, error: &TrainerError) {
        match session.restore(self.options.verify) {
            Ok(detached) => {
                drop(detached);
                if self.quitting {
                    self.finish(Finish::Untouched);
                } else {
                    self.fail(error.recovery(), error.to_string(), Vec::new());
                }
            }
            Err(failure) => self.pin(failure.session, &failure.error, false),
        }
    }

    /// Keeps a session whose game may still hold hooks. `again` says the restore that failed was already a
    /// second try at a pinned session.
    fn pin(&mut self, session: Session<Recovery>, error: &TrainerError, again: bool) {
        if matches!(error, TrainerError::Exited) {
            self.slot = Slot::Empty;
            self.record(Event::info(EntryKind::Game, "the game exited"));
            if self.quitting {
                self.finish(Finish::Untouched);
            } else {
                self.waiting();
            }
            return;
        }
        let sites = session.hooked();
        let message = error.to_string();
        self.slot = Slot::Pinned(session);
        if self.forced || (self.quitting && again) {
            self.record(Event::error(
                EntryKind::Hooks,
                format!("exiting with the game's code not fully restored: {message}"),
            ));
            self.due = None;
            self.done = Some(Done::HooksLeft { sites, message });
            return;
        }
        // The user reads the failure; quitting again tries once more, then exits.
        self.quitting = false;
        self.fail(error.recovery(), message, sites);
    }

    fn restore(&mut self) {
        let again = matches!(self.slot, Slot::Pinned(_));
        let outcome = match std::mem::replace(&mut self.slot, Slot::Empty) {
            Slot::Attached(session) => session.restore(self.options.verify),
            Slot::Pinned(session) => session.restore(self.options.verify),
            other => {
                self.slot = other;
                return;
            }
        };
        match outcome {
            Ok(detached) => {
                let report = detached.report();
                let message = format!(
                    "restored every hooked site in {} ms ({:?})",
                    report.timing.held.as_millis(),
                    report.text
                );
                // Dropping the session releases the control word, which tells a waiting close notice that
                // restoration finished.
                drop(detached);
                self.record(Event::info(EntryKind::Hooks, message));
                if self.quitting {
                    self.finish(Finish::Restored);
                } else {
                    self.waiting();
                }
            }
            Err(failure) => {
                let error = failure.error;
                self.record(Event::error(
                    EntryKind::Hooks,
                    format!("restoring the game failed: {error}"),
                ));
                self.pin(failure.session, &error, again);
            }
        }
    }
}
